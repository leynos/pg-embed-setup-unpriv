//! Pass two of extension installation: writing the planned files into the
//! tree, with the platform-specific mode and ownership work that goes with it.
//!
//! Split from `install` so pass one, which validates and decides, reads
//! separately from pass two, which acts.

use std::{
    fs,
    io::{self, Read},
    path::Path,
};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Report, eyre};

use super::{
    Sha256Hex,
    install::{
        ARCHIVE_DECOMPRESSED_CAP,
        ENTRY_DECOMPRESSED_CAP,
        EntryPathExt,
        PlannedFile,
        install_failed,
        invalid,
        open_archive,
    },
    layout::classify_entry_path,
};
use crate::error::BootstrapResult;

/// Pass two: write every planned file.
pub(super) fn write_all(
    path: &Utf8Path,
    bytes: &[u8],
    planned: &[PlannedFile],
    install_dir: &Utf8Path,
) -> BootstrapResult<()> {
    let owner = tree_owner(install_dir)?;
    let mut written: Vec<Utf8PathBuf> = Vec::new();
    let mut budget = ARCHIVE_DECOMPRESSED_CAP;
    let planned_by_path: std::collections::BTreeMap<&Utf8Path, &PlannedFile> = planned
        .iter()
        .map(|file| (file.relative.as_path(), file))
        .collect();
    let mut reader = open_archive(bytes);
    for entry_result in reader.entries()? {
        let mut entry =
            entry_result.map_err(|err| invalid(path, &format!("unreadable entry: {err}")))?;
        let Some(file) = classify_entry_path(&entry.path_bytes_lossy_path()) else {
            continue;
        };
        let Some(plan) = planned_by_path.get(file.as_path()).copied() else {
            continue;
        };
        let contents = read_entry_bounded(path, &mut entry, &plan.relative, &mut budget)
            .map_err(|err| over_cap_after_writing(&plan.relative, &written, err))?;
        write_file(install_dir, plan, &contents, owner)
            .map_err(|err| install_failed(&plan.relative, &written, err))?;
        written.push(plan.relative.clone());
    }
    Ok(())
}

/// Reports a pass-two cap breach as a partial installation.
///
/// The caps are gated in `install::check_decompressed_caps` from the tar
/// headers, so reaching one here means the bytes exceeded what the header
/// declared and earlier files of this archive are already in the tree. The
/// breach keeps its `ExtensionArchiveInvalid` kind, which is what the archive
/// is, but it is wrapped so the operator gets the same list of written files
/// that a write failure gives.
fn over_cap_after_writing(
    relative: &Utf8Path,
    written: &[Utf8PathBuf],
    err: crate::error::BootstrapError,
) -> crate::error::BootstrapError {
    if written.is_empty() {
        return err;
    }
    let kind = err.kind();
    crate::error::BootstrapError::new(
        kind,
        err.into_report().wrap_err(format!(
            "failed to install {relative}; files already written: {written:?}"
        )),
    )
}

/// Reads one entry, refusing to decompress past the per-file cap or to exhaust
/// the archive's remaining budget.
///
/// Reads one byte past whichever limit binds, so an entry that exceeds it is
/// detected without reading the rest of it. `read_to_end` on the decoder would
/// otherwise allocate whatever the archive expands to, which the compressed
/// cap does not constrain.
fn read_entry_bounded(
    path: &Utf8Path,
    entry: &mut impl Read,
    relative: &Utf8Path,
    budget: &mut u64,
) -> BootstrapResult<Vec<u8>> {
    let limit = ENTRY_DECOMPRESSED_CAP.min(*budget);
    let mut contents = Vec::new();
    entry
        .take(limit.saturating_add(1))
        .read_to_end(&mut contents)
        .map_err(|err| invalid(path, &format!("cannot read {relative}: {err}")))?;
    let read = contents.len() as u64;
    if read > limit {
        return Err(invalid(
            path,
            &format!(
                "{relative} decompresses past the limit; each file is capped at \
                 {ENTRY_DECOMPRESSED_CAP} bytes and the archive at {ARCHIVE_DECOMPRESSED_CAP}"
            ),
        ));
    }
    *budget -= read;
    Ok(contents)
}

/// Refuses a destination whose parent components are not real directories.
///
/// `classify_entry_path` validates the name the archive carries, not the tree
/// the file lands in. If `lib` or `share/extension` under `install_dir` is a
/// symlink, `create_dir_all` and `NamedTempFile::new_in` both follow it and
/// `persist` then installs the file outside `install_dir` altogether. Every
/// component of the relative path that already exists is therefore checked
/// with `symlink_metadata`, which does not follow, and must be a directory. A
/// component that does not exist yet is one `create_dir_all` will create, and
/// nothing below a missing component can exist, so the walk stops there.
///
/// The destination itself is deliberately not covered. A symlink there is
/// replaced by the atomic `persist`, which is how an already-installed file
/// is meant to be rewritten; it is not the parent-component escape.
fn require_real_parents(install_dir: &Utf8Path, relative: &Utf8Path) -> Result<(), Report> {
    let components: Vec<&str> = relative
        .components()
        .map(|component| component.as_str())
        .collect();
    let parents = components.len().saturating_sub(1);
    let mut current = install_dir.to_path_buf();
    for component in components.into_iter().take(parents) {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(metadata) => {
                return Err(eyre!(
                    "{current} is a {:?}, not a directory; the installation tree must not route \
                     through a symlink",
                    metadata.file_type()
                ));
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => break,
            Err(err) => return Err(eyre!("cannot inspect {current}: {err}")),
        }
    }
    Ok(())
}

/// Writes one file atomically, skipping it when an identical copy exists.
fn write_file(
    install_dir: &Utf8Path,
    plan: &PlannedFile,
    bytes: &[u8],
    owner: Owner,
) -> Result<(), Report> {
    require_real_parents(install_dir, &plan.relative)?;
    let destination = install_dir.join(&plan.relative);
    if Sha256Hex::of_file(&destination).is_ok_and(|existing| existing == Sha256Hex::of_bytes(bytes))
    {
        // Identical bytes keep their inode, but the mode and owner are still
        // brought into line so a root-owned or 0600 copy does not stop the
        // server from loading it.
        set_mode(destination.as_std_path(), plan.mode)?;
        return apply_owner(destination.as_std_path(), owner);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| eyre!("{destination} has no parent directory"))?;
    fs::create_dir_all(parent).map_err(|err| eyre!("cannot create {parent}: {err}"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|err| eyre!("cannot create temporary file in {parent}: {err}"))?;
    io::Write::write_all(&mut temp, bytes)
        .map_err(|err| eyre!("cannot write {destination}: {err}"))?;
    set_mode(temp.path(), plan.mode)?;
    apply_owner(temp.path(), owner)?;
    temp.persist(&destination)
        .map_err(|err| eyre!("cannot move file into place at {destination}: {err}"))?;
    Ok(())
}

/// Applies a Unix mode to a written file.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), Report> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|err| eyre!("cannot set mode {mode:o} on {}: {err}", path.display()))
}

/// Modes are not applied on platforms without Unix permissions.
#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<(), Report> { Ok(()) }

/// Owner of the installation tree, propagated to installed files.
#[derive(Debug, Clone, Copy)]
struct Owner {
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
}

/// Reads the uid and gid that own the installation directory.
#[cfg(unix)]
fn tree_owner(install_dir: &Utf8Path) -> BootstrapResult<Owner> {
    // Scoped here: both are used only on this Unix-only path, so importing
    // them at module level leaves them unused off Unix.
    use std::os::unix::fs::MetadataExt;

    use crate::{error::BootstrapErrorKind, extensions::extension_error};

    let metadata = fs::metadata(install_dir).map_err(|err| {
        extension_error(
            BootstrapErrorKind::ExtensionInstallFailed,
            eyre!("cannot stat installation directory {install_dir}: {err}"),
        )
    })?;
    Ok(Owner {
        uid: metadata.uid(),
        gid: metadata.gid(),
    })
}

/// Ownership is not tracked on platforms without Unix uids.
#[cfg(not(unix))]
fn tree_owner(_install_dir: &Utf8Path) -> BootstrapResult<Owner> { Ok(Owner {}) }

/// Chowns `path` to the tree owner when it differs, so the demoted worker can
/// remove the files during `cleanup-full`.
#[cfg(unix)]
fn apply_owner(path: &Path, owner: Owner) -> Result<(), Report> {
    use std::os::unix::fs::MetadataExt;
    let metadata =
        fs::metadata(path).map_err(|err| eyre!("cannot stat {}: {err}", path.display()))?;
    if metadata.uid() == owner.uid && metadata.gid() == owner.gid {
        return Ok(());
    }
    std::os::unix::fs::chown(path, Some(owner.uid), Some(owner.gid)).map_err(|err| {
        eyre!(
            "cannot chown {} to {}:{}: {err}",
            path.display(),
            owner.uid,
            owner.gid
        )
    })
}

/// Ownership is not applied on platforms without Unix uids.
#[cfg(not(unix))]
fn apply_owner(_path: &Path, _owner: Owner) -> Result<(), Report> { Ok(()) }

//! Pass two of extension installation: writing the planned files into the
//! tree, with the platform-specific mode and ownership work that goes with it.
//!
//! Split from `install` so pass one, which validates and decides, reads
//! separately from pass two, which acts.

use std::io::Read;

use camino::{Utf8Path, Utf8PathBuf};
use cap_std::fs::File;
use color_eyre::eyre::{Report, eyre};

use super::{
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
    tree::InstallTree,
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
    // One handle for the whole archive, held until the last rename completes.
    let context = WriteContext {
        tree: InstallTree::open(install_dir)?,
        owner,
    };
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
        write_file(&context, plan, &contents)
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

/// The installation tree and the ownership every file written into it takes.
///
/// Carried together so the per-file writer stays inside the four-argument
/// ceiling, and so the tree handle outlives every file placed through it.
struct WriteContext {
    tree: InstallTree,
    owner: Owner,
}

/// Writes one file atomically, skipping it when an identical copy exists.
///
/// Every filesystem operation resolves through the tree handle rather than a
/// path, so the parent a file lands under cannot be swapped for a link to
/// somewhere else between the check and the write.
fn write_file(context: &WriteContext, plan: &PlannedFile, bytes: &[u8]) -> Result<(), Report> {
    let tree = &context.tree;
    tree.require_real_parents(&plan.relative)?;
    let destination = tree.path_of(&plan.relative);
    if let Some(existing) = tree.open_identical_regular_file(&plan.relative, bytes) {
        // Identical bytes keep their inode, but the mode and owner are still
        // brought into line so a root-owned or 0600 copy does not stop the
        // server from loading it.
        set_mode(&existing, &destination, plan.mode)?;
        return apply_owner(&existing, &destination, context.owner);
    }
    tree.create_parents(&plan.relative)?;
    let (temp, temp_relative) = tree.create_temp_beside(&plan.relative)?;
    let placed = fill_temp(context, plan, bytes, &temp)
        .and_then(|()| tree.place(&temp_relative, &plan.relative));
    if placed.is_err() {
        tree.discard_temp(&temp_relative);
    }
    placed
}

/// Writes the bytes into the temporary file and stamps its mode and owner.
///
/// Both are applied through the open handle before the rename, so the file
/// arrives at its destination already correct and never appears there with the
/// wrong mode.
fn fill_temp(
    context: &WriteContext,
    plan: &PlannedFile,
    bytes: &[u8],
    temp: &File,
) -> Result<(), Report> {
    let destination = context.tree.path_of(&plan.relative);
    let mut handle = temp;
    std::io::Write::write_all(&mut handle, bytes)
        .map_err(|err| eyre!("cannot write {destination}: {err}"))?;
    set_mode(temp, &destination, plan.mode)?;
    apply_owner(temp, &destination, context.owner)
}

/// Applies a Unix mode to an open file.
///
/// The mode is set through the handle, so it reaches the file that was opened
/// and never a path the destination was replaced by in the meantime.
/// `destination` names the file in the error only.
#[cfg(unix)]
fn set_mode(file: &File, destination: &Utf8Path, mode: u32) -> Result<(), Report> {
    use cap_std::fs::{Permissions, PermissionsExt};
    file.set_permissions(Permissions::from_mode(mode))
        .map_err(|err| eyre!("cannot set mode {mode:o} on {destination}: {err}"))
}

/// Modes are not applied on platforms without Unix permissions.
#[cfg(not(unix))]
fn set_mode(_file: &File, _destination: &Utf8Path, _mode: u32) -> Result<(), Report> { Ok(()) }

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
    // Scoped here: all three are used only on this Unix-only path, so
    // importing them at module level leaves them unused off Unix, and
    // `-D warnings` turns an unused import into a Windows build failure.
    use std::{fs, os::unix::fs::MetadataExt};

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

/// Chowns an open file to the tree owner when it differs, so the demoted
/// worker can remove the files during `cleanup-full`.
///
/// Both the stat and the chown go through the handle, so neither can be
/// redirected by a replacement at the path. `destination` names the file in
/// the errors only.
#[cfg(unix)]
fn apply_owner(file: &File, destination: &Utf8Path, owner: Owner) -> Result<(), Report> {
    use cap_std::fs::MetadataExt;
    let metadata = file
        .metadata()
        .map_err(|err| eyre!("cannot stat {destination}: {err}"))?;
    if metadata.uid() == owner.uid && metadata.gid() == owner.gid {
        return Ok(());
    }
    std::os::unix::fs::fchown(file, Some(owner.uid), Some(owner.gid)).map_err(|err| {
        eyre!(
            "cannot chown {destination} to {}:{}: {err}",
            owner.uid,
            owner.gid
        )
    })
}

/// Ownership is not applied on platforms without Unix uids.
#[cfg(not(unix))]
fn apply_owner(_file: &File, _destination: &Utf8Path, _owner: Owner) -> Result<(), Report> {
    Ok(())
}

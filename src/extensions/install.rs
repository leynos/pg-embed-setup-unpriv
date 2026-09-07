//! Validates an extension archive and writes its files into the tree.
//!
//! Extraction runs in two passes. The first reads every entry and rejects
//! anything outside the rules without writing a byte; the second writes each
//! file to a temporary sibling and renames it over the destination so a
//! shared object another process has mapped is replaced by a new inode.

use std::{
    fs,
    io::{self, Read},
    path::Path,
};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Report, eyre};
use flate2::read::GzDecoder;
use tar::{Archive, EntryType};

use super::{Sha256Hex, extension_error, layout::classify_entry_path, manifest::ManifestArtifact};
use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult};

const LIB_MODE: u32 = 0o755;
const SHARE_MODE: u32 = 0o644;

/// One regular file the archive will write.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedFile {
    relative: Utf8PathBuf,
    mode: u32,
}

/// Validates the archive at `path` and installs its files under `install_dir`.
///
/// The archive is read into memory once and its digest checked against the
/// manifest at that moment, so the bytes that are validated are exactly the
/// bytes that are written; a concurrent change to the cached file cannot
/// slip between the two passes. Returns the installed paths relative to the
/// install root, sorted.
pub(super) fn install_archive(
    path: &Utf8Path,
    artifact: &ManifestArtifact,
    install_dir: &Utf8Path,
) -> BootstrapResult<Vec<Utf8PathBuf>> {
    let bytes = read_verified(path, artifact)?;
    let planned = plan(path, &bytes, artifact)?;
    write_all(path, &bytes, &planned, install_dir)?;
    tracing::info!(
        target: super::LOG_TARGET,
        archive = %path,
        files = planned.len(),
        install_dir = %install_dir,
        "installed extension archive"
    );
    Ok(planned.into_iter().map(|file| file.relative).collect())
}

/// Reads the archive, capped at the manifest size plus one byte, and confirms
/// it still hashes to the manifest digest.
///
/// The cap matters because the cache lock is released before installation:
/// a file swapped for a larger one in that window is rejected after reading
/// at most `size + 1` bytes rather than being read whole.
///
/// The read limit saturates and the buffer is grown by the read rather than
/// preallocated from `size`, so a manifest that escaped validation with an
/// absurd size cannot overflow the limit or ask for an allocation the process
/// cannot serve. `Manifest::validate` bounds `size` at `ARCHIVE_SIZE_CAP`; this
/// is the second line of that defence.
fn read_verified(path: &Utf8Path, artifact: &ManifestArtifact) -> BootstrapResult<Vec<u8>> {
    let file = fs::File::open(path)
        .map_err(|err| invalid(path, &format!("cannot open archive: {err}")))?;
    let mut bytes = Vec::new();
    file.take(artifact.size.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| invalid(path, &format!("cannot read archive: {err}")))?;
    let actual = Sha256Hex::of_bytes(&bytes);
    if bytes.len() as u64 != artifact.size || actual != artifact.sha256 {
        return Err(extension_error(
            BootstrapErrorKind::ExtensionArchiveDigestMismatch,
            eyre!(
                "extension archive {path} hashes to {actual} but the manifest records {}",
                artifact.sha256
            ),
        ));
    }
    Ok(bytes)
}

/// Pass one: read every entry and validate without writing.
fn plan(
    path: &Utf8Path,
    bytes: &[u8],
    artifact: &ManifestArtifact,
) -> BootstrapResult<Vec<PlannedFile>> {
    let mut files: Vec<PlannedFile> = Vec::new();
    let mut reader = open_archive(bytes);
    for entry_result in reader.entries()? {
        let entry =
            entry_result.map_err(|err| invalid(path, &format!("unreadable entry: {err}")))?;
        if let Some(file) =
            plan_entry(path, entry.header().entry_type(), &entry.path_bytes_lossy())?
        {
            if files.iter().any(|known| known.relative == file.relative) {
                return Err(invalid(path, &format!("duplicate entry {}", file.relative)));
            }
            files.push(file);
        }
    }
    // Byte order, matching how the manifest list is sorted below.
    files.sort_by(|a, b| a.relative.as_str().cmp(b.relative.as_str()));
    check_against_manifest(path, &files, artifact)?;
    Ok(files)
}

/// Classifies one tar entry: directories are skipped, regular files under an
/// allowed prefix are planned, anything else is rejected.
fn plan_entry(
    path: &Utf8Path,
    kind: EntryType,
    name: &str,
) -> BootstrapResult<Option<PlannedFile>> {
    if kind.is_dir() {
        return Ok(None);
    }
    if !kind.is_file() {
        return Err(invalid(
            path,
            &format!(
                "entry {name:?} is a {kind:?}; only regular files and directories are allowed"
            ),
        ));
    }
    let relative = classify_entry_path(Path::new(name)).ok_or_else(|| {
        invalid(
            path,
            &format!("entry {name:?} is outside lib/ or share/extension/"),
        )
    })?;
    let mode = if relative.starts_with("lib") {
        LIB_MODE
    } else {
        SHARE_MODE
    };
    Ok(Some(PlannedFile { relative, mode }))
}

/// Requires the planned file set to equal the manifest's `files` list.
fn check_against_manifest(
    path: &Utf8Path,
    files: &[PlannedFile],
    artifact: &ManifestArtifact,
) -> BootstrapResult<()> {
    let mut expected: Vec<&str> = artifact.files.iter().map(String::as_str).collect();
    expected.sort_unstable();
    let actual: Vec<&str> = files.iter().map(|file| file.relative.as_str()).collect();
    if actual != expected {
        return Err(invalid(
            path,
            &format!("archive contents {actual:?} differ from the manifest file list {expected:?}"),
        ));
    }
    Ok(())
}

/// Pass two: write every planned file.
fn write_all(
    path: &Utf8Path,
    bytes: &[u8],
    planned: &[PlannedFile],
    install_dir: &Utf8Path,
) -> BootstrapResult<()> {
    let owner = tree_owner(install_dir)?;
    let mut written: Vec<Utf8PathBuf> = Vec::new();
    let mut reader = open_archive(bytes);
    for entry_result in reader.entries()? {
        let mut entry =
            entry_result.map_err(|err| invalid(path, &format!("unreadable entry: {err}")))?;
        let Some(file) = classify_entry_path(&entry.path_bytes_lossy_path()) else {
            continue;
        };
        let Some(plan) = planned.iter().find(|known| known.relative == file) else {
            continue;
        };
        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(|err| invalid(path, &format!("cannot read {}: {err}", plan.relative)))?;
        write_file(install_dir, plan, &contents, owner)
            .map_err(|err| install_failed(&plan.relative, &written, err))?;
        written.push(plan.relative.clone());
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

#[cfg(unix)]
/// Applies a Unix mode to a written file.
fn set_mode(path: &Path, mode: u32) -> Result<(), Report> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|err| eyre!("cannot set mode {mode:o} on {}: {err}", path.display()))
}

#[cfg(not(unix))]
/// Modes are not applied on platforms without Unix permissions.
fn set_mode(_path: &Path, _mode: u32) -> Result<(), Report> { Ok(()) }

/// Owner of the installation tree, propagated to installed files.
#[derive(Debug, Clone, Copy)]
struct Owner {
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    gid: u32,
}

#[cfg(unix)]
/// Reads the uid and gid that own the installation directory.
fn tree_owner(install_dir: &Utf8Path) -> BootstrapResult<Owner> {
    use std::os::unix::fs::MetadataExt;
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

#[cfg(not(unix))]
/// Ownership is not tracked on platforms without Unix uids.
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

#[cfg(not(unix))]
/// Ownership is not applied on platforms without Unix uids.
fn apply_owner(_path: &Path, _owner: Owner) -> Result<(), Report> { Ok(()) }

/// Entry iterator over an in-memory archive: `'r` is the reader borrow, `'b`
/// the archive bytes.
type Entries<'r, 'b> = tar::Entries<'r, GzDecoder<io::Cursor<&'b [u8]>>>;

/// Wraps the in-memory archive so its entries can be iterated.
fn open_archive(bytes: &[u8]) -> OpenArchive<'_> {
    OpenArchive {
        archive: Archive::new(GzDecoder::new(io::Cursor::new(bytes))),
    }
}

/// Holds the archive so its entries can be iterated by callers.
struct OpenArchive<'a> {
    archive: Archive<GzDecoder<io::Cursor<&'a [u8]>>>,
}

impl<'b> OpenArchive<'b> {
    /// Iterates the archive's entries, mapping a malformed archive to
    /// `ExtensionArchiveInvalid`.
    fn entries(&mut self) -> BootstrapResult<Entries<'_, 'b>> {
        self.archive.entries().map_err(|err| {
            extension_error(
                BootstrapErrorKind::ExtensionArchiveInvalid,
                eyre!("cannot read archive entries: {err}"),
            )
        })
    }
}

/// Convenience accessors on tar entries for lossy path rendering.
trait EntryPathExt {
    /// The entry path as a lossy string, for error messages.
    fn path_bytes_lossy(&self) -> String;
    /// The entry path as a lossy `PathBuf`, for classification.
    fn path_bytes_lossy_path(&self) -> std::path::PathBuf;
}

impl<R: Read> EntryPathExt for tar::Entry<'_, R> {
    fn path_bytes_lossy(&self) -> String {
        String::from_utf8_lossy(&self.path_bytes()).into_owned()
    }

    fn path_bytes_lossy_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(self.path_bytes_lossy())
    }
}

/// Builds an `ExtensionArchiveInvalid` error naming the archive.
fn invalid(path: &Utf8Path, detail: &str) -> BootstrapError {
    extension_error(
        BootstrapErrorKind::ExtensionArchiveInvalid,
        eyre!("extension archive {path}: {detail}"),
    )
}

/// Builds an `ExtensionInstallFailed` error listing the files already written.
fn install_failed(relative: &Utf8Path, written: &[Utf8PathBuf], err: Report) -> BootstrapError {
    extension_error(
        BootstrapErrorKind::ExtensionInstallFailed,
        err.wrap_err(format!(
            "failed to install {relative}; files already written: {written:?}"
        )),
    )
}

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

pub(super) const LIB_MODE: u32 = 0o755;
pub(super) const SHARE_MODE: u32 = 0o644;

/// Upper bound on one file's decompressed size.
///
/// `ARCHIVE_SIZE_CAP` bounds the compressed archive, which says nothing about
/// what it expands to: a highly compressible archive within that cap can
/// decompress to orders of magnitude more. A shared object or a SQL script in
/// an extension is single-digit megabytes, so 64 MiB per file is generous.
pub(super) const ENTRY_DECOMPRESSED_CAP: u64 = 64 * 1024 * 1024;

/// Upper bound on the total decompressed size of one archive.
///
/// Bounds the whole extraction, so many entries each under the per-file cap
/// cannot add up to an unbounded allocation either.
pub(super) const ARCHIVE_DECOMPRESSED_CAP: u64 = 256 * 1024 * 1024;

/// One regular file the archive will write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PlannedFile {
    pub(super) relative: Utf8PathBuf,
    pub(super) mode: u32,
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
    super::write::write_all(path, &bytes, &planned, install_dir)?;
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
    // A set rather than a scan, so an archive with many entries costs
    // linearithmic time overall instead of quadratic.
    let mut seen: std::collections::BTreeSet<Utf8PathBuf> = std::collections::BTreeSet::new();
    let mut reader = open_archive(bytes);
    for entry_result in reader.entries()? {
        let entry =
            entry_result.map_err(|err| invalid(path, &format!("unreadable entry: {err}")))?;
        if let Some(file) =
            plan_entry(path, entry.header().entry_type(), &entry.path_bytes_lossy())?
        {
            if !seen.insert(file.relative.clone()) {
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

/// Entry iterator over an in-memory archive: `'r` is the reader borrow, `'b`
/// the archive bytes.
pub(super) type Entries<'r, 'b> = tar::Entries<'r, GzDecoder<io::Cursor<&'b [u8]>>>;

/// Wraps the in-memory archive so its entries can be iterated.
pub(super) fn open_archive(bytes: &[u8]) -> OpenArchive<'_> {
    OpenArchive {
        archive: Archive::new(GzDecoder::new(io::Cursor::new(bytes))),
    }
}

/// Holds the archive so its entries can be iterated by callers.
pub(super) struct OpenArchive<'a> {
    archive: Archive<GzDecoder<io::Cursor<&'a [u8]>>>,
}

impl<'b> OpenArchive<'b> {
    /// Iterates the archive's entries, mapping a malformed archive to
    /// `ExtensionArchiveInvalid`.
    pub(super) fn entries(&mut self) -> BootstrapResult<Entries<'_, 'b>> {
        self.archive.entries().map_err(|err| {
            extension_error(
                BootstrapErrorKind::ExtensionArchiveInvalid,
                eyre!("cannot read archive entries: {err}"),
            )
        })
    }
}

/// Convenience accessors on tar entries for lossy path rendering.
pub(super) trait EntryPathExt {
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
pub(super) fn invalid(path: &Utf8Path, detail: &str) -> BootstrapError {
    extension_error(
        BootstrapErrorKind::ExtensionArchiveInvalid,
        eyre!("extension archive {path}: {detail}"),
    )
}

/// Builds an `ExtensionInstallFailed` error listing the files already written.
pub(super) fn install_failed(
    relative: &Utf8Path,
    written: &[Utf8PathBuf],
    err: Report,
) -> BootstrapError {
    extension_error(
        BootstrapErrorKind::ExtensionInstallFailed,
        err.wrap_err(format!(
            "failed to install {relative}; files already written: {written:?}"
        )),
    )
}

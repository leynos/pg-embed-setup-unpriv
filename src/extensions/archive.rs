//! Acquires extension archives: cache lookup, download and digest verification.
//!
//! Archives are stored under `<cache_dir>/<sha256>/<file>` and re-verified on
//! every use, so a corrupted cache entry is replaced rather than trusted.
//! Downloads insist on HTTPS (loopback hosts excepted, for local mirrors and
//! tests), refuse redirects to any other scheme, and retry transient
//! failures a bounded number of times.

use std::time::Instant;

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Report, eyre};
use tracing::{debug, info, warn};

use super::{
    ArchiveOrigin,
    LOG_TARGET,
    Sha256Hex,
    digest::HashingWriter,
    extension_error,
    http::{http_get, millis, redact_url},
    manifest::ManifestArtifact,
};
use crate::{
    cache::CacheLock,
    error::{BootstrapError, BootstrapErrorKind, BootstrapResult},
};

/// An archive on local disk whose digest has been verified.
#[derive(Debug)]
pub(super) struct AcquiredArchive {
    /// Verified archive path.
    pub(super) path: Utf8PathBuf,
    /// Whether it came from the cache or was downloaded now.
    pub(super) origin: ArchiveOrigin,
}

/// Returns a verified local copy of `artifact`, downloading it when needed.
pub(super) fn acquire(
    cache_dir: &Utf8Path,
    artifact: &ManifestArtifact,
) -> BootstrapResult<AcquiredArchive> {
    let entry_dir = cache_dir.join(artifact.sha256.as_str());
    std::fs::create_dir_all(&entry_dir)
        .map_err(|err| unavailable(eyre!("cannot create extension cache {entry_dir}: {err}")))?;
    let _lock = CacheLock::acquire_exclusive(cache_dir, artifact.sha256.as_str())
        .map_err(|err| unavailable(eyre!("cannot lock extension cache {cache_dir}: {err}")))?;
    let path = entry_dir.join(&artifact.file);
    let state = cached_state(&path, &artifact.sha256);
    let reusable = matches!(state, CachedState::Valid);
    log_cache_state(state, &path);
    if reusable {
        return Ok(AcquiredArchive {
            path,
            origin: ArchiveOrigin::Cached,
        });
    }
    clear_unusable_entry(&path)?;
    download(artifact, &entry_dir, &path)?;
    Ok(AcquiredArchive {
        path,
        origin: ArchiveOrigin::Downloaded,
    })
}

/// Removes whatever occupies the entry path so a download can replace it.
///
/// The download finishes with `NamedTempFile::persist`, which renames over the
/// destination. A rename can replace a regular file but not a directory, so an
/// entry that is not a regular file has to go first; otherwise the acquire
/// fails with a rename error instead of the cache healing itself. A corrupt
/// regular file is deliberately left in place, because `persist` replaces it
/// atomically and removing it first would open a window where the entry is
/// absent.
///
/// The probe does not follow symlinks: a link whose target no longer verifies
/// is removed rather than written through.
pub(super) fn clear_unusable_entry(path: &Utf8Path) -> BootstrapResult<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(unavailable(eyre!(
                "cannot inspect extension cache entry {path}: {err}"
            )));
        }
    };
    if metadata.is_file() {
        return Ok(());
    }
    let removed = if metadata.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|err| {
        unavailable(eyre!(
            "cannot remove unusable extension cache entry {path}: {err}"
        ))
    })
}

/// Why a cache entry was or was not reused.
///
/// The three failing outcomes are kept apart because they are different
/// operational problems: an entry that was never written, one whose bytes no
/// longer hash to the manifest digest, and one this process cannot read at
/// all. Collapsing the last into the second reports a digest mismatch for what
/// is really a permission or filesystem fault, which sends the reader looking
/// for a corrupted download that never happened.
#[derive(Debug)]
pub(super) enum CachedState {
    /// The entry is present and hashes to the expected digest.
    Valid,
    /// No entry exists at the path.
    Missing,
    /// The entry exists but its bytes hash to something else.
    Corrupt,
    /// The entry exists but could not be read; the error says why.
    Unreadable(std::io::Error),
}

/// Classifies the cache entry at `path` against the expected digest.
///
/// Every outcome other than [`CachedState::Valid`] leads to a re-download,
/// which is the cache's self-healing path. A download that cannot then write
/// reports its own error, so an unreadable entry is never silently swallowed.
pub(super) fn cached_state(path: &Utf8Path, expected: &Sha256Hex) -> CachedState {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return CachedState::Unreadable(std::io::Error::other(
                "cache entry is not a regular file",
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return CachedState::Missing,
        Err(err) => return CachedState::Unreadable(err),
    }
    match Sha256Hex::of_file(path) {
        Ok(actual) if &actual == expected => CachedState::Valid,
        Ok(_) => CachedState::Corrupt,
        Err(err) => CachedState::Unreadable(err),
    }
}

/// Emits the cache hit, miss, corrupt or unreadable event for `path`.
fn log_cache_state(state: CachedState, path: &Utf8Path) {
    match state {
        CachedState::Valid => log_cache_hit(path),
        CachedState::Missing => log_cache_miss(path),
        CachedState::Corrupt => log_cache_corrupt(path),
        CachedState::Unreadable(error) => log_cache_unreadable(path, &error),
    }
}

/// Debug event for a valid cache entry.
fn log_cache_hit(path: &Utf8Path) {
    debug!(target: LOG_TARGET, path = %path, "extension archive cache hit");
}

/// Debug event for an absent cache entry.
fn log_cache_miss(path: &Utf8Path) {
    debug!(target: LOG_TARGET, path = %path, "extension archive cache miss");
}

/// Warning event for a cache entry whose digest no longer matches.
fn log_cache_corrupt(path: &Utf8Path) {
    warn!(
        target: LOG_TARGET,
        path = %path,
        "cached extension archive digest mismatch; re-downloading"
    );
}

/// Warning event for a cache entry this process cannot read.
fn log_cache_unreadable(path: &Utf8Path, error: &std::io::Error) {
    warn!(
        target: LOG_TARGET,
        path = %path,
        error = %error,
        "cached extension archive cannot be read; re-downloading"
    );
}

/// Downloads the archive to a temporary file, verifies size and digest,
/// and renames it into place.
fn download(
    artifact: &ManifestArtifact,
    entry_dir: &Utf8Path,
    path: &Utf8Path,
) -> BootstrapResult<()> {
    let started = Instant::now();
    let temp = tempfile::NamedTempFile::new_in(entry_dir)
        .map_err(|err| unavailable(eyre!("cannot create temporary file in {entry_dir}: {err}")))?;
    let mut writer = HashingWriter::new(temp);
    http_get(&artifact.url, artifact.size, &mut writer).map_err(|err| {
        unavailable(eyre!(
            "cannot download {}: {err}",
            redact_url(&artifact.url)
        ))
    })?;
    let (downloaded, actual, written) = writer.finish();
    if written != artifact.size || actual != artifact.sha256 {
        drop(downloaded);
        return Err(extension_error(
            BootstrapErrorKind::ExtensionArchiveDigestMismatch,
            eyre!(
                "{} downloaded {written} bytes hashing to {actual}; the manifest records {} bytes \
                 hashing to {}",
                redact_url(&artifact.url),
                artifact.size,
                artifact.sha256
            ),
        ));
    }
    downloaded
        .persist(path)
        .map_err(|err| unavailable(eyre!("cannot move downloaded archive into {path}: {err}")))?;
    info!(
        target: LOG_TARGET,
        url = %artifact.url,
        bytes = written,
        elapsed_ms = millis(started.elapsed()),
        "downloaded extension archive"
    );
    Ok(())
}

/// Wraps a report as `ExtensionArchiveUnavailable`.
const fn unavailable(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionArchiveUnavailable, report)
}

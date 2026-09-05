//! Acquires extension archives: cache lookup, download and digest verification.
//!
//! Archives are stored under `<cache_dir>/<sha256>/<file>` and re-verified on
//! every use, so a corrupted cache entry is replaced rather than trusted.

use std::{
    io::{self, Read},
    time::Duration,
};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Report, eyre};
use tracing::debug;

use super::{
    ArchiveOrigin,
    LOG_TARGET,
    Sha256Hex,
    digest::HashingWriter,
    extension_error,
    manifest::ManifestArtifact,
};
use crate::{
    cache::CacheLock,
    error::{BootstrapError, BootstrapErrorKind, BootstrapResult},
};

/// Overall timeout for one HTTP request.
const HTTP_TIMEOUT: Duration = Duration::from_secs(120);

/// An archive on local disk whose digest has been verified.
#[derive(Debug)]
pub(super) struct AcquiredArchive {
    /// Verified archive path.
    pub(super) path: Utf8PathBuf,
    /// Whether it came from the cache or was downloaded now.
    pub(super) origin: ArchiveOrigin,
}

/// Performs a bounded HTTPS GET, streaming the body into `writer`.
///
/// Reads at most `cap + 1` bytes so a caller can detect an oversized body by
/// comparing what it received against `cap`.
pub(super) fn http_get(url: &str, cap: u64, writer: &mut dyn io::Write) -> Result<u64, Report> {
    let client = reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|err| eyre!("cannot build HTTP client: {err}"))?;
    let response = client
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|err| eyre!("{err}"))?;
    let mut limited = response.take(cap + 1);
    io::copy(&mut limited, writer).map_err(|err| eyre!("body read failed: {err}"))
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
    if cached_copy_is_valid(&path, &artifact.sha256) {
        debug!(target: LOG_TARGET, path = %path, "using cached extension archive");
        return Ok(AcquiredArchive {
            path,
            origin: ArchiveOrigin::Cached,
        });
    }
    download(artifact, &entry_dir, &path)?;
    Ok(AcquiredArchive {
        path,
        origin: ArchiveOrigin::Downloaded,
    })
}

fn cached_copy_is_valid(path: &Utf8Path, expected: &Sha256Hex) -> bool {
    path.is_file() && Sha256Hex::of_file(path).is_ok_and(|actual| &actual == expected)
}

fn download(
    artifact: &ManifestArtifact,
    entry_dir: &Utf8Path,
    path: &Utf8Path,
) -> BootstrapResult<()> {
    debug!(target: LOG_TARGET, url = %artifact.url, "downloading extension archive");
    let temp = tempfile::NamedTempFile::new_in(entry_dir)
        .map_err(|err| unavailable(eyre!("cannot create temporary file in {entry_dir}: {err}")))?;
    let mut writer = HashingWriter::new(temp);
    http_get(&artifact.url, artifact.size, &mut writer)
        .map_err(|err| unavailable(eyre!("cannot download {}: {err}", artifact.url)))?;
    let (downloaded, actual, written) = writer.finish();
    if written != artifact.size || actual != artifact.sha256 {
        drop(downloaded);
        return Err(extension_error(
            BootstrapErrorKind::ExtensionArchiveDigestMismatch,
            eyre!(
                "{} downloaded {written} bytes hashing to {actual}; the manifest records {} bytes \
                 hashing to {}",
                artifact.url,
                artifact.size,
                artifact.sha256
            ),
        ));
    }
    downloaded
        .persist(path)
        .map_err(|err| unavailable(eyre!("cannot move downloaded archive into {path}: {err}")))?;
    Ok(())
}

const fn unavailable(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionArchiveUnavailable, report)
}

//! Acquires extension archives: cache lookup, download and digest verification.
//!
//! Archives are stored under `<cache_dir>/<sha256>/<file>` and re-verified on
//! every use, so a corrupted cache entry is replaced rather than trusted.
//! Downloads insist on HTTPS (loopback hosts excepted, for local mirrors and
//! tests), refuse redirects to any other scheme, and retry transient
//! failures a bounded number of times.

use std::{
    io::{self, Read},
    time::{Duration, Instant},
};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Report, eyre};
use reqwest::{StatusCode, Url, redirect};
use tracing::{debug, info, warn};

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
/// Attempts made for connection failures and 5xx responses.
const HTTP_ATTEMPTS: u32 = 3;
/// Delay before the second attempt; doubles for each later attempt.
const HTTP_BACKOFF: Duration = Duration::from_millis(200);

/// An archive on local disk whose digest has been verified.
#[derive(Debug)]
pub(super) struct AcquiredArchive {
    /// Verified archive path.
    pub(super) path: Utf8PathBuf,
    /// Whether it came from the cache or was downloaded now.
    pub(super) origin: ArchiveOrigin,
}

/// Returns `true` when `url` may be fetched: `https://`, or plain `http://`
/// to a loopback host only.
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::extensions::is_permitted_url;
///
/// assert!(is_permitted_url(
///     "https://github.com/leynos/df12-pg-extensions/releases/x.tar.gz"
/// ));
/// assert!(is_permitted_url("http://127.0.0.1:8080/x.tar.gz"));
/// assert!(!is_permitted_url("http://example.com/x.tar.gz"));
/// assert!(!is_permitted_url("ftp://example.com/x.tar.gz"));
/// ```
#[must_use]
pub fn is_permitted_url(url: &str) -> bool {
    Url::parse(url).is_ok_and(|parsed| is_permitted(&parsed))
}

fn is_permitted(url: &Url) -> bool {
    match url.scheme() {
        "https" => true,
        "http" => url.host_str().is_some_and(is_loopback_host),
        _ => false,
    }
}

fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Builds the HTTP client: bounded timeout, redirects only to permitted URLs.
fn build_client() -> Result<reqwest::blocking::Client, Report> {
    let policy = redirect::Policy::custom(|attempt| {
        if attempt.previous().len() >= 10 {
            attempt.error("too many redirects")
        } else if is_permitted(attempt.url()) {
            attempt.follow()
        } else {
            attempt.error("redirect to a non-HTTPS URL is not permitted")
        }
    });
    reqwest::blocking::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .redirect(policy)
        .build()
        .map_err(|err| eyre!("cannot build HTTP client: {err}"))
}

/// Performs a bounded HTTPS GET, streaming the body into `writer`.
///
/// Reads at most `cap + 1` bytes so a caller can detect an oversized body by
/// comparing what it received against `cap`. Connection failures and 5xx
/// responses are retried with backoff; 4xx responses are not.
pub(super) fn http_get(url: &str, cap: u64, writer: &mut dyn io::Write) -> Result<u64, Report> {
    if !is_permitted_url(url) {
        return Err(eyre!(
            "{url} is not an https:// URL (loopback http is the only exception)"
        ));
    }
    let client = build_client()?;
    let started = Instant::now();
    let (response, attempts) = fetch_with_retry(&client, url)?;
    let received = io::copy(&mut response.take(cap + 1), writer)
        .map_err(|err| eyre!("body read failed: {err}"))?;
    debug!(
        target: LOG_TARGET,
        url,
        bytes = received,
        attempts,
        elapsed_ms = millis(started.elapsed()),
        "http get complete"
    );
    Ok(received)
}

/// Sends the request up to [`HTTP_ATTEMPTS`] times, backing off between
/// transient failures; returns the response and the attempt count.
fn fetch_with_retry(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<(reqwest::blocking::Response, u32), Report> {
    let mut attempt = 1;
    loop {
        match send(client, url) {
            Ok(response) => return Ok((response, attempt)),
            Err(Failure::Permanent(err)) => return Err(err),
            Err(Failure::Transient(err)) if attempt >= HTTP_ATTEMPTS => {
                return Err(err.wrap_err(format!("giving up after {attempt} attempts")));
            }
            Err(Failure::Transient(err)) => {
                let delay = HTTP_BACKOFF * 2_u32.pow(attempt - 1);
                warn!(
                    target: LOG_TARGET,
                    url,
                    attempt,
                    error = %err,
                    delay_ms = millis(delay),
                    "transient http failure; retrying"
                );
                std::thread::sleep(delay);
                attempt += 1;
            }
        }
    }
}

/// Milliseconds for log fields, saturating rather than truncating.
fn millis(duration: Duration) -> u64 { u64::try_from(duration.as_millis()).unwrap_or(u64::MAX) }

/// Why one attempt failed, and whether another is worth making.
enum Failure {
    Transient(Report),
    Permanent(Report),
}

fn send(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<reqwest::blocking::Response, Failure> {
    let response = client.get(url).send().map_err(|err| {
        if err.is_redirect() {
            Failure::Permanent(eyre!("{err}"))
        } else {
            Failure::Transient(eyre!("{err}"))
        }
    })?;
    let status = response.status();
    if status.is_success() {
        Ok(response)
    } else if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        Err(Failure::Transient(eyre!("server returned {status}")))
    } else {
        Err(Failure::Permanent(eyre!("server returned {status}")))
    }
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
    log_cache_state(state, &path);
    if matches!(state, CachedState::Valid) {
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

#[derive(Debug, Clone, Copy)]
enum CachedState {
    Valid,
    Missing,
    Corrupt,
}

fn cached_state(path: &Utf8Path, expected: &Sha256Hex) -> CachedState {
    if !path.is_file() {
        return CachedState::Missing;
    }
    match Sha256Hex::of_file(path) {
        Ok(actual) if &actual == expected => CachedState::Valid,
        _ => CachedState::Corrupt,
    }
}

fn log_cache_state(state: CachedState, path: &Utf8Path) {
    match state {
        CachedState::Valid => log_cache_hit(path),
        CachedState::Missing => log_cache_miss(path),
        CachedState::Corrupt => log_cache_corrupt(path),
    }
}

fn log_cache_hit(path: &Utf8Path) {
    debug!(target: LOG_TARGET, path = %path, "extension archive cache hit");
}

fn log_cache_miss(path: &Utf8Path) {
    debug!(target: LOG_TARGET, path = %path, "extension archive cache miss");
}

fn log_cache_corrupt(path: &Utf8Path) {
    warn!(
        target: LOG_TARGET,
        path = %path,
        "cached extension archive digest mismatch; re-downloading"
    );
}

fn download(
    artifact: &ManifestArtifact,
    entry_dir: &Utf8Path,
    path: &Utf8Path,
) -> BootstrapResult<()> {
    let started = Instant::now();
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
    info!(
        target: LOG_TARGET,
        url = %artifact.url,
        bytes = written,
        elapsed_ms = millis(started.elapsed()),
        "downloaded extension archive"
    );
    Ok(())
}

const fn unavailable(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionArchiveUnavailable, report)
}

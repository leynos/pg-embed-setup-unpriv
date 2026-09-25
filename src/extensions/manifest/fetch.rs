//! Reading the manifest bytes from a path or a permitted URL.
//!
//! Split from the schema half because it answers a different question.
//! Acquisition is about where the bytes come from and whether they hash to
//! the pin; the schema half is about what they say once they are here.

use std::io::Read;

use color_eyre::eyre::eyre;

use super::{
    MANIFEST_SIZE_CAP,
    Manifest,
    ManifestSource,
    Sha256Hex,
    invalid,
    unavailable_manifest,
};
use crate::{
    error::{BootstrapErrorKind, BootstrapResult},
    extensions::{
        LOG_TARGET,
        extension_error,
        http::{http_get, redact_url},
    },
};

/// Fetches, verifies and parses the manifest described by `source`.
///
/// # Errors
///
/// Returns `ExtensionManifestUnavailable` when the path or URL cannot be
/// read, `ExtensionManifestDigestMismatch` when the bytes do not hash to the
/// pinned digest, and `ExtensionManifestInvalid` when parsing fails.
///
/// Crate-private, for the same reason as [`artifact_version`]: the only
/// caller is [`super::install_extensions`], and the `pub` it carried reached
/// no consumer, because this module is private and nothing re-exports it.
pub(in crate::extensions) fn load(source: &ManifestSource) -> BootstrapResult<Manifest> {
    let (bytes, pinned) = match source {
        ManifestSource::Path { path, sha256 } => (read_path(path)?, sha256.as_ref()),
        ManifestSource::Url { url, sha256 } => (fetch_url(url)?, Some(sha256)),
    };
    if let Some(expected) = pinned {
        verify_digest(&bytes, expected, source)?;
    }
    let manifest = Manifest::parse(&bytes)?;
    log_loaded(source, bytes.len(), pinned.is_some(), &manifest);
    Ok(manifest)
}

/// Records what was loaded, how large it was, and whether a digest pinned it.
fn log_loaded(source: &ManifestSource, bytes: usize, pinned: bool, manifest: &Manifest) {
    tracing::info!(
        target: LOG_TARGET,
        location = %source.location(),
        bytes,
        pinned,
        release = %manifest.release,
        extensions = manifest.extensions.len(),
        "loaded extension manifest"
    );
}

/// Reads a manifest from disk, capped at [`MANIFEST_SIZE_CAP`] plus one byte
/// so an oversized file is refused rather than buffered whole.
fn read_path(path: &camino::Utf8Path) -> BootstrapResult<Vec<u8>> {
    let file = std::fs::File::open(path)
        .map_err(|err| unavailable_manifest(eyre!("cannot open manifest at {path}: {err}")))?;
    let mut bytes = Vec::new();
    file.take(MANIFEST_SIZE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| unavailable_manifest(eyre!("cannot read manifest at {path}: {err}")))?;
    check_size(bytes, path.as_str())
}

/// Fetches a manifest over HTTP, subject to the same size cap as a local one.
fn fetch_url(url: &str) -> BootstrapResult<Vec<u8>> {
    let mut bytes = Vec::new();
    http_get(url, MANIFEST_SIZE_CAP, &mut bytes).map_err(|err| {
        unavailable_manifest(eyre!(
            "cannot fetch manifest from {}: {err}",
            redact_url(url)
        ))
    })?;
    check_size(bytes, &redact_url(url))
}

/// Rejects bytes over [`MANIFEST_SIZE_CAP`], naming where they came from.
fn check_size(bytes: Vec<u8>, location: &str) -> BootstrapResult<Vec<u8>> {
    if bytes.len() as u64 > MANIFEST_SIZE_CAP {
        return Err(invalid(eyre!(
            "manifest at {location} exceeds {MANIFEST_SIZE_CAP} bytes"
        )));
    }
    Ok(bytes)
}

fn verify_digest(
    bytes: &[u8],
    expected: &Sha256Hex,
    source: &ManifestSource,
) -> BootstrapResult<()> {
    let actual = Sha256Hex::of_bytes(bytes);
    if &actual == expected {
        return Ok(());
    }
    Err(extension_error(
        BootstrapErrorKind::ExtensionManifestDigestMismatch,
        eyre!(
            "manifest at {} hashes to {actual} but PG_EXTENSIONS_MANIFEST_SHA256 pins {expected}",
            source.location()
        ),
    ))
}

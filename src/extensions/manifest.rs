//! Manifest loading, verification, validation and artefact selection.
//!
//! The manifest is published by `df12-pg-extensions` alongside the archives it
//! describes. Every archive digest lives in the manifest, so pinning the
//! manifest digest pins the archives transitively.

use std::io::Read;

use color_eyre::eyre::{Report, eyre};
use postgresql_embedded::Version;
use serde::Deserialize;

use super::{
    ExtensionName,
    ManifestSource,
    Sha256Hex,
    archive::{http_get, is_permitted_url},
    extension_error,
};
use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult};

/// The only manifest schema this crate understands.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Upper bound on manifest size; anything larger is not a manifest.
pub(super) const MANIFEST_SIZE_CAP: u64 = 1024 * 1024;

/// A published extension manifest (`schema_version` 1).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    /// Schema version; must equal [`SUPPORTED_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Release tag the manifest belongs to, for example `v1.0.0`.
    pub release: String,
    /// Generation timestamp as published.
    pub generated_at: String,
    /// Extensions described by this manifest.
    pub extensions: Vec<ManifestExtension>,
}

/// One extension and the artefacts built for it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ManifestExtension {
    /// `CREATE EXTENSION` name.
    pub name: String,
    /// Upstream package name.
    pub package: String,
    /// Extension version, for example `0.8.6`.
    pub version: String,
    /// Where the extension was built from.
    pub source: ManifestSourceInfo,
    /// Per-`PostgreSQL`-per-target archives.
    pub artifacts: Vec<ManifestArtifact>,
}

/// Provenance of an extension build.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ManifestSourceInfo {
    /// Upstream repository URL.
    pub repository: String,
    /// Upstream tag.
    pub tag: String,
    /// Commit the tag resolved to.
    pub commit: String,
}

/// One archive: a build of an extension for one `PostgreSQL` release and target.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ManifestArtifact {
    /// Theseus release built against, for example `17.11.0`.
    pub postgresql: String,
    /// Target triple, for example `x86_64-unknown-linux-gnu`.
    pub target: String,
    /// Archive file name.
    pub file: String,
    /// Download URL.
    pub url: String,
    /// Archive digest.
    #[serde(deserialize_with = "deserialize_digest")]
    pub sha256: Sha256Hex,
    /// Archive size in bytes.
    pub size: u64,
    /// Regular files in the archive, relative to the install root.
    pub files: Vec<String>,
}

fn deserialize_digest<'de, D>(deserializer: D) -> Result<Sha256Hex, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Sha256Hex::parse(&raw).map_err(serde::de::Error::custom)
}

/// What to look for in a manifest: one name for one running server and target.
#[derive(Debug, Clone, Copy)]
pub struct ArtifactQuery<'a> {
    /// Requested `CREATE EXTENSION` name.
    pub name: &'a ExtensionName,
    /// Version of the `PostgreSQL` installed in the tree.
    pub running: &'a Version,
    /// Compile target triple.
    pub target: &'a str,
}

/// The extension and artefact chosen for a request.
#[derive(Debug, Clone, Copy)]
pub struct Selection<'a> {
    /// The extension entry the artefact belongs to.
    pub extension: &'a ManifestExtension,
    /// The artefact to install.
    pub artifact: &'a ManifestArtifact,
}

impl Manifest {
    /// Parses and validates manifest bytes.
    ///
    /// # Errors
    ///
    /// Returns `ExtensionManifestInvalid` for invalid JSON, a schema version
    /// other than 1, a missing field, a malformed digest, an unparsable
    /// `postgresql` version, an archive `file` that is not a single path
    /// component, or an empty `files` list.
    pub fn parse(bytes: &[u8]) -> BootstrapResult<Self> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|err| invalid(eyre!("manifest is not valid JSON for schema 1: {err}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn validate(&self) -> BootstrapResult<()> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(invalid(eyre!(
                "manifest schema_version {} is not supported; this crate understands {}",
                self.schema_version,
                SUPPORTED_SCHEMA_VERSION
            )));
        }
        for extension in &self.extensions {
            ExtensionName::new(extension.name.as_str())
                .map_err(|err| invalid(eyre!("manifest extension name: {err}")))?;
            for artifact in &extension.artifacts {
                validate_artifact(&extension.name, artifact)?;
            }
        }
        Ok(())
    }

    /// Selects the artefact for `name` matching the running `PostgreSQL` major
    /// and `target`; the minor is not a match key.
    ///
    /// # Errors
    ///
    /// Returns `ExtensionUnavailable` when no artefact matches; the message
    /// lists what the manifest offers for that name.
    pub fn select<'a>(
        &'a self,
        query: ArtifactQuery<'_>,
        source: &ManifestSource,
    ) -> BootstrapResult<Selection<'a>> {
        let ArtifactQuery {
            name,
            running,
            target,
        } = query;
        let Some(extension) = self
            .extensions
            .iter()
            .find(|extension| extension.name == name.as_str())
        else {
            return Err(unavailable(eyre!(
                "manifest at {} lists no extension named {name}; it offers: {}",
                source.location(),
                self.extension_names().join(", ")
            )));
        };
        let artifact = extension
            .artifacts
            .iter()
            .find(|artifact| artifact_matches(artifact, running, target))
            .ok_or_else(|| unavailable(no_artifact_report(extension, running, target, source)))?;
        Ok(Selection {
            extension,
            artifact,
        })
    }

    fn extension_names(&self) -> Vec<&str> {
        self.extensions
            .iter()
            .map(|extension| extension.name.as_str())
            .collect()
    }
}

fn validate_artifact(name: &str, artifact: &ManifestArtifact) -> BootstrapResult<()> {
    artifact_version(artifact).ok_or_else(|| {
        invalid(eyre!(
            "{name}: artefact postgresql {:?} is not a version",
            artifact.postgresql
        ))
    })?;
    if !is_single_component(&artifact.file) {
        return Err(invalid(eyre!(
            "{name}: artefact file {:?} must be a bare file name",
            artifact.file
        )));
    }
    if artifact.files.is_empty() {
        return Err(invalid(eyre!(
            "{name}: artefact {} lists no files",
            artifact.file
        )));
    }
    if !is_permitted_url(&artifact.url) {
        return Err(invalid(eyre!(
            "{name}: artefact url {:?} must use https:// (loopback http is the only exception)",
            artifact.url
        )));
    }
    Ok(())
}

fn is_single_component(file: &str) -> bool {
    !file.is_empty() && !file.contains('/') && !file.contains('\\') && file != "." && file != ".."
}

/// Parses an artefact's `postgresql` field as a Theseus version.
#[must_use]
pub fn artifact_version(artifact: &ManifestArtifact) -> Option<Version> {
    Version::parse(&artifact.postgresql).ok()
}

/// An archive built for one `PostgreSQL` major loads into every minor of that
/// major: the server's `Pg_magic_func` block checks the major and the layout
/// constants, not the minor, and modules built before 16.5 load into 16.15.
/// The Theseus release in the manifest is therefore information, not a key.
fn artifact_matches(artifact: &ManifestArtifact, running: &Version, target: &str) -> bool {
    artifact_version(artifact)
        .is_some_and(|built| built.major == running.major && artifact.target == target)
}

fn no_artifact_report(
    extension: &ManifestExtension,
    running: &Version,
    target: &str,
    source: &ManifestSource,
) -> Report {
    let offered: Vec<String> = extension
        .artifacts
        .iter()
        .map(|artifact| format!("{} on {}", artifact.postgresql, artifact.target))
        .collect();
    eyre!(
        "manifest at {} has no {} archive for PostgreSQL {} on {target}; it offers: {}",
        source.location(),
        extension.name,
        running.major,
        if offered.is_empty() {
            "nothing".to_owned()
        } else {
            offered.join(", ")
        }
    )
}

/// Fetches, verifies and parses the manifest described by `source`.
///
/// # Errors
///
/// Returns `ExtensionManifestUnavailable` when the path or URL cannot be
/// read, `ExtensionManifestDigestMismatch` when the bytes do not hash to the
/// pinned digest, and `ExtensionManifestInvalid` when parsing fails.
pub fn load(source: &ManifestSource) -> BootstrapResult<Manifest> {
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

fn log_loaded(source: &ManifestSource, bytes: usize, pinned: bool, manifest: &Manifest) {
    tracing::info!(
        target: super::LOG_TARGET,
        location = %source.location(),
        bytes,
        pinned,
        release = %manifest.release,
        extensions = manifest.extensions.len(),
        "loaded extension manifest"
    );
}

fn read_path(path: &camino::Utf8Path) -> BootstrapResult<Vec<u8>> {
    let file = std::fs::File::open(path)
        .map_err(|err| unavailable_manifest(eyre!("cannot open manifest at {path}: {err}")))?;
    let mut bytes = Vec::new();
    file.take(MANIFEST_SIZE_CAP + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| unavailable_manifest(eyre!("cannot read manifest at {path}: {err}")))?;
    check_size(bytes, path.as_str())
}

fn fetch_url(url: &str) -> BootstrapResult<Vec<u8>> {
    let mut bytes = Vec::new();
    http_get(url, MANIFEST_SIZE_CAP, &mut bytes)
        .map_err(|err| unavailable_manifest(eyre!("cannot fetch manifest from {url}: {err}")))?;
    check_size(bytes, url)
}

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

const fn invalid(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionManifestInvalid, report)
}

const fn unavailable(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionUnavailable, report)
}

const fn unavailable_manifest(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionManifestUnavailable, report)
}

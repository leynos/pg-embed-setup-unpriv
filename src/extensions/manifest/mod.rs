//! Manifest schema, validation, acquisition and artefact selection.
//!
//! The manifest is published by `df12-pg-extensions` alongside the archives it
//! describes. Every archive digest lives in the manifest, so pinning the
//! manifest digest pins the archives transitively.

use color_eyre::eyre::{Report, eyre};
use postgresql_embedded::Version;
use serde::Deserialize;

use super::{ExtensionName, Sha256Hex, extension_error, http::is_permitted_url};
use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult};

mod fetch;
mod select;
mod source;

pub(super) use self::fetch::load;
pub use self::{
    select::{ArtifactQuery, Selection},
    source::ManifestSource,
};

/// The only manifest schema this crate understands.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Upper bound on manifest size; anything larger is not a manifest.
pub(super) const MANIFEST_SIZE_CAP: u64 = 1024 * 1024;

/// Upper bound on the declared size of one archive.
///
/// A `size` is a length the crate will read and buffer, so an unbounded value
/// is a denial of service in the manifest: `u64::MAX` would ask for a
/// `u64::MAX`-byte read limit. Real extension archives are hundreds of
/// kilobytes (`pgvector` is under one megabyte), so 256 MiB leaves several
/// orders of magnitude of headroom while keeping the value representable and
/// the buffer bounded.
pub(super) const ARCHIVE_SIZE_CAP: u64 = 256 * 1024 * 1024;

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

/// Deserializes a digest string, rejecting anything but 64 lower-case hex.
fn deserialize_digest<'de, D>(deserializer: D) -> Result<Sha256Hex, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Sha256Hex::parse(&raw).map_err(serde::de::Error::custom)
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
    ///
    /// # Examples
    ///
    /// ```
    /// use pg_embedded_setup_unpriv::extensions::Manifest;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let manifest = Manifest::parse(MANIFEST.as_bytes())?;
    /// assert_eq!(manifest.extensions.len(), 1);
    /// assert_eq!(manifest.extensions[0].name, "vector");
    ///
    /// // A schema this crate does not implement is refused rather than read
    /// // on a best-effort basis.
    /// let wrong = MANIFEST.replace("\"schema_version\": 1", "\"schema_version\": 2");
    /// assert!(Manifest::parse(wrong.as_bytes()).is_err());
    /// # Ok(())
    /// # }
    /// # const MANIFEST: &str = r#"{
    /// #   "schema_version": 1,
    /// #   "release": "v1.0.0",
    /// #   "generated_at": "2026-09-05T00:00:00+00:00",
    /// #   "extensions": [{
    /// #     "name": "vector",
    /// #     "package": "pgvector",
    /// #     "version": "0.8.6",
    /// #     "source": {
    /// #       "repository": "https://github.com/pgvector/pgvector",
    /// #       "tag": "v0.8.6",
    /// #       "commit": "0123456789abcdef0123456789abcdef01234567"
    /// #     },
    /// #     "artifacts": [{
    /// #       "postgresql": "17.11.0",
    /// #       "target": "x86_64-unknown-linux-gnu",
    /// #       "file": "vector.tar.gz",
    /// #       "url": "https://example.invalid/vector.tar.gz",
    /// #       "sha256": "0000000000000000000000000000000000000000000000000000000000000000",
    /// #       "size": 1024,
    /// #       "files": ["lib/vector.so"]
    /// #     }]
    /// #   }]
    /// # }"#;
    /// ```
    pub fn parse(bytes: &[u8]) -> BootstrapResult<Self> {
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|err| invalid(eyre!("manifest is not valid JSON for schema 1: {err}")))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Applies every whole-manifest rule: the schema version, each extension
    /// name, and each artefact.
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

    /// The names this manifest offers, for the "no such extension" message.
    fn extension_names(&self) -> Vec<&str> {
        self.extensions
            .iter()
            .map(|extension| extension.name.as_str())
            .collect()
    }
}

/// Checks one artefact's version, file name, size, file list and URL.
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
    if artifact.size == 0 || artifact.size > ARCHIVE_SIZE_CAP {
        return Err(invalid(eyre!(
            "{name}: artefact {} declares size {} bytes; the size must be between 1 and \
             {ARCHIVE_SIZE_CAP}",
            artifact.file,
            artifact.size
        )));
    }
    if artifact.files.is_empty() {
        return Err(invalid(eyre!(
            "{name}: artefact {} lists no files",
            artifact.file
        )));
    }
    if !is_permitted_url(&artifact.url) {
        // Redacted like every other rendering of a configured URL: a
        // manifest is consumer-supplied, so a rejected artefact URL can
        // still carry userinfo, a signed query or a token in its fragment,
        // and this error is returned to a caller that may print it.
        return Err(invalid(eyre!(
            "{name}: artefact url {:?} must use https:// (loopback http is the only exception)",
            super::http::redact_url(&artifact.url)
        )));
    }
    Ok(())
}

/// True when `file` is a bare file name with no separators.
fn is_single_component(file: &str) -> bool {
    !file.is_empty() && !file.contains('/') && !file.contains('\\') && file != "." && file != ".."
}

/// Parses an artefact's `postgresql` field as a Theseus version.
///
/// `None` cannot arise for an artefact reached through a parsed [`Manifest`]:
/// [`Manifest::parse`] rejects an unparsable `postgresql` field, so the
/// option exists only because this function is also the thing that decides
/// parsability.
///
/// Crate-private. It was `pub` inside a private module, which reaches no
/// consumer: nothing re-exports it from [`super`], and `cargo doc` renders
/// no page for it. The `pub` claimed a surface that did not exist, and the
/// documentation rule asking every public function for an example does not
/// reach a function no reader can find. (`cargo test --doc` does collect an
/// example from such an item, so one would have been checked; it would
/// simply have documented an unreachable function.)
fn artifact_version(artifact: &ManifestArtifact) -> Option<Version> {
    Version::parse(&artifact.postgresql).ok()
}

/// Wraps `report` as an `ExtensionManifestInvalid` failure.
const fn invalid(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionManifestInvalid, report)
}

/// Wraps `report` as an `ExtensionUnavailable` failure.
const fn unavailable(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionUnavailable, report)
}

/// Wraps `report` as an `ExtensionManifestUnavailable` failure.
const fn unavailable_manifest(report: Report) -> BootstrapError {
    extension_error(BootstrapErrorKind::ExtensionManifestUnavailable, report)
}

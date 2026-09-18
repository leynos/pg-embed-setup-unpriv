//! Choosing one artefact for the running server and compile target.
//!
//! Split from the schema half because it answers a different question.
//! Parsing asks whether the manifest is well formed; this asks which of its
//! rows describes the server about to start, and the answer is deliberately
//! narrower than what would load.

use color_eyre::eyre::{Report, eyre};
use postgresql_embedded::Version;

use super::{
    Manifest,
    ManifestArtifact,
    ManifestExtension,
    ManifestSource,
    artifact_version,
    unavailable,
};
use crate::{error::BootstrapResult, extensions::ExtensionName};

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
    /// Selects the artefact for `name` matching the running `PostgreSQL` major
    /// and minor and the compile `target`.
    ///
    /// Theseus's third version component is a build number rather than a
    /// `PostgreSQL` release, so it is not compared. There is no cross-minor
    /// fallback: see [`ArtifactQuery`].
    ///
    /// # Errors
    ///
    /// Returns `ExtensionUnavailable` when no artefact matches; the message
    /// lists what the manifest offers for that name.
    ///
    /// # Examples
    ///
    /// ```
    /// use camino::Utf8PathBuf;
    /// use pg_embedded_setup_unpriv::extensions::{
    ///     ArtifactQuery,
    ///     ExtensionName,
    ///     Manifest,
    ///     ManifestSource,
    /// };
    /// use postgresql_embedded::Version;
    ///
    /// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    /// let manifest = Manifest::parse(MANIFEST.as_bytes())?;
    /// let name = ExtensionName::new("vector")?;
    /// let source = ManifestSource::Path {
    ///     path: Utf8PathBuf::from("/srv/extensions/manifest.json"),
    ///     sha256: None,
    /// };
    ///
    /// let running = Version::new(17, 11, 0);
    /// let chosen = manifest.select(
    ///     ArtifactQuery {
    ///         name: &name,
    ///         running: &running,
    ///         target: "x86_64-unknown-linux-gnu",
    ///     },
    ///     &source,
    /// )?;
    /// assert_eq!(chosen.artifact.postgresql, "17.11.0");
    ///
    /// // A neighbouring minor is a miss, not a fallback: the manifest pins one
    /// // digest per version, and 17.10 is a different row.
    /// let other = Version::new(17, 10, 0);
    /// assert!(
    ///     manifest
    ///         .select(
    ///             ArtifactQuery {
    ///                 name: &name,
    ///                 running: &other,
    ///                 target: "x86_64-unknown-linux-gnu"
    ///             },
    ///             &source,
    ///         )
    ///         .is_err()
    /// );
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
}

/// Both components of the `PostgreSQL` version must match, and the compile
/// target with them.
///
/// An archive built for one major would in fact load into every minor of that
/// major: the server's `Pg_magic_func` block checks the major and the layout
/// constants, not the minor. Selection is narrower than loading on purpose.
/// The manifest pins one digest per (name, version, target) row, so accepting
/// a neighbouring minor would install bytes the consumer's pinned manifest
/// digest does not describe for the server actually running, and the exactness
/// of the chain is the point of the hook. Issue #222 states the rule and puts
/// cross-minor fallback out of scope, behind an explicit opt-in if the estate
/// ever wants it.
///
/// Theseus's third component is a build number rather than a `PostgreSQL`
/// release, so it is not compared.
fn artifact_matches(artifact: &ManifestArtifact, running: &Version, target: &str) -> bool {
    artifact_version(artifact).is_some_and(|built| {
        built.major == running.major && built.minor == running.minor && artifact.target == target
    })
}

/// Builds the `ExtensionUnavailable` message listing what the manifest offers.
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
        "manifest at {} has no {} archive for PostgreSQL {}.{} on {target}; it offers: {}",
        source.location(),
        extension.name,
        running.major,
        running.minor,
        if offered.is_empty() {
            "nothing".to_owned()
        } else {
            offered.join(", ")
        }
    )
}

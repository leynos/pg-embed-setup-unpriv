//! Prebuilt extension installation for embedded `PostgreSQL` trees.
//!
//! The Theseus binaries carry only the in-tree contrib extensions. This module
//! installs additional, prebuilt extensions (for example `pgvector`) from a
//! digest-pinned manifest before the server starts. It never compiles
//! anything: when no matching, verified archive exists it fails closed.
//!
//! Consumers declare extensions through the environment:
//!
//! | Variable | Meaning |
//! | --- | --- |
//! | `PG_EXTENSIONS` | Comma-separated `CREATE EXTENSION` names |
//! | `PG_EXTENSIONS_MANIFEST` | `https://` URL or filesystem path of `manifest.json` |
//! | `PG_EXTENSIONS_MANIFEST_SHA256` | Hex SHA-256 of the manifest (required for HTTPS) |
//! | `PG_EXTENSIONS_CACHE_DIR` | Where verified archives are kept between runs |
//!
//! [`TestCluster`](crate::TestCluster), [`bootstrap_for_tests`](crate::bootstrap_for_tests)
//! and the `pg_embedded_setup_unpriv` CLI honour them automatically; consumers
//! that manage their own `Settings` can call [`install_extensions`] directly.
//!
//! # Examples
//!
//! ```no_run
//! use camino::Utf8Path;
//! use pg_embedded_setup_unpriv::{
//!     PgEnvCfg,
//!     extensions::{ExtensionRequest, install_extensions},
//! };
//!
//! # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//! let cfg = PgEnvCfg::load()?;
//! if let Some(request) = ExtensionRequest::from_config(&cfg)? {
//!     let installed = install_extensions(&request, Utf8Path::new("/var/tmp/pg/install/17.11.0"))?;
//!     for extension in installed {
//!         println!(
//!             "{} {} ({} files)",
//!             extension.name,
//!             extension.version,
//!             extension.files.len()
//!         );
//!     }
//! }
//! # Ok(())
//! # }
//! ```

mod archive;
mod config;
mod digest;
mod install;
mod manifest;
mod name;
mod version;

#[cfg(test)]
mod tests;

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::Report;
use tracing::info;

pub use self::{
    archive::is_permitted_url,
    config::{ExtensionCacheConfig, resolve_extension_cache_dir},
    digest::{InvalidDigest, Sha256Hex},
    install::{ALLOWED_PREFIXES, classify_entry_path},
    manifest::{
        ArtifactQuery,
        Manifest,
        ManifestArtifact,
        ManifestExtension,
        ManifestSourceInfo,
        SUPPORTED_SCHEMA_VERSION,
    },
    name::ExtensionName,
    version::{parse_pg_config_version, running_version},
};
use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult};

/// Observability target for extension installation events.
pub(crate) const LOG_TARGET: &str = "pg_embed::extensions";

/// Where the manifest comes from and how it is verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    /// Fetched over HTTPS; the digest is mandatory so the pin is complete.
    Url {
        /// The `https://` URL of `manifest.json`.
        url: String,
        /// Expected SHA-256 of the manifest bytes.
        sha256: Sha256Hex,
    },
    /// Read from the filesystem; a local manifest is trusted like local source,
    /// so the digest is optional.
    Path {
        /// Path of `manifest.json`.
        path: Utf8PathBuf,
        /// Expected SHA-256 of the manifest bytes, when pinned.
        sha256: Option<Sha256Hex>,
    },
}

impl ManifestSource {
    /// Renders the location for error messages and logs.
    #[must_use]
    pub fn location(&self) -> String {
        match self {
            Self::Url { url, .. } => url.clone(),
            Self::Path { path, .. } => path.to_string(),
        }
    }
}

/// A validated declaration of which extensions to install and from where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRequest {
    /// Extension names in declaration order, deduplicated.
    pub names: Vec<ExtensionName>,
    /// Manifest location and pin.
    pub manifest: ManifestSource,
    /// Directory holding verified archives between runs.
    pub cache_dir: Utf8PathBuf,
}

/// Whether an archive came from the local cache or was downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveOrigin {
    /// A verified copy already existed under the cache directory.
    Cached,
    /// The archive was downloaded and verified during this install.
    Downloaded,
}

/// What the hook did for one extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledExtension {
    /// The `CREATE EXTENSION` name.
    pub name: ExtensionName,
    /// Extension version from the manifest, for example `0.8.6`.
    pub version: String,
    /// Theseus release the archive was built against, for example `17.11.0`.
    pub postgresql: String,
    /// Target triple of the archive, for example `x86_64-unknown-linux-gnu`.
    pub target: String,
    /// Digest of the archive that was installed.
    pub archive_sha256: Sha256Hex,
    /// Whether the archive came from the cache or was downloaded.
    pub origin: ArchiveOrigin,
    /// Installed files relative to the install root, sorted.
    pub files: Vec<Utf8PathBuf>,
}

/// The compile target of this crate, matching Theseus asset names.
#[must_use]
pub const fn compile_target() -> &'static str { env!("PG_EMBED_TARGET") }

/// Builds a categorized extension error.
pub(crate) const fn extension_error(kind: BootstrapErrorKind, report: Report) -> BootstrapError {
    BootstrapError::new(kind, report)
}

/// Installs the requested extensions into the embedded tree at `install_dir`.
///
/// `install_dir` is the versioned `PostgreSQL` root (the directory holding
/// `bin/`, `lib/` and `share/`). The manifest is fetched and verified, one
/// artefact is selected per name for the running major and minor and the
/// compile target, each archive is verified against the manifest digest, and
/// its files are validated in full before any file is written. Re-installing
/// an archive that is already in place is a reporting no-op.
///
/// Every requested name is resolved against the manifest before any archive
/// is acquired or written, so an unknown name or an unmatched version fails
/// with nothing on disk; a failure while acquiring or writing a later archive
/// leaves the earlier archives installed and names them in the error.
///
/// The function is synchronous and blocks the calling thread for the whole
/// install. When called from inside a Tokio runtime it moves the work to a
/// scoped helper thread so the blocking HTTP client never runs on an
/// executor thread, but the calling task still waits; on a current-thread
/// runtime that parks the only worker, so async code should prefer
/// [`install_extensions_async`], which uses `spawn_blocking`.
///
/// # Errors
///
/// Fails closed with one of the `BootstrapErrorKind::Extension*` kinds; see
/// the crate documentation for the full table. Nothing is compiled and the
/// server is not started when an error is returned.
///
/// # Examples
///
/// ```no_run
/// use camino::{Utf8Path, Utf8PathBuf};
/// use pg_embedded_setup_unpriv::extensions::{
///     ExtensionName,
///     ExtensionRequest,
///     ManifestSource,
///     install_extensions,
/// };
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let request = ExtensionRequest {
///     names: vec![ExtensionName::new("vector")?],
///     manifest: ManifestSource::Path {
///         path: Utf8PathBuf::from("/srv/extensions/manifest.json"),
///         sha256: None,
///     },
///     cache_dir: Utf8PathBuf::from("/var/cache/pg-embedded/extensions"),
/// };
/// let installed = install_extensions(&request, Utf8Path::new("/var/tmp/pg/install/17.11.0"))?;
/// assert_eq!(installed.len(), 1);
/// # Ok(())
/// # }
/// ```
pub fn install_extensions(
    request: &ExtensionRequest,
    install_dir: &Utf8Path,
) -> BootstrapResult<Vec<InstalledExtension>> {
    if tokio::runtime::Handle::try_current().is_ok() {
        std::thread::scope(|scope| {
            scope
                .spawn(|| install_extensions_blocking(request, install_dir))
                .join()
                .unwrap_or_else(|payload| Err(install_thread_panic(payload)))
        })
    } else {
        install_extensions_blocking(request, install_dir)
    }
}

/// Async form of [`install_extensions`] that runs the work on the Tokio
/// blocking pool.
///
/// # Errors
///
/// Returns the same errors as [`install_extensions`], plus
/// `ExtensionInstallFailed` when the blocking task cannot be joined.
pub async fn install_extensions_async(
    request: ExtensionRequest,
    install_dir: Utf8PathBuf,
) -> BootstrapResult<Vec<InstalledExtension>> {
    tokio::task::spawn_blocking(move || install_extensions_blocking(&request, &install_dir))
        .await
        .unwrap_or_else(|err| {
            Err(extension_error(
                BootstrapErrorKind::ExtensionInstallFailed,
                Report::new(err).wrap_err("extension install task failed"),
            ))
        })
}

fn install_thread_panic(payload: Box<dyn std::any::Any + Send>) -> BootstrapError {
    let message = crate::cluster::panic_utils::panic_payload_to_string(payload);
    extension_error(
        BootstrapErrorKind::ExtensionInstallFailed,
        Report::msg(format!("extension install thread panicked: {message}")),
    )
}

/// Runs the install pipeline on the current thread.
fn install_extensions_blocking(
    request: &ExtensionRequest,
    install_dir: &Utf8Path,
) -> BootstrapResult<Vec<InstalledExtension>> {
    let span = tracing::info_span!(
        target: LOG_TARGET,
        "install_extensions",
        install_dir = %install_dir,
        names = request.names.len()
    );
    let _entered = span.enter();
    let manifest = manifest::load(&request.manifest)?;
    let running = running_version(install_dir)?;
    let selections = select_all(request, &manifest, &running)?;
    request
        .names
        .iter()
        .zip(selections)
        .map(|(name, selection)| install_one(request, install_dir, name, selection))
        .collect()
}

/// Resolves every requested name before anything is acquired or written.
fn select_all<'a>(
    request: &ExtensionRequest,
    manifest: &'a Manifest,
    running: &postgresql_embedded::Version,
) -> BootstrapResult<Vec<manifest::Selection<'a>>> {
    request
        .names
        .iter()
        .map(|name| {
            let query = ArtifactQuery {
                name,
                running,
                target: compile_target(),
            };
            manifest.select(query, &request.manifest)
        })
        .collect()
}

/// Acquires and installs one already-selected extension.
fn install_one(
    request: &ExtensionRequest,
    install_dir: &Utf8Path,
    name: &ExtensionName,
    selection: manifest::Selection<'_>,
) -> BootstrapResult<InstalledExtension> {
    tracing::debug!(
        target: LOG_TARGET,
        name = %name,
        version = %selection.extension.version,
        postgresql = %selection.artifact.postgresql,
        target = %selection.artifact.target,
        file = %selection.artifact.file,
        "selected extension artefact"
    );
    let acquired = archive::acquire(&request.cache_dir, selection.artifact)?;
    let files = install::install_archive(&acquired.path, selection.artifact, install_dir)?;
    let installed = InstalledExtension {
        name: name.clone(),
        version: selection.extension.version.clone(),
        postgresql: selection.artifact.postgresql.clone(),
        target: selection.artifact.target.clone(),
        archive_sha256: selection.artifact.sha256.clone(),
        origin: acquired.origin,
        files,
    };
    log_installed(&installed);
    Ok(installed)
}

fn log_installed(installed: &InstalledExtension) {
    info!(
        target: LOG_TARGET,
        name = %installed.name,
        version = %installed.version,
        postgresql = %installed.postgresql,
        target = %installed.target,
        origin = ?installed.origin,
        files = installed.files.len(),
        "installed extension"
    );
}

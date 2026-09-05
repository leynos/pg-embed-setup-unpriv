//! Domain error types for the embedded `PostgreSQL` bootstrapper.

use color_eyre::Report;
use thiserror::Error;

/// Result alias for operations that may return a [`PgEmbeddedError`].
pub type Result<T> = std::result::Result<T, PgEmbeddedError>;

/// Result alias for bootstrap-specific fallible operations.
pub type BootstrapResult<T> = std::result::Result<T, BootstrapError>;

/// Result alias for privilege-management fallible operations.
pub type PrivilegeResult<T> = std::result::Result<T, PrivilegeError>;

/// Result alias for configuration fallible operations.
pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// Top-level error exposed by the crate.
#[derive(Debug, Error)]
pub enum PgEmbeddedError {
    /// Indicates bootstrap initialization failed.
    #[error("bootstrap failed: {0}")]
    Bootstrap(#[from] BootstrapError),
    /// Indicates privilege management failed.
    #[error("privilege management failed: {0}")]
    Privilege(#[from] PrivilegeError),
    /// Indicates configuration parsing failed.
    #[error("configuration parsing failed: {0}")]
    Config(#[from] ConfigError),
}

/// Categorizes bootstrap failures so callers can branch on structured errors.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum BootstrapErrorKind {
    /// Represents errors without a more specific semantic meaning.
    #[default]
    Other,
    /// Indicates the configured worker binary is missing from disk.
    WorkerBinaryMissing,
    /// Indicates a PATH entry used for worker discovery is not valid UTF-8.
    WorkerBinaryPathNonUtf8,
    /// `PG_EXTENSIONS*` is set but incomplete or malformed (no manifest, an
    /// HTTPS manifest without a digest, a bad name, or a bad digest).
    ExtensionConfigInvalid,
    /// The extension manifest could not be read from its path or URL.
    ExtensionManifestUnavailable,
    /// The manifest bytes do not hash to `PG_EXTENSIONS_MANIFEST_SHA256`.
    ExtensionManifestDigestMismatch,
    /// The manifest is not valid JSON, has the wrong schema version, or is
    /// missing a field.
    ExtensionManifestInvalid,
    /// The manifest offers no artifact for the requested name, the running
    /// `PostgreSQL` major and minor, and the compile target.
    ExtensionUnavailable,
    /// The extension archive could not be downloaded and no cached copy exists.
    ExtensionArchiveUnavailable,
    /// The archive bytes do not hash to the digest recorded in the manifest.
    ExtensionArchiveDigestMismatch,
    /// The archive contains a forbidden entry, escapes its prefixes, or does
    /// not match the file list recorded in the manifest.
    ExtensionArchiveInvalid,
    /// Writing the extension files into the installation tree failed.
    ExtensionInstallFailed,
}

/// Captures bootstrap-specific failures.
#[derive(Debug, Error)]
#[error("{report}")]
pub struct BootstrapError {
    kind: BootstrapErrorKind,
    #[source]
    report: Report,
}

impl BootstrapError {
    /// Constructs a new bootstrap error with the provided kind and diagnostic
    /// report.
    #[must_use]
    pub const fn new(kind: BootstrapErrorKind, report: Report) -> Self { Self { kind, report } }

    /// Returns the semantic category for this bootstrap failure.
    #[must_use]
    pub const fn kind(&self) -> BootstrapErrorKind { self.kind }

    /// Extracts the underlying diagnostic report.
    pub fn into_report(self) -> Report { self.report }
}

impl From<Report> for BootstrapError {
    fn from(report: Report) -> Self { Self::new(BootstrapErrorKind::Other, report) }
}

impl From<PrivilegeError> for BootstrapError {
    fn from(err: PrivilegeError) -> Self {
        let PrivilegeError(report) = err;
        Self::new(BootstrapErrorKind::Other, report)
    }
}

impl From<ConfigError> for BootstrapError {
    fn from(err: ConfigError) -> Self {
        let ConfigError(report) = err;
        Self::new(BootstrapErrorKind::Other, report)
    }
}

impl From<PgEmbeddedError> for BootstrapError {
    fn from(err: PgEmbeddedError) -> Self {
        match err {
            PgEmbeddedError::Bootstrap(inner) => inner,
            PgEmbeddedError::Privilege(inner) => inner.into(),
            PgEmbeddedError::Config(inner) => inner.into(),
        }
    }
}

/// Captures privilege-management failures.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct PrivilegeError(#[from] Report);

/// Captures configuration failures.
#[derive(Debug, Error)]
#[error(transparent)]
pub struct ConfigError(#[from] Report);

#[cfg(test)]
mod tests {
    //! Unit tests for error display formats.

    use color_eyre::eyre::eyre;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::bootstrap(
        "PG_EMBEDDED_WORKER must be set",
        "bootstrap failed:",
        |msg: &str| PgEmbeddedError::Bootstrap(BootstrapError::from(eyre!("{}", msg)))
    )]
    #[case::privilege(
        "failed to drop privileges",
        "privilege management failed:",
        |msg: &str| PgEmbeddedError::Privilege(PrivilegeError::from(eyre!("{}", msg)))
    )]
    #[case::config(
        "invalid port number",
        "configuration parsing failed:",
        |msg: &str| PgEmbeddedError::Config(ConfigError::from(eyre!("{}", msg)))
    )]
    fn pg_embedded_error_includes_inner_message(
        #[case] inner_message: &str,
        #[case] expected_prefix: &str,
        #[case] constructor: fn(&str) -> PgEmbeddedError,
    ) {
        let pg_err = constructor(inner_message);

        let display = pg_err.to_string();

        assert!(
            display.contains(expected_prefix),
            "expected '{expected_prefix}' prefix, got: {display}"
        );
        assert!(
            display.contains(inner_message),
            "expected inner message '{inner_message}' in display, got: {display}"
        );
    }

    #[test]
    fn bootstrap_error_displays_report_message() {
        let inner_message = "database connection failed";
        let err = BootstrapError::from(eyre!(inner_message));

        let display = err.to_string();

        assert!(
            display.contains(inner_message),
            "expected '{inner_message}' in display, got: {display}"
        );
    }

    #[test]
    fn bootstrap_error_kind_defaults_to_other() {
        assert_eq!(BootstrapErrorKind::default(), BootstrapErrorKind::Other);
    }

    #[test]
    fn bootstrap_error_preserves_kind_and_into_report() {
        let err = BootstrapError::new(
            BootstrapErrorKind::WorkerBinaryMissing,
            eyre!("worker gone"),
        );
        assert_eq!(err.kind(), BootstrapErrorKind::WorkerBinaryMissing);
        assert!(err.into_report().to_string().contains("worker gone"));
    }

    #[rstest]
    #[case::config(BootstrapErrorKind::ExtensionConfigInvalid)]
    #[case::manifest_unavailable(BootstrapErrorKind::ExtensionManifestUnavailable)]
    #[case::manifest_digest(BootstrapErrorKind::ExtensionManifestDigestMismatch)]
    #[case::manifest_invalid(BootstrapErrorKind::ExtensionManifestInvalid)]
    #[case::unavailable(BootstrapErrorKind::ExtensionUnavailable)]
    #[case::archive_unavailable(BootstrapErrorKind::ExtensionArchiveUnavailable)]
    #[case::archive_digest(BootstrapErrorKind::ExtensionArchiveDigestMismatch)]
    #[case::archive_invalid(BootstrapErrorKind::ExtensionArchiveInvalid)]
    #[case::install_failed(BootstrapErrorKind::ExtensionInstallFailed)]
    fn extension_error_kinds_survive_wrapping(#[case] kind: BootstrapErrorKind) {
        let err = BootstrapError::new(kind, eyre!("extension detail"));
        assert_eq!(err.kind(), kind);
        let wrapped = BootstrapError::from(PgEmbeddedError::Bootstrap(err));
        assert_eq!(wrapped.kind(), kind);
        assert!(wrapped.to_string().contains("extension detail"));
    }

    #[test]
    fn bootstrap_error_from_privilege_error_is_other() {
        let privilege = PrivilegeError::from(eyre!("no privileges"));
        let err = BootstrapError::from(privilege);
        assert_eq!(err.kind(), BootstrapErrorKind::Other);
        assert!(err.to_string().contains("no privileges"));
    }

    #[test]
    fn bootstrap_error_from_config_error_is_other() {
        let config = ConfigError::from(eyre!("bad config"));
        let err = BootstrapError::from(config);
        assert_eq!(err.kind(), BootstrapErrorKind::Other);
        assert!(err.to_string().contains("bad config"));
    }

    #[rstest]
    #[case::bootstrap(
        |report| PgEmbeddedError::Bootstrap(BootstrapError::new(
            BootstrapErrorKind::WorkerBinaryPathNonUtf8,
            report,
        )),
        BootstrapErrorKind::WorkerBinaryPathNonUtf8,
    )]
    #[case::privilege(
        |report| PgEmbeddedError::Privilege(PrivilegeError::from(report)),
        BootstrapErrorKind::Other,
    )]
    #[case::config(
        |report| PgEmbeddedError::Config(ConfigError::from(report)),
        BootstrapErrorKind::Other,
    )]
    fn bootstrap_error_from_pg_embedded_error_maps_each_variant(
        #[case] constructor: fn(Report) -> PgEmbeddedError,
        #[case] expected_kind: BootstrapErrorKind,
    ) {
        let pg_err = constructor(eyre!("inner detail"));
        let err = BootstrapError::from(pg_err);
        assert_eq!(err.kind(), expected_kind);
        assert!(err.to_string().contains("inner detail"));
    }
}

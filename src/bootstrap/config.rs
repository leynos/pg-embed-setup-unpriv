//! Explicit configuration for a test bootstrap.
//!
//! [`TestBootstrapConfig`] carries every input a test bootstrap would otherwise
//! read from the process environment. Callers that already resolved their
//! configuration can pass it to
//! [`bootstrap_for_tests_with`](crate::bootstrap_for_tests_with) and avoid
//! ambient reads such as `PG_EMBEDDED_WORKER`, `PG_SHUTDOWN_TIMEOUT_SECS` and
//! `PG_TEST_BACKEND`.
//!
//! Absent explicit controls keep the built-in defaults: the shutdown timeout
//! defaults to 15 seconds, the worker binary is discovered on `PATH` when
//! privilege dropping needs one, and the release URL keeps the value produced
//! by [`PgEnvCfg::to_settings_for_tests`].
//!
//! # Remaining ambient inputs
//!
//! Explicit configuration removes the `PG_*` reads that select the cluster's
//! settings, but a few inputs are still read from the process environment
//! after resolution. They cannot be supplied through this type:
//!
//! | Input | When it is read |
//! | --- | --- |
//! | `PG_EMBED_DURABLE` | converting the [`PgEnvCfg`] into settings for a test bootstrap |
//! | `TZ`, `TZDIR` | preparing the cluster environment |
//! | `PATH` | discovering the worker binary when no explicit worker is supplied |
//! | binary cache fallbacks | resolving the cache directory when `binary_cache_dir` is unset |
//! | extension cache fallbacks | resolving the cache directory when `extensions_cache_dir` is unset |
//! | `PG_EMBED_ORPHAN_WATCHER` | starting the cluster |
//! | privilege detection | deciding whether the worker path applies at all |
//! | `Settings::default()` lookups | home and current-directory fallbacks inside `postgresql_embedded` |
//!
//! The prepared environment is still applied to the process for the lifetime
//! of the cluster through `ScopedEnv::apply`; only the inputs used to build it
//! change.
//!
//! # Examples
//!
//! ```
//! use std::time::Duration;
//!
//! use pg_embedded_setup_unpriv::{PgEnvCfg, TestBootstrapConfig};
//!
//! let config = TestBootstrapConfig::new(PgEnvCfg::default())
//!     .with_shutdown_timeout(Duration::from_secs(30))
//!     .with_releases_url("https://example.invalid/releases");
//!
//! assert_eq!(config.shutdown_timeout(), Some(Duration::from_secs(30)));
//! assert_eq!(
//!     config.releases_url(),
//!     Some("https://example.invalid/releases")
//! );
//! ```

use std::{fmt, time::Duration};

use camino::Utf8PathBuf;

use crate::PgEnvCfg;

/// Explicit inputs for [`bootstrap_for_tests_with`](crate::bootstrap_for_tests_with).
///
/// The configuration is deliberately non-exhaustive: new explicit controls can
/// be added without breaking callers that build it through [`new`](Self::new)
/// and the `with_*` builders.
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::{PgEnvCfg, TestBootstrapConfig};
///
/// let config = TestBootstrapConfig::from(PgEnvCfg::default());
/// assert!(config.worker_binary().is_none());
/// ```
#[derive(Clone, PartialEq)]
#[non_exhaustive]
pub struct TestBootstrapConfig {
    cfg: PgEnvCfg,
    worker_binary: Option<Utf8PathBuf>,
    shutdown_timeout: Option<Duration>,
    releases_url: Option<String>,
}

impl TestBootstrapConfig {
    /// Wraps resolved [`PgEnvCfg`] values with no explicit overrides.
    ///
    /// # Examples
    ///
    /// ```
    /// use pg_embedded_setup_unpriv::{PgEnvCfg, TestBootstrapConfig};
    ///
    /// let config = TestBootstrapConfig::new(PgEnvCfg::default());
    /// assert_eq!(config.cfg(), &PgEnvCfg::default());
    /// ```
    #[must_use]
    pub const fn new(cfg: PgEnvCfg) -> Self {
        Self {
            cfg,
            worker_binary: None,
            shutdown_timeout: None,
            releases_url: None,
        }
    }

    /// Returns the resolved configuration values.
    ///
    /// # Examples
    ///
    /// ```
    /// use pg_embedded_setup_unpriv::{PgEnvCfg, TestBootstrapConfig};
    ///
    /// let cfg = PgEnvCfg {
    ///     port: Some(54_321),
    ///     ..PgEnvCfg::default()
    /// };
    /// let config = TestBootstrapConfig::new(cfg);
    /// assert_eq!(config.cfg().port, Some(54_321));
    /// ```
    #[must_use]
    pub const fn cfg(&self) -> &PgEnvCfg { &self.cfg }

    /// Returns the explicit worker binary, if one was supplied.
    ///
    /// When this is `None`, a privileged bootstrap discovers `pg_worker` on
    /// `PATH` instead of reading `PG_EMBEDDED_WORKER`.
    #[must_use]
    pub const fn worker_binary(&self) -> Option<&Utf8PathBuf> { self.worker_binary.as_ref() }

    /// Returns the explicit shutdown timeout, if one was supplied.
    ///
    /// When this is `None`, the bootstrap uses the 15-second default.
    #[must_use]
    pub const fn shutdown_timeout(&self) -> Option<Duration> { self.shutdown_timeout }

    /// Returns the explicit release URL, if one was supplied.
    ///
    /// When this is `None`, the bootstrap keeps the release URL produced by
    /// [`PgEnvCfg::to_settings_for_tests`].
    #[must_use]
    pub fn releases_url(&self) -> Option<&str> { self.releases_url.as_deref() }

    /// Sets the worker binary used for privileged subprocess execution.
    ///
    /// The path is validated when the bootstrap runs. Unprivileged execution
    /// ignores it, matching the ambient `PG_EMBEDDED_WORKER` behaviour.
    ///
    /// # Examples
    ///
    /// ```
    /// use pg_embedded_setup_unpriv::{PgEnvCfg, TestBootstrapConfig};
    ///
    /// let config = TestBootstrapConfig::new(PgEnvCfg::default())
    ///     .with_worker_binary("/usr/local/bin/pg_worker");
    /// assert_eq!(
    ///     config.worker_binary().map(|path| path.as_str()),
    ///     Some("/usr/local/bin/pg_worker")
    /// );
    /// ```
    #[must_use]
    pub fn with_worker_binary(mut self, path: impl Into<Utf8PathBuf>) -> Self {
        self.worker_binary = Some(path.into());
        self
    }

    /// Sets the grace period granted to `PostgreSQL` during shutdown.
    ///
    /// The value must lie between one and 600 seconds; the bootstrap rejects
    /// anything outside that range.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use pg_embedded_setup_unpriv::{PgEnvCfg, TestBootstrapConfig};
    ///
    /// let config = TestBootstrapConfig::new(PgEnvCfg::default())
    ///     .with_shutdown_timeout(Duration::from_secs(30));
    /// assert_eq!(config.shutdown_timeout(), Some(Duration::from_secs(30)));
    /// ```
    #[must_use]
    pub const fn with_shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = Some(timeout);
        self
    }

    /// Sets the release URL used to download `PostgreSQL` archives.
    ///
    /// The bootstrap rejects an empty or whitespace-only URL.
    ///
    /// # Examples
    ///
    /// ```
    /// use pg_embedded_setup_unpriv::{PgEnvCfg, TestBootstrapConfig};
    ///
    /// let config = TestBootstrapConfig::new(PgEnvCfg::default())
    ///     .with_releases_url("https://example.invalid/releases");
    /// assert_eq!(
    ///     config.releases_url(),
    ///     Some("https://example.invalid/releases")
    /// );
    /// ```
    #[must_use]
    pub fn with_releases_url(mut self, url: impl Into<String>) -> Self {
        self.releases_url = Some(url.into());
        self
    }
}

impl From<PgEnvCfg> for TestBootstrapConfig {
    fn from(cfg: PgEnvCfg) -> Self { Self::new(cfg) }
}

impl fmt::Debug for TestBootstrapConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `PgEnvCfg` derives `Debug` and carries the superuser password, so
        // redact it before rendering. Cloning is confined to diagnostics.
        let mut redacted = self.cfg.clone();
        if redacted.password.is_some() {
            redacted.password = Some("<redacted>".to_owned());
        }

        f.debug_struct("TestBootstrapConfig")
            .field("cfg", &redacted)
            .field("worker_binary", &self.worker_binary)
            .field("shutdown_timeout", &self.shutdown_timeout)
            .field("releases_url", &self.releases_url)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    //! Tests for explicit bootstrap configuration.

    use super::*;

    #[test]
    fn debug_redacts_password() {
        let cfg = PgEnvCfg {
            password: Some("super-secret-value".to_owned()),
            ..PgEnvCfg::default()
        };
        let rendered = format!("{:?}", TestBootstrapConfig::new(cfg));

        assert!(
            rendered.contains("<redacted>"),
            "expected redaction marker in {rendered}"
        );
        assert!(
            !rendered.contains("super-secret-value"),
            "expected no password material in {rendered}"
        );
    }

    #[test]
    fn builders_record_explicit_values() {
        let config = TestBootstrapConfig::new(PgEnvCfg::default())
            .with_worker_binary("/opt/pg_worker")
            .with_shutdown_timeout(Duration::from_secs(42))
            .with_releases_url("https://example.invalid/releases");

        assert_eq!(
            config.worker_binary().map(|path| path.as_str()),
            Some("/opt/pg_worker")
        );
        assert_eq!(config.shutdown_timeout(), Some(Duration::from_secs(42)));
        assert_eq!(
            config.releases_url(),
            Some("https://example.invalid/releases")
        );
    }
}

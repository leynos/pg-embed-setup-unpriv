//! Resolves bootstrap inputs from the process environment or explicit config.
//!
//! Keeping source selection in its own module lets [`orchestrate_bootstrap`]
//! stay a short, linear pipeline: detect privileges, resolve inputs, prepare
//! the cluster. The ambient path preserves the historical read order so error
//! precedence does not change; the explicit path skips the variables that a
//! [`TestBootstrapConfig`] replaces.
//!
//! [`orchestrate_bootstrap`]: super::orchestrate_bootstrap

use std::time::Duration;

use camino::Utf8PathBuf;
use color_eyre::eyre::Context;
use postgresql_embedded::Settings;

use super::{
    BootstrapKind,
    env::{
        DEFAULT_SHUTDOWN_TIMEOUT,
        EXPLICIT_WORKER_SOURCE,
        resolve_explicit_worker,
        shutdown_timeout_from_env,
        validate_explicit_shutdown_timeout,
        worker_binary_from_env,
    },
    mode::{ExecutionMode, ExecutionPrivileges, determine_execution_mode},
};
use crate::{
    PgEnvCfg,
    TestBootstrapConfig,
    error::{BootstrapError, BootstrapResult},
};

/// Error text for the ambient path when root execution has no worker binary.
const AMBIENT_MISSING_WORKER_HINT: &str =
    "PG_EMBEDDED_WORKER must be set when running with root privileges";

/// Error text for the explicit path when root execution has no worker binary.
///
/// The explicit path never reads `PG_EMBEDDED_WORKER`, so the message names the
/// inputs that actually apply.
const EXPLICIT_MISSING_WORKER_HINT: &str = concat!(
    "explicit configuration must supply a worker binary when running with root ",
    "privileges; call TestBootstrapConfig::with_worker_binary or put pg_worker on PATH"
);

/// Where a bootstrap takes its inputs from.
pub(super) enum BootstrapSource {
    /// Read the process environment, as `run` and `bootstrap_for_tests` do.
    Ambient,
    /// Use a caller-supplied [`TestBootstrapConfig`].
    ///
    /// Boxed because the configuration is much larger than the `Ambient`
    /// variant, and this enum is moved through the bootstrap pipeline.
    Explicit(Box<TestBootstrapConfig>),
}

/// Inputs resolved from either source before preparation begins.
pub(super) struct ResolvedInputs {
    pub(super) cfg: PgEnvCfg,
    pub(super) settings: Settings,
    pub(super) worker_binary: Option<Utf8PathBuf>,
    pub(super) execution_mode: ExecutionMode,
    pub(super) shutdown_timeout: Duration,
}

/// Resolves the inputs for `source`, dispatching to the matching reader.
pub(super) fn resolve_inputs(
    kind: BootstrapKind,
    source: BootstrapSource,
    privileges: ExecutionPrivileges,
) -> BootstrapResult<ResolvedInputs> {
    match source {
        BootstrapSource::Ambient => resolve_ambient_inputs(kind, privileges),
        BootstrapSource::Explicit(config) => resolve_explicit_inputs(kind, &config, privileges),
    }
}

/// Resolves inputs from the process environment, preserving the historical
/// read order so error precedence does not change.
fn resolve_ambient_inputs(
    kind: BootstrapKind,
    privileges: ExecutionPrivileges,
) -> BootstrapResult<ResolvedInputs> {
    let cfg = PgEnvCfg::load().context("failed to load configuration via OrthoConfig")?;
    let settings = build_settings(kind, &cfg)?;
    let worker_binary = worker_binary_from_env(privileges)?;
    let execution_mode = determine_execution_mode(
        privileges,
        worker_binary.as_ref(),
        AMBIENT_MISSING_WORKER_HINT,
    )?;
    let shutdown_timeout = shutdown_timeout_from_env()?;

    Ok(ResolvedInputs {
        cfg,
        settings,
        worker_binary,
        execution_mode,
        shutdown_timeout,
    })
}

/// Resolves inputs from an explicit configuration without reading the ambient
/// variables the explicit path replaces.
fn resolve_explicit_inputs(
    kind: BootstrapKind,
    config: &TestBootstrapConfig,
    privileges: ExecutionPrivileges,
) -> BootstrapResult<ResolvedInputs> {
    let cfg = config.cfg().clone();
    let mut settings = build_settings(kind, &cfg)?;
    if let Some(url) = config.releases_url() {
        settings.releases_url = validate_releases_url(url)?;
    }
    let worker_binary = resolve_explicit_worker(
        privileges,
        config.worker_binary().cloned(),
        EXPLICIT_WORKER_SOURCE,
    )?;
    let execution_mode = determine_execution_mode(
        privileges,
        worker_binary.as_ref(),
        EXPLICIT_MISSING_WORKER_HINT,
    )?;
    let shutdown_timeout = match config.shutdown_timeout() {
        Some(timeout) => validate_explicit_shutdown_timeout(timeout)?,
        None => DEFAULT_SHUTDOWN_TIMEOUT,
    };

    Ok(ResolvedInputs {
        cfg,
        settings,
        worker_binary,
        execution_mode,
        shutdown_timeout,
    })
}

fn build_settings(kind: BootstrapKind, cfg: &PgEnvCfg) -> BootstrapResult<Settings> {
    match kind {
        BootstrapKind::Default => Ok(cfg.to_settings()?),
        BootstrapKind::Test => Ok(cfg.to_settings_for_tests()?),
    }
}

/// Rejects an empty release URL before it reaches `postgresql_embedded`.
fn validate_releases_url(url: &str) -> BootstrapResult<String> {
    if url.trim().is_empty() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "releases URL must not be empty or whitespace-only"
        )));
    }

    Ok(url.to_owned())
}

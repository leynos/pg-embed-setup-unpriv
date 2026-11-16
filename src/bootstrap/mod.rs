//! Bootstraps embedded `PostgreSQL` while adapting to the caller's privileges.
//!
//! Provides [`bootstrap_for_tests`] so suites can retrieve structured settings and
//! prepared environment variables without reimplementing bootstrap orchestration.
mod config;
mod env;
mod env_types;
mod mode;
mod prepare;
mod source;

#[cfg(test)]
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use color_eyre::eyre::eyre;
pub use config::TestBootstrapConfig;
pub use env::{TestBootstrapEnvironment, find_timezone_dir};
pub use mode::{ExecutionMode, ExecutionPrivileges, detect_execution_privileges};
pub(crate) use mode::{root_privilege_drop_supported, unsupported_root_privilege_drop_error};
use postgresql_embedded::Settings;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use prepare::is_watching;
pub(crate) use prepare::{
    ClaimGuard,
    claim_guard_at,
    claim_guard_for,
    derived_slot_parent,
    has_live_peers,
    has_live_slots_in,
};
pub use prepare::{
    PasswordReuseOutcome,
    default_paths_under,
    reuse_existing_password,
    stored_cluster_password,
};
#[cfg(target_os = "linux")]
pub(crate) use prepare::{release_watcher, watch_slot_owner};
use serde::{Deserialize, Serialize};
pub(crate) use source::ConfigSource;

use self::{
    prepare::{DataLayout, prepare_bootstrap},
    source::resolve_inputs,
};
use crate::{
    error::{BootstrapResult, Result as CrateResult},
    extensions::{ExtensionRequest, InstalledExtension},
};

const DEFAULT_SETUP_TIMEOUT: Duration = Duration::from_mins(3);
const DEFAULT_START_TIMEOUT: Duration = Duration::from_mins(1);

#[cfg(test)]
type SetupOnlyLifecycleHook =
    Arc<dyn Fn(TestBootstrapSettings) -> BootstrapResult<()> + Send + Sync>;

#[cfg(test)]
static SETUP_ONLY_LIFECYCLE_HOOK: OnceLock<Mutex<Option<SetupOnlyLifecycleHook>>> = OnceLock::new();

#[derive(Clone, Copy)]
pub(super) enum BootstrapKind {
    Default,
    Test,
}

impl BootstrapKind {
    /// Returns the data layout this kind of bootstrap needs (ADR 005).
    ///
    /// The setup-only `run` initializes a cluster to be used later, possibly
    /// by another process, so it keeps the persistent `data` directory. A test
    /// bootstrap takes a per-cluster slot that dies with its process.
    const fn data_layout(self) -> DataLayout {
        match self {
            Self::Default => DataLayout::Persistent,
            Self::Test => DataLayout::PerCluster,
        }
    }
}

/// Controls cleanup behaviour when a cluster is dropped.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize, Default)]
pub enum CleanupMode {
    /// Remove only the data directory.
    #[default]
    DataOnly,
    /// Remove both the data and installation directories.
    Full,
    /// Skip cleanup entirely (useful for debugging).
    None,
}

/// Structured settings returned from [`bootstrap_for_tests`].
#[derive(Debug, Clone)]
pub struct TestBootstrapSettings {
    /// Privilege level detected for the current process.
    pub privileges: ExecutionPrivileges,
    /// Strategy for executing `PostgreSQL` lifecycle commands.
    pub execution_mode: ExecutionMode,
    /// `PostgreSQL` configuration prepared for the embedded instance.
    pub settings: Settings,
    /// Environment variables required to exercise the embedded instance.
    pub environment: TestBootstrapEnvironment,
    /// Optional path to the helper binary used for subprocess execution.
    pub worker_binary: Option<camino::Utf8PathBuf>,
    /// Maximum time to allow the worker to complete the setup phase.
    pub setup_timeout: Duration,
    /// Maximum time to allow the worker to complete the start phase.
    pub start_timeout: Duration,
    /// Grace period granted to `PostgreSQL` during drop before teardown proceeds regardless.
    pub shutdown_timeout: Duration,
    /// Controls cleanup behaviour when the cluster drops.
    pub cleanup_mode: CleanupMode,
    /// Optional override for the binary cache directory.
    ///
    /// When set, `TestCluster` uses this directory instead of the default
    /// resolved from environment variables.
    pub binary_cache_dir: Option<camino::Utf8PathBuf>,
    /// Extensions to install before the server starts, from `PG_EXTENSIONS*`.
    pub extensions: Option<ExtensionRequest>,
    /// What the extension hook installed, filled in once the cluster started.
    pub installed_extensions: Vec<InstalledExtension>,
}

/// Bootstraps an embedded `PostgreSQL` instance, downloads the distribution,
/// and initializes the data directory via `initdb`.
///
/// The server is **not** started — the resulting installation is ready for
/// subsequent use by other tools. A derived data directory stays at
/// `<root>/data`, where a [`TestCluster`](crate::TestCluster) would not look:
/// it claims a per-cluster directory of its own (ADR 005). Set `PG_DATA_DIR` for
/// both to share one directory.
///
/// The function honours the following environment variables when present:
/// - `PG_EMBED_ROOT`: Replaces the per-user `/var/tmp/pg-embed-{uid}` base under which the default
///   installation and data directories are derived.
/// - `PG_RUNTIME_DIR`: Overrides the `PostgreSQL` installation directory.
/// - `PG_DATA_DIR`: Overrides the data directory used for initialization.
/// - `PG_SUPERUSER`: Sets the superuser account name.
/// - `PG_PASSWORD`: Supplies the superuser password.
///
/// When executed as `root` on Unix platforms the runtime drops privileges to the `nobody` user
/// and prepares the filesystem on that user's behalf. Unprivileged executions reuse the current
/// user identity. The function returns a [`crate::Error`] describing failures encountered during
/// bootstrap or setup.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::run;
///
/// fn main() -> Result<(), pg_embedded_setup_unpriv::Error> {
///     run()?;
///     Ok(())
/// }
/// ```
///
/// # Errors
/// Returns an error when bootstrap preparation fails, when the `PostgreSQL`
/// distribution cannot be downloaded, or when `initdb` fails.
pub fn run() -> CrateResult<()> {
    let bootstrap = orchestrate_bootstrap(BootstrapKind::Default, ConfigSource::Ambient)?;
    run_setup_only_lifecycle(bootstrap)
}

/// Bootstraps `PostgreSQL` for integration tests and surfaces the prepared settings.
///
/// # Examples
/// ```no_run
/// use pg_embedded_setup_unpriv::bootstrap_for_tests;
/// use temp_env::with_vars;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let bootstrap = bootstrap_for_tests()?;
/// with_vars(
///     bootstrap.environment.to_env(),
///     || -> pg_embedded_setup_unpriv::BootstrapResult<()> {
///         // Launch application logic that relies on `bootstrap.settings` here.
///         Ok(())
///     },
/// )?;
/// # Ok(())
/// # }
/// ```
///
/// # Errors
/// Returns an error when bootstrap preparation fails or when subprocess orchestration
/// cannot be configured.
pub fn bootstrap_for_tests() -> BootstrapResult<TestBootstrapSettings> {
    orchestrate_bootstrap(BootstrapKind::Test, ConfigSource::Ambient)
}

/// Bootstraps `PostgreSQL` for integration tests from resolved configuration.
///
/// Unlike [`bootstrap_for_tests`], this constructor does not read
/// `PG_TEST_BACKEND`, `PG_EMBEDDED_WORKER` or `PG_SHUTDOWN_TIMEOUT_SECS`.
/// Configuration values come from the supplied [`TestBootstrapConfig`], and
/// absent explicit controls use the built-in defaults or `PATH` discovery.
///
/// Passing `PgEnvCfg::load()?` is equivalent to [`bootstrap_for_tests`] for
/// every `PgEnvCfg` field, but it still skips the ambient backend selection,
/// worker-binary, and shutdown-timeout reads. Use
/// [`TestBootstrapConfig::with_worker_binary`] and
/// [`TestBootstrapConfig::with_shutdown_timeout`] to supply those explicitly.
///
/// The prepared environment is still applied by
/// [`TestCluster`](crate::TestCluster) when it starts the server, so callers
/// that only inspect the returned settings should apply
/// [`TestBootstrapSettings::environment`] themselves.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::{PgEnvCfg, TestBootstrapConfig, bootstrap_for_tests_with};
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let config = TestBootstrapConfig::new(PgEnvCfg {
///     port: Some(54_321),
///     ..PgEnvCfg::default()
/// });
/// let bootstrap = bootstrap_for_tests_with(config)?;
/// assert_eq!(bootstrap.settings.port, 54_321);
/// # Ok(())
/// # }
/// ```
///
/// # Errors
/// Returns an error when the explicit configuration is invalid or when
/// bootstrap preparation fails.
pub fn bootstrap_for_tests_with(
    config: impl Into<TestBootstrapConfig>,
) -> BootstrapResult<TestBootstrapSettings> {
    orchestrate_bootstrap(
        BootstrapKind::Test,
        ConfigSource::Explicit(Box::new(config.into())),
    )
}

fn orchestrate_bootstrap(
    kind: BootstrapKind,
    source: ConfigSource,
) -> BootstrapResult<TestBootstrapSettings> {
    install_color_eyre();
    if matches!(kind, BootstrapKind::Test) && matches!(&source, ConfigSource::Ambient) {
        validate_backend_selection()?;
    }

    let privileges = detect_execution_privileges();
    let inputs = resolve_inputs(kind, source, privileges)?;
    let extensions = ExtensionRequest::from_config(&inputs.cfg)?;
    let prepared = prepare_bootstrap(privileges, inputs.settings, &inputs.cfg, kind.data_layout())?;

    Ok(TestBootstrapSettings {
        privileges,
        execution_mode: inputs.execution_mode,
        settings: prepared.settings,
        environment: prepared.environment,
        worker_binary: inputs.worker_binary,
        setup_timeout: DEFAULT_SETUP_TIMEOUT,
        start_timeout: DEFAULT_START_TIMEOUT,
        shutdown_timeout: inputs.shutdown_timeout,
        cleanup_mode: CleanupMode::default(),
        binary_cache_dir: inputs.cfg.binary_cache_dir,
        extensions,
        installed_extensions: Vec::new(),
    })
}

fn install_color_eyre() {
    if let Err(err) = color_eyre::install() {
        tracing::debug!("color_eyre already installed: {err}");
    }
}

fn validate_backend_selection() -> BootstrapResult<()> {
    let raw = std::env::var_os("PG_TEST_BACKEND").unwrap_or_default();
    let value = raw.to_string_lossy();
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == "postgresql_embedded" {
        return Ok(());
    }
    Err(eyre!(
        "SKIP-TEST-CLUSTER: unsupported PG_TEST_BACKEND '{trimmed}'; supported backends: \
         postgresql_embedded"
    )
    .into())
}

/// Executes the setup-only lifecycle for CLI invocations.
///
/// Keeping this as a dedicated helper keeps the public `run()` flow linear:
/// bootstrap preparation first, then setup lifecycle execution.
fn run_setup_only_lifecycle(bootstrap: TestBootstrapSettings) -> CrateResult<()> {
    #[cfg(test)]
    {
        let installed_setup_hook = SETUP_ONLY_LIFECYCLE_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(setup_hook) = installed_setup_hook {
            return setup_hook(bootstrap).map_err(Into::into);
        }
    }

    crate::cluster::setup_postgres_only(bootstrap)?;
    Ok(())
}

#[cfg(test)]
pub(crate) struct SetupOnlyLifecycleHookGuard;

#[cfg(test)]
impl Drop for SetupOnlyLifecycleHookGuard {
    fn drop(&mut self) {
        let mut slot = SETUP_ONLY_LIFECYCLE_HOOK
            .get_or_init(|| Mutex::new(None))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        slot.take();
    }
}

#[cfg(test)]
pub(crate) fn install_setup_only_lifecycle_hook<F>(hook: F) -> SetupOnlyLifecycleHookGuard
where
    F: Fn(TestBootstrapSettings) -> BootstrapResult<()> + Send + Sync + 'static,
{
    let mut slot = SETUP_ONLY_LIFECYCLE_HOOK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = Some(Arc::new(hook));
    SetupOnlyLifecycleHookGuard
}

#[cfg(test)]
mod mod_tests;

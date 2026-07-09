//! Tests for setup-only startup orchestration.

use std::{
    ffi::OsString,
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
    time::Duration,
};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Result, ensure, eyre};
use postgresql_embedded::VersionReq;
use rstest::{fixture, rstest};
use serial_test::serial;
use tempfile::tempdir;

use super::*;
use crate::test_support::{
    dummy_settings,
    install_run_root_operation_hook,
    scoped_env,
    test_runtime,
};

const TEST_POSTGRES_VERSION: &str = "17.4.0";

struct RootSetupPaths {
    _tempdir: tempfile::TempDir,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    scoped_cache_home: Utf8PathBuf,
    host_cache_home: Utf8PathBuf,
    cache_dir: Utf8PathBuf,
}

struct OperationsHookContext {
    operations: Arc<Mutex<Vec<String>>>,
    host_cache_home: Utf8PathBuf,
    _hook_guard: crate::test_support::HookGuard,
    _env_guard: crate::env::ScopedEnv,
}

struct TempBasePaths {
    _tempdir: tempfile::TempDir,
    base: Utf8PathBuf,
}

struct CachePopulationPaths {
    _tempdir: tempfile::TempDir,
    install_dir: Utf8PathBuf,
    data_dir: Utf8PathBuf,
    cache_dir: Utf8PathBuf,
    marker_path: Utf8PathBuf,
}

struct RuntimeErrorPaths {
    _tempdir: tempfile::TempDir,
    install_file: Utf8PathBuf,
    data_dir: Utf8PathBuf,
}

fn utf8_path(path: std::path::PathBuf, context: &str) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path).map_err(|_| eyre!("{context} is not valid UTF-8"))
}

#[fixture]
fn root_setup_paths() -> Result<Arc<RootSetupPaths>> {
    let tempdir_guard = tempdir()?;
    let base = utf8_path(tempdir_guard.path().to_path_buf(), "tempdir")?;
    let install_dir = base.join("install");
    let data_dir = base.join("data");
    let scoped_cache_home = base.join("scoped-cache-home");
    let host_cache_home = base.join("host-cache-home");
    let cache_dir = base.join("cache");

    fs::create_dir_all(install_dir.as_std_path())?;
    fs::create_dir_all(data_dir.as_std_path())?;
    fs::create_dir_all(cache_dir.as_std_path())?;

    Ok(Arc::new(RootSetupPaths {
        _tempdir: tempdir_guard,
        install_dir,
        data_dir,
        scoped_cache_home,
        host_cache_home,
        cache_dir,
    }))
}

#[fixture]
fn root_bootstrap(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<TestBootstrapSettings> {
    let root_setup_paths = root_setup_paths_res?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &root_setup_paths.install_dir,
        &root_setup_paths.data_dir,
        &root_setup_paths.scoped_cache_home,
    )?;
    Ok(bootstrap)
}

#[fixture]
fn operations_hook(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<OperationsHookContext> {
    let root_setup_paths = root_setup_paths_res?;
    let operations = Arc::new(Mutex::new(Vec::new()));
    let recorded_operations = Arc::clone(&operations);
    let hook_guard = install_run_root_operation_hook(move |_, _, operation| {
        recorded_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(operation.as_str().to_owned());
        Ok(())
    })?;
    let env_guard = scoped_env([
        (OsString::from("PG_BINARY_CACHE_DIR"), None),
        (
            OsString::from("XDG_CACHE_HOME"),
            Some(OsString::from(root_setup_paths.host_cache_home.as_str())),
        ),
    ]);

    Ok(OperationsHookContext {
        operations,
        host_cache_home: root_setup_paths.host_cache_home.clone(),
        _hook_guard: hook_guard,
        _env_guard: env_guard,
    })
}

#[fixture]
fn temp_base_paths() -> Result<TempBasePaths> {
    let tempdir_guard = tempdir()?;
    let base = utf8_path(tempdir_guard.path().to_path_buf(), "tempdir")?;
    Ok(TempBasePaths {
        _tempdir: tempdir_guard,
        base,
    })
}

#[fixture]
fn cache_population_paths(
    #[from(temp_base_paths)] temp_base_paths_res: Result<TempBasePaths>,
) -> Result<CachePopulationPaths> {
    let TempBasePaths {
        _tempdir: tempdir_guard,
        base,
    } = temp_base_paths_res?;
    let install_dir = base.join("install").join(TEST_POSTGRES_VERSION);
    let data_dir = base.join("data");
    let cache_dir = base.join("cache");
    fs::create_dir_all(install_dir.join("bin").as_std_path())?;
    fs::create_dir_all(data_dir.as_std_path())?;
    fs::create_dir_all(cache_dir.as_std_path())?;
    let marker_path = cache_dir.join(TEST_POSTGRES_VERSION).join(".complete");

    Ok(CachePopulationPaths {
        _tempdir: tempdir_guard,
        install_dir,
        data_dir,
        cache_dir,
        marker_path,
    })
}

#[fixture]
fn runtime_error_paths(
    #[from(temp_base_paths)] temp_base_paths_res: Result<TempBasePaths>,
) -> Result<RuntimeErrorPaths> {
    let TempBasePaths {
        _tempdir: tempdir_guard,
        base,
    } = temp_base_paths_res?;
    let install_file = base.join("install-file");
    let data_dir = base.join("data");
    fs::write(install_file.as_std_path(), b"not-a-directory")?;
    fs::create_dir_all(data_dir.as_std_path())?;
    Ok(RuntimeErrorPaths {
        _tempdir: tempdir_guard,
        install_file,
        data_dir,
    })
}

fn configure_root_bootstrap(
    bootstrap: &mut TestBootstrapSettings,
    install_dir: &Utf8Path,
    data_dir: &Utf8Path,
    scoped_cache_home: &Utf8Path,
) -> Result<()> {
    let runtime_dir = install_dir.join("run");
    bootstrap.settings.installation_dir = install_dir.to_path_buf().into_std_path_buf();
    bootstrap.settings.data_dir = data_dir.to_path_buf().into_std_path_buf();
    let exact_version = format!("={TEST_POSTGRES_VERSION}");
    bootstrap.settings.version = VersionReq::parse(&exact_version)?;
    bootstrap.environment.home = install_dir.to_path_buf();
    bootstrap.environment.xdg_cache_home = scoped_cache_home.to_path_buf();
    bootstrap.environment.xdg_runtime_dir = runtime_dir;
    bootstrap.environment.pgpass_file = install_dir.join(".pgpass");
    Ok(())
}

fn create_complete_cache_entry(cache_home: &Utf8PathBuf, version: &str) -> Result<()> {
    let version_dir = cache_home
        .join("pg-embedded")
        .join("binaries")
        .join(version);
    fs::create_dir_all(version_dir.join("bin"))?;
    fs::write(version_dir.join(".complete"), b"")?;
    Ok(())
}

#[rstest]
#[serial(worker_hook)]
fn setup_postgres_only_resolves_cache_before_scoped_env_and_runs_setup_only(
    #[from(root_bootstrap)] root_bootstrap_res: Result<TestBootstrapSettings>,
    #[from(operations_hook)] operations_hook_res: Result<OperationsHookContext>,
) -> Result<()> {
    let root_bootstrap = root_bootstrap_res?;
    let operations_hook = operations_hook_res?;
    let observed_xdg_cache_home = std::env::var("XDG_CACHE_HOME").ok();
    ensure!(
        observed_xdg_cache_home.as_deref() == Some(operations_hook.host_cache_home.as_str()),
        "expected host XDG_CACHE_HOME to be set for cache resolution (observed: \
         {observed_xdg_cache_home:?})",
    );

    create_complete_cache_entry(&operations_hook.host_cache_home, TEST_POSTGRES_VERSION)?;
    let resolved_cache_dir = cache_config_from_bootstrap(&root_bootstrap).cache_dir;
    let expected_cache_dir = operations_hook
        .host_cache_home
        .join("pg-embedded")
        .join("binaries");
    ensure!(
        resolved_cache_dir == expected_cache_dir,
        "cache config should resolve from host env before ScopedEnv (expected: \
         {expected_cache_dir}, observed: {resolved_cache_dir})",
    );
    let expected_install_dir = utf8_path(
        root_bootstrap.settings.installation_dir.clone(),
        "installation directory",
    )?
    .join(TEST_POSTGRES_VERSION);

    let prepared = setup_postgres_only(root_bootstrap)?;

    let recorded_ops = operations_hook
        .operations
        .lock()
        .expect("operation mutex poisoned");
    ensure!(
        matches!(recorded_ops.as_slice(), [operation] if operation == "setup"),
        "setup-only lifecycle should invoke exactly one Setup operation",
    );
    ensure!(
        prepared.settings.trust_installation_dir,
        "cache hit should mark installation directory as trusted"
    );

    let observed_install_dir = utf8_path(
        prepared.settings.installation_dir.clone(),
        "installation directory",
    )?;
    ensure!(
        observed_install_dir == expected_install_dir,
        "expected setup to consume host cache before scoped env (observed: {observed_install_dir})"
    );
    ensure!(
        !prepared.settings.data_dir.join("postmaster.pid").exists(),
        "setup-only path must not start PostgreSQL"
    );
    Ok(())
}

#[rstest]
#[serial(worker_hook)]
fn setup_lifecycle_invokes_setup_operation_only(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
    #[from(root_bootstrap)] root_bootstrap_res: Result<TestBootstrapSettings>,
    #[from(operations_hook)] operations_hook_res: Result<OperationsHookContext>,
) -> Result<()> {
    let root_setup_paths = root_setup_paths_res?;
    let root_bootstrap = root_bootstrap_res?;
    let operations_hook = operations_hook_res?;
    let env_vars = root_bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(root_setup_paths.cache_dir.clone());
    let runtime = test_runtime()?;
    let prepared = setup_lifecycle(&runtime, root_bootstrap, &env_vars, &cache_config)?;

    let recorded_ops = operations_hook
        .operations
        .lock()
        .expect("operation mutex poisoned");
    ensure!(
        matches!(recorded_ops.as_slice(), [operation] if operation == "setup"),
        "setup_lifecycle should only dispatch Setup",
    );
    ensure!(
        !prepared.settings.data_dir.join("postmaster.pid").exists(),
        "setup_lifecycle must not start PostgreSQL",
    );
    Ok(())
}

#[rstest]
#[serial(worker_hook)]
fn setup_with_privileges_root_dispatches_setup_only(
    #[from(root_bootstrap)] root_bootstrap_res: Result<TestBootstrapSettings>,
    #[from(operations_hook)] operations_hook_res: Result<OperationsHookContext>,
) -> Result<()> {
    let mut root_bootstrap = root_bootstrap_res?;
    let operations_hook = operations_hook_res?;
    let env_vars = root_bootstrap.environment.to_env();
    let runtime = test_runtime()?;
    setup_with_privileges(
        ExecutionPrivileges::Root,
        &runtime,
        &mut root_bootstrap,
        &env_vars,
    )?;

    let recorded_ops = operations_hook
        .operations
        .lock()
        .expect("operation mutex poisoned");
    ensure!(
        matches!(recorded_ops.as_slice(), [operation] if operation == "setup"),
        "setup_with_privileges should dispatch Setup only",
    );
    Ok(())
}

#[rstest]
fn populate_cache_on_miss_only_populates_on_cache_miss(
    #[from(cache_population_paths)] cache_population_paths_res: Result<CachePopulationPaths>,
) -> Result<()> {
    let cache_population_paths = cache_population_paths_res?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    bootstrap.settings.installation_dir = cache_population_paths
        .install_dir
        .clone()
        .into_std_path_buf();
    bootstrap.settings.data_dir = cache_population_paths.data_dir.into_std_path_buf();
    let cache_config = BinaryCacheConfig::with_dir(cache_population_paths.cache_dir.clone());

    populate_cache_on_miss(true, &cache_config, &bootstrap);
    ensure!(
        !cache_population_paths.marker_path.exists(),
        "cache marker should remain absent on cache hit"
    );

    populate_cache_on_miss(false, &cache_config, &bootstrap);
    ensure!(
        cache_population_paths.marker_path.exists(),
        "cache marker should be written on cache miss"
    );
    Ok(())
}

#[rstest]
fn setup_postgres_only_inside_runtime_returns_recoverable_error(
    #[from(runtime_error_paths)] runtime_error_paths_res: Result<RuntimeErrorPaths>,
) -> Result<()> {
    let runtime_error_paths = runtime_error_paths_res?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    bootstrap.settings.installation_dir = runtime_error_paths.install_file.into_std_path_buf();
    bootstrap.settings.data_dir = runtime_error_paths.data_dir.into_std_path_buf();
    bootstrap.setup_timeout = Duration::from_secs(1);

    let runtime = test_runtime()?;
    // The async wrapper exists only so runtime.block_on can execute the
    // synchronous setup_postgres_only inside an active Tokio runtime and verify
    // nested runtime creation returns an error instead of panicking.
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        runtime.block_on(async { setup_postgres_only(bootstrap) })
    }));

    ensure!(
        outcome.is_ok(),
        "setup_postgres_only should not panic inside an existing runtime"
    );
    let setup_result = outcome.expect("nested-runtime outcome should be captured");
    ensure!(
        setup_result.is_err(),
        "expected a recoverable setup error for invalid installation path"
    );
    Ok(())
}

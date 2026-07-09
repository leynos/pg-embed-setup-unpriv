//! Tests for bootstrap orchestration and backend selection.

use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};
use rstest::{fixture, rstest};
use serial_test::serial;
use tempfile::tempdir;

use super::*;
use crate::test_support::scoped_env;

/// Converts string key-value pairs to `OsString` pairs for `scoped_env`.
fn env_vars<const N: usize>(pairs: [(&str, Option<&str>); N]) -> Vec<(OsString, Option<OsString>)> {
    pairs
        .into_iter()
        .map(|(k, v)| (OsString::from(k), v.map(OsString::from)))
        .collect()
}

/// Converts a `PathBuf` into a `Utf8PathBuf`, reporting non-UTF-8 paths.
fn utf8_path(path: std::path::PathBuf) -> Result<Utf8PathBuf> {
    Utf8PathBuf::from_path_buf(path).map_err(|p| eyre!("non-UTF-8 path: {}", p.display()))
}

/// Asserts that the bootstrap settings record the expected install and data
/// directories.
macro_rules! assert_paths {
    ($settings:expr, $runtime_path:expr, $data_path:expr) => {{
        let observed_install =
            Utf8PathBuf::from_path_buf($settings.settings.installation_dir.clone())
                .expect("installation dir utf8");
        let observed_data =
            Utf8PathBuf::from_path_buf($settings.settings.data_dir.clone()).expect("data dir utf8");

        assert_eq!(observed_install.as_path(), $runtime_path.as_path());
        assert_eq!(observed_data.as_path(), $data_path.as_path());
    }};
}

#[rstest]
fn orchestrate_bootstrap_respects_env_overrides(run_test_paths: Result<Option<RunTestPaths>>) {
    let Some(paths) = run_test_paths.expect("run test paths fixture") else {
        return;
    };

    let _guard = scoped_env(env_vars([
        ("PG_RUNTIME_DIR", Some(paths.runtime_path.as_str())),
        ("PG_DATA_DIR", Some(paths.data_path.as_str())),
        ("PG_SUPERUSER", Some("bootstrap_test")),
        ("PG_PASSWORD", Some("bootstrap_test_pw")),
        ("PG_EMBEDDED_WORKER", None),
    ]));
    let settings = orchestrate_bootstrap(BootstrapKind::Default).expect("bootstrap to succeed");

    assert_paths!(&settings, &paths.runtime_path, &paths.data_path);
    assert_identity(&settings, "bootstrap_test", "bootstrap_test_pw");
    assert_environment(&settings, &paths.runtime_path);
}

/// Holds temporary directories for `run()` tests.
struct RunTestPaths {
    _runtime: tempfile::TempDir,
    _data: tempfile::TempDir,
    runtime_path: Utf8PathBuf,
    data_path: Utf8PathBuf,
}

/// Fixture providing run test paths, returning `None` if running as root.
#[fixture]
fn run_test_paths() -> Result<Option<RunTestPaths>> {
    if detect_execution_privileges() == ExecutionPrivileges::Root {
        tracing::warn!("skipping run test because root privileges require PG_EMBEDDED_WORKER");
        return Ok(None);
    }

    let runtime = tempdir()?;
    let data = tempdir()?;
    let runtime_path = utf8_path(runtime.path().to_path_buf())?;
    let data_path = utf8_path(data.path().to_path_buf())?;

    Ok(Some(RunTestPaths {
        _runtime: runtime,
        _data: data,
        runtime_path,
        data_path,
    }))
}

#[rstest]
fn bootstrap_creates_expected_directories(run_test_paths: Result<Option<RunTestPaths>>) {
    let Some(paths) = run_test_paths.expect("run test paths fixture") else {
        return;
    };

    let _guard = scoped_env(env_vars([
        ("PG_RUNTIME_DIR", Some(paths.runtime_path.as_str())),
        ("PG_DATA_DIR", Some(paths.data_path.as_str())),
        ("PG_SUPERUSER", Some("bootstrap_run")),
        ("PG_PASSWORD", Some("bootstrap_run_pw")),
        ("PG_EMBEDDED_WORKER", None),
    ]));

    orchestrate_bootstrap(BootstrapKind::Default).expect("bootstrap should succeed");

    assert!(
        paths.runtime_path.join("cache").exists(),
        "cache directory should be created"
    );
    assert!(
        paths.runtime_path.join("run").exists(),
        "runtime directory should be created"
    );
}

#[rstest]
#[serial(setup_only_hook)]
fn run_delegates_to_setup_only_lifecycle(run_test_paths: Result<Option<RunTestPaths>>) {
    let Some(paths) = run_test_paths.expect("run test paths fixture") else {
        return;
    };

    let captured = Arc::new(Mutex::new(None::<TestBootstrapSettings>));
    let captured_settings = Arc::clone(&captured);
    let _hook_guard = install_setup_only_lifecycle_hook(move |bootstrap| {
        let mut slot = captured_settings
            .lock()
            .expect("captured settings mutex poisoned");
        *slot = Some(bootstrap);
        Ok(())
    });

    let _guard = scoped_env(env_vars([
        ("PG_RUNTIME_DIR", Some(paths.runtime_path.as_str())),
        ("PG_DATA_DIR", Some(paths.data_path.as_str())),
        ("PG_SUPERUSER", Some("bootstrap_run")),
        ("PG_PASSWORD", Some("bootstrap_run_pw")),
        ("PG_EMBEDDED_WORKER", None),
    ]));

    run().expect("run should delegate to setup-only lifecycle");

    let captured_guard = captured.lock().expect("captured settings mutex poisoned");
    let observed = captured_guard
        .as_ref()
        .expect("setup-only lifecycle hook should capture bootstrap settings");
    assert_paths!(observed, &paths.runtime_path, &paths.data_path);
    assert_identity(observed, "bootstrap_run", "bootstrap_run_pw");
}

/// Holds temporary directories and their UTF-8 paths for bootstrap tests.
struct BootstrapPaths {
    _runtime: tempfile::TempDir,
    _data: tempfile::TempDir,
    _cache: tempfile::TempDir,
    runtime_path: Utf8PathBuf,
    data_path: Utf8PathBuf,
    cache_path: Utf8PathBuf,
}

/// Fixture providing bootstrap test paths, returning `None` if running as root.
#[fixture]
fn bootstrap_paths() -> Result<Option<BootstrapPaths>> {
    if detect_execution_privileges() == ExecutionPrivileges::Root {
        tracing::warn!(
            "skipping orchestrate test because root privileges require PG_EMBEDDED_WORKER"
        );
        return Ok(None);
    }

    let runtime = tempdir()?;
    let data = tempdir()?;
    let cache = tempdir()?;
    let runtime_path = utf8_path(runtime.path().to_path_buf())?;
    let data_path = utf8_path(data.path().to_path_buf())?;
    let cache_path = utf8_path(cache.path().to_path_buf())?;

    Ok(Some(BootstrapPaths {
        _runtime: runtime,
        _data: data,
        _cache: cache,
        runtime_path,
        data_path,
        cache_path,
    }))
}

/// Runs `orchestrate_bootstrap` with cache-related environment variables set.
///
/// Uses the mutex-protected `scoped_env` to avoid racing with other tests.
fn orchestrate_with_cache_env(paths: &BootstrapPaths) -> Result<TestBootstrapSettings> {
    let _guard = scoped_env(env_vars([
        ("PG_RUNTIME_DIR", Some(paths.runtime_path.as_str())),
        ("PG_DATA_DIR", Some(paths.data_path.as_str())),
        ("PG_BINARY_CACHE_DIR", Some(paths.cache_path.as_str())),
        ("PG_SUPERUSER", Some("cache_test")),
        ("PG_PASSWORD", Some("cache_test_pw")),
        ("PG_EMBEDDED_WORKER", None),
    ]));
    Ok(orchestrate_bootstrap(BootstrapKind::Default)?)
}

#[rstest]
fn orchestrate_bootstrap_propagates_binary_cache_dir(
    bootstrap_paths: Result<Option<BootstrapPaths>>,
) {
    let Some(paths) = bootstrap_paths.expect("bootstrap paths fixture") else {
        return;
    };

    let settings = orchestrate_with_cache_env(&paths).expect("bootstrap to succeed");

    assert_eq!(
        settings.binary_cache_dir,
        Some(paths.cache_path.clone()),
        "binary_cache_dir should propagate from PG_BINARY_CACHE_DIR"
    );
}

fn assert_identity(settings: &TestBootstrapSettings, expected_user: &str, expected_password: &str) {
    assert_eq!(settings.settings.username, expected_user);
    assert_eq!(settings.settings.password, expected_password);
    assert_eq!(settings.privileges, ExecutionPrivileges::Unprivileged);
    assert_eq!(settings.execution_mode, ExecutionMode::InProcess);
    assert!(settings.worker_binary.is_none());
}

fn assert_environment(settings: &TestBootstrapSettings, runtime_path: &Utf8PathBuf) {
    let env_pairs = settings.environment.to_env();
    let pgpass = runtime_path.join(".pgpass");
    assert!(env_pairs.contains(&("PGPASSFILE".into(), Some(pgpass.as_str().into()))));
    assert_eq!(settings.environment.home.as_path(), runtime_path.as_path());
}

#[rstest]
#[case::unset(None, true)]
#[case::empty(Some(""), true)]
#[case::embedded(Some("postgresql_embedded"), true)]
#[case::unsupported(Some("sqlite"), false)]
fn validate_backend_selection_respects_pg_test_backend(
    #[case] backend: Option<&str>,
    #[case] should_succeed: bool,
) {
    let _guard = scoped_env(env_vars([("PG_TEST_BACKEND", backend)]));
    let result = validate_backend_selection();
    assert_eq!(
        result.is_ok(),
        should_succeed,
        "unexpected backend validation result for {backend:?}"
    );
    if !should_succeed {
        let err = result.expect_err("expected backend validation to fail");
        assert!(
            err.to_string().contains("SKIP-TEST-CLUSTER"),
            "expected SKIP-TEST-CLUSTER in error message, got {err:?}"
        );
    }
}

//! Tests for `TestCluster` split constructors and `Deref` behaviour.
//!
//! These tests verify that the ambient and explicit split constructors produce
//! working handle/guard pairs, and that `TestCluster` correctly derefs to
//! `ClusterHandle`.
#![cfg(unix)]

#[cfg(feature = "async-api")]
use std::future::Future;
use std::{path::PathBuf, thread, time::Duration};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::{
    BootstrapResult,
    ClusterGuard,
    ClusterHandle,
    PgEnvCfg,
    TestBootstrapConfig,
    TestCluster,
    test_support,
};
use rstest::rstest;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/env.rs"]
mod env;
#[path = "support/env_snapshot.rs"]
mod env_snapshot;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use env_snapshot::EnvSnapshot;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};

/// Which constructor a lifecycle scenario exercises.
#[derive(Clone, Copy, Debug)]
enum SplitSource {
    /// `new_split()` / `start_async_split()`, reading the process environment.
    Ambient,
    /// `new_split_with()` / `start_async_split_with()`, using resolved config.
    Explicit,
}

impl SplitSource {
    fn sandbox_name(self, is_async: bool) -> &'static str {
        match (self, is_async) {
            (Self::Ambient, false) => "split-constructor",
            (Self::Ambient, true) => "async-split",
            (Self::Explicit, false) => "split-with-constructor",
            (Self::Explicit, true) => "async-split-with",
        }
    }
}

// ============================================================================
// new_split() / new_split_with() tests
// ============================================================================

/// Tests that the split constructors create a working handle/guard pair.
///
/// Verifies:
/// - Handle provides access to cluster settings
/// - Guard manages cluster lifecycle
/// - Dropping guard stops the cluster
/// - Environment is restored after guard drops
///
/// The explicit case supplies the sandbox paths and a resolved worker binary
/// directly, so it never relies on `PG_RUNTIME_DIR`, `PG_DATA_DIR`, or `PATH`
/// discovery.
#[rstest]
#[case::ambient(SplitSource::Ambient)]
#[case::explicit(SplitSource::Explicit)]
fn new_split_creates_working_handle_and_guard(
    serial_guard: ScenarioSerialGuard,
    #[case] source: SplitSource,
) -> Result<()> {
    let sandbox = TestSandbox::new(source.sandbox_name(false)).context("create test sandbox")?;
    sandbox.reset()?;

    let env_before = EnvSnapshot::capture();
    let result = sandbox.with_env(sandbox.env_without_timezone(), || match source {
        SplitSource::Ambient => run_split_lifecycle_test(TestCluster::new_split),
        SplitSource::Explicit => {
            let config = explicit_config(&sandbox)?;
            run_split_lifecycle_test(|| TestCluster::new_split_with(config))
        }
    });

    if should_skip_on_error(&result) {
        return Ok(());
    }
    let data_dir = result?;

    // Verify environment restored
    let env_after = EnvSnapshot::capture();
    ensure!(
        env_before == env_after,
        "environment should be restored after guard drops"
    );

    // Verify cluster stopped
    wait_for_postmaster_shutdown(&data_dir)?;
    drop(serial_guard);
    Ok(())
}

fn run_split_lifecycle_test<F>(start: F) -> std::result::Result<Utf8PathBuf, color_eyre::Report>
where
    F: FnOnce() -> BootstrapResult<(ClusterHandle, ClusterGuard)>,
{
    let (handle, guard) = start().map_err(color_eyre::Report::from)?;
    let data_dir = ensure_postmaster_running(&handle)?;

    // Verify handle methods work
    ensure!(
        handle
            .database_exists("postgres")
            .map_err(color_eyre::Report::from)?,
        "postgres database should exist"
    );

    // Drop guard to trigger shutdown
    drop(guard);
    Ok(data_dir)
}

/// Builds an explicit configuration that points at the sandbox directories.
///
/// The explicit path must not read `PG_RUNTIME_DIR` or `PG_DATA_DIR`, so the
/// sandbox paths are supplied directly instead of through the environment. The
/// worker binary is resolved through the test-support helper and supplied
/// explicitly, so a root run does not depend on `pg_worker` being on `PATH`.
fn explicit_config(sandbox: &TestSandbox) -> Result<TestBootstrapConfig> {
    let config = TestBootstrapConfig::new(PgEnvCfg {
        runtime_dir: Some(sandbox.install_dir().to_owned()),
        data_dir: Some(sandbox.data_dir().to_owned()),
        superuser: Some("postgres".to_owned()),
        password: Some("postgres".to_owned()),
        ..PgEnvCfg::default()
    });

    let Some(worker) = test_support::worker_binary_for_tests() else {
        return Ok(config);
    };
    let worker_path = Utf8PathBuf::from_path_buf(PathBuf::from(worker))
        .map_err(|path| eyre!("worker binary path is not valid UTF-8: {}", path.display()))?;
    Ok(config.with_worker_binary(worker_path))
}

// ============================================================================
// Deref behaviour tests
// ============================================================================

/// Tests that `TestCluster` derefs to `ClusterHandle`.
///
/// Verifies that methods available on `ClusterHandle` can be called
/// directly on `TestCluster` through the `Deref` implementation.
#[rstest]
fn test_cluster_derefs_to_cluster_handle(serial_guard: ScenarioSerialGuard) -> Result<()> {
    let sandbox = TestSandbox::new("deref-test").context("create test sandbox")?;
    sandbox.reset()?;

    let result = sandbox.with_env(sandbox.env_without_timezone(), run_deref_test);

    if should_skip_on_error(&result) {
        return Ok(());
    }
    result?;
    drop(serial_guard);
    Ok(())
}

fn run_deref_test() -> std::result::Result<(), color_eyre::Report> {
    let cluster = TestCluster::new().map_err(color_eyre::Report::from)?;

    // These methods are on ClusterHandle, accessed via Deref
    let _settings = cluster.settings();
    let _environment = cluster.environment();
    let _bootstrap = cluster.bootstrap();

    // Verify delegation methods work through Deref
    ensure!(
        cluster
            .database_exists("postgres")
            .map_err(color_eyre::Report::from)?,
        "should access database_exists through Deref"
    );

    // The fact that settings(), environment(), bootstrap(), and database_exists()
    // all work on `cluster` proves that Deref to ClusterHandle is working,
    // since these methods are defined on ClusterHandle, not TestCluster.

    Ok(())
}

/// Generic skip helper for tests returning `Result<T, color_eyre::Report>`.
fn should_skip_on_error<T>(result: &std::result::Result<T, color_eyre::Report>) -> bool {
    let Err(err) = result else {
        return false;
    };
    let message = err.to_string();
    let debug = format!("{err:?}");
    cluster_skip_message(&message, Some(&debug))
        .map(|reason| {
            tracing::warn!("{reason}");
        })
        .is_some()
}

// ============================================================================
// start_async_split() / start_async_split_with() tests
// ============================================================================

/// Tests that the async split constructors create a working handle/guard pair.
#[cfg(feature = "async-api")]
#[rstest]
#[case::ambient(SplitSource::Ambient)]
#[case::explicit(SplitSource::Explicit)]
fn start_async_split_creates_working_handle_and_guard(
    serial_guard: ScenarioSerialGuard,
    #[case] source: SplitSource,
) -> Result<()> {
    let sandbox = TestSandbox::new(source.sandbox_name(true)).context("create test sandbox")?;
    sandbox.reset()?;

    let env_before = EnvSnapshot::capture();
    let result = sandbox.with_env(sandbox.env_without_timezone(), || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| eyre!("failed to build runtime: {e}"))?
            .block_on(async {
                match source {
                    SplitSource::Ambient => {
                        run_async_split_lifecycle_test(TestCluster::start_async_split()).await
                    }
                    SplitSource::Explicit => {
                        let config = explicit_config(&sandbox)?;
                        run_async_split_lifecycle_test(TestCluster::start_async_split_with(config))
                            .await
                    }
                }
            })
    });

    if should_skip_on_error(&result) {
        return Ok(());
    }
    // Shutdown is verified inside run_async_split_lifecycle_test() to ensure
    // the spawned cleanup task has time to execute before the runtime drops.
    result?;

    // Verify environment restored
    let env_after = EnvSnapshot::capture();
    ensure!(
        env_before == env_after,
        "environment should be restored after guard drops"
    );

    drop(serial_guard);
    Ok(())
}

#[cfg(feature = "async-api")]
async fn run_async_split_lifecycle_test<F>(start: F) -> std::result::Result<(), color_eyre::Report>
where
    F: Future<Output = BootstrapResult<(ClusterHandle, ClusterGuard)>>,
{
    let (handle, guard) = start.await.map_err(color_eyre::Report::from)?;
    let data_dir = ensure_postmaster_running(&handle)?;

    // Drop guard to trigger shutdown
    drop(guard);

    // Wait for cleanup inside async context so the spawned cleanup task can complete.
    // The guard's Drop spawns a fire-and-forget task; we must stay in the async context
    // long enough for it to run before the runtime is dropped.
    wait_for_postmaster_shutdown_async(&data_dir).await?;

    Ok(())
}

// ============================================================================
// Test helpers
// ============================================================================

const POSTMASTER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const POSTMASTER_POLL_INTERVAL: Duration = Duration::from_millis(50);
const POSTMASTER_SHUTDOWN_ERROR: &str =
    "postmaster.pid should be removed once cluster stops (waited 10s)";

/// Extracts the cluster data directory and asserts the postmaster is running.
fn ensure_postmaster_running(
    handle: &ClusterHandle,
) -> std::result::Result<Utf8PathBuf, color_eyre::Report> {
    let data_dir = Utf8PathBuf::from_path_buf(handle.settings().data_dir.clone())
        .map_err(|_| eyre!("data_dir is not valid UTF-8"))?;

    ensure!(
        data_dir.join("postmaster.pid").exists(),
        "postmaster.pid should exist while cluster runs"
    );
    Ok(data_dir)
}

fn wait_for_postmaster_shutdown(data_dir: &Utf8PathBuf) -> Result<()> {
    use std::time::Instant;

    let pid = data_dir.join("postmaster.pid");
    let deadline = Instant::now() + POSTMASTER_SHUTDOWN_TIMEOUT;

    while pid.exists() && Instant::now() < deadline {
        thread::sleep(POSTMASTER_POLL_INTERVAL);
    }

    ensure!(!pid.exists(), POSTMASTER_SHUTDOWN_ERROR);
    Ok(())
}

/// Async version of shutdown wait for use within tokio runtime.
///
/// The guard's `Drop` spawns a fire-and-forget cleanup task. We must poll
/// for completion inside the async context so the spawned task can execute
/// before the runtime is dropped.
#[cfg(feature = "async-api")]
async fn wait_for_postmaster_shutdown_async(
    data_dir: &Utf8PathBuf,
) -> std::result::Result<(), color_eyre::Report> {
    use std::time::Instant;

    use tokio::time::sleep;

    let pid = data_dir.join("postmaster.pid");
    let deadline = Instant::now() + POSTMASTER_SHUTDOWN_TIMEOUT;

    while pid.exists() && Instant::now() < deadline {
        sleep(POSTMASTER_POLL_INTERVAL).await;
    }

    ensure!(!pid.exists(), POSTMASTER_SHUTDOWN_ERROR);
    Ok(())
}

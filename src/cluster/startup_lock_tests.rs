//! The install tree's setup lock through the production start paths (#289).
//!
//! `setup_lock_tests.rs` proves the release decision and the hook's retake in
//! isolation. These cases drive `start_postgres` and `start_postgres_async`
//! themselves, with a worker-operation hook that reports, at each operation,
//! whether another handle could take the lock: released before `Setup` after a
//! cache hit, held through `Setup` and `Start` on a miss, and free on return.
//! They also check the timing events that let a slow start be attributed to a
//! phase: the wait for the lock, and one `setup` and one `start` step.

use std::sync::{Arc, Mutex};

use camino::Utf8Path;
use color_eyre::eyre::{Result, ensure, eyre};
use fs4::FileExt;
use rstest::rstest;
use serial_test::serial;

use super::*;
use crate::cluster::setup_lock::SETUP_LOCK_FILE;

/// Returns whether another handle can take the setup lock in `install_root`.
fn lock_is_free(install_root: &Utf8Path) -> bool {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(install_root.join(SETUP_LOCK_FILE))
        .is_ok_and(|file| FileExt::try_lock(&file).is_ok())
}

/// A hook that builds the versioned tree on `Setup` and records
/// `<operation>(lock_free=<bool>)` for each operation.
fn recording_hook(
    install_root: &Utf8Path,
) -> Result<(Arc<Mutex<Vec<String>>>, crate::test_support::HookGuard)> {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&recorded);
    let root = install_root.to_path_buf();
    let versioned = install_root.join(TEST_POSTGRES_VERSION);
    let guard = install_run_root_operation_hook(move |_, _, operation| {
        if matches!(operation, crate::cluster::WorkerOperation::Setup) {
            for sub in ["bin", "lib", "share/extension"] {
                std::fs::create_dir_all(versioned.join(sub).as_std_path())
                    .map_err(|err| eyre!("create {sub}: {err}"))?;
            }
        }
        sink.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(format!(
                "{}(lock_free={})",
                operation.as_str(),
                lock_is_free(&root)
            ));
        Ok(())
    })?;
    Ok((recorded, guard))
}

/// Seeds the binary cache with a complete entry for the test version, so the
/// lookup is a hit.
fn seed_cache_hit(cache_dir: &Utf8Path) -> Result<()> {
    let entry = cache_dir.join(TEST_POSTGRES_VERSION);
    std::fs::create_dir_all(entry.join("bin").as_std_path())?;
    std::fs::write(entry.join(".complete").as_std_path(), b"")?;
    Ok(())
}

/// What the recording hook must have seen: on a hit the lock is free at
/// `Setup` and at `Start`; on a miss the start holds it through both, since
/// `Setup` populates the tree, and lets go only when it returns.
fn expected_operations(cache_hit: bool) -> [String; 2] {
    [
        format!("setup(lock_free={cache_hit})"),
        format!("start(lock_free={cache_hit})"),
    ]
}

fn observed(recorded: &Mutex<Vec<String>>) -> Vec<String> {
    recorded
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// The synchronous start releases the lock before `Setup` after a cache hit
/// and holds it through `Setup` on a miss.
#[rstest]
#[case::hit(true)]
#[case::miss(false)]
#[serial(worker_hook)]
fn the_sync_start_keeps_the_setup_lock_only_on_a_cache_miss(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
    #[case] cache_hit: bool,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    if cache_hit {
        seed_cache_hit(&paths.cache_dir)?;
    }
    let (recorded, _guard) = recording_hook(&paths.install_dir)?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &paths.install_dir,
        &paths.data_dir,
        &paths.scoped_cache_home,
    )?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;

    start_postgres(&runtime, bootstrap, &env_vars, &cache_config)?;

    ensure!(
        observed(&recorded) == expected_operations(cache_hit),
        "recorded {:?}",
        observed(&recorded)
    );
    ensure!(
        lock_is_free(&paths.install_dir),
        "the lock is free once the start returns"
    );
    Ok(())
}

/// The asynchronous start makes the same decision.
#[cfg(feature = "async-api")]
#[rstest]
#[case::hit(true)]
#[case::miss(false)]
#[serial(worker_hook)]
fn the_async_start_keeps_the_setup_lock_only_on_a_cache_miss(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
    #[case] cache_hit: bool,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    if cache_hit {
        seed_cache_hit(&paths.cache_dir)?;
    }
    let (recorded, _guard) = recording_hook(&paths.install_dir)?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &paths.install_dir,
        &paths.data_dir,
        &paths.scoped_cache_home,
    )?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;

    runtime.block_on(start_postgres_async(bootstrap, &env_vars, &cache_config))?;

    ensure!(
        observed(&recorded) == expected_operations(cache_hit),
        "recorded {:?}",
        observed(&recorded)
    );
    ensure!(
        lock_is_free(&paths.install_dir),
        "the lock is free once the start returns"
    );
    Ok(())
}

/// Counts the captured lines that contain every one of `needles`.
fn lines_with(logs: &[String], needles: &[&str]) -> usize {
    logs.iter()
        .filter(|line| needles.iter().all(|needle| line.contains(needle)))
        .count()
}

/// One start records the wait for the setup lock once, and each of `Setup` and
/// `Start` once with its elapsed time.
///
/// These events are the evidence a caller uses to say which phase of a slow
/// start grew, so a start that dropped one, logged it twice or lost its
/// duration would leave the diagnosis with nothing to read.
fn ensure_timing_events(logs: &[String]) -> Result<()> {
    for (needles, what) in [
        (
            &["took the install tree's setup lock", "waited_ms="][..],
            "setup lock wait",
        ),
        (
            &["lifecycle step finished", "step=\"setup\"", "elapsed_ms="][..],
            "setup step",
        ),
        (
            &["lifecycle step finished", "step=\"start\"", "elapsed_ms="][..],
            "start step",
        ),
    ] {
        ensure!(
            lines_with(logs, needles) == 1,
            "expected exactly one {what} event, captured {logs:?}"
        );
    }
    Ok(())
}

/// The synchronous start logs its lock wait and both lifecycle steps.
#[rstest]
#[serial(worker_hook)]
fn the_sync_start_logs_the_lock_wait_and_each_step_once(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    seed_cache_hit(&paths.cache_dir)?;
    let (_recorded, _guard) = recording_hook(&paths.install_dir)?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &paths.install_dir,
        &paths.data_dir,
        &paths.scoped_cache_home,
    )?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;

    let (logs, outcome) = crate::test_support::capture_debug_logs(|| {
        start_postgres(&runtime, bootstrap, &env_vars, &cache_config)
    });

    outcome?;
    ensure_timing_events(&logs)
}

/// The asynchronous start logs the same events, the lock wait from the
/// caller's task rather than from the blocking pool that took the lock.
#[cfg(feature = "async-api")]
#[rstest]
#[serial(worker_hook)]
fn the_async_start_logs_the_lock_wait_and_each_step_once(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    seed_cache_hit(&paths.cache_dir)?;
    let (_recorded, _guard) = recording_hook(&paths.install_dir)?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &paths.install_dir,
        &paths.data_dir,
        &paths.scoped_cache_home,
    )?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;

    let (logs, outcome) = crate::test_support::capture_debug_logs(|| {
        runtime.block_on(start_postgres_async(bootstrap, &env_vars, &cache_config))
    });

    outcome?;
    ensure_timing_events(&logs)
}

/// A hook that fails `Setup`, after noting whether a watcher was registered for
/// `data_dir` at that moment. Only `Setup` fails, so the failure comes after the
/// watcher is armed and before `Start`.
#[cfg(target_os = "linux")]
fn failing_setup_hook(
    data_dir: std::path::PathBuf,
    armed_at_setup: Arc<Mutex<Option<bool>>>,
) -> Result<crate::test_support::HookGuard> {
    Ok(install_run_root_operation_hook(move |_, _, operation| {
        if matches!(operation, crate::cluster::WorkerOperation::Setup) {
            *armed_at_setup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some(crate::bootstrap::is_watching(&data_dir));
            return Err(eyre!("setup refused for the test").into());
        }
        Ok(())
    })?)
}

/// Makes `data_dir` a slot, by creating the lock file beside it that marks one.
#[cfg(target_os = "linux")]
fn make_a_slot(data_dir: &Utf8Path) -> Result<()> {
    std::fs::write(format!("{data_dir}.lock"), b"")?;
    Ok(())
}

/// A start that fails after the watcher is armed releases it: the watcher was
/// registered when `Setup` ran, and is gone once the start has returned its
/// error, so a cluster that never came up leaves nothing waiting for the
/// process to exit (#287).
#[cfg(target_os = "linux")]
#[rstest]
#[serial(worker_hook)]
fn a_failed_sync_start_releases_the_armed_watcher(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    make_a_slot(&paths.data_dir)?;
    let armed = Arc::new(Mutex::new(None));
    let _guard = failing_setup_hook(
        paths.data_dir.as_std_path().to_path_buf(),
        Arc::clone(&armed),
    )?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &paths.install_dir,
        &paths.data_dir,
        &paths.scoped_cache_home,
    )?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;

    let outcome = start_postgres(&runtime, bootstrap, &env_vars, &cache_config);

    ensure!(outcome.is_err(), "a refused Setup fails the start");
    let seen = *armed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure!(
        seen == Some(true),
        "the watcher must be armed before Setup runs"
    );
    ensure!(
        !crate::bootstrap::is_watching(paths.data_dir.as_std_path()),
        "a failed start must release its watcher"
    );
    Ok(())
}

/// The asynchronous start releases the armed watcher on failure too.
#[cfg(all(target_os = "linux", feature = "async-api"))]
#[rstest]
#[serial(worker_hook)]
fn a_failed_async_start_releases_the_armed_watcher(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    make_a_slot(&paths.data_dir)?;
    let armed = Arc::new(Mutex::new(None));
    let _guard = failing_setup_hook(
        paths.data_dir.as_std_path().to_path_buf(),
        Arc::clone(&armed),
    )?;
    let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
    configure_root_bootstrap(
        &mut bootstrap,
        &paths.install_dir,
        &paths.data_dir,
        &paths.scoped_cache_home,
    )?;
    let env_vars = bootstrap.environment.to_env();
    let cache_config = BinaryCacheConfig::with_dir(paths.cache_dir.clone());
    let runtime = test_runtime()?;

    let outcome = runtime.block_on(start_postgres_async(bootstrap, &env_vars, &cache_config));

    ensure!(outcome.is_err(), "a refused Setup fails the start");
    let seen = *armed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure!(
        seen == Some(true),
        "the watcher must be armed before Setup runs"
    );
    ensure!(
        !crate::bootstrap::is_watching(paths.data_dir.as_std_path()),
        "a failed start must release its watcher"
    );
    Ok(())
}

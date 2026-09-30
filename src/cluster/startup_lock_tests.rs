//! The install tree's setup lock through the production start paths (#289).
//!
//! `setup_lock_tests.rs` proves the release decision and the hook's retake in
//! isolation. These cases drive `start_postgres` and `start_postgres_async`
//! themselves, with a worker-operation hook that reports, at each operation,
//! whether another handle could take the lock: released before `Setup` after a
//! cache hit, held through `Setup` and `Start` on a miss, and free on return.

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

//! Extension hook ordering: install happens after `Setup` and before `Start`
//! in the root, async-root and setup-only lifecycles.

use std::sync::{Arc, Mutex};

use color_eyre::eyre::{Result, ensure, eyre};
use rstest::rstest;
use serial_test::serial;

use super::*;
use crate::cluster::extension_hook::populate_cache_on_miss;

#[path = "startup_extension_fixtures.rs"]
mod fixtures;

use fixtures::{PROBE_FILES, failing_bootstrap, lifecycle_case, ordering_bootstrap};

fn assert_installed_between_setup_and_start(
    operations: &Mutex<Vec<String>>,
    bootstrap: &TestBootstrapSettings,
) -> Result<()> {
    let recorded = operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    ensure!(
        recorded == ["setup", "start(extension_present=true)"],
        "expected install between Setup and Start, recorded {recorded:?}"
    );
    ensure!(
        bootstrap.installed_extensions.len() == 1,
        "the handle must report the installed extension"
    );
    Ok(())
}

/// The hook's failure stops the lifecycle: the error reaches the caller and
/// `Start` is never dispatched.
///
/// Without this, a regression that logged the hook error and carried on, or
/// that started the server before installing, would still pass the ordering
/// and pipeline tests, because those only observe the successful path.
fn assert_stopped_before_start(
    operations: &Mutex<Vec<String>>,
    err: &crate::error::BootstrapError,
) -> Result<()> {
    ensure!(
        err.kind() == crate::error::BootstrapErrorKind::ExtensionManifestInvalid,
        "expected ExtensionManifestInvalid, got {:?}",
        err.kind()
    );
    let recorded = operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    ensure!(
        recorded == ["setup"],
        "Start must not be dispatched after a hook failure, recorded {recorded:?}"
    );
    Ok(())
}

/// The synchronous root lifecycle stops when the hook fails.
#[rstest]
#[serial(worker_hook)]
fn root_lifecycle_stops_when_the_extension_hook_fails(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let case = lifecycle_case(&paths, failing_bootstrap)?;
    let err = start_postgres(
        &case.runtime,
        case.bootstrap,
        &case.env_vars,
        &case.cache_config,
    )
    .err()
    .ok_or_else(|| eyre!("a broken manifest must stop the lifecycle"))?;
    assert_stopped_before_start(&case.hook.operations, &err)
}

/// The asynchronous root lifecycle stops on the same failure.
#[cfg(feature = "async-api")]
#[rstest]
#[serial(worker_hook)]
fn async_root_lifecycle_stops_when_the_extension_hook_fails(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let case = lifecycle_case(&paths, failing_bootstrap)?;
    let err = case
        .runtime
        .block_on(start_postgres_async(
            case.bootstrap,
            &case.env_vars,
            &case.cache_config,
        ))
        .err()
        .ok_or_else(|| eyre!("a broken manifest must stop the lifecycle"))?;
    assert_stopped_before_start(&case.hook.operations, &err)
}

/// The setup-only lifecycle reports the failure and completes nothing.
#[rstest]
#[serial(worker_hook)]
fn setup_only_lifecycle_stops_when_the_extension_hook_fails(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let case = lifecycle_case(&paths, failing_bootstrap)?;
    let err = setup_lifecycle(
        &case.runtime,
        case.bootstrap,
        &case.env_vars,
        &case.cache_config,
    )
    .err()
    .ok_or_else(|| eyre!("a broken manifest must stop the setup-only lifecycle"))?;
    assert_stopped_before_start(&case.hook.operations, &err)
}

/// The synchronous root lifecycle installs extensions after Setup and before Start.
#[rstest]
#[serial(worker_hook)]
fn root_lifecycle_installs_extensions_between_setup_and_start(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let case = lifecycle_case(&paths, ordering_bootstrap)?;
    let outcome = start_postgres(
        &case.runtime,
        case.bootstrap,
        &case.env_vars,
        &case.cache_config,
    )?;
    assert_installed_between_setup_and_start(&case.hook.operations, &outcome.bootstrap)
}

/// The asynchronous root lifecycle keeps the same ordering.
#[cfg(feature = "async-api")]
#[rstest]
#[serial(worker_hook)]
fn async_root_lifecycle_installs_extensions_between_setup_and_start(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let case = lifecycle_case(&paths, ordering_bootstrap)?;
    let outcome = case.runtime.block_on(start_postgres_async(
        case.bootstrap,
        &case.env_vars,
        &case.cache_config,
    ))?;
    assert_installed_between_setup_and_start(&case.hook.operations, &outcome.bootstrap)
}

/// The CLI setup-only lifecycle installs extensions after Setup without starting.
#[rstest]
#[serial(worker_hook)]
fn setup_only_lifecycle_installs_extensions_after_setup(
    #[from(root_setup_paths)] root_setup_paths_res: Result<Arc<RootSetupPaths>>,
) -> Result<()> {
    let paths = root_setup_paths_res?;
    let case = lifecycle_case(&paths, ordering_bootstrap)?;
    let prepared = setup_lifecycle(
        &case.runtime,
        case.bootstrap,
        &case.env_vars,
        &case.cache_config,
    )?;
    let recorded = case
        .hook
        .operations
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    ensure!(
        recorded == ["setup"],
        "setup-only must not start: {recorded:?}"
    );
    let versioned = paths.install_dir.join(TEST_POSTGRES_VERSION);
    ensure!(
        PROBE_FILES
            .iter()
            .all(|(name, _)| versioned.join(name).is_file()),
        "extension files must be installed by the setup-only path"
    );
    ensure!(
        prepared.installed_extensions.len() == 1,
        "report carried through"
    );
    Ok(())
}

/// Binary-cache population runs only on a miss.
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

    let hit = PostSetup {
        cache_config: &cache_config,
        cache_hit: true,
    };
    populate_cache_on_miss(hit, &bootstrap);
    ensure!(
        !cache_population_paths.marker_path.exists(),
        "cache marker should remain absent on cache hit"
    );

    let miss = PostSetup {
        cache_config: &cache_config,
        cache_hit: false,
    };
    populate_cache_on_miss(miss, &bootstrap);
    ensure!(
        cache_population_paths.marker_path.exists(),
        "cache marker should be written on cache miss"
    );
    Ok(())
}

//! Setup-only lifecycle used by the CLI binary: download and `initdb` without
//! starting the server, with the same post-setup hook as a full start.

use postgresql_embedded::PostgreSQL;
use tokio::runtime::Runtime;
use tracing::info;

use super::{
    ClusterWorkerInvoker,
    LifecycleStep,
    PostSetup,
    cache_config_from_bootstrap,
    cache_integration,
    invoke_root_operation,
    invoke_unprivileged_operation,
    log_lifecycle_start,
    run_post_setup,
};
use crate::{
    ExecutionPrivileges,
    TestBootstrapSettings,
    cache::BinaryCacheConfig,
    env::ScopedEnv,
    error::BootstrapResult,
    observability::LOG_TARGET,
};

/// Performs `PostgreSQL` setup (download + `initdb`) without starting the server.
///
/// This entry point is intended for the CLI binary, which prepares the
/// installation and data directory so that a subsequent `TestCluster::new()`
/// can reuse the cached binaries without a redundant download.
///
/// The cache directory is resolved from the host environment *before*
/// `ScopedEnv` is applied, matching the resolution order in
/// `TestCluster::new_split()` so the CLI and test runs share the same cache.
pub(crate) fn setup_postgres_only(
    bootstrap: TestBootstrapSettings,
) -> BootstrapResult<TestBootstrapSettings> {
    // Resolve cache directory from the host environment before applying the
    // scoped sandbox, matching the resolution order in TestCluster::new_split().
    let cache_config = cache_config_from_bootstrap(&bootstrap);
    let env_vars = bootstrap.environment.to_env();
    let _env_guard = ScopedEnv::apply(&env_vars);

    crate::cluster::runtime::run_with_runtime("setup_postgres_only", move |runtime| {
        setup_lifecycle(runtime, bootstrap, &env_vars, &cache_config)
    })
}

/// Drives the setup-only lifecycle (download + `initdb`), populating the
/// binary cache on a miss.
pub(in crate::cluster) fn setup_lifecycle(
    runtime: &Runtime,
    mut bootstrap: TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    cache_config: &BinaryCacheConfig,
) -> BootstrapResult<TestBootstrapSettings> {
    let privileges = bootstrap.privileges;
    log_lifecycle_start(privileges, &bootstrap, false);

    let version_req = bootstrap.settings.version.clone();
    let cache_hit =
        cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

    setup_with_privileges(privileges, runtime, &mut bootstrap, env_vars)?;
    run_post_setup(
        &mut bootstrap,
        PostSetup {
            cache_config,
            cache_hit,
        },
    )?;
    log_setup_complete(privileges, cache_hit);

    Ok(bootstrap)
}

/// Runs the privilege-aware `Setup` operation only (no `Start`).
pub(in crate::cluster) fn setup_with_privileges(
    privileges: ExecutionPrivileges,
    runtime: &Runtime,
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
) -> BootstrapResult<()> {
    if privileges == ExecutionPrivileges::Root {
        let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        invoke_root_operation(&invoker, LifecycleStep::Setup)
    } else {
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
        invoke_unprivileged_operation(&invoker, &mut embedded, LifecycleStep::Setup)
    }
}

/// Logs completion of the setup-only lifecycle.
fn log_setup_complete(privileges: ExecutionPrivileges, cache_hit: bool) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        cache_hit,
        "embedded postgres setup complete (server not started)"
    );
}

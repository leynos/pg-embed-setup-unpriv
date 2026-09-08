//! Startup orchestration for `TestCluster` and the CLI setup-only path.
//!
//! Contains logic for bootstrapping and starting the embedded `PostgreSQL` instance,
//! including cache integration, lifecycle invocation, and privilege handling.
//! The [`setup_postgres_only`] entry point drives download + `initdb` without
//! starting the server, used by the CLI binary.

use postgresql_embedded::PostgreSQL;
use tokio::runtime::Runtime;
use tracing::info;

#[cfg(feature = "async-api")]
use super::worker_invoker::AsyncInvoker;
use super::{
    cache_integration,
    extension_hook::{PostSetup, run_post_setup},
    installation,
    worker_invoker::WorkerInvoker as ClusterWorkerInvoker,
    worker_operation,
};
use crate::{
    ExecutionPrivileges,
    TestBootstrapSettings,
    cache::BinaryCacheConfig,
    error::BootstrapResult,
    observability::LOG_TARGET,
};

#[path = "startup_setup_only.rs"]
mod setup_only;
pub(crate) use self::setup_only::setup_postgres_only;
#[cfg(test)]
pub(super) use self::setup_only::{setup_lifecycle, setup_with_privileges};

#[derive(Clone, Copy)]
enum LifecycleStep {
    Setup,
    Start,
}

impl LifecycleStep {
    const fn worker_operation(self) -> worker_operation::WorkerOperation {
        match self {
            Self::Setup => worker_operation::WorkerOperation::Setup,
            Self::Start => worker_operation::WorkerOperation::Start,
        }
    }
}

/// Outcome from starting the `PostgreSQL` instance.
pub(super) struct StartupOutcome {
    pub(super) bootstrap: TestBootstrapSettings,
    pub(super) postgres: Option<PostgreSQL>,
    pub(super) is_managed_via_worker: bool,
}

/// Creates a `BinaryCacheConfig` from bootstrap settings.
///
/// Uses the explicitly configured `binary_cache_dir` if set, otherwise
/// falls back to the default resolution from environment variables.
pub(super) fn cache_config_from_bootstrap(bootstrap: &TestBootstrapSettings) -> BinaryCacheConfig {
    bootstrap
        .binary_cache_dir
        .as_ref()
        .map_or_else(BinaryCacheConfig::new, |dir| {
            BinaryCacheConfig::with_dir(dir.clone())
        })
}

/// Starts the `PostgreSQL` instance with privilege-aware lifecycle handling.
pub(super) fn start_postgres(
    runtime: &Runtime,
    mut bootstrap: TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    cache_config: &BinaryCacheConfig,
) -> BootstrapResult<StartupOutcome> {
    let privileges = bootstrap.privileges;
    log_lifecycle_start(privileges, &bootstrap, false);

    let version_req = bootstrap.settings.version.clone();
    let cache_hit =
        cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

    let context = LifecycleContext {
        runtime,
        env_vars,
        post: PostSetup {
            cache_config,
            cache_hit,
        },
    };
    let (is_managed_via_worker, postgres) =
        handle_privilege_lifecycle(privileges, &context, &mut bootstrap)?;

    log_lifecycle_complete(privileges, is_managed_via_worker, cache_hit, false);

    Ok(StartupOutcome {
        bootstrap,
        postgres,
        is_managed_via_worker,
    })
}

/// Logs the start of the lifecycle.
fn log_lifecycle_start(
    privileges: ExecutionPrivileges,
    bootstrap: &TestBootstrapSettings,
    is_async: bool,
) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        mode = ?bootstrap.execution_mode,
        async_mode = is_async,
        "starting embedded postgres lifecycle"
    );
}

/// Logs completion of the lifecycle.
fn log_lifecycle_complete(
    privileges: ExecutionPrivileges,
    is_managed_via_worker: bool,
    cache_hit: bool,
    is_async: bool,
) {
    info!(
        target: LOG_TARGET,
        privileges = ?privileges,
        worker_managed = is_managed_via_worker,
        cache_hit,
        async_mode = is_async,
        "embedded postgres started"
    );
}

/// Inputs shared by the setup and start steps of one lifecycle run.
#[derive(Clone, Copy)]
struct LifecycleContext<'a> {
    runtime: &'a Runtime,
    env_vars: &'a [(String, Option<String>)],
    post: PostSetup<'a>,
}

/// Handles the privilege-aware lifecycle invocation.
///
/// Returns a tuple of `(is_managed_via_worker, postgres_handle)` where:
/// - Root execution: worker-managed (true, None)
/// - Unprivileged execution: in-process (false, Some(embedded))
fn handle_privilege_lifecycle(
    privileges: ExecutionPrivileges,
    context: &LifecycleContext<'_>,
    bootstrap: &mut TestBootstrapSettings,
) -> BootstrapResult<(bool, Option<PostgreSQL>)> {
    if privileges == ExecutionPrivileges::Root {
        invoke_lifecycle_root(context, bootstrap)?;
        Ok((true, None))
    } else {
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        invoke_lifecycle(context, bootstrap, &mut embedded)?;
        Ok((false, prepare_postgres_handle(false, bootstrap, embedded)))
    }
}

/// Invokes a root lifecycle operation via the worker subprocess.
fn invoke_root_operation(
    invoker: &ClusterWorkerInvoker<'_>,
    step: LifecycleStep,
) -> BootstrapResult<()> {
    invoker.invoke_as_root(step.worker_operation())
}

/// Invokes an unprivileged lifecycle operation in-process.
fn invoke_unprivileged_operation(
    invoker: &ClusterWorkerInvoker<'_>,
    embedded: &mut PostgreSQL,
    step: LifecycleStep,
) -> BootstrapResult<()> {
    match step {
        LifecycleStep::Setup => invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            embedded.setup().await
        }),
        LifecycleStep::Start => invoker.invoke(worker_operation::WorkerOperation::Start, async {
            embedded.start().await
        }),
    }
}

/// Prepares the `PostgreSQL` handle based on whether it's worker-managed.
pub(super) fn prepare_postgres_handle(
    is_managed_via_worker: bool,
    bootstrap: &mut TestBootstrapSettings,
    embedded: PostgreSQL,
) -> Option<PostgreSQL> {
    if is_managed_via_worker {
        None
    } else {
        bootstrap.settings = embedded.settings().clone();
        Some(embedded)
    }
}

/// Runs `Setup`, the post-setup hook, `Start` and the port refresh, using
/// `dispatch` to execute each step either via the worker or in-process.
fn run_lifecycle_steps<F>(
    context: &LifecycleContext<'_>,
    bootstrap: &mut TestBootstrapSettings,
    mut dispatch: F,
) -> BootstrapResult<()>
where
    F: FnMut(&ClusterWorkerInvoker<'_>, LifecycleStep) -> BootstrapResult<()>,
{
    let setup_invoker = ClusterWorkerInvoker::new(context.runtime, bootstrap, context.env_vars);
    dispatch(&setup_invoker, LifecycleStep::Setup)?;
    run_post_setup(bootstrap, context.post)?;
    let start_invoker = ClusterWorkerInvoker::new(context.runtime, bootstrap, context.env_vars);
    dispatch(&start_invoker, LifecycleStep::Start)?;
    installation::refresh_worker_port(bootstrap)
}

/// Invokes the lifecycle for root-privileged execution via worker subprocess.
fn invoke_lifecycle_root(
    context: &LifecycleContext<'_>,
    bootstrap: &mut TestBootstrapSettings,
) -> BootstrapResult<()> {
    run_lifecycle_steps(context, bootstrap, invoke_root_operation)
}

/// Invokes the lifecycle for unprivileged in-process execution.
fn invoke_lifecycle(
    context: &LifecycleContext<'_>,
    bootstrap: &mut TestBootstrapSettings,
    embedded: &mut PostgreSQL,
) -> BootstrapResult<()> {
    run_lifecycle_steps(context, bootstrap, |invoker, step| {
        invoke_unprivileged_operation(invoker, embedded, step)
    })
}

// ============================================================================
// Async API (feature-gated)
// ============================================================================

/// Async variant of `start_postgres` that runs on the caller's runtime.
#[cfg(feature = "async-api")]
pub(super) async fn start_postgres_async(
    mut bootstrap: TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    cache_config: &BinaryCacheConfig,
) -> BootstrapResult<StartupOutcome> {
    let privileges = bootstrap.privileges;
    log_lifecycle_start(privileges, &bootstrap, true);

    // Try to use cached binaries before starting the lifecycle
    let version_req = bootstrap.settings.version.clone();
    let cache_hit =
        cache_integration::try_use_binary_cache(cache_config, &version_req, &mut bootstrap);

    let post = PostSetup {
        cache_config,
        cache_hit,
    };
    let (is_managed_via_worker, postgres) = if privileges == ExecutionPrivileges::Root {
        Box::pin(invoke_lifecycle_root_async(&mut bootstrap, env_vars, post)).await?;
        (true, None)
    } else {
        let mut embedded = PostgreSQL::new(bootstrap.settings.clone());
        Box::pin(invoke_lifecycle_async(
            &mut bootstrap,
            env_vars,
            post,
            &mut embedded,
        ))
        .await?;
        (
            false,
            prepare_postgres_handle(false, &mut bootstrap, embedded),
        )
    };

    log_lifecycle_complete(privileges, is_managed_via_worker, cache_hit, true);
    Ok(StartupOutcome {
        bootstrap,
        postgres,
        is_managed_via_worker,
    })
}

/// Async variant of `invoke_lifecycle`.
#[cfg(feature = "async-api")]
async fn invoke_lifecycle_async(
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    post: PostSetup<'_>,
    embedded: &mut PostgreSQL,
) -> BootstrapResult<()> {
    let invoker = AsyncInvoker::new(bootstrap, env_vars);
    Box::pin(
        invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            embedded.setup().await
        }),
    )
    .await?;
    super::extension_hook::run_post_setup_async(bootstrap, post).await?;
    let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
    Box::pin(
        start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
            embedded.start().await
        }),
    )
    .await?;
    installation::refresh_worker_port_async(bootstrap).await
}

/// Async variant of `invoke_lifecycle_root`.
#[cfg(feature = "async-api")]
async fn invoke_lifecycle_root_async(
    bootstrap: &mut TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    post: PostSetup<'_>,
) -> BootstrapResult<()> {
    let setup_invoker = AsyncInvoker::new(bootstrap, env_vars);
    // No-op future: the worker subprocess performs the actual setup; this drives the invocation.
    Box::pin(
        setup_invoker.invoke(worker_operation::WorkerOperation::Setup, async {
            Ok::<(), postgresql_embedded::Error>(())
        }),
    )
    .await?;
    super::extension_hook::run_post_setup_async(bootstrap, post).await?;
    let start_invoker = AsyncInvoker::new(bootstrap, env_vars);
    // No-op future: the worker subprocess performs the actual start; this drives the invocation.
    Box::pin(
        start_invoker.invoke(worker_operation::WorkerOperation::Start, async {
            Ok::<(), postgresql_embedded::Error>(())
        }),
    )
    .await?;
    installation::refresh_worker_port_async(bootstrap).await
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod startup_tests;

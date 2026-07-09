//! Async worker invoker operating on the caller's Tokio runtime.
//!
//! Provides the `async-api` counterpart of [`WorkerInvoker`], awaiting
//! in-process operations directly and delegating privileged operations to the
//! worker subprocess via `spawn_blocking`.

use std::future::Future;

use color_eyre::eyre::{Context, eyre};

use super::{
    WorkerInvoker,
    create_lifecycle_span,
    execute_root_operation,
    log_in_process_start,
    log_worker_dispatch,
    timeout_error,
};
use crate::{
    ExecutionPrivileges,
    TestBootstrapSettings,
    cluster::WorkerOperation,
    error::{BootstrapError, BootstrapResult},
};

/// Async variant of [`WorkerInvoker`] that operates on the caller's runtime.
///
/// Use this invoker when running within an existing async context (e.g., inside
/// `#[tokio::test]`). Unlike `WorkerInvoker`, this does not require an owned
/// runtime reference since it directly `.await`s futures.
#[derive(Debug)]
pub(crate) struct AsyncInvoker<'a> {
    bootstrap: &'a TestBootstrapSettings,
    env_vars: &'a [(String, Option<String>)],
}

impl<'a> AsyncInvoker<'a> {
    /// Creates an async invoker bound to bootstrap configuration and environment.
    pub(crate) const fn new(
        bootstrap: &'a TestBootstrapSettings,
        env_vars: &'a [(String, Option<String>)],
    ) -> Self {
        Self {
            bootstrap,
            env_vars,
        }
    }

    /// Executes an operation asynchronously, either in-process or via the worker.
    ///
    /// For unprivileged operations, the future is awaited directly.
    /// For root operations, the synchronous worker is spawned via `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns a [`BootstrapError`] when the operation fails.
    pub(crate) async fn invoke<Fut>(
        &self,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        let span = self.lifecycle_span(operation);
        let _entered = span.enter();

        let result = self
            .dispatch_operation_async(operation, in_process_op)
            .await;
        WorkerInvoker::log_outcome(operation, &result);
        result
    }

    async fn dispatch_operation_async<Fut>(
        &self,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        match self.bootstrap.privileges {
            ExecutionPrivileges::Unprivileged => {
                self.run_unprivileged_async(operation, in_process_op).await
            }
            ExecutionPrivileges::Root => self.run_root_async(operation).await,
        }
    }

    async fn run_unprivileged_async<Fut>(
        &self,
        operation: WorkerOperation,
        in_process_op: Fut,
    ) -> BootstrapResult<()>
    where
        Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
    {
        log_in_process_start(operation, true);
        let timeout = operation.timeout(self.bootstrap);
        invoke_unprivileged_async(in_process_op, operation.error_context(), timeout).await
    }

    async fn run_root_async(&self, operation: WorkerOperation) -> BootstrapResult<()> {
        log_worker_dispatch(
            operation,
            self.bootstrap.worker_binary.as_ref().map(|p| p.as_str()),
            true,
        );
        // Worker subprocess spawning is inherently blocking; use spawn_blocking.
        let bootstrap = (*self.bootstrap).clone();
        let env_vars = self.env_vars.to_vec();
        tokio::task::spawn_blocking(move || {
            execute_root_operation(&bootstrap, &env_vars, operation)
        })
        .await
        .map_err(|err| BootstrapError::from(eyre!("worker task panicked: {err}")))?
    }

    fn lifecycle_span(&self, operation: WorkerOperation) -> tracing::Span {
        create_lifecycle_span(operation, self.bootstrap, true)
    }
}

/// Awaits an unprivileged operation's future with timeout protection.
async fn invoke_unprivileged_async<Fut>(
    future: Fut,
    ctx: &'static str,
    timeout: std::time::Duration,
) -> BootstrapResult<()>
where
    Fut: Future<Output = Result<(), postgresql_embedded::Error>> + Send,
{
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| timeout_error(ctx, timeout))?
        .context(ctx)
        .map_err(BootstrapError::from)
}

//! Async lifecycle API for [`TestCluster`].
//!
//! Hosts the `async-api` feature surface: asynchronous startup on the
//! caller's Tokio runtime and explicit asynchronous shutdown.

use tracing::info_span;

use super::{
    TestCluster,
    guard::ClusterGuard,
    handle::ClusterHandle,
    runtime_mode::ClusterRuntime,
    shutdown,
    startup::{cache_config_from_bootstrap, start_postgres_async},
};
use crate::{
    bootstrap_for_tests,
    env::ScopedEnv,
    error::BootstrapResult,
    observability::LOG_TARGET,
};

enum StopAsyncPath {
    WorkerManaged,
    InProcess(Box<postgresql_embedded::PostgreSQL>),
    Noop,
}

impl TestCluster {
    /// Boots a `PostgreSQL` instance asynchronously for use in `#[tokio::test]` contexts.
    ///
    /// Unlike [`TestCluster::new`], this constructor does not create its own Tokio runtime.
    /// Instead, it runs on the caller's async runtime, making it safe to call from within
    /// `#[tokio::test]` and other async contexts.
    ///
    /// **Important:** Clusters created with `start_async()` should be shut down explicitly
    /// using [`stop_async()`](Self::stop_async). The `Drop` implementation will attempt
    /// best-effort cleanup but may not succeed if the runtime is no longer available.
    ///
    /// # Errors
    ///
    /// Returns an error if the bootstrap configuration cannot be prepared or if starting
    /// the embedded cluster fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// #[tokio::test]
    /// async fn test_with_embedded_postgres() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    ///     let cluster = TestCluster::start_async().await?;
    ///     let url = cluster.settings().url("my_database");
    ///     // ... async database operations ...
    ///     cluster.stop_async().await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn start_async() -> BootstrapResult<Self> {
        let (handle, guard) = Self::start_async_split().await?;
        Ok(Self { handle, guard })
    }

    /// Boots a `PostgreSQL` instance asynchronously and returns a separate handle and guard.
    ///
    /// This is the async equivalent of [`new_split()`](Self::new_split).
    ///
    /// # Returns
    ///
    /// A tuple of:
    /// - [`ClusterHandle`]: `Send + Sync` handle for accessing the cluster
    /// - [`ClusterGuard`]: `!Send` guard managing shutdown and environment
    ///
    /// # Errors
    ///
    /// Returns an error if the bootstrap configuration cannot be prepared or if
    /// starting the embedded cluster fails.
    pub async fn start_async_split() -> BootstrapResult<(ClusterHandle, ClusterGuard)> {
        use tracing::Instrument;

        let span = info_span!(target: LOG_TARGET, "test_cluster", async_mode = true);

        // Sync bootstrap preparation (no await needed).
        // Resolve cache directory BEFORE applying test environment.
        // Otherwise, the test sandbox's XDG_CACHE_HOME would be used.
        let initial_bootstrap = bootstrap_for_tests()?;
        let cache_config = cache_config_from_bootstrap(&initial_bootstrap);
        let env_vars = initial_bootstrap.environment.to_env();
        let env_guard = ScopedEnv::apply(&env_vars);

        // Async postgres startup, instrumented with the span.
        // Box::pin to avoid large future on the stack.
        let outcome = Box::pin(start_postgres_async(
            initial_bootstrap,
            &env_vars,
            &cache_config,
        ))
        .instrument(span.clone())
        .await?;

        let handle = ClusterHandle::new(outcome.bootstrap.clone());
        let guard = ClusterGuard {
            runtime: ClusterRuntime::Async,
            postgres: outcome.postgres,
            bootstrap: outcome.bootstrap,
            is_managed_via_worker: outcome.is_managed_via_worker,
            env_vars,
            worker_guard: None,
            _env_guard: env_guard,
            _cluster_span: span,
        };

        Ok((handle, guard))
    }

    fn stop_async_path(&mut self) -> StopAsyncPath {
        if self.guard.is_managed_via_worker {
            StopAsyncPath::WorkerManaged
        } else if let Some(postgres) = self.guard.postgres.take() {
            StopAsyncPath::InProcess(Box::new(postgres))
        } else {
            StopAsyncPath::Noop
        }
    }

    /// Explicitly shuts down an async cluster.
    ///
    /// This method should be called for clusters created with [`start_async()`](Self::start_async)
    /// to ensure proper cleanup. It consumes `self` to prevent the `Drop` implementation from
    /// attempting duplicate shutdown.
    ///
    /// For worker-managed clusters (root privileges), the worker subprocess is invoked
    /// synchronously via `spawn_blocking`.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown operation fails. The cluster resources are released
    /// regardless of whether shutdown succeeds.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use pg_embedded_setup_unpriv::TestCluster;
    ///
    /// #[tokio::test]
    /// async fn test_explicit_shutdown() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
    ///     let cluster = TestCluster::start_async().await?;
    ///     // ... use cluster ...
    ///     cluster.stop_async().await?;
    ///     Ok(())
    /// }
    /// ```
    pub async fn stop_async(mut self) -> BootstrapResult<()> {
        let context = shutdown::stop_context(self.handle.settings());
        shutdown::log_async_stop(&context, self.guard.is_managed_via_worker);

        match self.stop_async_path() {
            StopAsyncPath::WorkerManaged => {
                shutdown::stop_worker_managed_async(
                    &self.guard.bootstrap,
                    &self.guard.env_vars,
                    &context,
                )
                .await
            }
            StopAsyncPath::InProcess(postgres) => {
                let cleanup = shutdown::InProcessCleanup {
                    cleanup_mode: self.guard.bootstrap.cleanup_mode,
                    settings: &self.guard.bootstrap.settings,
                    context: &context,
                };
                shutdown::stop_in_process_async(
                    *postgres,
                    self.guard.bootstrap.shutdown_timeout,
                    cleanup,
                )
                .await
            }
            StopAsyncPath::Noop => Ok(()),
        }
    }
}

//! RAII wrapper that boots an embedded `PostgreSQL` instance for tests.
//!
//! The cluster starts during [`TestCluster::new`] and shuts down automatically when the
//! value drops out of scope.
//!
//! # Synchronous API
//!
//! Use [`TestCluster::new`] from synchronous contexts or when you want the cluster to
//! own its own Tokio runtime:
//!
//! ```no_run
//! use pg_embedded_setup_unpriv::TestCluster;
//!
//! # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//! let cluster = TestCluster::new()?;
//! let url = cluster.settings().url("my_database");
//! // Perform test database work here.
//! drop(cluster); // `PostgreSQL` stops automatically.
//!
//! # Ok(())
//! # }
//! ```
//!
//! # Async API
//!
//! When running within an existing async runtime (e.g., `#[tokio::test]`), use
//! [`TestCluster::start_async`] to avoid the "Cannot start a runtime from within a
//! runtime" panic that occurs when nesting Tokio runtimes:
//!
//! ```ignore
//! use pg_embedded_setup_unpriv::TestCluster;
//!
//! #[tokio::test]
//! async fn test_with_embedded_postgres() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
//!     let cluster = TestCluster::start_async().await?;
//!     let url = cluster.settings().url("my_database");
//!     // ... async database operations ...
//!     cluster.stop_async().await?;
//!     Ok(())
//! }
//! ```
//!
//! The async API requires the `async-api` feature flag:
//!
//! ```toml
//! [dependencies]
//! pg-embedded-setup-unpriv = { version = "...", features = ["async-api"] }
//! ```

#[cfg(feature = "async-api")]
mod async_api;
mod cache_integration;
mod cleanup;
mod connection;
mod delegation;
mod guard;
mod handle;
mod installation;
mod lifecycle;
mod lifecycle_template;
pub(crate) mod panic_utils;
mod runtime;
mod runtime_mode;
mod shutdown;
#[cfg(any(unix, windows))]
mod shutdown_hook;
mod startup;
mod temporary_database;
mod worker_invoker;
mod worker_operation;
mod lifecycle_loom_tests;
use std::ops::Deref;

use tracing::info_span;

pub(crate) use self::startup::setup_postgres_only;
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub use self::worker_invoker::WorkerInvoker;
#[doc(hidden)]
pub use self::worker_operation::WorkerOperation;
pub use self::{
    connection::{ConnectionMetadata, TestClusterConnection},
    guard::ClusterGuard,
    handle::ClusterHandle,
    lifecycle::DatabaseName,
    temporary_database::TemporaryDatabase,
};
use self::{
    runtime::build_runtime,
    runtime_mode::ClusterRuntime,
    startup::{cache_config_from_bootstrap, start_postgres},
};
use crate::{
    bootstrap_for_tests,
    env::ScopedEnv,
    error::BootstrapResult,
    observability::LOG_TARGET,
};

/// Embedded `PostgreSQL` instance whose lifecycle follows Rust's drop semantics.
///
/// `TestCluster` combines a [`ClusterHandle`] (for cluster access) with a
/// [`ClusterGuard`] (for lifecycle management). For most use cases, this
/// combined type is the simplest option.
///
/// # Send-Safe Patterns
///
/// `TestCluster` is `!Send` because it contains environment guards that must
/// be dropped on the creating thread. For patterns requiring `Send` (such as
/// `OnceLock` or rstest timeouts), use [`new_split()`](Self::new_split) to
/// obtain a `Send`-safe [`ClusterHandle`]:
///
/// ```no_run
/// use std::sync::OnceLock;
///
/// use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
///
/// static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
///
/// fn shared_cluster() -> &'static ClusterHandle {
///     SHARED.get_or_init(|| {
///         let (handle, guard) = TestCluster::new_split().expect("cluster bootstrap failed");
///         handle
///             .register_shutdown_on_exit()
///             .expect("shutdown hook registration failed");
///         std::mem::forget(guard);
///         handle
///     })
/// }
/// ```
#[derive(Debug)]
pub struct TestCluster {
    /// Send-safe handle providing cluster access.
    pub(crate) handle: ClusterHandle,
    /// Lifecycle guard managing shutdown and environment restoration.
    pub(crate) guard: ClusterGuard,
}

impl TestCluster {
    /// Boots a `PostgreSQL` instance configured by [`bootstrap_for_tests`].
    ///
    /// The constructor blocks until the underlying server process is running and returns an
    /// error when startup fails.
    ///
    /// # Errors
    /// Returns an error if the bootstrap configuration cannot be prepared or if starting the
    /// embedded cluster fails.
    pub fn new() -> BootstrapResult<Self> {
        let (handle, guard) = Self::new_split()?;
        Ok(Self { handle, guard })
    }

    /// Boots a `PostgreSQL` instance and returns a separate handle and guard.
    ///
    /// This constructor is useful for patterns requiring `Send`, such as shared
    /// cluster fixtures with [`OnceLock`](std::sync::OnceLock) or rstest fixtures
    /// with timeouts.
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
    ///
    /// # Examples
    ///
    /// ## Shared Cluster with `OnceLock`
    ///
    /// For shared clusters that run for the entire process lifetime, register
    /// the shutdown hook and forget the guard to prevent shutdown on drop:
    ///
    /// ```no_run
    /// use std::sync::OnceLock;
    ///
    /// use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
    ///
    /// static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
    ///
    /// fn shared_cluster() -> &'static ClusterHandle {
    ///     SHARED.get_or_init(|| {
    ///         let (handle, guard) = TestCluster::new_split().expect("cluster bootstrap failed");
    ///         handle
    ///             .register_shutdown_on_exit()
    ///             .expect("shutdown hook registration failed");
    ///         std::mem::forget(guard);
    ///         handle
    ///     })
    /// }
    /// ```
    ///
    /// **Warning**: Dropping the guard shuts down the cluster. Do not use the
    /// handle after the guard has been dropped unless the guard was forgotten.
    pub fn new_split() -> BootstrapResult<(ClusterHandle, ClusterGuard)> {
        let span = info_span!(target: LOG_TARGET, "test_cluster");
        // Resolve cache directory BEFORE applying test environment.
        // Otherwise, the test sandbox's XDG_CACHE_HOME would be used.
        let (runtime, env_vars, env_guard, outcome) = {
            let _entered = span.enter();
            let initial_bootstrap = bootstrap_for_tests()?;
            let cache_config = cache_config_from_bootstrap(&initial_bootstrap);
            let runtime = build_runtime()?;
            let env_vars = initial_bootstrap.environment.to_env();
            let env_guard = ScopedEnv::apply(&env_vars);
            let outcome = start_postgres(&runtime, initial_bootstrap, &env_vars, &cache_config)?;
            (runtime, env_vars, env_guard, outcome)
        };

        let handle = ClusterHandle::new(outcome.bootstrap.clone());
        let guard = ClusterGuard {
            runtime: ClusterRuntime::Sync(runtime),
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

    /// Extends the cluster lifetime to cover additional scoped environment guards.
    ///
    /// Primarily used by fixtures that need to ensure `PG_EMBEDDED_WORKER` remains set for the
    /// duration of the cluster lifetime.
    #[doc(hidden)]
    #[must_use]
    pub fn with_worker_guard(self, worker_guard: Option<ScopedEnv>) -> Self {
        Self {
            handle: self.handle,
            guard: self.guard.with_worker_guard(worker_guard),
        }
    }
}

/// Provides transparent access to [`ClusterHandle`] methods.
///
/// This allows `TestCluster` to be used interchangeably with `ClusterHandle`
/// for all read-only operations like `settings()`, `connection()`, etc.
impl Deref for TestCluster {
    type Target = ClusterHandle;

    fn deref(&self) -> &Self::Target { &self.handle }
}

// Note: TestCluster does NOT implement Drop because the ClusterGuard handles shutdown.
// When TestCluster drops, its _guard field drops, which triggers ClusterGuard::Drop.

#[cfg(test)]
mod mod_tests;

#[cfg(all(test, feature = "cluster-unit-tests"))]
mod drop_logging_tests;

#[cfg(all(test, not(feature = "cluster-unit-tests")))]
#[path = "../../tests/test_cluster.rs"]
mod test_cluster_tests;

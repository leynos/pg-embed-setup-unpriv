//! Send-safe handle for accessing a running `PostgreSQL` cluster.
//!
//! [`ClusterHandle`] provides thread-safe access to cluster metadata and
//! connection helpers. Unlike [`TestCluster`](super::TestCluster), handles
//! implement [`Send`] and [`Sync`], enabling patterns such as:
//!
//! - Shared cluster fixtures using [`OnceLock`](std::sync::OnceLock)
//! - rstest fixtures with timeouts (which require `Send + 'static`)
//! - Cross-thread sharing in async test patterns
//!
//! # Architecture
//!
//! The handle/guard split separates concerns:
//!
//! - **`ClusterHandle`**: Read-only access to cluster metadata. `Send + Sync`.
//! - **`ClusterGuard`**: Manages environment and shutdown. `!Send`.
//!
//! This separation preserves the safety of thread-local environment management
//! whilst enabling the most common shared cluster use cases.
//!
//! # Examples
//!
//! ```no_run
//! use std::sync::OnceLock;
//!
//! use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
//!
//! static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
//!
//! fn shared_handle() -> &'static ClusterHandle {
//!     SHARED.get_or_init(|| {
//!         let (handle, guard) = TestCluster::new_split().expect("cluster bootstrap failed");
//!         handle
//!             .register_shutdown_on_exit()
//!             .expect("shutdown hook registration failed");
//!         std::mem::forget(guard);
//!         handle
//!     })
//! }
//! ```

use postgresql_embedded::Settings;

use super::{
    connection::TestClusterConnection,
    lifecycle::DatabaseName,
    temporary_database::TemporaryDatabase,
};
use crate::{TestBootstrapEnvironment, TestBootstrapSettings, error::BootstrapResult};

/// Send-safe handle providing read-only access to a running `PostgreSQL` cluster.
///
/// Handles are lightweight and cloneable. They contain only the bootstrap
/// metadata needed to construct connections and query cluster state.
///
/// # Thread Safety
///
/// `ClusterHandle` implements [`Send`] and [`Sync`], making it safe to share
/// across threads. The underlying `PostgreSQL` process is an external OS process
/// that handles concurrent connections safely.
///
/// # Obtaining a Handle
///
/// Use [`TestCluster::new_split()`](super::TestCluster::new_split) to obtain
/// a handle and guard pair:
///
/// ```no_run
/// use pg_embedded_setup_unpriv::TestCluster;
///
/// let (handle, guard) = TestCluster::new_split()?;
/// // handle: ClusterHandle (Send + Sync)
/// // guard: ClusterGuard (!Send, manages lifecycle)
/// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
/// ```
#[derive(Debug, Clone)]
pub struct ClusterHandle {
    bootstrap: TestBootstrapSettings,
}

// Compile-time assertions that ClusterHandle is Send + Sync.
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_sync<T: Sync>() {}
    assert_send::<ClusterHandle>();
    assert_sync::<ClusterHandle>();
};

impl From<TestBootstrapSettings> for ClusterHandle {
    fn from(bootstrap: TestBootstrapSettings) -> Self { Self { bootstrap } }
}

impl ClusterHandle {
    /// Registers a process-exit hook that stops the `PostgreSQL` postmaster
    /// when the process terminates.
    ///
    /// Intended for shared clusters where the [`ClusterGuard`](super::ClusterGuard)
    /// is intentionally forgotten. The hook requests platform shutdown and
    /// waits up to the configured shutdown timeout before escalating to the
    /// platform's forceful termination mechanism.
    ///
    /// The method is idempotent: subsequent calls after the first
    /// successful registration are no-ops. Only one cluster can be
    /// tracked per process, matching the one-shared-cluster pattern.
    ///
    /// # Platform Support
    ///
    /// Supported on Unix (Linux, macOS) and Windows. Windows shutdown requests
    /// terminate the postmaster process tree immediately because the process
    /// exit hook cannot safely issue a graceful `PostgreSQL` shutdown command. On
    /// other platforms this method is a silent no-op that returns `Ok(())`, so
    /// callers need not gate on platform cfgs.
    ///
    /// # Errors
    ///
    /// Returns an error if `libc::atexit` registration fails on a supported
    /// platform.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::sync::OnceLock;
    ///
    /// use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
    ///
    /// static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
    ///
    /// fn shared_handle() -> &'static ClusterHandle {
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
    pub fn register_shutdown_on_exit(&self) -> BootstrapResult<()> {
        self.register_shutdown_on_exit_impl()
    }

    #[cfg(any(unix, windows))]
    fn register_shutdown_on_exit_impl(&self) -> BootstrapResult<()> {
        super::shutdown_hook::register_shutdown_hook(
            self.bootstrap.settings.clone(),
            self.bootstrap.shutdown_timeout,
            self.bootstrap.cleanup_mode,
        )
    }

    #[cfg(not(any(unix, windows)))]
    fn register_shutdown_on_exit_impl(&self) -> BootstrapResult<()> {
        // No-op on unsupported platforms. Unix and Windows both have concrete
        // process-exit reapers; other targets can still use normal Drop-based
        // cleanup.
        Ok(())
    }
}

// Process-exit shutdown hook registration.
impl ClusterHandle {
    /// Registers a process-exit hook that stops the `PostgreSQL` postmaster
    /// when the process terminates.
    ///
    /// Intended for shared clusters where the [`ClusterGuard`](super::ClusterGuard)
    /// is intentionally forgotten. The hook requests platform shutdown and
    /// waits up to the configured shutdown timeout before escalating to the
    /// platform's forceful termination mechanism.
    ///
    /// The method is idempotent: subsequent calls after the first
    /// successful registration are no-ops. Only one cluster can be
    /// tracked per process, matching the one-shared-cluster pattern.
    ///
    /// # Platform Support
    ///
    /// Supported on Unix (Linux, macOS) and Windows. Windows shutdown requests
    /// terminate the postmaster process tree immediately because the process
    /// exit hook cannot safely issue a graceful `PostgreSQL` shutdown command. On
    /// other platforms this method is a silent no-op that returns `Ok(())`, so
    /// callers need not gate on platform cfgs.
    ///
    /// # Errors
    ///
    /// Returns an error if `libc::atexit` registration fails on a supported
    /// platform.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::sync::OnceLock;
    ///
    /// use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
    ///
    /// static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
    ///
    /// fn shared_handle() -> &'static ClusterHandle {
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
    pub fn register_shutdown_on_exit(&self) -> BootstrapResult<()> {
        self.register_shutdown_on_exit_impl()
    }

    #[cfg(any(unix, windows))]
    fn register_shutdown_on_exit_impl(&self) -> BootstrapResult<()> {
        super::shutdown_hook::register_shutdown_hook(
            self.bootstrap.settings.clone(),
            self.bootstrap.shutdown_timeout,
            self.bootstrap.cleanup_mode,
        )
    }

    #[cfg(not(any(unix, windows)))]
    fn register_shutdown_on_exit_impl(&self) -> BootstrapResult<()> {
        // No-op on unsupported platforms. Unix and Windows both have concrete
        // process-exit reapers; other targets can still use normal Drop-based
        // cleanup.
        Ok(())
    }
}

// Process-exit shutdown hook registration.
impl ClusterHandle {
    /// Registers a process-exit hook that stops the `PostgreSQL` postmaster
    /// when the process terminates.
    ///
    /// Intended for shared clusters where the [`ClusterGuard`](super::ClusterGuard)
    /// is intentionally forgotten. The hook requests platform shutdown and
    /// waits up to the configured shutdown timeout before escalating to the
    /// platform's forceful termination mechanism.
    ///
    /// The method is idempotent: subsequent calls after the first
    /// successful registration are no-ops. Only one cluster can be
    /// tracked per process, matching the one-shared-cluster pattern.
    ///
    /// # Platform Support
    ///
    /// Supported on Unix (Linux, macOS) and Windows. Windows shutdown requests
    /// terminate the postmaster process tree immediately because the process
    /// exit hook cannot safely issue a graceful `PostgreSQL` shutdown command. On
    /// other platforms this method is a silent no-op that returns `Ok(())`, so
    /// callers need not gate on platform cfgs.
    ///
    /// # Errors
    ///
    /// Returns an error if `libc::atexit` registration fails on a supported
    /// platform.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::sync::OnceLock;
    ///
    /// use pg_embedded_setup_unpriv::{ClusterHandle, TestCluster};
    ///
    /// static SHARED: OnceLock<ClusterHandle> = OnceLock::new();
    ///
    /// fn shared_handle() -> &'static ClusterHandle {
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
    pub fn register_shutdown_on_exit(&self) -> BootstrapResult<()> {
        self.register_shutdown_on_exit_impl()
    }

    #[cfg(any(unix, windows))]
    fn register_shutdown_on_exit_impl(&self) -> BootstrapResult<()> {
        super::shutdown_hook::register_shutdown_hook(
            self.bootstrap.settings.clone(),
            self.bootstrap.shutdown_timeout,
            self.bootstrap.cleanup_mode,
        )
    }

    #[cfg(not(any(unix, windows)))]
    fn register_shutdown_on_exit_impl(&self) -> BootstrapResult<()> {
        // No-op on unsupported platforms. Unix and Windows both have concrete
        // process-exit reapers; other targets can still use normal Drop-based
        // cleanup.
        Ok(())
    }
}

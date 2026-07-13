//! Shared cluster singleton implementations.
//!
//! This module provides thread-safe singleton patterns for shared test clusters.
//! The clusters are initialised lazily on first access and persist for the entire
//! process lifetime.

use std::sync::{Arc, Mutex, OnceLock};

use super::{
    fixtures::ensure_worker_env,
    shared_singleton_core::{SharedInitState, get_or_try_init_shared},
};
use crate::{
    ClusterHandle,
    TestCluster,
    error::{BootstrapError, BootstrapResult},
};

// ============================================================================
// Shared cluster handle singleton
// ============================================================================

/// Global state for the shared cluster handle singleton.
///
/// Uses `OnceLock<Mutex<...>>` to support fallible initialisation whilst
/// maintaining thread-safe singleton semantics. The `Mutex` protects
/// initialisation; once complete, the pointer is stable.
///
/// The handle is leaked to obtain a `'static` reference for the entire
/// process lifetime.
static SHARED_CLUSTER_HANDLE: OnceLock<Mutex<SharedHandleState>> = OnceLock::new();
type SharedHandleState = SharedInitState<&'static ClusterHandle, Arc<BootstrapError>>;
/// Returns a reference to the shared cluster handle.
///
/// The cluster is initialised lazily on first access using [`OnceLock`] for
/// thread-safe singleton semantics. Subsequent calls return the same handle
/// instance, eliminating per-test bootstrap overhead.
///
/// This function returns a [`ClusterHandle`] which is `Send + Sync`, making it
/// suitable for use in contexts requiring thread safety (e.g., rstest fixtures
/// with timeouts).
///
/// # Errors
///
/// Returns a [`BootstrapError`] if the cluster cannot be started. Once
/// initialisation fails, subsequent calls return an error with the same
/// [`BootstrapErrorKind`](crate::error::BootstrapErrorKind) and a message
/// indicating the previous failure.
///
/// # Thread Safety
///
/// This function is safe to call from multiple threads concurrently. The first
/// caller to reach the initialisation path will bootstrap the cluster while
/// other callers wait.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::test_support::shared_cluster_handle;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let handle = shared_cluster_handle()?;
/// assert!(handle.database_exists("postgres")?);
///
/// // Second call returns the same instance
/// let handle2 = shared_cluster_handle()?;
/// assert!(std::ptr::eq(handle, handle2));
/// # Ok(())
/// # }
/// ```
pub fn shared_cluster_handle() -> BootstrapResult<&'static ClusterHandle> {
    let mutex = SHARED_CLUSTER_HANDLE.get_or_init(|| Mutex::new(SharedInitState::Uninitialised));
    let mut guard = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    get_or_try_init_shared(
        &mut guard,
        initialise_shared_cluster_handle,
        cached_shared_handle_error,
    )
}

fn initialise_shared_cluster_handle()
-> Result<&'static ClusterHandle, (Arc<BootstrapError>, BootstrapError)> {
    let worker_guard = ensure_worker_env();
    match TestCluster::new_split() {
        Ok((handle, cluster_guard)) => {
            // Attach worker guard to cluster guard, then leak it.
            // The guard manages shutdown; leaking it means the cluster
            // runs for the process lifetime.
            log_shared_cluster_event("shared cluster handle initialized");
            let guarded = cluster_guard.with_worker_guard(worker_guard);
            leak_shared_handle_after_shutdown_hook(
                handle,
                guarded,
                ClusterHandle::register_shutdown_on_exit,
            )
        }
        Err(err) => {
            log_shared_cluster_error(&err, "shared cluster handle initialization failed");
            // Store error info for subsequent callers to retrieve.
            let stored = Arc::new(BootstrapError::new(
                err.kind(),
                color_eyre::eyre::eyre!("bootstrap failed: {:?}", err),
            ));
            // Return the original error with full diagnostics.
            Err((stored, err))
        }
    }
}

fn cached_shared_handle_error(original_err: &Arc<BootstrapError>) -> BootstrapError {
    log_cached_shared_handle_error(original_err);
    let report = color_eyre::eyre::eyre!(
        "shared cluster initialisation previously failed: {:?}",
        original_err
    );
    BootstrapError::new(original_err.kind(), report)
}

fn leak_shared_handle_after_shutdown_hook<Guard, Register>(
    handle: ClusterHandle,
    guarded: Guard,
    register_shutdown_hook: Register,
) -> Result<&'static ClusterHandle, (Arc<BootstrapError>, BootstrapError)>
where
    Register: FnOnce(&ClusterHandle) -> BootstrapResult<()>,
{
    if let Err(err) = register_shutdown_hook(&handle) {
        log_shared_cluster_error(
            &err,
            "shared cluster shutdown hook registration failed; dropping guard",
        );
        let stored = Arc::new(BootstrapError::new(
            err.kind(),
            color_eyre::eyre::eyre!("shutdown hook registration failed: {:?}", err),
        ));
        return Err((stored, err));
    }

    // Leak the guard only after the process-exit hook is armed. If hook
    // registration fails, `guarded` drops here and stops the cluster.
    std::mem::forget(guarded);

    log_shared_cluster_event("shared cluster shutdown hook registered; leaking handle");
    Ok(Box::leak(Box::new(handle)))
}

fn log_shared_cluster_event(message: &'static str) {
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        "{message}"
    );
}

fn log_shared_cluster_error(err: &BootstrapError, message: &'static str) {
    tracing::error!(
        target: crate::observability::LOG_TARGET,
        error = ?err,
        "{message}"
    );
}

fn log_cached_shared_handle_error(original_err: &Arc<BootstrapError>) {
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        error = ?original_err,
        "replaying cached shared cluster initialization failure"
    );
}

// ============================================================================
// Legacy shared cluster singleton (for backward compatibility)
// ============================================================================

/// Global state for the legacy shared cluster singleton.
///
/// Uses `OnceLock<Mutex<...>>` to support fallible initialisation whilst
/// maintaining thread-safe singleton semantics.
static SHARED_CLUSTER: OnceLock<Mutex<SharedClusterState>> = OnceLock::new();

type SharedClusterState = SharedInitState<SharedClusterPtr, Arc<BootstrapError>>;

/// Wrapper around raw pointer to `TestCluster` that implements `Send + Sync`.
///
/// Required because `TestCluster` is `!Send` (contains `ScopedEnv` with
/// `PhantomData<Rc<()>>`). The pointer is safe to share across threads because:
/// 1. The cluster is only initialised once and never moved.
/// 2. All access goes through immutable references.
/// 3. The cluster's public API is thread-safe (database operations use independent connections).
#[derive(Clone, Copy)]
struct SharedClusterPtr(*const TestCluster);

// SAFETY: SharedClusterPtr upholds the following invariants:
// 1. The pointer targets a `Box::leak`ed allocation that outlives all references.
// 2. No mutable access occurs through this pointer; all usage is via `&TestCluster`.
// 3. `TestCluster` methods internally handle synchronization (each database operation creates an
//    independent connection).
unsafe impl Send for SharedClusterPtr {}
unsafe impl Sync for SharedClusterPtr {}

/// Returns a reference to the shared test cluster.
///
/// The cluster is initialised lazily on first access using [`OnceLock`] for
/// thread-safe singleton semantics. Subsequent calls return the same cluster
/// instance, eliminating per-test bootstrap overhead.
///
/// # Recommendation
///
/// Prefer [`shared_cluster_handle()`] for new code. It returns a `Send + Sync`
/// handle suitable for rstest fixtures with timeouts and other thread-safe
/// contexts. This function is retained for backward compatibility with
/// existing tests that depend on the `TestCluster` type.
///
/// # Errors
///
/// Returns a [`BootstrapError`] if the cluster cannot be started. Once
/// initialisation fails, subsequent calls return an error with the same
/// [`BootstrapErrorKind`](crate::error::BootstrapErrorKind) and a message
/// indicating the previous failure.
///
/// # Thread Safety
///
/// This function is safe to call from multiple threads concurrently. The first
/// caller to reach the initialisation path will bootstrap the cluster while
/// other callers wait. All callers receive the same cluster reference.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::test_support::shared_cluster;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cluster = shared_cluster()?;
/// assert!(cluster.database_exists("postgres")?);
///
/// // Second call returns the same instance
/// let cluster2 = shared_cluster()?;
/// assert!(std::ptr::eq(cluster, cluster2));
/// # Ok(())
/// # }
/// ```
pub fn shared_cluster() -> BootstrapResult<&'static TestCluster> {
    let mutex = SHARED_CLUSTER.get_or_init(|| Mutex::new(SharedClusterState::Uninitialised));
    let mut guard = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let ptr = get_or_try_init_shared(
        &mut guard,
        initialise_shared_cluster,
        cached_shared_handle_error,
    )?;

    // SAFETY: The pointer was created from Box::leak and is valid forever.
    Ok(unsafe { &*ptr.0 })
}

fn initialise_shared_cluster() -> Result<SharedClusterPtr, (Arc<BootstrapError>, BootstrapError)> {
    let worker_guard = ensure_worker_env();
    match TestCluster::new() {
        Ok(new_cluster) => {
            log_shared_cluster_event("shared cluster initialized");
            leak_shared_cluster_after_shutdown_hook(
                new_cluster.with_worker_guard(worker_guard),
                |cluster| cluster.handle.register_shutdown_on_exit(),
            )
            .map(SharedClusterPtr)
        }
        Err(err) => {
            log_shared_cluster_error(&err, "shared cluster initialization failed");
            let stored = Arc::new(BootstrapError::new(
                err.kind(),
                color_eyre::eyre::eyre!("bootstrap failed: {:?}", err),
            ));
            Err((stored, err))
        }
    }
}

fn leak_shared_cluster_after_shutdown_hook<Cluster, Register>(
    cluster: Cluster,
    register_shutdown_hook: Register,
) -> Result<*const Cluster, (Arc<BootstrapError>, BootstrapError)>
where
    Register: FnOnce(&Cluster) -> BootstrapResult<()>,
{
    if let Err(err) = register_shutdown_hook(&cluster) {
        log_shared_cluster_error(
            &err,
            "shared cluster shutdown hook registration failed; dropping cluster",
        );
        let stored = Arc::new(BootstrapError::new(
            err.kind(),
            color_eyre::eyre::eyre!("shutdown hook registration failed: {:?}", err),
        ));
        return Err((stored, err));
    }

    let leaked = Box::leak(Box::new(cluster));
    log_shared_cluster_event("shared cluster shutdown hook registered; leaking cluster");
    Ok(std::ptr::from_ref(leaked))
}

#[cfg(test)]
mod tests {
    //! Tests for shared singleton lifecycle helpers.

    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;
    use crate::{ExecutionPrivileges, test_support::dummy_settings};

    struct DropProbe(Arc<AtomicBool>);

    impl Drop for DropProbe {
        fn drop(&mut self) { self.0.store(true, Ordering::SeqCst); }
    }

    #[test]
    fn registration_failure_drops_guard_instead_of_leaking_shared_handle() {
        let was_dropped = Arc::new(AtomicBool::new(false));
        let handle = ClusterHandle::from(dummy_settings(ExecutionPrivileges::Unprivileged));
        let guarded = DropProbe(Arc::clone(&was_dropped));

        let result = leak_shared_handle_after_shutdown_hook(handle, guarded, |_| {
            Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "injected shutdown hook failure"
            )))
        });

        assert!(
            result.is_err(),
            "shared-handle initialisation should fail when shutdown hook registration fails"
        );
        assert!(
            was_dropped.load(Ordering::SeqCst),
            concat!(
                "guard must drop on registration failure so postmaster.pid is removed ",
                "and no orphaned PostgreSQL process remains"
            )
        );
    }

    #[test]
    fn registration_failure_drops_guard_instead_of_leaking_shared_cluster() {
        let was_dropped = Arc::new(AtomicBool::new(false));
        let cluster = DropProbe(Arc::clone(&was_dropped));

        let result = leak_shared_cluster_after_shutdown_hook(cluster, |_| {
            Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "injected shutdown hook failure"
            )))
        });

        assert!(
            result.is_err(),
            "shared-cluster initialisation should fail when shutdown hook registration fails"
        );
        assert!(
            was_dropped.load(Ordering::SeqCst),
            "cluster must drop when shutdown hook registration fails"
        );
    }
}

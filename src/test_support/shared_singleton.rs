//! Shared cluster singleton implementations.
//!
//! This module provides thread-safe singleton patterns for shared test clusters.
//! The clusters are initialized lazily on first access and persist for the entire
//! process lifetime.

use std::sync::{Arc, Mutex, OnceLock};

use super::{
    bootstrap_retry::{RetryPolicy, retry_transient},
    fixtures::ensure_worker_env,
    shared_config::{SharedOrigin, resolve_origin},
    shared_singleton_core::{SharedInitState, get_or_try_init_shared},
};
use crate::{
    ClusterGuard,
    ClusterHandle,
    TestBootstrapConfig,
    TestCluster,
    env::ScopedEnv,
    error::{BootstrapError, BootstrapResult},
};

// ============================================================================
// Shared cluster handle singleton
// ============================================================================

/// Global state for the shared cluster handle singleton.
///
/// Uses `OnceLock<Mutex<...>>` to support fallible initialization whilst
/// maintaining thread-safe singleton semantics. The `Mutex` protects
/// initialization; once complete, the pointer is stable.
///
/// The handle is leaked to obtain a `'static` reference for the entire
/// process lifetime.
static SHARED_CLUSTER_HANDLE: OnceLock<Mutex<SharedHandleState>> = OnceLock::new();

/// Cached handle state plus the configuration that produced it.
struct SharedHandleState {
    state: SharedInitState<&'static ClusterHandle, Arc<BootstrapError>>,
    origin: Option<SharedOrigin>,
}

impl SharedHandleState {
    const fn uninitialized() -> Self {
        Self {
            state: SharedInitState::Uninitialized,
            origin: None,
        }
    }
}
/// Returns a reference to the shared cluster handle.
///
/// The cluster is initialized lazily on first access using [`OnceLock`] for
/// thread-safe singleton semantics. Subsequent calls return the same handle
/// instance, eliminating per-test bootstrap overhead.
///
/// This function returns a [`ClusterHandle`] which is `Send + Sync`, making it
/// suitable for use in contexts requiring thread safety (e.g., rstest fixtures
/// with timeouts).
///
/// # Errors
///
/// Returns a [`BootstrapError`] if the cluster cannot be started. A transient
/// failure (a download or extraction I/O error, a lifecycle timeout, or an
/// extension archive that could not be fetched) is retried up to three
/// attempts in all, one then two seconds apart, and reports the attempt count
/// if it persists. Any other failure is returned at once. Once
/// initialization fails, subsequent calls return an error with the same
/// [`BootstrapErrorKind`](crate::error::BootstrapErrorKind) and a message
/// indicating the previous failure.
///
/// # Thread Safety
///
/// This function is safe to call from multiple threads concurrently. The first
/// caller to reach the initialization path will bootstrap the cluster while
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
    shared_cluster_handle_with_origin(SharedOrigin::Ambient)
}

/// Returns a reference to the shared cluster handle built from `config`.
///
/// The shared cluster is a process-wide singleton, so the first caller decides
/// which configuration it runs with. An explicit call reuses the cached result
/// only when the singleton was initialized with an equal
/// [`TestBootstrapConfig`]; otherwise it returns a configuration error that
/// names the conflict without printing configuration values. Ambient callers
/// of [`shared_cluster_handle`] always reuse the cached result.
///
/// # Errors
///
/// Returns a [`BootstrapError`] when the cluster cannot be started, when a
/// previous initialization failure is replayed, or when the requested
/// configuration conflicts with the singleton's origin.
///
/// # Examples
///
/// ```no_run
/// use pg_embedded_setup_unpriv::{
///     PgEnvCfg,
///     TestBootstrapConfig,
///     test_support::shared_cluster_handle_with,
/// };
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let config = TestBootstrapConfig::new(PgEnvCfg::default());
/// let handle = shared_cluster_handle_with(config)?;
/// assert!(handle.database_exists("postgres")?);
/// # Ok(())
/// # }
/// ```
pub fn shared_cluster_handle_with(
    config: impl Into<TestBootstrapConfig>,
) -> BootstrapResult<&'static ClusterHandle> {
    shared_cluster_handle_with_origin(SharedOrigin::Explicit(Box::new(config.into())))
}

fn shared_cluster_handle_with_origin(
    requested: SharedOrigin,
) -> BootstrapResult<&'static ClusterHandle> {
    let mutex =
        SHARED_CLUSTER_HANDLE.get_or_init(|| Mutex::new(SharedHandleState::uninitialized()));
    let mut guard = mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    resolve_origin(&mut guard.origin, &requested)?;

    match requested {
        SharedOrigin::Ambient => get_or_try_init_shared(
            &mut guard.state,
            initialize_shared_cluster_handle,
            cached_shared_handle_error,
        ),
        SharedOrigin::Explicit(config) => get_or_try_init_shared(
            &mut guard.state,
            move || initialize_shared_cluster_handle_with(&config),
            cached_shared_handle_error,
        ),
    }
}

fn initialize_shared_cluster_handle()
-> Result<&'static ClusterHandle, (Arc<BootstrapError>, BootstrapError)> {
    let worker_guard = ensure_worker_env();
    finish_shared_handle(
        worker_guard,
        retry_transient(
            RetryPolicy::SHARED_CLUSTER,
            TestCluster::new_split,
            std::thread::sleep,
        ),
    )
}

fn initialize_shared_cluster_handle_with(
    config: &TestBootstrapConfig,
) -> Result<&'static ClusterHandle, (Arc<BootstrapError>, BootstrapError)> {
    // The explicit path supplies its own worker binary (or relies on PATH
    // discovery), so it must not read or mutate `PG_EMBEDDED_WORKER`.
    finish_shared_handle(
        None,
        retry_transient(
            RetryPolicy::SHARED_CLUSTER,
            || TestCluster::new_split_with(config.clone()),
            std::thread::sleep,
        ),
    )
}

fn finish_shared_handle(
    worker_guard: Option<ScopedEnv>,
    outcome: BootstrapResult<(ClusterHandle, ClusterGuard)>,
) -> Result<&'static ClusterHandle, (Arc<BootstrapError>, BootstrapError)> {
    match outcome {
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

pub(super) fn cached_shared_handle_error(original_err: &Arc<BootstrapError>) -> BootstrapError {
    log_cached_shared_handle_error(original_err);
    let report = color_eyre::eyre::eyre!(
        "shared cluster initialization previously failed: {:?}",
        original_err
    );
    BootstrapError::new(original_err.kind(), report)
}

pub(super) fn register_shutdown_hook_or_fail<T, Register>(
    target: &T,
    register_shutdown_hook: Register,
    failure_message: &'static str,
) -> Result<(), (Arc<BootstrapError>, BootstrapError)>
where
    Register: FnOnce(&T) -> BootstrapResult<()>,
{
    if let Err(err) = register_shutdown_hook(target) {
        log_shared_cluster_error(&err, failure_message);
        let stored = Arc::new(BootstrapError::new(
            err.kind(),
            color_eyre::eyre::eyre!("shutdown hook registration failed: {:?}", err),
        ));
        return Err((stored, err));
    }

    Ok(())
}

fn leak_shared_handle_after_shutdown_hook<Guard, Register>(
    handle: ClusterHandle,
    guarded: Guard,
    register_shutdown_hook: Register,
) -> Result<&'static ClusterHandle, (Arc<BootstrapError>, BootstrapError)>
where
    Register: FnOnce(&ClusterHandle) -> BootstrapResult<()>,
{
    register_shutdown_hook_or_fail(
        &handle,
        register_shutdown_hook,
        "shared cluster shutdown hook registration failed; dropping guard",
    )?;

    // Leak the guard only after the process-exit hook is armed. If hook
    // registration fails, `guarded` drops here and stops the cluster.
    std::mem::forget(guarded);

    log_shared_cluster_event("shared cluster shutdown hook registered; leaking handle");
    Ok(Box::leak(Box::new(handle)))
}

pub(super) fn log_shared_cluster_event(message: &'static str) {
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        "{message}"
    );
}

pub(super) fn log_shared_cluster_error(err: &BootstrapError, message: &'static str) {
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
            "shared-handle initialization should fail when shutdown hook registration fails"
        );
        assert!(
            was_dropped.load(Ordering::SeqCst),
            concat!(
                "guard must drop on registration failure so postmaster.pid is removed ",
                "and no orphaned PostgreSQL process remains"
            )
        );
    }
}

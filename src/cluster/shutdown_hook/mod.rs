//! Process-exit hook that stops the `PostgreSQL` postmaster on `atexit`.
//!
//! Shared test clusters use [`std::mem::forget`] on the [`ClusterGuard`](super::ClusterGuard)
//! to keep the cluster alive for the process lifetime. This prevents `Drop`
//! from running, leaving the postmaster orphaned after the test binary exits.
//!
//! This module provides [`register_shutdown_hook`], which stores cluster
//! metadata in a [`Mutex`] and registers an `extern "C"` callback via
//! [`libc::atexit`]. When the process exits, the callback reads the
//! postmaster PID from disk, asks the platform to stop it, polls for exit, and
//! escalates to forceful termination if the timeout elapses.

use std::{
    path::Path,
    sync::{Condvar, Mutex, TryLockError},
    time::Duration,
};

use platform::{
    force_shutdown,
    parse_postmaster_process,
    postmaster_process_is_running as platform_postmaster_process_is_running,
    prepare_process_exit_failsafe,
    request_shutdown,
};
use postgresql_embedded::Settings;

use crate::{CleanupMode, error::BootstrapResult};

mod platform;

/// State captured at registration time and read by the atexit callback.
struct RegisteredShutdownState {
    settings: Settings,
    shutdown_timeout: Duration,
    cleanup_mode: CleanupMode,
    _exit_failsafe: platform::ProcessExitFailsafe,
}

/// Registration state for the singleton process-exit hook.
enum ShutdownRegistrationState {
    Empty,
    Registering,
    Registered(Box<RegisteredShutdownState>),
}

/// Initialisation guard for the atexit callback.
///
/// Uses an explicit state machine rather than `OnceLock` so registration can
/// be rolled back if preflight or `libc::atexit` registration fails, avoiding a
/// poisoned state where subsequent calls silently no-op.
static SHUTDOWN_STATE: Mutex<ShutdownRegistrationState> =
    Mutex::new(ShutdownRegistrationState::Empty);
static SHUTDOWN_STATE_CHANGED: Condvar = Condvar::new();

/// Polling interval when waiting for the postmaster to exit after SIGTERM.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Grace period after SIGKILL before proceeding to cleanup.
const POST_SIGKILL_GRACE: Duration = Duration::from_millis(100);

/// Platform-specific process identifier stored in `postmaster.pid`.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub type PostmasterPid = platform::PostmasterPid;

/// PID-reuse-safe platform-specific postmaster identity.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub type PostmasterProcess = platform::PostmasterProcess;

/// Registers an atexit hook that will stop the `PostgreSQL` postmaster on
/// process exit.
///
/// The hook is registered at most once per process. Subsequent calls are
/// idempotent no-ops and return `Ok(())`.
///
/// # Errors
///
/// Returns an error if `libc::atexit` reports failure (non-zero return).
pub(super) fn register_shutdown_hook(
    settings: Settings,
    shutdown_timeout: Duration,
    cleanup_mode: CleanupMode,
) -> BootstrapResult<()> {
    if !reserve_shutdown_registration() {
        return Ok(());
    }

    let state = match build_shutdown_state(settings, shutdown_timeout, cleanup_mode) {
        Ok(state) => state,
        Err(err) => {
            rollback_shutdown_registration();
            return Err(err);
        }
    };

    commit_shutdown_registration(state);
    log_registration_success();
    Ok(())
}

/// Claims the singleton registration slot before external work starts.
fn reserve_shutdown_registration() -> bool {
    let mut guard = SHUTDOWN_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    loop {
        match &*guard {
            ShutdownRegistrationState::Empty => {
                *guard = ShutdownRegistrationState::Registering;
                return true;
            }
            ShutdownRegistrationState::Registered(_) => {
                log_already_registered();
                return false;
            }
            ShutdownRegistrationState::Registering => {
                guard = SHUTDOWN_STATE_CHANGED
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        }
    }
}

/// Builds shutdown state without holding the global registration mutex.
fn build_shutdown_state(
    settings: Settings,
    shutdown_timeout: Duration,
    cleanup_mode: CleanupMode,
) -> BootstrapResult<RegisteredShutdownState> {
    let postmaster_process = read_postmaster_process_from_dir(&settings.data_dir)?;
    register_atexit()?;
    let exit_failsafe = prepare_process_exit_failsafe(postmaster_process);

    Ok(RegisteredShutdownState {
        settings,
        shutdown_timeout,
        cleanup_mode,
        _exit_failsafe: exit_failsafe,
    })
}

/// Stores registered state after all fallible registration work succeeds.
fn commit_shutdown_registration(state: RegisteredShutdownState) {
    let mut guard = SHUTDOWN_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = ShutdownRegistrationState::Registered(Box::new(state));
    SHUTDOWN_STATE_CHANGED.notify_all();
}

/// Releases the registration slot after preflight or atexit registration fails.
fn rollback_shutdown_registration() {
    let mut guard = SHUTDOWN_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = ShutdownRegistrationState::Empty;
    SHUTDOWN_STATE_CHANGED.notify_all();
}

#[cfg(all(test, feature = "cluster-unit-tests"))]
fn reset_shutdown_registration_for_tests() {
    let mut guard = SHUTDOWN_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = ShutdownRegistrationState::Empty;
    SHUTDOWN_STATE_CHANGED.notify_all();
}

/// Logs that a duplicate registration was skipped.
fn log_already_registered() {
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        "shutdown hook already registered; skipping duplicate registration"
    );
}

/// Logs a successful hook registration.
fn log_registration_success() {
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        "registered atexit shutdown hook for PostgreSQL postmaster"
    );
}

/// Calls `libc::atexit` to register the shutdown callback.
fn register_atexit() -> BootstrapResult<()> {
    // SAFETY: `shutdown_callback` is an `extern "C"` function with no parameters
    // and no return value, matching the signature required by `atexit(3)`.
    // The function accesses only the `SHUTDOWN_STATE` static which is
    // initialised above and remains valid for the lifetime of the process.
    let rc = unsafe { libc::atexit(shutdown_callback) };
    if rc != 0 {
        return Err(color_eyre::eyre::eyre!("libc::atexit registration failed (rc={rc})").into());
    }
    Ok(())
}

/// Callback invoked by the C runtime during process exit.
///
/// Reads the postmaster PID from disk, asks the platform to terminate it, and
/// escalates to forceful termination if the configured timeout expires.
extern "C" fn shutdown_callback() {
    let Some(work) = shutdown_work() else {
        return;
    };

    let process = match read_postmaster_process_from_dir(&work.settings.data_dir) {
        Ok(Some(process)) => process,
        Ok(None) => {
            // PID file missing — cluster already stopped or was never started.
            best_effort_cleanup(&work);
            return;
        }
        Err(err) => {
            tracing::warn!(
                target: crate::observability::LOG_TARGET,
                %err,
                "failed to read postmaster process during shutdown callback"
            );
            return;
        }
    };

    if !postmaster_process_is_running_for_shutdown(process) {
        best_effort_cleanup(&work);
        return;
    }

    stop_postmaster(process, &work);
    best_effort_cleanup(&work);
}

/// Returns callback work without holding the global shutdown-state mutex.
fn shutdown_work() -> Option<ShutdownWork> {
    let guard = match SHUTDOWN_STATE.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(poisoned)) => {
            tracing::warn!(
                target: crate::observability::LOG_TARGET,
                "shutdown state mutex was poisoned; continuing shutdown cleanup"
            );
            poisoned.into_inner()
        }
        Err(TryLockError::WouldBlock) => {
            // Avoid blocking (or deadlocking) inside an atexit handler.
            return None;
        }
    };

    let ShutdownRegistrationState::Registered(state) = &*guard else {
        return None;
    };

    Some(ShutdownWork {
        settings: state.settings.clone(),
        shutdown_timeout: state.shutdown_timeout,
        cleanup_mode: state.cleanup_mode,
    })
}

/// Data needed by the atexit callback after releasing the global mutex.
struct ShutdownWork {
    settings: Settings,
    shutdown_timeout: Duration,
    cleanup_mode: CleanupMode,
}

/// Requests postmaster shutdown and escalates on timeout.
fn stop_postmaster(process: platform::PostmasterProcess, state: &ShutdownWork) {
    request_shutdown(process);

    if wait_for_exit(process, state.shutdown_timeout) {
        return;
    }

    // Timeout expired — escalate to forceful platform termination.
    tracing::warn!(
        target: crate::observability::LOG_TARGET,
        timeout_ms = state.shutdown_timeout.as_millis(),
        "postmaster did not exit before shutdown-hook timeout; escalating to forceful termination"
    );
    force_shutdown(process);
    std::thread::sleep(POST_SIGKILL_GRACE);
}

/// Polls until the process exits or the timeout elapses.
///
/// Returns `true` if the process exited within the timeout.
fn wait_for_exit(process: platform::PostmasterProcess, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if !postmaster_process_is_running_for_shutdown(process) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn postmaster_process_is_running_for_shutdown(process: platform::PostmasterProcess) -> bool {
    platform_postmaster_process_is_running(process).unwrap_or_else(|err| {
        tracing::warn!(
            target: crate::observability::LOG_TARGET,
            %err,
            "failed to probe postmaster process during shutdown"
        );
        true
    })
}

// ---------------------------------------------------------------------------
// PID file and process helpers
// ---------------------------------------------------------------------------

/// Reads the postmaster PID from `data_dir/postmaster.pid`.
///
/// Returns `Ok(None)` if the file is missing or empty.
///
/// # Errors
///
/// Returns an error if the PID file cannot be read or contains a malformed PID.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn read_postmaster_pid(data_dir: &Path) -> BootstrapResult<Option<PostmasterPid>> {
    let pid_file = data_dir.join("postmaster.pid");
    let contents = read_postmaster_pid_file(&pid_file)?;
    let Some(first_line) = contents.lines().next() else {
        return Ok(None);
    };
    if first_line.trim().is_empty() {
        return Ok(None);
    }
    platform::parse_pid(first_line).map(Some).ok_or_else(|| {
        color_eyre::eyre::eyre!("malformed postmaster PID in {}", pid_file.display()).into()
    })
}

/// Reads the PID-reuse-safe postmaster process identity from `postmaster.pid`.
///
/// Returns `Ok(None)` if the file is missing or empty.
///
/// # Errors
///
/// Returns an error if the PID file cannot be read or contains a malformed
/// platform-specific process identity.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn read_postmaster_process(data_dir: &Path) -> BootstrapResult<Option<PostmasterProcess>> {
    read_postmaster_process_from_dir(data_dir)
}

fn read_postmaster_process_from_dir(
    data_dir: &Path,
) -> BootstrapResult<Option<platform::PostmasterProcess>> {
    let pid_file = data_dir.join("postmaster.pid");
    let contents = read_postmaster_pid_file(&pid_file)?;
    if contents.trim().is_empty() {
        return Ok(None);
    }
    parse_postmaster_process(&contents)
        .map(Some)
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("malformed postmaster process in {}", pid_file.display()).into()
        })
}

fn read_postmaster_pid_file(pid_file: &Path) -> BootstrapResult<String> {
    match std::fs::read_to_string(pid_file) {
        Ok(contents) => Ok(contents),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => {
            Err(color_eyre::eyre::eyre!("failed to read {}: {err}", pid_file.display()).into())
        }
    }
}

/// Returns `true` if a process with the given PID is currently running.
///
/// Invalid PIDs are rejected immediately by the platform implementation.
///
/// # Errors
///
/// Returns an error when the platform process probe fails for a reason other
/// than the process being absent.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn process_is_running(pid: PostmasterPid) -> BootstrapResult<bool> {
    platform::process_is_running_for_platform(pid)
}

/// Returns `true` when the postmaster identity still matches a live process.
///
/// This helper preserves the shutdown hook's PID-reuse-safe process identity
/// check for integration tests.
///
/// # Errors
///
/// Returns an error when the platform process probe fails for a reason other
/// than the process being absent.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn postmaster_process_is_running(process: PostmasterProcess) -> BootstrapResult<bool> {
    platform_postmaster_process_is_running(process)
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

/// Best-effort directory cleanup after the postmaster has stopped.
fn best_effort_cleanup(state: &ShutdownWork) {
    super::cleanup::cleanup_in_process(state.cleanup_mode, &state.settings, "atexit-shutdown-hook");
}

#[cfg(all(test, feature = "cluster-unit-tests"))]
mod tests;

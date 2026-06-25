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

use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use crate::CleanupMode;
use crate::error::BootstrapResult;
use platform::{force_shutdown, parse_pid, process_is_running_for_platform, request_shutdown};
use postgresql_embedded::Settings;

mod platform;

/// State captured at registration time and read by the atexit callback.
struct ShutdownState {
    settings: Settings,
    shutdown_timeout: Duration,
    cleanup_mode: CleanupMode,
}

/// Initialisation guard for the atexit callback.
///
/// Uses `Mutex<Option<...>>` rather than `OnceLock` so that state can be
/// rolled back if `libc::atexit` registration fails, avoiding a poisoned
/// state where subsequent calls silently no-op.
static SHUTDOWN_STATE: Mutex<Option<ShutdownState>> = Mutex::new(None);

/// Polling interval when waiting for the postmaster to exit after SIGTERM.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Grace period after SIGKILL before proceeding to cleanup.
const POST_SIGKILL_GRACE: Duration = Duration::from_millis(100);

/// Platform-specific process identifier stored in `postmaster.pid`.
pub type PostmasterPid = platform::PostmasterPid;

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
    let mut guard = SHUTDOWN_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    if guard.is_some() {
        log_already_registered();
        return Ok(());
    }

    register_atexit()?;

    // Store state only AFTER atexit succeeds, so a failed registration
    // does not poison the slot for future attempts.
    *guard = Some(ShutdownState {
        settings,
        shutdown_timeout,
        cleanup_mode,
    });

    log_registration_success();
    Ok(())
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
    let Ok(guard) = SHUTDOWN_STATE.try_lock() else {
        // Mutex is poisoned or held by another thread — bail to avoid
        // blocking (or deadlocking) inside an atexit handler.
        return;
    };

    let Some(state) = guard.as_ref() else {
        return;
    };

    let Some(pid) = read_postmaster_pid(&state.settings.data_dir) else {
        // PID file missing — cluster already stopped or was never started.
        best_effort_cleanup(state);
        return;
    };

    if !process_is_running(pid) {
        best_effort_cleanup(state);
        return;
    }

    stop_postmaster(pid, state);
    best_effort_cleanup(state);
}

/// Requests postmaster shutdown and escalates on timeout.
fn stop_postmaster(pid: PostmasterPid, state: &ShutdownState) {
    request_shutdown(pid);

    if wait_for_exit(pid, state.shutdown_timeout) {
        return;
    }

    // Timeout expired — escalate to forceful platform termination.
    force_shutdown(pid);
    std::thread::sleep(POST_SIGKILL_GRACE);
}

/// Polls until the process exits or the timeout elapses.
///
/// Returns `true` if the process exited within the timeout.
fn wait_for_exit(pid: PostmasterPid, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if !process_is_running(pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

// ---------------------------------------------------------------------------
// PID file and process helpers
// ---------------------------------------------------------------------------

/// Reads the postmaster PID from `data_dir/postmaster.pid`.
///
/// Returns `None` if the file is missing, empty, cannot be parsed, or
/// contains a non-positive value.
#[must_use]
pub fn read_postmaster_pid(data_dir: &Path) -> Option<PostmasterPid> {
    let pid_file = data_dir.join("postmaster.pid");
    let contents = std::fs::read_to_string(&pid_file).ok()?;
    let first_line = contents.lines().next()?;
    parse_pid(first_line)
}

/// Returns `true` if a process with the given PID is currently running.
///
/// Invalid PIDs are rejected immediately by the platform implementation.
#[must_use]
pub fn process_is_running(pid: PostmasterPid) -> bool {
    process_is_running_for_platform(pid)
}

// ---------------------------------------------------------------------------
// Cleanup
// ---------------------------------------------------------------------------

/// Best-effort directory cleanup after the postmaster has stopped.
fn best_effort_cleanup(state: &ShutdownState) {
    super::cleanup::cleanup_in_process(state.cleanup_mode, &state.settings, "atexit-shutdown-hook");
}

#[cfg(all(test, feature = "cluster-unit-tests"))]
mod tests {
    use super::*;

    use color_eyre::eyre::{Result, ensure};
    use rstest::{fixture, rstest};
    use tempfile::TempDir;

    /// Creates a fresh temporary directory for PID file tests.
    #[fixture]
    fn pid_dir() -> Result<TempDir> {
        Ok(tempfile::tempdir()?)
    }

    #[rstest]
    #[case::valid_file(Some("12345\nother\nlines\n"), Some(12345))]
    #[case::missing_file(None, None)]
    #[case::empty_file(Some(""), None)]
    #[case::zero_pid(Some("0\n"), None)]
    #[case::negative_pid(Some("-1\n"), None)]
    fn read_postmaster_pid_parses_first_line(
        pid_dir: Result<TempDir>,
        #[case] file_content: Option<&str>,
        #[case] expected: Option<PostmasterPid>,
    ) -> Result<()> {
        let dir = pid_dir?;
        if let Some(content) = file_content {
            std::fs::write(dir.path().join("postmaster.pid"), content)?;
        }

        let result = read_postmaster_pid(dir.path());

        ensure!(result == expected, "expected {expected:?}, got {result:?}");
        Ok(())
    }

    #[test]
    fn process_is_running_returns_true_for_current_process() -> Result<()> {
        let pid = PostmasterPid::try_from(std::process::id())?;

        ensure!(process_is_running(pid), "current process should be running");
        Ok(())
    }

    #[test]
    fn process_is_running_returns_false_for_nonexistent_pid() -> Result<()> {
        // PID i32::MAX is extremely unlikely to be in use.
        ensure!(
            !process_is_running(PostmasterPid::MAX),
            "nonexistent PID should not be running"
        );
        Ok(())
    }

    #[test]
    fn process_is_running_rejects_zero_pid() -> Result<()> {
        let pid = 0;

        ensure!(
            !process_is_running(pid),
            "zero PID should not be considered running"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_is_running_rejects_negative_pid() -> Result<()> {
        let pid = -1;

        ensure!(
            !process_is_running(pid),
            "negative PID should not be considered running"
        );
        Ok(())
    }
}

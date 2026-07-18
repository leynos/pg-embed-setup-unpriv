//! Tests for the process-exit hook that do not require a live postmaster.
//!
//! The cases that touch the singleton `SHUTDOWN_STATE` share the
//! `shutdown_hook_state` serial key and reset the registration to `Empty`,
//! so they stay isolated even under `cargo test` (which, unlike `nextest`,
//! runs the whole binary in one process). The reset also neutralises the
//! real `atexit` handler that registration installs, since the callback
//! finds no work.
use std::time::Duration;

use postgresql_embedded::Settings;
use serial_test::serial;

use super::*;
use crate::{CleanupMode, test_support::capture_warn_logs};

/// Resets the shutdown-hook singleton on scope exit.
///
/// Registration mutates the process-wide `SHUTDOWN_STATE`; without this guard an
/// early return (via `?` or a failed `ensure!`) would leave the state
/// `Registered` and leak into later tests and the real `atexit` callback.
struct ResetRegistrationGuard;

impl Drop for ResetRegistrationGuard {
    fn drop(&mut self) { reset_shutdown_registration_for_tests(); }
}

#[test]
fn absent_process_reports_not_running() {
    assert!(
        !postmaster_process_is_running_for_shutdown(0),
        "a non-positive pid must not be reported as running"
    );
}

#[test]
fn wait_for_exit_returns_true_for_absent_process() {
    assert!(
        wait_for_exit(0, Duration::from_millis(20)),
        "an already-absent process should be treated as exited"
    );
}

#[test]
#[serial(shutdown_hook_state)]
fn shutdown_work_is_none_before_registration() {
    reset_shutdown_registration_for_tests();
    assert!(
        shutdown_work().is_none(),
        "no work should be available before registration"
    );
}

#[test]
fn stop_postmaster_returns_for_absent_process() {
    let work = ShutdownWork {
        settings: Settings::default(),
        shutdown_timeout: Duration::from_millis(20),
        cleanup_mode: CleanupMode::None,
    };
    // The process is absent, so shutdown returns without forceful escalation.
    let (logs, ()) = capture_warn_logs(|| stop_postmaster(0, &work));
    assert!(
        !logs
            .iter()
            .any(|line| line.contains("escalating to forceful termination")),
        "an absent process must not trigger forceful escalation, got {logs:?}"
    );
}

#[test]
#[serial(shutdown_hook_state)]
fn register_shutdown_hook_is_idempotent() -> color_eyre::Result<()> {
    reset_shutdown_registration_for_tests();
    // Guarantee the singleton is cleared even if an assertion below returns
    // early, so the leaked `Registered` state cannot reach later tests or the
    // real `atexit` callback.
    let _reset_guard = ResetRegistrationGuard;
    let first_dir = tempfile::tempdir()?;
    let second_dir = tempfile::tempdir()?;
    let first = Settings {
        data_dir: first_dir.path().to_path_buf(),
        ..Settings::default()
    };
    let second = Settings {
        data_dir: second_dir.path().to_path_buf(),
        ..Settings::default()
    };

    register_shutdown_hook(first.clone(), Duration::from_millis(20), CleanupMode::None)?;
    // A second registration with distinct settings must be a no-op: first wins.
    register_shutdown_hook(second, Duration::from_millis(999), CleanupMode::Full)?;

    let work =
        shutdown_work().ok_or_else(|| color_eyre::eyre::eyre!("registration should be present"))?;
    color_eyre::eyre::ensure!(
        work.settings == first,
        "the second registration must not overwrite the first"
    );
    color_eyre::eyre::ensure!(
        work.shutdown_timeout == Duration::from_millis(20),
        "the first shutdown timeout must be preserved"
    );
    color_eyre::eyre::ensure!(
        work.cleanup_mode == CleanupMode::None,
        "the first cleanup mode must be preserved"
    );

    // `_reset_guard` clears the singleton on drop so later cases (and the
    // installed atexit callback) observe no registration.
    Ok(())
}

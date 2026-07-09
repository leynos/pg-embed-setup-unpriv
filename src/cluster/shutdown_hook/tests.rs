//! Unit tests for shutdown-hook PID parsing and singleton registration.

use color_eyre::eyre::{Result, ensure};
use proptest::prelude::*;
use rstest::{fixture, rstest};
use serial_test::serial;
use tempfile::TempDir;

use super::*;

/// Creates a fresh temporary directory for PID file tests.
#[fixture]
fn pid_dir() -> Result<TempDir> {
    let dir = tempfile::tempdir()?;
    Ok(dir)
}

#[rstest]
#[case::valid_file(Some("12345\nother\nlines\n"), Some(12345))]
#[case::missing_file(None, None)]
#[case::empty_file(Some(""), None)]
fn read_postmaster_pid_parses_first_line(
    pid_dir: Result<TempDir>,
    #[case] file_content: Option<&str>,
    #[case] expected: Option<PostmasterPid>,
) -> Result<()> {
    let dir = pid_dir?;
    if let Some(content) = file_content {
        std::fs::write(dir.path().join("postmaster.pid"), content)?;
    }

    let result = read_postmaster_pid(dir.path())?;

    ensure!(result == expected, "expected {expected:?}, got {result:?}");
    Ok(())
}

#[rstest]
#[case::zero_pid("0\n")]
#[case::negative_pid("-1\n")]
#[case::non_numeric_pid("not-a-pid\n")]
fn read_postmaster_pid_reports_malformed_file(
    pid_dir: Result<TempDir>,
    #[case] file_content: &str,
) -> Result<()> {
    let dir = pid_dir?;
    std::fs::write(dir.path().join("postmaster.pid"), file_content)?;

    let result = read_postmaster_pid(dir.path());

    ensure!(result.is_err(), "expected malformed PID to error");
    Ok(())
}

#[test]
fn process_is_running_returns_true_for_current_process() -> Result<()> {
    let pid = PostmasterPid::try_from(std::process::id())?;

    ensure!(
        process_is_running(pid)?,
        "current process should be running"
    );
    Ok(())
}

#[test]
fn process_is_running_returns_false_for_nonexistent_pid() -> Result<()> {
    // PID i32::MAX is extremely unlikely to be in use.
    ensure!(
        !process_is_running(PostmasterPid::MAX)?,
        "nonexistent PID should not be running"
    );
    Ok(())
}

#[test]
fn process_is_running_rejects_zero_pid() -> Result<()> {
    let pid = 0;

    ensure!(
        !process_is_running(pid)?,
        "zero PID should not be considered running"
    );
    Ok(())
}

#[test]
#[serial(shutdown_hook_state)]
fn register_shutdown_hook_rolls_back_after_preflight_failure() -> Result<()> {
    reset_shutdown_registration_for_tests();
    let failing_data_dir = tempfile::tempdir()?;
    std::fs::write(
        failing_data_dir.path().join("postmaster.pid"),
        "not-a-pid\n",
    )?;
    let mut settings = Settings {
        data_dir: failing_data_dir.path().into(),
        ..Settings::default()
    };

    let failed = register_shutdown_hook(
        settings.clone(),
        Duration::from_millis(1),
        CleanupMode::None,
    );

    ensure!(failed.is_err(), "preflight failure should be reported");
    ensure!(
        matches!(
            *SHUTDOWN_STATE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ShutdownRegistrationState::Empty
        ),
        "failed registration should roll back singleton state"
    );

    let data_dir = tempfile::tempdir()?;
    settings.data_dir = data_dir.path().into();
    register_shutdown_hook(settings, Duration::from_millis(1), CleanupMode::None)?;
    reset_shutdown_registration_for_tests();
    Ok(())
}

#[test]
#[serial(shutdown_hook_state)]
fn concurrent_register_shutdown_hook_calls_share_singleton() -> Result<()> {
    reset_shutdown_registration_for_tests();
    let data_dir = tempfile::tempdir()?;
    let settings = Settings {
        data_dir: data_dir.path().into(),
        ..Settings::default()
    };

    std::thread::scope(|scope| {
        let handles = (0..4)
            .map(|_| {
                let thread_settings = settings.clone();
                scope.spawn(move || {
                    register_shutdown_hook(
                        thread_settings,
                        Duration::from_millis(1),
                        CleanupMode::None,
                    )
                })
            })
            .collect::<Vec<_>>();

        for handle in handles {
            match handle.join() {
                Ok(result) => result?,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        }
        Ok::<(), crate::error::BootstrapError>(())
    })?;

    ensure!(
        matches!(
            *SHUTDOWN_STATE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            ShutdownRegistrationState::Registered(_)
        ),
        "concurrent registration should leave one registered state"
    );
    reset_shutdown_registration_for_tests();
    Ok(())
}

proptest! {
    #[test]
    fn parse_pid_accepts_only_positive_values(pid in 1_u32..=i32::MAX as u32) {
        let expected = PostmasterPid::try_from(pid).expect("positive PID should fit");
        let parsed = platform::parse_pid(&pid.to_string());
        prop_assert_eq!(parsed, Some(expected));
    }

    #[cfg(unix)]
    #[test]
    fn parse_pid_rejects_non_positive_values(pid in i32::MIN..=0) {
        let parsed = platform::parse_pid(&pid.to_string());
        prop_assert_eq!(parsed, None);
    }

    #[test]
    fn parse_pid_rejects_arbitrary_non_numeric_text(text in "\\PC*") {
        prop_assume!(text.trim().parse::<PostmasterPid>().is_err());
        prop_assert_eq!(platform::parse_pid(&text), None);
    }
}

#[cfg(unix)]
#[test]
fn process_is_running_rejects_negative_pid() -> Result<()> {
    let pid = -1;

    ensure!(
        !process_is_running(pid)?,
        "negative PID should not be considered running"
    );
    Ok(())
}

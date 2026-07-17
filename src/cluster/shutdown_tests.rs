//! Tests for cluster shutdown behaviour.
use rstest::rstest;

use super::*;
use crate::test_support::capture_warn_logs;

#[rstest]
#[case::timeout(
    || warn_stop_timeout(5, "ctx"),
    "stop() timed out after 5s (ctx)"
)]
#[case::failure(
    || warn_stop_failure("ctx", &"boom"),
    "failed to stop embedded postgres instance"
)]
#[case::async_drop(
    || warn_async_drop_without_stop("ctx"),
    "dropped without calling stop_async()"
)]
#[case::no_runtime(
    || error_no_runtime_for_cleanup("ctx"),
    "no async runtime available for cleanup"
)]
fn warning_functions_emit_expected_logs(#[case] action: fn(), #[case] expected_substring: &str) {
    let (logs, ()) = capture_warn_logs(action);
    assert!(
        logs.iter().any(|line| line.contains(expected_substring)),
        "expected log containing '{expected_substring}', got {logs:?}"
    );
}

#[test]
fn stop_context_reports_version_and_data_dir() {
    let settings = Settings::default();
    let context = stop_context(&settings);
    assert!(
        context.contains("version") && context.contains("data_dir"),
        "expected version and data_dir in context, got: {context}"
    );
}

mod worker_managed {
    //! Worker-managed drop tests that intercept privileged operations with a
    //! test hook, so no real `pg_worker` binary or root privileges are needed.
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use serial_test::serial;

    use super::super::*;
    use crate::{
        CleanupMode,
        ExecutionPrivileges,
        test_support::{dummy_settings, install_run_root_operation_hook, test_runtime},
    };

    #[test]
    #[serial(worker_hook)]
    fn stop_worker_managed_reports_hook_failure() -> color_eyre::Result<()> {
        let runtime = test_runtime()?;
        let bootstrap = dummy_settings(ExecutionPrivileges::Root);
        let env_vars = bootstrap.environment.to_env();
        let _guard = install_run_root_operation_hook(|_, _, _| {
            Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                "hook stop failure"
            )))
        })
        .map_err(|err| color_eyre::eyre::eyre!(err))?;

        let result = stop_worker_managed_with_runtime(&runtime, &bootstrap, &env_vars, "stop-test");
        color_eyre::eyre::ensure!(result.is_err(), "hook failure should propagate");
        Ok(())
    }

    #[test]
    #[serial(worker_hook)]
    fn drop_sync_cluster_worker_managed_invokes_stop_hook() -> color_eyre::Result<()> {
        let runtime = test_runtime()?;
        let mut bootstrap = dummy_settings(ExecutionPrivileges::Root);
        // Disable the follow-up cleanup operation so only the single stop
        // invocation reaches the hook; this pins the drop path to exactly one
        // stop call rather than tolerating duplicates.
        bootstrap.cleanup_mode = CleanupMode::None;
        let env_vars = bootstrap.environment.to_env();
        let calls = Arc::new(AtomicUsize::new(0));
        let hook_calls = Arc::clone(&calls);
        let _guard = install_run_root_operation_hook(move |_, _, _| {
            hook_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .map_err(|err| color_eyre::eyre::eyre!(err))?;

        let mut postgres = None;
        drop_sync_cluster(
            &runtime,
            DropContext {
                is_managed_via_worker: true,
                postgres: &mut postgres,
                bootstrap: &bootstrap,
                env_vars: &env_vars,
                context: "drop-test",
            },
        );

        let count = calls.load(Ordering::Relaxed);
        color_eyre::eyre::ensure!(
            count == 1,
            "worker-managed drop must invoke the stop hook exactly once, got {count}"
        );
        Ok(())
    }

    #[test]
    fn drop_sync_cluster_in_process_without_handle_is_noop() {
        let runtime = test_runtime().expect("runtime");
        let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
        let env_vars = bootstrap.environment.to_env();
        let mut postgres = None;
        // No postgres handle and not worker-managed: shutdown is a no-op.
        drop_sync_cluster(
            &runtime,
            DropContext {
                is_managed_via_worker: false,
                postgres: &mut postgres,
                bootstrap: &bootstrap,
                env_vars: &env_vars,
                context: "noop-test",
            },
        );
    }
}

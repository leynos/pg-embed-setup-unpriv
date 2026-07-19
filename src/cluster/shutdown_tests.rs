//! Tests for cluster shutdown behaviour.
use postgresql_embedded::VersionReq;
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
    // Use distinctive values so the assertion fails if either is omitted,
    // reordered, or mislabelled, unlike a bare substring check.
    let settings = Settings {
        version: VersionReq::parse("=17.0.0").expect("valid version requirement"),
        data_dir: std::path::PathBuf::from("/tmp/pg-ctx-test"),
        ..Settings::default()
    };
    let context = stop_context(&settings);
    assert_eq!(context, "version =17.0.0, data_dir /tmp/pg-ctx-test");
}

mod worker_managed {
    //! Worker-managed drop tests that intercept privileged operations with a
    //! test hook, so no real `pg_worker` binary or root privileges are needed.
    use std::{
        fs,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };

    use serial_test::serial;
    use tempfile::tempdir;

    use super::super::*;
    use crate::{
        CleanupMode,
        ExecutionPrivileges,
        cluster::WorkerOperation,
        test_support::{dummy_settings, install_run_root_operation_hook, test_runtime},
    };

    #[test]
    #[serial(worker_hook)]
    fn stop_worker_managed_reports_hook_failure() -> color_eyre::Result<()> {
        let runtime = test_runtime()?;
        let bootstrap = dummy_settings(ExecutionPrivileges::Root);
        let env_vars = bootstrap.environment.to_env();
        let _guard = install_run_root_operation_hook(|_, _, operation| {
            // Only the expected `Stop` operation yields the sentinel message;
            // any other operation produces a distinct message so the assertion
            // below fails if the drop/stop path dispatches the wrong operation.
            let message = if matches!(operation, WorkerOperation::Stop) {
                "hook stop failure"
            } else {
                "unexpected worker operation"
            };
            Err(crate::error::BootstrapError::from(color_eyre::eyre::eyre!(
                "{message}"
            )))
        })
        .map_err(|err| color_eyre::eyre::eyre!(err))?;

        let result = stop_worker_managed_with_runtime(&runtime, &bootstrap, &env_vars, "stop-test");
        let err = result
            .err()
            .ok_or_else(|| color_eyre::eyre::eyre!("hook failure should propagate"))?;
        // Assert on the hook's Stop-only sentinel so the test proves the
        // installed hook was exercised with `WorkerOperation::Stop`, not merely
        // that some unrelated failure occurred.
        color_eyre::eyre::ensure!(
            err.to_string().contains("hook stop failure"),
            "expected the installed hook's Stop error to propagate, got: {err}"
        );
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
        let stop_calls = Arc::new(AtomicUsize::new(0));
        let hook_calls = Arc::clone(&calls);
        let hook_stop_calls = Arc::clone(&stop_calls);
        let _guard = install_run_root_operation_hook(move |_, _, operation| {
            hook_calls.fetch_add(1, Ordering::Relaxed);
            // Track stop dispatches separately so the assertions below prove the
            // single invocation was `WorkerOperation::Stop`, not some other root
            // operation dispatched by mistake.
            if matches!(operation, WorkerOperation::Stop) {
                hook_stop_calls.fetch_add(1, Ordering::Relaxed);
            }
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
        let stop_count = stop_calls.load(Ordering::Relaxed);
        color_eyre::eyre::ensure!(
            stop_count == 1,
            "the single invocation must dispatch WorkerOperation::Stop, got {stop_count} stop \
             call(s) of {count}"
        );
        Ok(())
    }

    #[test]
    #[serial(worker_hook)]
    fn drop_sync_cluster_in_process_without_handle_is_noop() -> color_eyre::Result<()> {
        let runtime = test_runtime()?;
        let mut bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
        // Point the data directory at a real path and arm full cleanup so that
        // an accidental in-process cleanup on the no-op path would delete it.
        let sandbox = tempdir()?;
        let data_dir = sandbox.path().join("data");
        fs::create_dir_all(&data_dir)?;
        fs::write(data_dir.join("marker"), b"data")?;
        bootstrap.settings.data_dir = data_dir.clone();
        bootstrap.cleanup_mode = CleanupMode::Full;
        let env_vars = bootstrap.environment.to_env();
        // Count privileged-operation invocations to prove the no-op path never
        // reaches the worker stop/cleanup hook.
        let calls = Arc::new(AtomicUsize::new(0));
        let hook_calls = Arc::clone(&calls);
        let _guard = install_run_root_operation_hook(move |_, _, _| {
            hook_calls.fetch_add(1, Ordering::Relaxed);
            Ok(())
        })
        .map_err(|err| color_eyre::eyre::eyre!(err))?;

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

        let count = calls.load(Ordering::Relaxed);
        color_eyre::eyre::ensure!(
            count == 0,
            "the no-op drop path must not invoke the root-operation hook, got {count}"
        );
        // The in-process cleanup boundary must not have run: the data directory
        // (and its marker) must survive the no-op drop.
        color_eyre::eyre::ensure!(
            data_dir.join("marker").exists(),
            "the no-op drop path must not perform in-process cleanup"
        );
        Ok(())
    }
}

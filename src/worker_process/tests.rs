//! Behavioural tests for the worker subprocess runner.
//!
//! These drive [`run`] against stand-in worker binaries (shell scripts)
//! while privilege dropping is disabled, so the orchestration, timeout, and
//! exit-handling branches are covered without root or a real worker.

use std::{os::unix::fs::PermissionsExt as _, time::Duration};

use camino::Utf8Path;
use color_eyre::eyre::ensure;
use postgresql_embedded::Settings;
use serial_test::serial;

use super::*;
use crate::cluster::WorkerOperation;

fn write_script(dir: &Utf8Path, body: &str) -> BootstrapResult<camino::Utf8PathBuf> {
    let script = dir.join("fake_worker");
    std::fs::write(script.as_std_path(), body)
        .map_err(|err| BootstrapError::from(eyre!("write worker script: {err}")))?;
    let mut perms = std::fs::metadata(script.as_std_path())
        .map_err(|err| BootstrapError::from(eyre!("stat worker script: {err}")))?
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(script.as_std_path(), perms)
        .map_err(|err| BootstrapError::from(eyre!("chmod worker script: {err}")))?;
    Ok(script)
}

fn run_with_script(body: &str, timeout: Duration) -> BootstrapResult<()> {
    let temp =
        tempfile::tempdir().map_err(|err| BootstrapError::from(eyre!("create temp dir: {err}")))?;
    let dir = Utf8Path::from_path(temp.path())
        .ok_or_else(|| BootstrapError::from(eyre!("temp dir path is not UTF-8")))?;
    let worker = write_script(dir, body)?;
    let settings = Settings::default();
    let env_vars: Vec<(String, Option<String>)> = Vec::new();
    // Disable privilege dropping so the payload is not chowned to `nobody`,
    // which would require root.
    let _guard = disable_privilege_drop_for_tests();
    let args = WorkerRequestArgs {
        worker: &worker,
        settings: &settings,
        env_vars: &env_vars,
        operation: WorkerOperation::Setup,
        timeout,
    };
    run(&WorkerRequest::new(args))
}

// The privilege-drop toggle (`SKIP_PRIVILEGE_DROP`) is process-global, so every
// test that flips it shares the `privilege_drop_toggle` serial key to avoid
// racing the counter assertions in `worker_process::privileges`'s own tests.

#[test]
#[serial(privilege_drop_toggle)]
fn run_succeeds_when_worker_exits_zero() -> BootstrapResult<()> {
    run_with_script("#!/bin/sh\nexit 0\n", Duration::from_secs(30))
}

#[test]
#[serial(privilege_drop_toggle)]
fn run_surfaces_failure_output_when_worker_exits_nonzero() -> color_eyre::Result<()> {
    let err = run_with_script(
        "#!/bin/sh\necho boom-on-stderr 1>&2\nexit 3\n",
        Duration::from_secs(30),
    )
    .err()
    .ok_or_else(|| eyre!("non-zero exit should error"))?;
    ensure!(
        err.to_string().contains("boom-on-stderr"),
        "expected captured stderr in error, got: {err}"
    );
    Ok(())
}

#[test]
#[serial(privilege_drop_toggle)]
fn run_times_out_when_worker_runs_too_long() -> color_eyre::Result<()> {
    // The worker sleeps briefly; the timeout fires well before it exits.
    // Keep the sleep short because the killed shell's child keeps the
    // captured output pipe open until it exits, bounding the test runtime.
    let err = run_with_script("#!/bin/sh\nsleep 2\n", Duration::from_millis(150))
        .err()
        .ok_or_else(|| eyre!("slow worker should time out"))?;
    ensure!(
        err.to_string().contains("timed out"),
        "expected timeout error, got: {err}"
    );
    Ok(())
}

#[test]
#[serial(privilege_drop_toggle)]
fn run_errors_when_worker_binary_is_missing() -> color_eyre::Result<()> {
    let temp = tempfile::tempdir()?;
    let dir =
        Utf8Path::from_path(temp.path()).ok_or_else(|| eyre!("temp dir path is not UTF-8"))?;
    let worker = dir.join("does-not-exist");
    let settings = Settings::default();
    let env_vars: Vec<(String, Option<String>)> = Vec::new();
    let _guard = disable_privilege_drop_for_tests();
    let args = WorkerRequestArgs {
        worker: &worker,
        settings: &settings,
        env_vars: &env_vars,
        operation: WorkerOperation::Setup,
        timeout: Duration::from_secs(5),
    };
    let err = run(&WorkerRequest::new(args))
        .err()
        .ok_or_else(|| eyre!("missing binary should error"))?;
    ensure!(
        err.to_string().contains("spawn worker command"),
        "expected spawn error, got: {err}"
    );
    Ok(())
}

//! End-to-end lifecycle test for the process-exit shutdown hook.
//!
//! Verifies that `PostgreSQL` processes do not survive test binary exit when
//! the shutdown hook is registered. Uses a subprocess pattern: the parent
//! spawns itself as a child process which creates a cluster, registers the
//! hook, writes the postmaster PID to a temp file, then calls
//! `std::process::exit(0)`. The parent waits for the child to exit and then
//! confirms the postmaster has also terminated.
#![cfg(any(unix, windows))]

#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use std::{env, fs, path::Path, thread, time::Duration};

use cluster_skip::cluster_skip_message;
use color_eyre::eyre::{Context, Result, eyre};
use pg_embedded_setup_unpriv::test_support::read_postmaster_process;
use rstest::rstest;
use serial::{ScenarioSerialGuard, serial_guard};

#[cfg(unix)]
type OsPid = libc::pid_t;

#[cfg(windows)]
type OsPid = u32;

/// Environment variable used to signal that this binary is running as the
/// child subprocess.
const CHILD_ENV_KEY: &str = "SHUTDOWN_HOOK_LIFECYCLE_CHILD";

/// Maximum time to wait for the postmaster to exit after the child process
/// terminates.
const POSTMASTER_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Polling interval when waiting for the postmaster to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

// ============================================================================
// Parent (test harness)
// ============================================================================

/// Spawns a child process that creates a cluster with the shutdown hook,
/// then verifies the postmaster is stopped after the child exits.
#[rstest]
fn postmaster_exits_after_child_process_with_shutdown_hook(
    serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    let _guard = serial_guard;
    let tmp_dir = tempfile::tempdir().context("create temp dir")?;
    let pid_file = tmp_dir.path().join("postmaster.pid");

    let child_status = spawn_child(&pid_file)?;

    if !child_status.success() {
        return Err(eyre!("child process exited with status {child_status}"));
    }

    // The child writes either a postmaster identity file or "SKIP" to the temp file.
    // "SKIP" signals that the environment cannot support cluster creation
    // (e.g. missing PostgreSQL binaries).
    let content = fs::read_to_string(&pid_file).context("read PID file from child")?;
    if content.trim() == "SKIP" {
        tracing::warn!("SKIP: child could not create a cluster in this environment");
        return Ok(());
    }

    let _postmaster_process = read_postmaster_process(tmp_dir.path())?
        .ok_or_else(|| eyre!("postmaster process identity not found after child exit"))?;
    let postmaster_pid = read_postmaster_pid_from_identity_file(&content)?;
    wait_for_postmaster_exit(postmaster_pid)
}

/// Spawns the child subprocess that creates and forgets a cluster.
fn spawn_child(pid_file: &Path) -> Result<std::process::ExitStatus> {
    let exe = env::current_exe().context("resolve current exe")?;
    let pid_path = pid_file
        .to_str()
        .ok_or_else(|| eyre!("non-UTF-8 temp path"))?;
    std::process::Command::new(exe)
        .env(CHILD_ENV_KEY, pid_path)
        .arg("--ignored")
        .arg("shutdown_hook_lifecycle_child_entry")
        .status()
        .context("spawn child process")
}

fn read_postmaster_pid_from_identity_file(contents: &str) -> Result<OsPid> {
    let first_line = contents
        .lines()
        .next()
        .ok_or_else(|| eyre!("postmaster identity file was empty"))?;
    first_line
        .trim()
        .parse::<OsPid>()
        .with_context(|| format!("parse postmaster PID from '{first_line}'"))
}
fn wait_for_postmaster_exit(postmaster_pid: OsPid) -> Result<()> {
    let deadline = std::time::Instant::now() + POSTMASTER_EXIT_TIMEOUT;
    loop {
        if !os_process_is_running(postmaster_pid) {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(eyre!(
                "postmaster process did not exit within {POSTMASTER_EXIT_TIMEOUT:?}"
            ));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn os_process_is_running(pid: OsPid) -> bool {
    // SAFETY: `kill(pid, 0)` probes process existence without sending a signal.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(code) if code == libc::ESRCH
    )
}

#[cfg(windows)]
fn os_process_is_running(pid: OsPid) -> bool {
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const ERROR_INVALID_PARAMETER: u32 = 87;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
        fn GetLastError() -> u32;
    }

    use std::ffi::c_void;

    // SAFETY: `OpenProcess` receives a concrete PID copied from
    // `postmaster.pid`, and handle inheritance is disabled.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // SAFETY: reads the thread's last OS error after `OpenProcess`.
        let code = unsafe { GetLastError() };
        return code != ERROR_INVALID_PARAMETER;
    }
    // SAFETY: the non-null handle was returned by `OpenProcess` above.
    unsafe {
        CloseHandle(handle);
    }
    true
}

/// Returns `true` if the error should cause a soft skip rather than a hard
/// failure.
fn should_skip(message: &str, debug: &str) -> bool {
    cluster_skip_message(message, Some(debug)).is_some()
        || debug.contains("another server might be running")
}

// ============================================================================
// Child (subprocess entry point)
// ============================================================================

/// Entry point for the child subprocess.
///
/// This function is invoked when the binary detects the `CHILD_ENV_KEY`
/// environment variable. It creates a cluster, registers the shutdown hook,
/// writes the postmaster PID, and exits.
///
/// When the environment cannot support cluster creation (e.g. missing
/// `PostgreSQL` binaries), the child writes "SKIP" to the PID file and
/// exits cleanly so the parent can detect the soft skip.
#[test]
#[ignore = "child subprocess entry point - not a standalone test"]
fn shutdown_hook_lifecycle_child_entry() -> Result<()> {
    let Ok(pid_file_path) = env::var(CHILD_ENV_KEY) else {
        // Not running as the child subprocess — skip silently.
        return Ok(());
    };

    let (handle, guard) = match pg_embedded_setup_unpriv::TestCluster::new_split() {
        Ok(pair) => pair,
        Err(err) => {
            let message = err.to_string();
            let debug = format!("{err:?}");
            if should_skip(&message, &debug) {
                // Signal soft skip to the parent process by writing "SKIP"
                // and exiting cleanly, so the parent does not treat this as
                // a hard failure.
                let _unused = fs::write(&pid_file_path, "SKIP");
                std::process::exit(0);
            }
            return Err(err).context("create cluster in child");
        }
    };

    handle
        .register_shutdown_on_exit()
        .context("register shutdown hook")?;

    // Write postmaster PID to the temp file for the parent to verify.
    let source_pid_file = handle.settings().data_dir.join("postmaster.pid");
    fs::copy(&source_pid_file, &pid_file_path).context("write postmaster identity file")?;

    // Forget the guard so Drop doesn't shut down the cluster — the atexit
    // hook is responsible.
    std::mem::forget(guard);

    // exit(0) triggers atexit handlers.
    std::process::exit(0);
}

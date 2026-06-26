//! Platform-specific process controls for the shutdown hook.

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub(super) use self::windows::{
    PostmasterProcess, ProcessExitFailsafe, force_shutdown, parse_postmaster_process,
    postmaster_process_is_running, prepare_process_exit_failsafe, request_shutdown,
};

#[cfg(all(
    windows,
    any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker")
))]
pub(super) use self::windows::{PostmasterPid, parse_pid, process_is_running_for_platform};

/// Platform-specific process identifier stored in `postmaster.pid`.
#[cfg(unix)]
pub(super) type PostmasterPid = libc::pid_t;

/// Platform-specific process identity stored in `postmaster.pid`.
#[cfg(unix)]
pub(super) type PostmasterProcess = PostmasterPid;

/// No-op Unix failsafe retained for a uniform shutdown state shape.
#[cfg(unix)]
pub(super) struct ProcessExitFailsafe;

/// Parses a strictly positive postmaster PID.
#[cfg(unix)]
pub(super) fn parse_pid(raw: &str) -> Option<PostmasterPid> {
    let pid = raw.trim().parse::<PostmasterPid>().ok()?;
    (pid > 0).then_some(pid)
}

/// Parses the platform-specific postmaster process identity.
#[cfg(unix)]
pub(super) fn parse_postmaster_process(contents: &str) -> Option<PostmasterProcess> {
    let first_line = contents.lines().next()?;
    parse_pid(first_line)
}

/// Retains platform resources that should survive until process exit.
#[cfg(unix)]
pub(super) const fn prepare_process_exit_failsafe(
    _process: Option<PostmasterProcess>,
) -> ProcessExitFailsafe {
    ProcessExitFailsafe
}

/// Requests graceful `PostgreSQL` shutdown.
#[cfg(unix)]
pub(super) fn request_shutdown(process: PostmasterProcess) {
    send_signal(process, libc::SIGTERM);
}

/// Forces `PostgreSQL` shutdown after the graceful timeout.
#[cfg(unix)]
pub(super) fn force_shutdown(process: PostmasterProcess) {
    send_signal(process, libc::SIGKILL);
}

/// Returns `true` when the process exists.
#[cfg(unix)]
pub(super) fn process_is_running_for_platform(pid: PostmasterPid) -> bool {
    if pid <= 0 {
        return false;
    }

    // SAFETY: `kill` with signal `0` probes whether the process exists
    // without sending a signal. `pid` is positive, avoiding process-group
    // semantics.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }

    !matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(code) if code == libc::ESRCH
    )
}

/// Returns `true` when the postmaster identity still matches a live process.
#[cfg(unix)]
pub(super) fn postmaster_process_is_running(process: PostmasterProcess) -> bool {
    process_is_running_for_platform(process)
}

#[cfg(unix)]
fn send_signal(pid: PostmasterPid, signal: libc::c_int) {
    if pid <= 0 {
        return;
    }

    // SAFETY: `pid` is a positive process identifier and `signal` is one of
    // the libc signal constants used by this module. Errors are ignored
    // because shutdown is best effort inside an atexit handler.
    unsafe {
        libc::kill(pid, signal);
    }
}

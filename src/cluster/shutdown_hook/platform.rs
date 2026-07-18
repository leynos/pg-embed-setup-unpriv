//! Platform-specific process controls for the shutdown hook.

#[cfg(unix)]
use crate::error::BootstrapResult;

#[cfg(windows)]
mod windows;

#[cfg(all(
    windows,
    any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker")
))]
pub(super) use self::windows::{PostmasterPid, parse_pid, process_is_running_for_platform};
#[cfg(windows)]
pub(super) use self::windows::{
    PostmasterProcess,
    ProcessExitFailsafe,
    force_shutdown,
    parse_postmaster_process,
    postmaster_process_is_running,
    prepare_process_exit_failsafe,
    request_shutdown,
};

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
    tracing::debug!(
        target: crate::observability::LOG_TARGET,
        pid = process,
        signal = libc::SIGTERM,
        "requesting Unix postmaster shutdown"
    );
    send_signal(process, libc::SIGTERM);
}

/// Forces `PostgreSQL` shutdown after the graceful timeout.
#[cfg(unix)]
pub(super) fn force_shutdown(process: PostmasterProcess) {
    tracing::warn!(
        target: crate::observability::LOG_TARGET,
        pid = process,
        signal = libc::SIGKILL,
        "forcing Unix postmaster shutdown"
    );
    send_signal(process, libc::SIGKILL);
}

/// Returns `true` when the process exists.
#[cfg(unix)]
pub(super) fn process_is_running_for_platform(pid: PostmasterPid) -> BootstrapResult<bool> {
    if pid <= 0 {
        return Ok(false);
    }

    // SAFETY: `kill` with signal `0` probes whether the process exists
    // without sending a signal. `pid` is positive, avoiding process-group
    // semantics.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return Ok(true);
    }

    let err = std::io::Error::last_os_error();
    if matches!(err.raw_os_error(), Some(code) if code == libc::ESRCH) {
        return Ok(false);
    }

    Err(color_eyre::eyre::eyre!("failed to probe process {pid}: {err}").into())
}

/// Returns `true` when the postmaster identity still matches a live process.
#[cfg(unix)]
pub(super) fn postmaster_process_is_running(process: PostmasterProcess) -> BootstrapResult<bool> {
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

#[cfg(all(test, unix))]
mod tests {
    //! Unit tests for Unix postmaster process controls.

    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("123", Some(123))]
    #[case("  42  ", Some(42))]
    #[case("0", None)]
    #[case("-5", None)]
    #[case("not-a-pid", None)]
    #[case("", None)]
    fn parse_pid_accepts_only_positive_integers(
        #[case] raw: &str,
        #[case] expected: Option<PostmasterPid>,
    ) {
        assert_eq!(parse_pid(raw), expected);
    }

    #[test]
    fn parse_postmaster_process_reads_first_line() {
        let contents = "789\n/var/lib/pgdata\n1700000000\n";
        assert_eq!(parse_postmaster_process(contents), Some(789));
        assert_eq!(parse_postmaster_process(""), None);
    }

    #[test]
    fn process_is_running_detects_current_process() {
        let pid = PostmasterPid::try_from(std::process::id()).expect("pid fits pid_t");
        assert!(
            process_is_running_for_platform(pid).expect("probe current process"),
            "the current process must be reported as running"
        );
        assert!(
            !process_is_running_for_platform(0).expect("probe non-positive pid"),
            "a non-positive pid must never be running"
        );
    }

    #[test]
    fn postmaster_process_is_running_matches_current_process() {
        let pid = PostmasterPid::try_from(std::process::id()).expect("pid fits pid_t");
        assert!(postmaster_process_is_running(pid).expect("probe current process"));
    }

    #[test]
    fn shutdown_signals_are_noops_for_non_positive_pids() {
        // These must not signal any real process; a non-positive pid is ignored
        // by `send_signal`, so the calls are safe.
        request_shutdown(0);
        force_shutdown(0);
    }

    #[test]
    fn prepare_process_exit_failsafe_returns_state() {
        let _failsafe = prepare_process_exit_failsafe(Some(1234));
        let _none_failsafe = prepare_process_exit_failsafe(None);
        // The Unix failsafe is an intentional no-op: assert it stays zero-sized
        // so a future change that smuggles in observable state (and would need
        // real teardown) fails here rather than silently regressing.
        assert_eq!(
            core::mem::size_of::<ProcessExitFailsafe>(),
            0,
            "the Unix ProcessExitFailsafe must remain a zero-sized no-op marker"
        );
    }
}

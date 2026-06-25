//! Platform-specific process controls for the shutdown hook.

/// Platform-specific process identifier stored in `postmaster.pid`.
#[cfg(unix)]
pub(super) type PostmasterPid = libc::pid_t;

/// Platform-specific process identifier stored in `postmaster.pid`.
#[cfg(windows)]
pub(super) type PostmasterPid = u32;

/// Parses a strictly positive postmaster PID.
#[cfg(unix)]
pub(super) fn parse_pid(raw: &str) -> Option<PostmasterPid> {
    let pid = raw.trim().parse::<PostmasterPid>().ok()?;
    (pid > 0).then_some(pid)
}

/// Parses a strictly positive postmaster PID.
#[cfg(windows)]
pub(super) fn parse_pid(raw: &str) -> Option<PostmasterPid> {
    let pid = raw.trim().parse::<PostmasterPid>().ok()?;
    (pid > 0).then_some(pid)
}

/// Requests graceful `PostgreSQL` shutdown.
#[cfg(unix)]
pub(super) fn request_shutdown(pid: PostmasterPid) {
    send_signal(pid, libc::SIGTERM);
}

/// Requests graceful `PostgreSQL` shutdown.
#[cfg(windows)]
pub(super) fn request_shutdown(pid: PostmasterPid) {
    run_taskkill(pid, ShutdownForce::No);
}

/// Forces `PostgreSQL` shutdown after the graceful timeout.
#[cfg(unix)]
pub(super) fn force_shutdown(pid: PostmasterPid) {
    send_signal(pid, libc::SIGKILL);
}

/// Forces `PostgreSQL` shutdown after the graceful timeout.
#[cfg(windows)]
pub(super) fn force_shutdown(pid: PostmasterPid) {
    run_taskkill(pid, ShutdownForce::Yes);
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

/// Returns `true` when the process exists and has not exited.
#[cfg(windows)]
pub(super) fn process_is_running_for_platform(pid: PostmasterPid) -> bool {
    if pid == 0 {
        return false;
    }

    ProcessHandle::open_query(pid).is_some_and(|process| process.is_active())
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

#[cfg(windows)]
use std::{ffi::c_void, process::Command, ptr::NonNull};

#[cfg(windows)]
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
#[cfg(windows)]
const STILL_ACTIVE: u32 = 259;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
    fn GetExitCodeProcess(process: *mut c_void, exit_code: *mut u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[cfg(windows)]
struct ProcessHandle(NonNull<c_void>);

#[cfg(windows)]
impl ProcessHandle {
    fn open_query(pid: PostmasterPid) -> Option<Self> {
        // SAFETY: `OpenProcess` is called with query-only access, handle
        // inheritance disabled, and a concrete process id read from
        // `postmaster.pid`. A null return is handled as absence.
        let raw_handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        NonNull::new(raw_handle).map(Self)
    }

    fn is_active(&self) -> bool {
        let mut exit_code = 0_u32;
        // SAFETY: `self.0` is a non-null process handle owned by this wrapper,
        // and `exit_code` points to valid writable storage for the duration of
        // the call.
        let succeeded = unsafe { GetExitCodeProcess(self.0.as_ptr(), &mut exit_code) };
        succeeded != 0 && exit_code == STILL_ACTIVE
    }
}

#[cfg(windows)]
impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: `ProcessHandle` owns the non-null handle returned by
        // `OpenProcess`; closing it exactly once in `Drop` releases the OS
        // resource. The return value cannot be acted on during best-effort
        // cleanup.
        unsafe {
            CloseHandle(self.0.as_ptr());
        }
    }
}

#[cfg(windows)]
enum ShutdownForce {
    No,
    Yes,
}

#[cfg(windows)]
fn run_taskkill(pid: PostmasterPid, force: ShutdownForce) {
    if pid == 0 {
        return;
    }

    let pid_argument = pid.to_string();
    let mut command = Command::new("taskkill");
    command.args(["/PID", &pid_argument, "/T"]);
    if matches!(force, ShutdownForce::Yes) {
        command.arg("/F");
    }

    drop(command.status());
}

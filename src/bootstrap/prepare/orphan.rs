//! Stops a server left running in a dead owner's data directory.
//!
//! A test process that dies abruptly releases its slot lock, but the
//! `postgres` server it started is a separate process and can outlive it,
//! still holding the data directory. The sweep must not delete a directory
//! from under a running server, so it reads the directory's
//! `postmaster.pid` first. A live `postgres` process named there is stopped
//! the way `pg_ctl stop -m immediate` stops one, with `SIGQUIT`, and the
//! directory is removed only once that process has gone. A process that
//! cannot be confirmed as `postgres`, or cannot be stopped, leaves the
//! directory in place.

#[cfg(unix)]
use std::time::{Duration, Instant};

use camino::Utf8Path;
use tracing::{info, warn};

use crate::observability::LOG_TARGET;

/// How long a stopped server gets to exit before the sweep gives up on it.
#[cfg(unix)]
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Stops one server by process ID; a seam so tests can refuse a stop.
pub(super) trait OrphanStop {
    /// Asks the process to stop and waits for it; returns whether it exited.
    fn stop(&self, pid: u32) -> bool;
}

/// What a data directory's `postmaster.pid` says about its server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServerState {
    /// No server: no PID file, an unreadable one, or a PID that is not a
    /// running `postgres` process.
    Absent,
    /// A running `postgres` process with this PID.
    Running(u32),
    /// A live process that cannot be confirmed as `postgres` on this
    /// platform; treated as a server that must not be deleted from under.
    Unconfirmed(u32),
}

/// Stops the orphaned server in `data_dir`, if there is one.
///
/// Returns whether the directory is now free to remove: true when no server
/// holds it, or when the one that did was stopped; false when a server is
/// still running or cannot be confirmed as gone.
pub(super) fn stop_orphaned_server(data_dir: &Utf8Path, stop: &dyn OrphanStop) -> bool {
    match server_state(data_dir) {
        ServerState::Absent => true,
        ServerState::Unconfirmed(pid) => {
            warn_unconfirmed(pid, data_dir);
            false
        }
        ServerState::Running(pid) => stop_running(pid, data_dir, stop),
    }
}

/// Warns that a live process holds a dead slot's `postmaster.pid` but is not
/// known to be `postgres`.
fn warn_unconfirmed(pid: u32, data_dir: &Utf8Path) {
    warn!(target: LOG_TARGET, pid, data_dir = %data_dir,
        "a live process holds a dead slot's postmaster.pid and is not confirmed as postgres");
}

/// Stops the running server and logs it when it went.
fn stop_running(pid: u32, data_dir: &Utf8Path, stop: &dyn OrphanStop) -> bool {
    let stopped = stop.stop(pid);
    if stopped {
        info!(target: LOG_TARGET, pid, data_dir = %data_dir, "stopped a server orphaned by a dead owner");
    }
    stopped
}

/// Reads `postmaster.pid` and classifies the process it names.
pub(super) fn server_state(data_dir: &Utf8Path) -> ServerState {
    let Some(pid) = postmaster_pid(data_dir) else {
        return ServerState::Absent;
    };
    if !process_alive(pid) {
        return ServerState::Absent;
    }
    match is_postgres(pid) {
        Some(true) => ServerState::Running(pid),
        Some(false) => ServerState::Absent,
        None => ServerState::Unconfirmed(pid),
    }
}

/// Returns the PID on the first line of `postmaster.pid`, if readable.
fn postmaster_pid(data_dir: &Utf8Path) -> Option<u32> {
    let text = std::fs::read_to_string(data_dir.join("postmaster.pid")).ok()?;
    text.lines()
        .next()?
        .trim()
        .parse()
        .ok()
        .filter(|pid| *pid > 0)
}

/// Returns whether a process with this PID exists.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    // Signal 0 checks existence and permission without delivering anything;
    // EPERM still means the process exists.
    match nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), None) {
        Ok(()) | Err(nix::errno::Errno::EPERM) => true,
        Err(_) => false,
    }
}

/// Without a signal-0 probe, any recorded PID is presumed alive, which keeps
/// the directory rather than risking a running server.
#[cfg(not(unix))]
const fn process_alive(_pid: u32) -> bool { true }

/// Returns whether the process is `postgres`, or None where the platform
/// gives no cheap way to tell.
#[cfg(target_os = "linux")]
fn is_postgres(pid: u32) -> Option<bool> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    Some(comm.trim_end() == "postgres")
}

/// Other platforms cannot confirm the name cheaply, so the directory is kept.
#[cfg(not(target_os = "linux"))]
const fn is_postgres(_pid: u32) -> Option<bool> { None }

/// Stops a server as `pg_ctl stop -m immediate` does: `SIGQUIT` to the
/// postmaster, then a wait for it to exit.
pub(super) struct SignalStop;

impl OrphanStop for SignalStop {
    #[cfg(unix)]
    fn stop(&self, pid: u32) -> bool {
        use nix::sys::signal::{Signal, kill};
        let Ok(raw) = i32::try_from(pid) else {
            return false;
        };
        if kill(nix::unistd::Pid::from_raw(raw), Signal::SIGQUIT).is_err() {
            return !process_alive(pid);
        }
        wait_for_exit(pid)
    }

    #[cfg(not(unix))]
    fn stop(&self, _pid: u32) -> bool { false }
}

/// Polls until the process has gone, up to [`STOP_TIMEOUT`].
#[cfg(unix)]
fn wait_for_exit(pid: u32) -> bool {
    let deadline = Instant::now() + STOP_TIMEOUT;
    while Instant::now() < deadline {
        if !process_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !process_alive(pid)
}

#[cfg(test)]
#[path = "orphan_tests.rs"]
pub(super) mod tests;

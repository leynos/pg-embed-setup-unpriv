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

/// An operating-system process ID, kept apart from other integers so that a
/// PID cannot be passed where a port or a count is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProcessId(u32);

impl ProcessId {
    /// Wraps a raw PID.
    pub(super) const fn new(raw: u32) -> Self { Self(raw) }

    /// Returns the raw PID.
    pub(super) const fn get(self) -> u32 { self.0 }
}

impl std::fmt::Display for ProcessId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Stops one server by process ID; a seam so tests can refuse a stop.
pub(super) trait OrphanStop {
    /// Asks the process to stop and waits for it; returns whether it exited.
    fn stop(&self, pid: ProcessId) -> bool;
}

/// What a data directory's `postmaster.pid` says about its server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ServerState {
    /// No server: no PID file, an unreadable one, or a PID that is not a
    /// running `postgres` process.
    Absent,
    /// A running `postgres` process with this PID.
    Running(ProcessId),
    /// A live process that cannot be confirmed as `postgres` on this
    /// platform; treated as a server that must not be deleted from under.
    Unconfirmed(ProcessId),
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
fn warn_unconfirmed(pid: ProcessId, data_dir: &Utf8Path) {
    warn!(target: LOG_TARGET, pid = %pid, data_dir = %data_dir,
        "a live process holds a dead slot's postmaster.pid and is not confirmed as postgres");
}

/// Stops the running server and logs it when it went.
fn stop_running(pid: ProcessId, data_dir: &Utf8Path, stop: &dyn OrphanStop) -> bool {
    let stopped = stop.stop(pid);
    if stopped {
        info!(target: LOG_TARGET, pid = %pid, data_dir = %data_dir, "stopped a server orphaned by a dead owner");
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
    match is_server_for(pid, data_dir) {
        Some(true) => ServerState::Running(pid),
        Some(false) => ServerState::Absent,
        None => ServerState::Unconfirmed(pid),
    }
}

/// Returns the PID on the first line of `postmaster.pid`, if readable.
fn postmaster_pid(data_dir: &Utf8Path) -> Option<ProcessId> {
    let text = std::fs::read_to_string(data_dir.join("postmaster.pid")).ok()?;
    let raw: u32 = text.lines().next()?.trim().parse().ok()?;
    (raw > 0).then_some(ProcessId::new(raw))
}

/// Returns whether a process with this PID exists.
#[cfg(unix)]
fn process_alive(pid: ProcessId) -> bool {
    let Ok(raw) = i32::try_from(pid.get()) else {
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
const fn process_alive(_pid: ProcessId) -> bool { true }

/// Returns whether the process is a `postgres` server running `data_dir`, or
/// None where the platform cannot say.
///
/// The name alone is not enough: the PID in a stale `postmaster.pid` can be
/// reused by an unrelated `PostgreSQL` server owned by the same user, and
/// stopping that would take down a database this crate never started. So a
/// process named `postgres` must also be shown to serve this directory. On
/// Linux the postmaster's working directory is its data directory, which
/// `/proc/<pid>/cwd` reports whatever the process title says. A process that
/// cannot be shown either way, such as another user's, is unconfirmed and
/// keeps the slot.
#[cfg(target_os = "linux")]
fn is_server_for(pid: ProcessId, data_dir: &Utf8Path) -> Option<bool> {
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    if !names_postgres(&comm) {
        return Some(false);
    }
    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    Some(cwd == std::fs::canonicalize(data_dir).ok()?)
}

/// Asks `ps` where `/proc` does not exist, as on macOS and the BSDs.
///
/// A name other than `postgres` is not a server. A `postgres` whose command
/// line names the directory as an argument is. One that does not is
/// unconfirmed rather than absent, because a server may rewrite its process
/// title and a wrong "absent" would delete a live server's directory. A `ps`
/// that cannot run, fails, or prints nothing is unconfirmed too: the caller
/// has just seen the process alive.
#[cfg(all(unix, not(target_os = "linux")))]
fn is_server_for(pid: ProcessId, data_dir: &Utf8Path) -> Option<bool> {
    if !names_postgres(&ps_field(PsField::Comm, pid)?) {
        return Some(false);
    }
    names_data_dir(&ps_field(PsField::Command, pid)?, data_dir).then_some(true)
}

/// Without a way to inspect a process, the directory is kept.
#[cfg(not(unix))]
const fn is_server_for(_pid: ProcessId, _data_dir: &Utf8Path) -> Option<bool> { None }

/// A column `ps` can print for one process.
#[cfg(all(unix, not(target_os = "linux")))]
#[derive(Clone, Copy)]
enum PsField {
    /// The command name, a path on macOS.
    Comm,
    /// The full command line.
    Command,
}

/// Runs `ps -o <field> -p <pid>`, returning its non-empty output.
#[cfg(all(unix, not(target_os = "linux")))]
fn ps_field(field: PsField, pid: ProcessId) -> Option<String> {
    let column = match field {
        PsField::Comm => "comm=",
        PsField::Command => "command=",
    };
    let output = std::process::Command::new("ps")
        .args(["-o", column, "-p", &pid.to_string()])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).into_owned();
    (output.status.success() && !text.trim().is_empty()).then_some(text)
}

/// Returns whether a command name, possibly a path, is that of `postgres`.
#[cfg(unix)]
fn names_postgres(comm: &str) -> bool {
    let name = comm.trim();
    name.rsplit('/').next() == Some("postgres")
}

/// Returns whether a command line names `data_dir` as a whole argument.
///
/// `ps` separates arguments with spaces, so the match must end at whitespace
/// or the end of the line: `…/data/1-2-0` must not match a server for
/// `…/data/1-2-01`.
#[cfg(all(unix, not(target_os = "linux")))]
fn names_data_dir(command_line: &str, data_dir: &Utf8Path) -> bool {
    command_line
        .match_indices(data_dir.as_str())
        .any(|(start, _)| {
            command_line
                .get(start + data_dir.as_str().len()..)
                .is_some_and(|rest| rest.chars().next().is_none_or(char::is_whitespace))
        })
}

/// Stops a server as `pg_ctl stop -m immediate` does: `SIGQUIT` to the
/// postmaster, then a wait for it to exit.
pub(super) struct SignalStop;

impl OrphanStop for SignalStop {
    #[cfg(unix)]
    fn stop(&self, pid: ProcessId) -> bool {
        use nix::sys::signal::{Signal, kill};
        let Ok(raw) = i32::try_from(pid.get()) else {
            return false;
        };
        if kill(nix::unistd::Pid::from_raw(raw), Signal::SIGQUIT).is_err() {
            return !process_alive(pid);
        }
        wait_for_exit(pid)
    }

    #[cfg(not(unix))]
    fn stop(&self, _pid: ProcessId) -> bool { false }
}

/// Polls until the process has gone, up to [`STOP_TIMEOUT`].
#[cfg(unix)]
fn wait_for_exit(pid: ProcessId) -> bool {
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

//! Stops a server that outlives an abruptly killed owner (#287).
//!
//! A test process killed by SIGKILL or an OOM kill runs no destructor, and the
//! `postgres` it started is a grandchild of the library's own spawn, so it
//! keeps running and holds the data directory. The next bootstrap's sweep
//! reclaims it, but only when another bootstrap runs. On Linux a watcher does
//! it at once: after a successful start the library spawns
//! `setsid flock <slot.lock> sh -c <script>`. `flock(2)` blocks until the
//! kernel releases the owner's slot lock, which it does when the owner dies
//! however it dies, and then the script stops the server.
//!
//! The watcher needs no helper binary, so it covers a library-only test binary.
//! The script does the identity check the sweep does in Rust: the PID in
//! `postmaster.pid` is signalled only if `/proc/<pid>/comm` is `postgres` and
//! `/proc/<pid>/cwd` is the data directory. A bare `pg_ctl stop -D` would
//! signal whatever a stale file names, and a recycled PID is not necessarily a
//! server. On a normal teardown the owner exits, the lock is released, and the
//! script finds no `postmaster.pid` and ends. Where `setsid`, `flock` or
//! `/proc` is missing, as on macOS, the spawn fails or the script matches
//! nothing, and the sweep stays the reclaim.

use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
};

use tracing::debug;

use crate::observability::LOG_TARGET;

/// The watcher's shell body: `$1` is the data directory.
///
/// It reads the PID on the first line of `postmaster.pid`, requires it to be
/// digits, requires `/proc/<pid>/comm` to be `postgres` and `/proc/<pid>/cwd` to
/// be the data directory's real path, and only then sends `SIGQUIT`, as
/// `pg_ctl stop -m immediate` does. Every other outcome exits quietly.
pub(crate) const WATCHER_SCRIPT: &str = r#"
data_dir=$1
pid=$(head -n 1 "$data_dir/postmaster.pid" 2>/dev/null) || exit 0
case $pid in ''|*[!0-9]*) exit 0 ;; esac
[ "$(cat "/proc/$pid/comm" 2>/dev/null)" = postgres ] || exit 0
real=$(cd "$data_dir" 2>/dev/null && pwd -P) || exit 0
[ "$(readlink "/proc/$pid/cwd" 2>/dev/null)" = "$real" ] || exit 0
kill -QUIT "$pid" 2>/dev/null
exit 0
"#;

/// Watchers spawned by this process, by data directory.
///
/// A watcher is meant to outlive its owner, so a live process never waits on
/// one; the exit that releases the slot lock is what wakes it. That lock is held
/// for the life of the process, not of the cluster (`cluster_slot::HELD`), so a
/// watcher would not end by itself when its cluster is dropped. A cluster that
/// is stopped normally ends its own watcher through [`release_watcher`], so a
/// long-lived test process does not accumulate one per cluster it has started.
static WATCHERS: Mutex<Option<HashMap<PathBuf, Child>>> = Mutex::new(None);

/// Builds the watcher command for a slot: blocks on `lock`, then runs the
/// script against `data_dir`, in a session of its own with no standard streams.
fn watcher_command(lock: &Path, data_dir: &Path) -> Command {
    let mut command = Command::new("setsid");
    command
        .arg("flock")
        .arg("--exclusive")
        .arg(lock)
        .args(["sh", "-c", WATCHER_SCRIPT, "pg-embed-watch"])
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

/// Spawns the watcher for `data_dir` and returns its handle.
///
/// # Errors
///
/// Returns the spawn error, `NotFound` where `setsid` or `flock` is missing.
pub(crate) fn spawn_watcher(lock: &Path, data_dir: &Path) -> io::Result<Child> {
    watcher_command(lock, data_dir).spawn()
}

/// Starts a watcher for the slot that owns `data_dir`, if it is a slot.
///
/// Best effort: a directory that is not a slot, or a host without `setsid` or
/// `flock`, is skipped with a debug event and the sweep remains the reclaim.
pub(crate) fn watch_slot_owner(data_dir: &Path) -> bool {
    let Some(lock) = super::cluster_slot::slot_lock_path(data_dir) else {
        return false;
    };
    match spawn_watcher(&lock, data_dir) {
        Ok(child) => {
            WATCHERS
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_or_insert_with(HashMap::new)
                .insert(data_dir.to_path_buf(), child);
            true
        }
        Err(err) => {
            debug!(target: LOG_TARGET, error = %err, "no orphan watcher for this cluster");
            false
        }
    }
}

/// Ends the watcher for `data_dir`, after its server was stopped normally.
///
/// Returns whether there was one. The watcher is killed and reaped, so it
/// neither lingers nor becomes a zombie.
pub(crate) fn release_watcher(data_dir: &Path) -> bool {
    let removed = WATCHERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_mut()
        .and_then(|watchers| watchers.remove(data_dir));
    let Some(mut child) = removed else {
        return false;
    };
    let _killed = child.kill();
    let _reaped = child.wait();
    true
}

#[cfg(test)]
#[path = "orphan_watch_tests.rs"]
mod tests;

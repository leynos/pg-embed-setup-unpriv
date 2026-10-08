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

use std::{io, path::Path};

use tracing::debug;

use crate::observability::{self, LOG_TARGET, Metric, OrphanWatcherOutcomeMetric};

#[path = "orphan_watch_process.rs"]
mod process;
#[path = "orphan_watch_registry.rs"]
mod registry;

pub(crate) use self::process::spawn_watcher;
#[cfg(test)]
use self::process::spawn_with;

/// Environment variable that, set to `off`, keeps a process from spawning
/// watchers, so the next bootstrap's sweep is the only reclaim of a killed
/// owner's server. A test of the sweep needs it: the watcher would stop the
/// orphan before the sweep has one to find.
pub(crate) const OPT_OUT_VAR: &str = "PG_EMBED_ORPHAN_WATCHER";

/// Whether the environment turns watchers off.
fn is_disabled(lookup: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    lookup(OPT_OUT_VAR).is_some_and(|value| value == "off")
}

/// Records what became of a watcher request: a debug event with the bounded
/// outcome and, for a failure, the error's kind (never a path), and the same
/// outcome as a [`Metric`] for a consumer's recorder.
fn record_outcome(outcome: OrphanWatcherOutcomeMetric, error_kind: Option<io::ErrorKind>) {
    debug!(
        target: LOG_TARGET,
        outcome = ?outcome,
        error_kind = ?error_kind,
        "orphan watcher"
    );
    observability::record(Metric::OrphanWatcher(outcome));
}

/// Starts a watcher for the slot that owns `data_dir`, if it is a slot.
///
/// Best effort: `PG_EMBED_ORPHAN_WATCHER=off` skips it, a directory that is not a slot, or one
/// whose slot status cannot be read, is skipped, and so is a host without `setsid` or `flock`.
/// Each case is recorded as an outcome, and the sweep remains the reclaim.
pub(crate) fn watch_slot_owner(data_dir: &Path) -> bool {
    use OrphanWatcherOutcomeMetric as Outcome;
    if is_disabled(|key| std::env::var_os(key)) {
        record_outcome(Outcome::Disabled, None);
        return false;
    }
    let lock = match super::cluster_slot::slot_lock_path(data_dir) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            record_outcome(Outcome::NotASlot, None);
            return false;
        }
        Err(err) => {
            record_outcome(Outcome::SlotUnknown, Some(err.kind()));
            return false;
        }
    };
    match spawn_watcher(&lock, data_dir) {
        Ok(child) => {
            if let Some(Err(err)) = registry::register(data_dir, child) {
                record_outcome(Outcome::ReleaseFailed, Some(err.kind()));
            }
            record_outcome(Outcome::Spawned, None);
            true
        }
        Err(err) => {
            record_outcome(Outcome::SpawnFailed, Some(err.kind()));
            false
        }
    }
}

/// Whether a watcher is registered for `data_dir`.
#[cfg(test)]
pub(crate) fn is_watching(data_dir: &Path) -> bool { registry::is_registered(data_dir) }

/// The process ID of the watcher registered for `data_dir`.
#[cfg(test)]
pub(crate) fn watcher_pid(data_dir: &Path) -> Option<u32> { registry::registered_pid(data_dir) }

/// Ends the watcher for `data_dir`, after its server was stopped normally.
///
/// Returns whether there was one. `Released` is recorded only once the watcher
/// was killed and reaped; if that failed, `ReleaseFailed` is recorded instead
/// with the error's kind, because the watcher may still be waiting.
pub(crate) fn release_watcher(data_dir: &Path) -> bool {
    match registry::remove_and_kill(data_dir) {
        None => false,
        Some(Ok(())) => {
            record_outcome(OrphanWatcherOutcomeMetric::Released, None);
            true
        }
        Some(Err(err)) => {
            record_outcome(OrphanWatcherOutcomeMetric::ReleaseFailed, Some(err.kind()));
            true
        }
    }
}

#[cfg(test)]
#[path = "orphan_watch_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "orphan_watch_fd_tests.rs"]
mod fd_tests;

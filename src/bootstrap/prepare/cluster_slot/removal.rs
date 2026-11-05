//! Removal of a swept slot's files, split from `cluster_slot` to keep that
//! module within the size limit.

use std::io;

use tracing::{info, warn};

use super::{ClusterSlot, lock_path};
use crate::observability::LOG_TARGET;

/// Removes a dead slot's directory, password file and lock file, and logs
/// the outcome.
pub(super) fn remove_slot(name: &str, slot: &ClusterSlot) {
    match remove_slot_files(slot) {
        Ok(()) => log_swept(name),
        Err(err) => log_removal_failed(name, &err),
    }
}

/// Logs that a dead slot was removed.
fn log_swept(name: &str) {
    info!(target: LOG_TARGET, slot = name, "swept a dead cluster's data directory");
}

/// Logs that a dead slot could not be fully removed.
fn log_removal_failed(name: &str, err: &io::Error) {
    warn!(target: LOG_TARGET, slot = name, error = %err, "could not fully remove a dead slot");
}

/// Removes every file of a slot, reporting the first failure after trying
/// them all so one stubborn leftover does not strand the others.
fn remove_slot_files(slot: &ClusterSlot) -> io::Result<()> {
    let results = [
        remove_if_present(std::fs::remove_dir_all(&slot.data_dir)),
        remove_if_present(std::fs::remove_file(&slot.password_file)),
        remove_if_present(std::fs::remove_file(lock_path(&slot.data_dir))),
    ];
    results.into_iter().collect()
}

/// Treats "already gone" as success.
fn remove_if_present(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

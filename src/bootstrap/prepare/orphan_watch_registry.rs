//! The registry of watchers this process has spawned (#287).

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Child,
    sync::Mutex,
};

/// Watchers spawned by this process, by data directory.
///
/// A watcher is meant to outlive its owner, so a live process never waits on
/// one; the exit that releases the slot lock is what wakes it. That lock is held
/// for the life of the process, not of the cluster (`cluster_slot::HELD`), so a
/// watcher would not end by itself when its cluster is dropped. A cluster that
/// is stopped normally ends its own watcher through [`release_watcher`], so a
/// long-lived test process does not accumulate one per cluster it has started.
static WATCHERS: Mutex<Option<HashMap<PathBuf, Child>>> = Mutex::new(None);

/// Keeps `child` as the watcher for `data_dir`.
///
/// A watcher already registered for the directory is ended rather than dropped,
/// because dropping a `Child` neither kills nor reaps it and the old watcher
/// would wait for the process to exit.
pub(super) fn register(data_dir: &Path, child: Child) {
    let replaced = WATCHERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get_or_insert_with(HashMap::new)
        .insert(data_dir.to_path_buf(), child);
    if let Some(mut old) = replaced {
        let _killed = old.kill();
        let _reaped = old.wait();
    }
}

/// Whether a watcher is registered for `data_dir`.
#[cfg(test)]
pub(super) fn is_registered(data_dir: &Path) -> bool {
    WATCHERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .is_some_and(|watchers| watchers.contains_key(data_dir))
}

/// Kills and reaps the watcher registered for `data_dir`, returning whether
/// there was one, so it neither lingers nor becomes a zombie.
pub(super) fn remove_and_kill(data_dir: &Path) -> bool {
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

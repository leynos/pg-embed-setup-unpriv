//! Per-cluster data directories under a shared root.
//!
//! Every process that bootstraps from a derived root (`PG_EMBED_ROOT`, or the
//! per-user default) gets a data directory of its own, so concurrent test
//! processes never share one (#261). Under `<root>/data` each cluster owns a
//! "slot": the data directory `<name>/`, its password file `<name>.pgpass`,
//! and a lock file `<name>.lock`.
//!
//! The lock file carries an exclusive kernel lock (`fs4`: `flock(2)` on Unix,
//! `LockFileEx` on Windows) that the owning process holds until it exits. The
//! kernel releases it when the process dies, however it dies. A later
//! bootstrap sweeps slots whose lock it can take, which are exactly the slots
//! whose owner is gone. Liveness is the held lock, never the PID in the name,
//! so a reused PID cannot make a dead slot look alive or a live one dead. The
//! lock file is opened by `std`, which sets `O_CLOEXEC`, so the `postgres` and
//! `pg_ctl` children a bootstrap spawns do not inherit it and cannot keep a
//! dead owner's slot "alive".

use std::{
    fs::{File, OpenOptions},
    io,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Report, eyre};
use fs4::{FileExt, TryLockError};
use tracing::{info, warn};

use super::orphan::{self, OrphanStop};
use crate::{
    error::{BootstrapError, BootstrapResult},
    observability::LOG_TARGET,
};

/// Suffix of a slot's lock file.
const LOCK_SUFFIX: &str = ".lock";

/// Suffix of a slot's password file.
const PASSWORD_SUFFIX: &str = ".pgpass";

/// Lock files held for the life of the process, one per claimed slot.
///
/// A slot's lock must outlive every handle to its cluster, and a shared
/// cluster lives until the process exits, so the files are kept here rather
/// than in the cluster. Dropping one would let another process sweep a live
/// cluster's directory.
static HELD: Mutex<Vec<File>> = Mutex::new(Vec::new());

/// A claimed slot: the data directory and password file for one cluster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClusterSlot {
    /// The cluster's own data directory, `<parent>/<name>`.
    pub(super) data_dir: Utf8PathBuf,
    /// The cluster's own password file, `<parent>/<name>.pgpass`.
    pub(super) password_file: Utf8PathBuf,
}

/// Name of the guard file that orders slot claims against a full cleanup. It
/// does not end in [`LOCK_SUFFIX`], so it is never read as a slot.
const CLAIM_GUARD: &str = ".claim-guard";

/// An exclusive lock on a slot parent's claim guard, held until dropped.
///
/// A slot claim takes it, and so does a full cleanup across its probe for live
/// peers and its removal of the install tree. A cluster starting during the
/// cleanup therefore either claimed its slot before the probe, and is seen as
/// a peer, or claims it after the removal, and provisions the tree afresh.
#[derive(Debug)]
pub(crate) struct ClaimGuard {
    _file: File,
}

impl ClaimGuard {
    /// Blocks until the guard under `parent` is free, then holds it.
    fn acquire(parent: &std::path::Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(parent.join(CLAIM_GUARD))?;
        FileExt::lock(&file)?;
        Ok(Self { _file: file })
    }
}

/// Takes the claim guard for the slot whose data directory is `data_dir`.
///
/// Returns None when `data_dir` is not a slot, so nothing shares its install
/// tree and there is nothing to order. A directory that cannot be told from a
/// slot, because its parent cannot be searched, is an error, not None:
/// unknown is not "not a slot".
pub(crate) fn claim_guard_for(data_dir: &std::path::Path) -> Option<io::Result<ClaimGuard>> {
    match slot_lookup(data_dir) {
        SlotLookup::NotSlot => None,
        SlotLookup::Slot(parent) => Some(ClaimGuard::acquire(parent)),
        SlotLookup::Unknown => Some(Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cannot tell whether the data directory is a slot",
        ))),
    }
}

/// Whether a data directory is a slot.
enum SlotLookup<'a> {
    /// A slot, whose lock file sits beside it under this parent.
    Slot(&'a std::path::Path),
    /// Not a slot, such as an explicit `PG_DATA_DIR`.
    NotSlot,
    /// The lock file's presence could not be read, so the answer is unknown.
    Unknown,
}

/// Looks for `data_dir`'s own lock file beside it. Only "not found" means it
/// is not a slot; any other failure to look leaves the answer unknown.
fn slot_lookup(data_dir: &std::path::Path) -> SlotLookup<'_> {
    let (Some(parent), Some(own)) = (
        data_dir.parent(),
        data_dir.file_name().and_then(std::ffi::OsStr::to_str),
    ) else {
        return SlotLookup::NotSlot;
    };
    match std::fs::metadata(parent.join(format!("{own}{LOCK_SUFFIX}"))) {
        Ok(metadata) if metadata.is_file() => SlotLookup::Slot(parent),
        Ok(_) => SlotLookup::NotSlot,
        Err(err) if err.kind() == io::ErrorKind::NotFound => SlotLookup::NotSlot,
        Err(_) => SlotLookup::Unknown,
    }
}

/// Sweeps dead slots under `parent`, then claims a new one for this cluster.
///
/// # Errors
///
/// Returns an error when `parent` cannot be created or listed, or when the
/// new slot's lock file cannot be created and locked.
pub(super) fn claim_slot(parent: &Utf8Path) -> BootstrapResult<ClusterSlot> {
    std::fs::create_dir_all(parent).map_err(|err| slot_error(parent, "create", err))?;
    sweep_dead_slots(parent, &orphan::SignalStop)?;
    let name = slot_name();
    let _guard = ClaimGuard::acquire(parent.as_std_path())
        .map_err(|err| slot_error(&parent.join(CLAIM_GUARD), "lock", err))?;
    let lock = lock_new_slot(parent, &name)?;
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(lock);
    let slot = slot_paths(parent, &name);
    info!(target: LOG_TARGET, data_dir = %slot.data_dir, "claimed a per-cluster data directory");
    Ok(slot)
}

/// Removes every slot under `parent` whose owner no longer holds its lock.
///
/// A slot whose directory still holds a live, orphaned server is stopped
/// through `stop` first, and skipped with a warning when it cannot be.
///
/// # Errors
///
/// Returns an error only when `parent` cannot be listed. Per-slot failures
/// are logged and skipped, so one stubborn leftover never blocks a bootstrap.
pub(super) fn sweep_dead_slots(parent: &Utf8Path, stop: &dyn OrphanStop) -> BootstrapResult<usize> {
    let entries = std::fs::read_dir(parent).map_err(|err| slot_error(parent, "list", err))?;
    let mut swept = 0;
    for name in entries.filter_map(|entry| lock_stem(&entry.ok()?)) {
        if sweep_one(parent, &name, stop) {
            swept += 1;
        }
    }
    Ok(swept)
}

/// Returns the lock file of the slot whose data directory is `data_dir`.
pub(super) fn lock_path(data_dir: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("{data_dir}{LOCK_SUFFIX}"))
}

/// Returns the lock file of `data_dir` when it is a slot, `Ok(None)` when it is
/// not, and an error, as [`claim_guard_for`] does, when that cannot be told.
///
/// # Errors
///
/// Returns `PermissionDenied` when the slot parent cannot be searched.
#[cfg(target_os = "linux")]
pub(crate) fn slot_lock_path(data_dir: &std::path::Path) -> io::Result<Option<std::path::PathBuf>> {
    match slot_lookup(data_dir) {
        SlotLookup::Slot(parent) => Ok(data_dir
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map(|own| parent.join(format!("{own}{LOCK_SUFFIX}")))),
        SlotLookup::NotSlot => Ok(None),
        SlotLookup::Unknown => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "cannot tell whether the data directory is a slot",
        )),
    }
}

/// Returns whether another live cluster shares the slot parent of `data_dir`,
/// and so the install tree beside it.
///
/// A cluster removes the shared install tree only when it is the last one
/// using it: peers keep running from it after their startup lock is released,
/// so deleting it would pull their binaries and extensions away. `data_dir` is
/// a slot only if its own lock file sits beside it; any other directory,
/// such as an explicit `PG_DATA_DIR`, has no peers. A peer is a slot whose lock
/// is held, or whose lock cannot be probed. A slot parent that cannot be listed
/// or an entry that cannot be read counts as a peer too, because doubt keeps
/// the tree.
pub(crate) fn has_live_peers(data_dir: &std::path::Path) -> bool {
    let parent = match slot_lookup(data_dir) {
        SlotLookup::Slot(parent) => parent,
        SlotLookup::NotSlot => return false,
        SlotLookup::Unknown => return true,
    };
    let Some(own) = data_dir.file_name().and_then(std::ffi::OsStr::to_str) else {
        return false;
    };
    any_slot_held(parent, Some(own))
}

/// Returns whether any slot under `parent` other than `own` holds its lock. A
/// parent that cannot be listed, or an entry that cannot be read, counts as
/// held.
fn any_slot_held(parent: &std::path::Path, own: Option<&str>) -> bool {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return true;
    };
    entries.into_iter().any(|listed| {
        listed.map_or(true, |entry| {
            lock_stem(&entry).is_some_and(|name| {
                Some(name.as_str()) != own
                    && !is_lock_free(&parent.join(format!("{name}{LOCK_SUFFIX}")))
            })
        })
    })
}

/// Returns the slot parent that clusters sharing the install tree at
/// `install_dir` claim under: `<root>/data` for `<root>/install`, or for a
/// version directory inside it. None when `install_dir` is not laid out that
/// way, so an explicit `PG_RUNTIME_DIR` has none. The parent need not exist
/// yet: a bootstrap that creates it and claims a slot is exactly the race the
/// caller guards against, by creating it first.
pub(crate) fn derived_slot_parent(install_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let named_install =
        |dir: &&std::path::Path| dir.file_name().is_some_and(|name| name == "install");
    let install_root = [Some(install_dir), install_dir.parent()]
        .into_iter()
        .flatten()
        .find(named_install)?;
    Some(install_root.parent()?.join("data"))
}

/// Returns whether any slot under `parent` holds its lock.
pub(crate) fn has_live_slots_in(parent: &std::path::Path) -> bool { any_slot_held(parent, None) }

/// Takes the claim guard of the slot parent `parent`.
///
/// Creates the parent if it is missing, as a slot claim does, so a cleanup and
/// a first claim contend on the same guard file.
pub(crate) fn claim_guard_at(parent: &std::path::Path) -> io::Result<ClaimGuard> {
    std::fs::create_dir_all(parent)?;
    ClaimGuard::acquire(parent)
}

/// Returns whether nobody holds the lock file at `path`; a file that cannot be
/// opened or probed counts as held.
fn is_lock_free(path: &std::path::Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    FileExt::try_lock(&file).is_ok()
}

/// Sweeps one slot if its owner is gone; returns whether it was removed.
fn sweep_one(parent: &Utf8Path, name: &str, stop: &dyn OrphanStop) -> bool {
    let Some(_lock) = take_dead_lock(parent, name) else {
        return false;
    };
    let slot = slot_paths(parent, name);
    if !orphan::stop_orphaned_server(&slot.data_dir, stop) {
        warn!(
            target: LOG_TARGET,
            data_dir = %slot.data_dir,
            "a dead owner's server could not be stopped; leaving its directory"
        );
        return false;
    }
    remove_slot(name, &slot);
    true
}

/// Takes a slot's lock without blocking, which succeeds only if its owner
/// is gone. The lock is held while the slot is removed.
fn take_dead_lock(parent: &Utf8Path, name: &str) -> Option<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(parent.join(format!("{name}{LOCK_SUFFIX}")))
        .ok()?;
    FileExt::try_lock(&file).ok().map(|()| file)
}

/// Removes a dead slot's directory, password file and lock file, and logs
/// the outcome.
fn remove_slot(name: &str, slot: &ClusterSlot) {
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

/// Creates and locks a new slot's lock file, refusing an existing one.
fn lock_new_slot(parent: &Utf8Path, name: &str) -> BootstrapResult<File> {
    let path = parent.join(format!("{name}{LOCK_SUFFIX}"));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|err| slot_error(&path, "create", err))?;
    match FileExt::try_lock(&file) {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(BootstrapError::from(eyre!(
            "slot lock {path} is already held"
        ))),
        Err(TryLockError::Error(err)) => Err(slot_error(&path, "lock", err)),
    }
}

/// Returns a slot name unique to this cluster: the process ID, the time,
/// and a per-process counter, so one process can hold several slots.
fn slot_name() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let count = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{nanos}-{count}", std::process::id())
}

/// Returns a slot's data directory and password file.
fn slot_paths(parent: &Utf8Path, name: &str) -> ClusterSlot {
    ClusterSlot {
        data_dir: parent.join(name),
        password_file: parent.join(format!("{name}{PASSWORD_SUFFIX}")),
    }
}

/// Returns the slot name of a lock file, or None for anything else.
fn lock_stem(entry: &std::fs::DirEntry) -> Option<String> {
    let file_name = entry.file_name().into_string().ok()?;
    file_name
        .strip_suffix(LOCK_SUFFIX)
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
}

/// Wraps an I/O failure on a slot path.
fn slot_error(path: &Utf8Path, action: &str, err: io::Error) -> BootstrapError {
    BootstrapError::from(Report::new(err).wrap_err(format!("cannot {action} {path}")))
}

#[cfg(test)]
#[path = "cluster_slot_tests.rs"]
mod tests;

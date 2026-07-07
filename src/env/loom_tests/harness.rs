//! Shared Loom fake-environment harness for `ScopedEnv` model tests.

use std::{cell::RefCell, collections::BTreeMap, ffi::OsString};

use loom::sync::{
    Mutex,
    MutexGuard,
    atomic::{AtomicUsize, Ordering},
};

use crate::env::{
    ScopedEnv,
    state::{EnvLockOps, ThreadStateInner},
};

type FakeEnv = BTreeMap<OsString, Option<OsString>>;

type Snapshot = Vec<(String, Option<String>)>;

loom::lazy_static! {
    static ref LOOM_ENV_LOCK: Mutex<FakeEnv> = Mutex::new(BTreeMap::new());
    static ref FAKE_ENV_MUTATIONS: AtomicUsize = AtomicUsize::new(0);
    static ref USE_THREAD_LOCAL_SNAPSHOT: AtomicUsize = AtomicUsize::new(0);
}

/// Provides the Loom-backed environment lock used by these model checks.
struct LoomEnvLock;

impl EnvLockOps for LoomEnvLock {
    type Guard = MutexGuard<'static, FakeEnv>;

    /// Acquire the Loom-backed fake environment lock.
    fn lock_env_mutex() -> Self::Guard {
        LOOM_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Keep the Loom lock interface aligned with production poison handling.
    fn ensure_lock_is_clean() {}

    /// Read a fake environment variable while the modelled guard is held.
    fn var_os(guard: &Self::Guard, key: &OsString) -> Option<OsString> {
        guard.get(key).cloned().flatten()
    }

    /// Write a fake environment variable and record the modelled mutation.
    fn set_var(guard: &mut Self::Guard, key: &OsString, value: OsString) {
        FAKE_ENV_MUTATIONS.fetch_add(1, Ordering::SeqCst);
        guard.insert(key.clone(), Some(value));
        record_thread_local_snapshot(guard);
    }

    /// Remove a fake environment variable and record the modelled mutation.
    fn remove_var(guard: &mut Self::Guard, key: &OsString) {
        FAKE_ENV_MUTATIONS.fetch_add(1, Ordering::SeqCst);
        guard.insert(key.clone(), None);
        record_thread_local_snapshot(guard);
    }
}

loom::thread_local! {
    static LOOM_THREAD_STATE: RefCell<ThreadStateInner<LoomEnvLock>> =
        RefCell::new(ThreadStateInner::new());
    static LAST_FAKE_ENV_SNAPSHOT: RefCell<Snapshot> = RefCell::new(Vec::new());
}

/// Enters a scoped environment frame using Loom thread-local state.
fn enter_scope_loom(vars: Vec<(OsString, Option<OsString>)>) -> usize {
    LOOM_THREAD_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        state.enter_scope(vars)
    })
}

/// Exits a scoped environment frame from Loom thread-local state.
fn exit_scope_loom(index: usize) {
    LOOM_THREAD_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        state.exit_scope(index);
    });
}

/// Applies test environment changes through the Loom state hooks.
pub(super) fn apply_loom(vars: &[(String, Option<String>)]) -> ScopedEnv {
    let owned: Vec<(OsString, Option<OsString>)> = vars
        .iter()
        .map(|(key, value)| (OsString::from(key), value.as_ref().map(OsString::from)))
        .collect();
    ScopedEnv::apply_owned_with_state(owned, enter_scope_loom, exit_scope_loom)
}

/// Convert compact test literals into owned environment pairs.
pub(super) fn vars(input: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    input
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.map(str::to_owned)))
        .collect()
}

/// Convert the fake environment map into a stable assertion snapshot.
fn snapshot_from_map(map: &FakeEnv) -> Snapshot {
    map.iter()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value
                    .as_ref()
                    .map(|stored| stored.to_string_lossy().into_owned()),
            )
        })
        .collect()
}

/// Store the latest fake environment state for panic-path assertions.
fn record_thread_local_snapshot(map: &FakeEnv) {
    let snapshot = snapshot_from_map(map);
    LAST_FAKE_ENV_SNAPSHOT.with(|cell| *cell.borrow_mut() = snapshot);
}

/// Snapshot the fake environment, using the unwind-safe copy when requested.
fn snapshot_fake_env() -> Snapshot {
    if USE_THREAD_LOCAL_SNAPSHOT.load(Ordering::SeqCst) != 0 {
        return LAST_FAKE_ENV_SNAPSHOT.with(|cell| cell.borrow().clone());
    }
    let guard = LoomEnvLock::lock_env_mutex();
    snapshot_from_map(&guard)
}

/// Snapshot the current scope through the held Loom lock guard.
fn snapshot_current_scope() -> Snapshot {
    LOOM_THREAD_STATE.with(|cell| {
        cell.borrow()
            .with_lock_guard(|guard| snapshot_from_map(guard))
    })
}

/// Return this model thread's current recursive scope depth.
pub(super) fn current_thread_depth() -> usize {
    LOOM_THREAD_STATE.with(|cell| cell.borrow().depth())
}

/// Report whether this model thread has fully reset its scope state.
pub(super) fn current_thread_state_is_reset() -> bool {
    LOOM_THREAD_STATE.with(|cell| {
        let state = cell.borrow();
        state.depth() == 0 && state.is_stack_empty() && !state.has_lock()
    })
}

/// Replace the fake environment with a deterministic baseline.
pub(super) fn seed_fake_env(input: &[(&str, Option<&str>)]) {
    let mut guard = LoomEnvLock::lock_env_mutex();
    guard.clear();
    for (key, value) in input {
        guard.insert(OsString::from(key), value.map(OsString::from));
    }
    record_thread_local_snapshot(&guard);
    FAKE_ENV_MUTATIONS.store(0, Ordering::SeqCst);
    USE_THREAD_LOCAL_SNAPSHOT.store(0, Ordering::SeqCst);
}

/// Assert that the fake environment matches the expected snapshot.
pub(super) fn assert_fake_env(expected: &[(&str, Option<&str>)]) {
    assert_eq!(snapshot_fake_env(), vars(expected));
}

/// Assert that the currently held scope exposes the expected environment.
pub(super) fn assert_current_scope_env(expected: &[(&str, Option<&str>)]) {
    assert_eq!(snapshot_current_scope(), vars(expected));
}

/// Return the number of fake environment mutations in the current model run.
pub(super) fn fake_env_mutations() -> usize { FAKE_ENV_MUTATIONS.load(Ordering::SeqCst) }

/// Switch assertions to the unwind-safe thread-local snapshot.
pub(super) fn use_thread_local_snapshot() { USE_THREAD_LOCAL_SNAPSHOT.store(1, Ordering::SeqCst); }

/// Run a bounded Loom model suited to the scoped environment scenarios.
pub(super) fn run_loom_model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let mut builder = loom::model::Builder::new();
    // These bounds keep the scheduler search tractable enough for routine CI.
    // Increasing the preemption bound in particular needs a matching runtime
    // budget review; see the developer guide for details.
    builder.max_threads = 3;
    builder.max_branches = 64;
    builder.preemption_bound = Some(3);
    builder.check(f);
}

//! Loom-backed concurrency checks for `ScopedEnv`.

use super::ScopedEnv;
use super::state::{EnvLockOps, ThreadStateInner};
use loom::sync::Arc;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::thread;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::panic::{self, AssertUnwindSafe};

loom::lazy_static! {
    static ref LOOM_ENV_LOCK: loom::sync::Mutex<FakeEnv> =
        loom::sync::Mutex::new(BTreeMap::new());
    static ref FAKE_ENV_MUTATIONS: AtomicUsize = AtomicUsize::new(0);
    static ref USE_THREAD_LOCAL_SNAPSHOT: AtomicUsize = AtomicUsize::new(0);
}

type FakeEnv = BTreeMap<OsString, Option<OsString>>;
type Snapshot = Vec<(String, Option<String>)>;

struct LoomEnvLock;

impl EnvLockOps for LoomEnvLock {
    type Guard = loom::sync::MutexGuard<'static, FakeEnv>;

    fn lock_env_mutex() -> Self::Guard {
        LOOM_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn ensure_lock_is_clean() {}

    fn var_os(guard: &Self::Guard, key: &OsString) -> Option<OsString> {
        guard.get(key).cloned().flatten()
    }

    fn set_var(guard: &mut Self::Guard, key: &OsString, value: OsString) {
        FAKE_ENV_MUTATIONS.fetch_add(1, Ordering::SeqCst);
        guard.insert(key.clone(), Some(value));
        record_thread_local_snapshot(guard);
    }

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

fn enter_scope_loom(vars: Vec<(OsString, Option<OsString>)>) -> usize {
    LOOM_THREAD_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        state.enter_scope(vars)
    })
}

fn exit_scope_loom(index: usize) {
    LOOM_THREAD_STATE.with(|cell| {
        let mut state = cell.borrow_mut();
        state.exit_scope(index);
    });
}

fn apply_loom(vars: &[(String, Option<String>)]) -> ScopedEnv {
    let owned: Vec<(OsString, Option<OsString>)> = vars
        .iter()
        .map(|(key, value)| (OsString::from(key), value.as_ref().map(OsString::from)))
        .collect();
    ScopedEnv::apply_owned_with_state(owned, enter_scope_loom, exit_scope_loom)
}

fn vars(input: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
    input
        .iter()
        .map(|(key, value)| ((*key).to_owned(), value.map(str::to_owned)))
        .collect()
}

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

fn record_thread_local_snapshot(map: &FakeEnv) {
    let snapshot = snapshot_from_map(map);
    LAST_FAKE_ENV_SNAPSHOT.with(|cell| *cell.borrow_mut() = snapshot);
}
fn snapshot_fake_env() -> Snapshot {
    if USE_THREAD_LOCAL_SNAPSHOT.load(Ordering::SeqCst) != 0 {
        return LAST_FAKE_ENV_SNAPSHOT.with(|cell| cell.borrow().clone());
    }
    let guard = LoomEnvLock::lock_env_mutex();
    snapshot_from_map(&guard)
}
fn snapshot_current_scope() -> Snapshot {
    LOOM_THREAD_STATE.with(|cell| {
        cell.borrow()
            .with_lock_guard(|guard| snapshot_from_map(guard))
    })
}
fn current_thread_depth() -> usize {
    LOOM_THREAD_STATE.with(|cell| cell.borrow().depth())
}
fn current_thread_state_is_reset() -> bool {
    LOOM_THREAD_STATE.with(|cell| {
        let state = cell.borrow();
        state.depth() == 0 && state.is_stack_empty() && !state.has_lock()
    })
}
fn seed_fake_env(input: &[(&str, Option<&str>)]) {
    let mut guard = LoomEnvLock::lock_env_mutex();
    guard.clear();
    for (key, value) in input {
        guard.insert(OsString::from(key), value.map(OsString::from));
    }
    record_thread_local_snapshot(&guard);
    FAKE_ENV_MUTATIONS.store(0, Ordering::SeqCst);
    USE_THREAD_LOCAL_SNAPSHOT.store(0, Ordering::SeqCst);
}
fn assert_fake_env(expected: &[(&str, Option<&str>)]) {
    assert_eq!(snapshot_fake_env(), vars(expected));
}
fn assert_current_scope_env(expected: &[(&str, Option<&str>)]) {
    assert_eq!(snapshot_current_scope(), vars(expected));
}
fn run_loom_model<F>(f: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let mut builder = loom::model::Builder::new();
    builder.max_threads = 3;
    builder.max_branches = 64;
    builder.preemption_bound = Some(3);
    builder.check(f);
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_serialises_concurrent_scopes() {
    run_loom_model(|| {
        let active_counter = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..2 {
            let active_clone = Arc::clone(&active_counter);
            handles.push(thread::spawn(move || {
                let empty: &[(String, Option<String>)] = &[];
                let _guard = apply_loom(empty);

                let previous = active_clone.fetch_add(1, Ordering::SeqCst);
                assert_eq!(
                    previous, 0,
                    "ScopedEnv must serialise concurrent environment scopes"
                );
                let current = active_clone.fetch_sub(1, Ordering::SeqCst);
                assert_eq!(current, 1, "ScopedEnv must release the scope cleanly");
            }));
        }

        for handle in handles {
            handle.join().expect("thread should join cleanly");
        }

        assert_eq!(active_counter.load(Ordering::SeqCst), 0);
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_allows_reentrant_scopes_on_one_thread() {
    run_loom_model(|| {
        let active_counter = Arc::new(AtomicUsize::new(0));
        let active_thread = Arc::clone(&active_counter);

        let handle = thread::spawn(move || {
            let empty: &[(String, Option<String>)] = &[];
            let outer = apply_loom(empty);
            let inner = apply_loom(empty);

            let previous = active_thread.fetch_add(1, Ordering::SeqCst);
            assert_eq!(previous, 0, "outer scope should hold the lock");
            let current = active_thread.fetch_sub(1, Ordering::SeqCst);
            assert_eq!(current, 1, "inner scope should not release the lock");

            drop(inner);
            drop(outer);
        });

        handle.join().expect("thread should join cleanly");
        assert_eq!(active_counter.load(Ordering::SeqCst), 0);
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_exercises_backup_restore_bookkeeping() {
    run_loom_model(|| {
        let baseline = &[
            ("PGDATA", Some("/var/lib/postgresql/base")),
            ("PGHOST", None),
            ("TZDIR", Some("/usr/share/zoneinfo")),
        ];
        seed_fake_env(baseline);

        {
            let overrides = vars(&[
                ("PGDATA", Some("/tmp/model-data")),
                ("PGHOST", Some("/tmp/model-socket")),
                ("TZDIR", None),
            ]);
            let _guard = apply_loom(&overrides);

            assert_current_scope_env(&[
                ("PGDATA", Some("/tmp/model-data")),
                ("PGHOST", Some("/tmp/model-socket")),
                ("TZDIR", None),
            ]);
        }

        assert_fake_env(baseline);
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_handles_spawn_while_holding_scope() {
    run_loom_model(|| {
        let baseline = &[("PGDATA", Some("base")), ("PGHOST", None)];
        seed_fake_env(baseline);

        let helper_started = Arc::new(AtomicUsize::new(0));
        let helper_acquired = Arc::new(AtomicUsize::new(0));
        let outer_released = Arc::new(AtomicUsize::new(0));
        let outer = apply_loom(&vars(&[("PGDATA", Some("outer"))]));
        assert_current_scope_env(&[("PGDATA", Some("outer")), ("PGHOST", None)]);

        let started_clone = Arc::clone(&helper_started);
        let acquired_clone = Arc::clone(&helper_acquired);
        let released_clone = Arc::clone(&outer_released);
        let helper = thread::spawn(move || {
            started_clone.store(1, Ordering::SeqCst);
            let _guard = apply_loom(&vars(&[("PGHOST", Some("helper"))]));
            acquired_clone.store(1, Ordering::SeqCst);
            while released_clone.load(Ordering::SeqCst) == 0 {
                thread::yield_now();
            }
            assert_current_scope_env(&[("PGDATA", Some("base")), ("PGHOST", Some("helper"))]);
        });

        while helper_started.load(Ordering::SeqCst) == 0 {
            thread::yield_now();
        }
        thread::yield_now();
        let was_blocked = helper_acquired.load(Ordering::SeqCst) == 0;
        drop(outer);
        outer_released.store(1, Ordering::SeqCst);
        helper.join().expect("helper thread should join cleanly");
        assert_eq!(
            helper_acquired.load(Ordering::SeqCst),
            1,
            "helper scope should acquire after the outer scope is released"
        );
        assert!(
            was_blocked,
            "helper scope should still be blocked while the outer scope is held"
        );

        assert_fake_env(baseline);
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_restores_on_panic_unwind() {
    run_loom_model(|| {
        let baseline = &[("PGDATA", Some("base")), ("TZDIR", None)];
        seed_fake_env(baseline);

        let result = panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = apply_loom(&vars(&[
                ("PGDATA", Some("panic-data")),
                ("TZDIR", Some("panic-tz")),
            ]));
            assert_current_scope_env(&[
                ("PGDATA", Some("panic-data")),
                ("TZDIR", Some("panic-tz")),
            ]);
            panic!("intentional scoped env unwind");
        }));

        let panic_payload = result.expect_err("scope should panic inside catch_unwind");
        assert_eq!(
            panic_payload.downcast_ref::<&'static str>(),
            Some(&"intentional scoped env unwind")
        );
        // Loom poisons the mocked mutex during unwind, so assert via the last
        // mutation snapshot rather than reacquiring the fake environment lock.
        USE_THREAD_LOCAL_SNAPSHOT.store(1, Ordering::SeqCst);
        assert_fake_env(baseline);
        assert_eq!(
            FAKE_ENV_MUTATIONS.load(Ordering::SeqCst),
            4,
            "panic path should apply two overrides and restore both values"
        );
        assert!(
            current_thread_state_is_reset(),
            "panic path should clear thread-local scope state"
        );
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_handles_asymmetric_scope_lifetimes() {
    run_loom_model(|| {
        let baseline = &[("PGDATA", Some("base")), ("PGHOST", None), ("TZDIR", None)];
        seed_fake_env(baseline);
        let active_counter = Arc::new(AtomicUsize::new(0));

        let long_counter = Arc::clone(&active_counter);
        let long = thread::spawn(move || {
            let _guard = apply_loom(&vars(&[
                ("PGDATA", Some("long")),
                ("PGHOST", Some("long-host")),
            ]));
            assert_eq!(long_counter.fetch_add(1, Ordering::SeqCst), 0);
            thread::yield_now();
            assert_eq!(long_counter.fetch_sub(1, Ordering::SeqCst), 1);
        });

        let quick_counter = Arc::clone(&active_counter);
        let quick = thread::spawn(move || {
            let _guard = apply_loom(&vars(&[("TZDIR", Some("quick"))]));
            assert_eq!(
                quick_counter.fetch_add(1, Ordering::SeqCst),
                0,
                "quick scope must not overlap the longer scope"
            );
            assert_eq!(quick_counter.fetch_sub(1, Ordering::SeqCst), 1);
        });

        long.join().expect("long thread should join cleanly");
        quick.join().expect("quick thread should join cleanly");
        assert_eq!(active_counter.load(Ordering::SeqCst), 0);
        assert_fake_env(baseline);
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_tracks_per_thread_depth_correctly() {
    run_loom_model(|| {
        let baseline = &[("PGDATA", Some("base")), ("PGHOST", None), ("TZDIR", None)];
        seed_fake_env(baseline);

        let first = thread::spawn(move || {
            let guard = apply_loom(&vars(&[("PGDATA", Some("thread-a"))]));
            assert_eq!(current_thread_depth(), 1);
            thread::yield_now();
            drop(guard);
            assert_eq!(current_thread_depth(), 0);
        });

        let second = thread::spawn(move || {
            let outer = apply_loom(&vars(&[("PGHOST", Some("thread-b"))]));
            assert_eq!(current_thread_depth(), 1);
            let inner = apply_loom(&vars(&[("TZDIR", Some("thread-b-inner"))]));
            assert_eq!(current_thread_depth(), 2);
            drop(inner);
            assert_eq!(current_thread_depth(), 1);
            drop(outer);
            assert_eq!(current_thread_depth(), 0);
        });

        first.join().expect("first thread should join cleanly");
        second.join().expect("second thread should join cleanly");
        assert_fake_env(baseline);
    });
}

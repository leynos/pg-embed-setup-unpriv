//! Loom-backed concurrency checks for `ScopedEnv`.

mod harness;

use std::panic::{self, AssertUnwindSafe};

use harness::{
    apply_loom,
    assert_current_scope_env,
    assert_fake_env,
    current_thread_depth,
    current_thread_state_is_reset,
    fake_env_mutations,
    run_loom_model,
    seed_fake_env,
    use_thread_local_snapshot,
    vars,
};
use loom::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

/// Models that independent threads cannot overlap active environment scopes.
#[test]
#[ignore = "requires Loom model checking"]
fn scoped_env_serializes_concurrent_scopes() {
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
                    "ScopedEnv must serialize concurrent environment scopes"
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

/// Models that nested scopes on one thread retain the outer lock until exit.
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

/// Models backup and restoration for set, replace, and unset operations.
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

/// Models helper-thread acquisition while another thread holds the scope.
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

/// Models restoration and thread-local reset when a scoped body unwinds.
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
        use_thread_local_snapshot();
        assert_fake_env(baseline);
        assert_eq!(
            fake_env_mutations(),
            4,
            "panic path should apply two overrides and restore both values"
        );
        assert!(
            current_thread_state_is_reset(),
            "panic path should clear thread-local scope state"
        );
    });
}

/// Models that short and long scopes remain serialized across threads.
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

/// Models independent per-thread recursion depth under nested scopes.
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

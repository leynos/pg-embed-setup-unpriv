//! Loom checks for shared singleton state transitions.
//!
//! These tests model `shared_singleton_core` under deterministic thread
//! interleavings. They verify that concurrent callers initialize the shared
//! singleton at most once, observe one cached success or failure, and never see
//! a torn intermediate state while reusing the same core state-machine logic as
//! `shared_singleton.rs`.

use super::shared_singleton_core::{SharedInitState, get_or_try_init_shared};
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

const FIRST_FAILURE: usize = 13;
const CACHED_FAILURE: usize = 17;

struct LoomSharedState {
    state: Mutex<SharedInitState<usize, usize>>,
    initialisations: AtomicUsize,
}

impl LoomSharedState {
    fn new() -> Self {
        Self {
            state: Mutex::new(SharedInitState::Uninitialised),
            initialisations: AtomicUsize::new(0),
        }
    }

    fn get_success(&self) -> Result<usize, usize> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        get_or_try_init_shared(
            &mut state,
            || {
                self.initialisations.fetch_add(1, Ordering::SeqCst);
                Ok(7)
            },
            |cached| *cached,
        )
    }

    fn get_failure(&self) -> Result<usize, usize> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        get_or_try_init_shared(
            &mut state,
            || {
                self.initialisations.fetch_add(1, Ordering::SeqCst);
                Err((CACHED_FAILURE, FIRST_FAILURE))
            },
            |cached| *cached,
        )
    }
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
fn shared_singleton_initialises_once_for_concurrent_callers() {
    run_loom_model(|| {
        let state = Arc::new(LoomSharedState::new());
        let first = Arc::clone(&state);
        let second = Arc::clone(&state);

        let first_thread = thread::spawn(move || first.get_success());
        let second_thread = thread::spawn(move || second.get_success());

        let Ok(first_result) = first_thread.join() else {
            panic!("first thread should join");
        };
        let Ok(second_result) = second_thread.join() else {
            panic!("second thread should join");
        };

        assert_eq!(first_result, Ok(7));
        assert_eq!(second_result, Ok(7));
        assert_eq!(state.initialisations.load(Ordering::SeqCst), 1);
    });
}

#[test]
#[ignore = "requires Loom model checking"]
fn shared_singleton_caches_failed_initialisation() {
    run_loom_model(|| {
        let state = Arc::new(LoomSharedState::new());
        let first = Arc::clone(&state);
        let second = Arc::clone(&state);

        let first_thread = thread::spawn(move || first.get_failure());
        let second_thread = thread::spawn(move || second.get_failure());

        let Ok(first_result) = first_thread.join() else {
            panic!("first thread should join");
        };
        let Ok(second_result) = second_thread.join() else {
            panic!("second thread should join");
        };

        assert_cached_failure_translation(first_result, second_result);
        assert_eq!(state.initialisations.load(Ordering::SeqCst), 1);
    });
}

fn assert_cached_failure_translation(
    first_result: Result<usize, usize>,
    second_result: Result<usize, usize>,
) {
    let Err(first_error) = first_result else {
        panic!("first caller should observe failure");
    };
    let Err(second_error) = second_result else {
        panic!("second caller should observe failure");
    };
    let mut errors = [first_error, second_error];
    errors.sort_unstable();
    assert_eq!(errors, [FIRST_FAILURE, CACHED_FAILURE]);
}

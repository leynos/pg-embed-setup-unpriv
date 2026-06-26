//! Loom checks for shutdown-hook registration state.

use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;

enum LoomShutdownRegistrationState {
    Empty,
    Registering,
    Registered,
}

struct LoomShutdownHook {
    state: Mutex<LoomShutdownRegistrationState>,
    registrations: AtomicUsize,
}

impl LoomShutdownHook {
    fn new() -> Self {
        Self {
            state: Mutex::new(LoomShutdownRegistrationState::Empty),
            registrations: AtomicUsize::new(0),
        }
    }

    fn register(&self) {
        if !self.reserve() {
            return;
        }
        self.registrations.fetch_add(1, Ordering::SeqCst);
        self.commit();
    }

    fn reserve(&self) -> bool {
        loop {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match *state {
                LoomShutdownRegistrationState::Empty => {
                    *state = LoomShutdownRegistrationState::Registering;
                    return true;
                }
                LoomShutdownRegistrationState::Registered => return false,
                LoomShutdownRegistrationState::Registering => {
                    drop(state);
                    thread::yield_now();
                }
            }
        }
    }

    fn commit(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *state = LoomShutdownRegistrationState::Registered;
    }

    fn is_registered(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        matches!(*state, LoomShutdownRegistrationState::Registered)
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
fn shutdown_hook_registers_once_under_concurrent_calls() {
    run_loom_model(|| {
        let hook = Arc::new(LoomShutdownHook::new());
        let first = Arc::clone(&hook);
        let second = Arc::clone(&hook);

        let first_thread = thread::spawn(move || first.register());
        let second_thread = thread::spawn(move || second.register());

        assert!(first_thread.join().is_ok(), "first thread should join");
        assert!(second_thread.join().is_ok(), "second thread should join");
        assert!(hook.is_registered(), "registration should store shutdown state");
        assert_eq!(hook.registrations.load(Ordering::SeqCst), 1);
    });
}

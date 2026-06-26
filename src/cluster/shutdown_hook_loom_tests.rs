//! Loom checks for shutdown-hook registration state.

use super::{ShutdownRegistration, ShutdownState, register_shutdown_hook_with_state};
use crate::CleanupMode;
use loom::sync::atomic::{AtomicUsize, Ordering};
use loom::sync::{Arc, Mutex};
use loom::thread;
use postgresql_embedded::Settings;
use std::time::Duration;

struct LoomShutdownHook {
    state: Mutex<Option<ShutdownState>>,
    registrations: AtomicUsize,
}

impl LoomShutdownHook {
    fn new() -> Self {
        Self {
            state: Mutex::new(None),
            registrations: AtomicUsize::new(0),
        }
    }

    fn register(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = register_shutdown_hook_with_state(
            &mut state,
            ShutdownRegistration {
                settings: Settings::default(),
                shutdown_timeout: Duration::from_secs(1),
                cleanup_mode: CleanupMode::None,
            },
            || {
                self.registrations.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        );
        assert!(result.is_ok(), "registration should succeed");
    }

    fn has_state(&self) -> bool {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.is_some()
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
        assert!(hook.has_state(), "registration should store shutdown state");
        assert_eq!(hook.registrations.load(Ordering::SeqCst), 1);
    });
}

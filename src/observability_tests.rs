//! Re-entrancy of the metrics seam: a recorder may touch the recorder slot
//! from inside its own callback.
//!
//! [`record`] takes the read lock only to clone the installed handle out. If
//! it held that lock across the callback instead, a recorder that installs
//! another recorder, or drops a guard, would ask for the write lock while
//! already holding the read lock on the same thread, and `RwLock` deadlocks
//! there. These tests run the recording on a worker thread and give it a
//! deadline, so the regression is a failed assertion rather than a hung run.
//!
//! Installation is process-wide, so both tests carry
//! `#[serial(metrics_recorder)]` for the same structural reason as the
//! password metric tests: two installing concurrently would collect each
//! other's counts.

use std::{
    sync::{
        Arc,
        Mutex,
        PoisonError,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use serial_test::serial;

use super::{
    Metric,
    MetricsRecorder,
    MetricsRecorderGuard,
    PasswordReuseOutcomeMetric,
    install_metrics_recorder,
    record,
};

/// How long a recording is allowed to take before it counts as wedged.
///
/// Generous by design: the work under test is a lock, a clone and a call, so
/// a machine slow enough to exceed this is not the failure being measured.
const DEADLINE: Duration = Duration::from_secs(10);

/// The count used throughout; the variant is irrelevant to re-entrancy.
const SAMPLE: Metric = Metric::PasswordReuse(PasswordReuseOutcomeMetric::Reused);

/// A recorder that runs `on_record` and then keeps what it was given.
struct Reentrant<F: Fn() + Send + Sync> {
    on_record: F,
    seen: Mutex<Vec<Metric>>,
}

impl<F: Fn() + Send + Sync> MetricsRecorder for Reentrant<F> {
    fn record(&self, metric: Metric) {
        (self.on_record)();
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(metric);
    }
}

/// A recorder that only counts, for use as the one an inner scope installs.
struct Counting(AtomicUsize);

impl MetricsRecorder for Counting {
    fn record(&self, _metric: Metric) { self.0.fetch_add(1, Ordering::Relaxed); }
}

/// Records [`SAMPLE`] on a worker thread and reports whether it finished
/// within [`DEADLINE`].
///
/// The worker is left running on a timeout rather than joined: joining a
/// thread blocked on the recorder lock would hang the run, which is the very
/// outcome the deadline exists to turn into a verdict. Each test runs in its
/// own process, so the stranded thread goes with it.
fn record_completes_within_deadline() -> bool {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        record(SAMPLE);
        // A closed channel means the deadline already elapsed and the test
        // has failed on it; there is nobody left to tell.
        tx.send(()).unwrap_or_default();
    });
    rx.recv_timeout(DEADLINE).is_ok()
}

/// Fails the test with `message` after abandoning `guards`.
///
/// The deadline is only missed when a worker thread is wedged holding the
/// recorder read lock. Dropping a guard then asks for the write lock and
/// blocks, so an ordinary panic would hang the process during unwinding and
/// the run would report a timeout instead of this message. Leaking the guards
/// costs one `Arc` in a process that is about to fail and exit.
fn fail_wedged(guards: Vec<MetricsRecorderGuard>, message: &str) -> ! {
    for guard in guards {
        std::mem::forget(guard);
    }
    panic!("{message}");
}

/// Installing a recorder from inside a recorder's callback completes.
#[test]
#[serial(metrics_recorder)]
fn a_recorder_may_install_another_recorder_from_its_callback() {
    let inner = Arc::new(Counting(AtomicUsize::new(0)));
    let installed = Arc::clone(&inner);
    let recorder = Arc::new(Reentrant {
        on_record: move || {
            let guard =
                install_metrics_recorder(Arc::clone(&installed) as Arc<dyn MetricsRecorder>);
            // Hold it only long enough to prove the write lock was available.
            drop(guard);
        },
        seen: Mutex::new(Vec::new()),
    });
    let outer = Arc::clone(&recorder);
    let guard = install_metrics_recorder(recorder as Arc<dyn MetricsRecorder>);

    if !record_completes_within_deadline() {
        fail_wedged(
            vec![guard],
            "recording deadlocked: install_metrics_recorder was called while record held the lock",
        );
    }
    assert_eq!(
        outer
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        &[SAMPLE],
        "the outer recorder did not receive the count",
    );
}

/// Uninstalling from inside a recorder's callback completes, and the
/// recorder survives its own uninstallation for the rest of the call.
///
/// This is the other half of the same hazard: `MetricsRecorderGuard::drop`
/// takes the write lock, so a callback that lets a guard fall out of scope
/// would wedge just as an installation would. It also pins the reason
/// `record` clones the handle rather than borrowing it: the recorder is
/// uninstalled part-way through its own callback and must still be alive to
/// finish.
#[test]
#[serial(metrics_recorder)]
fn a_recorder_may_drop_its_own_guard_from_its_callback() {
    // Installed underneath, so the callback's drop has something to restore.
    let outer = Arc::new(Counting(AtomicUsize::new(0)));
    let outer_guard = install_metrics_recorder(Arc::clone(&outer) as Arc<dyn MetricsRecorder>);

    let stashed: Arc<Mutex<Option<MetricsRecorderGuard>>> = Arc::new(Mutex::new(None));
    let dropper = Arc::clone(&stashed);
    let recorder = Arc::new(Reentrant {
        on_record: move || {
            let taken = dropper
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            drop(taken);
        },
        seen: Mutex::new(Vec::new()),
    });
    let inner = Arc::clone(&recorder);
    *stashed.lock().unwrap_or_else(PoisonError::into_inner) = Some(install_metrics_recorder(
        recorder as Arc<dyn MetricsRecorder>,
    ));

    if !record_completes_within_deadline() {
        let abandoned = stashed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let mut guards = vec![outer_guard];
        guards.extend(abandoned);
        fail_wedged(
            guards,
            "recording deadlocked: a guard dropped while record held the lock",
        );
    }
    assert_eq!(
        inner
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice(),
        &[SAMPLE],
        "the recorder did not finish its callback after uninstalling itself",
    );
    assert_eq!(
        outer.0.load(Ordering::Relaxed),
        0,
        "the restored recorder should not have seen this count",
    );
}

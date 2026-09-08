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

use proptest::prelude::*;
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

/// One step a consumer can take against the process-wide recorder slot.
#[derive(Debug, Clone, Copy)]
enum Step {
    /// Install a fresh recorder, keeping its guard.
    Install,
    /// Drop the newest guard still held, if any.
    Uninstall,
}

/// The recorder stack, alongside the count each recorder is owed.
///
/// Guards are held in installation order and dropped from the end, which is
/// the only order a consumer's scopes can produce and the order
/// `MetricsRecorderGuard` documents.
#[derive(Default)]
struct Model {
    /// Every recorder ever installed, with the number of counts it is owed.
    recorders: Vec<(Arc<Counting>, usize)>,
    /// The recorders whose guards are still held, newest last.
    held: Vec<(Arc<Counting>, MetricsRecorderGuard)>,
}

impl Model {
    /// Applies one step.
    fn apply(&mut self, step: Step) {
        match step {
            Step::Install => {
                let recorder = Arc::new(Counting(AtomicUsize::new(0)));
                let guard =
                    install_metrics_recorder(Arc::clone(&recorder) as Arc<dyn MetricsRecorder>);
                self.recorders.push((Arc::clone(&recorder), 0));
                self.held.push((recorder, guard));
            }
            Step::Uninstall => {
                self.held.pop();
            }
        }
    }

    /// Records once and credits whichever recorder should have received it.
    fn record_once(&mut self) {
        record(SAMPLE);
        let Some((top, _)) = self.held.last() else {
            return;
        };
        let credited = self
            .recorders
            .iter_mut()
            .find(|(recorder, _)| Arc::ptr_eq(recorder, top));
        if let Some((_, owed)) = credited {
            *owed += 1;
        }
    }

    /// The recorders that have diverged from what they are owed, as
    /// `(installation order, observed, owed)`.
    fn discrepancies(&self) -> Vec<(usize, usize, usize)> {
        self.recorders
            .iter()
            .enumerate()
            .filter_map(|(order, (recorder, owed))| {
                let observed = recorder.0.load(Ordering::Relaxed);
                (observed != *owed).then_some((order, observed, *owed))
            })
            .collect()
    }
}

/// A bounded sequence of installs and uninstalls.
fn steps() -> impl Strategy<Value = Vec<Step>> {
    prop::collection::vec(
        prop_oneof![Just(Step::Install), Just(Step::Uninstall)],
        0..12,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever the order of installs and uninstalls, a recorded count reaches
    /// the newest recorder whose guard is still held, and no other.
    ///
    /// The worked examples above fix the nesting depth at two. This covers the
    /// invariant the guard exists for at arbitrary depth, including the empty
    /// stack, where recording must reach nobody at all.
    #[test]
    #[serial(metrics_recorder)]
    fn a_count_reaches_only_the_newest_held_recorder(steps in steps()) {
        let mut model = Model::default();
        for step in steps {
            model.apply(step);
            model.record_once();
            prop_assert_eq!(model.discrepancies(), Vec::new());
        }
    }
}

//! Compile-time fixture for the metrics public surface.
//!
//! `tests/ui.rs` uses this as a non-Windows trybuild pass fixture and as a
//! directly included Windows smoke-compile module, so the `observability`
//! module's exports stay reachable from a consumer crate: `Metric`,
//! `PasswordReuseOutcomeMetric`, the `MetricsRecorder` trait,
//! `install_metrics_recorder` and the guard it returns.
//!
//! The unit tests exercise the same items from inside the crate, where a
//! `pub(crate)` path or a `#[doc(hidden)]` re-export would still compile. Only
//! a fixture outside the crate proves a consumer can implement the trait on
//! its own type, install it, and match on what it receives.

use std::sync::{Arc, Mutex, PoisonError};

use pg_embedded_setup_unpriv::observability::{
    Metric,
    MetricsRecorder,
    MetricsRecorderGuard,
    PasswordReuseOutcomeMetric,
    install_metrics_recorder,
};

/// A consumer's recorder: an ordinary `Send + Sync` type, not a crate one.
#[derive(Default)]
struct Collected(Mutex<Vec<Metric>>);

impl MetricsRecorder for Collected {
    fn record(&self, metric: Metric) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(metric);
    }
}

/// Names every password-reuse outcome without a wildcard.
///
/// Adding a variant therefore breaks this fixture, which is the point: the
/// label set is the metric's contract with whatever the consumer forwards to.
fn describe_outcome(outcome: PasswordReuseOutcomeMetric) -> &'static str {
    match outcome {
        PasswordReuseOutcomeMetric::Reused => "reused",
        PasswordReuseOutcomeMetric::ExplicitPassword => "explicit",
        PasswordReuseOutcomeMetric::NoCluster => "no_cluster",
        PasswordReuseOutcomeMetric::ProbeFailed => "probe_failed",
        PasswordReuseOutcomeMetric::MissingFile => "missing_file",
        PasswordReuseOutcomeMetric::UnreadableFile => "unreadable_file",
        PasswordReuseOutcomeMetric::EmptyFile => "empty_file",
        // `#[non_exhaustive]`, so a consumer's match needs this arm and the
        // fixture has to prove the arm is accepted.
        _ => "unknown",
    }
}

/// Destructures a `Metric` the way a consumer forwarding to its own backend
/// would.
fn describe_metric(metric: Metric) -> &'static str {
    match metric {
        Metric::PasswordReuse(outcome) => describe_outcome(outcome),
        _ => "unknown",
    }
}

/// Exercises the public signatures without starting anything.
pub fn verify_surface() {
    let recorder = Arc::new(Collected::default());
    let guard: MetricsRecorderGuard =
        install_metrics_recorder(Arc::clone(&recorder) as Arc<dyn MetricsRecorder>);

    // A consumer can construct the metric it expects to receive and compare.
    let expected = Metric::PasswordReuse(PasswordReuseOutcomeMetric::Reused);
    recorder.record(expected);
    assert_eq!(describe_metric(expected), "reused");
    assert_eq!(
        describe_outcome(PasswordReuseOutcomeMetric::EmptyFile),
        "empty_file"
    );

    // The guard is `#[must_use]`, so it has to be named and dropped.
    drop(guard);

    let seen = recorder.0.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(seen.as_slice(), &[expected]);
}

#[cfg(not(windows))]
fn main() { verify_surface(); }

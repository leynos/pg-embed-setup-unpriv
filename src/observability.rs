//! Shared tracing configuration and metric recording for observability.
//!
//! Centralizes the log target used by the crate so subscribers can filter
//! observability events without pulling in unrelated application logs, and
//! provides the seam through which a consumer collects counts.
//!
//! # Why a seam rather than a metrics crate
//!
//! Every subsystem here reports through bounded `tracing` events, which a
//! consumer can filter but not readily aggregate. A metric is the other thing:
//! a count to add up without parsing logs. This crate takes no metrics
//! dependency, because a library should not choose one on its consumer's
//! behalf; a consumer installs a [`MetricsRecorder`] and forwards each count
//! to whatever it already runs. With no recorder installed, recording is a
//! branch and a return.

use std::sync::{Arc, PoisonError, RwLock};

/// Target used by observability spans and logs.
pub(crate) const LOG_TARGET: &str = "pg_embed::observability";

/// Outcome of one password-reuse decision.
///
/// This is an enum rather than a string so the label set is bounded by
/// construction: a caller cannot route a password, a path, or any other
/// unbounded value into a metric through it. `ProbeFailed` and
/// `UnreadableFile` stay distinct even though both map to
/// [`BootstrapErrorKind::ClusterPasswordUnreadable`](crate::BootstrapErrorKind),
/// because failing to search the data directory and failing to read the
/// password file are different operational problems.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PasswordReuseOutcomeMetric {
    /// The stored password was adopted.
    Reused,
    /// The caller supplied `PG_PASSWORD`, so the file was never read.
    ExplicitPassword,
    /// The data directory holds no cluster.
    NoCluster,
    /// The `PG_VERSION` marker could not be probed.
    ProbeFailed,
    /// The cluster exists but its password file is absent.
    MissingFile,
    /// The password file exists but could not be read.
    UnreadableFile,
    /// The password file exists but is empty.
    EmptyFile,
}

/// A count this crate records.
///
/// Every variant carries a bounded label set and nothing else. Marked
/// `#[non_exhaustive]` so later releases can add counts without breaking a
/// consumer's `match`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Metric {
    /// One password-reuse decision, recorded once per bootstrap that consults
    /// the stored password.
    PasswordReuse(PasswordReuseOutcomeMetric),
}

/// Receives each [`Metric`] the crate records.
///
/// # Examples
///
/// ```
/// use std::sync::{Arc, Mutex};
///
/// use pg_embedded_setup_unpriv::observability::{
///     Metric,
///     MetricsRecorder,
///     install_metrics_recorder,
/// };
///
/// #[derive(Default)]
/// struct Collected(Mutex<Vec<Metric>>);
///
/// impl MetricsRecorder for Collected {
///     fn record(&self, metric: Metric) {
///         self.0
///             .lock()
///             .unwrap_or_else(|err| err.into_inner())
///             .push(metric);
///     }
/// }
///
/// let recorder = Arc::new(Collected::default());
/// let _guard = install_metrics_recorder(Arc::clone(&recorder) as Arc<dyn MetricsRecorder>);
/// // The guard restores the previous recorder when it drops.
/// ```
pub trait MetricsRecorder: Send + Sync {
    /// Records one count. Implementations must not panic.
    fn record(&self, metric: Metric);
}

/// The installed recorder, if any.
static RECORDER: RwLock<Option<Arc<dyn MetricsRecorder>>> = RwLock::new(None);

/// Restores the previous recorder when dropped.
///
/// Installing returns a guard rather than being permanent so a test can
/// install a collector without leaking it into the next test, matching how
/// this crate's other process-wide hooks behave.
#[must_use = "the recorder is uninstalled when the guard drops"]
pub struct MetricsRecorderGuard {
    previous: Option<Arc<dyn MetricsRecorder>>,
}

impl Drop for MetricsRecorderGuard {
    fn drop(&mut self) {
        let mut slot = RECORDER.write().unwrap_or_else(PoisonError::into_inner);
        *slot = self.previous.take();
    }
}

/// Installs `recorder` and returns a guard that restores the previous one.
///
/// Installation is process-wide and nests: the guard puts back whatever was
/// installed before it, so an inner scope cannot strand an outer collector.
///
/// # Examples
///
/// ```
/// use std::sync::{Arc, Mutex};
///
/// use pg_embedded_setup_unpriv::observability::{
///     Metric,
///     MetricsRecorder,
///     install_metrics_recorder,
/// };
///
/// #[derive(Default)]
/// struct Counting(Mutex<usize>);
///
/// impl MetricsRecorder for Counting {
///     fn record(&self, _metric: Metric) {
///         *self.0.lock().unwrap_or_else(|err| err.into_inner()) += 1;
///     }
/// }
///
/// let outer = Arc::new(Counting::default());
/// let outer_guard = install_metrics_recorder(Arc::clone(&outer) as Arc<dyn MetricsRecorder>);
///
/// let inner = Arc::new(Counting::default());
/// let inner_guard = install_metrics_recorder(Arc::clone(&inner) as Arc<dyn MetricsRecorder>);
/// // Counts recorded here reach `inner`, not `outer`.
/// drop(inner_guard);
/// // `outer` is installed again from here until `outer_guard` drops.
/// drop(outer_guard);
/// // Nothing is installed now, and recording is a branch and a return.
/// ```
pub fn install_metrics_recorder(recorder: Arc<dyn MetricsRecorder>) -> MetricsRecorderGuard {
    let mut slot = RECORDER.write().unwrap_or_else(PoisonError::into_inner);
    let previous = slot.replace(recorder);
    MetricsRecorderGuard { previous }
}

/// Records `metric` with the installed recorder, or does nothing.
///
/// The installed handle is cloned out and the read lock released before the
/// consumer's `record` runs. Holding the lock across that call would deadlock
/// any recorder that installs another recorder or drops a guard from inside
/// its callback, because both take the write lock on the same thread.
pub(crate) fn record(metric: Metric) {
    let installed = {
        let slot = RECORDER.read().unwrap_or_else(PoisonError::into_inner);
        slot.as_ref().map(Arc::clone)
    };
    if let Some(recorder) = installed {
        recorder.record(metric);
    }
}

#[cfg(test)]
#[path = "observability_tests.rs"]
mod tests;

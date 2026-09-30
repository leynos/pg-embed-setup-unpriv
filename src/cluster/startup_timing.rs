//! Per-step timing for the start lifecycle (#289).
//!
//! Each of `Setup` and `Start` logs one debug event with its elapsed time, on
//! the synchronous and asynchronous paths alike, so a slow start can be
//! attributed to a phase.

use super::{BootstrapResult, LOG_TARGET, LifecycleStep, debug};

/// Runs one lifecycle step and records how long it took.
///
/// `Setup` covers the installation and `initdb`, and `Start` the server's own
/// start, so a slow start can be attributed to a phase (#289).
pub(super) fn timed_step(
    step: LifecycleStep,
    run: impl FnOnce() -> BootstrapResult<()>,
) -> BootstrapResult<()> {
    let started = std::time::Instant::now();
    let outcome = run();
    log_step_finished(step, started);
    outcome
}

/// Async twin of [`timed_step`].
#[cfg(feature = "async-api")]
pub(super) async fn timed_step_async(
    step: LifecycleStep,
    run: impl std::future::Future<Output = BootstrapResult<()>>,
) -> BootstrapResult<()> {
    let started = std::time::Instant::now();
    let outcome = run.await;
    log_step_finished(step, started);
    outcome
}

fn log_step_finished(step: LifecycleStep, started: std::time::Instant) {
    debug!(
        target: LOG_TARGET,
        step = step.name(),
        elapsed_ms = started.elapsed().as_millis(),
        "lifecycle step finished"
    );
}

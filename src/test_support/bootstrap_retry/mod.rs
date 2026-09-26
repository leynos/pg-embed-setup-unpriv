//! Bounded retry for transient shared-cluster bootstrap failures.
//!
//! The shared-cluster singletons cache their first failure for the life of
//! the process, so one network hiccup while fetching the `PostgreSQL`
//! archive, or one slow start under load, used to fail every later test in
//! the binary. [`retry_transient`] retries such a failure a bounded number of
//! times before the singleton caches it.
//!
//! Only failures that are plainly transient are retried, and they are
//! recognized by type rather than by message: an I/O or repository failure
//! from `postgresql_embedded` or its archive layer (download and
//! extraction), a lifecycle timeout, and an extension archive that could not
//! be downloaded. Everything else, a configuration error or a missing
//! version for instance, fails at once. So does an I/O or archive failure
//! after the binaries came from the shared binary cache, which would only
//! copy the same tree again. A transient failure that persists
//! still fails after the last attempt, with the attempt count added to the
//! report, so a retry never turns a real failure into a pass.
//!
//! Scope: this module serves the shared-cluster singletons in
//! `shared_singleton.rs` alone. Root-privileged bootstraps report worker
//! failures as text, so they carry no typed cause and are never retried.

use std::time::Duration;

use postgresql_archive::Error as ArchiveError;
use postgresql_embedded::Error as EmbeddedError;

use crate::error::{
    BootstrapError,
    BootstrapErrorKind,
    BootstrapResult,
    CachedBinariesUsed,
    LifecycleTimeout,
};

/// How many attempts a bootstrap gets, and how long to wait between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RetryPolicy {
    attempts: u32,
    first_delay: Duration,
}

impl RetryPolicy {
    /// The shared-cluster policy: three attempts, one then two seconds apart.
    pub(super) const SHARED_CLUSTER: Self = Self::new(3, Duration::from_secs(1));

    /// Builds a policy of `attempts` tries, doubling the delay after each.
    ///
    /// The first attempt is always made, so a policy of zero attempts
    /// behaves as a policy of one.
    pub(super) const fn new(attempts: u32, first_delay: Duration) -> Self {
        Self {
            attempts,
            first_delay,
        }
    }

    /// Returns the delay before the attempt that follows attempt `made`.
    const fn delay_after(self, made: u32) -> Duration {
        self.first_delay
            .saturating_mul(2_u32.saturating_pow(made.saturating_sub(1)))
    }
}

/// Runs `attempt` until it succeeds, fails deterministically, or exhausts
/// the policy, sleeping through `sleep` between tries.
///
/// A deterministic failure is returned unchanged. A transient failure on
/// the last attempt keeps its kind and gains the attempt count.
///
/// # Errors
///
/// Returns the first deterministic failure, or the last transient one once
/// the policy is exhausted.
pub(super) fn retry_transient<T, Attempt, Sleep>(
    policy: RetryPolicy,
    mut attempt: Attempt,
    mut sleep: Sleep,
) -> BootstrapResult<T>
where
    Attempt: FnMut() -> BootstrapResult<T>,
    Sleep: FnMut(Duration),
{
    let mut made = 1;
    loop {
        let err = match attempt() {
            Ok(value) => return Ok(value),
            Err(err) => err,
        };
        if !is_transient(&err) {
            return Err(err);
        }
        if made >= policy.attempts {
            return Err(exhausted(err, made));
        }
        log_retry(&err, made, policy.attempts);
        sleep(policy.delay_after(made));
        made += 1;
    }
}

/// Returns whether a bootstrap failure is plainly transient.
///
/// After a cache hit the binaries were copied, not downloaded, so an I/O or
/// archive failure points at the cached tree and would recur on every retry;
/// only a timeout stays transient then.
pub(crate) fn is_transient(err: &BootstrapError) -> bool {
    let from_cache = err.report().downcast_ref::<CachedBinariesUsed>().is_some();
    err.kind() == BootstrapErrorKind::ExtensionArchiveUnavailable
        || err
            .report()
            .chain()
            .any(|cause| is_transient_cause(cause, from_cache))
}

/// Returns whether one cause in a report's chain is transient.
fn is_transient_cause(cause: &(dyn std::error::Error + 'static), from_cache: bool) -> bool {
    cause.is::<LifecycleTimeout>()
        || !from_cache
            && cause
                .downcast_ref::<EmbeddedError>()
                .is_some_and(is_transient_embedded)
}

/// Returns whether a `postgresql_embedded` error comes from I/O or the
/// archive repository, the download and extraction paths.
const fn is_transient_embedded(err: &EmbeddedError) -> bool {
    match err {
        EmbeddedError::IoError(_) => true,
        EmbeddedError::ArchiveError(inner) => {
            matches!(
                inner,
                ArchiveError::IoError(_) | ArchiveError::RepositoryFailure(_)
            )
        }
        _ => false,
    }
}

/// Adds the attempt count to the last transient failure, keeping its kind.
fn exhausted(err: BootstrapError, made: u32) -> BootstrapError {
    let kind = err.kind();
    let report = err.into_report().wrap_err(format!(
        "bootstrap failed after {made} attempts; each failure was transient"
    ));
    BootstrapError::new(kind, report)
}

/// Logs a transient failure that is about to be retried.
fn log_retry(err: &BootstrapError, made: u32, attempts: u32) {
    tracing::warn!(
        target: crate::observability::LOG_TARGET,
        attempt = made,
        attempts,
        error = %err,
        "transient bootstrap failure; retrying"
    );
}

#[cfg(test)]
mod tests;

//! Tests for the bounded retry of transient bootstrap failures.

use std::{cell::RefCell, time::Duration};

use color_eyre::eyre::{WrapErr, eyre};
use postgresql_archive::Error as ArchiveError;
use postgresql_embedded::Error as EmbeddedError;
use proptest::prelude::*;
use rstest::rstest;

use super::{RetryPolicy, is_transient, retry_transient};
use crate::error::{BootstrapError, BootstrapErrorKind, BootstrapResult, LifecycleTimeout};

/// A failure as the in-process lifecycle reports it: the embedded error
/// wrapped in the operation's context.
fn embedded_failure(err: EmbeddedError) -> BootstrapError {
    BootstrapError::from(
        Err::<(), _>(err)
            .context("postgresql_embedded::setup()")
            .expect_err("the result is an error"),
    )
}

/// A download failure from the archive layer.
fn download_failure() -> BootstrapError {
    embedded_failure(EmbeddedError::ArchiveError(ArchiveError::IoError(
        "connection reset by peer".into(),
    )))
}

/// A failure that retrying cannot fix: the requested version does not exist.
fn missing_version() -> BootstrapError {
    embedded_failure(EmbeddedError::ArchiveError(ArchiveError::VersionNotFound(
        "=99.0.0".into(),
    )))
}

/// A lifecycle timeout, as the invoker raises it.
fn start_timeout() -> BootstrapError {
    BootstrapError::from(color_eyre::Report::new(LifecycleTimeout {
        context: "postgresql_embedded::start()",
        seconds: 60.0,
    }))
}

/// Runs `retry_transient` over a script of outcomes, returning the result,
/// the number of attempts made and the delays slept.
fn run_script(
    policy: RetryPolicy,
    script: Vec<BootstrapResult<u8>>,
) -> (BootstrapResult<u8>, usize, Vec<Duration>) {
    let outcomes = RefCell::new(script.into_iter());
    let made = RefCell::new(0);
    let slept = RefCell::new(Vec::new());
    let result = retry_transient(
        policy,
        || {
            *made.borrow_mut() += 1;
            outcomes
                .borrow_mut()
                .next()
                .unwrap_or_else(|| Err(download_failure()))
        },
        |delay| slept.borrow_mut().push(delay),
    );
    (result, made.into_inner(), slept.into_inner())
}

#[rstest]
#[case::download(download_failure(), true)]
#[case::extraction_io(embedded_failure(EmbeddedError::IoError("disk full".into())), true)]
#[case::repository(
    embedded_failure(EmbeddedError::ArchiveError(ArchiveError::RepositoryFailure(
        "502 Bad Gateway".into()
    ))),
    true
)]
#[case::start_timeout(start_timeout(), true)]
#[case::extension_download(
    BootstrapError::new(BootstrapErrorKind::ExtensionArchiveUnavailable, eyre!("no route")),
    true
)]
#[case::missing_version(missing_version(), false)]
#[case::start_refused(
    embedded_failure(EmbeddedError::DatabaseStartError("could not bind".into())),
    false
)]
#[case::configuration(
    BootstrapError::new(BootstrapErrorKind::ExtensionConfigInvalid, eyre!("no manifest")),
    false
)]
#[case::untyped(BootstrapError::from(eyre!("worker exited with status 1")), false)]
fn failures_are_classified_by_type(#[case] err: BootstrapError, #[case] expected: bool) {
    assert_eq!(is_transient(&err), expected, "classified {err:?}");
}

#[test]
fn a_transient_first_failure_is_retried_and_then_succeeds() {
    let (result, made, slept) = run_script(
        RetryPolicy::SHARED_CLUSTER,
        vec![Err(download_failure()), Ok(7)],
    );

    assert_eq!(result.expect("the second attempt succeeds"), 7);
    assert_eq!(made, 2, "one retry after the transient failure");
    assert_eq!(slept, vec![Duration::from_secs(1)]);
}

#[test]
fn a_deterministic_failure_is_not_retried() {
    let (result, made, slept) = run_script(
        RetryPolicy::SHARED_CLUSTER,
        vec![Err(missing_version()), Ok(7)],
    );

    let err = result.expect_err("a missing version fails at once");
    assert_eq!(made, 1, "a deterministic failure gets no second attempt");
    assert!(slept.is_empty(), "nothing is waited for: {slept:?}");
    assert!(
        !err.to_string().contains("attempts"),
        "the failure is returned unchanged: {err}"
    );
}

#[test]
fn a_permanent_transient_failure_still_fails_after_the_bound() {
    let (result, made, slept) = run_script(
        RetryPolicy::SHARED_CLUSTER,
        vec![
            Err(start_timeout()),
            Err(start_timeout()),
            Err(start_timeout()),
            Ok(7),
        ],
    );

    let err = result.expect_err("three timeouts exhaust the policy");
    assert_eq!(made, 3, "the fourth, successful outcome is never reached");
    assert_eq!(slept, vec![Duration::from_secs(1), Duration::from_secs(2)]);
    assert!(
        err.to_string()
            .contains("bootstrap failed after 3 attempts"),
        "the report names the count: {err}"
    );
    assert!(is_transient(&err), "the last cause is kept: {err:?}");
    assert_eq!(err.kind(), BootstrapErrorKind::Other, "the kind is kept");
}

#[test]
fn a_zero_attempt_policy_makes_exactly_one_attempt() {
    let (result, made, slept) = run_script(
        RetryPolicy::new(0, Duration::ZERO),
        vec![Err(download_failure()), Ok(7)],
    );

    let err = result.expect_err("the one attempt fails");
    assert_eq!(made, 1, "the first attempt is always made, and no other");
    assert!(slept.is_empty(), "nothing is waited for: {slept:?}");
    assert!(err.to_string().contains("after 1 attempts"), "{err}");
}

/// One scripted outcome for the bound property.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    Success,
    Transient,
    Deterministic,
}

impl Outcome {
    /// Builds the result this outcome stands for.
    fn result(self) -> BootstrapResult<u8> {
        match self {
            Self::Success => Ok(1),
            Self::Transient => Err(download_failure()),
            Self::Deterministic => Err(missing_version()),
        }
    }
}

/// Returns the number of attempts a policy of `attempts` should make.
fn expected_attempts(script: &[Outcome], attempts: usize) -> usize {
    let stop = script
        .iter()
        .position(|outcome| !matches!(outcome, Outcome::Transient))
        .map_or(usize::MAX, |index| index + 1);
    stop.min(attempts)
}

proptest! {
    #[test]
    fn attempts_stop_at_the_first_decisive_outcome_or_the_bound(
        attempts in 1_u32..6,
        script in prop::collection::vec(
            prop_oneof![
                Just(Outcome::Success),
                Just(Outcome::Transient),
                Just(Outcome::Deterministic),
            ],
            1..8,
        ),
    ) {
        let policy = RetryPolicy::new(attempts, Duration::ZERO);
        let (result, made, slept) = run_script(policy, script.iter().map(|o| o.result()).collect());

        let bound = usize::try_from(attempts).expect("a small count fits");
        let expected = expected_attempts(&script, bound);
        prop_assert_eq!(made, expected);
        prop_assert_eq!(slept.len(), expected - 1);
        let decisive = script.get(expected - 1).copied().unwrap_or(Outcome::Transient);
        prop_assert_eq!(result.is_ok(), matches!(decisive, Outcome::Success));
    }
}

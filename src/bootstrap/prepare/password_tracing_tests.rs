//! The `password_reuse` event each branch emits, and what it may carry.
//!
//! The metric tests next door prove one bounded outcome is recorded per
//! call. They say nothing about the event, which is the surface an operator
//! reads when a bootstrap adopts, or refuses to adopt, a stored password.
//! An event that never fired, fired at the wrong level, or named the wrong
//! outcome would leave every metric assertion passing.
//!
//! The level is part of the contract rather than incidental: a success is
//! `INFO` and a refusal is `WARN`, so a deployment filtering at `WARN` sees
//! exactly the calls that ended a bootstrap.

use super::*;
use crate::test_support::{capture_info_logs, capture_warn_logs};

/// One success scenario: the staged state, the caller's choice, the outcome.
///
/// Bundled into a struct because the case carries four fields and clippy
/// caps a function at four arguments; the metric tests next door use the
/// same shape for the same reason.
struct TracingCase {
    with_cluster: bool,
    stored: Option<&'static str>,
    explicit: bool,
    expected: &'static str,
}

/// Returns the `password_reuse` lines among `logs`.
fn reuse_events(logs: &[String]) -> Vec<&String> {
    logs.iter()
        .filter(|line| line.contains("password_reuse"))
        .collect()
}

/// Returns the one `password_reuse` line among `logs`.
///
/// Exactly one is part of the contract: a branch emitting twice would
/// double-count in whatever reads the events, and one emitting none would
/// leave the metric assertions next door passing over a silent bootstrap.
/// The helper reports that as an error for the calling test to unwrap rather
/// than deciding the verdict itself.
fn sole_reuse_event(logs: &[String]) -> Result<String> {
    let events = reuse_events(logs);
    match events.as_slice() {
        [only] => Ok((*only).clone()),
        other => Err(eyre!(
            "expected exactly one password_reuse event, found {}: {logs:?}",
            other.len()
        )),
    }
}

/// Runs one reuse with `explicit`, capturing info-level lines.
///
/// The helper arranges and runs; it does not decide. Whether the call was
/// meant to succeed is the calling test's verdict, so the result travels out
/// rather than being unwrapped here.
fn info_logs_of(
    scratch: &Scratch,
    explicit: bool,
) -> (Vec<String>, BootstrapResult<PasswordReuseOutcome>) {
    let mut settings = Settings {
        password: "fresh".into(),
        ..Settings::default()
    };
    capture_info_logs(|| {
        reuse_existing_password(
            &mut settings,
            &scratch.data_dir,
            &scratch.password_file,
            explicit,
        )
    })
}

/// Runs one reuse with no explicit password, capturing warning-level lines.
fn warn_logs_of(scratch: &Scratch) -> (Vec<String>, BootstrapResult<PasswordReuseOutcome>) {
    let mut settings = Settings {
        password: "fresh".into(),
        ..Settings::default()
    };
    capture_warn_logs(|| {
        reuse_existing_password(
            &mut settings,
            &scratch.data_dir,
            &scratch.password_file,
            false,
        )
    })
}

/// Every success branch emits one `password_reuse` event naming its outcome.
#[rstest]
#[case::reused(TracingCase { with_cluster: true, stored: Some("kept-secret"), explicit: false, expected: "Reused" })]
#[case::explicit(TracingCase { with_cluster: true, stored: Some("kept-secret"), explicit: true, expected: "ExplicitPassword" })]
#[case::no_cluster(TracingCase { with_cluster: false, stored: None, explicit: false, expected: "NoCluster" })]
fn each_success_branch_emits_its_outcome(scratch: Result<Scratch>, #[case] case: TracingCase) {
    let dir = scratch.expect("scratch");
    arrange(&dir, case.with_cluster, case.stored).expect("arrange");
    let (logs, outcome) = info_logs_of(&dir, case.explicit);
    outcome.expect("the reuse must succeed");
    let event = sole_reuse_event(&logs).expect("one event");
    assert!(
        event.contains(case.expected),
        "the event must name the {} outcome: {event:?}",
        case.expected
    );
}

/// Every failure branch emits one `password_reuse` warning naming its outcome.
///
/// The two portable failures are covered here. `probe_failed` and
/// `unreadable_file` need the permission staging the metric tests already
/// carry, and neither adds an event shape this does not.
#[rstest]
#[case::missing_file(None, "missing_file")]
#[case::empty_file(Some(""), "empty_file")]
fn each_failure_branch_warns_with_its_outcome(
    scratch: Result<Scratch>,
    #[case] stored: Option<&str>,
    #[case] expected: &str,
) {
    let dir = scratch.expect("scratch");
    arrange(&dir, true, stored).expect("arrange");
    let (logs, outcome) = warn_logs_of(&dir);
    outcome.expect_err("the reuse must fail");
    let event = sole_reuse_event(&logs).expect("one event");
    assert!(
        event.contains(expected),
        "the event must name the {expected} outcome: {event:?}"
    );
}

/// A success is not logged at warning level.
///
/// The split is what makes `WARN` a usable filter: were the success event a
/// warning too, an operator watching for bootstraps that failed to adopt a
/// password would see every bootstrap that did.
#[rstest]
fn a_successful_reuse_is_not_a_warning(scratch: Result<Scratch>) {
    let dir = scratch.expect("scratch");
    arrange(&dir, true, Some("kept-secret")).expect("arrange");
    let (logs, outcome) = warn_logs_of(&dir);
    outcome.expect("the reuse must succeed");
    assert!(
        reuse_events(&logs).is_empty(),
        "a reuse that succeeded must not warn: {logs:?}"
    );
}

/// The stored password never reaches the log, at either level.
///
/// The event carries the outcome and the locations it read; the secret it
/// read is the one thing that must not be there. A field added later that
/// rendered the password, or an error that echoed the file's contents, would
/// fail here rather than in production.
#[rstest]
fn the_stored_password_is_never_logged(scratch: Result<Scratch>) {
    let dir = scratch.expect("scratch");
    arrange(&dir, true, Some("kept-secret")).expect("arrange");
    let (info, adopted) = info_logs_of(&dir, false);
    adopted.expect("the reuse must succeed");
    let (warnings, repeated) = warn_logs_of(&dir);
    repeated.expect("the reuse must succeed again");
    for line in info.iter().chain(warnings.iter()) {
        assert!(
            !line.contains("kept-secret"),
            "the stored password must never be logged: {line:?}"
        );
    }
}

//! Runs a scoped-environment test body in a child copy of the test binary.
//!
//! These tests exercise the library's own process-environment scope, so they
//! must change the real process environment. Running each body in a child
//! process keeps those changes (and the `ENV_LOCK` they hold) out of the
//! parent test process, so the tests need no `#[serial]` marker and cannot
//! disturb any other test running in parallel.

use std::{env, io::Write, process::Command, thread};

/// Names the variable whose presence makes the test binary act as the child.
const CHILD_MARKER: &str = "ENV_SCOPE_TEST_CHILD";

/// Line the child prints once its body has returned, so the parent can tell a
/// body that ran from a filter that matched no test.
const CHILD_DONE: &str = "ENV_SCOPE_TEST_CHILD: body returned";

/// Run `body` in a child process, or run it here when this is the child.
///
/// In the parent this re-runs the current test, by its full libtest name, in a
/// child process and fails unless the child exits successfully after running
/// `body`; it returns `None`. In the child it runs `body` and returns its
/// result, so an `Err` or a panic fails the child and therefore the parent.
///
/// The parent's own environment must not hold `ENV_SCOPE_TEST_CHILD`: with it
/// set, the body runs in the parent instead of a child.
///
/// # Panics
///
/// Panics in the parent when the child cannot be spawned, fails, or never
/// reaches the end of `body`.
pub(super) fn in_child_process<T>(body: impl FnOnce() -> T) -> Option<T> {
    if env::var_os(CHILD_MARKER).is_some() {
        let result = body();
        let written = writeln!(std::io::stdout().lock(), "{CHILD_DONE}");
        assert!(written.is_ok(), "the child could not report completion");
        return Some(result);
    }
    let current = thread::current();
    let outcome = current
        .name()
        .ok_or_else(|| String::from("the test thread has no name"))
        .and_then(rerun_in_child);
    assert_eq!(outcome, Ok(()), "the child run of the test failed");
    None
}

/// Re-run the test called `name` in a child process with the marker set.
fn rerun_in_child(name: &str) -> Result<(), String> {
    let exe = env::current_exe().map_err(|err| format!("locate the test binary: {err}"))?;
    let output = Command::new(exe)
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, "1")
        .output()
        .map_err(|err| format!("spawn the child for {name}: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && stdout.contains(CHILD_DONE) {
        return Ok(());
    }
    Err(format!(
        "{name} did not confirm completion ({}): {stdout}{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// Full libtest name of the passing test below.
const PASSING_NAME: &str = "env::tests::child::tests::a_passing_child_is_reported_as_success";

#[cfg(test)]
mod tests {
    //! Checks, through `in_child_process` itself, that a panicking child, an
    //! `Err` result and a filter that matches no test each fail the parent.
    //!
    //! Each of the panic and `Err` cases is `#[should_panic]` on the helper's
    //! own message; the unmatched-filter case calls `rerun_in_child` directly,
    //! because the helper derives the test name from its own thread. In
    //! the child the body fails with a different message, so the child fails
    //! too, and the parent's `in_child_process` is what raises the expected
    //! panic; a helper that stopped failing the parent would make the parent
    //! test fail for not panicking.

    use super::*;

    /// A body that returns normally passes: the positive control.
    #[test]
    fn a_passing_child_is_reported_as_success() { in_child_process(|| ()); }

    #[test]
    fn the_control_passes_when_rerun_directly() {
        assert_eq!(rerun_in_child(PASSING_NAME), Ok(()));
    }

    /// The helper, not just `rerun_in_child`, names the test thread it reruns.
    #[test]
    fn the_rerun_uses_the_libtest_thread_name() {
        let current = thread::current();
        assert_eq!(
            current.name(),
            Some("env::tests::child::tests::the_rerun_uses_the_libtest_thread_name")
        );
    }

    #[test]
    #[should_panic(expected = "the child run of the test failed")]
    fn a_panicking_child_fails_the_parent() { in_child_process(|| panic!("the body panicked")); }

    #[test]
    #[should_panic(expected = "the child run of the test failed")]
    fn an_err_result_in_the_child_fails_the_parent() {
        let outcome = in_child_process(|| Err::<(), &str>("the body returned an error"));
        // Only the child reaches this line: surface its `Err` as a failure
        // whose message differs from the parent's.
        assert_eq!(outcome, Some(Ok(())), "the body returned an error");
    }

    #[test]
    fn a_filter_that_matches_no_test_is_reported_as_failure() {
        let outcome = rerun_in_child("env::tests::child::tests::no_such_test");
        assert!(outcome.is_err(), "no matching test must fail: {outcome:?}");
    }
}

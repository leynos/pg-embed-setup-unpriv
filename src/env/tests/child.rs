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
        .and_then(|name| rerun_in_child(name, &[]));
    assert_eq!(outcome, Ok(()), "the child run of the test failed");
    None
}

/// Re-run the test called `name` in a child process with the marker set and
/// `extra` added to the child's environment.
fn rerun_in_child(name: &str, extra: &[(&str, &str)]) -> Result<(), String> {
    let exe = env::current_exe().map_err(|err| format!("locate the test binary: {err}"))?;
    let output = Command::new(exe)
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(CHILD_MARKER, "1")
        .envs(extra.iter().copied())
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

/// Names the variable that tells the probe test how to behave in the child.
const PROBE_MODE: &str = "ENV_SCOPE_TEST_PROBE_MODE";

/// Full libtest name of the probe test below.
const PROBE_NAME: &str = "env::tests::child::tests::probe";

#[cfg(test)]
mod tests {
    //! Checks that the helper reports a failing child, an `Err` result and a
    //! filter that matches no test, each as a failure of the parent.

    use super::*;

    /// Test the helper runs in a child: it fails or errs on request.
    ///
    /// Without a mode it passes, so the ordinary run is a positive control.
    #[test]
    fn probe() -> Result<(), String> { in_child_process(probe_outcome).unwrap_or(Ok(())) }

    /// What the probe does in the child, as chosen by `PROBE_MODE`.
    fn probe_outcome() -> Result<(), String> {
        match env::var(PROBE_MODE).as_deref() {
            Ok("panic") => {
                panic_on_request();
                Ok(())
            }
            Ok("err") => Err(String::from("probe asked to return an error")),
            _ => Ok(()),
        }
    }

    /// Panics, as the probe is asked to in the `panic` mode.
    fn panic_on_request() { panic!("probe asked to panic") }

    #[test]
    fn a_passing_child_is_reported_as_success() {
        assert_eq!(rerun_in_child(PROBE_NAME, &[]), Ok(()));
    }

    #[test]
    fn a_panicking_child_is_reported_as_failure() {
        let outcome = rerun_in_child(PROBE_NAME, &[(PROBE_MODE, "panic")]);
        assert!(outcome.is_err(), "a panicking child must fail: {outcome:?}");
    }

    #[test]
    fn an_err_result_in_the_child_is_reported_as_failure() {
        let outcome = rerun_in_child(PROBE_NAME, &[(PROBE_MODE, "err")]);
        assert!(outcome.is_err(), "an Err child must fail: {outcome:?}");
    }

    #[test]
    fn a_filter_that_matches_no_test_is_reported_as_failure() {
        let outcome = rerun_in_child("env::tests::child::tests::no_such_test", &[]);
        assert!(outcome.is_err(), "no matching test must fail: {outcome:?}");
    }
}

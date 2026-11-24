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
    let outcome = rerun_in_child();
    assert_eq!(outcome, Ok(()), "the child run of the test failed");
    None
}

/// Re-run the current test in a child process with the marker set.
fn rerun_in_child() -> Result<(), String> {
    let current = thread::current();
    let name = current
        .name()
        .ok_or_else(|| String::from("the test thread has no name"))?;
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

//! Regression coverage for the Quick start password guard.
//!
//! The user guide asks readers to supply a strong, unique `PG_PASSWORD` before
//! copying the configuration block, and the block refuses an unset or empty
//! value without echoing it. These tests extract that block from
//! `docs/users-guide.md` and run it, so a later edit cannot silently drop the
//! guard, the export, or the trace-safe sentinel check. A child process
//! confirms that the documented export actually hands the password on, which
//! an inherited environment variable alone would not prove.
//!
//! The snippet is Bash, so the tests run only on Unix hosts.

#![cfg(unix)]

use std::process::{Command, Output};

const USER_GUIDE: &str = include_str!("../docs/users-guide.md");
const GUARD_MARKER: &str = "[ -z \"${PG_PASSWORD:+set}\" ]";
/// The diagnostic Bash emits for the documented `:?` expansion.
///
/// Bash prefixes a `${PG_PASSWORD:?word}` failure with the variable name and a
/// colon, so the expected text begins `PG_PASSWORD: `. The format is stable
/// across supported shells; a mismatch here means either the guard text or
/// that shell convention changed, not that the test is flaky.
const MISSING_MESSAGE: &str =
    "PG_PASSWORD: Set PG_PASSWORD externally to a strong, unique password";
const SECRET: &str = "quick-start-secret";

/// A child process that reports whether it inherited the documented password.
///
/// The child compares the inherited value with the expected one instead of
/// printing it, so the test cannot pass by losing the secret to its own
/// captured output.
const CHILD_CHECK: &str = concat!(
    "bash -c 'if [ \"${PG_PASSWORD:-}\" = \"${EXPECTED_PASSWORD:-}\" ]; then ",
    "printf \"CHILD-MATCH\\n\"; else printf \"CHILD-NOMATCH\\n\"; fi'"
);

/// Extracts the configuration fence that contains the password guard.
///
/// Locating the fence by the guard marker rather than by line number keeps the
/// test working when prose around the example changes, while a removed guard
/// fails the search and therefore the test.
fn configuration_block() -> String {
    USER_GUIDE
        .split("```bash\n")
        .skip(1)
        .filter_map(|chunk| chunk.split("```").next())
        .find(|block| block.contains(GUARD_MARKER))
        .expect("the guide contains the password guard in a bash fence")
        .to_owned()
}

/// Runs `script` with `PG_PASSWORD` either set to `password` or removed.
fn run(script: &str, password: Option<&str>) -> Output {
    let mut command = Command::new("bash");
    command.arg("-c").arg(script);
    match password {
        Some(value) => command.env("PG_PASSWORD", value),
        None => command.env_remove("PG_PASSWORD"),
    };
    command.output().expect("failed to invoke bash")
}

/// Runs `script` as a shell that has not yet exported `PG_PASSWORD`.
///
/// `EXPECTED_PASSWORD` carries the secret so the script can create an
/// unexported `PG_PASSWORD`, exactly as `read -rs PG_PASSWORD` does in the
/// guide. That makes the documented `export PG_PASSWORD` line the only reason
/// a child process can see the value.
fn run_before_export(script: &str) -> Output {
    let mut command = Command::new("bash");
    command.arg("-c").arg(script);
    command.env("EXPECTED_PASSWORD", SECRET);
    command.env_remove("PG_PASSWORD");
    command.output().expect("failed to invoke bash")
}

#[test]
fn missing_or_empty_password_fails_and_aborts() {
    let script = format!("{}\nprintf 'REACHED-END\\n'", configuration_block());

    for (case, password) in [("unset", None), ("empty", Some(""))] {
        let output = run(&script, password);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        assert!(!output.status.success(), "{case}: the guard succeeded");
        assert!(
            stderr.contains(MISSING_MESSAGE),
            "{case}: the guard reported unexpected diagnostics:\n{stderr}",
        );
        assert!(
            !stdout.contains("REACHED-END"),
            "{case}: execution continued past the guard",
        );
    }
}

#[test]
fn documented_export_hands_the_password_to_a_child_process() {
    let script = format!(
        "PG_PASSWORD=${{EXPECTED_PASSWORD}}\n{}\n{}",
        configuration_block(),
        CHILD_CHECK,
    );

    let output = run_before_export(&script);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "the guard rejected a supplied password:\n{stderr}",
    );
    assert!(
        stdout.contains("CHILD-MATCH") && !stdout.contains("CHILD-NOMATCH"),
        "the documented export did not hand the password to a child process:\n{stdout}",
    );
    assert!(
        !stdout.contains(SECRET) && !stderr.contains(SECRET),
        "the configuration block printed the password",
    );
}

#[test]
fn tracing_does_not_expose_the_password() {
    let script = format!("set -x\n{}", configuration_block());
    let output = run(&script, Some(SECRET));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "the guard failed under `set -x`");
    assert!(
        !stdout.contains(SECRET) && !stderr.contains(SECRET),
        "the trace exposed the password:\n{stderr}",
    );
    assert!(
        stderr.contains("-z set"),
        "the trace did not show the sentinel test:\n{stderr}",
    );
}

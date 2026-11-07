//! Regression coverage for the Quick start password guard.
//!
//! The user guide asks readers to supply a strong, unique `PG_PASSWORD` before
//! copying the configuration block, and the block refuses an unset or empty
//! value without echoing it. These tests extract that block from
//! `docs/users-guide.md` and run it, so a later edit cannot silently drop the
//! guard, the export, or the trace-safe sentinel check.
//!
//! The snippet is Bash, so the tests run only on Unix hosts.

#![cfg(unix)]

use std::process::{Command, Output};

const USER_GUIDE: &str = include_str!("../docs/users-guide.md");
const GUARD_MARKER: &str = "[ -z \"${PG_PASSWORD:+set}\" ]";
const MISSING_MESSAGE: &str =
    "PG_PASSWORD: Set PG_PASSWORD externally to a strong, unique password";
const SECRET: &str = "quick-start-secret";

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
fn supplied_password_is_exported_without_being_printed() {
    let output = run(&configuration_block(), Some(SECRET));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "the guard rejected a supplied password:\n{stderr}",
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

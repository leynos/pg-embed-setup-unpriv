//! Regression coverage for the Quick start password guard.
//!
//! The user guide asks readers to supply a strong, unique `PG_PASSWORD` before
//! copying the configuration block, and the block refuses an unset or empty
//! value without echoing it. These tests extract that block from
//! `docs/users-guide.md` and run it, so a later edit cannot silently drop the
//! guard, the export, or the trace-safe sentinel check. A child process
//! confirms that the documented export actually hands the password on, which
//! an inherited environment variable alone would not prove. A further test
//! feeds the documented silent read a password containing spaces and
//! backslashes and checks it reaches a child unchanged, so a weakened read
//! cannot silently mangle a password.
//!
//! The snippet is Bash, so the tests run only on Unix hosts.

#![cfg(unix)]

use std::{
    io::Write as _,
    process::{Command, Output, Stdio},
};

const USER_GUIDE: &str = include_str!("../docs/users-guide.md");
const GUARD_MARKER: &str = "[ -z \"${PG_PASSWORD:+set}\" ]";
/// The documented password-entry line, which the guide must keep verbatim.
///
/// `IFS= read -rs` is what preserves surrounding whitespace and backslashes
/// while suppressing terminal echo. The fence is located by this exact line, so
/// a weakened form such as `read PG_PASSWORD` fails the lookup rather than
/// passing a weaker contract.
const READ_MARKER: &str = "IFS= read -rs PG_PASSWORD";
/// The diagnostic Bash emits for the documented `:?` expansion.
///
/// Bash prefixes a `${PG_PASSWORD:?word}` failure with the variable name and a
/// colon, so the expected text begins `PG_PASSWORD: `. The format is stable
/// across supported shells; a mismatch here means either the guard text or
/// that shell convention changed, not that the test is flaky.
const MISSING_MESSAGE: &str =
    "PG_PASSWORD: Set PG_PASSWORD externally to a strong, unique password";
const SECRET: &str = "quick-start-secret";

/// A password with the characters a careless read would alter.
const TRICKY_SECRET: &str = "  spaced secret \\ with backslashes  ";

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

/// Extracts the documented password-entry fence.
///
/// The guide keeps that fence separate from the configuration fence so an
/// existing cluster can skip it; locating it by `READ_MARKER` keeps the test
/// honest when the prose around it changes.
fn read_block() -> String {
    USER_GUIDE
        .split("```bash\n")
        .skip(1)
        .filter_map(|chunk| chunk.split("```").next())
        .find(|block| block.contains(READ_MARKER))
        .expect("the guide contains the password read in a bash fence")
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

/// Runs `script` with `stdin` fed to it as terminal input would be.
///
/// Nothing is exposed through the environment except the expected value, so
/// the only source of `PG_PASSWORD` is the documented read under test.
fn run_with_stdin(script: &str, stdin: &str) -> Output {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("EXPECTED_PASSWORD", stdin)
        .env_remove("PG_PASSWORD")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn bash");
    child
        .stdin
        .take()
        .expect("bash stdin is piped")
        .write_all(format!("{stdin}\n").as_bytes())
        .expect("failed to write bash stdin");
    child
        .wait_with_output()
        .expect("failed to collect bash output")
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

/// Proves the documented read hands the password on byte for byte.
///
/// The read fence runs first, so `PG_PASSWORD` comes only from `stdin`. The
/// configuration fence then exports it, and the child reports whether the
/// value it inherited still matches. Surrounding whitespace or a backslash
/// would be altered by a plain `read`, so `CHILD-NOMATCH` catches a weakened
/// read. The captured streams must not contain the password either, which
/// rules out the entry step printing what it read.
#[test]
fn documented_read_preserves_the_password_verbatim() {
    let script = format!(
        "{}\n{}\n{}",
        read_block(),
        configuration_block(),
        CHILD_CHECK,
    );
    let output = run_with_stdin(&script, TRICKY_SECRET);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "the documented read and guard failed:\n{stderr}",
    );
    assert!(
        stdout.contains("CHILD-MATCH") && !stdout.contains("CHILD-NOMATCH"),
        "the documented read changed the password before a child saw it:\n{stdout}",
    );
    assert!(
        !stdout.contains(TRICKY_SECRET) && !stderr.contains(TRICKY_SECRET),
        "the password entry printed the password:\n{stdout}\n{stderr}",
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

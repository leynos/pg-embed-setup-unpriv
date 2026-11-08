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

/// A child process that reports whether it inherited the documented password.
///
/// The child compares the inherited value with the expected one instead of
/// printing it, so the test cannot pass by losing the secret to its own
/// captured output.
const CHILD_CHECK: &str = concat!(
    "bash -c 'if [ \"${PG_PASSWORD:-}\" = \"${EXPECTED_PASSWORD:-}\" ]; then ",
    "printf \"CHILD-MATCH\\n\"; else printf \"CHILD-NOMATCH\\n\"; fi'"
);

/// A password exercised by these tests.
///
/// Keeping the value in one type means every runner and assertion names a
/// secret rather than an undifferentiated string.
#[derive(Clone, Copy)]
struct Secret(&'static str);

impl Secret {
    /// The value handed to the shell.
    fn value(self) -> &'static str { self.0 }
}

/// The password the documented export must hand on.
const SECRET: Secret = Secret("quick-start-secret");

/// A password with the characters a careless read would alter.
const TRICKY_SECRET: Secret = Secret("  spaced secret \\ with backslashes  ");

/// A failure label for the shared output assertions.
#[derive(Clone, Copy)]
struct Context(&'static str);

/// A Bash script assembled from the documented fences.
struct Script(String);

impl Script {
    /// The script text handed to `bash -c`.
    fn text(&self) -> &str { &self.0 }
}

/// Finds the first Bash fence in the guide that contains `marker`.
///
/// Locating fences by marker rather than by line number keeps the tests
/// working when prose around the examples changes, while a removed marker
/// fails the search and therefore the test. `missing` names what was looked
/// for in the failure message.
fn bash_fence(marker: &str, missing: Context) -> Script {
    USER_GUIDE
        .split("```bash\n")
        .skip(1)
        .filter_map(|chunk| chunk.split("```").next())
        .find(|block| block.contains(marker))
        .map_or_else(
            || panic!("the guide contains {} in a bash fence", missing.0),
            |block| Script(block.to_owned()),
        )
}

/// Extracts the configuration fence that contains the password guard.
fn configuration_block() -> Script { bash_fence(GUARD_MARKER, Context("the password guard")) }

/// Extracts the documented password-entry fence.
///
/// The guide keeps that fence separate from the configuration fence so an
/// existing cluster can skip it.
fn read_block() -> Script { bash_fence(READ_MARKER, Context("the password read")) }

/// The documented export followed by the child check.
fn export_script() -> Script {
    Script(format!(
        "PG_PASSWORD=${{EXPECTED_PASSWORD}}\n{}\n{}",
        configuration_block().text(),
        CHILD_CHECK,
    ))
}

/// The documented read, then the export and child check.
fn read_script() -> Script {
    Script(format!(
        "{}\n{}\n{}",
        read_block().text(),
        configuration_block().text(),
        CHILD_CHECK,
    ))
}

/// Builds a Bash command for `script` with `PG_PASSWORD` removed.
///
/// Each runner adds only the variable its case needs, so a child can see the
/// password only through the documented export.
fn bash(script: &Script) -> Command {
    let mut command = Command::new("bash");
    command.arg("-c").arg(script.text());
    command.env_remove("PG_PASSWORD");
    command
}

/// Runs `script` with `PG_PASSWORD` either set to `password` or removed.
fn run(script: &Script, password: Option<Secret>) -> Output {
    let mut command = bash(script);
    if let Some(secret) = password {
        command.env("PG_PASSWORD", secret.value());
    }
    command.output().expect("failed to invoke bash")
}

/// Runs `script` as a shell that has not yet exported `PG_PASSWORD`.
///
/// `EXPECTED_PASSWORD` carries the secret so the script can create an
/// unexported `PG_PASSWORD`, exactly as `read -rs PG_PASSWORD` does in the
/// guide. That makes the documented `export PG_PASSWORD` line the only reason
/// a child process can see the value.
fn run_before_export(script: &Script) -> Output {
    bash(script)
        .env("EXPECTED_PASSWORD", SECRET.value())
        .output()
        .expect("failed to invoke bash")
}

/// Runs `script` with `stdin` fed to it as terminal input would be.
///
/// Nothing is exposed through the environment except the expected value, so
/// the only source of `PG_PASSWORD` is the documented read under test.
fn run_with_stdin(script: &Script, stdin: Secret) -> Output {
    let mut child = bash(script)
        .env("EXPECTED_PASSWORD", stdin.value())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn bash");
    child
        .stdin
        .take()
        .expect("bash stdin is piped")
        .write_all(format!("{}\n", stdin.value()).as_bytes())
        .expect("failed to write bash stdin");
    child
        .wait_with_output()
        .expect("failed to collect bash output")
}

/// Asserts that a handoff handed `secret` on without printing it.
///
/// `what` names the scenario, so a failure says which documented path broke.
fn assert_handoff(output: &Output, secret: Secret, what: Context) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{}: the guard rejected the password:\n{stderr}",
        what.0,
    );
    assert!(
        stdout.contains("CHILD-MATCH") && !stdout.contains("CHILD-NOMATCH"),
        "{}: the password did not reach the child unchanged:\n{stdout}",
        what.0,
    );
    assert_streams_hide(output, secret, what);
}

/// Asserts that neither captured stream printed `secret`.
fn assert_streams_hide(output: &Output, secret: Secret, context: Context) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stdout.contains(secret.value()) && !stderr.contains(secret.value()),
        "{}: the password was printed:\n{stdout}\n{stderr}",
        context.0,
    );
}

#[test]
fn missing_or_empty_password_fails_and_aborts() {
    let script = Script(format!(
        "{}\nprintf 'REACHED-END\\n'",
        configuration_block().text(),
    ));

    for (case, password) in [("unset", None), ("empty", Some(Secret("")))] {
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
    let output = run_before_export(&export_script());
    assert_handoff(&output, SECRET, Context("the documented export"));
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
    let output = run_with_stdin(&read_script(), TRICKY_SECRET);
    assert_handoff(&output, TRICKY_SECRET, Context("the documented read"));
}

#[test]
fn tracing_does_not_expose_the_password() {
    let script = Script(format!("set -x\n{}", configuration_block().text()));
    let output = run(&script, Some(SECRET));
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "the guard failed under `set -x`");
    assert_streams_hide(&output, SECRET, Context("the trace"));
    assert!(
        stderr.contains("-z set"),
        "the trace did not show the sentinel test:\n{stderr}",
    );
}

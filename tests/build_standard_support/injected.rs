//! The policy checks fed canned `make -n` text through the injected runner, so no
//! process runs. The real-`make` tests live in the contract file; these cover the
//! parsing and policy path, and run a synthetic held-out target so the check fires
//! in a repository that defines none.

use std::fmt::Write as _;

use super::{
    config::{Pin, THREADS_FLAG},
    development::{Tools, development_problems_expecting},
    make::{Host, MakeRunner, Target},
};

/// Renders canned `make -n` text for a fake runner through a fallible writer, so
/// the fakes keep the runner's `Result` shape honestly.
pub(super) fn canned(text: std::fmt::Arguments) -> Result<String, String> {
    let mut out = String::new();
    out.write_fmt(text).map_err(|error| error.to_string())?;
    Ok(out)
}

/// Returns the linker flag the standard adds on a host: mold on Linux, nothing elsewhere.
pub(super) const fn linker_flag(host: Host) -> &'static str {
    if host.takes_linker_flag() {
        " -Clink-arg=-fuse-ld=mold"
    } else {
        ""
    }
}

/// Defines a fake runner that prints the same canned text for every target and host.
macro_rules! fake_make {
    ($name:ident, $text:expr) => {
        fn $name(_target: Target<'_>, _host: Host) -> Result<String, String> {
            canned(format_args!("{}\n", $text))
        }
    };
}
pub(super) use fake_make;

/// Renders a command that assigns what a compliant recipe assigns on a host, followed by any lines
/// after it.
fn compliant_text(host: Host, commands: &str) -> String {
    let linker = linker_flag(host);
    format!(
        "RUSTFLAGS=\"${{RUSTFLAGS:+$RUSTFLAGS }}-D warnings {THREADS_FLAG}{linker}\" {commands}"
    )
}

/// Defines a fake runner whose first command assigns what a compliant recipe assigns on the host.
macro_rules! compliant_make {
    ($name:ident, $commands:expr) => {
        fn $name(_target: Target<'_>, host: Host) -> Result<String, String> {
            canned(format_args!("{}\n", compliant_text(host, $commands)))
        }
    };
}

// A fake runner: a compliant `make -n` for any target, with no process behind it.
compliant_make!(compliant_make, "cargo test");

// A fake runner whose recipes run only a metadata probe, no build, test or lint tool.
fake_make!(probe_only_make, "cargo metadata --format-version 1");

// A fake runner whose every recipe runs an assigned version probe and nothing else.
compliant_make!(assigned_probe_make, "cargo nextest --version");

// A fake runner whose command loses the caller's `RUSTFLAGS`.
fake_make!(dropping_make, "RUSTFLAGS=\"-D warnings\" cargo test");

// A fake runner whose test command assigns nothing, so the warning policy never reaches it.
fake_make!(bare_test_make, "cargo test");

// A fake runner whose lint commands assign nothing beside a command that does.
compliant_make!(
    bare_lint_make,
    "cargo test\ncargo clippy --all-targets\nwhitaker --all"
);

// A fake runner with commands that run no compiled code under test, beside one that assigns.
compliant_make!(
    exempt_commands_make,
    "cargo test\ncargo fmt --all --check\ncargo metadata --format-version 1\nRUSTDOCFLAGS=\"-D \
     warnings\" cargo doc"
);

/// A fake runner whose `lint` target runs Clippy and not Whitaker, and whose other targets run
/// tests.
fn lint_without_whitaker_make(target: Target<'_>, host: Host) -> Result<String, String> {
    let linker = linker_flag(host);
    let tool = if target.name() == "lint" {
        "cargo clippy --all-targets"
    } else {
        "cargo test"
    };
    canned(format_args!(
        "RUSTFLAGS=\"${{RUSTFLAGS:+$RUSTFLAGS }}-D warnings {THREADS_FLAG}{linker}\" {tool}\n"
    ))
}

/// A fake runner whose chained recipe leaves its second command bare beside a probe that assigns.
fn chained_bare_make(_target: Target<'_>, host: Host) -> Result<String, String> {
    let linker = linker_flag(host);
    canned(format_args!(
        "if RUSTFLAGS=\"${{RUSTFLAGS:+$RUSTFLAGS }}-D warnings {THREADS_FLAG}{linker}\" cargo \
         nextest --version; then cargo nextest run; else echo \"falling back to cargo test\"; \
         RUSTFLAGS=\"${{RUSTFLAGS:+$RUSTFLAGS }}-D warnings {THREADS_FLAG}{linker}\" cargo test; \
         fi\n"
    ))
}

/// A fake runner for a target that is not defined.
pub(super) fn undefined_make(target: Target<'_>, _host: Host) -> Result<String, String> {
    Err(format!(
        "`make -n {}` failed, so it is not defined",
        target.name()
    ))
}

/// Turns a failed expectation into the error a test returns.
pub(super) fn ensure(holds: bool, message: &str) -> Result<(), String> {
    if holds {
        Ok(())
    } else {
        Err(message.to_owned())
    }
}

/// Scenario: the development policy fed canned `make -n` text through the injected runner, so
/// no process runs.
///
/// Invariant: a compliant command raises no complaint on either host, a command that drops the
/// caller's flags raises one, and a runner error reaches the caller instead of being read as an
/// empty output.
#[test]
fn the_development_policy_runs_against_an_injected_runner() -> Result<(), String> {
    let pin = Pin::Nightly;
    for host in [Host::Linux, Host::Darwin] {
        let (problems, read) = development_problems_expecting(compliant_make, host, pin, &[])?;
        ensure(
            problems.is_empty(),
            &format!("a compliant fake raised {problems:?}"),
        )?;
        ensure(read > 0, "the fake's commands were not read")?;
    }
    let (dropped, _) = development_problems_expecting(dropping_make, Host::Linux, pin, &[])?;
    ensure(
        !dropped.is_empty(),
        "a command that drops the caller's flags passed",
    )?;
    ensure(
        development_problems_expecting(undefined_make, Host::Linux, pin, &[]).is_err(),
        "a runner error was swallowed",
    )
}

/// Scenario: development recipes with commands that assign no `RUSTFLAGS`, chained and alone,
/// beside commands that need none.
///
/// Invariant: a build, test or lint command that assigns nothing is refused wherever it sits,
/// including beside a probe that does assign; a formatter, a metadata probe and a documentation
/// build are not refused.
#[test]
fn a_development_command_without_an_assignment_is_refused_and_an_exempt_one_is_not()
-> Result<(), String> {
    let pin = Pin::Nightly;
    let (bare_test, _) = development_problems_expecting(bare_test_make, Host::Linux, pin, &[])?;
    ensure(
        !bare_test.is_empty(),
        "a test command that assigns no RUSTFLAGS passed",
    )?;
    let (bare_lint, _) = development_problems_expecting(bare_lint_make, Host::Linux, pin, &[])?;
    ensure(
        !bare_lint.is_empty(),
        "lint commands that assign no RUSTFLAGS passed",
    )?;
    let (chained, _) = development_problems_expecting(chained_bare_make, Host::Linux, pin, &[])?;
    ensure(
        !chained.is_empty()
            && chained
                .iter()
                .all(|problem| problem.contains("nextest run")),
        &format!("a bare command chained beside an assigning probe raised {chained:?}"),
    )?;
    let (exempt, exempt_read) =
        development_problems_expecting(exempt_commands_make, Host::Linux, pin, &[])?;
    ensure(
        exempt.is_empty() && exempt_read > 0,
        &format!("a formatter, probe or doc build raised {exempt:?}"),
    )
}

/// Scenario: development recipes whose only command is a metadata probe, or an assigned version
/// probe.
///
/// Invariant: each development target must run a tool of its own, so a target that only probes is
/// refused instead of hiding behind the assignments another target supplies, and an assignment is
/// no tool: a probe that carries `RUSTFLAGS` does not stand in for the build, test or lint command.
#[test]
fn a_development_target_that_runs_no_tool_is_refused() -> Result<(), String> {
    for runner in [probe_only_make as MakeRunner, assigned_probe_make] {
        let (problems, _) = development_problems_expecting(runner, Host::Linux, Pin::Nightly, &[])?;
        ensure(
            problems
                .iter()
                .any(|problem| problem.contains("runs no build, test or lint command")),
            "a target that only probes passed",
        )?;
    }
    Ok(())
}

/// Scenario: a record of the tools each target runs, against recipes that keep or drop one.
///
/// Invariant: a target that stops running a recorded tool is refused by name, one lint command does
/// not stand in for the other, and a target that keeps every recorded tool raises nothing.
#[test]
fn a_target_that_stops_running_a_recorded_tool_is_refused() -> Result<(), String> {
    let recorded = [
        Tools {
            target: "test",
            keys: &["cargo test"],
        },
        Tools {
            target: "lint",
            keys: &["cargo clippy", "whitaker"],
        },
    ];
    let (problems, _) = development_problems_expecting(
        lint_without_whitaker_make,
        Host::Linux,
        Pin::Nightly,
        &recorded,
    )?;
    ensure(
        problems.iter().any(|problem| {
            problem.contains("`make lint`") && problem.contains("no longer runs `whitaker`")
        }),
        "a lint target without Whitaker passed",
    )?;
    ensure(
        !problems
            .iter()
            .any(|problem| problem.contains("`cargo clippy`")),
        "a kept tool was reported missing",
    )?;
    let kept = [
        Tools {
            target: "test",
            keys: &["cargo test"],
        },
        Tools {
            target: "lint",
            keys: &["cargo clippy"],
        },
    ];
    let (clean, _) = development_problems_expecting(
        lint_without_whitaker_make,
        Host::Linux,
        Pin::Nightly,
        &kept,
    )?;
    ensure(
        clean
            .iter()
            .all(|problem| !problem.contains("no longer runs")),
        "a kept tool was reported missing",
    )
}

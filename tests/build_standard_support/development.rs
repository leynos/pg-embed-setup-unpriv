//! The development half of the Makefile reader: the commands `make -n` prints for each development
//! target, judged against a toolchain pin and a host. Each command must assign `RUSTFLAGS` with the
//! standard flags and keep the caller's own, the commands that run tests must keep `-D warnings`,
//! and each target must run the tools the contract recorded for it.

use super::{
    config::{Pin, Problems},
    make::{Assignment, Command, Host, MakeRunner, Target, make_commands},
    shell::{compiles, names_program},
};

/// Makefile targets that build for development. A command in one either assigns
/// `RUSTFLAGS` with the standard flags or assigns none and so takes the
/// configuration's. The list is this repository's own, and a target that stops
/// being defined fails the contract rather than dropping out of it.
const DEVELOPMENT_TARGETS: &[&str] = &[
    "test",
    "typecheck",
    "lint",
    "build",
    "test-doc",
    "test-loom",
];
/// The tools each development target runs, as recorded when the contract was written: for example
/// `cargo build`, `cargo clippy`, `cargo nextest run`, `whitaker`. A target that stops running one
/// of them fails the contract, so a version probe cannot replace `cargo build` and one lint command
/// cannot replace the other required ones.
const EXPECTED_TOOLS: &[Tools] = &[
    Tools {
        target: "test",
        keys: &["cargo nextest run", "cargo test"],
    },
    Tools {
        target: "typecheck",
        keys: &["cargo check"],
    },
    Tools {
        target: "lint",
        keys: &["cargo clippy", "whitaker"],
    },
    Tools {
        target: "build",
        keys: &["cargo build"],
    },
    Tools {
        target: "test-doc",
        keys: &["cargo test"],
    },
    Tools {
        target: "test-loom",
        keys: &["cargo test"],
    },
];

/// The tools a development target is recorded as running, such as `cargo build` or `whitaker`.
#[derive(Clone, Copy)]
pub struct Tools {
    pub target: &'static str,
    pub keys: &'static [&'static str],
}

/// Returns the complaint about a development command that assigns no `RUSTFLAGS`.
fn bare_problem(target: Target<'_>, host: Host, command: &str) -> String {
    format!(
        "`make {target}` on {} runs `{command}` without assigning RUSTFLAGS",
        host.make_value()
    )
}

/// Returns the complaint about one development command, if any: an assigned
/// `RUSTFLAGS` keeps the caller's own flags and restates the frontend flag on a
/// nightly pin, and mold on Linux.
pub fn development_problem(
    target: Target<'_>,
    host: Host,
    pin: Pin,
    assignment: &Assignment,
) -> Option<String> {
    let (flags, inherits) = match assignment {
        Assignment::Flags(flags, inherits) => (flags, inherits),
        Assignment::Bare(command) => return Some(bare_problem(target, host, command)),
        Assignment::Unassigned => return None,
    };
    if !inherits {
        return Some(format!(
            "`make {target}` on {} drops the caller's RUSTFLAGS",
            host.make_value()
        ));
    }
    let reason = flags.meets(pin, host.takes_linker_flag()).err()?;
    Some(format!("`make {target}` on {} {reason}", host.make_value()))
}

/// Returns whether a command line runs tests: `cargo test` or `cargo nextest run`, past any
/// `+toolchain` and option words. A version probe (`cargo nextest --version`) and a build run none.
///
/// ```text
/// runs_tests("cargo nextest run --all-targets") == true
/// runs_tests("cargo +nightly test --doc")       == true
/// runs_tests("cargo nextest --version")         == false
/// ```
pub fn runs_tests(command: &str) -> bool {
    let mut words = command
        .split_whitespace()
        .skip_while(|word| !names_program(word, "cargo"))
        .skip(1);
    let mut subcommand = words.find(|word| !word.starts_with('+') && !word.starts_with('-'));
    if subcommand == Some("nextest") {
        subcommand = words.find(|word| !word.starts_with('-'));
        return subcommand == Some("run");
    }
    subcommand == Some("test")
}

/// Returns the complaints about the `test` target's commands: it must run tests, and every command
/// that runs tests must keep `-D warnings`. A version probe or a prerequisite build is not held to
/// it, and the probe keeping the policy cannot stand in for a test command that dropped it.
pub fn test_policy_problems(target: Target<'_>, host: Host, commands: &[Command]) -> Problems {
    if target.name() != "test" {
        return Vec::new();
    }
    let running: Vec<&Command> = commands
        .iter()
        .filter(|command| runs_tests(&command.text))
        .collect();
    if running.is_empty() {
        return vec![format!(
            "`make {target}` on {} runs no test command",
            host.make_value()
        )];
    }
    running
        .into_iter()
        .filter(|command| !matches!(&command.assignment, Assignment::Flags(flags, _) if flags.denies_warnings()))
        .map(|command| format!("`make {target}` on {} runs `{}` without -D warnings", host.make_value(), command.text))
        .collect()
}

/// Returns the tool a command runs, as a short key such as `cargo build`, `cargo nextest run` or
/// `whitaker`, or `None` for a command that builds, tests and lints nothing (a version probe, a
/// formatter).
///
/// ```text
/// tool_key("RUSTFLAGS=\"-D warnings\" cargo +nightly build --release") == Some("cargo build")
/// tool_key("cargo nextest run --all-targets")                          == Some("cargo nextest run")
/// tool_key("cargo nextest --version")                                  == None
/// ```
pub fn tool_key(command: &str) -> Option<String> {
    if !compiles(command) {
        return None;
    }
    let mut words = command
        .split_whitespace()
        .skip_while(|word| !names_program(word, "cargo"))
        .skip(1);
    let Some(subcommand) = words.find(|word| !word.starts_with('+') && !word.starts_with('-'))
    else {
        return command
            .split_whitespace()
            .any(|word| names_program(word, "whitaker"))
            .then(|| "whitaker".to_owned());
    };
    if subcommand == "nextest" {
        let action = words
            .find(|word| !word.starts_with('-'))
            .unwrap_or_default();
        return Some(format!("cargo nextest {action}").trim_end().to_owned());
    }
    Some(format!("cargo {subcommand}"))
}

/// Returns the complaints when a development target runs no build, test or lint tool of its own (so
/// another target's commands cannot stand in for it, and an assigned version probe is no tool), or
/// stops running a tool the contract recorded for it.
fn target_tool_problems(
    target: Target<'_>,
    host: Host,
    commands: &[Command],
    expected: &[Tools],
) -> Problems {
    let running: Vec<String> = commands
        .iter()
        .filter_map(|command| tool_key(&command.text))
        .collect();
    let mut problems = Problems::new();
    if running.is_empty() {
        problems.push(format!(
            "`make {target}` on {} runs no build, test or lint command",
            host.make_value()
        ));
    }
    let wanted = expected
        .iter()
        .find(|tools| tools.target == target.name())
        .map_or(&[][..], |tools| tools.keys);
    problems.extend(
        wanted
            .iter()
            .filter(|tool| !running.iter().any(|key| key == **tool))
            .map(|tool| {
                format!(
                    "`make {target}` on {} no longer runs `{tool}`",
                    host.make_value()
                )
            }),
    );
    problems
}

/// Returns every complaint about the development targets on one host, and how
/// many assignments it read, so a test can refuse to pass over nothing.
///
/// # Errors
///
/// Returns the reason when a listed target is not defined or unreadable.
pub fn development_problems(
    runner: MakeRunner,
    host: Host,
    pin: Pin,
) -> Result<(Problems, usize), String> {
    development_problems_expecting(runner, host, pin, EXPECTED_TOOLS)
}

/// Like [`development_problems`], against a given record of the tools each target runs, so a test
/// can name a synthetic record for a fake runner.
///
/// # Errors
///
/// Returns the reason when a listed target is not defined or unreadable.
pub fn development_problems_expecting(
    runner: MakeRunner,
    host: Host,
    pin: Pin,
    expected: &[Tools],
) -> Result<(Problems, usize), String> {
    let mut problems = Vec::new();
    let mut read = 0;
    for name in DEVELOPMENT_TARGETS {
        let target = Target(name);
        let commands = make_commands(runner, target, host)?;
        read += commands
            .iter()
            .filter(|command| matches!(command.assignment, Assignment::Flags(..)))
            .count();
        problems.extend(target_tool_problems(target, host, &commands, expected));
        problems.extend(
            commands
                .iter()
                .filter_map(|command| development_problem(target, host, pin, &command.assignment)),
        );
        problems.extend(test_policy_problems(target, host, &commands));
    }
    Ok((problems, read))
}

//! Readers for the Makefile half of the build standard: the commands `make -n`
//! prints for each development, coverage and release target, judged against a
//! toolchain pin and a host.

use super::{
    config::{Flags, LINKER_FLAG, Pin, Problems, THREADS_FLAG},
    shell::{compiles, shell_commands, without_leading_keywords},
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
/// Makefile targets that measure or ship, so every command assigns `RUSTFLAGS`
/// and none carries a standard flag.
const HELD_OUT_TARGETS: &[&str] = &["release"];

/// The host `make` is told it runs on, through `BUILD_HOST_OS`.
#[derive(Clone, Copy)]
pub enum Host {
    Linux,
    Darwin,
}

impl Host {
    /// Returns the value `uname -s` reports for the host.
    pub const fn make_value(self) -> &'static str {
        match self {
            Self::Linux => "Linux",
            Self::Darwin => "Darwin",
        }
    }

    /// Returns whether the host takes mold, which ships for Linux alone.
    pub const fn takes_linker_flag(self) -> bool { matches!(self, Self::Linux) }
}

/// What one `make -n` command assigns to `RUSTFLAGS`.
#[derive(Debug, PartialEq, Eq)]
pub enum Assignment {
    /// A command that assigns nothing and takes the configuration's flags, and runs no
    /// build, test or lint tool (a formatter, a metadata probe, a documentation build).
    Unassigned,
    /// A build, test or lint command that assigns nothing. Development recipes must
    /// assign `RUSTFLAGS` there, so the caller's flags and the warning policy reach it.
    Bare(String),
    /// An assignment, and whether it keeps the caller's own `RUSTFLAGS`.
    Flags(Flags, bool),
}

/// Reads the `RUSTFLAGS` a `make -n` output line assigns. An unreadable form is
/// an error, because it still replaces the configuration's sources and so must
/// not pass.
///
/// ```text
/// assigned_rustflags("RUSTFLAGS=\"-Zthreads=8\" cargo test") -> Flags(["-Zthreads=8"], inherits: false)
/// assigned_rustflags("RUSTFLAGS=\"${RUSTFLAGS:+$RUSTFLAGS }-Zthreads=8\" cargo test") -> inherits: true
/// assigned_rustflags("cargo test")                           -> Unassigned
/// assigned_rustflags("RUSTFLAGS=-Zthreads=8 cargo test")     -> Err
/// assigned_rustflags("RUSTFLAGS=\"${RUSTFLAGS-}-Zthreads=8\" cargo test") -> Err (glued)
/// ```
///
/// # Errors
///
/// Returns the reason when an assignment is unquoted, unterminated, or glues the
/// caller's flags to the next one.
pub fn assigned_rustflags(line: &str) -> Result<Assignment, String> {
    let Some((_, rest)) = line.split_once("RUSTFLAGS=\"") else {
        if line.contains("RUSTFLAGS=") {
            return Err(format!("unreadable RUSTFLAGS assignment in `{line}`"));
        }
        return Ok(Assignment::Unassigned);
    };
    let (assigned, _) = rest
        .split_once('"')
        .ok_or_else(|| format!("unterminated RUSTFLAGS in `{line}`"))?;
    // The recipes prepend the caller's own flags with these expansions; they are
    // not standard flags. `${RUSTFLAGS-}` adds no separator, so glued to the next
    // word it makes one token with it (`-Dwarnings-Zthreads=8`) and hides the flag.
    let glued = assigned
        .split("${RUSTFLAGS-}")
        .skip(1)
        .any(|after| !after.is_empty() && !after.starts_with(' '));
    if glued {
        return Err(format!(
            "inherited RUSTFLAGS glued to the next flag in `{line}`"
        ));
    }
    let inherits =
        assigned.contains("${RUSTFLAGS:+$RUSTFLAGS }") || assigned.contains("${RUSTFLAGS-}");
    let own = assigned
        .replace("${RUSTFLAGS:+$RUSTFLAGS }", " ")
        .replace("${RUSTFLAGS-}", " ");
    Ok(Assignment::Flags(
        Flags::from_words(own.split_whitespace()),
        inherits,
    ))
}

/// Reads the assignment of each cargo or whitaker command `make -n` printed. A line that chains
/// commands is read one command at a time.
///
/// # Errors
///
/// Returns the reason when a command assigns `RUSTFLAGS` in an unreadable form.
pub fn commands_from(stdout: &str) -> Result<Vec<Assignment>, String> {
    Ok(commands_with_text(stdout)?
        .into_iter()
        .map(|(_, assignment)| assignment)
        .collect())
}

/// Reads each cargo or whitaker command `make -n` printed with the command's own text, so a policy
/// can attach to the command it governs.
///
/// # Errors
///
/// Returns the reason when a command assigns `RUSTFLAGS` in an unreadable form.
pub fn commands_with_text(stdout: &str) -> Result<Vec<(String, Assignment)>, String> {
    // A recipe continued with a trailing backslash is one logical line.
    let joined = stdout.replace("\\\n", " ");
    joined
        .lines()
        .flat_map(shell_commands)
        .map(|command| without_leading_keywords(&command).to_owned())
        .filter(|command| !command.starts_with("echo"))
        // A tool-availability probe names Cargo but runs no build.
        .filter(|command| !command.starts_with("command -v"))
        .filter(|command| command.contains("cargo") || command.contains("whitaker"))
        .map(|command| {
            let assignment = match assigned_rustflags(&command)? {
                Assignment::Unassigned if compiles(&command) => Assignment::Bare(command.clone()),
                other => other,
            };
            Ok((command, assignment))
        })
        .collect()
}

/// A Makefile target name, typed so a runner takes a target and not just any text.
#[derive(Clone, Copy, Debug)]
pub struct Target<'a>(pub &'a str);

impl<'a> Target<'a> {
    /// Returns the target's name.
    ///
    /// # Parameters
    ///
    /// - `self`: the target.
    ///
    /// # Returns
    ///
    /// The Makefile target's name, as the text `make` takes.
    pub const fn name(self) -> &'a str { self.0 }
}

/// Runs `make -n` for a target on a host and returns what it printed. The tests
/// that drive real `make` use `real_make` from the process module; a test of the
/// parsing path passes a function that returns canned text instead, so no process runs.
pub type MakeRunner = fn(Target<'_>, Host) -> Result<String, String>;

/// Reads the commands a runner reports for a target on a host, with their text.
fn make_commands(
    runner: MakeRunner,
    target: &str,
    host: Host,
) -> Result<Vec<(String, Assignment)>, String> {
    commands_with_text(&runner(Target(target), host)?)
}

/// Returns the complaint about a development command that assigns no `RUSTFLAGS`.
fn bare_problem(target: &str, host: Host, command: &str) -> String {
    format!(
        "`make {target}` on {} runs `{command}` without assigning RUSTFLAGS",
        host.make_value()
    )
}

/// Returns the complaint about one development command, if any: an assigned
/// `RUSTFLAGS` keeps the caller's own flags and restates the frontend flag on a
/// nightly pin, and mold on Linux.
pub fn development_problem(
    target: &str,
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
        .skip_while(|word| *word != "cargo" && !word.ends_with("/cargo"))
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
pub fn test_policy_problems(
    target: &str,
    host: Host,
    commands: &[(String, Assignment)],
) -> Problems {
    if target != "test" {
        return Vec::new();
    }
    let running: Vec<&(String, Assignment)> = commands
        .iter()
        .filter(|(text, _)| runs_tests(text))
        .collect();
    if running.is_empty() {
        return vec![format!(
            "`make {target}` on {} runs no test command",
            host.make_value()
        )];
    }
    running
        .into_iter()
        .filter(|(_, assignment)| !matches!(assignment, Assignment::Flags(flags, _) if flags.denies_warnings()))
        .map(|(text, _)| format!("`make {target}` on {} runs `{text}` without -D warnings", host.make_value()))
        .collect()
}

/// Returns the complaint when a development target runs no build, test or lint command of its own,
/// so another target's commands cannot stand in for it.
fn empty_target_problem(
    target: &str,
    host: Host,
    commands: &[(String, Assignment)],
) -> Option<String> {
    let runs_a_tool = commands
        .iter()
        .any(|(_, assignment)| !matches!(assignment, Assignment::Unassigned));
    (!runs_a_tool).then(|| {
        format!(
            "`make {target}` on {} runs no build, test or lint command",
            host.make_value()
        )
    })
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
    let mut problems = Vec::new();
    let mut read = 0;
    for target in DEVELOPMENT_TARGETS {
        let commands = make_commands(runner, target, host)?;
        read += commands
            .iter()
            .filter(|(_, command)| matches!(command, Assignment::Flags(..)))
            .count();
        problems.extend(empty_target_problem(target, host, &commands));
        problems.extend(
            commands
                .iter()
                .filter_map(|(_, command)| development_problem(target, host, pin, command)),
        );
        problems.extend(test_policy_problems(target, host, &commands));
    }
    Ok((problems, read))
}

/// Returns every complaint about one held-out command: it assigns nothing, so
/// it takes the configuration's flags, or the assignment names a standard flag.
fn held_out_command_problems(target: &str, assignment: &Assignment) -> Problems {
    let (flags, inherits) = match assignment {
        Assignment::Flags(flags, inherits) => (flags, *inherits),
        Assignment::Bare(command) => {
            return vec![format!(
                "`make {target}` runs `{command}`, which takes the configuration's flags"
            )];
        }
        Assignment::Unassigned => return Vec::new(),
    };
    let named = [
        (flags.names_threads(), THREADS_FLAG),
        (flags.names_linker(), LINKER_FLAG),
    ];
    let mut problems: Problems = named
        .into_iter()
        .filter(|(is_named, _)| *is_named)
        .map(|(_, flag)| format!("`make {target}` takes {flag}"))
        .collect();
    // A release build keeps the caller's own flags (a sanitizer, a target feature) while it drops
    // the standard's; only the coverage build, a measurement, ignores them.
    if target == "release" && !inherits {
        problems.push(format!("`make {target}` drops the caller's RUSTFLAGS"));
    }
    problems
}

/// Returns every complaint about the held-out targets, and how many commands it
/// read: each assigns `RUSTFLAGS`, since only an assignment displaces the
/// configuration's sources.
///
/// # Errors
///
/// Returns the reason when a listed target is not defined or unreadable.
pub fn held_out_problems(runner: MakeRunner) -> Result<(Problems, usize), String> {
    held_out_problems_for(runner, HELD_OUT_TARGETS)
}

/// Returns the complaints about a given list of held-out targets, so a test can name a
/// synthetic target and exercise the check in a repository that defines none.
///
/// # Errors
///
/// Returns the reason when a named target is not defined or unreadable.
pub fn held_out_problems_for(
    runner: MakeRunner,
    targets: &[&str],
) -> Result<(Problems, usize), String> {
    let mut problems = Vec::new();
    let mut read = 0;
    for target in targets {
        let commands: Vec<Assignment> = make_commands(runner, target, Host::Linux)?
            .into_iter()
            .map(|(_, command)| command)
            .collect();
        read += commands.len();
        // A command that assigns nothing and runs no build tool (a formatter, a metadata probe) is
        // `Unassigned`; a target made only of those would hide behind the other targets' count.
        if !commands
            .iter()
            .any(|command| !matches!(command, Assignment::Unassigned))
        {
            problems.push(format!(
                "`make {target}` runs no build or test command, so the check reads nothing of it"
            ));
        }
        problems.extend(
            commands
                .iter()
                .flat_map(|command| held_out_command_problems(target, command)),
        );
    }
    Ok((problems, read))
}

/// Returns the number of held-out targets the repository defines.
pub const fn held_out_target_count() -> usize { HELD_OUT_TARGETS.len() }

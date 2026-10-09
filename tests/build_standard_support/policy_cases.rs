//! Cases for the policy readers of the build-standard contract: which commands run tests, which
//! tool a command runs, and the warning policy the commands that run tests must keep, each driven
//! over fixtures first so that no rule passes by detecting nothing.

use rstest::rstest;

use super::{
    config::{Flags, THREADS_FLAG},
    development::{runs_tests, test_policy_problems, tool_key},
    make::{Assignment, Command, Host, Target},
    reader_cases::Fixture,
};

/// One command of the `test` target in a case: its text and the flags its `RUSTFLAGS` assigns.
#[derive(Clone, Copy)]
struct Cmd(&'static str, &'static [&'static str]);

/// Scenario: the `test` target's commands, with and without the warning policy, beside a version
/// probe and a build that never carried it.
///
/// Invariant: `make test` must run tests, and every command that runs them must keep `-D warnings`;
/// a probe or a build is not held to it, and a probe keeping the policy cannot stand in for a test
/// command that dropped it; another target is not held to it.
#[rstest]
#[case::test_keeps_the_policy(Fixture("test"), &[Cmd("cargo nextest run", &["-D", "warnings", THREADS_FLAG])], 0)]
#[case::test_spelled_joined(Fixture("test"), &[Cmd("cargo test", &["-Dwarnings", THREADS_FLAG])], 0)]
#[case::test_drops_the_policy(Fixture("test"), &[Cmd("cargo nextest run", &[THREADS_FLAG])], 1)]
#[case::test_denies_nothing_useful(Fixture("test"), &[Cmd("cargo test", &["-A", "warnings", THREADS_FLAG])], 1)]
#[case::a_probe_may_omit_it_beside_the_run(
    Fixture("test"),
    &[Cmd("cargo nextest --version", &[THREADS_FLAG]), Cmd("cargo nextest run", &["-D", "warnings", THREADS_FLAG])],
    0
)]
#[case::a_probe_cannot_stand_in_for_the_run(
    Fixture("test"),
    &[Cmd("cargo nextest --version", &["-D", "warnings", THREADS_FLAG]), Cmd("cargo nextest run", &[THREADS_FLAG])],
    1
)]
#[case::the_doctest_line_is_a_test_command_too(
    Fixture("test"),
    &[Cmd("cargo nextest run", &["-D", "warnings"]), Cmd("cargo test --doc", &[THREADS_FLAG])],
    1
)]
#[case::a_prerequisite_build_may_omit_it(
    Fixture("test"),
    &[Cmd("cargo build --bin tool", &[THREADS_FLAG]), Cmd("cargo test", &["-D", "warnings"])],
    0
)]
#[case::a_toolchain_override_is_skipped(Fixture("test"), &[Cmd("cargo +nightly test", &[THREADS_FLAG])], 1)]
#[case::build_may_omit_the_policy(Fixture("build"), &[Cmd("cargo build", &[THREADS_FLAG])], 0)]
#[case::test_runs_no_test_command(Fixture("test"), &[Cmd("cargo build", &["-D", "warnings"])], 1)]
#[case::test_assigns_no_command(Fixture("test"), &[], 1)]
fn the_test_target_keeps_the_warning_policy(
    #[case] target: Fixture,
    #[case] commands: &[Cmd],
    #[case] expected: usize,
) {
    let assigned: Vec<Command> = commands
        .iter()
        .map(|Cmd(text, words)| Command {
            text: (*text).to_owned(),
            assignment: Assignment::Flags(Flags::from_words(words.iter().copied()), true),
        })
        .collect();
    let found = test_policy_problems(Target(target.0), Host::Darwin, &assigned).len();
    assert_eq!(found, expected, "target {}: {assigned:?}", target.0);
}

/// Scenario: command lines in each spelling `make -n` prints Cargo in, on each host.
///
/// Invariant: a command runs tests when its Cargo, bare or at any path (a `.exe` on Windows), is
/// followed past any toolchain override and options by `test` or `nextest run`; a probe or a build
/// does not. (A line that merely echoes Cargo never reaches the reader: `commands_with_text` drops
/// it.)
#[rstest]
#[case::bare_test(Fixture("cargo test"), true)]
#[case::nextest_run(Fixture("cargo nextest run --all-targets"), true)]
#[case::an_absolute_unix_path(Fixture("/usr/bin/cargo test --doc"), true)]
#[case::a_windows_path(Fixture("C:/Users/x/.cargo/bin/cargo.exe nextest run"), true)]
#[case::a_windows_path_with_backslashes(Fixture("C:\\tools\\cargo.exe test"), true)]
#[case::a_toolchain_override(Fixture("cargo +nightly nextest run"), true)]
#[case::options_before_the_subcommand(Fixture("cargo --locked test"), true)]
#[case::a_version_probe(Fixture("cargo nextest --version"), false)]
#[case::a_build(Fixture("cargo build --release"), false)]
#[case::another_executable(Fixture("notcargo test"), false)]
fn the_test_reader_recognizes_a_test_run_in_each_spelling(
    #[case] command: Fixture,
    #[case] expected: bool,
) {
    assert_eq!(runs_tests(command.0), expected, "{}", command.0);
}

/// Scenario: command lines in the spellings `make -n` prints, reduced to the tool they run.
///
/// Invariant: a build, test or lint command reads as its tool whatever assignment, toolchain
/// override or path precedes it, `nextest` keeps its action, and a version probe, a formatter and a
/// documentation build read as none.
#[rstest]
#[case::a_build(Fixture("cargo build --release"), Some("cargo build"))]
#[case::an_assignment_and_a_toolchain(
    Fixture("RUSTFLAGS=\"-D warnings\" cargo +nightly clippy --all-targets"),
    Some("cargo clippy")
)]
#[case::nextest_run(Fixture("cargo nextest run --all-targets"), Some("cargo nextest run"))]
#[case::a_windows_path(Fixture("C:/tools/cargo.exe check"), Some("cargo check"))]
#[case::whitaker(Fixture("RUSTFLAGS=\"-D warnings\" whitaker --all"), Some("whitaker"))]
#[case::a_version_probe(Fixture("cargo nextest --version"), None)]
#[case::a_formatter(Fixture("cargo fmt --all --check"), None)]
#[case::a_documentation_build(Fixture("cargo doc --no-deps"), None)]
#[case::not_cargo_at_all(Fixture("echo build"), None)]
fn the_tool_reader_names_what_a_command_runs(
    #[case] command: Fixture,
    #[case] expected: Option<&str>,
) {
    assert_eq!(tool_key(command.0).as_deref(), expected, "{}", command.0);
}

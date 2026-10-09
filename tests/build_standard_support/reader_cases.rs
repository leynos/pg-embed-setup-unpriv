//! Cases for the readers of the build-standard contract: the configuration reader, the pin
//! reader, the command reader, the workflow readers and the warning policies, each driven over
//! fixtures first so that no rule passes by detecting nothing. The tests of the repository itself
//! live in the contract file beside this module.

use rstest::rstest;

use super::{
    ci_steps::{
        COVERAGE_DENIES_WARNINGS,
        Workflow,
        coverage_presence_problem,
        coverage_problems,
        linker_install_problems,
    },
    config::{Flags, Pin, Problems, THREADS_FLAG, config_problems},
    fixtures::{
        BARE_NIGHTLY,
        BUILD_LOSES_THREADS,
        COMMENT_AFTER_CHANNEL,
        COMMENT_NAMING_THE_ACTION,
        COMMENTED_OK,
        COVERAGE_BORROWING_A_SIBLING,
        COVERAGE_COMMENTED_POLICY,
        COVERAGE_DENYING_WITH_COMMENT,
        COVERAGE_EMPTY_POLICY,
        COVERAGE_LOOKALIKE_POLICY,
        COVERAGE_OK,
        COVERAGE_OTHER_POLICY,
        COVERAGE_UNASSIGNED,
        COVERAGE_WITH_LINKER,
        COVERAGE_WITH_THREADS,
        LINKER_IN_BUILD,
        LINUX_LOSES_LINKER,
        NIGHTLY,
        NIGHTLY_OK,
        NIGHTLY_SPELLED_APART,
        NO_BUILD_SOURCE,
        NO_CHANNEL,
        SHORT_DATED_NIGHTLY,
        SIBLING_KEY_OK,
        SPREAD_ARRAY,
        STABLE,
        STABLE_OK,
        STABLE_WITH_THREADS,
        STEP_BEFORE_A_SIBLING_THAT_INSTALLS,
        STEP_INPUT_OFF,
        STEP_INSTALLS,
        STEP_INSTALLS_BARE,
        STEP_MISSING_INPUT,
        TRAILING_CONTENT,
        TRIPLE_ONLY,
        TWO_CHANNELS,
        UNCLOSED_CHANNEL,
        UNDATED_NIGHTLY,
        UNKNOWN_CHANNEL,
        UNQUOTED_BESIDE_VALID,
    },
    make::{Assignment, assigned_rustflags, commands_from},
};

/// Text handed to a case, wrapped so that a case reads as data and the test
/// signatures name what they take rather than passing bare strings around.
#[derive(Clone, Copy)]
pub(super) struct Fixture(pub(super) &'static str);

/// The workflow reader a case runs.
#[derive(Clone, Copy)]
enum Reader {
    /// Each `setup-rust` step must pass `install-mold: 'true'` itself.
    InstallInput,
    /// Each coverage step must assign its own `RUSTFLAGS`.
    CoverageAssignment,
}

impl Reader {
    /// Returns the complaints this reader raises about a workflow.
    fn problems(self, workflow: &Workflow) -> Problems {
        match self {
            Self::InstallInput => linker_install_problems(workflow),
            Self::CoverageAssignment => coverage_problems(workflow),
        }
    }
}

/// Checks that a fixture configuration draws the expected number of complaints.
fn draws(config: Fixture, pin: Pin, expected: usize) -> Result<(), String> {
    let found = config_problems(config.0, pin)?.len();
    if found == expected {
        Ok(())
    } else {
        Err(format!("{:?}: {found} problems, not {expected}", config.0))
    }
}

/// Scenario: configurations of each shape, read on a nightly and a stable pin.
///
/// Invariant: a compliant nightly file passes, and each way of losing the
/// frontend flag, losing mold, naming mold beyond Linux, or letting a source
/// drift is reported; a stable pin refuses the frontend flag it cannot take.
#[rstest]
#[case::compliant_nightly(Fixture(NIGHTLY_OK), Pin::Nightly, 0)]
#[case::linker_spelled_as_a_pair(Fixture(NIGHTLY_SPELLED_APART), Pin::Nightly, 0)]
#[case::compliant_stable(Fixture(STABLE_OK), Pin::Stable, 0)]
#[case::build_loses_the_frontend(Fixture(BUILD_LOSES_THREADS), Pin::Nightly, 2)]
#[case::linux_loses_the_linker(Fixture(LINUX_LOSES_LINKER), Pin::Nightly, 1)]
#[case::linker_named_in_build(Fixture(LINKER_IN_BUILD), Pin::Nightly, 1)]
#[case::no_build_source(Fixture(NO_BUILD_SOURCE), Pin::Nightly, 1)]
#[case::stable_names_the_frontend(Fixture(STABLE_WITH_THREADS), Pin::Stable, 1)]
#[case::empty_configuration(Fixture(""), Pin::Nightly, 3)]
#[case::linux_selector_covers_one_architecture(Fixture(TRIPLE_ONLY), Pin::Nightly, 1)]
#[case::comments_after_headers_and_entries(Fixture(COMMENTED_OK), Pin::Nightly, 0)]
#[case::a_key_that_only_starts_like_rustflags(Fixture(SIBLING_KEY_OK), Pin::Nightly, 0)]
fn the_configuration_reader_reports_each_defect(
    #[case] config: Fixture,
    #[case] pin: Pin,
    #[case] expected: usize,
) -> Result<(), String> {
    draws(config, pin, expected)
}

/// Scenario: a `rustflags` array spread over several lines.
///
/// Invariant: the reader refuses it, because reading half of an entry would let
/// a lost flag pass.
#[test]
fn a_rustflags_array_spread_over_lines_is_refused() -> Result<(), String> {
    match config_problems(SPREAD_ARRAY, Pin::Nightly) {
        Ok(_) => Err("a rustflags array spread over lines was read".to_owned()),
        Err(_) => Ok(()),
    }
}

/// Scenario: toolchain files pinning each kind of channel, and files that do
/// not.
///
/// Invariant: only a `nightly` channel reads as nightly, so only it is asked to
/// carry `-Zthreads`; a missing, repeated, unknown or malformed channel is an
/// error, not a stable pin by default, and a comment after the quote is fine. A
/// nightly is `nightly-YYYY-MM-DD`; a bare `nightly` floats, and any other suffix is unknown.
#[rstest]
#[case::nightly(Fixture(NIGHTLY), Some(Pin::Nightly))]
#[case::stable(Fixture(STABLE), Some(Pin::Stable))]
#[case::missing(Fixture(NO_CHANNEL), None)]
#[case::repeated(Fixture(TWO_CHANNELS), None)]
#[case::unknown(Fixture(UNKNOWN_CHANNEL), None)]
#[case::unquoted_beside_a_valid_one(Fixture(UNQUOTED_BESIDE_VALID), None)]
#[case::no_closing_quote(Fixture(UNCLOSED_CHANNEL), None)]
#[case::an_undated_nightly(Fixture(UNDATED_NIGHTLY), None)]
#[case::a_bare_nightly_floats(Fixture(BARE_NIGHTLY), None)]
#[case::a_date_that_is_not_padded(Fixture(SHORT_DATED_NIGHTLY), None)]
#[case::content_after_the_quote(Fixture(TRAILING_CONTENT), None)]
#[case::comment_after_the_quote(Fixture(COMMENT_AFTER_CHANNEL), Some(Pin::Stable))]
fn the_pin_reader_tells_the_channels_apart(
    #[case] toolchain: Fixture,
    #[case] expected: Option<Pin>,
) {
    assert_eq!(Pin::read(toolchain.0).ok(), expected);
}

/// Builds the assignment a fixture line is expected to read as.
fn flags(words: &[&str], inherits: bool) -> Assignment {
    Assignment::Flags(Flags::from_words(words.iter().copied()), inherits)
}

/// Scenario: `make -n` output lines in each spelling of an assignment.
///
/// Invariant: a quoted assignment is read, with the caller's inherited flags
/// set aside, and a line assigning none reads as unassigned.
#[rstest]
#[case::plain(Fixture("RUSTFLAGS=\"-D warnings -Zthreads=8\" cargo test"), flags(&["-D", "warnings", THREADS_FLAG], false))]
#[case::inherited_flags_glued_on(
    Fixture("RUSTFLAGS=\"${RUSTFLAGS:+$RUSTFLAGS }-Zthreads=8\" cargo check"),
    flags(&[THREADS_FLAG], true)
)]
#[case::inherited_flags_only(Fixture("RUSTFLAGS=\"${RUSTFLAGS-}\" cargo build --release"), flags(&[], true))]
#[case::inherited_flags_then_a_space(
    Fixture("RUSTFLAGS=\"${RUSTFLAGS-} -Zthreads=8\" cargo check"),
    flags(&[THREADS_FLAG], true)
)]
#[case::no_assignment(Fixture("cargo clippy --all-targets"), Assignment::Unassigned)]
fn the_command_reader_reads_each_assignment(
    #[case] line: Fixture,
    #[case] expected: Assignment,
) -> Result<(), String> {
    if assigned_rustflags(line.0)? == expected {
        Ok(())
    } else {
        Err(format!("`{}` was read wrongly", line.0))
    }
}

/// Scenario: `make -n` output lines whose assignment the reader cannot parse.
///
/// Invariant: each is refused rather than passed, because an assignment in a
/// form the reader does not understand still replaces the configuration.
#[rstest]
#[case::unquoted(Fixture("RUSTFLAGS=-Zthreads=8 cargo test"))]
#[case::unterminated(Fixture("RUSTFLAGS=\"-Zthreads=8 cargo test"))]
#[case::inherited_flags_glued_to_a_flag(Fixture(
    "RUSTFLAGS=\"${RUSTFLAGS-}-Zthreads=8\" cargo test"
))]
fn the_command_reader_refuses_what_it_cannot_parse(#[case] line: Fixture) -> Result<(), String> {
    match assigned_rustflags(line.0) {
        Ok(_) => Err(format!("`{}` was read, not refused", line.0)),
        Err(_) => Ok(()),
    }
}

/// Scenario: workflow steps read by each workflow reader.
///
/// Invariant: a `setup-rust` step must pass `install-mold: 'true'` itself, and a
/// coverage step must assign `RUSTFLAGS` itself and name neither standard flag.
/// Another step's input or assignment does not count, and a comment naming the
/// action is not a step.
#[rstest]
#[case::quoted_true(Reader::InstallInput, Fixture(STEP_INSTALLS), 0)]
#[case::bare_true(Reader::InstallInput, Fixture(STEP_INSTALLS_BARE), 0)]
#[case::missing_input(Reader::InstallInput, Fixture(STEP_MISSING_INPUT), 1)]
#[case::input_off(Reader::InstallInput, Fixture(STEP_INPUT_OFF), 1)]
#[case::input_on_a_sibling_step(
    Reader::InstallInput,
    Fixture(STEP_BEFORE_A_SIBLING_THAT_INSTALLS),
    1
)]
#[case::comment_only(Reader::InstallInput, Fixture(COMMENT_NAMING_THE_ACTION), 0)]
#[case::unassigned(Reader::CoverageAssignment, Fixture(COVERAGE_UNASSIGNED), 1)]
#[case::with_the_frontend_flag(Reader::CoverageAssignment, Fixture(COVERAGE_WITH_THREADS), 1)]
#[case::with_the_linker(Reader::CoverageAssignment, Fixture(COVERAGE_WITH_LINKER), 1)]
#[case::assignment_on_a_sibling_step(
    Reader::CoverageAssignment,
    Fixture(COVERAGE_BORROWING_A_SIBLING),
    1
)]
fn the_workflow_readers_judge_each_step(
    #[case] reader: Reader,
    #[case] workflow: Fixture,
    #[case] expected: usize,
) -> Result<(), String> {
    let found = reader
        .problems(&Workflow {
            file: "fixture.yml",
            text: workflow.0,
        })
        .len();
    if found == expected {
        Ok(())
    } else {
        Err(format!(
            "{:?}: {found} problems, not {expected}",
            workflow.0
        ))
    }
}

/// Scenario: a recipe continued over lines, beside an `echo` and another
/// command.
///
/// Invariant: the continued command is one command, and lines that are not a
/// Cargo or Whitaker command are ignored.
#[test]
fn a_continued_command_is_one_command() -> Result<(), String> {
    let joined = commands_from(concat!(
        "RUSTFLAGS=\"-A\" \\\n",
        "cargo test\necho cargo test\nmake other\n"
    ))?;
    if joined == vec![flags(&["-A"], false)] {
        Ok(())
    } else {
        Err(format!("read wrongly: {joined:?}"))
    }
}

/// Scenario: coverage steps that deny warnings, assign an empty policy, or assign a
/// different one.
///
/// Invariant: the warning policy is pinned both ways, so every case fires in one mode.
/// Where the repository's coverage denies warnings, a step that denies them is accepted
/// and an empty or different policy is refused; where it deliberately does not, a step
/// that starts denying warnings is the drift and is refused, while the others are accepted.
#[rstest]
#[case::denying_warnings(Fixture(COVERAGE_OK), true)]
#[case::an_empty_warning_policy(Fixture(COVERAGE_EMPTY_POLICY), false)]
#[case::a_different_warning_policy(Fixture(COVERAGE_OTHER_POLICY), false)]
#[case::a_lookalike_flag(Fixture(COVERAGE_LOOKALIKE_POLICY), false)]
#[case::a_policy_only_in_a_comment(Fixture(COVERAGE_COMMENTED_POLICY), false)]
#[case::a_comment_after_the_policy(Fixture(COVERAGE_DENYING_WITH_COMMENT), true)]
fn a_coverage_step_keeps_the_repository_warning_policy(
    #[case] workflow: Fixture,
    #[case] denies: bool,
) {
    let found = coverage_problems(&Workflow {
        file: "fixture.yml",
        text: workflow.0,
    })
    .len();
    assert_eq!(
        found,
        usize::from(denies != COVERAGE_DENIES_WARNINGS),
        "workflow:\n{}",
        workflow.0
    );
}

/// Scenario: the number of coverage steps found against the number recorded.
///
/// Invariant: a removed or an added coverage step is a complaint, and a match is not, so a
/// repository with no coverage step records none and passes.
#[test]
fn the_coverage_step_count_is_pinned_both_ways() {
    assert!(coverage_presence_problem(1, 1).is_none());
    assert!(coverage_presence_problem(0, 0).is_none());
    assert!(coverage_presence_problem(0, 1).is_some());
    assert!(coverage_presence_problem(2, 1).is_some());
}

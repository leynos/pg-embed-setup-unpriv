//! Regression coverage for how CI obtains the Whitaker Dylint suite.
//!
//! CI once installed `whitaker-installer` with `cargo install --locked`,
//! building the tool from source and verifying nothing. These checks keep the
//! workflow on the shared action, which resolves a pinned, checksum-verified
//! release archive, and keep the installer version explicit so a change to it
//! arrives as a reviewed diff rather than silently.
//!
//! The action pin is checked for its shape, a full lowercase commit SHA, and
//! not against one hard-coded SHA or a copy of the revisions the concordat
//! QG-002 rule accepts. A value check passed for exactly one upstream commit,
//! and a copied list had to be refreshed by hand for every approved bump, which
//! made this suite red on Dependabot updates. Which revisions are acceptable is
//! the rule's decision, made where the rule lives; this suite holds the
//! workflow to an immutable, well-formed pin on the shared action.
//!
//! Assertions about the action are made against the extracted install step, not
//! against the whole file, so an unrelated step or a comment cannot satisfy
//! them while the real step drifts.

const CI_WORKFLOW: &str = include_str!("../.github/workflows/ci.yml");
const STEP_NAME: &str = "- name: Install Whitaker Dylint suite";
const INSTALL_WHITAKER_ACTION: &str = "leynos/shared-actions/.github/actions/install-whitaker@";
const INSTALLER_VERSION_INPUT: &str = "installer-version: ${{ env.WHITAKER_INSTALLER_VERSION }}";
const SHA_LENGTH: usize = 40;
/// The oldest installer the shared action accepts; it refuses anything lower.
const MINIMUM_INSTALLER_VERSION: [u32; 3] = [0, 2, 9];

/// Return the number of leading spaces on `line`.
fn indent_of(line: &str) -> usize { line.len() - line.trim_start().len() }

/// A workflow document whose Whitaker step and pin can be checked together.
struct Workflow<'a> {
    text: &'a str,
}

impl Workflow<'_> {
    /// Extract the install step, stopping at the next sibling or outdent.
    fn whitaker_step(&self) -> WhitakerStep {
        let mut lines = self.text.lines();
        let Some(header) = lines.by_ref().find(|line| line.trim() == STEP_NAME) else {
            panic!("ci.yml has no step named {STEP_NAME:?}");
        };
        let step_indent = indent_of(header);
        let text = lines
            .take_while(|line| {
                let is_sibling_step =
                    indent_of(line) == step_indent && line.trim_start().starts_with("- ");
                let is_outdented = !line.trim().is_empty() && indent_of(line) < step_indent;
                !is_sibling_step && !is_outdented
            })
            .collect::<Vec<_>>()
            .join("\n");
        WhitakerStep {
            text,
            field_indent: step_indent + 2,
        }
    }

    /// Validate the shape of the extracted action pin.
    fn validate_pin(&self) -> Result<(), String> {
        let revision = self.whitaker_step().revision()?;
        if !Revision(&revision).is_full_lowercase_sha() {
            return Err(format!(
                "install-whitaker must use a full lowercase SHA: {revision:?}"
            ));
        }
        Ok(())
    }
}

/// The extracted install step, kept separate from unrelated workflow text.
struct WhitakerStep {
    text: String,
    field_indent: usize,
}

impl WhitakerStep {
    /// Read the sole direct `uses` entry, excluding comments and nested fields.
    fn revision(&self) -> Result<String, String> {
        let mut entries = self.text.lines().filter_map(|line| {
            (indent_of(line) == self.field_indent)
                .then_some(line.trim().strip_prefix("uses:"))
                .flatten()
        });
        let entry = entries
            .next()
            .ok_or_else(|| "missing uses entry".to_owned())?;
        if entries.next().is_some() {
            return Err("duplicate uses entries".to_owned());
        }
        let value = entry
            .split_whitespace()
            .next()
            .ok_or_else(|| "empty uses entry".to_owned())?;
        let revision = value
            .strip_prefix(INSTALL_WHITAKER_ACTION)
            .ok_or_else(|| format!("unexpected uses value: {value:?}"))?;
        Ok(revision.to_owned())
    }
}

/// Collapse shell line continuations and drop comment lines.
///
/// A command split after a trailing backslash still runs as one command, so it
/// must be matched as one; a comment naming a forbidden command is only prose.
fn logical_lines(text: &str) -> Vec<String> {
    let mut joined = Vec::new();
    let mut pending = String::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some(head) = trimmed.strip_suffix('\\') {
            pending.push_str(head.trim_end());
            pending.push(' ');
        } else {
            pending.push_str(trimmed);
            joined.push(std::mem::take(&mut pending));
        }
    }
    if !pending.is_empty() {
        joined.push(pending);
    }
    joined
}

/// Report whether a command obtains the installer through Cargo.
///
/// Both arms matter: `cargo install` builds the tool from source, and
/// `cargo binstall` fetches it from a registry without the pinned digest the
/// shared action verifies.
fn installs_whitaker_through_cargo(command: &str) -> bool {
    (command.contains("cargo install") || command.contains("cargo binstall"))
        && command.contains("whitaker-installer")
}

/// A candidate action revision, checked for its syntax.
struct Revision<'a>(&'a str);

impl Revision<'_> {
    /// Require the lowercase SHA form, as Git prints it.
    fn is_full_lowercase_sha(&self) -> bool {
        self.0.len() == SHA_LENGTH
            && self
                .0
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    }
}

#[test]
fn the_install_step_uses_the_shared_action() {
    let step = Workflow { text: CI_WORKFLOW }.whitaker_step();

    assert!(
        step.revision().is_ok(),
        "the Whitaker step must use the shared install-whitaker action:\n{}",
        step.text,
    );
}

#[test]
fn the_install_step_pins_the_shared_action_to_an_approved_commit_sha() {
    Workflow { text: CI_WORKFLOW }
        .validate_pin()
        .expect("the workflow pin must be approved");
}

#[test]
fn a_malformed_workflow_pin_fails_validation() {
    let current = Workflow { text: CI_WORKFLOW }
        .whitaker_step()
        .revision()
        .expect("valid uses entry");
    let branch = "main";
    assert!(!Revision(branch).is_full_lowercase_sha());
    let old_pin = format!("uses: {INSTALL_WHITAKER_ACTION}{current}");
    let new_pin = format!("uses: {INSTALL_WHITAKER_ACTION}{branch}");
    assert_eq!(
        CI_WORKFLOW.matches(&old_pin).count(),
        1,
        "mutation must target the install step"
    );
    let mutated = CI_WORKFLOW.replacen(&old_pin, &new_pin, 1);
    let error = Workflow { text: &mutated }
        .validate_pin()
        .expect_err("a branch pin must fail");
    assert!(
        error.contains("full lowercase SHA"),
        "unexpected error: {error}"
    );
}

/// A candidate the pin guard must refuse.
///
/// A truncated SHA and a branch name are both mutable or ambiguous references;
/// each must fail the format check on its own.
#[rstest::rstest]
#[case::truncated_sha("6dea567", "a truncated SHA")]
#[case::branch_name("main", "a branch name")]
fn a_malformed_revision_is_rejected(#[case] candidate: &str, #[case] what: &str) {
    assert!(
        !Revision(candidate).is_full_lowercase_sha(),
        "{what} must not pass as a pin, found {candidate:?}",
    );
}

/// Uppercase hex must be refused even though Git accepts it.
///
/// The rule lists lowercase revisions, so a case-insensitive format check would
/// let a pin through that the rule's exact comparison would then refuse.
#[test]
fn an_uppercase_sha_is_rejected() {
    let uppercase = "6DEA5677A84FEC60CA51B07202570E3AF12FFDB4";

    assert!(
        !Revision(uppercase).is_full_lowercase_sha(),
        "an uppercase SHA must fail the format check: {uppercase:?}",
    );
}

#[test]
fn the_install_step_requests_an_explicit_installer_version() {
    Workflow { text: CI_WORKFLOW }
        .whitaker_step()
        .validate_installer_input()
        .expect("the installer version must use the configured value");
}

#[test]
fn the_workflow_defines_a_concrete_installer_version() {
    let Some(rest) = CI_WORKFLOW
        .split_once("WHITAKER_INSTALLER_VERSION: '")
        .map(|(_, rest)| rest)
    else {
        panic!("ci.yml does not define WHITAKER_INSTALLER_VERSION");
    };
    let Some(version) = rest.split('\'').next() else {
        panic!("WHITAKER_INSTALLER_VERSION is not a quoted literal");
    };

    assert!(
        !version.is_empty()
            && version
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())),
        "WHITAKER_INSTALLER_VERSION must be a concrete version, found {version:?}",
    );

    // Every component must parse; a dropped or overflowing one would otherwise
    // shift the comparison and let a bad version pass the floor.
    let parsed: Result<Vec<u32>, _> = version.split('.').map(str::parse::<u32>).collect();
    let Ok(parts) = parsed else {
        panic!("WHITAKER_INSTALLER_VERSION has a component that is not a u32: {version:?}");
    };
    assert!(
        parts.as_slice() >= MINIMUM_INSTALLER_VERSION.as_slice(),
        "WHITAKER_INSTALLER_VERSION {version:?} is below the action's 0.2.9 floor",
    );
}

#[test]
fn the_guard_rejects_every_form_of_the_step_this_replaced() {
    let legacy = concat!(
        "          if cargo binstall --version >/dev/null 2>&1; then\n",
        "            cargo binstall --no-confirm --locked \\\n",
        "              \"whitaker-installer@${WHITAKER_INSTALLER_VERSION}\"\n",
        "          else\n",
        "            cargo install --locked whitaker-installer \\\n",
        "              --version \"${WHITAKER_INSTALLER_VERSION}\"\n",
        "          fi\n",
    );
    let matched = logical_lines(legacy)
        .into_iter()
        .filter(|command| installs_whitaker_through_cargo(command))
        .count();

    assert_eq!(
        matched, 2,
        "the guard must reject both legacy forms, including continued commands",
    );
}

#[test]
fn ci_never_obtains_the_whitaker_installer_through_cargo() {
    let offending: Vec<String> = logical_lines(CI_WORKFLOW)
        .into_iter()
        .filter(|command| installs_whitaker_through_cargo(command))
        .collect();

    assert!(
        offending.is_empty(),
        "CI must not build or fetch whitaker-installer through Cargo: {offending:?}",
    );
}

#[path = "whitaker_install_pin/uses_tests.rs"]
mod uses_tests;

#[path = "whitaker_install_pin/installer_input.rs"]
mod installer_input;

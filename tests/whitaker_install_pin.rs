//! Regression coverage for how CI obtains the Whitaker Dylint suite.
//!
//! CI once installed `whitaker-installer` with `cargo install --locked`,
//! building the tool from source and verifying nothing. These checks keep the
//! workflow on the shared action, which resolves a pinned, checksum-verified
//! release archive, and keep the installer version explicit so a change to it
//! arrives as a reviewed diff rather than silently.
//!
//! The action pin is checked against a local copy of the revisions the
//! concordat QG-002 rule accepts, not against one hard-coded SHA. A single SHA
//! assertion passed for exactly one upstream commit and then had to be edited
//! by hand for every later approved bump, which is what made this suite red on
//! a Dependabot update. Membership keeps the reviewed gate — the list only
//! changes when the rule does, and the list's own guards make an edited entry
//! visible — while letting an approved revision through.
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
/// How many revisions the copied list must hold.
///
/// An exact count is the guard against a truncated or partly applied list: the
/// membership check below would otherwise pass while silently refusing every
/// legitimate bump, or accept one the upstream rule does not.
const APPROVED_REVISION_COUNT: usize = 77;

/// The shared-actions revisions the concordat QG-002 rule accepts.
///
/// Copied from `leynos/concordat` commit
/// `f46a4848ad5e62cfd170360074cddc3a792f3ea7` (PR #324), file
/// `platform-standards/canon/lint-rules/whitaker-provisioning/rule.yaml`, the
/// `compliant_install_whitaker_refs` default. That list is derived, never
/// edited by hand: upstream generates it from `install_whitaker_roots`, the
/// shared-actions commits whose `install-whitaker` directory has been
/// reviewed, and includes every later commit that leaves the directory's Git
/// tree id unchanged. Copy it verbatim, in the same order, when it is
/// refreshed, so a diff against upstream stays readable.
const APPROVED_ACTION_REVISIONS: &str = include_str!("fixtures/approved_whitaker_revisions.txt");

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
        WhitakerStep(text)
    }

    /// Validate the extracted action pin against the QG-002 list.
    fn validate_pin(&self) -> Result<(), String> {
        let revision = self.whitaker_step().revision();
        if !Revision(&revision).is_full_lowercase_sha() {
            return Err(format!(
                "install-whitaker must use a full lowercase SHA: {revision:?}"
            ));
        }
        if !Revision(&revision).is_approved() {
            return Err(format!(
                "install-whitaker revision {revision:?} is not approved by QG-002"
            ));
        }
        Ok(())
    }
}

/// The extracted install step, kept separate from unrelated workflow text.
struct WhitakerStep(String);

impl WhitakerStep {
    /// Return the complete revision token used by the install action.
    fn revision(&self) -> String {
        let Some(rest) = self
            .0
            .split_once(INSTALL_WHITAKER_ACTION)
            .map(|(_, rest)| rest)
        else {
            panic!(
                "the Whitaker step does not reference the shared action:\n{}",
                self.0
            );
        };
        rest.chars().take_while(|c| !c.is_whitespace()).collect()
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

/// A candidate action revision, checked for both syntax and approval.
struct Revision<'a>(&'a str);

impl Revision<'_> {
    /// Require the lowercase SHA form used by the upstream approved list.
    fn is_full_lowercase_sha(&self) -> bool {
        self.0.len() == SHA_LENGTH
            && self
                .0
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    }

    /// Check exact membership in the copied QG-002 list.
    fn is_approved(&self) -> bool {
        APPROVED_ACTION_REVISIONS
            .lines()
            .any(|approved| approved == self.0)
    }
}

#[test]
fn the_install_step_uses_the_shared_action() {
    let step = Workflow { text: CI_WORKFLOW }.whitaker_step();

    assert!(
        step.0.contains(INSTALL_WHITAKER_ACTION),
        "the Whitaker step must use the shared install-whitaker action:\n{}",
        step.0,
    );
}

#[test]
fn the_install_step_pins_the_shared_action_to_an_approved_commit_sha() {
    Workflow { text: CI_WORKFLOW }
        .validate_pin()
        .expect("the workflow pin must be approved");
}

#[test]
fn an_unapproved_workflow_pin_fails_validation() {
    let approved = Workflow { text: CI_WORKFLOW }.whitaker_step().revision();
    let unapproved = "0".repeat(SHA_LENGTH);
    assert!(Revision(&unapproved).is_full_lowercase_sha());
    assert!(!Revision(&unapproved).is_approved());
    let old_pin = format!("{INSTALL_WHITAKER_ACTION}{approved}");
    let new_pin = format!("{INSTALL_WHITAKER_ACTION}{unapproved}");
    assert_eq!(
        CI_WORKFLOW.matches(&old_pin).count(),
        1,
        "mutation must target the install step"
    );
    let mutated = CI_WORKFLOW.replacen(&old_pin, &new_pin, 1);
    let error = Workflow { text: &mutated }
        .validate_pin()
        .expect_err("an unapproved pin must fail");
    assert!(
        error.contains("not approved by QG-002"),
        "unexpected error: {error}"
    );
}

#[test]
fn the_approved_list_holds_the_expected_number_of_revisions() {
    assert_eq!(
        APPROVED_ACTION_REVISIONS.lines().count(),
        APPROVED_REVISION_COUNT,
        "the copied list must hold exactly {APPROVED_REVISION_COUNT} revisions",
    );
}

/// Every copied entry must be a well-formed SHA.
///
/// The guarantee has to hold for every entry, not a sample: a single
/// mistyped character in a copied revision would otherwise pass review and
/// silently stop matching the pin it was meant to approve. The index in the
/// message still names the offending entry, so a failure needs no bespoke
/// per-case assertion.
#[test]
fn the_approved_list_holds_only_full_lowercase_shas() {
    for (index, revision) in APPROVED_ACTION_REVISIONS.lines().enumerate() {
        assert!(
            Revision(revision).is_full_lowercase_sha(),
            "entry {index} of the approved list is not a full lowercase SHA: {revision:?}",
        );
    }
}

#[test]
fn the_approved_list_holds_no_duplicates() {
    let mut seen = std::collections::BTreeSet::new();
    let duplicates: Vec<&str> = APPROVED_ACTION_REVISIONS
        .lines()
        .filter(|revision| !seen.insert(*revision))
        .collect();

    assert!(
        duplicates.is_empty(),
        "the approved list repeats revisions: {duplicates:?}",
    );
}

/// A candidate the pin guard must refuse, and why.
///
/// The two header checks are deliberately split: a well-formed SHA that is
/// simply not on the list proves membership is doing work, and a malformed one
/// proves the format check does too. Without both, a guard that only compared
/// lengths, or only checked membership, would still pass the suite.
#[rstest::rstest]
#[case::well_formed_but_unlisted("0".repeat(SHA_LENGTH), "an off-list SHA")]
#[case::truncated_sha("6dea567", "a truncated SHA whose prefix is listed")]
fn an_unapproved_revision_is_rejected(#[case] candidate: String, #[case] what: &str) {
    let is_well_formed = Revision(&candidate).is_full_lowercase_sha();
    let is_approved = Revision(&candidate).is_approved();

    assert!(
        !is_approved,
        "{what} must not pass as approved, found {candidate:?}",
    );
    // The first case is well formed on purpose, so it isolates membership; the
    // second is not, so it isolates the format check. Assert the intended shape
    // so a fixture that drifts from its purpose cannot weaken the suite.
    let should_be_well_formed = candidate.len() == SHA_LENGTH;
    assert_eq!(
        is_well_formed, should_be_well_formed,
        "{what} does not have the shape this case is meant to test: {candidate:?}",
    );
}

/// Uppercase hex must be refused even though Git accepts it.
///
/// The copied list and the upstream rule are lowercase, so a case-insensitive
/// format check would let a hand-edited entry "match" a list it does not equal.
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
    let step = Workflow { text: CI_WORKFLOW }.whitaker_step();

    assert!(
        step.0.contains(INSTALLER_VERSION_INPUT),
        "the Whitaker step must pass {INSTALLER_VERSION_INPUT:?}:\n{}",
        step.0,
    );
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

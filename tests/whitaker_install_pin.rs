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
const APPROVED_ACTION_REVISIONS: [&str; APPROVED_REVISION_COUNT] = [
    "6dea5677a84fec60ca51b07202570e3af12ffdb4",
    "8a83824b29dfe8f861714b544dc85fb925ed010c",
    "14c325b60788814cd50298eb8060228daee3b842",
    "d4d248bbbecdcf7b4f5bc79ffd4d6caee370bd79",
    "9a27950942334d69ff79005b3a8db23bf151f43f",
    "a904888c815dc5919a3b24d41a9bb4d566459a93",
    "fc73b3fe2da2be3e641b4809268b6e350dfd0d28",
    "a38feb9be25755c30eca5bda96bd3786a5b89c6b",
    "d65313b25cc6db5a23518fe409280b167a3305e1",
    "b23b7b8d2b9438f8f697d2b7976463020bb0d1ea",
    "a8ca3302e60c6852561c19b75fa39020f86cb207",
    "6346e6a781b9c7db16ea2647b41076e6bf170b91",
    "03d0ed80b88628cca2b6f96bf32e4220b0121926",
    "6738afdb486258ed4f1dd5f526a58309be30396e",
    "584a0793a634e119673fd1e71f88f78970611f25",
    "63f7dbac64f6fc6217b859d71fff46f85df97e9e",
    "1cb2cfb9faf06424690cdf7753045745982de128",
    "2802a8448ceabe34995b091da62ef60e5276a7c7",
    "2573e4ccc4dadb9d30b18e408d393890989c1b9a",
    "ce8d747242ab3c9d58674d5838cf04c801eeb2f9",
    "32741eab2df11defd36eb06eaf2ac047ac8729ba",
    "2f58fab140cc8dfe69756aaa6a3bbb987c426023",
    "c856f4c570e8d83d64318ee2768a4906d0d807f6",
    "8901b50fda116a1a33304809b8767809dc17c203",
    "b7d70b5d3ff0ac0199416b9678ea7af25bb3254e",
    "8250c914bd32aeacfdf8d9e87b804ec0024ee83e",
    "d57cb19b82281236088108f2ffb7e13bc00fc2f8",
    "5e218e3c055da635947aa823e98387aa2ace0306",
    "6cec89bac47a21cf756d68d638a9a510998e57f8",
    "abf0dcf2686de1eaf79b6dc9a16662b631bed149",
    "e6d7c2f25cec0576b599de9acdb3bf29c3ca6655",
    "e734ba497fa85a1577dba6b66715b2d41587419f",
    "c4ed5ffaf0640b1907d5359a87fd1677034eec27",
    "013346ccfd37bd1e02eb430525233f1a4cd942b8",
    "ff1dd759dfffc0db3459e30e833f52437ee62b57",
    "88977798a5c3bae1549afb99642529488c665276",
    "ebe2f3105334283441f4c915eb979a0f4b9f2c04",
    "d9de10c64b2b84f42209c7a0d1d83aa3a3ffa0ea",
    "b6eb26ff70e41779a80fb7f4f073011d847ab5fc",
    "14eb66098248084fe8072da519dda6fec2745df2",
    "7d751eded578e48dacf43ab173d5820e6437c474",
    "b9a3bff63b1e19b3a48d433668184e24e1abd3ba",
    "6ecf4a7691031f92c3e75e176705cf541ad5283d",
    "f3b62790a564a18b0ef11e1dda32e7fa6113e4e5",
    "0606ca899ce2995a0c4bc78e660e80832e6e8855",
    "122fc392099957575259c97f0599bc3f0f095882",
    "d99a5e2aeba9c2164a70549f00540d0bd12a573b",
    "2f47859bcf82ae8b5bc10b457683fbd589ca2e41",
    "03ef8cdcea566bd1d34fac511f27928cf763c7d5",
    "f03cb6565da66299c43148ed46b2409866ac25d5",
    "c17d433f99daa94e53ca5eb566088518a312cb71",
    "5bc2b2611f5921ef2c56e4ee3fde2b879a319361",
    "d0c2585d9e144775e4ec9fe26e7eeb03c14844dd",
    "b59d7d13d29636cc9f0c33a81ba17a9a2bde9bb8",
    "b804b69fa7f978cf9091b9d9bd5481d8ce58c2ea",
    "3c0fa8e72567f5fb58f9b4f281fbc902e4ec48c5",
    "ec1432aa6fa5379b2dd8f4b3402fd5b250a597bd",
    "cfcc379e29be516160f499864ae82720842359af",
    "65e76d487b4ed17d0f4003a17eaccdc8859a7d0e",
    "d93ba23803a96cf76e233ae1331e24548d0157a5",
    "3754876bc4e94dc65c83af3def37828934f8f65e",
    "cabf105ae230e3759cf77b1c2d1d73ea0b67e9a9",
    "0b6bec31b2d7a1bb19bff3c4d9d76d54c6607bac",
    "c85021def0e09a26f6f2ef2d701c0d3dc04d4e3d",
    "26c918632c5413039a84358fafe012a4399a9ec7",
    "2ddf19b3fd52c0f1089db0d57b015f859b80ab67",
    "aea9c14c24b73042e99f0be8245a4f171b67c31a",
    "e693d81d176d2305e2b736a0be0f9038085dec7f",
    "aaf2bbccebae2fe4e8eaf7dab949de60f4d30071",
    "a563db345cb3bd1f88548fc0f3403583fc45bb67",
    "81374b6f183e59f0570ad36217ded90d3240fda6",
    "3e4cba9ff5898f40723b95e09b8d5bae26c1b1f6",
    "5ac271baf94615e9a876ce1b77a4b266dd81f201",
    "126d02e32a07c2b049e8b68fff09298526e5971a",
    "d26e53630893c942af44eb3db7efd4fcbe0c8093",
    "2aed718022775671809f8c85a2c10547dfb1ae13",
    "d5fded1107ee6822fdbf9539d81ff18ce6ce073a",
];

/// Return the number of leading spaces on `line`.
fn indent_of(line: &str) -> usize { line.len() - line.trim_start().len() }

/// Return the lines of the Whitaker install step, excluding its `- name:` line.
///
/// The step ends at the next sibling step, identified by a list entry at the
/// same indentation.
fn whitaker_step_lines() -> Option<Vec<&'static str>> {
    let mut lines = CI_WORKFLOW.lines();
    let header = lines.by_ref().find(|line| line.trim() == STEP_NAME)?;
    let step_indent = indent_of(header);
    Some(
        lines
            .take_while(|line| {
                let is_sibling_step =
                    indent_of(line) == step_indent && line.trim_start().starts_with("- ");
                let is_outdented = !line.trim().is_empty() && indent_of(line) < step_indent;
                !is_sibling_step && !is_outdented
            })
            .collect(),
    )
}

/// Return the whole Whitaker install step as a single string.
fn whitaker_step() -> String {
    let Some(lines) = whitaker_step_lines() else {
        panic!("ci.yml has no step named {STEP_NAME:?}");
    };
    lines.join("\n")
}

/// Return the text following `needle`, or `None` when it is absent.
///
/// `str::get` is used rather than an index range because the lint gate denies
/// `clippy::string_slice`.
fn text_after<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    haystack
        .find(needle)
        .and_then(|index| haystack.get(index + needle.len()..))
}

/// Return the revision `install-whitaker` is pinned to, up to the end of the
/// token.
///
/// Taking the whole token matters: a branch name or a trailing comment marker
/// must not be able to pass as the reviewed SHA.
fn pinned_action_revision(step: &str) -> String {
    let Some(rest) = text_after(step, INSTALL_WHITAKER_ACTION) else {
        panic!("the Whitaker step does not reference the shared action:\n{step}");
    };
    rest.chars().take_while(|c| !c.is_whitespace()).collect()
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

/// Report whether `revision` is written as a full, lowercase commit SHA.
///
/// Uppercase is rejected even though Git accepts it: the copied list and the
/// upstream rule are both lowercase, so a case-insensitive match would let a
/// hand-edited entry agree with a list it does not actually equal.
fn is_full_lowercase_sha(revision: &str) -> bool {
    revision.len() == SHA_LENGTH
        && revision
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Report whether the copied list holds `revision`.
fn is_approved_revision(revision: &str) -> bool { APPROVED_ACTION_REVISIONS.contains(&revision) }

#[test]
fn the_install_step_uses_the_shared_action() {
    let step = whitaker_step();

    assert!(
        step.contains(INSTALL_WHITAKER_ACTION),
        "the Whitaker step must use the shared install-whitaker action:\n{step}",
    );
}

#[test]
fn the_install_step_pins_the_shared_action_to_an_approved_commit_sha() {
    let revision = pinned_action_revision(&whitaker_step());

    assert!(
        is_full_lowercase_sha(&revision),
        "install-whitaker must be pinned to a full lowercase commit SHA, found {revision:?}",
    );
    assert!(
        is_approved_revision(&revision),
        concat!(
            "install-whitaker is pinned to {:?}, which QG-002 does not list. ",
            "Refresh APPROVED_ACTION_REVISIONS from concordat when a bump is approved.",
        ),
        revision,
    );
}

#[test]
fn the_approved_list_holds_the_expected_number_of_revisions() {
    assert_eq!(
        APPROVED_ACTION_REVISIONS.len(),
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
    for (index, revision) in APPROVED_ACTION_REVISIONS.iter().enumerate() {
        assert!(
            is_full_lowercase_sha(revision),
            "entry {index} of the approved list is not a full lowercase SHA: {revision:?}",
        );
    }
}

#[test]
fn the_approved_list_holds_no_duplicates() {
    let mut seen = std::collections::BTreeSet::new();
    let duplicates: Vec<&str> = APPROVED_ACTION_REVISIONS
        .iter()
        .copied()
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
    let is_well_formed = is_full_lowercase_sha(&candidate);
    let is_approved = is_approved_revision(&candidate);

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
        !is_full_lowercase_sha(uppercase),
        "an uppercase SHA must fail the format check: {uppercase:?}",
    );
}

#[test]
fn the_install_step_requests_an_explicit_installer_version() {
    let step = whitaker_step();

    assert!(
        step.contains(INSTALLER_VERSION_INPUT),
        "the Whitaker step must pass {INSTALLER_VERSION_INPUT:?}:\n{step}",
    );
}

#[test]
fn the_workflow_defines_a_concrete_installer_version() {
    let Some(rest) = text_after(CI_WORKFLOW, "WHITAKER_INSTALLER_VERSION: '") else {
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

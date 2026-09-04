//! Regression coverage for how CI obtains the Whitaker Dylint suite.
//!
//! CI once installed `whitaker-installer` with `cargo install --locked`,
//! building the tool from source and verifying nothing. These checks keep the
//! workflow on the shared action, which resolves a pinned, checksum-verified
//! release archive, and keep the installer version explicit so a change to it
//! arrives as a reviewed diff rather than silently.
//!
//! Assertions about the action are made against the extracted install step, not
//! against the whole file, so an unrelated step or a comment cannot satisfy
//! them while the real step drifts.

const CI_WORKFLOW: &str = include_str!("../.github/workflows/ci.yml");
const STEP_NAME: &str = "- name: Install Whitaker Dylint suite";
const INSTALL_WHITAKER_ACTION: &str = "leynos/shared-actions/.github/actions/install-whitaker@";
const INSTALLER_VERSION_INPUT: &str = "installer-version: ${{ env.WHITAKER_INSTALLER_VERSION }}";
const SHA_LENGTH: usize = 40;

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

#[test]
fn the_install_step_uses_the_shared_action() {
    let step = whitaker_step();

    assert!(
        step.contains(INSTALL_WHITAKER_ACTION),
        "the Whitaker step must use the shared install-whitaker action:\n{step}",
    );
}

#[test]
fn the_install_step_pins_the_shared_action_to_a_commit_sha() {
    let step = whitaker_step();
    let Some(rest) = text_after(&step, INSTALL_WHITAKER_ACTION) else {
        panic!("the Whitaker step does not reference the shared action:\n{step}");
    };
    let reference: String = rest.chars().take(SHA_LENGTH).collect();

    assert!(
        reference.len() == SHA_LENGTH && reference.chars().all(|c| c.is_ascii_hexdigit()),
        "install-whitaker must be pinned to a full commit SHA, found {reference:?}",
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

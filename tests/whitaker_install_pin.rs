//! Regression coverage for how CI obtains the Whitaker Dylint suite.
//!
//! CI once installed `whitaker-installer` with `cargo install --locked`,
//! building the tool from source and verifying nothing. These checks keep the
//! workflow on the shared action, which resolves a pinned, checksum-verified
//! release archive, and keep the installer version explicit so a change to it
//! arrives as a reviewed diff rather than silently.

const CI_WORKFLOW: &str = include_str!("../.github/workflows/ci.yml");
const INSTALL_WHITAKER_ACTION: &str = "leynos/shared-actions/.github/actions/install-whitaker@";
const SHA_LENGTH: usize = 40;

/// Return the text following `needle`, or `None` when it is absent.
///
/// `str::get` is used rather than an index range because the lint gate denies
/// `clippy::string_slice`.
fn text_after<'a>(haystack: &'a str, needle: &str) -> Option<&'a str> {
    haystack
        .find(needle)
        .and_then(|index| haystack.get(index + needle.len()..))
}

#[test]
fn ci_installs_whitaker_through_the_shared_action() {
    assert!(
        CI_WORKFLOW.contains(INSTALL_WHITAKER_ACTION),
        "CI must install Whitaker through the shared install-whitaker action",
    );
}

#[test]
fn ci_pins_the_shared_action_to_a_commit_sha() {
    let Some(rest) = text_after(CI_WORKFLOW, INSTALL_WHITAKER_ACTION) else {
        panic!("CI does not reference the shared install-whitaker action");
    };
    let reference: String = rest.chars().take(SHA_LENGTH).collect();

    assert!(
        reference.len() == SHA_LENGTH && reference.chars().all(|c| c.is_ascii_hexdigit()),
        "install-whitaker must be pinned to a full commit SHA, found {reference:?}",
    );
}

#[test]
fn ci_requests_an_explicit_installer_version() {
    assert!(
        CI_WORKFLOW.contains("installer-version:"),
        "the install-whitaker step must pass an explicit installer-version",
    );

    let Some(rest) = text_after(CI_WORKFLOW, "WHITAKER_INSTALLER_VERSION: '") else {
        panic!("CI does not define WHITAKER_INSTALLER_VERSION");
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

/// Report whether a workflow line obtains the installer through Cargo.
///
/// Both arms matter: `cargo install` builds the tool from source, and
/// `cargo binstall` fetches it from a registry without the pinned digest the
/// shared action verifies.
fn installs_whitaker_through_cargo(line: &str) -> bool {
    (line.contains("cargo install") || line.contains("cargo binstall"))
        && line.contains("whitaker-installer")
}

#[test]
fn the_predicate_rejects_the_step_this_replaced() {
    for legacy in [
        r#"cargo binstall --no-confirm --locked "whitaker-installer@${WHITAKER_INSTALLER_VERSION}""#,
        r#"cargo install --locked whitaker-installer --version "${WHITAKER_INSTALLER_VERSION}""#,
    ] {
        assert!(
            installs_whitaker_through_cargo(legacy),
            "the guard must reject {legacy:?}",
        );
    }
}

#[test]
fn ci_never_obtains_the_whitaker_installer_through_cargo() {
    // Comments are skipped in both YAML and shell senses: this file explains
    // the command it replaced, and naming it must not trip the guard.
    let offending: Vec<&str> = CI_WORKFLOW
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter(|line| installs_whitaker_through_cargo(line))
        .collect();

    assert!(
        offending.is_empty(),
        "CI must not build or fetch whitaker-installer through Cargo: {offending:?}",
    );
}

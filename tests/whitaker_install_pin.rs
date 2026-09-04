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

#[test]
fn ci_never_builds_the_whitaker_installer_from_source() {
    for forbidden in [
        "cargo install --locked whitaker-installer",
        "cargo binstall",
    ] {
        assert!(
            !CI_WORKFLOW.contains(&format!("{forbidden} whitaker-installer"))
                && !CI_WORKFLOW.contains(&format!("{forbidden}\" whitaker-installer")),
            "CI must not obtain whitaker-installer with {forbidden:?}",
        );
    }
    assert!(
        !CI_WORKFLOW.contains("whitaker-installer@"),
        "CI must not install whitaker-installer from a registry",
    );
}

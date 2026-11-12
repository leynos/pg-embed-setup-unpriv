//! Regression tests for selecting the actual workflow action entry.

use super::*;

/// A well-formed reference in a comment must not hide a malformed `uses` pin.
#[test]
fn well_formed_comment_does_not_mask_malformed_uses() {
    let current = Workflow { text: CI_WORKFLOW }
        .whitaker_step()
        .revision()
        .expect("valid uses entry");
    let actual = format!("uses: {INSTALL_WHITAKER_ACTION}{current}");
    let replacement = format!(
        "# {INSTALL_WHITAKER_ACTION}{current}\n        uses: {INSTALL_WHITAKER_ACTION}main"
    );
    assert_eq!(CI_WORKFLOW.matches(&actual).count(), 1);
    let mutated = CI_WORKFLOW.replacen(&actual, &replacement, 1);
    let error = Workflow { text: &mutated }
        .validate_pin()
        .expect_err("the actual malformed uses entry must fail");
    assert!(error.contains("full lowercase SHA"), "{error}");
}

/// A duplicate direct `uses` key must not be accepted as a valid step.
#[test]
fn duplicate_uses_entries_are_rejected() {
    let current = Workflow { text: CI_WORKFLOW }
        .whitaker_step()
        .revision()
        .expect("valid uses entry");
    let actual = format!("uses: {INSTALL_WHITAKER_ACTION}{current}");
    assert_eq!(CI_WORKFLOW.matches(&actual).count(), 1);
    let mutated = CI_WORKFLOW.replacen(&actual, &format!("{actual}\n        {actual}"), 1);
    let error = Workflow { text: &mutated }
        .validate_pin()
        .expect_err("duplicate uses entries must fail");
    assert!(error.contains("duplicate uses entries"), "{error}");
}

/// A comment cannot hide a different actual installer version input.
#[test]
fn installer_version_comment_does_not_mask_wrong_input() {
    let replacement = format!("# {INSTALLER_VERSION_INPUT}\n          installer-version: '0.0.0'");
    assert_eq!(CI_WORKFLOW.matches(INSTALLER_VERSION_INPUT).count(), 1);
    let mutated = CI_WORKFLOW.replacen(INSTALLER_VERSION_INPUT, &replacement, 1);
    let error = Workflow { text: &mutated }
        .whitaker_step()
        .validate_installer_input()
        .expect_err("a wrong input must not pass because of a comment");
    assert!(
        error.contains("expected one configured installer input"),
        "{error}"
    );
}

/// Two installer version entries do not define an unambiguous input.
#[test]
fn duplicate_installer_version_entries_are_rejected() {
    let replacement = format!("{INSTALLER_VERSION_INPUT}\n          {INSTALLER_VERSION_INPUT}");
    let mutated = CI_WORKFLOW.replacen(INSTALLER_VERSION_INPUT, &replacement, 1);
    let error = Workflow { text: &mutated }
        .whitaker_step()
        .validate_installer_input()
        .expect_err("duplicate installer inputs must fail");
    assert!(
        error.contains("expected one configured installer input"),
        "{error}"
    );
}

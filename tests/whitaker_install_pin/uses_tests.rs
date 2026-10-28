//! Regression tests for selecting the actual workflow action entry.

use super::*;

/// An approved reference in a comment must not hide an unapproved `uses` pin.
#[test]
fn approved_comment_does_not_mask_unapproved_uses() {
    let approved = Workflow { text: CI_WORKFLOW }
        .whitaker_step()
        .revision()
        .expect("valid uses entry");
    let actual = format!("uses: {INSTALL_WHITAKER_ACTION}{approved}");
    let replacement = format!(
        "# {INSTALL_WHITAKER_ACTION}{approved}\n        uses: {INSTALL_WHITAKER_ACTION}{}",
        "0".repeat(SHA_LENGTH)
    );
    assert_eq!(CI_WORKFLOW.matches(&actual).count(), 1);
    let mutated = CI_WORKFLOW.replacen(&actual, &replacement, 1);
    let error = Workflow { text: &mutated }
        .validate_pin()
        .expect_err("the actual unapproved uses entry must fail");
    assert!(error.contains("not approved by QG-002"), "{error}");
}

/// A duplicate direct `uses` key must not be accepted as a valid step.
#[test]
fn duplicate_uses_entries_are_rejected() {
    let approved = Workflow { text: CI_WORKFLOW }
        .whitaker_step()
        .revision()
        .expect("valid uses entry");
    let actual = format!("uses: {INSTALL_WHITAKER_ACTION}{approved}");
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

//! Regression coverage for the `CodeScene` uploader inputs this repository's
//! workflows pass.
//!
//! At the approved pin the shared uploader treats its committed
//! `cli-manifest.json` as the trust anchor for the cs-coverage archive, and it
//! *rejects* a non-empty `installer-checksum` with a hard failure rather than
//! ignoring it. A workflow that still passes the input therefore breaks the
//! upload step as soon as the pin moves, and the `CODESCENE_CLI_SHA256`
//! repository variable that fed it could only ever repeat the manifest digest.
//!
//! Four concerns are asserted, each in its own test so a failure names the
//! defect rather than a bundle:
//!
//! * no workflow passes the deprecated input;
//! * no workflow references the variable that fed it;
//! * every uploader reference is pinned to one approved full SHA;
//! * the dispatch workflow that refreshed the variable is absent.
//!
//! The workflow directory is read rather than a fixed list of files being
//! included, so a workflow added later is covered without anyone remembering
//! to extend this test. Each assertion that ranges over a collection checks
//! the collection has content first: a contract over an empty collection is
//! satisfied by deleting the thing it guards.

use std::path::Path;

use cap_std::{ambient_authority, fs::Dir};

/// Directory holding this repository's own workflow definitions.
const WORKFLOW_DIRECTORY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.github/workflows");
/// Full SHA of the approved `upload-codescene-coverage` pin.
const APPROVED_UPLOADER_PIN: &str = "a5765019912a8ab6882b12db049c7cde635f3a85";
/// Marker preceding the pin in a workflow's `uses:` expression.
const UPLOADER_REFERENCE: &str = "leynos/shared-actions/.github/actions/upload-codescene-coverage@";
/// Input the uploader rejects outright at the approved pin.
const DEPRECATED_INPUT: &str = "installer-checksum";
/// Repository variable whose only consumer was the deprecated input.
const DEPRECATED_VARIABLE: &str = "CODESCENE_CLI_SHA256";
/// Dispatch workflow that refreshed the now-unread repository variable.
const REFRESH_WORKFLOW: &str = "get-codescene-sha.yml";

/// Unwrap `result`, reporting `context` when it failed.
///
/// A contract that cannot read its own inputs has no answer to give, so the
/// failure is raised here with the context naming the filesystem operation.
/// `expect` is avoided because the lint gate denies it outside test-only code.
fn require<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

/// Open the workflow directory as a capability handle.
///
/// `cap-std` is used rather than `std::fs` because the Whitaker suite denies
/// ambient filesystem operations outside the crates `dylint.toml` names, and
/// this contract is not part of the ambient bootstrap surface.
fn open_workflow_directory() -> Dir {
    require(
        Dir::open_ambient_dir(WORKFLOW_DIRECTORY, ambient_authority()),
        &format!("cannot open {WORKFLOW_DIRECTORY}"),
    )
}

/// Report whether `name` denotes a workflow document rather than any other
/// file.
fn is_workflow_file(name: &str) -> bool {
    Path::new(name).extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("yml") || extension.eq_ignore_ascii_case("yaml")
    })
}

/// Return every workflow file name paired with its text, sorted so a failure
/// names the same file every run.
fn workflows() -> Vec<(String, String)> {
    let directory = open_workflow_directory();
    let entries = require(
        directory.entries(),
        &format!("cannot list {WORKFLOW_DIRECTORY}"),
    );
    let mut found: Vec<(String, String)> = entries
        .map(|entry| {
            let listed = require(entry, "cannot read a workflow directory entry");
            listed.file_name().to_string_lossy().into_owned()
        })
        .filter(|name| is_workflow_file(name))
        .map(|name| {
            let contents = require(
                directory.read_to_string(&name),
                &format!("cannot read workflow {name}"),
            );
            (name, contents)
        })
        .collect();
    found.sort();
    found
}

/// Assert that no workflow mentions `needle`.
///
/// Both containment clauses have the same shape, so they share one assertion
/// rather than being copied: `reason` names why the mention is wrong, and the
/// caller stays a single test so a failure still names one defect.
fn assert_no_workflow_mentions(needle: &str, reason: &str) {
    let workflows = workflows();
    assert!(
        !workflows.is_empty(),
        "no workflow files were examined, so this contract would pass vacuously",
    );
    let offenders: Vec<&String> = workflows
        .iter()
        .filter(|(_, contents)| contents.contains(needle))
        .map(|(name, _)| name)
        .collect();
    assert!(
        offenders.is_empty(),
        "{needle} {reason}; remove it from {offenders:?}",
    );
}

/// Return every uploader pin found, paired with the workflow carrying it.
///
/// A pin is the run of non-whitespace characters following the action
/// reference, so a tag, a branch name or an empty value is reported unchanged
/// and fails the allowlist below rather than being silently accepted.
fn uploader_pins() -> Vec<(String, String)> {
    workflows()
        .into_iter()
        .flat_map(|(name, contents)| {
            contents
                .split(UPLOADER_REFERENCE)
                .skip(1)
                .map(|tail| {
                    let pin = tail
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned();
                    (name.clone(), pin)
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// The uploader rejects a non-empty value, so no workflow may pass the input.
#[test]
fn no_workflow_passes_the_deprecated_installer_checksum() {
    assert_no_workflow_mentions(
        DEPRECATED_INPUT,
        &format!("is deprecated and rejected by the uploader at {APPROVED_UPLOADER_PIN}"),
    );
}

/// The variable existed only to feed the rejected input, so it must go too.
#[test]
fn no_workflow_references_the_deprecated_checksum_variable() {
    assert_no_workflow_mentions(
        DEPRECATED_VARIABLE,
        "fed the deprecated installer checksum and has no remaining consumer",
    );
}

/// One approved SHA, asserted as an allowlist rather than as a floor.
///
/// A floor would require ordering SHAs, which cannot be computed from a
/// checkout. Naming the approved pin keeps the contract hermetic and fails
/// closed on any other value, including a tag or a branch name.
#[test]
fn every_uploader_reference_is_pinned_to_the_approved_sha() {
    let pins = uploader_pins();

    assert!(
        !pins.is_empty(),
        "no upload-codescene-coverage reference was found, so this contract would pass vacuously; \
         this repository is expected to send coverage to CodeScene",
    );
    let wrong: Vec<&(String, String)> = pins
        .iter()
        .filter(|(_, pin)| pin != APPROVED_UPLOADER_PIN)
        .collect();
    assert!(
        wrong.is_empty(),
        "every upload-codescene-coverage reference must be pinned to {APPROVED_UPLOADER_PIN}; \
         found {wrong:?}",
    );
}

/// Nothing consumes the variable it wrote, so the workflow is dead code.
#[test]
fn the_checksum_refresh_workflow_is_absent() {
    let directory = open_workflow_directory();

    assert!(
        !directory.exists(REFRESH_WORKFLOW),
        "{REFRESH_WORKFLOW} refreshed {DEPRECATED_VARIABLE}, which no workflow reads any more; \
         delete it rather than leaving a dispatch that writes an unused repository variable",
    );
}

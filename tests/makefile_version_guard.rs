//! Regression coverage for the Makefile's package-version guard.

use std::{path::Path, process::Command};

use makefile_lossless::Makefile;

const MAKEFILE: &str = include_str!("../Makefile");
const VERSION_ERROR: &str =
    "VERSION is empty; set [package].version in Cargo.toml or pass VERSION explicitly";

fn repository_root() -> &'static Path { Path::new(env!("CARGO_MANIFEST_DIR")) }

#[test]
fn makefile_parses_without_recovery() {
    let parse = Makefile::parse(MAKEFILE);

    assert!(
        parse.ok(),
        "Makefile required parser recovery: {:?}",
        parse.positioned_errors(),
    );
}

#[test]
fn empty_version_still_fails_during_makefile_read() {
    let output = Command::new("make")
        .args(["--no-print-directory", "--dry-run", "VERSION=", "lint"])
        .current_dir(repository_root())
        .output()
        .unwrap_or_else(|error| panic!("failed to invoke make: {error}"));
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        !output.status.success(),
        "empty VERSION unexpectedly succeeded"
    );
    assert!(
        stderr.contains(VERSION_ERROR),
        "empty VERSION reported an unexpected error:\n{stderr}",
    );
}

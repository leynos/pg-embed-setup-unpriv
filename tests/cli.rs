//! Command-line interface behaviour tests for the setup binary.

use color_eyre::eyre::{Result, ensure};
use rstest::rstest;
use std::process::Command;

#[derive(Clone, Copy)]
enum CliMetadataExpectation {
    Version,
    Help,
}

fn run_cli_metadata_flag(flag: &str) -> Result<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_pg_embedded_setup_unpriv"))
        .arg(flag)
        .env("PG_VERSION_REQ", "not a valid semver requirement")
        .env("PG_TEST_BACKEND", "definitely_unsupported_backend")
        .env_remove("GITHUB_TOKEN")
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure!(
        output.status.success(),
        "expected {flag} to succeed without bootstrap; stdout: {stdout}; stderr: {stderr}"
    );
    ensure!(
        !stderr.contains("PG_VERSION_REQ invalid semver spec"),
        "{flag} must not parse bootstrap configuration; stderr: {stderr}"
    );
    ensure!(
        !stderr.contains("SKIP-TEST-CLUSTER"),
        "{flag} must not validate backend selection; stderr: {stderr}"
    );

    Ok(stdout.into_owned())
}

#[rstest]
#[case::version("--version", CliMetadataExpectation::Version)]
#[case::help("--help", CliMetadataExpectation::Help)]
fn version_flag_prints_version_without_bootstrap(
    #[case] flag: &str,
    #[case] expectation: CliMetadataExpectation,
) -> Result<()> {
    let stdout = run_cli_metadata_flag(flag)?;

    match expectation {
        CliMetadataExpectation::Version => ensure!(
            stdout.contains(env!("CARGO_PKG_VERSION")),
            "expected stdout to include package version {}; stdout: {stdout}",
            env!("CARGO_PKG_VERSION")
        ),
        CliMetadataExpectation::Help => assert_help_output(stdout.as_str())?,
    }

    Ok(())
}

fn assert_help_output(stdout: &str) -> Result<()> {
    ensure!(
        stdout.contains("PG_VERSION_REQ"),
        "expected stdout to document PG_VERSION_REQ; stdout: {stdout}"
    );
    ensure!(
        stdout.contains("PG_BINARY_CACHE_DIR"),
        "expected stdout to document PG_BINARY_CACHE_DIR; stdout: {stdout}"
    );
    let normalized_stdout = stdout.replace(
        "Usage: pg_embedded_setup_unpriv.exe",
        "Usage: pg_embedded_setup_unpriv",
    );
    insta::assert_snapshot!(normalized_stdout.as_str(), @r"
Initialises postgresql_embedded clusters with platform-appropriate setup

Usage: pg_embedded_setup_unpriv

Options:
  -h, --help     Print help
  -V, --version  Print version

Configuration is read from environment variables:
  PG_VERSION_REQ          PostgreSQL semver requirement.
  PG_PORT                 PostgreSQL port.
  PG_SUPERUSER            Administrative PostgreSQL user.
  PG_PASSWORD             Administrative PostgreSQL password.
  PG_DATA_DIR             PostgreSQL data directory.
  PG_RUNTIME_DIR          PostgreSQL binary installation directory.
  PG_LOCALE               initdb locale.
  PG_ENCODING             initdb encoding.
  PG_BINARY_CACHE_DIR     Shared PostgreSQL binary cache directory.
");

    Ok(())
}

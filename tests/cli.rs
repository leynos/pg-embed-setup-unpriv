//! Command-line interface behaviour tests for the setup binary.

use color_eyre::eyre::{Result, ensure};
use std::process::Command;

#[test]
fn version_flag_prints_version_without_bootstrap() -> Result<()> {
    let output = Command::new(env!("CARGO_BIN_EXE_pg_embedded_setup_unpriv"))
        .arg("--version")
        .env("PG_VERSION_REQ", "not a valid semver requirement")
        .env("PG_TEST_BACKEND", "definitely_unsupported_backend")
        .env_remove("GITHUB_TOKEN")
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    ensure!(
        output.status.success(),
        "expected --version to succeed without bootstrap; stdout: {stdout}; stderr: {stderr}"
    );
    ensure!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "expected stdout to include package version {}; stdout: {stdout}",
        env!("CARGO_PKG_VERSION")
    );
    ensure!(
        !stderr.contains("PG_VERSION_REQ invalid semver spec"),
        "--version must not parse bootstrap configuration; stderr: {stderr}"
    );
    ensure!(
        !stderr.contains("SKIP-TEST-CLUSTER"),
        "--version must not validate backend selection; stderr: {stderr}"
    );

    Ok(())
}

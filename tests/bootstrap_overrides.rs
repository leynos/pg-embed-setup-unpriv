//! Behavioural coverage at the public bootstrap boundary for the
//! `PG_EMBED_ROOT` and `PG_MAX_CONNECTIONS` overrides: the environment a
//! consumer sets is what `bootstrap_for_tests()` acts on.
#![cfg(unix)]

use std::ffi::OsString;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, ensure, eyre};
use pg_embedded_setup_unpriv::{
    BootstrapResult,
    TestBootstrapSettings,
    bootstrap_for_tests,
    test_support::capture_debug_logs,
};
use rstest::rstest;

#[path = "support/env.rs"]
mod env;

/// A fresh root directory for one scenario.
fn scratch_root() -> Result<(tempfile::TempDir, Utf8PathBuf)> {
    let temp = tempfile::tempdir()?;
    let root = Utf8PathBuf::from_path_buf(temp.path().join("pg"))
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    Ok((temp, root))
}

/// Runs the public bootstrap with `PG_EMBED_ROOT` and the given extra
/// variables, clearing the two leaf overrides so the root decides.
fn bootstrap_under(
    root: &Utf8PathBuf,
    extra: &[(&str, Option<&str>)],
) -> BootstrapResult<TestBootstrapSettings> {
    let mut vars = env::build_env([("PG_EMBED_ROOT", root.as_str())]);
    vars.push((OsString::from("PG_RUNTIME_DIR"), None));
    vars.push((OsString::from("PG_DATA_DIR"), None));
    for (key, value) in extra {
        vars.push((OsString::from(key), value.map(OsString::from)));
    }
    env::with_scoped_env(vars, bootstrap_for_tests)
}

/// `PG_EMBED_ROOT` alone places both leaves beneath the root.
#[test]
fn embed_root_derives_both_leaves_at_the_public_boundary() -> Result<()> {
    let (_temp, root) = scratch_root()?;
    let bootstrap = bootstrap_under(&root, &[])?;
    ensure!(
        bootstrap.settings.installation_dir == root.join("install").as_std_path(),
        "install leaf not under root: {}",
        bootstrap.settings.installation_dir.display()
    );
    ensure!(
        bootstrap.settings.data_dir == root.join("data").as_std_path(),
        "data leaf not under root: {}",
        bootstrap.settings.data_dir.display()
    );
    Ok(())
}

/// An explicit leaf variable still wins over the root-derived default.
#[rstest]
#[case::runtime_dir("PG_RUNTIME_DIR")]
#[case::data_dir("PG_DATA_DIR")]
fn explicit_leaf_wins_over_embed_root_at_the_public_boundary(#[case] leaf: &str) -> Result<()> {
    let (_temp, root) = scratch_root()?;
    let explicit = root.join("elsewhere");
    let bootstrap = bootstrap_under(&root, &[(leaf, Some(explicit.as_str()))])?;
    let observed = if leaf == "PG_RUNTIME_DIR" {
        &bootstrap.settings.installation_dir
    } else {
        &bootstrap.settings.data_dir
    };
    ensure!(
        observed == explicit.as_std_path(),
        "{leaf} was not honoured: {}",
        observed.display()
    );
    Ok(())
}

/// `PG_MAX_CONNECTIONS` replaces the test cap of 20 at the public boundary,
/// and a value below the floor is refused before any directory is touched.
#[rstest]
#[case::raised("120", Some("120"))]
#[case::below_floor("2", None)]
fn max_connections_at_the_public_boundary(
    #[case] value: &str,
    #[case] expected: Option<&str>,
) -> Result<()> {
    let (_temp, root) = scratch_root()?;
    let outcome = bootstrap_under(&root, &[("PG_MAX_CONNECTIONS", Some(value))]);
    match (outcome, expected) {
        (Ok(bootstrap), Some(limit)) => ensure!(
            bootstrap
                .settings
                .configuration
                .get("max_connections")
                .is_some_and(|observed| observed == limit),
            "max_connections did not follow PG_MAX_CONNECTIONS={value}"
        ),
        (Err(err), None) => ensure!(
            err.to_string().contains("PG_MAX_CONNECTIONS"),
            "unexpected error: {err}"
        ),
        (Ok(_), None) => color_eyre::eyre::bail!("PG_MAX_CONNECTIONS={value} must be rejected"),
        (Err(err), Some(_)) => color_eyre::eyre::bail!("unexpected failure: {err}"),
    }
    Ok(())
}

/// The `settings_decision` event reports the connection limit the server will
/// actually run at, not the raw `PG_MAX_CONNECTIONS` option.
///
/// The two differ for the case that matters most: a test bootstrap that sets
/// no override still runs at 20, because `apply_worker_limits` puts it in the
/// configuration. An event carrying the unset option would tell an operator
/// nothing about the running server.
#[rstest]
#[case::default_test_cap(None, "20")]
#[case::explicit_override(Some("64"), "64")]
fn settings_decision_reports_the_effective_connection_limit(
    #[case] override_value: Option<&str>,
    #[case] expected: &str,
) -> Result<()> {
    let (_temp, root) = scratch_root()?;
    // Always name the variable so the default case clears any ambient value.
    let extra: Vec<(&str, Option<&str>)> = vec![("PG_MAX_CONNECTIONS", override_value)];
    let (logs, outcome) = capture_debug_logs(|| bootstrap_under(&root, &extra));
    outcome?;
    let decision = logs
        .iter()
        .find(|line| line.contains("settings_decision"))
        .ok_or_else(|| eyre!("no settings_decision event in:\n{}", logs.join("\n")))?;
    ensure!(
        decision.contains(&format!("max_connections=\"{expected}\"")),
        "settings_decision did not report {expected}: {decision}"
    );
    ensure!(
        decision.contains(root.join("install").as_str())
            && decision.contains(root.join("data").as_str()),
        "settings_decision did not name both resolved directories: {decision}"
    );
    ensure!(
        decision.contains("install_default=true") && decision.contains("data_default=true"),
        "settings_decision did not report both leaves as derived: {decision}"
    );
    ensure!(
        decision.contains("root_source=Override"),
        "settings_decision did not attribute the root to PG_EMBED_ROOT: {decision}"
    );
    Ok(())
}

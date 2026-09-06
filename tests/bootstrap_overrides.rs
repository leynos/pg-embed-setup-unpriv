//! Behavioural coverage at the public bootstrap boundary for the
//! `PG_EMBED_ROOT` and `PG_MAX_CONNECTIONS` overrides: the environment a
//! consumer sets is what `bootstrap_for_tests()` acts on.
#![cfg(unix)]

use std::ffi::OsString;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, ensure, eyre};
use pg_embedded_setup_unpriv::{BootstrapResult, TestBootstrapSettings, bootstrap_for_tests};
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

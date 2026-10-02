//! The disposable-cluster defaults and the `initdb --no-sync` step.

use color_eyre::eyre::{Result, ensure};
use postgresql_embedded::Settings;
use rstest::rstest;

use super::{
    DISPOSABLE_SETTINGS,
    apply_disposable_defaults,
    initialize_without_sync,
    is_disposable,
    is_durable_requested,
};
use crate::PgEnvCfg;

/// The configuration a test bootstrap produces from an environment with the
/// given `PG_EMBED_DURABLE`, built without touching the process environment.
fn test_settings(durable: bool) -> Result<Settings> {
    let mut settings = PgEnvCfg::default().to_settings_with_context(false)?;
    apply_disposable_defaults(&mut settings, durable);
    Ok(settings)
}

/// By default a test cluster is non-durable: all three server settings are off,
/// so the cluster counts as disposable.
#[test]
fn a_test_cluster_is_non_durable_by_default() -> Result<()> {
    let settings = test_settings(false)?;
    for (key, value) in DISPOSABLE_SETTINGS {
        let found = settings.configuration.get(key).map(String::as_str);
        ensure!(found == Some(value), "{key} is {found:?}, expected {value}");
    }
    ensure!(
        is_disposable(&settings),
        "the default cluster is disposable"
    );
    Ok(())
}

/// The opt-out restores durability: `fsync = on` is stated, because the server
/// is otherwise started with `-F`, the other two settings stay unset so
/// `PostgreSQL`'s own `on` applies, and the cluster is not disposable.
#[test]
fn opting_out_restores_the_durable_settings() -> Result<()> {
    let settings = test_settings(true)?;
    let fsync = settings.configuration.get("fsync").map(String::as_str);
    ensure!(fsync == Some("on"), "fsync is {fsync:?}, expected on");
    for key in ["synchronous_commit", "full_page_writes"] {
        ensure!(
            !settings.configuration.contains_key(key),
            "{key} must stay unset"
        );
    }
    ensure!(!is_disposable(&settings), "an opted-out cluster is durable");
    Ok(())
}

/// A durability setting the caller chose is kept, not overwritten.
#[test]
fn a_callers_durability_setting_wins() {
    let mut settings = Settings::default();
    settings.configuration.insert("fsync".into(), "on".into());
    apply_disposable_defaults(&mut settings, false);
    assert_eq!(
        settings.configuration.get("fsync").map(String::as_str),
        Some("on")
    );
    assert_eq!(
        settings
            .configuration
            .get("synchronous_commit")
            .map(String::as_str),
        Some("off")
    );
}

/// Only the value `1` asks for a durable cluster.
#[rstest]
#[case::one(Some("1"), true)]
#[case::unset(None, false)]
#[case::zero(Some("0"), false)]
#[case::yes(Some("yes"), false)]
#[case::empty(Some(""), false)]
fn only_the_value_one_opts_out(#[case] value: Option<&str>, #[case] durable: bool) {
    let lookup = |key: &str| {
        (key == super::DURABLE_VAR)
            .then(|| value.map(std::ffi::OsString::from))
            .flatten()
    };
    assert_eq!(is_durable_requested(lookup), durable);
}

/// The setup-only path never asks for disposable settings: settings built the
/// non-test way carry none of them, so `initdb` is left to upstream's durable one.
#[test]
fn the_setup_only_settings_stay_durable() -> Result<()> {
    let settings = PgEnvCfg::default().to_settings()?;
    ensure!(!is_disposable(&settings), "setup-only settings are durable");
    ensure!(
        !initialize_without_sync(&settings)?,
        "initdb is left to upstream"
    );
    Ok(())
}

/// The test bootstrap's settings, built by the real entry point from the real
/// environment, are non-durable, and the non-test ones are not: this is the wiring
/// the pure helpers above cannot show. The process has no `PG_EMBED_DURABLE`.
#[test]
fn the_test_entry_point_applies_the_defaults_and_the_other_does_not() -> Result<()> {
    if std::env::var_os(super::DURABLE_VAR).is_some() {
        return Ok(());
    }
    let cfg = PgEnvCfg::default();
    ensure!(
        is_disposable(&cfg.to_settings_for_tests()?),
        "test settings are disposable"
    );
    ensure!(
        !is_disposable(&cfg.to_settings()?),
        "plain settings are not"
    );
    Ok(())
}

/// A stand-in `initdb`, an executable script that records its arguments and
/// creates `postgresql.conf` as the real one does.
#[cfg(unix)]
fn fake_initdb(install: &std::path::Path, record: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let bin = install.join("bin");
    std::fs::create_dir_all(&bin)?;
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {record}\nwhile [ $# -gt 0 ]; do\n  case $1 in\n    \
         --pgdata) mkdir -p \"$2\"; touch \"$2/postgresql.conf\" ;;\n  esac\n  shift\ndone\n",
        record = record.display()
    );
    let initdb = bin.join("initdb");
    std::fs::write(&initdb, script)?;
    std::fs::set_permissions(&initdb, std::fs::Permissions::from_mode(0o755))
}

/// For a disposable cluster with its binaries installed, the step runs `initdb`
/// with `--no-sync`, initializes the directory, and does nothing the second time;
/// for a durable cluster it never runs `initdb` at all.
#[cfg(unix)]
#[rstest]
#[case::disposable(false, true)]
#[case::durable(true, false)]
fn initdb_runs_with_no_sync_only_for_a_disposable_cluster(
    #[case] durable: bool,
    #[case] runs: bool,
) -> color_eyre::eyre::Result<()> {
    use color_eyre::eyre::ensure;
    let dir = tempfile::tempdir()?;
    let record = dir.path().join("args");
    fake_initdb(&dir.path().join("install"), &record)?;
    let mut settings = Settings {
        installation_dir: dir.path().join("install"),
        data_dir: dir.path().join("data"),
        password_file: dir.path().join(".pgpass"),
        ..Settings::default()
    };
    apply_disposable_defaults(&mut settings, durable);

    let first = initialize_without_sync(&settings)?;
    let second = initialize_without_sync(&settings)?;

    ensure!(first == runs, "first call returned {first}");
    ensure!(!second, "an initialized directory is left alone");
    if runs {
        let args = std::fs::read_to_string(&record)?;
        ensure!(
            args.lines().any(|line| line == "--no-sync"),
            "arguments: {args}"
        );
    } else {
        ensure!(
            !record.exists(),
            "initdb must not run for a durable cluster"
        );
    }
    Ok(())
}

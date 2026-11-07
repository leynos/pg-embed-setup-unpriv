//! Behavioural coverage for the explicit-settings bootstrap constructor.
//!
//! These tests set conflicting ambient `PG_*` variables and assert that
//! [`bootstrap_for_tests_with`] uses the supplied [`TestBootstrapConfig`]
//! instead, whilst [`bootstrap_for_tests`] keeps its ambient behaviour.
#![cfg(unix)]

use std::{ffi::OsString, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, bail, ensure, eyre};
use nix::unistd::geteuid;
use pg_embedded_setup_unpriv::{
    BootstrapErrorKind,
    BootstrapResult,
    PgEnvCfg,
    TestBootstrapConfig,
    TestBootstrapSettings,
    bootstrap_for_tests,
    bootstrap_for_tests_with,
    test_support,
};
use rstest::rstest;

#[path = "support/env.rs"]
mod env;

/// Temporary directories shared by one scenario.
struct Scratch {
    _temp: tempfile::TempDir,
    root: Utf8PathBuf,
    tzdir: Utf8PathBuf,
}

fn scratch() -> Result<Scratch> {
    let temp = tempfile::tempdir()?;
    let root = Utf8PathBuf::from_path_buf(temp.path().join("pg"))
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    let tzdir = Utf8PathBuf::from_path_buf(temp.path().join("zoneinfo"))
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    std::fs::create_dir_all(tzdir.as_std_path())?;
    Ok(Scratch {
        _temp: temp,
        root,
        tzdir,
    })
}

/// Runs the explicit bootstrap with a controlled ambient environment.
///
/// Every variable the explicit path is meant to ignore is cleared first, then
/// `extra` is applied so a scenario can prove the explicit configuration wins.
/// `TZDIR` points at an existing temporary directory because time zone
/// preparation remains an ambient input.
fn bootstrap_with(
    scratch: &Scratch,
    config: impl Into<TestBootstrapConfig>,
    extra: &[(&str, Option<&str>)],
) -> BootstrapResult<TestBootstrapSettings> {
    let worker_config = ensure_test_worker(config.into());
    let mut vars = env::build_env([("PG_EMBED_ROOT", scratch.root.as_str())]);
    for key in [
        "PG_RUNTIME_DIR",
        "PG_DATA_DIR",
        "PG_MAX_CONNECTIONS",
        "PG_EMBED_DURABLE",
        "TZ",
    ] {
        vars.push((OsString::from(key), None));
    }
    vars.push((
        OsString::from("TZDIR"),
        Some(OsString::from(scratch.tzdir.as_str())),
    ));
    for (key, value) in extra {
        vars.push((OsString::from(*key), value.map(OsString::from)));
    }

    env::with_scoped_env(vars, || bootstrap_for_tests_with(worker_config))
}

fn explicit_cfg(scratch: &Scratch) -> PgEnvCfg {
    PgEnvCfg {
        embed_root: Some(scratch.root.clone()),
        ..PgEnvCfg::default()
    }
}

/// Resolves the staged test worker as a UTF-8 path, if one is available.
fn test_worker() -> Option<Utf8PathBuf> {
    let worker = test_support::worker_binary_for_tests()?;
    Utf8PathBuf::from_path_buf(PathBuf::from(worker)).ok()
}

/// Supplies the test worker when the caller did not choose one.
///
/// The explicit path ignores `PG_EMBEDDED_WORKER`, so a root run needs the
/// worker passed through the configuration rather than through the ambient
/// environment. An explicitly configured worker is always preserved.
fn ensure_test_worker(config: TestBootstrapConfig) -> TestBootstrapConfig {
    if config.worker_binary().is_some() {
        return config;
    }
    match test_worker() {
        Some(worker) => config.with_worker_binary(worker),
        None => config,
    }
}

/// Skips a scenario that cannot run as root without a worker binary.
fn skip_without_test_worker(test_name: &str) -> bool {
    if !geteuid().is_root() || test_worker().is_some() {
        return false;
    }
    tracing::warn!("skipping {test_name}: no worker binary is available for a root run");
    true
}

#[test]
fn explicit_configuration_wins_over_conflicting_environment() -> Result<()> {
    if skip_without_test_worker("explicit configuration precedence") {
        return Ok(());
    }

    let scratch = scratch()?;
    let cfg = PgEnvCfg {
        superuser: Some("explicit_user".to_owned()),
        password: Some("explicit_pw".to_owned()),
        port: Some(22_222),
        ..explicit_cfg(&scratch)
    };

    let bootstrap = bootstrap_with(
        &scratch,
        cfg,
        &[
            ("PG_SUPERUSER", Some("ambient_user")),
            ("PG_PASSWORD", Some("ambient_pw")),
            ("PG_PORT", Some("11111")),
            ("PG_MAX_CONNECTIONS", Some("99")),
        ],
    )?;

    ensure!(
        bootstrap.settings.username == "explicit_user",
        "explicit superuser was not honoured: {}",
        bootstrap.settings.username
    );
    ensure!(
        bootstrap.settings.password == "explicit_pw",
        "explicit password was not honoured"
    );
    ensure!(
        bootstrap.settings.port == 22_222,
        "explicit port was not honoured: {}",
        bootstrap.settings.port
    );
    ensure!(
        bootstrap
            .settings
            .configuration
            .get("max_connections")
            .is_some_and(|value| value == "20"),
        "an unset explicit field must keep the test default, not the ambient override: {:?}",
        bootstrap.settings.configuration.get("max_connections")
    );
    Ok(())
}

#[test]
fn explicit_configuration_ignores_pg_test_backend() -> Result<()> {
    if skip_without_test_worker("explicit backend selection") {
        return Ok(());
    }

    let scratch = scratch()?;
    let explicit = bootstrap_with(
        &scratch,
        explicit_cfg(&scratch),
        &[("PG_TEST_BACKEND", Some("sqlite"))],
    );
    ensure!(
        explicit.is_ok(),
        "explicit bootstrap must ignore PG_TEST_BACKEND: {:?}",
        explicit.err()
    );

    let ambient = env::with_scoped_env(
        env::build_env([("PG_TEST_BACKEND", "sqlite")]),
        bootstrap_for_tests,
    );
    let err = ambient.expect_err("ambient bootstrap must reject an unsupported backend");
    ensure!(
        err.to_string().contains("SKIP-TEST-CLUSTER"),
        "ambient failure should carry the skip marker: {err}"
    );
    Ok(())
}

#[rstest]
#[case::defaults_when_ambient_invalid(None, Some(Duration::from_secs(15)))]
#[case::explicit_timeout_wins(Some(Duration::from_secs(30)), Some(Duration::from_secs(30)))]
#[case::zero_rejected(Some(Duration::ZERO), None)]
#[case::over_limit_rejected(Some(Duration::from_secs(601)), None)]
fn explicit_shutdown_timeout_ignores_ambient_value(
    #[case] explicit: Option<Duration>,
    #[case] expected: Option<Duration>,
) -> Result<()> {
    if skip_without_test_worker("explicit shutdown timeout") {
        return Ok(());
    }

    let scratch = scratch()?;
    let mut config = TestBootstrapConfig::new(explicit_cfg(&scratch));
    if let Some(timeout) = explicit {
        config = config.with_shutdown_timeout(timeout);
    }

    let outcome = bootstrap_with(
        &scratch,
        config,
        &[("PG_SHUTDOWN_TIMEOUT_SECS", Some("not-a-number"))],
    );
    match (outcome, expected) {
        (Ok(bootstrap), Some(expected_timeout)) => ensure!(
            bootstrap.shutdown_timeout == expected_timeout,
            "expected {expected_timeout:?}, got {:?}",
            bootstrap.shutdown_timeout
        ),
        (Err(_), None) => {}
        (Ok(bootstrap), None) => bail!(
            "expected the explicit timeout to be rejected, got {:?}",
            bootstrap.shutdown_timeout
        ),
        (Err(err), Some(_)) => bail!("unexpected failure: {err}"),
    }
    Ok(())
}

#[test]
fn explicit_release_url_reaches_settings() -> Result<()> {
    if skip_without_test_worker("explicit release URL") {
        return Ok(());
    }

    let scratch = scratch()?;
    let config = TestBootstrapConfig::new(explicit_cfg(&scratch))
        .with_releases_url("https://example.invalid/releases");

    let bootstrap = bootstrap_with(&scratch, config, &[])?;
    ensure!(
        bootstrap.settings.releases_url == "https://example.invalid/releases",
        "release URL did not reach settings: {}",
        bootstrap.settings.releases_url
    );
    Ok(())
}

#[rstest]
#[case::empty("")]
#[case::whitespace("   ")]
fn explicit_release_url_rejects_empty(#[case] url: &str) -> Result<()> {
    if skip_without_test_worker("empty explicit release URL") {
        return Ok(());
    }

    let scratch = scratch()?;
    let config = TestBootstrapConfig::new(explicit_cfg(&scratch)).with_releases_url(url);

    let err =
        bootstrap_with(&scratch, config, &[]).expect_err("an empty release URL must be rejected");
    ensure!(
        err.to_string().contains("releases URL"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn explicit_max_connections_below_floor_is_rejected() -> Result<()> {
    if skip_without_test_worker("explicit max_connections floor") {
        return Ok(());
    }

    let scratch = scratch()?;
    let cfg = PgEnvCfg {
        max_connections: Some(2),
        ..explicit_cfg(&scratch)
    };

    let err = bootstrap_with(&scratch, cfg, &[])
        .expect_err("max_connections below the floor must be rejected");
    ensure!(
        err.to_string().contains("PG_MAX_CONNECTIONS"),
        "unexpected error: {err}"
    );
    Ok(())
}

fn skip_worker_validation_when_unprivileged(test_name: &str) -> bool {
    if geteuid().is_root() {
        return false;
    }

    tracing::warn!("skipping {test_name}: requires root privileges");
    true
}

#[test]
fn explicit_worker_binary_is_validated_when_root() -> Result<()> {
    if skip_worker_validation_when_unprivileged("explicit worker binary validation") {
        return Ok(());
    }

    let scratch = scratch()?;
    let missing = scratch.root.join("missing-worker");
    let config = TestBootstrapConfig::new(explicit_cfg(&scratch)).with_worker_binary(missing);

    let err = bootstrap_with(&scratch, config, &[])
        .expect_err("a missing explicit worker binary must be rejected");
    ensure!(
        err.kind() == BootstrapErrorKind::WorkerBinaryMissing,
        "expected WorkerBinaryMissing, got {:?}",
        err.kind()
    );
    Ok(())
}

#[test]
fn explicit_worker_binary_must_be_a_regular_file_when_root() -> Result<()> {
    if skip_worker_validation_when_unprivileged("explicit worker directory validation") {
        return Ok(());
    }

    let scratch = scratch()?;
    std::fs::create_dir_all(scratch.root.as_std_path())?;
    let config =
        TestBootstrapConfig::new(explicit_cfg(&scratch)).with_worker_binary(scratch.root.clone());

    let err = bootstrap_with(&scratch, config, &[])
        .expect_err("a directory worker path must be rejected");
    ensure!(
        err.to_string().contains("must reference a regular file"),
        "unexpected error: {err}"
    );
    Ok(())
}

#[test]
fn explicit_worker_binary_must_be_executable_when_root() -> Result<()> {
    if skip_worker_validation_when_unprivileged("explicit worker executability validation") {
        return Ok(());
    }

    let scratch = scratch()?;
    std::fs::create_dir_all(scratch.root.as_std_path())?;
    let worker = scratch.root.join("pg_worker_stub");
    std::fs::write(worker.as_std_path(), b"#!/bin/sh\nexit 0\n")?;
    let mut perms = std::fs::metadata(worker.as_std_path())?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(worker.as_std_path(), perms)?;

    let config = TestBootstrapConfig::new(explicit_cfg(&scratch)).with_worker_binary(worker);
    let err = bootstrap_with(&scratch, config, &[])
        .expect_err("a non-executable worker must be rejected");
    ensure!(
        err.to_string().contains("must be executable"),
        "unexpected error: {err}"
    );
    Ok(())
}

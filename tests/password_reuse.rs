//! Behavioural coverage at the public bootstrap boundary: a second
//! `bootstrap_for_tests()` against a data directory that already holds a
//! cluster adopts the password the first bootstrap stored, which is the shape
//! `shared_cluster_handle()` meets when a later test process reuses the
//! cluster an earlier one initialized.
#![cfg(unix)]

use std::ffi::OsString;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, bail, ensure, eyre};
use pg_embedded_setup_unpriv::{
    BootstrapErrorKind,
    BootstrapResult,
    TestBootstrapSettings,
    bootstrap_for_tests,
};
use rstest::rstest;

#[path = "support/env.rs"]
mod env;

/// A staged install and data directory pair: the data directory carries the
/// `PG_VERSION` marker of an initialized cluster and the install tree holds
/// the stored password when `stored` is given.
struct StagedCluster {
    _temp: tempfile::TempDir,
    install: Utf8PathBuf,
    data: Utf8PathBuf,
}

/// Stages the directories and marker files for one scenario.
fn staged_cluster(stored: Option<&str>) -> Result<StagedCluster> {
    let temp = tempfile::tempdir()?;
    let base = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    let install = base.join("install");
    let data = base.join("data");
    std::fs::create_dir_all(&install)?;
    std::fs::create_dir_all(&data)?;
    std::fs::write(data.join("PG_VERSION"), "17\n")?;
    if let Some(value) = stored {
        std::fs::write(install.join(".pgpass"), value)?;
    }
    Ok(StagedCluster {
        _temp: temp,
        install,
        data,
    })
}

/// Runs the public bootstrap against the staged directories with `PG_PASSWORD`
/// set or cleared as the scenario requires.
fn bootstrap_with(
    staged: &StagedCluster,
    password: Option<&str>,
) -> BootstrapResult<TestBootstrapSettings> {
    let mut vars = env::build_env([
        ("PG_RUNTIME_DIR", staged.install.as_str()),
        ("PG_DATA_DIR", staged.data.as_str()),
    ]);
    vars.push((OsString::from("PG_PASSWORD"), password.map(OsString::from)));
    env::with_scoped_env(vars, bootstrap_for_tests)
}

/// The public bootstrap adopts a stored password, keeps an explicit one, and
/// fails with a named error kind when the stored file is missing.
#[rstest]
#[case::adopts_stored(Some("stored-secret\n"), None, Ok("stored-secret"))]
#[case::explicit_wins(Some("stored-secret"), Some("explicit"), Ok("explicit"))]
#[case::missing_file(None, None, Err(BootstrapErrorKind::ClusterPasswordMissing))]
fn bootstrap_for_tests_reuses_the_stored_password(
    #[case] stored: Option<&str>,
    #[case] explicit: Option<&str>,
    #[case] expected: Result<&str, BootstrapErrorKind>,
) -> Result<()> {
    let staged = staged_cluster(stored)?;
    match (bootstrap_with(&staged, explicit), expected) {
        (Ok(bootstrap), Ok(password)) => {
            ensure!(
                bootstrap.settings.password == password,
                "expected {password}, got {}",
                bootstrap.settings.password
            );
        }
        (Err(err), Err(kind)) => ensure!(err.kind() == kind, "unexpected kind: {err}"),
        (Ok(_), Err(kind)) => bail!("expected {kind:?}, but the bootstrap succeeded"),
        (Err(err), Ok(_)) => bail!("unexpected failure: {err}"),
    }
    Ok(())
}

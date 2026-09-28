//! The install tree's `.pgpass`: its permissions and owner, and its removal
//! when no cluster exists (#259).
//!
//! A module of the `bootstrap_for_tests` binary, split out to keep each file
//! under 400 lines; both cases drive `bootstrap_for_tests` in a sandbox.

use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
};

use color_eyre::eyre::{Context, Result, ensure, eyre};
use nix::unistd::User;
use pg_embedded_setup_unpriv::{
    ExecutionPrivileges,
    bootstrap_for_tests,
    detect_execution_privileges,
    test_support::worker_binary_for_tests,
};

use super::sandbox::TestSandbox;

#[test]
fn bootstrap_for_tests_sets_pgpass_permissions_and_owner() -> Result<()> {
    if detect_execution_privileges() == ExecutionPrivileges::Root
        && worker_binary_for_tests().is_none()
    {
        tracing::warn!(
            "Skipping pgpass permission test because PG_EMBEDDED_WORKER is unavailable."
        );
        return Ok(());
    }

    let sandbox = TestSandbox::new("bootstrap-pgpass")?;
    sandbox.reset()?;
    fs::create_dir_all(sandbox.install_dir().as_std_path()).context("create install dir")?;
    fs::create_dir_all(sandbox.data_dir().as_std_path()).context("create data dir")?;
    // The data directory must hold a cluster for the password file to be
    // kept: with no cluster, a leftover file is removed as stale (#259).
    fs::write(sandbox.data_dir().join("PG_VERSION").as_std_path(), b"17\n")
        .context("write PG_VERSION")?;

    let pgpass_path = sandbox.install_dir().join(".pgpass");
    fs::write(pgpass_path.as_std_path(), b"pgpass").context("write pgpass")?;
    let mut perms = fs::metadata(pgpass_path.as_std_path())
        .context("pgpass metadata")?
        .permissions();
    perms.set_mode(0o644);
    fs::set_permissions(pgpass_path.as_std_path(), perms).context("seed pgpass permissions")?;

    let env_vars = sandbox.base_env();
    let bootstrap = sandbox
        .with_env(env_vars, bootstrap_for_tests)
        .context("bootstrap_for_tests")?;

    let expected_user = match bootstrap.privileges {
        ExecutionPrivileges::Root => User::from_name("nobody")
            .context("resolve nobody user")?
            .ok_or_else(|| eyre!("user 'nobody' not found"))?,
        ExecutionPrivileges::Unprivileged => User::from_uid(nix::unistd::geteuid())
            .context("resolve current user")?
            .ok_or_else(|| eyre!("current user not found"))?,
    };

    let metadata = fs::metadata(pgpass_path.as_std_path()).context("pgpass metadata")?;
    let observed_mode = metadata.permissions().mode() & 0o777;
    ensure!(
        observed_mode == 0o600,
        "expected pgpass mode 0o600, got 0o{observed_mode:03o}"
    );
    ensure!(
        metadata.uid() == expected_user.uid.as_raw(),
        "expected uid {}, got {}",
        expected_user.uid,
        metadata.uid()
    );
    ensure!(
        metadata.gid() == expected_user.gid.as_raw(),
        "expected gid {}, got {}",
        expected_user.gid,
        metadata.gid()
    );
    ensure!(
        bootstrap.environment.pgpass_file == pgpass_path,
        "expected pgpass path to remain aligned with install directory"
    );

    Ok(())
}

/// A password file left by a reaped cluster is removed during preparation,
/// on whichever path this run takes (#259).
///
/// Run unprivileged, this covers the unprivileged preparation. Run as root
/// with `PG_EMBEDDED_WORKER` set, as the CI root lane runs the suite, it
/// covers the root preparation, which no child-process test reaches.
#[test]
fn bootstrap_for_tests_discards_a_stale_pgpass_without_a_cluster() -> Result<()> {
    if detect_execution_privileges() == ExecutionPrivileges::Root
        && worker_binary_for_tests().is_none()
    {
        tracing::warn!("Skipping stale pgpass test because PG_EMBEDDED_WORKER is unavailable.");
        return Ok(());
    }

    let sandbox = TestSandbox::new("bootstrap-stale-pgpass")?;
    sandbox.reset()?;
    fs::create_dir_all(sandbox.install_dir().as_std_path()).context("create install dir")?;
    fs::create_dir_all(sandbox.data_dir().as_std_path()).context("create data dir")?;
    let pgpass_path = sandbox.install_dir().join(".pgpass");
    fs::write(pgpass_path.as_std_path(), b"reaped-cluster-password")
        .context("seed the stale pgpass")?;

    let env_vars = sandbox.base_env();
    sandbox
        .with_env(env_vars, bootstrap_for_tests)
        .context("bootstrap_for_tests")?;

    ensure!(
        !pgpass_path.as_std_path().exists(),
        "a password file with no cluster beside it must be removed before initdb reads it"
    );
    Ok(())
}

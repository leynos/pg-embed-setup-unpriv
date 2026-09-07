//! End-to-end coverage: the password read back from a real cluster works.
//!
//! `tests/password_reuse.rs` stages a `PG_VERSION` marker and a password file
//! by hand, which proves the read-back path but not that the password it
//! returns can actually log in. The defect this feature fixes was exactly
//! that: a second bootstrap against a persisted data directory generated a
//! fresh random password, so the cluster ran with credentials nobody held.
//! Only a test that authenticates against a live server catches it.
//!
//! This file runs in its own process, so the cluster it starts is isolated
//! from other suites.
//!
//! It proves the password by opening a real connection, which needs `diesel`
//! and therefore `libpq`. The macOS and Windows lanes build without
//! `diesel-support` and have no `libpq` to link against, so the suite is gated
//! on that feature exactly as the library's own connection helpers are. The
//! Linux lane runs `--all-features` and does execute it.
#![cfg(all(unix, feature = "diesel-support"))]

use std::ffi::OsString;

use color_eyre::eyre::{Report, Result, ensure, eyre};
use diesel::{Connection, PgConnection, RunQueryDsl, sql_query};
use pg_embedded_setup_unpriv::{BootstrapError, TestCluster, bootstrap_for_tests};
use rstest::rstest;
use tracing::warn;

#[path = "support/cap_fs_bootstrap.rs"]
mod cap_fs;
#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/env.rs"]
mod env;
#[path = "support/sandbox.rs"]
mod sandbox;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};

/// A cluster that could not be started here, or a genuine test failure.
enum Failure {
    /// The environment cannot run an embedded cluster.
    Skipped(String),
    /// The behaviour under test is wrong.
    Failed(Report),
}

/// A second bootstrap against a live cluster adopts a password that logs in.
///
/// The first bootstrap initializes the cluster with a generated password and
/// starts it. The second runs against the same directories, still with no
/// `PG_PASSWORD`, which is the path that used to invent a fresh password. The
/// test then opens a real connection with whatever that second bootstrap
/// decided on, so a regression fails at authentication and not only at a
/// string comparison.
#[rstest]
fn a_second_bootstrap_adopts_a_password_that_authenticates(
    serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    let sandbox = TestSandbox::new("password-reuse-e2e")?;
    sandbox.reset()?;
    // PG_PASSWORD must be absent for both bootstraps: with it set the reuse
    // branch is never taken and the test would pass without exercising it.
    let mut vars = sandbox.env_without_timezone();
    vars.push((OsString::from("PG_PASSWORD"), None));
    let outcome = sandbox.with_env(vars, reuse_against_live_cluster);
    drop(serial_guard);
    match outcome {
        Ok(()) => Ok(()),
        Err(Failure::Skipped(reason)) => {
            warn!("SKIP: {reason}");
            Ok(())
        }
        Err(Failure::Failed(report)) => Err(report),
    }
}

/// Starts a cluster, bootstraps again without a password, and logs in.
fn reuse_against_live_cluster() -> std::result::Result<(), Failure> {
    let (handle, guard) = TestCluster::new_split().map_err(|err| skip_or_fail(&err))?;
    let running = handle.settings();
    let host = running.host.clone();
    let port = running.port;
    let superuser = running.username.clone();
    let started_with = running.password.clone();

    let checked = bootstrap_and_authenticate(&host, port, &superuser, &started_with);
    drop(guard);
    checked
}

/// Runs the second bootstrap and proves its password opens a session.
fn bootstrap_and_authenticate(
    host: &str,
    port: u16,
    superuser: &str,
    started_with: &str,
) -> std::result::Result<(), Failure> {
    let second = bootstrap_for_tests().map_err(|err| skip_or_fail(&err))?;
    let adopted = second.settings.password;
    let verdict = (|| -> Result<()> {
        ensure!(
            adopted == started_with,
            "the second bootstrap did not adopt the running cluster's password"
        );
        let url = format!("postgres://{superuser}:{adopted}@{host}:{port}/postgres");
        let mut connection = PgConnection::establish(&url)
            .map_err(|err| eyre!("the adopted password did not log in: {err}"))?;
        sql_query("SELECT 1").execute(&mut connection)?;
        Ok(())
    })();
    verdict.map_err(Failure::Failed)
}

/// Classifies a bootstrap error as an environment skip or a real failure.
fn skip_or_fail(err: &BootstrapError) -> Failure {
    let debug = format!("{err:?}");
    cluster_skip_message(&err.to_string(), Some(&debug))
        .map_or_else(|| Failure::Failed(eyre!("{err}")), Failure::Skipped)
}

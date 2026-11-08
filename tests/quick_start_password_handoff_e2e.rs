//! End-to-end coverage for the Quick start password handoff.
//!
//! The Quick start in `docs/users-guide.md` asks the reader to read
//! `PG_PASSWORD` into an unexported shell variable, refuses an unset or empty
//! value, and exports the value for the documented helper. The shell-level
//! tests in `tests/quick_start_password_guard.rs` prove the guard and the
//! export with a stub child; this suite proves the whole documented workflow
//! against `pg_embedded_setup_unpriv` and a live cluster.
//!
//! Three cases run under sandboxed directories. An unset or empty password
//! aborts before the helper can initialise a cluster. A supplied password
//! reaches the helper, which records it for the cluster it initializes; the
//! suite then starts that cluster, proves the recorded password logs in, and
//! proves a different password does not.
//!
//! Authentication needs `libpq`, so the suite is gated on `diesel-support`
//! exactly as the library's connection helpers are. The Linux lane runs it
//! with `--all-features`.

#![cfg(all(unix, feature = "diesel-support"))]

use std::{
    ffi::{OsStr, OsString},
    process::Command,
};

use camino::Utf8Path;
use color_eyre::eyre::{Report, Result, ensure, eyre};
use diesel::{Connection, PgConnection, RunQueryDsl, sql_query};
use pg_embedded_setup_unpriv::{BootstrapError, TestCluster};
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
use env::ScopedEnvVars;
use sandbox::TestSandbox;
use serial::{ScenarioSerialGuard, serial_guard};

/// The password the documented block must hand to the helper.
const SECRET: &str = "quick-start-handoff-secret";

/// A password the documented export never supplied.
const WRONG_SECRET: &str = "not-the-documented-password";

/// An ambient password the scenario must neither use nor disturb.
const AMBIENT_SECRET: &str = "ambient-password-outside-the-scope";

/// The guard marker that identifies the documented configuration fence.
const GUARD_MARKER: &str = "[ -z \"${PG_PASSWORD:+set}\" ]";

/// The diagnostic the documented guard prints for a missing password.
const MISSING_MESSAGE: &str =
    "PG_PASSWORD: Set PG_PASSWORD externally to a strong, unique password";

/// A cluster that cannot run here, or a genuine test failure.
enum Failure {
    /// The environment cannot run an embedded cluster.
    Skipped(String),
    /// The behaviour under test is wrong.
    Failed(Report),
}

/// Proves the documented workflow against the documented helper.
#[rstest]
fn documented_handoff_authenticates_a_live_cluster(
    serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    let sandbox = TestSandbox::new("quick-start-handoff-e2e")?;
    sandbox.reset()?;
    let outcome = run_with_ambient_password(&sandbox);
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

/// Runs the scenario with a conflicting password already in the environment.
///
/// `handoff_env` unsets `PG_PASSWORD` instead of leaving it untouched, so the
/// bootstrap must adopt the password the helper stored, not an ambient value.
/// The outer scope proves the documented block's environment changes are
/// undone afterwards, so a caller's password survives the test.
fn run_with_ambient_password(sandbox: &TestSandbox) -> std::result::Result<(), Failure> {
    let before = std::env::var_os("PG_PASSWORD");
    let (outcome, restored) = env::with_scoped_env(
        [(
            OsString::from("PG_PASSWORD"),
            Some(OsString::from(AMBIENT_SECRET)),
        )],
        || {
            let outcome = sandbox.with_env(handoff_env(sandbox), || run_scenario(sandbox));
            (outcome, std::env::var_os("PG_PASSWORD"))
        },
    );
    let retains_ambient = restored.as_deref() == Some(OsStr::new(AMBIENT_SECRET));
    if !retains_ambient || std::env::var_os("PG_PASSWORD") != before {
        return Err(Failure::Failed(eyre!(
            "the scoped environment did not restore the ambient password"
        )));
    }
    outcome
}

/// Runs the aborting cases and then the authenticating case.
fn run_scenario(sandbox: &TestSandbox) -> std::result::Result<(), Failure> {
    for (case, setup) in [("unset", ""), ("empty", "PG_PASSWORD=\n")] {
        assert_aborts_before_the_helper(sandbox, case, setup)?;
    }
    run_supplied_password_case(sandbox)
}

/// Asserts that the documented block fails and leaves no cluster behind.
fn assert_aborts_before_the_helper(
    sandbox: &TestSandbox,
    case: &str,
    password_setup: &str,
) -> std::result::Result<(), Failure> {
    let output = run_handoff(sandbox, password_setup)?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    ensure_no_secret(&output)?;
    if output.status.success() {
        return Err(Failure::Failed(eyre!(
            "{case}: the documented block accepted the password"
        )));
    }
    if !stderr.contains(MISSING_MESSAGE) {
        return Err(Failure::Failed(eyre!(
            "{case}: the guard reported unexpected diagnostics:\n{stderr}"
        )));
    }
    if sandbox.data_dir().exists() {
        return Err(Failure::Failed(eyre!(
            "{case}: the helper initialised a cluster despite the failed guard"
        )));
    }
    Ok(())
}

/// Runs the supplied-password case against the helper, then the cluster.
fn run_supplied_password_case(sandbox: &TestSandbox) -> std::result::Result<(), Failure> {
    let output = run_handoff(sandbox, "PG_PASSWORD=${EXPECTED_PASSWORD}\n")?;
    ensure_no_secret(&output)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(skip_or_fail_text(&stderr));
    }
    assert_helper_stored_the_password(sandbox)?;
    authenticate()
}

/// Asserts that the helper, not this test, initialized the cluster.
///
/// The password file is the helper's record of the credential it used, so
/// comparing it here shows the exported value reached `initdb`. The later
/// login then shows the cluster accepts that same value.
fn assert_helper_stored_the_password(sandbox: &TestSandbox) -> std::result::Result<(), Failure> {
    assert_cluster_exists(sandbox.data_dir())?;
    let stored = std::fs::read_to_string(sandbox.install_dir().join(".pgpass").as_std_path())
        .map_err(|err| Failure::Failed(eyre!("the helper left no password file: {err}")))?;
    if stored != SECRET {
        return Err(Failure::Failed(eyre!(
            "the helper recorded a different password than the documented export"
        )));
    }
    Ok(())
}

/// Asserts that `data_dir` holds a cluster initialized by the helper.
fn assert_cluster_exists(data_dir: &Utf8Path) -> std::result::Result<(), Failure> {
    if data_dir.join("PG_VERSION").is_file() {
        return Ok(());
    }
    Err(Failure::Failed(eyre!(
        "the documented helper did not initialise a cluster in {}",
        data_dir.as_str(),
    )))
}

/// Starts the prepared cluster and proves the stored password logs in.
///
/// `PG_PASSWORD` is absent here, so the bootstrap must adopt the password the
/// helper stored rather than one this test supplied. A cluster that accepted
/// any other password would also fail the second login below.
fn authenticate() -> std::result::Result<(), Failure> {
    let (handle, guard) = TestCluster::new_split().map_err(|err| skip_or_fail(&err))?;
    let settings = handle.settings();
    let verdict = (|| -> Result<()> {
        ensure!(
            settings.password == SECRET,
            "the bootstrap did not adopt the password the helper stored",
        );
        let correct = connection_url(settings, SECRET);
        let mut connection = PgConnection::establish(&correct)
            .map_err(|err| eyre!("the supplied password did not log in: {err}"))?;
        sql_query("SELECT 1").execute(&mut connection)?;
        let wrong = connection_url(settings, WRONG_SECRET);
        ensure!(
            PgConnection::establish(&wrong).is_err(),
            "the cluster accepted a password the documented export did not supply",
        );
        Ok(())
    })();
    drop(guard);
    verdict.map_err(Failure::Failed)
}

/// Builds a connection URL for `password` from the running cluster's settings.
fn connection_url(settings: &postgresql_embedded::Settings, password: &str) -> String {
    format!(
        "postgres://{}:{}@{}:{}/postgres",
        settings.username, password, settings.host, settings.port,
    )
}

/// Builds the sandbox environment for the documented workflow.
///
/// The shared timezone-free base matches the other cluster suites: the helper
/// resolves the host's time zone data itself. The base also seeds a known
/// password for other suites, so that entry becomes an unset: only the
/// documented export may hand a password to the helper, and dropping the key
/// would leave an ambient value in force.
fn handoff_env(sandbox: &TestSandbox) -> ScopedEnvVars {
    sandbox
        .env_without_timezone()
        .into_iter()
        .map(|(key, value)| {
            if key == "PG_PASSWORD" {
                (key, None)
            } else {
                (key, value)
            }
        })
        .collect()
}

/// Asserts that neither stream captured the password.
fn ensure_no_secret(output: &std::process::Output) -> std::result::Result<(), Failure> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if stdout.contains(SECRET) || stderr.contains(SECRET) {
        return Err(Failure::Failed(eyre!(
            "the documented handoff printed the password"
        )));
    }
    Ok(())
}

/// Runs the documented block and the documented helper in one Bash process.
///
/// `password_setup` mirrors the documented read: `PG_PASSWORD` is a plain
/// shell variable, not an exported one, because the helper starts with
/// `PG_PASSWORD` removed from its environment. The guide's example
/// directories and version pin are illustrative, so they are replaced with the
/// sandbox's isolated paths, and the version pin is restored for the next
/// bootstrap; the password lines themselves are executed verbatim.
fn run_handoff(
    sandbox: &TestSandbox,
    password_setup: &str,
) -> std::result::Result<std::process::Output, Failure> {
    let script = format!(
        concat!(
            "AMBIENT_VERSION_REQ=${{PG_VERSION_REQ-}}\n",
            "{}",
            "{}\n",
            "if [ -n \"${{AMBIENT_VERSION_REQ}}\" ]; then\n",
            "  export PG_VERSION_REQ=\"${{AMBIENT_VERSION_REQ}}\"\n",
            "else\n",
            "  unset PG_VERSION_REQ\n",
            "fi\n",
            "export PG_RUNTIME_DIR=${{SANDBOX_RUNTIME_DIR}}\n",
            "export PG_DATA_DIR=${{SANDBOX_DATA_DIR}}\n",
            "export PG_SUPERUSER=${{SANDBOX_SUPERUSER}}\n",
            "${{HELPER_BIN}}\n",
        ),
        password_setup,
        documented_block(),
    );

    Command::new("bash")
        .arg("-c")
        .arg(script)
        .env("EXPECTED_PASSWORD", SECRET)
        .env("SANDBOX_RUNTIME_DIR", sandbox.install_dir().as_str())
        .env("SANDBOX_DATA_DIR", sandbox.data_dir().as_str())
        .env("SANDBOX_SUPERUSER", "postgres")
        .env("HELPER_BIN", env!("CARGO_BIN_EXE_pg_embedded_setup_unpriv"))
        .env_remove("PG_PASSWORD")
        .output()
        .map_err(|err| Failure::Failed(eyre!("failed to invoke bash: {err}")))
}

/// Extracts the documented configuration fence that contains the guard.
fn documented_block() -> String {
    include_str!("../docs/users-guide.md")
        .split("```bash\n")
        .skip(1)
        .filter_map(|chunk| chunk.split("```").next())
        .find(|block| block.contains(GUARD_MARKER))
        .expect("the guide contains the password guard in a bash fence")
        .to_owned()
}

/// Classifies a bootstrap error as an environment skip or a real failure.
fn skip_or_fail(err: &BootstrapError) -> Failure {
    let debug = format!("{err:?}");
    let message = err.to_string();
    cluster_skip_message(&message, Some(&debug))
        .map_or_else(|| Failure::Failed(eyre!("{message}")), Failure::Skipped)
}

/// Classifies a helper failure as an environment skip or a real failure.
fn skip_or_fail_text(stderr: &str) -> Failure {
    cluster_skip_message(stderr, None).map_or_else(
        || Failure::Failed(eyre!("the documented helper failed:\n{stderr}")),
        Failure::Skipped,
    )
}

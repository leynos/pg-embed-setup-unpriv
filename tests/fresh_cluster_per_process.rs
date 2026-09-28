//! Two processes bootstrapping one after another in one install root.
//!
//! Under cargo-nextest each test is its own process: it bootstraps a
//! cluster, and the exit hook reaps the data directory. The install tree's
//! password file used to survive that, and the next process's `initdb` was
//! handed the reaped cluster's password while the handle reported a new one,
//! so every process after the first failed to log in (issue #259).
//!
//! Each case here re-runs this test binary as a child with the same
//! `PG_EMBED_ROOT`, sequentially, and asserts that every child can connect.
//! The environment is set only on the child `Command`.
#![cfg(unix)]

use std::{io::Write, path::Path, process::Command};

use cap_std::{ambient_authority, fs::Dir};
use pg_embedded_setup_unpriv::{BootstrapError, test_support};
use rstest::rstest;

/// Set in the child's environment so its test runs only when spawned.
const CHILD_MARKER: &str = "PG_EMBED_FRESH_CLUSTER_CHILD";

/// What the child prints when it bootstrapped and connected.
const CONNECTED: &str = "FRESH-CLUSTER-CHILD: connected";

/// Returns whether this process is a spawned child.
fn is_child() -> bool { std::env::var_os(CHILD_MARKER).is_some() }

/// Bootstraps the shared cluster and asks its admin database whether the
/// `postgres` database exists, which it always does on a cluster that works.
fn bootstrap_and_connect() -> Result<bool, BootstrapError> {
    test_support::shared_cluster_handle()?.database_exists("postgres")
}

/// Child: bootstrap, connect, and print the verdict for the parent.
#[test]
#[ignore = "run only as a child of the fresh-cluster tests"]
fn child_bootstrap_and_connect() {
    if is_child() {
        let verdict = match bootstrap_and_connect() {
            Ok(true) => CONNECTED.to_owned(),
            Ok(false) => "FRESH-CLUSTER-CHILD: connected, but no postgres database".to_owned(),
            Err(err) => format!("FRESH-CLUSTER-CHILD: failed: {err:?}"),
        };
        writeln!(std::io::stdout().lock(), "{verdict}").expect("stdout is writable");
    }
}

/// Runs one child against `root`, with `PG_PASSWORD` when given.
fn run_child(root: &Path, password: Option<&str>) -> std::io::Result<String> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "child_bootstrap_and_connect",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MARKER, "1")
        .env("PG_EMBED_ROOT", root)
        .env_remove("PG_DATA_DIR")
        .env_remove("PG_RUNTIME_DIR")
        .env_remove("PG_PASSWORD")
        .env_remove("PG_TEST_BACKEND")
        .env_remove("PG_EMBEDDED_WORKER");
    if let Some(value) = password {
        command.env("PG_PASSWORD", value);
    }
    let output = command.output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Returns whether the suite runs as root, where bootstrap goes through the
/// privileged worker and a different install layout.
fn running_as_root() -> bool { nix::unistd::geteuid().is_root() }

/// Returns whether any `PG_VERSION` marker, and so any cluster, sits under
/// `dir`.
fn holds_a_cluster(dir: &Dir) -> std::io::Result<bool> {
    for listed in dir.entries()? {
        let entry = listed?;
        let is_cluster = if entry.file_type()?.is_dir() {
            holds_a_cluster(&entry.open_dir()?)?
        } else {
            entry.file_name() == "PG_VERSION"
        };
        if is_cluster {
            return Ok(true);
        }
    }
    Ok(false)
}

/// What one run of children in a shared root observed.
struct RootRun {
    /// Each child's stdout, in order.
    outputs: Vec<String>,
    /// Whether a cluster was left in the root after each child exited.
    cluster_left: Vec<bool>,
}

/// Runs one child per entry in `passwords`, in order, against one root.
fn run_in_one_root(passwords: &[Option<&str>]) -> std::io::Result<RootRun> {
    let root = tempfile::tempdir()?;
    let mut run = RootRun {
        outputs: Vec::new(),
        cluster_left: Vec::new(),
    };
    for password in passwords {
        run.outputs.push(run_child(root.path(), *password)?);
        let root_dir = Dir::open_ambient_dir(root.path(), ambient_authority())?;
        run.cluster_left.push(holds_a_cluster(&root_dir)?);
    }
    Ok(run)
}

/// Asserts that every child connected, and that each child's cluster was
/// reaped before the next began, so no child can pass by reusing another's.
fn assert_fresh_and_connected(run: &RootRun, children: usize) {
    assert_eq!(run.outputs.len(), children, "every child must have run");
    for (index, said) in run.outputs.iter().enumerate() {
        assert!(
            said.contains(CONNECTED),
            "process {} in the shared root could not connect:\n{said}",
            index + 1
        );
    }
    assert!(
        run.cluster_left.iter().all(|left| !left),
        "a child left its cluster behind, so the next could reuse it: {:?}",
        run.cluster_left
    );
}

/// A second process in the same root gets a fresh cluster it can log in to,
/// whether its password is generated or given as `PG_PASSWORD`.
///
/// As root the bootstrap takes the worker path, which child processes do not
/// exercise here; the `bootstrap_for_tests` stale-file test covers root's
/// preparation instead, so this case skips itself explicitly.
#[rstest]
#[case::generated(None)]
#[case::explicit(Some("fresh-cluster-test"))]
fn a_second_process_in_one_root_can_connect(#[case] second_password: Option<&str>) {
    if is_child() {
        return;
    }
    if running_as_root() {
        tracing::warn!(
            "SKIP: root bootstraps use the worker path; see bootstrap_for_tests' pgpass cases"
        );
        return;
    }
    let run = run_in_one_root(&[None, second_password]).expect("the child test binary runs");
    assert_fresh_and_connected(&run, 2);
}

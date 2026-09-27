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

use pg_embedded_setup_unpriv::{BootstrapError, test_support};

/// Set in the child's environment so its test runs only when spawned.
const CHILD_MARKER: &str = "PG_EMBED_FRESH_CLUSTER_CHILD";

/// What the child prints when it bootstrapped and connected.
const CONNECTED: &str = "FRESH-CLUSTER-CHILD: connected";

/// Returns whether this process is a spawned child.
fn is_child() -> bool { std::env::var_os(CHILD_MARKER).is_some() }

/// Bootstraps the shared cluster and connects to its admin database.
fn bootstrap_and_connect() -> Result<(), BootstrapError> {
    let handle = test_support::shared_cluster_handle()?;
    handle.database_exists("postgres").map(|_| ())
}

/// Child: bootstrap, connect, and print the verdict for the parent.
#[test]
#[ignore = "run only as a child of the fresh-cluster tests"]
fn child_bootstrap_and_connect() {
    if is_child() {
        let verdict = match bootstrap_and_connect() {
            Ok(()) => CONNECTED.to_owned(),
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

/// Runs one child per entry in `passwords`, in order, against one root, and
/// returns each child's stdout; empty when this process is a child or root.
fn run_in_one_root(passwords: &[Option<&str>]) -> std::io::Result<Vec<String>> {
    if is_child() || running_as_root() {
        return Ok(Vec::new());
    }
    let root = tempfile::tempdir()?;
    passwords
        .iter()
        .map(|password| run_child(root.path(), *password))
        .collect()
}

/// Asserts that every child connected, naming the first that did not.
fn assert_all_connected(outputs: &[String]) {
    for (index, said) in outputs.iter().enumerate() {
        assert!(
            said.contains(CONNECTED),
            "process {} in the shared root could not connect:\n{said}",
            index + 1
        );
    }
}

/// A second process in the same root gets a cluster it can log in to.
#[test]
fn a_second_process_in_one_root_can_connect() {
    let outputs = run_in_one_root(&[None, None]).expect("the child test binary runs");
    assert_all_connected(&outputs);
}

/// `PG_PASSWORD` on the second process is the password its cluster gets,
/// not the first process's generated one.
#[test]
fn an_explicit_password_after_a_generated_one_can_connect() {
    let outputs =
        run_in_one_root(&[None, Some("fresh-cluster-test")]).expect("the child test binary runs");
    assert_all_connected(&outputs);
}

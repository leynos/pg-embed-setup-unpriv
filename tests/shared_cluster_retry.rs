//! Drives each shared-cluster singleton through the transient-failure retry.
//!
//! The singletons are process-wide and read their configuration from the
//! environment, so each case runs in a child process: the parent re-runs this
//! test binary with one ignored child test selected and the fault injected
//! through `Command::env`, and no environment is mutated in-process.
//!
//! The fault is a download that can never connect. Every HTTP proxy variable
//! points at a closed loopback port, and the binary cache and installation
//! directories are empty, so each bootstrap attempt fails while resolving the
//! `PostgreSQL` archive. That failure is transient, so a singleton that goes
//! through the retry reports `bootstrap failed after 3 attempts`, and one that
//! does not reports the bare download error.
#![cfg(unix)]

use std::{io::Write, process::Command};

use cap_std::{ambient_authority, fs::Dir};
use pg_embedded_setup_unpriv::{BootstrapError, test_support};

/// Set in the child's environment so its test runs only when spawned.
const CHILD_MARKER: &str = "PG_EMBED_RETRY_CHILD";

/// What the child prints when the singleton failed as the retry reports it.
const RETRIED: &str = "RETRY-CHILD-RESULT: retried";

/// A proxy that refuses every connection.
const DEAD_PROXY: &str = "http://127.0.0.1:9";

/// Returns whether this process is a spawned child.
fn is_child() -> bool { std::env::var_os(CHILD_MARKER).is_some() }

/// What the child prints when the singleton failed once, on cached binaries.
const CACHED_ONCE: &str = "RETRY-CHILD-RESULT: cached binaries, not retried";

/// Returns the child's verdict on the singleton's result.
fn verdict(result: Result<(), BootstrapError>) -> String {
    match result {
        Err(err) if format!("{err:?}").contains("bootstrap failed after 3 attempts") => {
            RETRIED.to_owned()
        }
        Err(err) if format!("{err:?}").contains("shared binary cache") => CACHED_ONCE.to_owned(),
        Err(err) => format!("RETRY-CHILD-RESULT: not retried: {err:?}"),
        Ok(()) => "RETRY-CHILD-RESULT: succeeded despite the dead proxy".to_owned(),
    }
}

/// Writes the child's verdict to stdout, where the parent reads it.
fn report(result: Result<(), BootstrapError>) -> std::io::Result<()> {
    writeln!(std::io::stdout().lock(), "{}", verdict(result))
}

/// Child: bootstrap through `shared_cluster_handle` and report.
#[test]
#[ignore = "run only as a child of shared_cluster_handle_goes_through_the_retry"]
fn child_shared_cluster_handle() {
    if is_child() {
        report(test_support::shared_cluster_handle().map(|_| ())).expect("stdout is writable");
    }
}

/// Child: bootstrap through the legacy `shared_cluster` and report.
#[test]
#[ignore = "run only as a child of shared_cluster_goes_through_the_retry"]
fn child_shared_cluster() {
    if is_child() {
        report(test_support::shared_cluster().map(|_| ())).expect("stdout is writable");
    }
}

/// The cached version the corrupt-cache case plants.
const CACHED_VERSION: &str = "17.4.0";

/// Plants a cache entry under `scratch` that passes the completeness check
/// but holds no binaries, so a cache hit copies a tree that cannot run.
fn plant_corrupt_entry(scratch: &std::path::Path) -> std::io::Result<()> {
    let root = Dir::open_ambient_dir(scratch, ambient_authority())?;
    let entry = format!("cache/{CACHED_VERSION}");
    root.create_dir_all(format!("{entry}/bin"))?;
    root.write(format!("{entry}/.complete"), b"")
}

/// Runs one child test with the download fault injected; returns its stdout.
///
/// With `corrupt_cache`, the binary cache also holds a planted entry for
/// the requested version.
fn run_child(child: &str, corrupt_cache: bool) -> std::io::Result<String> {
    let scratch = tempfile::tempdir()?;
    let dir = |name: &str| scratch.path().join(name);
    if corrupt_cache {
        plant_corrupt_entry(scratch.path())?;
    }
    let output = Command::new(std::env::current_exe()?)
        .env("PG_VERSION_REQ", format!("={CACHED_VERSION}"))
        .args([
            "--exact",
            child,
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MARKER, "1")
        .env("PG_RUNTIME_DIR", dir("install"))
        .env("PG_DATA_DIR", dir("data"))
        .env("PG_BINARY_CACHE_DIR", dir("cache"))
        .env("XDG_CACHE_HOME", dir("xdg"))
        .env("HTTPS_PROXY", DEAD_PROXY)
        .env("https_proxy", DEAD_PROXY)
        .env("HTTP_PROXY", DEAD_PROXY)
        .env("http_proxy", DEAD_PROXY)
        .env("ALL_PROXY", DEAD_PROXY)
        .env("all_proxy", DEAD_PROXY)
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .env_remove("PG_EXTENSIONS")
        .env_remove("PG_TEST_BACKEND")
        .env_remove("PG_EMBEDDED_WORKER")
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Returns whether the suite runs as root, where bootstraps go through the
/// worker subprocess, report failures as text, and are never retried.
fn running_as_root() -> bool { nix::unistd::geteuid().is_root() }

/// Runs a child and returns its stdout, or None when this process is itself
/// a child or runs as root.
fn child_stdout(child: &str, corrupt_cache: bool) -> std::io::Result<Option<String>> {
    if is_child() || running_as_root() {
        return Ok(None);
    }
    run_child(child, corrupt_cache).map(Some)
}

/// Asserts that a child's stdout, when there is one, carries `expected`.
fn assert_reports(stdout: Option<String>, expected: &str) {
    if let Some(said) = stdout {
        assert!(
            said.contains(expected),
            "expected {expected:?}; child said:\n{said}"
        );
    }
}

/// `shared_cluster_handle` retries a transient bootstrap failure to the bound.
#[test]
fn shared_cluster_handle_goes_through_the_retry() {
    let stdout =
        child_stdout("child_shared_cluster_handle", false).expect("the child test binary runs");
    assert_reports(stdout, RETRIED);
}

/// `shared_cluster` retries a transient bootstrap failure to the bound.
#[test]
fn shared_cluster_goes_through_the_retry() {
    let stdout = child_stdout("child_shared_cluster", false).expect("the child test binary runs");
    assert_reports(stdout, RETRIED);
}

/// A failure on binaries copied from a corrupt cache entry is not retried,
/// since each retry would copy the same tree, and the report says how to
/// clear the entry.
#[test]
fn a_corrupt_cache_entry_fails_once() {
    let stdout =
        child_stdout("child_shared_cluster_handle", true).expect("the child test binary runs");
    assert_reports(stdout, CACHED_ONCE);
}

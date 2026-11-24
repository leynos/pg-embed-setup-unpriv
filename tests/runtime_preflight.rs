//! Real-archive proof that the runtime preflight passes on a supported host.
//!
//! The first run stages the real Theseus archive (or reuses the shared binary
//! cache). The preflight runs in the normal bootstrap path, so the cluster
//! starting at all shows the archive passed on this host; the marker beside
//! the staged tree shows the check ran and was remembered. The opt-out is
//! covered by a unit test: a cache entry staged before the opt-out can carry
//! a marker, so the marker's absence is not a reliable observation here.
#![cfg(target_os = "linux")]

use std::{fs, path::PathBuf};

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Context, Result, ensure, eyre};
use pg_embedded_setup_unpriv::{
    BootstrapErrorKind,
    PgEnvCfg,
    RuntimePreflight,
    TestBootstrapConfig,
    TestCluster,
    test_support,
};

#[path = "support/cluster_skip.rs"]
mod cluster_skip;
#[path = "support/serial.rs"]
mod serial;
#[path = "support/skip.rs"]
mod skip;

use cluster_skip::cluster_skip_message;
use rstest::rstest;
use serial::{ScenarioSerialGuard, serial_guard};

/// The marker the preflight leaves in a staged tree it has passed.
const MARKER: &str = ".pg-embed-runtime-ok";

fn config(root: &Utf8Path, preflight: RuntimePreflight) -> Result<TestBootstrapConfig> {
    let config = TestBootstrapConfig::new(PgEnvCfg {
        runtime_dir: Some(root.join("install")),
        data_dir: Some(root.join("data")),
        superuser: Some("postgres".to_owned()),
        password: Some("postgres".to_owned()),
        ..PgEnvCfg::default()
    })
    .with_runtime_preflight(preflight);
    let Some(worker) = test_support::worker_binary_for_tests() else {
        return Ok(config);
    };
    let worker_path = Utf8PathBuf::from_path_buf(PathBuf::from(worker))
        .map_err(|path| eyre!("worker path is not valid UTF-8: {}", path.display()))?;
    Ok(config.with_worker_binary(worker_path))
}

/// Whether any staged tree under `install_dir` carries the preflight marker.
fn marker_present(install_dir: &camino::Utf8Path) -> bool {
    fs::read_dir(install_dir.as_std_path())
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.path().join(MARKER).is_file() || entry.file_name() == MARKER)
}

/// How a failed cluster start is judged by this test.
#[derive(Debug, PartialEq, Eq)]
enum StartFailure {
    /// The failure is the defect this test exists to catch.
    Fail(&'static str),
    /// The environment cannot run PostgreSQL; the test is skipped with this reason.
    Skip(String),
}

/// Loader and resolver signatures that mean a library is missing or the
/// preflight could not look, which a soft skip must never absorb.
///
/// A loader failure reads "cannot open shared object file: No such file or
/// directory", which also matches the generic missing-file skip, so these are
/// checked first.
const HARD_FAILURES: &[(&str, &str)] = &[
    (
        "error while loading shared libraries",
        "a staged binary could not load a shared library",
    ),
    (
        "cannot open shared object file",
        "the loader could not open a shared object",
    ),
    (
        "runtime preflight unavailable",
        "the preflight could not run",
    ),
];

/// Classifies a failed cluster start as a hard failure or an environmental skip.
///
/// Hard-failure signatures win over the shared skip heuristics.
fn classify_start_failure(message: &str, debug: &str) -> StartFailure {
    let lowered = format!("{message}\n{debug}").to_ascii_lowercase();
    if let Some((_, why)) = HARD_FAILURES
        .iter()
        .find(|(needle, _)| lowered.contains(needle))
    {
        return StartFailure::Fail(why);
    }
    cluster_skip_message(message, Some(debug))
        .map_or(StartFailure::Fail("bootstrap failed"), StartFailure::Skip)
}

#[rstest]
#[case::loader_missing_object(
    "initdb: error while loading shared libraries: libxml2.so.2: cannot open shared object file: \
     No such file or directory"
)]
#[case::loader_open_failure(
    "libicu.so.74: cannot open shared object file: No such file or directory"
)]
#[case::loader_message_only("error while loading shared libraries: libz.so.1")]
#[case::loader_prefix_with_missing_file(
    "initdb: error while loading shared libraries: libz.so.1: No such file or directory"
)]
#[case::unavailable_resolver_with_missing_file(
    "initdb failed: No such file or directory; runtime preflight unavailable: ldd not found"
)]
#[case::unavailable_resolver("runtime preflight unavailable: ldd not found")]
#[case::unrecognised("an unrelated failure")]
fn loader_and_resolver_failures_are_never_skipped(#[case] text: &str) {
    assert!(
        matches!(classify_start_failure(text, text), StartFailure::Fail(_)),
        "{text:?} must fail"
    );
}

#[rstest]
fn a_loader_failure_in_the_debug_text_alone_still_fails() {
    let outcome = classify_start_failure("setup failed", "inner: cannot open shared object file");
    assert!(matches!(outcome, StartFailure::Fail(_)), "{outcome:?}");
}

#[rstest]
#[case::missing_binary("No such file or directory (os error 2)")]
#[case::missing_path("failed to read /tmp/x/bin/postgres: no such file or directory")]
fn an_ordinary_missing_file_still_skips(#[case] text: &str) {
    assert!(
        matches!(classify_start_failure(text, text), StartFailure::Skip(_)),
        "{text:?} must skip"
    );
}

#[rstest]
fn the_real_archive_passes_the_preflight_on_this_host(
    serial_guard: ScenarioSerialGuard,
) -> Result<()> {
    let root = tempfile::tempdir().context("create temporary root")?;
    let root_path = Utf8Path::from_path(root.path()).ok_or_else(|| eyre!("non-UTF-8 tempdir"))?;
    let config = config(root_path, RuntimePreflight::Check)?;

    let started = TestCluster::new_split_with(config);
    let (handle, guard) = match started {
        Ok(pair) => pair,
        Err(err) => {
            // A resolver that cannot run must fail this test, not match the
            // soft-skip text for a missing file and pass without checking.
            ensure!(
                !matches!(
                    err.kind(),
                    BootstrapErrorKind::RuntimeIncompatible
                        | BootstrapErrorKind::RuntimeCheckIndeterminate
                ),
                "the preflight rejected this host: {err}"
            );
            let message = err.to_string();
            let debug = format!("{err:?}");
            let reason = match classify_start_failure(&message, &debug) {
                StartFailure::Fail(why) => return Err(eyre!("{why}: {debug}")),
                StartFailure::Skip(reason) => reason,
            };
            tracing::warn!("{reason}");
            return Ok(());
        }
    };
    drop(guard);
    drop(handle);

    ensure!(
        marker_present(&root_path.join("install")),
        "the preflight should have passed and left its marker beside the staged tree"
    );
    drop(serial_guard);
    Ok(())
}

//! PID-file parsing and process liveness helpers for shutdown-hook support.

use std::path::Path;

use super::platform;
use crate::error::BootstrapResult;

/// Platform-specific process identifier stored in `postmaster.pid`.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub type PostmasterPid = platform::PostmasterPid;

/// PID-reuse-safe platform-specific postmaster identity.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub type PostmasterProcess = platform::PostmasterProcess;

/// Reads the postmaster PID from `data_dir/postmaster.pid`.
///
/// Returns `Ok(None)` if the file is missing or empty.
///
/// # Errors
///
/// Returns an error if the PID file cannot be read or contains a malformed PID.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn read_postmaster_pid(data_dir: &Path) -> BootstrapResult<Option<PostmasterPid>> {
    let pid_file = data_dir.join("postmaster.pid");
    let contents = read_postmaster_pid_file(&pid_file)?;
    let Some(first_line) = contents.lines().next() else {
        return Ok(None);
    };
    if first_line.trim().is_empty() {
        return Ok(None);
    }
    platform::parse_pid(first_line).map(Some).ok_or_else(|| {
        color_eyre::eyre::eyre!("malformed postmaster PID in {}", pid_file.display()).into()
    })
}

/// Reads the PID-reuse-safe postmaster process identity from `postmaster.pid`.
///
/// Returns `Ok(None)` if the file is missing or empty.
///
/// # Errors
///
/// Returns an error if the PID file cannot be read or contains a malformed
/// platform-specific process identity.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn read_postmaster_process(data_dir: &Path) -> BootstrapResult<Option<PostmasterProcess>> {
    read_postmaster_process_from_dir(data_dir)
}

pub(super) fn read_postmaster_process_from_dir(
    data_dir: &Path,
) -> BootstrapResult<Option<platform::PostmasterProcess>> {
    let pid_file = data_dir.join("postmaster.pid");
    let contents = read_postmaster_pid_file(&pid_file)?;
    if contents.trim().is_empty() {
        return Ok(None);
    }
    platform::parse_postmaster_process(&contents)
        .map(Some)
        .ok_or_else(|| {
            color_eyre::eyre::eyre!("malformed postmaster process in {}", pid_file.display()).into()
        })
}

fn read_postmaster_pid_file(pid_file: &Path) -> BootstrapResult<String> {
    match std::fs::read_to_string(pid_file) {
        Ok(contents) => Ok(contents),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => {
            Err(color_eyre::eyre::eyre!("failed to read {}: {err}", pid_file.display()).into())
        }
    }
}

/// Returns `true` if a process with the given PID is currently running.
///
/// Invalid PIDs are rejected immediately by the platform implementation.
///
/// # Errors
///
/// Returns an error when the platform process probe fails for a reason other
/// than the process being absent.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn process_is_running(pid: PostmasterPid) -> BootstrapResult<bool> {
    platform::process_is_running_for_platform(pid)
}

/// Returns `true` when the postmaster identity still matches a live process.
///
/// This helper preserves the shutdown hook's PID-reuse-safe process identity
/// check for integration tests.
///
/// # Errors
///
/// Returns an error when the platform process probe fails for a reason other
/// than the process being absent.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub fn postmaster_process_is_running(process: PostmasterProcess) -> BootstrapResult<bool> {
    platform::postmaster_process_is_running(process)
}

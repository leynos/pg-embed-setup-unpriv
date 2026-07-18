//! Cleanup orchestration for the `pg_worker` binary.
//!
//! Decides which directories to remove for the `cleanup` and `cleanup-full`
//! operations and aggregates any removal failures into a single error.

use camino::Utf8Path;

use super::{WorkerError, removal};

pub(super) fn execute_cleanup(
    data_dir: &Utf8Path,
    install_dir: Option<&Utf8Path>,
    install_root: Option<&Utf8Path>,
) -> Result<(), WorkerError> {
    let mut failures = Vec::new();
    collect_removal_error(&mut failures, data_dir, "data");
    if let Some(path) = install_dir {
        collect_removal_error(&mut failures, path, "installation");
    }
    if let Some(path) = install_root {
        let is_already_removed = install_dir.is_some_and(|install_path| install_path == path);
        if !is_already_removed {
            collect_removal_error(&mut failures, path, "installation-root");
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(WorkerError::CleanupFailed(failures.join("; ")))
    }
}

fn collect_removal_error(failures: &mut Vec<String>, path: &Utf8Path, label: &str) {
    if let Err(err) = removal::remove_dir_all_if_exists(path, label) {
        failures.push(err);
    }
}

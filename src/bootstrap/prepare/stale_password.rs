//! Discards a password file left behind by a cluster that no longer exists.
//!
//! `postgresql_embedded` writes the superuser password to the install tree's
//! password file only when that file is absent, and then hands the file to
//! `initdb`. The data directory is removed when a cluster is reaped, but the
//! password file is not. So a later bootstrap in the same install root
//! initialized its fresh cluster with the previous cluster's password while
//! reporting its own, and nobody could log in. Removing the file whenever the
//! data directory holds no cluster makes `initdb` receive the password the
//! bootstrap reports, whether generated or taken from `PG_PASSWORD`.

use std::io::ErrorKind;

use camino::Utf8Path;
use color_eyre::eyre::Report;
use tracing::info;

use super::password::has_cluster_marker;
use crate::{
    error::{BootstrapError, BootstrapErrorKind, BootstrapResult},
    observability::LOG_TARGET,
};

/// Removes `password_file` when `data_dir` holds no cluster.
///
/// Returns whether a file was removed. A data directory that holds a cluster
/// keeps its password file, because reuse reads it.
///
/// # Errors
///
/// Returns an error when the data directory cannot be probed, or when a
/// stale password file exists but cannot be removed.
pub(super) fn discard_orphaned_password_file(
    data_dir: &Utf8Path,
    password_file: &Utf8Path,
) -> BootstrapResult<bool> {
    if has_cluster_marker(data_dir).map_err(|failure| failure.error)? {
        return Ok(false);
    }
    match std::fs::remove_file(password_file) {
        Ok(()) => {
            log_discarded(data_dir, password_file);
            Ok(true)
        }
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(BootstrapError::new(
            BootstrapErrorKind::ClusterPasswordUnreadable,
            Report::new(err).wrap_err(format!(
                "data directory {data_dir} holds no cluster, but the stale password file \
                 {password_file} cannot be removed; remove it before bootstrapping"
            )),
        )),
    }
}

/// Logs the removal; the path is logged, never the password.
fn log_discarded(data_dir: &Utf8Path, password_file: &Utf8Path) {
    info!(
        target: LOG_TARGET,
        data_dir = %data_dir,
        password_file = %password_file,
        "discarded a password file left by a reaped cluster"
    );
}

#[cfg(test)]
#[path = "stale_password_tests.rs"]
mod tests;

//! Reuses the superuser password of an existing cluster.
//!
//! `postgresql_embedded::Settings::default()` generates a fresh random password
//! on every bootstrap, but a data directory that already holds a cluster keeps
//! the password its `initdb` was given. Without this step every later
//! bootstrap on a host with prior state starts a server nobody can log in to.
//! The password `initdb` used is the one `postgresql_embedded` wrote to the
//! password file, so it is read back from there.

use std::{
    fs::File,
    io::{ErrorKind, Read},
};

use camino::Utf8Path;
use color_eyre::eyre::{Report, eyre};
use postgresql_embedded::Settings;
use tracing::{info, warn};

use crate::{
    error::{BootstrapError, BootstrapErrorKind, BootstrapResult},
    observability::{self, LOG_TARGET, Metric, PasswordReuseOutcomeMetric},
};

/// Marker `initdb` leaves in a data directory.
const PG_VERSION_MARKER: &str = "PG_VERSION";

/// Most a password file may hold before the read is refused.
///
/// `postgresql_embedded` writes one password and a newline, so the real file
/// is tens of bytes. The cap exists because [`stored_cluster_password`] is
/// public and takes any path: without it a caller could aim the bootstrap at
/// an arbitrarily large file and the read would allocate all of it before
/// anything rejected the contents. Four kibibytes leaves room for any password
/// a person or a generator would produce.
const MAX_PASSWORD_FILE_BYTES: u64 = 4096;

/// Why the bootstrap did or did not adopt a stored password; a bounded label
/// for the `password_reuse` tracing event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordReuseOutcome {
    /// The stored password was adopted.
    Reused,
    /// The caller supplied `PG_PASSWORD`, which always wins.
    ExplicitPassword,
    /// The data directory holds no cluster, so a fresh password is fine.
    NoCluster,
}

/// Reads the password an existing cluster in `data_dir` was initialized with.
///
/// This is the query half of password reuse: it reads the filesystem and
/// changes nothing, including emitting nothing. A caller that wants the
/// `password_reuse` failure event calls [`reuse_existing_password`], which is
/// the command half and publishes on every branch. `Ok(None)` means the data directory holds no
/// cluster (no `PG_VERSION` marker). A cluster whose password file is missing,
/// unreadable, or empty is an error, because a server started against it
/// could not be logged in to.
///
/// # Errors
///
/// Returns an error naming the data directory, the password file and the
/// remedies (`PG_PASSWORD`, or removing the stale cluster).
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::stored_cluster_password;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let stored = stored_cluster_password(
///     Utf8Path::new("/var/tmp/pg-embed-1000/data"),
///     Utf8Path::new("/var/tmp/pg-embed-1000/install/.pgpass"),
/// )?;
/// assert!(stored.is_none() || stored.is_some_and(|password| !password.is_empty()));
/// # Ok(())
/// # }
/// ```
pub fn stored_cluster_password(
    data_dir: &Utf8Path,
    password_file: &Utf8Path,
) -> BootstrapResult<Option<String>> {
    query_stored_password(data_dir, password_file).map_err(|failure| failure.error)
}

/// A failed query, carrying the bounded label its event would use.
///
/// The label travels with the error instead of being emitted here, so the
/// query stays free of side effects and the command decides what to publish.
struct PasswordQueryFailure {
    /// Bounded `outcome` label: `probe_failed`, `missing_file`,
    /// `unreadable_file` or `empty_file`.
    outcome: &'static str,
    /// The same outcome as a bounded metric label.
    metric: PasswordReuseOutcomeMetric,
    /// The categorized error to return.
    error: BootstrapError,
}

/// The query half proper: reads, categorizes, and emits nothing.
fn query_stored_password(
    data_dir: &Utf8Path,
    password_file: &Utf8Path,
) -> Result<Option<String>, PasswordQueryFailure> {
    if !has_cluster_marker(data_dir)? {
        return Ok(None);
    }
    read_stored_password(password_file, data_dir).map(Some)
}

/// Probes the `PG_VERSION` marker, treating only "not found" as "no
/// cluster"; a permission failure or any other I/O error is propagated so a
/// temporarily unsearchable data directory cannot masquerade as a fresh one.
fn has_cluster_marker(data_dir: &Utf8Path) -> Result<bool, PasswordQueryFailure> {
    match std::fs::metadata(data_dir.join(PG_VERSION_MARKER)) {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(PasswordQueryFailure {
            outcome: "probe_failed",
            metric: PasswordReuseOutcomeMetric::ProbeFailed,
            error: BootstrapError::new(
                BootstrapErrorKind::ClusterPasswordUnreadable,
                Report::new(err).wrap_err(format!(
                    "cannot probe {data_dir} for an existing cluster ({PG_VERSION_MARKER})"
                )),
            ),
        }),
    }
}

/// Aligns `settings.password` with the cluster already present in
/// `data_dir`, unless the caller supplied an explicit password.
///
/// This is the command half: it consults the query, mutates only
/// `settings.password`, and is the only place a `password_reuse` event is
/// emitted or a [`Metric::PasswordReuse`] recorded, on success and on every
/// failure branch. Exactly one of each per call. Returns the outcome so
/// callers and the event can report which branch was taken.
///
/// # Errors
///
/// Propagates [`stored_cluster_password`]'s error when the data directory
/// holds a cluster but its password file is missing, unreadable or empty.
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::{PasswordReuseOutcome, PgEnvCfg, reuse_existing_password};
/// use postgresql_embedded::Settings;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cfg = PgEnvCfg::default();
/// let mut settings = cfg.to_settings()?;
/// let outcome = reuse_existing_password(
///     &mut settings,
///     Utf8Path::new("/var/tmp/pg-embed-1000/data"),
///     Utf8Path::new("/var/tmp/pg-embed-1000/install/.pgpass"),
///     cfg.password.is_some(),
/// )?;
/// assert!(outcome != PasswordReuseOutcome::Reused || !settings.password.is_empty());
/// # Ok(())
/// # }
/// ```
pub fn reuse_existing_password(
    settings: &mut Settings,
    data_dir: &Utf8Path,
    password_file: &Utf8Path,
    is_password_explicit: bool,
) -> BootstrapResult<PasswordReuseOutcome> {
    let outcome = if is_password_explicit {
        PasswordReuseOutcome::ExplicitPassword
    } else {
        // The query publishes nothing, so the command emits the failure event
        // on its behalf before propagating the error.
        match query_stored_password(data_dir, password_file) {
            Ok(Some(stored)) => {
                settings.password = stored;
                PasswordReuseOutcome::Reused
            }
            Ok(None) => PasswordReuseOutcome::NoCluster,
            Err(failure) => {
                log_failure(failure.outcome, data_dir, password_file);
                observability::record(Metric::PasswordReuse(failure.metric));
                return Err(failure.error);
            }
        }
    };
    log_outcome(outcome, data_dir, password_file);
    observability::record(Metric::PasswordReuse(match outcome {
        PasswordReuseOutcome::Reused => PasswordReuseOutcomeMetric::Reused,
        PasswordReuseOutcome::ExplicitPassword => PasswordReuseOutcomeMetric::ExplicitPassword,
        PasswordReuseOutcome::NoCluster => PasswordReuseOutcomeMetric::NoCluster,
    }));
    Ok(outcome)
}

/// Emits the bounded `password_reuse` event; no path or secret is a label.
fn log_outcome(outcome: PasswordReuseOutcome, data_dir: &Utf8Path, password_file: &Utf8Path) {
    info!(
        target: LOG_TARGET,
        outcome = ?outcome,
        data_dir = %data_dir,
        password_file = %password_file,
        "password_reuse"
    );
}

/// Emits the bounded `password_reuse` failure event before the error is
/// returned; `outcome` is one of `probe_failed`, `missing_file`,
/// `unreadable_file` or `empty_file`, and no secret is ever a label.
fn log_failure(outcome: &'static str, data_dir: &Utf8Path, password_file: &Utf8Path) {
    warn!(
        target: LOG_TARGET,
        outcome,
        data_dir = %data_dir,
        password_file = %password_file,
        "password_reuse"
    );
}

/// Reads and trims the stored password, turning I/O and emptiness into a
/// categorized error that keeps the original `io::Error` as its source.
fn read_stored_password(
    password_file: &Utf8Path,
    data_dir: &Utf8Path,
) -> Result<String, PasswordQueryFailure> {
    let raw = read_bounded(password_file).map_err(|err| {
        let (kind, hint, outcome, metric) = if err.kind() == ErrorKind::NotFound {
            (
                BootstrapErrorKind::ClusterPasswordMissing,
                "is missing",
                "missing_file",
                PasswordReuseOutcomeMetric::MissingFile,
            )
        } else {
            (
                BootstrapErrorKind::ClusterPasswordUnreadable,
                "cannot be read",
                "unreadable_file",
                PasswordReuseOutcomeMetric::UnreadableFile,
            )
        };
        PasswordQueryFailure {
            outcome,
            metric,
            error: BootstrapError::new(
                kind,
                Report::new(err).wrap_err(format!(
                    "data directory {data_dir} already holds a cluster but its password file \
                     {password_file} {hint}; set PG_PASSWORD to the password that initialized it, \
                     or remove the stale cluster"
                )),
            ),
        }
    })?;
    let stored = raw.trim_end_matches(['\n', '\r']).to_owned();
    if stored.is_empty() {
        return Err(PasswordQueryFailure {
            outcome: "empty_file",
            metric: PasswordReuseOutcomeMetric::EmptyFile,
            error: BootstrapError::new(
                BootstrapErrorKind::ClusterPasswordEmpty,
                eyre!(
                    "password file {password_file} is empty; set PG_PASSWORD or remove the stale \
                     cluster at {data_dir}"
                ),
            ),
        });
    }
    Ok(stored)
}

/// Reads at most [`MAX_PASSWORD_FILE_BYTES`] from a regular file.
///
/// Three refusals, all reported as ordinary [`std::io::Error`]s so the caller
/// keeps its existing "missing" versus "unreadable" split:
///
/// - anything that is not a regular file, which is checked by `metadata` before the file is opened.
///   Opening first would be too late on Unix, where opening a FIFO blocks until a writer appears
///   and the bootstrap would hang rather than fail.
/// - a file whose recorded length already exceeds the cap, which is refused without reading a byte.
/// - a file that grows past the cap between the two, which the bounded read catches by asking for
///   one byte more than the cap and rejecting a full buffer.
fn read_bounded(password_file: &Utf8Path) -> std::io::Result<String> {
    let metadata = std::fs::metadata(password_file)?;
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            format!("{password_file} is not a regular file"),
        ));
    }
    if metadata.len() > MAX_PASSWORD_FILE_BYTES {
        return Err(oversized(password_file, metadata.len()));
    }

    let mut raw = String::new();
    File::open(password_file)?
        .take(MAX_PASSWORD_FILE_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_PASSWORD_FILE_BYTES {
        return Err(oversized(password_file, raw.len() as u64));
    }
    Ok(raw)
}

/// The error both size refusals report.
fn oversized(password_file: &Utf8Path, observed: u64) -> std::io::Error {
    std::io::Error::new(
        ErrorKind::InvalidData,
        format!(
            "password file {password_file} is {observed} bytes, above the \
             {MAX_PASSWORD_FILE_BYTES}-byte limit"
        ),
    )
}

#[cfg(test)]
#[path = "password_tests.rs"]
mod tests;

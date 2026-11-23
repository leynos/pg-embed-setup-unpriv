//! The preflight proper: resolve every staged object through the port.

use std::{
    path::{Path, PathBuf},
    time::Instant,
};

use camino::Utf8PathBuf;
use tracing::debug;

use super::{
    error::{BinaryFinding, IndeterminateRuntime, RuntimePreflightError, UnresolvedLibraries},
    host::HostOs,
    marker,
    resolver::{ResolverUnavailable, SharedLibraryResolver},
};
use crate::observability::LOG_TARGET;

/// The executables the crate stages and runs: the server, `initdb` and `pg_ctl`.
const CHECKED_EXECUTABLES: [&str; 3] = ["postgres", "initdb", "pg_ctl"];

/// What a diagnostic needs to know about the staged archive and its host.
#[derive(Debug, Clone)]
pub(crate) struct ArchiveContext {
    /// The staged `PostgreSQL` version.
    pub(crate) postgresql_version: String,
    /// The releases URL the archive came from, with credentials, query and
    /// fragment removed, for diagnostics.
    pub(crate) archive_source: String,
    /// A digest of the full releases URL, so the cache key tells sources apart
    /// without storing a secret.
    pub(crate) source_digest: String,
    /// The host the objects must run on.
    pub(crate) host: HostOs,
}

impl ArchiveContext {
    /// The cache key: it changes when the archive, its source or the host does.
    fn marker_key(&self) -> String {
        format!(
            "v1\n{}\n{}\n{}\n",
            self.postgresql_version, self.source_digest, self.host.fingerprint
        )
    }
}

/// Runs the preflight against one resolver for one staged archive.
pub(crate) struct Preflight<'a, R> {
    resolver: &'a R,
    context: ArchiveContext,
}

impl<'a, R: SharedLibraryResolver> Preflight<'a, R> {
    /// Builds a preflight for the archive described by `context`.
    pub(crate) const fn new(resolver: &'a R, context: ArchiveContext) -> Self {
        Self { resolver, context }
    }

    /// Checks the staged executables, skipping the work when this archive has
    /// already passed on this host.
    ///
    /// A pass is recorded beside the archive; a failure never is, so a fixed
    /// host is checked afresh.
    ///
    /// # Errors
    /// Returns [`RuntimePreflightError`] when a library is unresolved or the
    /// resolver cannot answer.
    pub(crate) fn check_executables(&self, staged: &Path) -> Result<(), RuntimePreflightError> {
        let key = self.context.marker_key();
        if marker::is_recorded(staged, &key) {
            return Ok(());
        }
        let started = Instant::now();
        self.check_objects(&staged_executables(staged))?;
        marker::record(staged, &key);
        debug!(
            target: LOG_TARGET,
            elapsed_ms = started.elapsed().as_millis(),
            postgresql_version = %self.context.postgresql_version,
            "runtime preflight passed"
        );
        Ok(())
    }

    /// Checks the staged executables afresh, ignoring any recorded pass.
    ///
    /// Used to explain a failure: a marker only says the check passed once,
    /// and the failure being explained is evidence the environment changed.
    ///
    /// # Errors
    /// As for [`Self::check_executables`].
    pub(crate) fn diagnose_executables(&self, staged: &Path) -> Result<(), RuntimePreflightError> {
        self.check_objects(&staged_executables(staged))
    }

    /// Checks the given objects, without consulting or writing the marker.
    ///
    /// # Errors
    /// Returns [`RuntimePreflightError`] when a library is unresolved or the
    /// resolver cannot answer.
    pub(crate) fn check_objects(
        &self,
        objects: &[Utf8PathBuf],
    ) -> Result<(), RuntimePreflightError> {
        let mut findings = Vec::new();
        for object in objects {
            let missing = self
                .resolver
                .unresolved(object)
                .map_err(|err| self.indeterminate(err))?;
            if !missing.is_empty() {
                findings.push(BinaryFinding {
                    binary: object.clone(),
                    missing,
                });
            }
        }
        if findings.is_empty() {
            return Ok(());
        }
        Err(RuntimePreflightError::Unresolved(UnresolvedLibraries {
            findings,
            host_os: self.context.host.pretty_name.clone(),
            postgresql_version: self.context.postgresql_version.clone(),
            archive_source: self.context.archive_source.clone(),
        }))
    }

    fn indeterminate(&self, err: ResolverUnavailable) -> RuntimePreflightError {
        RuntimePreflightError::Indeterminate(IndeterminateRuntime {
            reason: err.reason,
            host_os: self.context.host.pretty_name.clone(),
            postgresql_version: self.context.postgresql_version.clone(),
            archive_source: self.context.archive_source.clone(),
        })
    }
}

/// Lists the checked executables that exist under `staged/bin`.
///
/// An absent executable means the tree is not fully staged, which is the
/// installer's failure to report, not the preflight's.
pub(crate) fn staged_executables(staged: &Path) -> Vec<Utf8PathBuf> {
    CHECKED_EXECUTABLES
        .iter()
        .map(|name| staged.join("bin").join(name))
        .filter(|path| path.is_file())
        .filter_map(|path: PathBuf| Utf8PathBuf::from_path_buf(path).ok())
        .collect()
}

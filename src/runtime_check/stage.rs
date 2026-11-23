//! Bridges the preflight to the bootstrap: find the staged tree, build the
//! diagnostic context and map failures onto [`BootstrapError`].

use std::path::{Path, PathBuf};

use camino::Utf8PathBuf;
use postgresql_embedded::Settings;
use tracing::{debug, warn};

use super::{
    RuntimePreflight,
    check::{ArchiveContext, Preflight},
    error::RuntimePreflightError,
    host::HostOs,
    resolver::{LddResolver, SharedLibraryResolver},
};
use crate::{
    TestBootstrapSettings,
    cluster::resolve_staged_dir,
    error::{BootstrapError, BootstrapErrorKind, BootstrapResult},
    observability::LOG_TARGET,
};

/// A staged installation to check, with the context its diagnostics need.
struct Staged {
    dir: PathBuf,
    context: ArchiveContext,
}

/// Locates the staged tree, or `None` when there is nothing to check.
///
/// There is nothing to check when the caller opted out, when the host is not
/// Linux, or when no `postgres` executable is staged yet.
fn locate(bootstrap: &TestBootstrapSettings) -> Option<Staged> {
    if let Some(reason) = skip_reason(bootstrap) {
        debug!(target: LOG_TARGET, reason, "runtime preflight not applied");
        return None;
    }
    let dir = staged_postgres_dir(&bootstrap.settings)?;
    let context = ArchiveContext {
        postgresql_version: staged_version(&dir, &bootstrap.settings),
        archive_source: bootstrap.settings.releases_url.clone(),
        host: HostOs::detect(),
    };
    Some(Staged { dir, context })
}

/// Why the check does not apply, or `None` when it does.
const fn skip_reason(bootstrap: &TestBootstrapSettings) -> Option<&'static str> {
    match (bootstrap.runtime_preflight, cfg!(target_os = "linux")) {
        (RuntimePreflight::Skip, _) => Some("skipped by configuration"),
        (_, false) => Some("skipped: only Linux is checked"),
        _ => None,
    }
}

/// The resolved installation directory, when it holds a `postgres` executable.
fn staged_postgres_dir(settings: &Settings) -> Option<PathBuf> {
    let dir = resolve_staged_dir(settings)?;
    dir.join("bin").join("postgres").is_file().then_some(dir)
}

/// The staged version for diagnostics: the version directory's name when it
/// has one, otherwise the configured requirement.
fn staged_version(dir: &Path, settings: &Settings) -> String {
    dir.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| name.starts_with(|c: char| c.is_ascii_digit()))
        .map_or_else(|| settings.version.to_string(), str::to_owned)
}

fn into_bootstrap_error(err: RuntimePreflightError) -> BootstrapError {
    let kind = match err {
        RuntimePreflightError::Unresolved(_) => BootstrapErrorKind::RuntimeIncompatible,
        RuntimePreflightError::Indeterminate(_) => BootstrapErrorKind::RuntimeCheckIndeterminate,
    };
    BootstrapError::new(kind, color_eyre::Report::new(err))
}

/// Checks the staged executables with the host's `ldd`.
///
/// Cheap to repeat: a pass is remembered beside the archive.
///
/// # Errors
/// Returns a [`BootstrapError`] of kind
/// [`RuntimeIncompatible`](BootstrapErrorKind::RuntimeIncompatible) or
/// [`RuntimeCheckIndeterminate`](BootstrapErrorKind::RuntimeCheckIndeterminate).
pub(crate) fn check_staged_executables(bootstrap: &TestBootstrapSettings) -> BootstrapResult<()> {
    check_staged_executables_with(&LddResolver::new(), bootstrap)
}

/// [`check_staged_executables`] through an explicit resolver.
pub(super) fn check_staged_executables_with<R: SharedLibraryResolver>(
    resolver: &R,
    bootstrap: &TestBootstrapSettings,
) -> BootstrapResult<()> {
    let Some(staged) = locate(bootstrap) else {
        return Ok(());
    };
    Preflight::new(resolver, staged.context)
        .check_executables(&staged.dir)
        .map_err(into_bootstrap_error)
}

/// Checks the shared libraries the extension hook installed.
///
/// # Errors
/// As for [`check_staged_executables`].
pub(crate) fn check_extension_libraries(bootstrap: &TestBootstrapSettings) -> BootstrapResult<()> {
    check_extension_libraries_with(&LddResolver::new(), bootstrap)
}

/// [`check_extension_libraries`] through an explicit resolver.
pub(super) fn check_extension_libraries_with<R: SharedLibraryResolver>(
    resolver: &R,
    bootstrap: &TestBootstrapSettings,
) -> BootstrapResult<()> {
    let Some(staged) = locate(bootstrap) else {
        return Ok(());
    };
    let libraries: Vec<Utf8PathBuf> = bootstrap
        .installed_extensions
        .iter()
        .flat_map(|extension| &extension.files)
        .filter(|file| file.extension() == Some("so"))
        .filter_map(|file| Utf8PathBuf::from_path_buf(staged.dir.join(file)).ok())
        .collect();
    Preflight::new(resolver, staged.context)
        .check_objects(&libraries)
        .map_err(into_bootstrap_error)
}

/// Turns a failed `Setup` into the explanation, when the staged tree is the cause.
///
/// `setup()` downloads and runs `initdb` in one call, so on a first download
/// the host's failure surfaces as an opaque `initdb` error. This re-reads the
/// staged tree and, if it finds unresolved libraries, returns that finding
/// instead. In every other case `original` is returned unchanged: a pass or
/// an indeterminate answer must not mask the real failure.
pub(crate) fn explain_setup_failure(
    bootstrap: &TestBootstrapSettings,
    original: BootstrapError,
) -> BootstrapError {
    explain_setup_failure_with(&LddResolver::new(), bootstrap, original)
}

/// [`explain_setup_failure`] through an explicit resolver.
pub(super) fn explain_setup_failure_with<R: SharedLibraryResolver>(
    resolver: &R,
    bootstrap: &TestBootstrapSettings,
    original: BootstrapError,
) -> BootstrapError {
    let Some(staged) = locate(bootstrap) else {
        return original;
    };
    match Preflight::new(resolver, staged.context).diagnose_executables(&staged.dir) {
        Err(unresolved @ RuntimePreflightError::Unresolved(_)) => {
            warn!(
                target: LOG_TARGET,
                setup_error = %original,
                "setup failed and the staged binaries have unresolved libraries"
            );
            into_bootstrap_error(unresolved)
        }
        Ok(()) => original,
        Err(RuntimePreflightError::Indeterminate(why)) => note_unavailable(original, &why.reason),
    }
}

/// Keeps `original` as the error and adds why the preflight could not explain it.
fn note_unavailable(original: BootstrapError, reason: &str) -> BootstrapError {
    let kind = original.kind();
    let note = format!("{original} (runtime preflight unavailable: {reason})");
    BootstrapError::new(kind, original.into_report().wrap_err(note))
}

/// Async twin of [`check_staged_executables`]; `ldd` runs on the blocking pool.
///
/// # Errors
/// As for [`check_staged_executables`], plus a failure of the blocking task.
#[cfg(feature = "async-api")]
pub(crate) async fn check_staged_executables_async(
    bootstrap: &TestBootstrapSettings,
) -> BootstrapResult<()> {
    check_staged_executables_async_with(LddResolver::new(), bootstrap).await
}

/// [`check_staged_executables_async`] through an explicit resolver.
#[cfg(feature = "async-api")]
pub(super) async fn check_staged_executables_async_with<R>(
    resolver: R,
    bootstrap: &TestBootstrapSettings,
) -> BootstrapResult<()>
where
    R: SharedLibraryResolver + Send + 'static,
{
    let owned = bootstrap.clone();
    tokio::task::spawn_blocking(move || check_staged_executables_with(&resolver, &owned))
        .await
        .unwrap_or_else(|err| Err(blocking_task_failed(err)))
}

/// Async twin of [`check_extension_libraries`]; `ldd` runs on the blocking pool.
///
/// # Errors
/// As for [`check_extension_libraries`], plus a failure of the blocking task.
#[cfg(feature = "async-api")]
pub(crate) async fn check_extension_libraries_async(
    bootstrap: &TestBootstrapSettings,
) -> BootstrapResult<()> {
    let owned = bootstrap.clone();
    tokio::task::spawn_blocking(move || check_extension_libraries(&owned))
        .await
        .unwrap_or_else(|err| Err(blocking_task_failed(err)))
}

/// Async twin of [`explain_setup_failure`]; `ldd` runs on the blocking pool.
#[cfg(feature = "async-api")]
pub(crate) async fn explain_setup_failure_async(
    bootstrap: &TestBootstrapSettings,
    original: BootstrapError,
) -> BootstrapError {
    let owned = bootstrap.clone();
    tokio::task::spawn_blocking(move || explain_setup_failure(&owned, original))
        .await
        .unwrap_or_else(blocking_task_failed)
}

/// Replaces a failed async `Setup` result with its explanation, when one exists.
///
/// # Errors
/// Returns `result`'s error, possibly replaced by the typed explanation.
#[cfg(feature = "async-api")]
pub(crate) async fn explain_failed_setup_async(
    bootstrap: &TestBootstrapSettings,
    result: BootstrapResult<()>,
) -> BootstrapResult<()> {
    match result {
        Ok(()) => Ok(()),
        Err(err) => Err(explain_setup_failure_async(bootstrap, err).await),
    }
}

#[cfg(feature = "async-api")]
fn blocking_task_failed(err: tokio::task::JoinError) -> BootstrapError {
    BootstrapError::from(color_eyre::Report::new(err).wrap_err("runtime preflight task failed"))
}

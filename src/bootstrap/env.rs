//! Parses environment variables used by the bootstrapper and surfaces the
//! resulting configuration for the filesystem preparers.
#[cfg(unix)]
use std::ffi::OsString;
use std::{
    env::{self, VarError},
    io::ErrorKind,
    path::PathBuf,
    time::Duration,
};

use camino::{Utf8Path, Utf8PathBuf};
#[cfg(unix)]
use cap_std::fs::PermissionsExt;
use color_eyre::eyre::Report;

pub use crate::bootstrap::env_types::TestBootstrapEnvironment;
pub(super) use crate::bootstrap::env_types::XdgDirs;
use crate::{
    bootstrap::{
        env_types::TimezoneEnv,
        mode::{ExecutionPrivileges, root_privilege_drop_supported},
    },
    error::{BootstrapError, BootstrapErrorKind, BootstrapResult},
    fs::ambient_dir_and_path,
};
#[cfg(unix)]
const WORKER_BINARY_NAME: &str = "pg_worker";
pub(super) const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_SHUTDOWN_TIMEOUT_SECS: u64 = 600;
const SHUTDOWN_TIMEOUT_ENV: &str = "PG_SHUTDOWN_TIMEOUT_SECS";

#[cfg(unix)]
pub(super) fn discover_worker_from_path() -> BootstrapResult<Option<Utf8PathBuf>> {
    discover_worker_from_path_value(env::var_os("PATH"))
}

/// Hard-fails on non-UTF-8 PATH entries, skips empty/"." entries, and returns
/// the first executable match for `WORKER_BINARY_NAME`.
#[cfg(unix)]
fn discover_worker_from_path_value(
    path_var: Option<OsString>,
) -> BootstrapResult<Option<Utf8PathBuf>> {
    let Some(path_value) = path_var else {
        return Ok(None);
    };
    for entry in env::split_paths(&path_value) {
        let dir = Utf8PathBuf::from_path_buf(entry).map_err(|invalid_entry| {
            let invalid_value = invalid_entry.as_os_str().to_string_lossy();
            let report = color_eyre::eyre::eyre!(
                "PATH contains a non-UTF-8 entry: {invalid_value:?}; remove or replace the \
                 malformed entry."
            );
            BootstrapError::new(BootstrapErrorKind::WorkerBinaryPathNonUtf8, report)
        })?;

        if dir.as_str().is_empty() || dir.as_str() == "." {
            continue;
        }

        let worker_path = dir.join(WORKER_BINARY_NAME);
        if worker_path.is_file() && is_executable(&worker_path) {
            return Ok(Some(worker_path));
        }
    }

    Ok(None)
}

#[cfg(unix)]
fn is_executable(path: &Utf8Path) -> bool {
    path.metadata().is_ok_and(|m| {
        // std metadata uses std permissions; keep this trait import for mode().
        use std::os::unix::fs::PermissionsExt;
        m.permissions().mode() & 0o111 != 0
    })
}

/// Common Unix paths where time zone databases may be installed.
///
/// Used by `find_timezone_dir` to probe for the first existing candidate.
#[cfg(unix)]
pub const TZDIR_CANDIDATES: [&str; 4] = [
    "/usr/share/zoneinfo",
    "/usr/lib/zoneinfo",
    "/etc/zoneinfo",
    "/share/zoneinfo",
];

/// Probes common Unix paths for the time zone database directory.
///
/// Returns the first existing candidate from `TZDIR_CANDIDATES`, or `None` if
/// none exist or on non-Unix platforms. This helper enables test harnesses to
/// set `TZDIR` consistently with production bootstrap logic.
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::find_timezone_dir;
/// if let Some(tzdir) = find_timezone_dir() {
///     println!("Found timezone directory: {}", tzdir);
/// }
/// ```
#[must_use]
pub fn find_timezone_dir() -> Option<&'static Utf8Path> {
    #[cfg(unix)]
    {
        TZDIR_CANDIDATES
            .iter()
            .copied()
            .map(Utf8Path::new)
            .find(|path| path.exists())
    }
    #[cfg(not(unix))]
    {
        None
    }
}
pub(super) fn shutdown_timeout_from_env() -> BootstrapResult<Duration> {
    match env::var(SHUTDOWN_TIMEOUT_ENV) {
        Ok(raw) => parse_shutdown_timeout(raw.trim()),
        Err(VarError::NotPresent) => Ok(DEFAULT_SHUTDOWN_TIMEOUT),
        Err(VarError::NotUnicode(value)) => Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "{SHUTDOWN_TIMEOUT_ENV} must contain a valid UTF-8 value (received {:?})",
            value
        ))),
    }
}

/// Parses a trimmed `PG_SHUTDOWN_TIMEOUT_SECS` value into a duration.
fn parse_shutdown_timeout(trimmed: &str) -> BootstrapResult<Duration> {
    if trimmed.is_empty() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "{SHUTDOWN_TIMEOUT_ENV} is present but empty"
        )));
    }

    let seconds: u64 = trimmed.parse().map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "failed to parse {SHUTDOWN_TIMEOUT_ENV} from '{trimmed}': {err}"
        ))
    })?;

    validate_shutdown_timeout_secs(seconds, trimmed)
}

/// Validates that the parsed timeout lies within the supported range.
fn validate_shutdown_timeout_secs(seconds: u64, trimmed: &str) -> BootstrapResult<Duration> {
    if seconds == 0 {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "{SHUTDOWN_TIMEOUT_ENV} must be at least 1 second (received {trimmed})"
        )));
    }

    if seconds > MAX_SHUTDOWN_TIMEOUT_SECS {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "{SHUTDOWN_TIMEOUT_ENV} must be {MAX_SHUTDOWN_TIMEOUT_SECS} seconds or less (received \
             {trimmed})"
        )));
    }

    Ok(Duration::from_secs(seconds))
}

/// Validates an explicitly supplied shutdown timeout.
///
/// Applies the same one-to-600-second bounds as
/// `PG_SHUTDOWN_TIMEOUT_SECS`, but reports the value as a duration because no
/// environment variable is involved.
pub(super) fn validate_explicit_shutdown_timeout(timeout: Duration) -> BootstrapResult<Duration> {
    if timeout < Duration::from_secs(1) {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "shutdown timeout must be at least 1 second (received {timeout:?})"
        )));
    }

    if timeout > Duration::from_secs(MAX_SHUTDOWN_TIMEOUT_SECS) {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "shutdown timeout must be {MAX_SHUTDOWN_TIMEOUT_SECS} seconds or less (received \
             {timeout:?})"
        )));
    }

    Ok(timeout)
}

/// Label for a worker path read from `PG_EMBEDDED_WORKER`.
const WORKER_ENV_SOURCE: &str = "PG_EMBEDDED_WORKER";
/// Label for a worker path supplied through configuration.
pub(super) const EXPLICIT_WORKER_SOURCE: &str = "the explicit worker binary";
/// Label for a worker path found by searching `PATH`.
#[cfg(unix)]
const DISCOVERED_WORKER_SOURCE: &str = "the worker binary discovered on PATH";

/// Resolves the worker binary from `PG_EMBEDDED_WORKER`.
///
/// The variable is read only for privileged execution, so a non-UTF-8 value
/// stays invisible to unprivileged runs. Validation and `PATH` discovery are
/// shared with [`resolve_explicit_worker`] to keep both paths in step.
pub(super) fn worker_binary_from_env(
    privileges: ExecutionPrivileges,
) -> BootstrapResult<Option<Utf8PathBuf>> {
    if privileges != ExecutionPrivileges::Root || !root_privilege_drop_supported() {
        return Ok(None);
    }

    let supplied = env::var_os(WORKER_ENV_SOURCE)
        .map(|raw| {
            Utf8PathBuf::from_path_buf(PathBuf::from(&raw)).map_err(|_| {
                let invalid_value = raw.to_string_lossy().to_string();
                BootstrapError::from(color_eyre::eyre::eyre!(
                    "PG_EMBEDDED_WORKER contains a non-UTF-8 value: {invalid_value:?}. Provide a \
                     UTF-8 encoded absolute path to the worker binary."
                ))
            })
        })
        .transpose()?;

    resolve_explicit_worker(privileges, supplied, WORKER_ENV_SOURCE)
}

/// Resolves the worker binary for an explicit bootstrap.
///
/// Mirrors [`worker_binary_from_env`] without reading `PG_EMBEDDED_WORKER`: a
/// supplied path is validated, and an absent one falls back to `PATH`
/// discovery. Validation errors name `supplied_source` so the caller is not
/// pointed at an environment variable the explicit path never reads.
/// Unprivileged execution ignores the option entirely.
pub(super) fn resolve_explicit_worker(
    privileges: ExecutionPrivileges,
    worker: Option<Utf8PathBuf>,
    supplied_source: &str,
) -> BootstrapResult<Option<Utf8PathBuf>> {
    if privileges != ExecutionPrivileges::Root || !root_privilege_drop_supported() {
        return Ok(None);
    }

    if let Some(path) = worker {
        validate_worker_path(&path, supplied_source)?;
        return Ok(Some(path));
    }

    #[cfg(unix)]
    {
        if let Some(discovered) = discover_worker_from_path()? {
            validate_worker_path(&discovered, DISCOVERED_WORKER_SOURCE)?;
            return Ok(Some(discovered));
        }
    }

    Ok(None)
}

pub(super) fn validate_worker_path(path: &Utf8PathBuf, source: &str) -> BootstrapResult<()> {
    if path.as_str().is_empty() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "Worker binary path must not be empty"
        )));
    }
    if path.as_str() == "/" {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "Worker binary path must not point at filesystem root"
        )));
    }

    validate_worker_binary(path, source)?;
    Ok(())
}

fn validate_worker_binary(path: &Utf8PathBuf, source: &str) -> BootstrapResult<()> {
    let (dir, relative) =
        ambient_dir_and_path(path).map_err(|err| worker_binary_error(path, source, err))?;
    let metadata = dir
        .metadata(relative.as_std_path())
        .map_err(|err| worker_binary_error(path, source, Report::new(err)))?;

    if !metadata.is_file() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "{source} must reference a regular file: {path}"
        )));
    }

    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "{source} must be executable: {path}"
            )));
        }
    }

    Ok(())
}

fn worker_binary_error(path: &Utf8Path, source: &str, err: Report) -> BootstrapError {
    let is_not_found = error_chain_has_not_found(&err);
    let context = format!("failed to access {source} at {path}: {err}");
    let report = err.wrap_err(context);

    if is_not_found {
        BootstrapError::new(BootstrapErrorKind::WorkerBinaryMissing, report)
    } else {
        BootstrapError::from(report)
    }
}

fn error_chain_has_not_found(err: &Report) -> bool {
    err.chain()
        .filter_map(|source| source.downcast_ref::<std::io::Error>())
        .any(|source| source.kind() == ErrorKind::NotFound)
}

pub(super) fn prepare_timezone_env() -> BootstrapResult<TimezoneEnv> {
    const DEFAULT_TIMEZONE: &str = "UTC";

    let tz_dir = if let Some(dir) = env::var_os("TZDIR") {
        let path = Utf8PathBuf::from_path_buf(PathBuf::from(dir)).map_err(
            |_| -> crate::error::BootstrapError {
                color_eyre::eyre::eyre!("TZDIR must be valid UTF-8").into()
            },
        )?;
        if !path.exists() {
            return Err(color_eyre::eyre::eyre!(
                "time zone database not found at {}. Set TZDIR or install tzdata.",
                path
            )
            .into());
        }
        Some(path)
    } else {
        discover_timezone_dir()?
    };

    let timezone = match env::var("TZ") {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) | Err(std::env::VarError::NotPresent) => DEFAULT_TIMEZONE.to_owned(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err(color_eyre::eyre::eyre!("TZ must be valid UTF-8").into());
        }
    };

    Ok(TimezoneEnv {
        dir: tz_dir,
        zone: timezone,
    })
}

fn discover_timezone_dir() -> BootstrapResult<Option<Utf8PathBuf>> {
    #[cfg(unix)]
    {
        let candidate = find_timezone_dir().ok_or_else(|| -> crate::error::BootstrapError {
            color_eyre::eyre::eyre!("time zone database not found. Set TZDIR or install tzdata.")
                .into()
        })?;

        Ok(Some(candidate.to_owned()))
    }

    #[cfg(not(unix))]
    {
        Ok(None)
    }
}

#[cfg(all(test, unix))]
#[path = "env_tests.rs"]
mod tests;

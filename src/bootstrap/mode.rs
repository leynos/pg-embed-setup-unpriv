//! Detects execution privileges and selects the appropriate orchestration mode.

use camino::Utf8PathBuf;

use crate::error::BootstrapError;
use crate::error::BootstrapResult;

#[cfg(unix)]
use nix::unistd::geteuid;

/// Represents the privileges the process is running with when bootstrapping `PostgreSQL`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionPrivileges {
    /// The process owns `root` privileges and must drop to `nobody` for filesystem work.
    Root,
    /// The process is already unprivileged, so bootstrap tasks run with the current UID/GID.
    Unprivileged,
}

/// Selects how `PostgreSQL` lifecycle commands run when privileged execution is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Execute lifecycle commands directly within the current process.
    ///
    /// This mode is only appropriate when the process already runs without elevated privileges.
    InProcess,
    /// Delegate lifecycle commands to a helper subprocess executed with reduced privileges.
    Subprocess,
}

/// Detects whether the process is running with root privileges.
///
/// # Examples
/// ```
/// use pg_embedded_setup_unpriv::{detect_execution_privileges, ExecutionPrivileges};
///
/// let privileges = detect_execution_privileges();
/// let mode = match privileges {
///     ExecutionPrivileges::Root => "subprocess",
///     ExecutionPrivileges::Unprivileged => "in-process",
/// };
/// assert!(matches!(mode, "subprocess" | "in-process"));
/// ```
#[must_use]
pub fn detect_execution_privileges() -> ExecutionPrivileges {
    #[cfg(unix)]
    {
        if geteuid().is_root() {
            ExecutionPrivileges::Root
        } else {
            ExecutionPrivileges::Unprivileged
        }
    }

    #[cfg(not(unix))]
    {
        ExecutionPrivileges::Unprivileged
    }
}

pub(crate) const fn root_privilege_drop_supported() -> bool {
    cfg!(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        )
    ))
}

pub(crate) fn unsupported_root_privilege_drop_error() -> BootstrapError {
    BootstrapError::from(color_eyre::eyre::eyre!(
        "privilege drop is not supported on this target; run without root privileges"
    ))
}

pub(super) fn determine_execution_mode(
    privileges: ExecutionPrivileges,
    worker_binary: Option<&Utf8PathBuf>,
) -> BootstrapResult<ExecutionMode> {
    #[cfg(unix)]
    {
        match privileges {
            ExecutionPrivileges::Root => {
                if !root_privilege_drop_supported() {
                    return Err(unsupported_root_privilege_drop_error());
                }
                if worker_binary.is_none() {
                    Err(BootstrapError::from(color_eyre::eyre::eyre!(
                        "PG_EMBEDDED_WORKER must be set when running with root privileges"
                    )))
                } else {
                    Ok(ExecutionMode::Subprocess)
                }
            }
            ExecutionPrivileges::Unprivileged => Ok(ExecutionMode::InProcess),
        }
    }

    #[cfg(not(unix))]
    {
        let _ = worker_binary;
        match privileges {
            ExecutionPrivileges::Root => Err(unsupported_root_privilege_drop_error()),
            ExecutionPrivileges::Unprivileged => Ok(ExecutionMode::InProcess),
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for execution mode determination.

    use super::*;

    #[cfg(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        )
    ))]
    #[test]
    fn determine_execution_mode_requires_worker_when_root() {
        let err = determine_execution_mode(ExecutionPrivileges::Root, None)
            .expect_err("root execution without worker must error");
        let message = err.to_string();
        assert!(
            message.contains("PG_EMBEDDED_WORKER must be set"),
            "unexpected error message: {message}",
        );
    }

    #[cfg(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        )
    ))]
    #[test]
    fn determine_execution_mode_allows_subprocess_with_worker() {
        let worker = Utf8PathBuf::from("/tmp/pg_worker");
        let mode = determine_execution_mode(ExecutionPrivileges::Root, Some(&worker))
            .expect("root execution with worker should succeed");
        assert_eq!(mode, ExecutionMode::Subprocess);
    }

    #[cfg(unix)]
    #[test]
    fn determine_execution_mode_in_process_when_unprivileged() {
        let mode = determine_execution_mode(ExecutionPrivileges::Unprivileged, None)
            .expect("unprivileged execution should succeed");
        assert_eq!(mode, ExecutionMode::InProcess);
    }

    #[cfg(unix)]
    #[test]
    fn determine_execution_mode_ignores_worker_when_unprivileged() {
        let worker = Utf8PathBuf::from("/tmp/pg_worker");
        let mode = determine_execution_mode(ExecutionPrivileges::Unprivileged, Some(&worker))
            .expect("unprivileged execution should succeed with worker configured");
        assert_eq!(mode, ExecutionMode::InProcess);
    }

    #[cfg(not(all(
        unix,
        any(
            target_os = "linux",
            target_os = "android",
            target_os = "freebsd",
            target_os = "openbsd",
            target_os = "dragonfly",
        )
    )))]
    #[test]
    fn determine_execution_mode_rejects_root_when_privilege_drop_is_unsupported() {
        let worker = Utf8PathBuf::from("/tmp/pg_worker");
        let err = determine_execution_mode(ExecutionPrivileges::Root, Some(&worker))
            .expect_err("non-unix root execution should fail before worker dispatch");
        let message = err.to_string();
        assert!(
            message.contains("privilege drop is not supported"),
            "unexpected error message: {message}",
        );
    }
}

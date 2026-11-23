//! Runtime-compatibility preflight for staged `PostgreSQL` archives.
//!
//! The prebuilt archives link against the host's shared libraries. On a host
//! that lacks one (for example `libxml2.so.2` on Ubuntu 26.04), the staged
//! `postgres` fails to load and the consumer sees an opaque start-up failure.
//! This module asks the host's resolver, behind a port, which SONAMEs are
//! unresolved and reports them as a typed, actionable error.
//!
//! It never symlinks a SONAME and never fetches a library: it reports.
//!
//! The hooks run at three points of the lifecycle, because
//! `postgresql_embedded`'s `setup()` downloads and runs `initdb` in one call:
//!
//! 1. before `Setup`, when a tree is already staged (a cache hit or an earlier install), so the
//!    check precedes `initdb`;
//! 2. after a failed `Setup`, to replace an opaque `initdb` failure with the explanation, which is
//!    how a first download is diagnosed;
//! 3. after extensions are installed, for their shared libraries.
//!
//! Linux is checked with `ldd`; other hosts skip with a debug log.

mod check;
mod error;
mod host;
mod marker;
mod resolver;
mod stage;

#[cfg(test)]
mod tests;

pub use error::{BinaryFinding, IndeterminateRuntime, RuntimePreflightError, UnresolvedLibraries};
pub(crate) use marker::MARKER_FILE;
pub(crate) use stage::{
    check_extension_libraries,
    check_staged_executables,
    explain_setup_failure,
};
#[cfg(feature = "async-api")]
pub(crate) use stage::{
    check_extension_libraries_async,
    check_staged_executables_async,
    explain_failed_setup_async,
};

/// Whether the bootstrap checks staged binaries for unresolved libraries.
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::RuntimePreflight;
///
/// assert_eq!(RuntimePreflight::default(), RuntimePreflight::Check);
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum RuntimePreflight {
    /// Check, and fail with a typed error when the host cannot run the archive
    /// or cannot say. This is the default.
    #[default]
    Check,
    /// Do not check. For hosts without a dependency resolver, such as
    /// distroless images, where the failure would otherwise be indeterminate.
    Skip,
}

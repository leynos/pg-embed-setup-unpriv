//! Typed failures of the runtime-compatibility preflight.
//!
//! The preflight reports a problem a consumer fixes by changing the host or
//! the archive, so every variant carries what that consumer needs: which
//! binary, which SONAMEs, which host, which archive, and the two remedies.

use std::fmt;

use camino::Utf8PathBuf;
use thiserror::Error;

/// Unresolved shared libraries found for one staged object.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BinaryFinding {
    /// The staged executable or extension library that cannot be loaded.
    pub binary: Utf8PathBuf,
    /// Each SONAME the host loader cannot resolve, for example `libxml2.so.2`.
    pub missing: Vec<String>,
}

/// What a host and archive pair is missing, reported before any cluster start.
///
/// Callers can recover this value from a bootstrap failure by downcasting the
/// report to [`RuntimePreflightError`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnresolvedLibraries {
    /// One entry per staged object with at least one unresolved SONAME.
    pub findings: Vec<BinaryFinding>,
    /// The host operating system, from `/etc/os-release`.
    pub host_os: String,
    /// The `PostgreSQL` version that was staged.
    pub postgresql_version: String,
    /// Where the archive came from (the configured releases URL).
    pub archive_source: String,
}

/// The preflight could not decide whether the staged binaries can load.
///
/// This is an error rather than a pass: a silent pass would hide the very
/// failure the preflight exists to explain.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IndeterminateRuntime {
    /// Why the host's dependency resolver could not answer.
    pub reason: String,
    /// The host operating system, from `/etc/os-release`.
    pub host_os: String,
    /// The `PostgreSQL` version that was staged.
    pub postgresql_version: String,
    /// Where the archive came from (the configured releases URL).
    pub archive_source: String,
}

/// A runtime-compatibility failure, in one of its two forms.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum RuntimePreflightError {
    /// The host lacks libraries the staged objects need.
    #[error("{0}")]
    Unresolved(UnresolvedLibraries),
    /// The host's resolver could not run, so compatibility is unknown.
    #[error("{0}")]
    Indeterminate(IndeterminateRuntime),
}

/// The remedies for a host that cannot run the staged archive.
const INCOMPATIBLE_REMEDIES: &str = concat!(
    "Remedies: (1) run on a supported host image with the documented runtime ",
    "packages installed (see \"Supported host images\" in the users' guide); or ",
    "(2) point `with_releases_url` at a PostgreSQL archive built for this host."
);

/// The remedies for a host whose resolver is unavailable.
const INDETERMINATE_REMEDIES: &str = concat!(
    "Remedies: (1) install the resolver (`libc-bin` provides `ldd` on Debian ",
    "and Ubuntu); or (2) opt out with ",
    "`TestBootstrapConfig::with_runtime_preflight(RuntimePreflight::Skip)`."
);

impl fmt::Display for UnresolvedLibraries {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "the staged PostgreSQL {} (archive source: {}) cannot run on this host ({}); \
             unresolved shared libraries:",
            self.postgresql_version, self.archive_source, self.host_os
        )?;
        for finding in &self.findings {
            writeln!(f, "  {}: {}", finding.binary, finding.missing.join(", "))?;
        }
        write!(f, "{INCOMPATIBLE_REMEDIES}")
    }
}

impl fmt::Display for IndeterminateRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "cannot tell whether the staged PostgreSQL {} (archive source: {}) can run on this \
             host ({}): {}. {INDETERMINATE_REMEDIES}",
            self.postgresql_version, self.archive_source, self.host_os, self.reason
        )
    }
}

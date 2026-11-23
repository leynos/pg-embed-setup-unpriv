//! Host identity used in diagnostics and in the preflight cache key.
//!
//! The pretty name comes from `/etc/os-release` and appears in errors. The
//! fingerprint joins the distribution, release and architecture with a
//! signature of the loader cache (`/etc/ld.so.cache`), so installing or
//! removing a library package invalidates a recorded pass in the marker.

use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use camino::Utf8Path;

/// Where the host's identity lives on systemd-era Linux distributions.
const OS_RELEASE_PATH: &str = "/etc/os-release";

/// The loader's cache of installed libraries, which `ldconfig` rewrites
/// whenever a package adds or removes one.
const LOADER_CACHE_PATH: &str = "/etc/ld.so.cache";

/// The host operating system, as far as the preflight needs to tell hosts apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostOs {
    /// Human-readable name, for example `Ubuntu 26.04 LTS`.
    pub(crate) pretty_name: String,
    /// Machine key of the distribution and release, for example `ubuntu-26.04`,
    /// with the CPU architecture appended.
    pub(crate) fingerprint: String,
}

impl HostOs {
    /// Reads the running host's identity.
    ///
    /// The fingerprint also carries the loader cache's signature, so installing
    /// or removing a library package on the same operating system release
    /// invalidates a recorded pass.
    pub(crate) fn detect() -> Self {
        let mut host = Self::from_os_release_file(Utf8Path::new(OS_RELEASE_PATH));
        let cache = loader_cache_signature(Path::new(LOADER_CACHE_PATH));
        host.fingerprint = format!("{}-ldcache-{cache}", host.fingerprint);
        host
    }

    /// Reads identity from an `os-release` file; a missing file yields `unknown`.
    pub(crate) fn from_os_release_file(path: &Utf8Path) -> Self {
        let contents = fs::read_to_string(path).unwrap_or_default();
        Self::parse(&contents, std::env::consts::ARCH)
    }

    /// Parses `os-release` text for the given architecture.
    pub(crate) fn parse(contents: &str, arch: &str) -> Self {
        let field = |key: &str| os_release_field(contents, key);
        let id = field("ID").unwrap_or_else(|| "unknown".to_owned());
        let version_id = field("VERSION_ID").unwrap_or_default();
        let pretty_name = field("PRETTY_NAME")
            .or_else(|| field("NAME"))
            .unwrap_or_else(|| format!("unknown {}", std::env::consts::OS));
        Self {
            pretty_name,
            fingerprint: format!("{id}-{version_id}-{arch}"),
        }
    }
}

/// Returns the unquoted value of `KEY=value` from `os-release` text.
fn os_release_field(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix('=')?;
        Some(value.trim().trim_matches(['"', '\'']).to_owned())
    })
}

/// Summarizes the loader cache as its size and modification time in nanoseconds.
///
/// A host without a cache (a minimal image before `ldconfig` has run) yields
/// `none`, which differs from any host that has one.
pub(crate) fn loader_cache_signature(path: &Path) -> String {
    let Ok(metadata) = fs::metadata(path) else {
        return "none".to_owned();
    };
    let modified = metadata
        .modified()
        .ok()
        .map_or_else(|| "unknown".to_owned(), signed_nanos_since_epoch);
    format!("{}-{modified}", metadata.len())
}

/// Encodes a time as signed nanoseconds from the Unix epoch, so two distinct
/// times before the epoch stay distinct.
pub(crate) fn signed_nanos_since_epoch(time: SystemTime) -> String {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => format!("+{}", after.as_nanos()),
        Err(before) => format!("-{}", before.duration().as_nanos()),
    }
}

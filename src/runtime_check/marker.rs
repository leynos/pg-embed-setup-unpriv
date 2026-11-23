//! Per-archive memo of a passed preflight.
//!
//! The marker sits inside the staged installation but never enters the shared
//! binary cache: the cache copy skips it in both directions, because a pass
//! describes one host's library environment. Its content is a key that names
//! the archive, the host operating system and the loader cache, so a marker
//! left in an installation directory that moves to another host does not match.

use std::{fs, path::Path};

use tracing::debug;

use crate::observability::LOG_TARGET;

/// File name of the marker inside the staged installation directory.
pub(crate) const MARKER_FILE: &str = ".pg-embed-runtime-ok";

/// Whether the marker in `staged` records a pass for exactly `key`.
pub(crate) fn is_recorded(staged: &Path, key: &str) -> bool {
    fs::read_to_string(staged.join(MARKER_FILE)).is_ok_and(|stored| stored == key)
}

/// Records a pass for `key`; a failure to write only costs a repeat check.
pub(crate) fn record(staged: &Path, key: &str) {
    if let Err(err) = fs::write(staged.join(MARKER_FILE), key) {
        debug!(
            target: LOG_TARGET,
            error = %err,
            "could not record the runtime preflight result"
        );
    }
}

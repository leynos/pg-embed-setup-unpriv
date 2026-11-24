//! The one test-side process-environment writer, for the fixture's isolation
//! checks.
//!
//! The fixture tests run under the scenario lock. The guard snapshots the
//! process environment around each fixture run and restores it afterwards, and
//! the leak probe plants a variable that the restore must remove. Both go when
//! the cluster API takes an injected environment (0.9.0).

use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
};

/// Captures the process environment and restores it on drop.
pub struct EnvIsolationGuard {
    snapshot: Vec<(OsString, OsString)>,
}

impl EnvIsolationGuard {
    /// Records the current process environment for later restoration.
    #[must_use]
    pub fn capture() -> Self {
        Self {
            snapshot: std::env::vars_os().collect(),
        }
    }
}

impl Drop for EnvIsolationGuard {
    fn drop(&mut self) {
        let saved_keys: HashSet<OsString> =
            self.snapshot.iter().map(|(key, _)| key.clone()).collect();
        let current_keys: Vec<OsString> = std::env::vars_os().map(|(key, _)| key).collect();
        for key in current_keys {
            if !saved_keys.contains(&key) {
                unsafe { remove_env_var(&key) };
            }
        }
        for (key, value) in &self.snapshot {
            unsafe { set_env_var(key, value) };
        }
    }
}

/// Sets an environment variable whilst bypassing nightly's lint.
#[expect(
    clippy::disallowed_methods,
    reason = "the one test-side environment writer: restores the snapshot taken by \
              EnvIsolationGuard and plants the fixture's leak probe, under the scenario lock; \
              goes when the cluster API takes an injected environment (0.9.0)"
)]
pub unsafe fn set_env_var<K, V>(key: K, value: V)
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    // SAFETY: callers must serialize environment mutations; enforced at call sites.
    unsafe { std::env::set_var(key, value) };
}

/// Removes an environment variable whilst bypassing nightly's lint.
#[expect(
    clippy::disallowed_methods,
    reason = "the one test-side environment writer: restores the snapshot taken by \
              EnvIsolationGuard and plants the fixture's leak probe, under the scenario lock; \
              goes when the cluster API takes an injected environment (0.9.0)"
)]
pub unsafe fn remove_env_var<K>(key: K)
where
    K: AsRef<OsStr>,
{
    // SAFETY: callers must serialize environment mutations; enforced at call sites.
    unsafe { std::env::remove_var(key) };
}

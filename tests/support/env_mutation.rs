//! Low-level process environment mutation helpers for isolated tests.

use std::ffi::OsStr;

/// Sets an environment variable whilst bypassing nightly's lint.
pub unsafe fn set_env_var<K, V>(key: K, value: V)
where
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    // SAFETY: callers must serialize environment mutations; enforced at call sites.
    unsafe { std::env::set_var(key, value) };
}

/// Removes an environment variable whilst bypassing nightly's lint.
pub unsafe fn remove_env_var<K>(key: K)
where
    K: AsRef<OsStr>,
{
    // SAFETY: callers must serialize environment mutations; enforced at call sites.
    unsafe { std::env::remove_var(key) };
}

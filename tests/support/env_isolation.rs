//! Environment helpers for isolating test scenarios.
//!
//! These build the variable lists handed to a scoped environment; they never
//! change the process environment themselves.

use std::ffi::{OsStr, OsString};

use camino::Utf8Path;

use super::env::ScopedEnvVars;

/// Overrides an environment variable entry in `vars`.
pub fn override_env_os(vars: &mut ScopedEnvVars, key: impl AsRef<OsStr>, value: Option<OsString>) {
    let key_ref = key.as_ref();
    if let Some((_, existing_value)) = vars
        .iter_mut()
        .find(|(candidate, _)| candidate.as_os_str() == key_ref)
    {
        *existing_value = value;
    } else {
        vars.push((key_ref.to_os_string(), value));
    }
}

/// Overrides a UTF-8 environment variable entry in `vars`.
pub fn override_env_path(vars: &mut ScopedEnvVars, key: impl AsRef<OsStr>, value: &Utf8Path) {
    override_env_os(vars, key, Some(OsString::from(value.as_str())));
}

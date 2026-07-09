//! Environment input validation for scoped environment state.

use std::ffi::OsString;

/// Reject environment keys that `std::env` cannot mutate safely.
pub(super) fn validate_env_key(key: &OsString) {
    assert!(
        !key.is_empty(),
        "ScopedEnv received an empty environment variable name"
    );
    assert!(
        !contains_nul(key),
        "ScopedEnv received an environment variable name containing NUL"
    );
    assert!(
        !contains_equals(key),
        "ScopedEnv received an environment variable name containing '='"
    );
}

/// Reject environment values that would make `std::env::set_var` panic.
pub(super) fn validate_env_value(value: Option<&OsString>) {
    if let Some(env_value) = value {
        assert!(
            !contains_nul(env_value),
            "ScopedEnv received an environment variable value containing NUL"
        );
    }
}

/// Report whether an environment key is valid for debug assertions.
pub(super) fn is_valid_env_key(key: &OsString) -> bool {
    !key.is_empty() && !contains_nul(key) && !contains_equals(key)
}

/// Report whether an environment value is valid for debug assertions.
pub(super) fn is_valid_env_value(value: Option<&OsString>) -> bool {
    value.is_none_or(|env_value| !contains_nul(env_value))
}

/// Detect `=` in Unix environment keys without lossy conversion.
#[cfg(unix)]
fn contains_equals(key: &OsString) -> bool {
    use std::os::unix::ffi::OsStrExt;

    key.as_os_str().as_bytes().contains(&b'=')
}

/// Detect `=` in Windows environment keys using wide units.
#[cfg(windows)]
fn contains_equals(key: &OsString) -> bool {
    use std::os::windows::ffi::OsStrExt;

    key.as_os_str()
        .encode_wide()
        .any(|value| value == u16::from(b'='))
}

/// Detect `=` in environment keys on fallback platforms.
#[cfg(not(any(unix, windows)))]
fn contains_equals(key: &OsString) -> bool { key.to_string_lossy().contains('=') }

/// Detect NUL bytes in Unix environment keys or values.
#[cfg(unix)]
fn contains_nul(value: &OsString) -> bool {
    use std::os::unix::ffi::OsStrExt;

    value.as_os_str().as_bytes().contains(&b'\0')
}

/// Detect NUL units in Windows environment keys or values.
#[cfg(windows)]
fn contains_nul(value: &OsString) -> bool {
    use std::os::windows::ffi::OsStrExt;

    value.as_os_str().encode_wide().any(|unit| unit == 0)
}

/// Detect NUL characters in environment values on fallback platforms.
#[cfg(not(any(unix, windows)))]
fn contains_nul(value: &OsString) -> bool { value.to_string_lossy().contains('\0') }

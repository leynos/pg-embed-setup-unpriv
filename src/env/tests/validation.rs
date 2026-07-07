//! Validation tests for pre-lock scoped environment input rejection.

use std::{ffi::OsString, panic};

use rstest::rstest;
use serial_test::serial;

use super::{ScopedEnv, ThreadState, assert_env_lock_released, assert_thread_state_reset};

/// Verifies `apply_os` rejects invalid environment input before locking.
#[rstest]
#[case::empty_key(
    vec![(OsString::from(""), Some(OsString::from("value")))],
    "empty environment names",
)]
#[case::contains_equals(
    vec![(OsString::from("INVALID=KEY"), Some(OsString::from("value")))],
    "environment names containing '='",
)]
#[case::key_contains_nul(
    vec![(OsString::from("INVALID\0KEY"), Some(OsString::from("value")))],
    "environment names containing NUL",
)]
#[case::value_contains_nul(
    vec![(OsString::from("INVALID_VALUE"), Some(OsString::from("bad\0value")))],
    "environment values containing NUL",
)]
#[serial]
fn apply_os_rejects_invalid_input_before_lock(
    #[case] vars: Vec<(OsString, Option<OsString>)>,
    #[case] reason: &str,
) {
    let result = panic::catch_unwind(|| {
        let _guard = ScopedEnv::apply_os(vars);
    });

    assert!(result.is_err(), "apply_os must reject {reason}");
    assert_thread_state_reset();
    assert_env_lock_released();
}

/// Verifies raw thread state rejects invalid input without acquiring the lock.
#[rstest]
#[case::empty_key(vec![(
    OsString::from(""),
    Some(OsString::from("value")),
)])]
#[case::key_contains_nul(vec![(
    OsString::from("THREAD_STATE\0INVALID_KEY"),
    Some(OsString::from("value")),
)])]
#[case::value_contains_nul(vec![(
    OsString::from("THREAD_STATE_INVALID_VALUE"),
    Some(OsString::from("bad\0value")),
)])]
#[serial]
fn thread_state_rejects_nul_before_lock(#[case] vars: Vec<(OsString, Option<OsString>)>) {
    let mut state = ThreadState::new();

    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        let _index = state.enter_scope(vars);
    }));

    assert!(result.is_err(), "ThreadState must reject invalid input");
    assert_eq!(state.depth(), 0);
    assert!(state.is_stack_empty());
    assert!(!state.has_lock());
    assert_env_lock_released();
}

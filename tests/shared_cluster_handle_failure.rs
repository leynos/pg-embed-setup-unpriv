//! Tests that `shared_cluster_handle()` caches failed initialisation.
//!
//! This test file runs in its own process (Cargo compiles each `tests/*.rs`
//! file as a separate binary), ensuring the global `OnceLock` state is isolated
//! from other tests.
//!
//! **Important**: This file must contain only this one test. Adding other tests
//! that call `shared_cluster_handle()` would interfere with the failure caching
//! verification.
#![cfg(unix)]

use pg_embedded_setup_unpriv::ScopedEnv;
use pg_embedded_setup_unpriv::test_support::{scoped_env, shared_cluster_handle};
use pg_embedded_setup_unpriv::{BootstrapError, BootstrapErrorKind, ClusterHandle};
use std::ffi::OsString;
use tracing::warn;

/// Sets up the environment to force bootstrap failure.
fn setup_failing_environment() -> ScopedEnv {
    scoped_env(vec![
        (
            OsString::from("TZDIR"),
            Some(OsString::from(
                "/nonexistent/timezone/directory/that/does/not/exist",
            )),
        ),
        // Clear TZ to ensure the bootstrap tries to read from TZDIR.
        (OsString::from("TZ"), None),
    ])
}

/// Extracts the error from a result, or returns None if it succeeded.
///
/// Logs a skip message if the call unexpectedly succeeded.
fn extract_error(
    result: Result<&'static ClusterHandle, BootstrapError>,
    context: &str,
) -> Option<BootstrapError> {
    if let Err(e) = result {
        return Some(e);
    }
    warn!(
        concat!(
            "SKIP: shared_cluster_handle succeeded despite invalid TZDIR ",
            "({}); system may have fallback timezone handling"
        ),
        context
    );
    None
}

/// Verifies that the cached error preserves the original error kind.
fn verify_error_kind_preserved(first: BootstrapErrorKind, second: BootstrapErrorKind) {
    assert_eq!(
        first, second,
        "cached error should preserve BootstrapErrorKind"
    );
}

/// Verifies that the error message indicates this is a cached failure.
fn verify_cached_error_message(error: &BootstrapError) {
    let message = format!("{error}");
    assert!(
        message.contains("previously failed"),
        "cached error message should indicate previous failure; got: {message}"
    );
}

/// Verifies that `shared_cluster_handle()` caches the error from a failed
/// initialisation attempt and returns the same error on subsequent calls.
///
/// This test forces bootstrap failure by setting TZDIR to a non-existent
/// directory, which causes timezone validation to fail.
#[test]
fn caches_failed_initialisation() {
    // Force bootstrap failure by pointing TZDIR to a non-existent directory.
    let _env_guard = setup_failing_environment();

    let err1 = shared_cluster_handle();
    let Some(first_error) = extract_error(err1, "first call") else {
        return;
    };
    let first_kind = first_error.kind();

    // Second call should return cached error
    let err2 = shared_cluster_handle();
    let second_error = err2.expect_err(
        "second call to shared_cluster_handle succeeded after first call failed; caching is broken",
    );

    verify_error_kind_preserved(first_kind, second_error.kind());
    verify_cached_error_message(&second_error);

    // Third call for good measure
    let err3 = shared_cluster_handle();
    let third_error = err3.expect_err("third call succeeded unexpectedly");

    verify_error_kind_preserved(second_error.kind(), third_error.kind());
}

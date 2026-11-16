//! Tests that the shared cluster singleton enforces the origin of its result.
//!
//! The origin rules are table-tested against a stand-in harness in
//! `src/test_support/shared_config.rs`. This binary drives the production
//! entry points, `shared_cluster_handle_with` and `shared_cluster_handle`, so
//! the wiring of `ensure_compatible` and `record_origin` is covered too.
//!
//! Cargo compiles each `tests/*.rs` file as a separate binary, so the
//! process-wide singleton is isolated from other tests. The singleton can only
//! be primed once per process, so this file must contain only this one test.
#![cfg(unix)]

use std::ffi::OsString;

use pg_embedded_setup_unpriv::{
    PgEnvCfg,
    ScopedEnv,
    TestBootstrapConfig,
    test_support::{scoped_env, shared_cluster_handle, shared_cluster_handle_with},
};

/// Forces bootstrap to fail quickly by pointing `TZDIR` at a missing directory.
fn setup_failing_environment() -> ScopedEnv {
    scoped_env(vec![
        (
            OsString::from("TZDIR"),
            Some(OsString::from(
                "/nonexistent/timezone/directory/that/does/not/exist",
            )),
        ),
        // Clear TZ so bootstrap reads from TZDIR.
        (OsString::from("TZ"), None),
    ])
}

/// An explicit configuration that differs from its siblings only by port.
fn config_on_port(port: u16) -> TestBootstrapConfig {
    TestBootstrapConfig::new(PgEnvCfg {
        port: Some(port),
        ..PgEnvCfg::default()
    })
}

/// A failed explicit initialization fixes the singleton's origin.
///
/// After one explicit configuration fails: a different explicit configuration
/// is rejected as a conflict, an equal one replays the cached failure, and an
/// ambient call reuses the cached failure.
#[test]
fn explicit_origin_governs_later_callers_after_a_failure() {
    let _env_guard = setup_failing_environment();

    // An unexpected success would leave nothing to assert about, so it fails
    // the test rather than skipping it.
    let first = shared_cluster_handle_with(config_on_port(54_321))
        .expect_err("the priming call must fail with an invalid TZDIR")
        .to_string();
    assert!(
        !first.contains("previously failed"),
        "the priming call must report the original failure: {first}"
    );

    let conflicting = shared_cluster_handle_with(config_on_port(54_322))
        .expect_err("a different explicit configuration must conflict");
    assert!(
        conflicting
            .to_string()
            .contains("already initialized with a different configuration"),
        "unexpected conflict message: {conflicting}"
    );

    let replayed = shared_cluster_handle_with(config_on_port(54_321))
        .expect_err("an equal explicit configuration must replay the failure");
    assert!(
        replayed.to_string().contains("previously failed"),
        "unexpected replay message: {replayed}"
    );

    let ambient = shared_cluster_handle().expect_err("an ambient caller must reuse the failure");
    assert!(
        ambient.to_string().contains("previously failed"),
        "unexpected ambient message: {ambient}"
    );
}

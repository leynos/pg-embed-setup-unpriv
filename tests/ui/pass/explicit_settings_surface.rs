//! Compile-time fixture for the explicit-settings public surface.
//!
//! `tests/ui.rs` uses this as a non-Windows trybuild pass fixture and as a
//! directly included Windows smoke-compile module. It proves that a consumer
//! crate can name [`TestBootstrapConfig`], build one with every builder, and
//! pass it to the explicit bootstrap, cluster and shared-singleton
//! constructors without starting a cluster.

use std::time::Duration;

use pg_embedded_setup_unpriv::{
    BootstrapResult,
    PgEnvCfg,
    TestBootstrapConfig,
    TestCluster,
    bootstrap_for_tests_with,
    test_support::shared_cluster_handle_with,
};

/// Builds a configuration through every public builder.
///
/// The values are deliberately inert: this fixture only needs the signatures
/// to type-check, so nothing here is started.
fn build_config() -> TestBootstrapConfig {
    TestBootstrapConfig::new(PgEnvCfg {
        ..PgEnvCfg::default()
    })
    .with_worker_binary("/nonexistent/pg_worker")
    .with_shutdown_timeout(Duration::from_secs(15))
    .with_releases_url("https://example.invalid/releases")
}

/// Names the explicit constructors without running them.
///
/// Keeping the calls in an uncalled function proves the signatures compile
/// whilst leaving the fixture safe to execute.
fn constructors_compile(config: TestBootstrapConfig) -> BootstrapResult<()> {
    let _ = bootstrap_for_tests_with(PgEnvCfg {
        ..PgEnvCfg::default()
    })?;
    let _ = bootstrap_for_tests_with(config.clone())?;
    let _ = TestCluster::new_with(config.clone())?;
    let _ = TestCluster::new_split_with(config.clone())?;
    let _ = shared_cluster_handle_with(config)?;
    Ok(())
}

/// Exercises the accessors and the `From<PgEnvCfg>` conversion.
///
/// # Errors
///
/// Returns an error only if the accessors disagree with the values supplied to
/// the builders.
pub fn verify_surface() -> BootstrapResult<()> {
    let config = build_config();
    assert_eq!(config.cfg(), &PgEnvCfg::default());
    assert_eq!(
        config.worker_binary().map(|path| path.as_str()),
        Some("/nonexistent/pg_worker")
    );
    assert_eq!(config.shutdown_timeout(), Some(Duration::from_secs(15)));
    assert_eq!(
        config.releases_url(),
        Some("https://example.invalid/releases")
    );

    let converted: TestBootstrapConfig = PgEnvCfg::default().into();
    assert_eq!(converted, TestBootstrapConfig::new(PgEnvCfg::default()));

    // Reference the constructor signatures without invoking them.
    let _ = constructors_compile as fn(TestBootstrapConfig) -> BootstrapResult<()>;
    Ok(())
}

#[cfg(not(windows))]
fn main() -> BootstrapResult<()> { verify_surface() }

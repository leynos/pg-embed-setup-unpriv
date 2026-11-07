//! Records the configuration that initialized the shared cluster singleton.
//!
//! The shared cluster is a process-wide singleton, so the first caller decides
//! which configuration it runs with. This module keeps that decision explicit:
//! ambient callers may reuse any cached result, whilst explicit callers may
//! only reuse a result produced by an equal explicit configuration.

use crate::{
    bootstrap::ConfigSource,
    error::{BootstrapError, BootstrapResult},
};

/// Records `requested` as the singleton origin, or rejects a conflicting call.
///
/// The first call wins. Later calls are accepted only when
/// [`ConfigSource::permits`] allows them to reuse the cached result.
pub(super) fn resolve_origin(
    origin: &mut Option<ConfigSource>,
    requested: &ConfigSource,
) -> BootstrapResult<()> {
    match origin {
        None => {
            *origin = Some(requested.clone());
            Ok(())
        }
        Some(existing) if existing.permits(requested) => Ok(()),
        Some(_) => Err(conflict_error()),
    }
}

/// Builds the error returned when an explicit caller conflicts with the origin.
///
/// The message deliberately omits configuration values so a conflict cannot
/// leak a password or other sensitive input into logs.
pub(super) fn conflict_error() -> BootstrapError {
    BootstrapError::from(color_eyre::eyre::eyre!(
        "shared cluster was already initialized with a different configuration; use a separate \
         process or pass the same TestBootstrapConfig"
    ))
}

#[cfg(test)]
mod tests {
    //! Tests for shared singleton origin and conflict rules.

    use super::*;
    use crate::{
        PgEnvCfg,
        TestBootstrapConfig,
        test_support::shared_singleton_core::{SharedInitState, get_or_try_init_shared},
    };

    fn explicit(port: u16) -> ConfigSource {
        ConfigSource::Explicit(Box::new(TestBootstrapConfig::new(PgEnvCfg {
            port: Some(port),
            ..PgEnvCfg::default()
        })))
    }

    /// Minimal state machine mirroring the singleton's origin and cache rules.
    struct Harness {
        state: SharedInitState<u32, &'static str>,
        origin: Option<ConfigSource>,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                state: SharedInitState::Uninitialized,
                origin: None,
            }
        }

        fn call(
            &mut self,
            requested: &ConfigSource,
            init: impl FnOnce() -> Result<u32, (&'static str, &'static str)>,
        ) -> Result<u32, &'static str> {
            resolve_origin(&mut self.origin, requested).map_err(|_| "conflict")?;
            get_or_try_init_shared(&mut self.state, init, |cached| *cached)
        }
    }

    #[test]
    fn equal_explicit_configuration_reuses_cached_success() {
        let mut harness = Harness::new();
        let first = harness
            .call(&explicit(54_321), || Ok(7))
            .expect("first initialization should succeed");
        let second = harness
            .call(&explicit(54_321), || panic!("must not reinitialize"))
            .expect("equal configuration should reuse the cached value");

        assert_eq!(first, second);
    }

    #[test]
    fn different_explicit_configuration_is_rejected() {
        let mut harness = Harness::new();
        harness
            .call(&explicit(54_321), || Ok(7))
            .expect("first initialization should succeed");

        let err = harness
            .call(&explicit(54_322), || panic!("must not initialize"))
            .expect_err("different configuration should conflict");

        assert_eq!(err, "conflict");
    }

    #[test]
    fn ambient_first_then_explicit_is_rejected() {
        let mut harness = Harness::new();
        harness
            .call(&ConfigSource::Ambient, || Ok(7))
            .expect("ambient initialization should succeed");

        let err = harness
            .call(&explicit(54_321), || panic!("must not initialize"))
            .expect_err("explicit call after ambient initialization should conflict");

        assert_eq!(err, "conflict");
    }

    #[test]
    fn explicit_first_then_ambient_reuses_cached_result() {
        let mut harness = Harness::new();
        harness
            .call(&explicit(54_321), || Ok(7))
            .expect("explicit initialization should succeed");

        let value = harness
            .call(&ConfigSource::Ambient, || panic!("must not reinitialize"))
            .expect("ambient call should reuse the cached value");

        assert_eq!(value, 7);
    }

    #[test]
    fn equal_explicit_configuration_reuses_cached_failure() {
        let mut harness = Harness::new();
        let first = harness
            .call(&explicit(54_321), || Err(("cached", "first failure")))
            .expect_err("first initialization should fail");
        let second = harness
            .call(&explicit(54_321), || panic!("must not reinitialize"))
            .expect_err("equal configuration should replay the cached failure");

        assert_eq!(first, "first failure");
        assert_eq!(second, "cached");
    }
}

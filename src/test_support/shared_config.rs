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

/// Rejects `requested` when it may not reuse the singleton's cached result.
///
/// The check runs before initialization so a conflicting call does not pay for
/// a bootstrap it will discard. Recording is a separate step because a
/// panicking initializer leaves no cached result for the origin to describe.
pub(super) fn ensure_compatible(
    origin: Option<&ConfigSource>,
    requested: &ConfigSource,
) -> BootstrapResult<()> {
    match origin {
        None => Ok(()),
        Some(existing) if existing.permits(requested) => Ok(()),
        Some(_) => Err(conflict_error()),
    }
}

/// Records `requested` as the configuration behind the singleton's result.
///
/// Call this only after initialization has produced a result, successful or
/// failed. A panic unwinds before this point, so a later call is not made to
/// conflict with a configuration that never initialized anything.
pub(super) fn record_origin(origin: &mut Option<ConfigSource>, requested: &ConfigSource) {
    origin.get_or_insert_with(|| requested.clone());
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

    use rstest::rstest;

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

    /// The source a simulated singleton call supplies.
    #[derive(Clone, Copy)]
    enum Attempt {
        Ambient,
        Explicit(u16),
    }

    impl Attempt {
        fn source(self) -> ConfigSource {
            match self {
                Self::Ambient => ConfigSource::Ambient,
                Self::Explicit(port) => explicit(port),
            }
        }
    }

    /// What the second call should observe.
    enum Expected {
        /// The cached value is reused.
        Reused(u32),
        /// The call is rejected because its configuration differs.
        Conflict,
        /// The first call's cached failure is replayed.
        CachedFailure,
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
            ensure_compatible(self.origin.as_ref(), requested).map_err(|_| "conflict")?;
            let result = get_or_try_init_shared(&mut self.state, init, |cached| *cached);
            if self.origin.is_none() {
                record_origin(&mut self.origin, requested);
            }
            result
        }
    }

    #[rstest]
    #[case::equal_explicit_reuses_success(
        Attempt::Explicit(54_321),
        false,
        Attempt::Explicit(54_321),
        Expected::Reused(7)
    )]
    #[case::different_explicit_conflicts(
        Attempt::Explicit(54_321),
        false,
        Attempt::Explicit(54_322),
        Expected::Conflict
    )]
    #[case::ambient_then_explicit_conflicts(
        Attempt::Ambient,
        false,
        Attempt::Explicit(54_321),
        Expected::Conflict
    )]
    #[case::explicit_then_ambient_reuses(
        Attempt::Explicit(54_321),
        false,
        Attempt::Ambient,
        Expected::Reused(7)
    )]
    #[case::equal_explicit_replays_failure(
        Attempt::Explicit(54_321),
        true,
        Attempt::Explicit(54_321),
        Expected::CachedFailure
    )]
    fn shared_origin_rules_are_enforced(
        #[case] prime: Attempt,
        #[case] prime_fails: bool,
        #[case] second: Attempt,
        #[case] expected: Expected,
    ) {
        let mut harness = Harness::new();
        let first = harness.call(&prime.source(), || initial(prime_fails));
        if prime_fails {
            assert_eq!(first.expect_err("prime should fail"), "first failure");
        } else {
            assert_eq!(first.expect("prime should succeed"), 7);
        }

        let result = harness.call(&second.source(), || panic!("must not reinitialize"));
        match expected {
            Expected::Reused(value) => {
                assert_eq!(result.expect("cached value should be reused"), value);
            }
            Expected::Conflict => assert_eq!(result.expect_err("call should conflict"), "conflict"),
            Expected::CachedFailure => {
                assert_eq!(result.expect_err("cached failure should replay"), "cached");
            }
        }
    }

    /// Result of the first call: a value, or a cacheable failure.
    fn initial(fails: bool) -> Result<u32, (&'static str, &'static str)> {
        if fails {
            Err(("cached", "first failure"))
        } else {
            Ok(7)
        }
    }

    /// A panicking initializer leaves no result, so no origin is recorded.
    ///
    /// Without this, the failed attempt would claim the singleton and a later
    /// explicit call would be rejected for conflicting with a configuration
    /// that never produced anything.
    #[test]
    fn panicking_initializer_does_not_record_origin() {
        let mut harness = Harness::new();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            harness.call(&ConfigSource::Ambient, || -> Result<u32, _> {
                panic!("initializer panicked")
            })
        }));
        assert!(panicked.is_err(), "the initializer panic should propagate");

        let value = harness
            .call(&explicit(54_321), || Ok(9))
            .expect("a later explicit call should take over the uninitialized singleton");
        assert_eq!(value, 9);
    }
}

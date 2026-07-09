//! Tests for template lifecycle coordination helpers.

use std::{
    any::Any,
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
};

use color_eyre::eyre::eyre;
use rstest::{fixture, rstest};

use super::*;
use crate::error::BootstrapError;

struct NoopLocks;

type SetupAndRollbackResult = Result<BootstrapResult<()>, Box<dyn Any + Send>>;

impl NoopLocks {
    const fn new() -> Self { Self }
}

impl TemplateLockOps for NoopLocks {
    fn with_template_lock<R>(&self, _template_name: &str, operation: impl FnOnce() -> R) -> R {
        operation()
    }
}

struct RollbackHarness {
    locks: NoopLocks,
    created: Cell<bool>,
    dropped: Cell<bool>,
}

impl RollbackHarness {
    fn new() -> Self {
        Self {
            locks: NoopLocks::new(),
            created: Cell::new(false),
            dropped: Cell::new(false),
        }
    }

    fn run_setup<Setup>(
        &self,
        setup_fn: Setup,
        rollback_should_fail: bool,
    ) -> SetupAndRollbackResult
    where
        Setup: FnOnce() -> BootstrapResult<()>,
    {
        catch_unwind(AssertUnwindSafe(|| {
            ensure_template_exists_with_lock(
                &self.locks,
                "template",
                TemplateCreationOps {
                    database_exists: || Ok(self.created.get()),
                    create_database: || {
                        self.created.set(true);
                        Ok(())
                    },
                    drop_database: || {
                        self.dropped.set(true);
                        self.rollback_result(rollback_should_fail)
                    },
                    setup_fn,
                },
            )
        }))
    }

    fn created(&self) -> bool { self.created.get() }

    fn dropped(&self) -> bool { self.dropped.get() }

    fn rollback_result(&self, rollback_should_fail: bool) -> BootstrapResult<()> {
        if rollback_should_fail {
            return Err(bootstrap_error("rollback failed"));
        }
        self.created.set(false);
        Ok(())
    }
}

#[fixture]
#[rustfmt::skip]
fn locks() -> NoopLocks {
    NoopLocks::new()
}

fn bootstrap_error(message: &str) -> BootstrapError { eyre!("{message}").into() }

fn assert_combined_error(error: &BootstrapError, expected_primary: &str, expected_rollback: &str) {
    let display = error.to_string();
    assert!(
        display.contains(expected_primary),
        "combined error should preserve setup failure, got: {display}"
    );
    assert!(
        display.contains(expected_rollback),
        "combined error should include rollback failure, got: {display}"
    );
}

#[rstest]
#[case::setup_error_rollback_succeeds(false, false)]
#[case::setup_error_rollback_fails(true, false)]
#[case::setup_panic_rollback_succeeds(false, true)]
#[case::setup_panic_rollback_fails(true, true)]
fn rollback_scenarios(#[case] rollback_should_fail: bool, #[case] setup_panics: bool) {
    let harness = RollbackHarness::new();
    let setup_count = Cell::new(0);
    let outer_result = harness.run_setup(
        || {
            setup_count.set(setup_count.get() + 1);
            assert!(!setup_panics, "setup panic");
            Err(bootstrap_error("setup failed"))
        },
        rollback_should_fail,
    );

    assert_eq!(setup_count.get(), 1);
    assert!(harness.dropped(), "failed setup should invoke rollback");
    match (setup_panics, rollback_should_fail) {
        (false, false) => {
            let inner_result = outer_result.expect("setup failure should not panic");
            let error = inner_result.expect_err("setup failure should be returned");
            assert!(
                error.to_string().contains("setup failed"),
                "setup error should be preserved, got: {error}"
            );
            assert!(
                !harness.created(),
                "failed setup should remove the template"
            );
        }
        (false, true) => {
            let inner_result = outer_result.expect("rollback failure should not resume a panic");
            let error = inner_result.expect_err("combined failure should be returned");
            assert_combined_error(&error, "setup failed", "rollback failed");
        }
        (true, false) => {
            let _panic = outer_result.expect_err("setup panic should be resumed");
            assert!(!harness.created(), "panic path should remove the template");
        }
        (true, true) => {
            let inner_result = outer_result.expect("rollback failure should not resume a panic");
            let error = inner_result.expect_err("combined failure should be returned");
            assert_combined_error(&error, "setup panic", "rollback failed");
        }
    }
}

#[rstest]
fn create_panic_does_not_roll_back_uncreated_template(locks: NoopLocks) {
    let dropped = Cell::new(false);

    let result = catch_unwind(AssertUnwindSafe(|| match ensure_template_exists_with_lock(
        &locks,
        "template",
        TemplateCreationOps {
            database_exists: || Ok(false),
            create_database: || panic!("create panic"),
            drop_database: || {
                dropped.set(true);
                Ok(())
            },
            setup_fn: || Ok(()),
        },
    ) {
        Ok(()) | Err(_) => {}
    }));

    assert!(result.is_err(), "create panic should be resumed");
    assert!(
        !dropped.get(),
        "panic before successful creation should not invoke rollback"
    );
}

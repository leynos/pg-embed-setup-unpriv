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

type SetupAndRollbackResult = (
    Cell<bool>,
    Cell<bool>,
    Result<BootstrapResult<()>, Box<dyn Any + Send>>,
);

impl NoopLocks {
    const fn new() -> Self { Self }
}

impl TemplateLockOps for NoopLocks {
    fn with_template_lock<R>(&self, _template_name: &str, operation: impl FnOnce() -> R) -> R {
        operation()
    }
}

#[fixture]
fn locks() -> NoopLocks {
    let locks = NoopLocks::new();
    std::convert::identity(locks)
}

fn bootstrap_error(message: &str) -> BootstrapError { eyre!("{message}").into() }

fn setup_and_rollback_result<Setup>(
    setup_fn: Setup,
    rollback_should_fail: bool,
) -> SetupAndRollbackResult
where
    Setup: FnOnce() -> BootstrapResult<()>,
{
    let locks = NoopLocks::new();
    let created = Cell::new(false);
    let dropped = Cell::new(false);

    let result = catch_unwind(AssertUnwindSafe(|| {
        ensure_template_exists_with_lock(
            &locks,
            "template",
            TemplateCreationOps {
                database_exists: || Ok(created.get()),
                create_database: || {
                    created.set(true);
                    Ok(())
                },
                drop_database: || {
                    dropped.set(true);
                    if rollback_should_fail {
                        Err(bootstrap_error("rollback failed"))
                    } else {
                        created.set(false);
                        Ok(())
                    }
                },
                setup_fn,
            },
        )
    }));

    (created, dropped, result)
}

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

#[test]
fn setup_failure_rolls_back_created_template() {
    let setup_count = Cell::new(0);
    let (created, dropped, outer_result) = setup_and_rollback_result(
        || {
            setup_count.set(setup_count.get() + 1);
            Err(bootstrap_error("setup failed"))
        },
        false,
    );
    let inner_result = outer_result.expect("setup failure should not panic");
    let error = inner_result.expect_err("setup failure should be returned");

    assert_eq!(setup_count.get(), 1);
    assert!(
        error.to_string().contains("setup failed"),
        "setup error should be preserved, got: {error}"
    );
    assert!(!created.get(), "failed setup should remove the template");
    assert!(dropped.get(), "failed setup should invoke rollback");
}

#[test]
fn rollback_failure_preserves_setup_error_context() {
    let setup_count = Cell::new(0);
    let (_created, dropped, outer_result) = setup_and_rollback_result(
        || {
            setup_count.set(setup_count.get() + 1);
            Err(bootstrap_error("setup failed"))
        },
        true,
    );
    let inner_result = outer_result.expect("setup failure should not panic");
    let error = inner_result.expect_err("combined setup and rollback failure should be returned");

    assert_eq!(setup_count.get(), 1);
    assert!(dropped.get(), "failed setup should invoke rollback");
    assert_combined_error(&error, "setup failed", "rollback failed");
}

#[test]
fn setup_panic_rolls_back_created_template() {
    let setup_count = Cell::new(0);
    let (created, dropped, outer_result) = setup_and_rollback_result(
        || {
            setup_count.set(setup_count.get() + 1);
            panic!("setup panic")
        },
        false,
    );
    let _panic = outer_result.expect_err("setup panic should be resumed");

    assert_eq!(setup_count.get(), 1);
    assert!(!created.get(), "panic path should remove the template");
    assert!(dropped.get(), "panic path should invoke rollback");
}

#[test]
fn setup_panic_reports_rollback_failure() {
    let setup_count = Cell::new(0);
    let (_created, dropped, outer_result) = setup_and_rollback_result(
        || {
            setup_count.set(setup_count.get() + 1);
            panic!("setup panic")
        },
        true,
    );
    let inner_result =
        outer_result.expect("rollback failure during setup panic should not resume panic");
    let error =
        inner_result.expect_err("rollback failure during setup panic should return an error");

    assert_eq!(setup_count.get(), 1);
    assert!(dropped.get(), "panic path should invoke rollback");
    assert_combined_error(&error, "setup panic", "rollback failed");
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

//! Tests for template lifecycle coordination helpers.

use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
};

use color_eyre::eyre::eyre;

use super::*;
use crate::error::BootstrapError;

struct NoopLocks;

impl TemplateLockOps for NoopLocks {
    fn with_template_lock<R>(&self, _template_name: &str, operation: impl FnOnce() -> R) -> R {
        operation()
    }
}

fn bootstrap_error(message: &str) -> BootstrapError { eyre!("{message}").into() }

#[test]
fn setup_failure_rolls_back_created_template() {
    let locks = NoopLocks;
    let created = Cell::new(false);
    let dropped = Cell::new(false);
    let setup_count = Cell::new(0);

    let result = ensure_template_exists_with_lock(
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
                created.set(false);
                Ok(())
            },
            setup_fn: || {
                setup_count.set(setup_count.get() + 1);
                Err(bootstrap_error("setup failed"))
            },
        },
    );

    assert!(result.is_err(), "setup failure should be returned");
    assert!(!created.get(), "failed setup should remove the template");
    assert!(dropped.get(), "failed setup should invoke rollback");
    assert_eq!(setup_count.get(), 1);
}

#[test]
fn rollback_failure_preserves_setup_error_context() {
    let locks = NoopLocks;

    let result = ensure_template_exists_with_lock(
        &locks,
        "template",
        TemplateCreationOps {
            database_exists: || Ok(false),
            create_database: || Ok(()),
            drop_database: || Err(bootstrap_error("rollback failed")),
            setup_fn: || Err(bootstrap_error("setup failed")),
        },
    );

    let Err(error) = result else {
        panic!("combined setup and rollback failure should be returned");
    };
    let display = error.to_string();

    assert!(
        display.contains("setup failed"),
        "combined error should preserve setup failure, got: {display}"
    );
    assert!(
        display.contains("rollback failed"),
        "combined error should include rollback failure, got: {display}"
    );
}

#[test]
fn setup_panic_rolls_back_created_template() {
    let locks = NoopLocks;
    let created = Cell::new(false);
    let dropped = Cell::new(false);

    let result = catch_unwind(AssertUnwindSafe(|| match ensure_template_exists_with_lock(
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
                created.set(false);
                Ok(())
            },
            setup_fn: || panic!("setup panic"),
        },
    ) {
        Ok(()) | Err(_) => {}
    }));

    assert!(result.is_err(), "setup panic should be resumed");
    assert!(!created.get(), "panic path should remove the template");
    assert!(dropped.get(), "panic path should invoke rollback");
}

#[test]
fn create_panic_does_not_roll_back_uncreated_template() {
    let locks = NoopLocks;
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

#[test]
fn setup_panic_reports_rollback_failure() {
    let locks = NoopLocks;

    let result = ensure_template_exists_with_lock(
        &locks,
        "template",
        TemplateCreationOps {
            database_exists: || Ok(false),
            create_database: || Ok(()),
            drop_database: || Err(bootstrap_error("rollback failed")),
            setup_fn: || panic!("setup panic"),
        },
    );

    let Err(error) = result else {
        panic!("rollback failure during setup panic should return an error");
    };
    let display = error.to_string();

    assert!(
        display.contains("setup panic"),
        "error should preserve setup panic, got: {display}"
    );
    assert!(
        display.contains("rollback failed"),
        "error should include rollback failure, got: {display}"
    );
}

//! Tests for template lifecycle coordination helpers.

use std::{
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
};

use color_eyre::eyre::eyre;
use rstest::{fixture, rstest};

use super::*;
use crate::error::BootstrapError;

struct NoopLocks;

impl NoopLocks {
    const fn new() -> Self { Self }
}

impl TemplateLockOps for NoopLocks {
    fn with_template_lock<R>(&self, _template_name: &str, operation: impl FnOnce() -> R) -> R {
        operation()
    }
}

struct TemplateState {
    created: Cell<bool>,
    dropped: Cell<bool>,
    setup_count: Cell<u8>,
}

#[derive(Clone, Copy)]
enum SetupAction {
    Error,
    Panic,
}

#[derive(Clone, Copy)]
enum RollbackAction {
    Succeed,
    Fail,
}

#[derive(Clone, Copy)]
enum ExpectedOutcome {
    ErrorRollbackSuccess,
    ErrorRollbackFailure,
    PanicRollbackSuccess,
    PanicRollbackFailure,
}

#[derive(Clone, Copy)]
struct Scenario {
    setup_action: SetupAction,
    rollback_action: RollbackAction,
    expected: ExpectedOutcome,
}

impl Scenario {
    const fn new(
        setup_action: SetupAction,
        rollback_action: RollbackAction,
        expected: ExpectedOutcome,
    ) -> Self {
        Self {
            setup_action,
            rollback_action,
            expected,
        }
    }
}

#[fixture]
fn locks() -> NoopLocks {
    let locks = NoopLocks::new();
    std::convert::identity(locks)
}

#[fixture]
fn template_state() -> TemplateState {
    TemplateState {
        created: Cell::new(false),
        dropped: Cell::new(false),
        setup_count: Cell::new(0),
    }
}

fn bootstrap_error(message: &str) -> BootstrapError { eyre!("{message}").into() }

#[rstest]
#[case::setup_error_rollback_success(Scenario::new(
    SetupAction::Error,
    RollbackAction::Succeed,
    ExpectedOutcome::ErrorRollbackSuccess
))]
#[case::setup_error_rollback_failure(Scenario::new(
    SetupAction::Error,
    RollbackAction::Fail,
    ExpectedOutcome::ErrorRollbackFailure
))]
#[case::setup_panic_rollback_success(Scenario::new(
    SetupAction::Panic,
    RollbackAction::Succeed,
    ExpectedOutcome::PanicRollbackSuccess
))]
#[case::setup_panic_rollback_failure(Scenario::new(
    SetupAction::Panic,
    RollbackAction::Fail,
    ExpectedOutcome::PanicRollbackFailure
))]
fn setup_failure_paths_roll_back_created_template(
    locks: NoopLocks,
    template_state: TemplateState,
    #[case] scenario: Scenario,
) {
    let created = &template_state.created;
    let dropped = &template_state.dropped;
    let setup_count = &template_state.setup_count;

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
                    match scenario.rollback_action {
                        RollbackAction::Succeed => {
                            created.set(false);
                            Ok(())
                        }
                        RollbackAction::Fail => Err(bootstrap_error("rollback failed")),
                    }
                },
                setup_fn: || {
                    setup_count.set(setup_count.get() + 1);
                    match scenario.setup_action {
                        SetupAction::Error => Err(bootstrap_error("setup failed")),
                        SetupAction::Panic => panic!("setup panic"),
                    }
                },
            },
        )
    }));

    assert_eq!(setup_count.get(), 1);
    assert!(dropped.get(), "failed setup should invoke rollback");

    match scenario.expected {
        ExpectedOutcome::ErrorRollbackSuccess => {
            let Ok(Err(error)) = result else {
                panic!("setup failure should be returned");
            };
            assert!(
                error.to_string().contains("setup failed"),
                "setup error should be preserved, got: {error}"
            );
            assert!(!created.get(), "failed setup should remove the template");
        }
        ExpectedOutcome::ErrorRollbackFailure => {
            let Ok(Err(error)) = result else {
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
        ExpectedOutcome::PanicRollbackSuccess => {
            assert!(result.is_err(), "setup panic should be resumed");
            assert!(!created.get(), "panic path should remove the template");
        }
        ExpectedOutcome::PanicRollbackFailure => {
            let Ok(Err(error)) = result else {
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

//! Template database lock coordination for lifecycle operations.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, Mutex, OnceLock};

use color_eyre::eyre::eyre;
use dashmap::DashMap;

use crate::error::{BootstrapError, BootstrapResult};

/// Lock provider used by template creation coordination.
pub(super) trait TemplateLockOps {
    /// Runs `operation` while holding the lock for `template_name`.
    fn with_template_lock<R>(&self, template_name: &str, operation: impl FnOnce() -> R) -> R;
}

/// Production template lock provider.
pub(super) struct StdTemplateLocks;

/// Global per-template locks to prevent concurrent template creation.
///
/// Uses a `DashMap` to allow lock-free reads and concurrent access to
/// different templates while serializing access to the same template.
static TEMPLATE_LOCKS: OnceLock<DashMap<String, Arc<Mutex<()>>>> = OnceLock::new();

impl TemplateLockOps for StdTemplateLocks {
    fn with_template_lock<R>(&self, template_name: &str, operation: impl FnOnce() -> R) -> R {
        let locks = TEMPLATE_LOCKS.get_or_init(DashMap::new);
        let lock = locks
            .entry(template_name.to_owned())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        let _guard = lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation()
    }
}

/// Global production template lock provider.
pub(super) static STD_TEMPLATE_LOCKS: StdTemplateLocks = StdTemplateLocks;

/// Operations needed to create a missing template database.
pub(super) struct TemplateCreationOps<Exists, Create, Drop, Setup> {
    /// Checks whether the template already exists.
    pub(super) database_exists: Exists,
    /// Creates the template database.
    pub(super) create_database: Create,
    /// Drops a created template after setup fails.
    pub(super) drop_database: Drop,
    /// Performs caller-provided template setup.
    pub(super) setup_fn: Setup,
}

/// Ensures a template exists while holding the provider's per-template lock.
pub(super) fn ensure_template_exists_with_lock<L, Exists, Create, Drop, Setup>(
    locks: &L,
    template_name: &str,
    ops: TemplateCreationOps<Exists, Create, Drop, Setup>,
) -> BootstrapResult<()>
where
    L: TemplateLockOps,
    Exists: FnMut() -> BootstrapResult<bool>,
    Create: FnMut() -> BootstrapResult<()>,
    Drop: FnMut() -> BootstrapResult<()>,
    Setup: FnOnce() -> BootstrapResult<()>,
{
    let TemplateCreationOps {
        mut database_exists,
        mut create_database,
        mut drop_database,
        setup_fn,
    } = ops;
    locks.with_template_lock(template_name, || {
        if database_exists()? {
            return Ok(());
        }

        create_database()?;
        run_setup_with_recovery(setup_fn, &mut drop_database)
    })
}

fn run_setup_with_recovery<Drop, Setup>(
    setup_fn: Setup,
    drop_database: &mut Drop,
) -> BootstrapResult<()>
where
    Drop: FnMut() -> BootstrapResult<()>,
    Setup: FnOnce() -> BootstrapResult<()>,
{
    match catch_unwind(AssertUnwindSafe(setup_fn)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(setup_error)) => rollback_after_setup_error(setup_error, drop_database),
        Err(payload) => handle_setup_panic(payload, drop_database),
    }
}

fn rollback_after_setup_error<Drop>(
    setup_error: BootstrapError,
    drop_database: &mut Drop,
) -> BootstrapResult<()>
where
    Drop: FnMut() -> BootstrapResult<()>,
{
    if let Err(rollback_error) = drop_database() {
        return Err(template_setup_rollback_error(setup_error, rollback_error));
    }
    Err(setup_error)
}

fn handle_setup_panic<Drop>(
    payload: Box<dyn Any + Send>,
    drop_database: &mut Drop,
) -> BootstrapResult<()>
where
    Drop: FnMut() -> BootstrapResult<()>,
{
    if let Err(rollback_error) = drop_database() {
        return Err(template_setup_panic_rollback_error(
            payload.as_ref(),
            rollback_error,
        ));
    }
    resume_unwind(payload);
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    payload.downcast_ref::<&str>().map_or_else(
        || {
            payload
                .downcast_ref::<String>()
                .map_or_else(|| "non-string panic payload".to_owned(), Clone::clone)
        },
        |message| (*message).to_owned(),
    )
}

fn template_setup_panic_rollback_error(
    payload: &(dyn Any + Send),
    rollback_error: BootstrapError,
) -> BootstrapError {
    let rollback_message = rollback_error.into_report().to_string();
    eyre!(
        "template setup panicked ({}) and rollback failed: {rollback_message}",
        panic_payload_message(payload)
    )
    .into()
}

fn template_setup_rollback_error(
    setup_error: BootstrapError,
    rollback_error: BootstrapError,
) -> BootstrapError {
    let setup_message = setup_error.to_string();
    let rollback_message = rollback_error.into_report().to_string();
    setup_error
        .into_report()
        .wrap_err(format!(
            "template rollback failed after setup error '{setup_message}': {rollback_message}"
        ))
        .into()
}

#[cfg(test)]
mod tests {
    //! Tests for template lifecycle coordination helpers.

    use std::cell::Cell;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use color_eyre::eyre::eyre;

    use super::*;
    use crate::error::BootstrapError;

    struct NoopLocks;

    impl TemplateLockOps for NoopLocks {
        fn with_template_lock<R>(&self, _template_name: &str, operation: impl FnOnce() -> R) -> R {
            operation()
        }
    }

    fn bootstrap_error(message: &str) -> BootstrapError {
        eyre!("{message}").into()
    }

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
}

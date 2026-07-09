//! Template database lock coordination for lifecycle operations.

use std::{
    any::Any,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{Arc, Mutex, OnceLock},
};

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

        create_and_setup_template(
            template_name,
            &mut create_database,
            &mut drop_database,
            setup_fn,
        )
    })
}

fn create_and_setup_template<Create, Drop, Setup>(
    template_name: &str,
    create_database: &mut Create,
    drop_database: &mut Drop,
    setup_fn: Setup,
) -> BootstrapResult<()>
where
    Create: FnMut() -> BootstrapResult<()>,
    Drop: FnMut() -> BootstrapResult<()>,
    Setup: FnOnce() -> BootstrapResult<()>,
{
    create_database()?;
    setup_template_or_rollback(template_name, setup_fn, drop_database)
}

fn setup_template_or_rollback<Drop, Setup>(
    template_name: &str,
    setup_fn: Setup,
    drop_database: &mut Drop,
) -> BootstrapResult<()>
where
    Drop: FnMut() -> BootstrapResult<()>,
    Setup: FnOnce() -> BootstrapResult<()>,
{
    match catch_unwind(AssertUnwindSafe(setup_fn)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(setup_error)) => {
            rollback_after_setup_error(template_name, setup_error, drop_database)
        }
        Err(payload) => resume_or_report_rollback_failure(template_name, payload, drop_database),
    }
}

fn rollback_after_setup_error<Drop>(
    template_name: &str,
    setup_error: BootstrapError,
    drop_database: &mut Drop,
) -> BootstrapResult<()>
where
    Drop: FnMut() -> BootstrapResult<()>,
{
    match rollback_and_log(
        template_name,
        drop_database,
        "template setup failed and rollback completed",
        None,
        None,
    ) {
        Ok(()) => Err(setup_error),
        Err(rollback_error) => Err(template_setup_rollback_error(setup_error, rollback_error)),
    }
}

fn resume_or_report_rollback_failure<Drop>(
    template_name: &str,
    payload: Box<dyn Any + Send>,
    drop_database: &mut Drop,
) -> BootstrapResult<()>
where
    Drop: FnMut() -> BootstrapResult<()>,
{
    match rollback_and_log(
        template_name,
        drop_database,
        "template setup panicked and rollback completed",
        Some("resume_unwind"),
        Some("converted_to_error"),
    ) {
        Ok(()) => resume_unwind(payload),
        Err(rollback_error) => Err(template_setup_panic_rollback_error(
            payload.as_ref(),
            rollback_error,
        )),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "rollback log metadata is intentionally explicit at the call site"
)]
fn rollback_and_log<Drop>(
    template_name: &str,
    drop_database: &mut Drop,
    message: &'static str,
    success_panic_action: Option<&'static str>,
    failure_panic_action: Option<&'static str>,
) -> Result<(), BootstrapError>
where
    Drop: FnMut() -> BootstrapResult<()>,
{
    match drop_database() {
        Ok(()) => {
            log_template_rollback(template_name, true, success_panic_action, message);
            Ok(())
        }
        Err(rollback_error) => {
            log_template_rollback(template_name, false, failure_panic_action, message);
            Err(rollback_error)
        }
    }
}

fn log_template_rollback(
    template_name: &str,
    rollback_succeeded: bool,
    panic_action: Option<&'static str>,
    message: &'static str,
) {
    tracing::warn!(
        target: crate::observability::LOG_TARGET,
        template_name,
        rollback_succeeded,
        panic_action,
        "{message}"
    );
}

fn panic_payload_message(payload: &(dyn Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "non-string panic payload".to_owned()
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
mod tests;

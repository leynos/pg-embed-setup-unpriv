//! Unit tests for the [`WorkerInvoker`] component, verifying both in-process
//! execution for unprivileged operations and hook delegation for root operations.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use color_eyre::eyre::{Result, ensure, eyre};
use serial_test::serial;

use super::*;
use crate::{
    ExecutionPrivileges,
    test_support::{
        RunRootOperationHookInstallError,
        drain_hook_install_logs,
        dummy_settings,
        install_run_root_operation_hook,
        test_runtime,
    },
};

#[test]
fn unprivileged_operations_execute_in_process() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let calls = AtomicUsize::new(0);

    invoker
        .invoke(WorkerOperation::Setup, async {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok::<(), postgresql_embedded::Error>(())
        })
        .map_err(|err| eyre!(err))?;

    ensure!(
        calls.load(Ordering::Relaxed) == 1,
        "expected in-process call"
    );
    Ok(())
}

#[test]
fn unprivileged_operations_execute_inside_existing_runtime() -> Result<()> {
    let runtime = test_runtime()?;
    let nested_runtime = test_runtime()?;
    runtime.block_on(async {
        let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
        let env_vars = bootstrap.environment.to_env();
        let invoker = WorkerInvoker::new(&nested_runtime, &bootstrap, &env_vars);

        invoker
            .invoke(WorkerOperation::Setup, async {
                Ok::<(), postgresql_embedded::Error>(())
            })
            .map_err(|err| eyre!(err))
    })?;

    Ok(())
}

#[test]
#[serial(worker_hook)]
fn root_operations_delegate_to_hook() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Root);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);
    let worker_calls = Arc::new(AtomicUsize::new(0));

    let hook_calls = Arc::clone(&worker_calls);
    let _guard = install_run_root_operation_hook(move |_, _, _| {
        hook_calls.fetch_add(1, Ordering::Relaxed);
        Ok(())
    })
    .map_err(|err| eyre!(err))?;

    invoker
        .invoke(WorkerOperation::Setup, async {
            Ok::<(), postgresql_embedded::Error>(())
        })
        .map_err(|err| eyre!(err))?;

    ensure!(
        worker_calls.load(Ordering::Relaxed) == 1,
        "expected worker hook to run"
    );
    Ok(())
}

#[test]
fn timeout_error_mentions_elapsed_duration() {
    let err = timeout_error("bootstrap", std::time::Duration::from_secs(3));
    assert!(
        err.to_string().contains("timed out after 3.0s"),
        "expected the exact formatted duration, got: {err}"
    );
}

#[test]
fn unprivileged_operation_propagates_inner_error() -> Result<()> {
    let runtime = test_runtime()?;
    let bootstrap = dummy_settings(ExecutionPrivileges::Unprivileged);
    let env_vars = bootstrap.environment.to_env();
    let invoker = WorkerInvoker::new(&runtime, &bootstrap, &env_vars);

    let result = invoker.invoke(WorkerOperation::Setup, async {
        Err::<(), postgresql_embedded::Error>(postgresql_embedded::Error::DatabaseStopError(
            "boom".to_owned(),
        ))
    });

    // The operation context is the top-level message; the inner "boom" survives
    // only in the error's source chain, so inspect the chain rather than
    // `to_string()`.
    let err = result
        .err()
        .ok_or_else(|| eyre!("in-process errors should propagate"))?;
    ensure!(
        err.into_report()
            .chain()
            .any(|source| source.to_string().contains("boom")),
        "expected the inner 'boom' error to survive in the propagated chain"
    );
    Ok(())
}

#[test]
#[serial(worker_hook)]
fn installing_hook_twice_errors() -> Result<()> {
    let _guard = install_run_root_operation_hook(|_, _, _| Ok(())).map_err(|err| eyre!(err))?;

    let Err(err) = install_run_root_operation_hook(|_, _, _| Ok(())) else {
        return Err(eyre!("expected second installation to fail"));
    };
    ensure!(
        err == RunRootOperationHookInstallError::AlreadyInstalled,
        "unexpected install error: {err:?}"
    );

    drop(drain_hook_install_logs());
    Ok(())
}

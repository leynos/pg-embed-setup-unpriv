//! Cleanup helpers for `TestCluster` shutdown.

use std::{error::Error, path::Path};

use postgresql_embedded::Settings;

use super::{worker_invoker::WorkerInvoker as ClusterWorkerInvoker, worker_operation};
use crate::{
    CleanupMode,
    TestBootstrapSettings,
    bootstrap::{
        ClaimGuard,
        claim_guard_at,
        claim_guard_for,
        derived_slot_parent,
        has_live_peers,
        has_live_slots_in,
    },
    cleanup_helpers::{RemovalOutcome, has_parent_dir, try_remove_dir_all},
    observability::LOG_TARGET,
};

#[derive(Debug, Clone, Copy)]
enum DirectoryLabel {
    Data,
    Installation,
    InstallationRoot,
}

impl DirectoryLabel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Installation => "installation",
            Self::InstallationRoot => "installation-root",
        }
    }
}

/// Invokes worker-managed cleanup for a dropped cluster.
///
/// # Examples
/// ```rust,ignore
/// # use pg_embedded_setup_unpriv::test_support::fixtures::test_cluster;
/// # let _ = test_cluster();
/// ```
pub(super) fn cleanup_worker_managed_with_runtime(
    runtime: &tokio::runtime::Runtime,
    bootstrap: &TestBootstrapSettings,
    env_vars: &[(String, Option<String>)],
    context: &str,
) {
    release_orphan_watcher(&bootstrap.settings);
    let plan = plan_cleanup(bootstrap.cleanup_mode, &bootstrap.settings, context);
    let Some(operation) = cleanup_operation(plan.mode) else {
        return;
    };
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        operation = operation.as_str(),
        "cleaning up postgres directories via worker"
    );
    let invoker = ClusterWorkerInvoker::new(runtime, bootstrap, env_vars);
    if let Err(err) = invoker.invoke_as_root(operation) {
        warn_cleanup_failure(context, operation, &err);
    }
}

/// Removes in-process directories after a successful stop.
///
/// # Examples
/// ```rust,ignore
/// # use pg_embedded_setup_unpriv::{CleanupMode, TestCluster};
/// # let cluster = TestCluster::new()?;
/// # let settings = cluster.settings().clone();
/// pg_embedded_setup_unpriv::cluster::cleanup::cleanup_in_process(
///     CleanupMode::DataOnly,
///     &settings,
///     "example",
/// );
/// # drop(cluster);
/// # Ok::<(), pg_embedded_setup_unpriv::error::BootstrapError>(())
/// ```
pub(super) fn cleanup_in_process(requested: CleanupMode, settings: &Settings, context: &str) {
    release_orphan_watcher(settings);
    let plan = plan_cleanup(requested, settings, context);
    let mode = plan.mode;
    if mode == CleanupMode::None {
        return;
    }
    log_cleanup_start(mode, plan.holds_guard(), context);
    cleanup_data_dir(mode, settings, context);
    cleanup_install_dir(mode, settings, context);
}

/// Ends the watcher that guarded this cluster: it has been stopped normally,
/// so nothing is left for a watcher to stop.
fn release_orphan_watcher(settings: &Settings) {
    #[cfg(target_os = "linux")]
    let _released = crate::bootstrap::release_watcher(&settings.data_dir);
    #[cfg(not(target_os = "linux"))]
    let _ = settings;
}

fn log_cleanup_start(cleanup_mode: CleanupMode, is_guarded: bool, context: &str) {
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        cleanup_mode = ?cleanup_mode,
        is_guarded,
        "cleaning up postgres directories"
    );
}

fn cleanup_data_dir(cleanup_mode: CleanupMode, settings: &Settings, context: &str) {
    if should_remove_data(cleanup_mode) {
        remove_dir_all_if_exists(&settings.data_dir, DirectoryLabel::Data, context);
    }
}

fn cleanup_install_dir(cleanup_mode: CleanupMode, settings: &Settings, context: &str) {
    if !should_remove_install(cleanup_mode) {
        return;
    }
    remove_dir_all_if_exists(
        &settings.installation_dir,
        DirectoryLabel::Installation,
        context,
    );
    let Some(parent) = settings.password_file.parent() else {
        return;
    };
    if should_remove_install_root(parent, settings) {
        remove_dir_all_if_exists(parent, DirectoryLabel::InstallationRoot, context);
    }
}

/// What a cleanup will do, and the lock that keeps it true until it is done.
pub(super) struct CleanupPlan {
    /// The mode to clean up with.
    pub(super) mode: CleanupMode,
    /// Held across the cleanup when it may remove a shared install tree.
    guard: Option<ClaimGuard>,
}

impl CleanupPlan {
    /// Returns whether the plan holds the claim guard.
    const fn holds_guard(&self) -> bool { self.guard.is_some() }
}

/// Plans a cleanup that never removes an install tree other clusters are
/// running from.
///
/// Per-cluster data directories (ADR 005) let clusters run side by side from
/// one install tree, so a [`CleanupMode::Full`] cleanup of one would pull the
/// binaries and extensions from under the others. It then removes only its own
/// data directory. The plan holds the root's claim guard across the probe for
/// live peers and the removal, so a cluster cannot claim a slot in between; a
/// guard that cannot be taken counts as a peer. Deciding here, before either
/// the in-process or the worker path chooses what to delete, keeps the two
/// consistent.
pub(super) fn plan_cleanup(
    requested: CleanupMode,
    settings: &Settings,
    context: &str,
) -> CleanupPlan {
    if requested != CleanupMode::Full {
        return CleanupPlan {
            mode: requested,
            guard: None,
        };
    }
    match claim_guard_for(&settings.data_dir) {
        None => plan_for_unslotted(settings, context),
        Some(Ok(guard)) if !has_live_peers(&settings.data_dir) => CleanupPlan {
            mode: requested,
            guard: Some(guard),
        },
        Some(_) => {
            log_install_kept(settings, context);
            CleanupPlan {
                mode: CleanupMode::DataOnly,
                guard: None,
            }
        }
    }
}

/// Plans a full cleanup of a data directory that is not a slot, such as an
/// explicit `PG_DATA_DIR`.
///
/// Its install tree may still be the derived `<root>/install` that slotted
/// clusters run from, so the slot parent beside it is checked and guarded the
/// same way. With no such parent nothing shares the tree.
fn plan_for_unslotted(settings: &Settings, context: &str) -> CleanupPlan {
    let full = |guard| CleanupPlan {
        mode: CleanupMode::Full,
        guard,
    };
    let Some(parent) = derived_slot_parent(&settings.installation_dir)
        .filter(|parent| parent != &settings.data_dir)
    else {
        return full(None);
    };
    match claim_guard_at(&parent) {
        Ok(guard) if !has_live_slots_in(&parent) => full(Some(guard)),
        _ => {
            log_install_kept(settings, context);
            CleanupPlan {
                mode: CleanupMode::DataOnly,
                guard: None,
            }
        }
    }
}

/// Logs that a full cleanup left the shared install tree in place.
fn log_install_kept(settings: &Settings, context: &str) {
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        path = %settings.installation_dir.display(),
        "kept the installation directory: other clusters may be running from it"
    );
}

const fn should_remove_data(cleanup_mode: CleanupMode) -> bool {
    matches!(cleanup_mode, CleanupMode::DataOnly | CleanupMode::Full)
}

const fn should_remove_install(cleanup_mode: CleanupMode) -> bool {
    matches!(cleanup_mode, CleanupMode::Full)
}

const fn cleanup_operation(cleanup_mode: CleanupMode) -> Option<worker_operation::WorkerOperation> {
    match cleanup_mode {
        CleanupMode::DataOnly => Some(worker_operation::WorkerOperation::Cleanup),
        CleanupMode::Full => Some(worker_operation::WorkerOperation::CleanupFull),
        CleanupMode::None => None,
    }
}

fn should_remove_install_root(parent: &Path, settings: &Settings) -> bool {
    parent != settings.installation_dir.as_path()
        && !has_parent_dir(parent)
        && parent.starts_with(&settings.installation_dir)
}

fn is_dangerous_cleanup_path(path: &Path) -> bool {
    path.as_os_str().is_empty() || (path.is_absolute() && path.parent().is_none())
}

fn remove_dir_all_if_exists(path: &Path, label: DirectoryLabel, context: &str) {
    if is_dangerous_cleanup_path(path) {
        let err = std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "refusing to remove root or empty path",
        );
        warn_cleanup_removal_failure(context, label, path, &err);
        return;
    }
    match try_remove_dir_all(path) {
        Ok(outcome) => log_removal_outcome(outcome, path, label, context),
        Err(err) => warn_cleanup_removal_failure(context, label, path, &err),
    }
}

fn log_removal_outcome(outcome: RemovalOutcome, path: &Path, label: DirectoryLabel, context: &str) {
    match outcome {
        RemovalOutcome::Removed => log_dir_removed(path, label, context),
        RemovalOutcome::Missing => log_dir_missing(path, label, context),
    }
}

fn log_dir_removed(path: &Path, label: DirectoryLabel, context: &str) {
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        path = %path.display(),
        label = label.as_str(),
        "removed postgres directory"
    );
}

fn log_dir_missing(path: &Path, label: DirectoryLabel, context: &str) {
    tracing::debug!(
        target: LOG_TARGET,
        context = %context,
        path = %path.display(),
        label = label.as_str(),
        "postgres directory already removed"
    );
}

fn warn_cleanup_failure(
    context: &str,
    operation: worker_operation::WorkerOperation,
    err: &dyn Error,
) {
    tracing::warn!(
        "SKIP-TEST-CLUSTER: failed to clean up postgres directories ({} via {}): {}",
        context,
        operation.as_str(),
        err
    );
}

fn warn_cleanup_removal_failure(
    context: &str,
    label: DirectoryLabel,
    path: &Path,
    err: &dyn Error,
) {
    tracing::warn!(
        "SKIP-TEST-CLUSTER: failed to remove {} directory {} ({context}): {err}",
        label.as_str(),
        path.display()
    );
}

#[cfg(test)]
#[path = "cleanup_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "property_tests.rs"]
mod property_tests;

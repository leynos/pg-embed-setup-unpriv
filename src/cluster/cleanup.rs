//! Cleanup helpers for `TestCluster` shutdown.

use std::{error::Error, path::Path};

use postgresql_embedded::Settings;

use super::{worker_invoker::WorkerInvoker as ClusterWorkerInvoker, worker_operation};
use crate::{
    CleanupMode,
    TestBootstrapSettings,
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
    let Some(operation) = cleanup_operation(bootstrap.cleanup_mode) else {
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
pub(super) fn cleanup_in_process(cleanup_mode: CleanupMode, settings: &Settings, context: &str) {
    if cleanup_mode == CleanupMode::None {
        return;
    }
    log_cleanup_start(cleanup_mode, context);
    cleanup_data_dir(cleanup_mode, settings, context);
    cleanup_install_dir(cleanup_mode, settings, context);
}

fn log_cleanup_start(cleanup_mode: CleanupMode, context: &str) {
    tracing::info!(
        target: LOG_TARGET,
        context = %context,
        cleanup_mode = ?cleanup_mode,
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
mod tests {
    //! Tests for cluster cleanup behaviour.
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use postgresql_embedded::Settings;
    use rstest::rstest;
    use tempfile::tempdir;

    use super::{cleanup_in_process, is_dangerous_cleanup_path, should_remove_install_root};
    use crate::CleanupMode;

    #[rstest]
    #[case::data_only(CleanupMode::DataOnly, false, true)]
    #[case::full(CleanupMode::Full, false, false)]
    #[case::none(CleanupMode::None, true, true)]
    fn cleanup_in_process_respects_mode(
        #[case] mode: CleanupMode,
        #[case] expect_data_exists: bool,
        #[case] expect_install_exists: bool,
    ) {
        let sandbox = tempdir().expect("tempdir");
        let data_dir = sandbox.path().join("data");
        let install_dir = sandbox.path().join("install");
        fs::create_dir_all(&data_dir).expect("create data dir");
        fs::create_dir_all(&install_dir).expect("create install dir");
        fs::write(data_dir.join("marker"), b"data").expect("write data marker");
        fs::write(install_dir.join("marker"), b"install").expect("write install marker");

        let settings = Settings {
            data_dir,
            installation_dir: install_dir,
            ..Settings::default()
        };

        cleanup_in_process(mode, &settings, "cleanup-test");
        cleanup_in_process(mode, &settings, "cleanup-test");

        assert_eq!(
            settings.data_dir.exists(),
            expect_data_exists,
            "data directory presence should match cleanup mode",
        );
        assert_eq!(
            settings.installation_dir.exists(),
            expect_install_exists,
            "installation directory presence should match cleanup mode",
        );
    }

    // The installation root is only ever removed when it lives under the
    // installation directory, so removing the installation directory always
    // cascades to it. That makes filesystem state an unreliable oracle for the
    // dedicated installation-root branch, so assert the decision directly; this
    // fails if `should_remove_install_root` stops guarding the branch.
    #[rstest]
    #[case::nested_under_install("/opt/pg/install", "/opt/pg/install/secrets", true)]
    #[case::equal_to_install("/opt/pg/install", "/opt/pg/install", false)]
    #[case::outside_install("/opt/pg/install", "/elsewhere/secrets", false)]
    #[case::parent_dir_traversal("/opt/pg/install", "/opt/pg/install/../evil", false)]
    fn should_remove_install_root_classifies_parent(
        #[case] install: &str,
        #[case] parent: &str,
        #[case] expected: bool,
    ) {
        let settings = Settings {
            installation_dir: PathBuf::from(install),
            ..Settings::default()
        };
        assert_eq!(
            should_remove_install_root(Path::new(parent), &settings),
            expected,
            "unexpected installation-root removal decision",
        );
    }

    // Validate the dangerous-path guard directly rather than driving
    // `cleanup_in_process` against the real filesystem root. The root is
    // resolved at runtime because a literal "/" is not an absolute path on
    // Windows; the test fails if the guard stops flagging the root or an empty
    // path.
    #[test]
    fn is_dangerous_cleanup_path_flags_root_and_empty() {
        assert!(
            is_dangerous_cleanup_path(Path::new("")),
            "an empty path must be flagged as dangerous"
        );

        let root = std::env::current_dir()
            .expect("resolve current dir")
            .ancestors()
            .last()
            .expect("an absolute directory has a root ancestor")
            .to_path_buf();
        assert!(
            is_dangerous_cleanup_path(&root),
            "filesystem root {root:?} must be flagged as dangerous"
        );

        assert!(
            !is_dangerous_cleanup_path(Path::new("data/pg-embed")),
            "an ordinary relative path must not be flagged"
        );
    }
}
#[cfg(test)]
#[path = "property_tests.rs"]
mod property_tests;

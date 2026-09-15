//! Metric recording for password reuse: exactly one bounded outcome per
//! command call, and none from the query.
//!
//! The recorder is process-wide, so every test here carries
//! `#[serial(metrics_recorder)]`: two installing concurrently would collect
//! each other's counts. That is the same structural reason the crate
//! serialises its worker-operation hook.

use std::sync::{Arc, Mutex, PoisonError};

use serial_test::serial;

use super::*;
use crate::observability::{
    Metric,
    MetricsRecorder,
    PasswordReuseOutcomeMetric,
    install_metrics_recorder,
};

/// A recorder that keeps what it was given.
#[derive(Default)]
struct Collected(Mutex<Vec<Metric>>);

impl Collected {
    fn taken(&self) -> Vec<Metric> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl MetricsRecorder for Collected {
    fn record(&self, metric: Metric) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(metric);
    }
}

/// Runs `body` with a collector installed and returns what it recorded.
fn collected(body: impl FnOnce()) -> Vec<Metric> {
    let recorder = Arc::new(Collected::default());
    let guard = install_metrics_recorder(Arc::clone(&recorder) as Arc<dyn MetricsRecorder>);
    body();
    drop(guard);
    recorder.taken()
}

/// One success scenario: the staged state, the caller's choice, the count.
///
/// Bundled into a struct because the case carries four fields and clippy
/// caps a function at four arguments; the file already uses this shape for
/// `ReuseCase`.
struct SuccessCase {
    with_cluster: bool,
    stored: Option<&'static str>,
    explicit: bool,
    expected: PasswordReuseOutcomeMetric,
}

/// Every success branch records exactly one bounded outcome.
#[rstest]
#[serial(metrics_recorder)]
#[case::reused(SuccessCase { with_cluster: true, stored: Some("kept-secret"), explicit: false, expected: PasswordReuseOutcomeMetric::Reused })]
#[case::explicit(SuccessCase { with_cluster: true, stored: Some("kept-secret"), explicit: true, expected: PasswordReuseOutcomeMetric::ExplicitPassword })]
#[case::no_cluster(SuccessCase { with_cluster: false, stored: None, explicit: false, expected: PasswordReuseOutcomeMetric::NoCluster })]
fn each_success_outcome_records_one_metric(scratch: Result<Scratch>, #[case] case: SuccessCase) {
    let dir = scratch.expect("scratch");
    arrange(&dir, case.with_cluster, case.stored).expect("arrange");
    let recorded = collected(|| {
        let mut settings = Settings {
            password: "fresh".into(),
            ..Settings::default()
        };
        reuse_existing_password(
            &mut settings,
            &dir.data_dir,
            &dir.password_file,
            case.explicit,
        )
        .expect("no error");
    });
    assert_eq!(recorded, vec![Metric::PasswordReuse(case.expected)]);
}

/// The two portable failure branches record exactly one bounded outcome.
#[rstest]
#[serial(metrics_recorder)]
#[case::missing(None, PasswordReuseOutcomeMetric::MissingFile)]
#[case::empty(Some(""), PasswordReuseOutcomeMetric::EmptyFile)]
fn each_failure_outcome_records_one_metric(
    scratch: Result<Scratch>,
    #[case] stored: Option<&str>,
    #[case] expected: PasswordReuseOutcomeMetric,
) {
    let dir = scratch.expect("scratch");
    arrange(&dir, true, stored).expect("arrange");
    let recorded = collected(|| {
        let mut settings = Settings::default();
        reuse_existing_password(&mut settings, &dir.data_dir, &dir.password_file, false)
            .expect_err("must fail");
    });
    assert_eq!(recorded, vec![Metric::PasswordReuse(expected)]);
}

/// The two permission-dependent failure branches record their own
/// outcomes, so all four failures are covered rather than the two that
/// need no permission staging.
///
/// Both are skipped as root, which reads regardless of mode, and both are
/// Unix-only for the same reason as the tests they mirror.
#[cfg(unix)]
#[rstest]
#[serial(metrics_recorder)]
fn an_unreadable_password_file_records_unreadable_file(scratch: Result<Scratch>) {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        return;
    }
    let dir = scratch.expect("scratch");
    arrange(&dir, true, Some("kept-secret")).expect("arrange");
    std::fs::set_permissions(&dir.password_file, std::fs::Permissions::from_mode(0o000))
        .expect("chmod");
    let recorded = collected(|| {
        let mut settings = Settings::default();
        reuse_existing_password(&mut settings, &dir.data_dir, &dir.password_file, false)
            .expect_err("must fail");
    });
    assert_eq!(
        recorded,
        vec![Metric::PasswordReuse(
            PasswordReuseOutcomeMetric::UnreadableFile
        )]
    );
}

/// A data directory that cannot be probed records `ProbeFailed`, which is
/// the outcome that would be lost if the label were derived from the error
/// kind: it shares `ClusterPasswordUnreadable` with the case above.
#[cfg(unix)]
#[rstest]
#[serial(metrics_recorder)]
fn an_unsearchable_data_dir_records_probe_failed(scratch: Result<Scratch>) {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        return;
    }
    let dir = scratch.expect("scratch");
    std::fs::set_permissions(&dir.data_dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let recorded = collected(|| {
        let mut settings = Settings::default();
        reuse_existing_password(&mut settings, &dir.data_dir, &dir.password_file, false)
            .expect_err("must fail");
    });
    std::fs::set_permissions(&dir.data_dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore");
    assert_eq!(
        recorded,
        vec![Metric::PasswordReuse(
            PasswordReuseOutcomeMetric::ProbeFailed
        )]
    );
}

/// The query records nothing, because it publishes nothing at all.
#[rstest]
#[serial(metrics_recorder)]
#[case::no_cluster(false, None)]
#[case::reusable(true, Some("kept-secret"))]
#[case::missing_file(true, None)]
fn the_query_records_no_metric(
    scratch: Result<Scratch>,
    #[case] with_cluster: bool,
    #[case] stored: Option<&str>,
) {
    let dir = scratch.expect("scratch");
    arrange(&dir, with_cluster, stored).expect("arrange");
    let recorded = collected(|| {
        drop(stored_cluster_password(&dir.data_dir, &dir.password_file));
    });
    assert!(
        recorded.is_empty(),
        "the query must not record: {recorded:?}"
    );
}

/// An explicit password records its outcome without reading the file.
///
/// The stored file is staged unreadable-by-absence, so a metric recorded
/// from the reading path would carry `MissingFile` instead.
#[rstest]
#[serial(metrics_recorder)]
fn an_explicit_password_records_without_reading(scratch: Result<Scratch>) {
    let dir = scratch.expect("scratch");
    arrange(&dir, true, None).expect("arrange");
    let recorded = collected(|| {
        let mut settings = Settings {
            password: "explicit".into(),
            ..Settings::default()
        };
        reuse_existing_password(&mut settings, &dir.data_dir, &dir.password_file, true)
            .expect("an explicit password must not consult the stored file");
    });
    assert_eq!(
        recorded,
        vec![Metric::PasswordReuse(
            PasswordReuseOutcomeMetric::ExplicitPassword
        )]
    );
}

/// No recorded metric can carry a secret or a path.
///
/// The outcome is an enum, so this holds by construction rather than by
/// inspection; the test pins that property against a future change to a
/// free-form label. Both the password and the two directory paths are
/// distinctive enough that either appearing in the debug rendering would
/// be caught.
#[rstest]
#[serial(metrics_recorder)]
fn recorded_metrics_carry_neither_secret_nor_path(scratch: Result<Scratch>) {
    let dir = scratch.expect("scratch");
    arrange(&dir, true, Some("swordfish-secret")).expect("arrange");
    let recorded = collected(|| {
        let mut settings = Settings {
            password: "fresh".into(),
            ..Settings::default()
        };
        reuse_existing_password(&mut settings, &dir.data_dir, &dir.password_file, false)
            .expect("no error");
    });
    let rendered = format!("{recorded:?}");
    assert!(!rendered.contains("swordfish-secret"), "{rendered}");
    assert!(!rendered.contains(dir.data_dir.as_str()), "{rendered}");
    assert!(!rendered.contains(dir.password_file.as_str()), "{rendered}");
}

//! Tests for password reuse against an existing cluster.

use camino::Utf8PathBuf;
use color_eyre::eyre::Result;
use rstest::{fixture, rstest};

use super::*;

/// Bootstraps with an explicit password against `$dir` and asserts it is
/// retained and the outcome is `ExplicitPassword`.
///
/// A macro rather than a helper function so a failure reports the line of the
/// calling test, and so the fallible call sits inside a recognised test body.
macro_rules! assert_explicit_password_kept {
    ($dir:expr) => {{
        let dir = $dir;
        let mut settings = Settings {
            password: "explicit".into(),
            ..Settings::default()
        };
        let outcome =
            reuse_existing_password(&mut settings, &dir.data_dir, &dir.password_file, true)
                .expect("an explicit password must not consult the stored file");
        assert_eq!(outcome, PasswordReuseOutcome::ExplicitPassword);
        assert_eq!(settings.password, "explicit");
    }};
}

/// An empty data directory and the password-file path beside it.
struct Scratch {
    _temp: tempfile::TempDir,
    data_dir: Utf8PathBuf,
    password_file: Utf8PathBuf,
}

impl Scratch {
    /// Marks the data directory as an initialized cluster.
    fn with_cluster(&self) -> Result<()> {
        std::fs::write(self.data_dir.join(PG_VERSION_MARKER), "17\n")?;
        Ok(())
    }

    /// Writes the stored password file.
    fn with_stored(&self, value: &str) -> Result<()> {
        std::fs::write(&self.password_file, value)?;
        Ok(())
    }
}

#[fixture]
fn scratch() -> Result<Scratch> {
    let temp = tempfile::tempdir()?;
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir)?;
    Ok(Scratch {
        _temp: temp,
        data_dir,
        password_file: root.join(".pgpass"),
    })
}

/// Applies one case's on-disk state to a scratch directory.
fn arrange(scratch: &Scratch, with_cluster: bool, stored: Option<&str>) -> Result<()> {
    if with_cluster {
        scratch.with_cluster()?;
    }
    if let Some(value) = stored {
        scratch.with_stored(value)?;
    }
    Ok(())
}

/// One reuse scenario: the on-disk state, the caller's choice, and the outcome.
struct ReuseCase {
    with_cluster: bool,
    stored: Option<&'static str>,
    explicit: bool,
    expected: PasswordReuseOutcome,
    expected_password: &'static str,
}

/// A stored password is adopted only when a cluster exists and none was given.
#[rstest]
#[case::existing_cluster(ReuseCase { with_cluster: true, stored: Some("kept-secret\n"), explicit: false, expected: PasswordReuseOutcome::Reused, expected_password: "kept-secret" })]
#[case::explicit_wins(ReuseCase { with_cluster: true, stored: Some("kept-secret"), explicit: true, expected: PasswordReuseOutcome::ExplicitPassword, expected_password: "fresh" })]
#[case::no_cluster(ReuseCase { with_cluster: false, stored: None, explicit: false, expected: PasswordReuseOutcome::NoCluster, expected_password: "fresh" })]
fn reuse_rules(scratch: Result<Scratch>, #[case] case: ReuseCase) {
    let dir = scratch.expect("scratch");
    arrange(&dir, case.with_cluster, case.stored).expect("arrange");
    let mut settings = Settings {
        password: "fresh".into(),
        ..Settings::default()
    };
    let adopted = reuse_existing_password(
        &mut settings,
        &dir.data_dir,
        &dir.password_file,
        case.explicit,
    )
    .expect("no error");
    assert_eq!(adopted, case.expected);
    assert_eq!(settings.password, case.expected_password);
}

/// The query half reports no cluster without touching the password file.
#[rstest]
fn no_cluster_yields_none_without_reading(scratch: Result<Scratch>) {
    let dir = scratch.expect("scratch");
    let stored = stored_cluster_password(&dir.data_dir, &dir.password_file)
        .expect("no cluster is not an error");
    assert_eq!(stored, None);
}

/// A cluster without a readable password file fails with a named remedy.
#[rstest]
#[case::missing(None, "is missing", BootstrapErrorKind::ClusterPasswordMissing)]
#[case::empty(Some(""), "is empty", BootstrapErrorKind::ClusterPasswordEmpty)]
fn missing_or_empty_password_file_fails(
    scratch: Result<Scratch>,
    #[case] stored: Option<&str>,
    #[case] needle: &str,
    #[case] kind: BootstrapErrorKind,
) {
    let dir = scratch.expect("scratch");
    arrange(&dir, true, stored).expect("arrange");
    let mut settings = Settings::default();
    let err = reuse_existing_password(&mut settings, &dir.data_dir, &dir.password_file, false)
        .expect_err("must fail");
    let message = err.to_string();
    assert!(message.contains(needle), "{message}");
    assert!(message.contains("PG_PASSWORD"), "{message}");
    assert!(
        message.contains(dir.data_dir.as_str()),
        "the message must name the data directory: {message}"
    );
    assert_eq!(err.kind(), kind);
}

/// A password file that exists but cannot be read is
/// `ClusterPasswordUnreadable`, distinct from the missing-file kind.
#[cfg(unix)]
#[rstest]
fn unreadable_password_file_is_an_error(scratch: Result<Scratch>) {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        return; // root reads regardless of mode, so the case cannot be staged
    }
    let dir = scratch.expect("scratch");
    arrange(&dir, true, Some("kept-secret")).expect("arrange");
    std::fs::set_permissions(&dir.password_file, std::fs::Permissions::from_mode(0o000))
        .expect("chmod");
    let err = stored_cluster_password(&dir.data_dir, &dir.password_file)
        .expect_err("an unreadable password file must not read as missing");
    assert_eq!(err.kind(), BootstrapErrorKind::ClusterPasswordUnreadable);
    assert!(err.to_string().contains("cannot be read"), "{err}");
    assert!(
        err.to_string().contains(dir.data_dir.as_str()),
        "the message must name the data directory: {err}"
    );
}

/// A data directory that cannot be probed is an error, not "no cluster".
#[cfg(unix)]
#[rstest]
fn unsearchable_data_dir_is_an_error(scratch: Result<Scratch>) {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        return; // root bypasses directory permissions, so the probe cannot fail
    }
    let dir = scratch.expect("scratch");
    std::fs::set_permissions(&dir.data_dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let outcome = stored_cluster_password(&dir.data_dir, &dir.password_file);
    std::fs::set_permissions(&dir.data_dir, std::fs::Permissions::from_mode(0o700))
        .expect("restore");
    let err = outcome.expect_err("EACCES must not read as no cluster");
    assert_eq!(err.kind(), BootstrapErrorKind::ClusterPasswordUnreadable);
    assert!(
        err.to_string().contains(dir.data_dir.as_str()),
        "the message must name the data directory: {err}"
    );
}

/// An explicit password is kept whatever state the stored file is in.
///
/// `reuse_existing_password` must not consult the filesystem at all when
/// the caller supplied `PG_PASSWORD`, so a cluster whose password file is
/// missing or empty is not an error on that path. Without this the
/// documented escape from a stale cluster would itself fail.
#[rstest]
#[case::missing_stored_file(None)]
#[case::empty_stored_file(Some(""))]
fn explicit_password_survives_an_invalid_stored_file(
    scratch: Result<Scratch>,
    #[case] stored: Option<&str>,
) {
    let dir = scratch.expect("scratch");
    arrange(&dir, true, stored).expect("arrange");
    assert_explicit_password_kept!(&dir);
}

/// The same rule for a stored file that exists but cannot be read.
#[cfg(unix)]
#[rstest]
fn explicit_password_survives_an_unreadable_stored_file(scratch: Result<Scratch>) {
    use std::os::unix::fs::PermissionsExt;
    if nix::unistd::geteuid().is_root() {
        return; // root reads regardless of mode, so the case cannot be staged
    }
    let dir = scratch.expect("scratch");
    arrange(&dir, true, Some("kept-secret")).expect("arrange");
    std::fs::set_permissions(&dir.password_file, std::fs::Permissions::from_mode(0o000))
        .expect("chmod");
    assert_explicit_password_kept!(&dir);
}

#[path = "password_metrics_tests.rs"]
mod metrics;

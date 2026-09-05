//! Reuses the superuser password of an existing cluster.
//!
//! `postgresql_embedded::Settings::default()` generates a fresh random password
//! on every bootstrap, but a data directory that already holds a cluster keeps
//! the password its `initdb` was given. Without this step every later
//! bootstrap on a host with prior state starts a server nobody can log in to.
//! The password `initdb` used is the one `postgresql_embedded` wrote to the
//! password file, so it is read back from there.

use std::io::ErrorKind;

use camino::Utf8Path;
use color_eyre::eyre::eyre;
use postgresql_embedded::Settings;
use tracing::info;

use crate::{
    error::{BootstrapError, BootstrapResult},
    observability::LOG_TARGET,
};

/// Marker `initdb` leaves in a data directory.
const PG_VERSION_MARKER: &str = "PG_VERSION";

/// Why the bootstrap did or did not adopt a stored password; a bounded label
/// for the `password_reuse` tracing event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordReuseOutcome {
    /// The stored password was adopted.
    Reused,
    /// The caller supplied `PG_PASSWORD`, which always wins.
    ExplicitPassword,
    /// The data directory holds no cluster, so a fresh password is fine.
    NoCluster,
}

/// Reads the password an existing cluster in `data_dir` was initialized with.
///
/// This is the query half of password reuse: it touches the filesystem and
/// changes nothing. `Ok(None)` means the data directory holds no cluster
/// (no `PG_VERSION` marker). A cluster whose password file is missing,
/// unreadable or empty is an error, because a server started against it
/// could not be logged in to.
///
/// # Errors
///
/// Returns an error naming the data directory, the password file and the
/// remedies (`PG_PASSWORD`, or removing the stale cluster).
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::stored_cluster_password;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let stored = stored_cluster_password(
///     Utf8Path::new("/var/tmp/pg-embed-1000/data"),
///     Utf8Path::new("/var/tmp/pg-embed-1000/install/.pgpass"),
/// )?;
/// assert!(stored.is_none() || stored.is_some_and(|password| !password.is_empty()));
/// # Ok(())
/// # }
/// ```
pub fn stored_cluster_password(
    data_dir: &Utf8Path,
    password_file: &Utf8Path,
) -> BootstrapResult<Option<String>> {
    if !data_dir.join(PG_VERSION_MARKER).is_file() {
        return Ok(None);
    }
    read_stored_password(password_file, data_dir).map(Some)
}

/// Aligns `settings.password` with the cluster already present in
/// `data_dir`, unless the caller supplied an explicit password.
///
/// This is the command half: it consults [`stored_cluster_password`] and
/// mutates only `settings.password`. Returns the outcome so callers and the
/// `password_reuse` tracing event can report which branch was taken.
///
/// # Errors
///
/// Propagates [`stored_cluster_password`]'s error when the data directory
/// holds a cluster but its password file is missing, unreadable or empty.
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::{PasswordReuseOutcome, PgEnvCfg, reuse_existing_password};
/// use postgresql_embedded::Settings;
///
/// # fn main() -> pg_embedded_setup_unpriv::BootstrapResult<()> {
/// let cfg = PgEnvCfg::default();
/// let mut settings = cfg.to_settings()?;
/// let outcome = reuse_existing_password(
///     &mut settings,
///     Utf8Path::new("/var/tmp/pg-embed-1000/data"),
///     Utf8Path::new("/var/tmp/pg-embed-1000/install/.pgpass"),
///     cfg.password.is_some(),
/// )?;
/// assert!(outcome != PasswordReuseOutcome::Reused || !settings.password.is_empty());
/// # Ok(())
/// # }
/// ```
pub fn reuse_existing_password(
    settings: &mut Settings,
    data_dir: &Utf8Path,
    password_file: &Utf8Path,
    is_password_explicit: bool,
) -> BootstrapResult<PasswordReuseOutcome> {
    let outcome = if is_password_explicit {
        PasswordReuseOutcome::ExplicitPassword
    } else if let Some(stored) = stored_cluster_password(data_dir, password_file)? {
        settings.password = stored;
        PasswordReuseOutcome::Reused
    } else {
        PasswordReuseOutcome::NoCluster
    };
    log_outcome(outcome, data_dir, password_file);
    Ok(outcome)
}

/// Emits the bounded `password_reuse` event; no path or secret is a label.
fn log_outcome(outcome: PasswordReuseOutcome, data_dir: &Utf8Path, password_file: &Utf8Path) {
    info!(
        target: LOG_TARGET,
        outcome = ?outcome,
        data_dir = %data_dir,
        password_file = %password_file,
        "password_reuse"
    );
}

/// Reads and trims the stored password, turning I/O and emptiness into the
/// user-facing remedy message.
fn read_stored_password(password_file: &Utf8Path, data_dir: &Utf8Path) -> BootstrapResult<String> {
    let raw = std::fs::read_to_string(password_file).map_err(|err| {
        let hint = if err.kind() == ErrorKind::NotFound {
            "is missing"
        } else {
            "cannot be read"
        };
        BootstrapError::from(eyre!(
            "data directory {data_dir} already holds a cluster but its password file \
             {password_file} {hint} ({err}); set PG_PASSWORD to the password that initialized it, \
             or remove the stale cluster"
        ))
    })?;
    let stored = raw.trim_end_matches(['\n', '\r']).to_owned();
    if stored.is_empty() {
        return Err(BootstrapError::from(eyre!(
            "password file {password_file} is empty; set PG_PASSWORD or remove the stale cluster \
             at {data_dir}"
        )));
    }
    Ok(stored)
}

#[cfg(test)]
mod tests {
    //! Tests for password reuse against an existing cluster.
    use camino::Utf8PathBuf;
    use color_eyre::eyre::Result;
    use rstest::{fixture, rstest};

    use super::*;

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
    #[case::missing(None, "is missing")]
    #[case::empty(Some(""), "is empty")]
    fn missing_or_empty_password_file_fails(
        scratch: Result<Scratch>,
        #[case] stored: Option<&str>,
        #[case] needle: &str,
    ) {
        let dir = scratch.expect("scratch");
        arrange(&dir, true, stored).expect("arrange");
        let mut settings = Settings::default();
        let err = reuse_existing_password(&mut settings, &dir.data_dir, &dir.password_file, false)
            .expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains(needle), "{message}");
        assert!(message.contains("PG_PASSWORD"), "{message}");
    }
}

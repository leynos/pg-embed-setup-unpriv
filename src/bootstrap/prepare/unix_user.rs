//! Unix user ownership helpers for bootstrap preparation.
//!
//! This Unix-only module handles filesystem ownership work that the
//! cross-platform prepare flow cannot express portably, such as handing
//! prepared directories and `PGPASSFILE` ownership to the target user. Keeping
//! those operations here leaves the shared prepare logic focused on path and
//! environment setup while isolating Unix permission and descriptor-based file
//! handling behind a narrow boundary.

use camino::Utf8PathBuf;
use cap_std::fs::{Dir, File, OpenOptions, OpenOptionsExt};
use nix::{
    sys::stat::{Mode, fchmod},
    unistd::{User, fchown},
};

use super::PGPASS_MODE;
use crate::{
    error::{BootstrapError, BootstrapResult},
    privileges::ensure_dir_for_user,
};

pub(super) fn ensure_install_dir_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    if let Err(err) = ensure_dir_for_user(path, user, 0o755) {
        tracing::warn!(
            target: crate::observability::LOG_TARGET,
            path = path.as_str(),
            uid = user.uid.as_raw(),
            gid = user.gid.as_raw(),
            error = ?err,
            "failed to prepare install directory for user"
        );
        return Err(err.into());
    }
    Ok(())
}

pub(super) fn ensure_pgpass_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    let (dir, relative) = crate::fs::ambient_dir_and_path(path)?;
    reject_root_pgpass_path(&relative)?;
    let Some(file) = open_pgpass_for_user(path, &dir, &relative)? else {
        return Ok(());
    };
    let metadata = pgpass_metadata(path, &file)?;
    ensure_pgpass_is_regular_file(path, metadata.is_file())?;

    set_pgpass_owner(path, &file, user)?;
    set_pgpass_mode(path, &file)?;
    Ok(())
}

fn reject_root_pgpass_path(relative: &Utf8PathBuf) -> BootstrapResult<()> {
    // The descriptor-relative lookup anchors path resolution and prevents
    // ancestor directory swap attacks. O_NOFOLLOW additionally ensures the
    // final path component is not a symlink.
    if relative.as_str().is_empty() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PGPASSFILE cannot point at the root directory"
        )));
    }
    Ok(())
}

fn open_pgpass_for_user(
    path: &Utf8PathBuf,
    dir: &Dir,
    relative: &Utf8PathBuf,
) -> BootstrapResult<Option<File>> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .create(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = match dir.open_with(relative.as_std_path(), &options) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            tracing::warn!(
                target: crate::observability::LOG_TARGET,
                path = path.as_str(),
                error = %err,
                "failed to open PGPASSFILE for ownership preparation"
            );
            return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "open {} failed: {err}",
                path.as_str()
            )));
        }
    };
    Ok(Some(file))
}

fn pgpass_metadata(path: &Utf8PathBuf, file: &File) -> BootstrapResult<cap_std::fs::Metadata> {
    file.metadata().map_err(|err| {
        tracing::warn!(
            target: crate::observability::LOG_TARGET,
            path = path.as_str(),
            error = %err,
            "failed to stat PGPASSFILE for ownership preparation"
        );
        BootstrapError::from(color_eyre::eyre::eyre!(
            "stat {} failed: {err}",
            path.as_str()
        ))
    })
}

fn ensure_pgpass_is_regular_file(path: &Utf8PathBuf, is_file: bool) -> BootstrapResult<()> {
    if !is_file {
        tracing::warn!(
            target: crate::observability::LOG_TARGET,
            path = path.as_str(),
            "PGPASSFILE ownership preparation rejected non-regular file"
        );
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PGPASSFILE must reference a regular file: {}",
            path.as_str()
        )));
    }
    Ok(())
}

fn set_pgpass_owner(path: &Utf8PathBuf, file: &File, user: &User) -> BootstrapResult<()> {
    let uid = user.uid.as_raw();
    let gid = user.gid.as_raw();
    fchown(file, Some(user.uid), Some(user.gid)).map_err(|err| {
        tracing::warn!(
            target: crate::observability::LOG_TARGET,
            path = path.as_str(),
            uid,
            gid,
            error = %err,
            "failed to set PGPASSFILE ownership"
        );
        BootstrapError::from(color_eyre::eyre::eyre!(
            "fchown {} failed (uid={uid} gid={gid}): {err}",
            path.as_str()
        ))
    })
}

fn set_pgpass_mode(path: &Utf8PathBuf, file: &File) -> BootstrapResult<()> {
    let mode = libc::mode_t::try_from(PGPASS_MODE).map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "invalid PGPASSFILE mode 0o{:03o}: {err}",
            PGPASS_MODE
        ))
    })?;
    fchmod(file, Mode::from_bits_truncate(mode)).map_err(|err| {
        tracing::warn!(
            target: crate::observability::LOG_TARGET,
            path = path.as_str(),
            mode = format_args!("0o{:03o}", PGPASS_MODE),
            error = %err,
            "failed to set PGPASSFILE permissions"
        );
        BootstrapError::from(color_eyre::eyre::eyre!(
            "fchmod {} failed (mode=0o{:03o}): {err}",
            path.as_str(),
            PGPASS_MODE
        ))
    })
}

#[cfg(test)]
mod tests {
    //! Unit tests for Unix user ownership helpers.
    //!
    //! Ownership is assigned to the *current* user, which non-root processes may
    //! do freely, so the fchown/fchmod paths are covered without elevated
    //! privileges.

    use std::os::unix::fs::PermissionsExt as _;

    use nix::unistd::{User, getuid};

    use super::*;

    fn current_user() -> User {
        User::from_uid(getuid())
            .expect("query current user")
            .expect("current user has a passwd entry")
    }

    fn utf8(path: &std::path::Path) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(path.to_path_buf()).expect("temp path is UTF-8")
    }

    #[test]
    fn ensure_install_dir_creates_directory_for_user() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir = utf8(temp.path()).join("install");
        ensure_install_dir_for_user(&dir, &current_user()).expect("prepare install dir");
        assert!(dir.is_dir(), "install directory should exist");
    }

    #[test]
    fn ensure_pgpass_sets_owner_only_permissions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pgpass = utf8(temp.path()).join(".pgpass");
        std::fs::write(pgpass.as_std_path(), b"host:5432:db:user:secret\n").expect("write pgpass");

        ensure_pgpass_for_user(&pgpass, &current_user()).expect("prepare pgpass");

        let mode = std::fs::metadata(pgpass.as_std_path())
            .expect("stat pgpass")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, PGPASS_MODE, "pgpass should be clamped to PGPASS_MODE");
    }

    #[test]
    fn ensure_pgpass_is_noop_when_file_absent() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pgpass = utf8(temp.path()).join("missing/.pgpass");
        // The parent exists but the file does not, so preparation is a no-op.
        std::fs::create_dir_all(pgpass.parent().expect("parent").as_std_path())
            .expect("create parent");
        ensure_pgpass_for_user(&pgpass, &current_user()).expect("absent pgpass is a no-op");
    }

    #[test]
    fn ensure_pgpass_rejects_root_path() {
        let err = ensure_pgpass_for_user(&Utf8PathBuf::from("/"), &current_user())
            .expect_err("root pgpass path must be rejected");
        assert!(
            err.to_string().contains("root directory"),
            "expected root-path rejection, got: {err}"
        );
    }

    #[test]
    fn ensure_pgpass_rejects_non_regular_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let dir_path = utf8(temp.path()).join("not-a-file");
        std::fs::create_dir(dir_path.as_std_path()).expect("create dir");
        let err = ensure_pgpass_for_user(&dir_path, &current_user())
            .expect_err("directory pgpass must be rejected");
        assert!(
            err.to_string().contains("regular file"),
            "expected regular-file rejection, got: {err}"
        );
    }

    #[test]
    fn reject_root_pgpass_path_flags_empty_relative() {
        assert!(reject_root_pgpass_path(&Utf8PathBuf::new()).is_err());
        assert!(reject_root_pgpass_path(&Utf8PathBuf::from("child")).is_ok());
    }
}

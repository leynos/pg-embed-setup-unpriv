//! Unix user ownership helpers for bootstrap preparation.
//!
//! This Unix-only module handles filesystem ownership work that the
//! cross-platform prepare flow cannot express portably, such as handing
//! prepared directories and `PGPASSFILE` ownership to the target user. Keeping
//! those operations here leaves the shared prepare logic focused on path and
//! environment setup while isolating Unix permission and descriptor-based file
//! handling behind a narrow boundary.

use camino::Utf8PathBuf;
use cap_std::fs::{OpenOptions, OpenOptionsExt};
use nix::sys::stat::{Mode, fchmod};
use nix::unistd::{User, fchown};

use crate::error::{BootstrapError, BootstrapResult};
use crate::privileges::ensure_dir_for_user;

use super::PGPASS_MODE;

pub(super) fn ensure_install_dir_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    ensure_dir_for_user(path, user, 0o755)?;
    Ok(())
}

pub(super) fn ensure_pgpass_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    // The descriptor-relative lookup anchors path resolution and prevents
    // ancestor directory swap attacks. O_NOFOLLOW additionally ensures the
    // final path component is not a symlink.
    let (dir, relative) = crate::fs::ambient_dir_and_path(path)?;
    if relative.as_str().is_empty() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PGPASSFILE cannot point at the root directory"
        )));
    }
    let mut options = OpenOptions::new();
    options
        .read(true)
        .create(false)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = match dir.open_with(relative.as_std_path(), &options) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => {
            return Err(BootstrapError::from(color_eyre::eyre::eyre!(
                "open {} failed: {err}",
                path.as_str()
            )));
        }
    };
    let metadata = file.metadata().map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "stat {} failed: {err}",
            path.as_str()
        ))
    })?;
    if !metadata.is_file() {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "PGPASSFILE must reference a regular file: {}",
            path.as_str()
        )));
    }

    let uid = user.uid.as_raw();
    let gid = user.gid.as_raw();

    fchown(&file, Some(user.uid), Some(user.gid)).map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "fchown {} failed (uid={uid} gid={gid}): {err}",
            path.as_str()
        ))
    })?;
    fchmod(&file, Mode::from_bits_truncate(PGPASS_MODE)).map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "fchmod {} failed (mode=0o{:03o}): {err}",
            path.as_str(),
            PGPASS_MODE
        ))
    })?;
    Ok(())
}

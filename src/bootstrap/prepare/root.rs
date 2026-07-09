//! Root-privileged bootstrap preparation.
//!
//! Prepares filesystem state when running as root: paths are handed to the
//! unprivileged `nobody` user so the bundled `PostgreSQL` binaries can
//! initialize safely after privileges drop.

use std::net::TcpListener;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, eyre};
use nix::unistd::{User, fchown};
use postgresql_embedded::Settings;

use super::{
    PGPASS_MODE,
    PreparedBootstrap,
    ensure_parents_for_paths,
    log_sanitized_settings,
    prepare_xdg_dirs,
    resolve_settings_paths_for_uid,
};
use crate::{
    PgEnvCfg,
    bootstrap::env::{TestBootstrapEnvironment, XdgDirs, prepare_timezone_env},
    error::{BootstrapError, BootstrapResult},
    privileges::{ensure_dir_for_user, ensure_tree_owned_by_user, make_data_dir_private},
};

pub(super) fn bootstrap_with_root(
    mut settings: Settings,
    cfg: &PgEnvCfg,
) -> BootstrapResult<PreparedBootstrap> {
    // Worker subprocesses drop after each operation; keep the data dir so start can
    // proceed after setup.
    settings.temporary = false;
    ensure_root_port(&mut settings)?;

    let nobody_user = User::from_name("nobody")
        .context("failed to resolve user 'nobody'")?
        .ok_or_else(|| color_eyre::eyre::eyre!("user 'nobody' not found"))?;

    let paths = resolve_settings_paths_for_uid(&mut settings, cfg, nobody_user.uid)?;
    log_sanitized_settings(&settings);

    ensure_parents_for_paths(&paths, |path| ensure_parent_for_user(path, &nobody_user))?;

    ensure_install_dir_for_user(&paths.install_dir, &nobody_user)?;
    make_data_dir_private(&paths.data_dir, &nobody_user)?;

    let timezone = prepare_timezone_env()?;
    let xdg = prepare_xdg_dirs(&paths.install_dir)?;
    ensure_xdg_dirs_owned_by_user(&xdg, &nobody_user)?;

    ensure_pgpass_for_user(&paths.password_file, &nobody_user)?;

    ensure_tree_owned_by_user(&paths.install_dir, &nobody_user)?;
    if paths.data_default {
        ensure_tree_owned_by_user(&paths.data_dir, &nobody_user)?;
    }

    let environment = TestBootstrapEnvironment::from_components(xdg, paths.password_file, timezone);
    Ok(PreparedBootstrap {
        settings,
        environment,
    })
}

fn ensure_root_port(settings: &mut Settings) -> BootstrapResult<()> {
    if settings.port > 0 {
        return Ok(());
    }

    let host = root_bind_host(settings);
    let listener = TcpListener::bind((host, 0))
        .map_err(|err| BootstrapError::from(eyre!("failed to allocate port: {err}")))?;
    let port = listener
        .local_addr()
        .map_err(|err| BootstrapError::from(eyre!("failed to read allocated port: {err}")))?
        .port();
    settings.port = port;
    Ok(())
}

fn root_bind_host(settings: &Settings) -> &str {
    let host = settings.host.as_str();
    if host.is_empty() || host.starts_with('/') {
        "127.0.0.1"
    } else {
        host
    }
}

pub(super) fn ensure_xdg_dirs_owned_by_user(xdg: &XdgDirs, user: &User) -> BootstrapResult<()> {
    // The cache/run directories are created by the root worker, so explicitly
    // hand them to the unprivileged user to keep custom install dirs usable.
    ensure_dir_for_user(&xdg.cache, user, 0o755)?;
    ensure_dir_for_user(&xdg.runtime, user, 0o700)?;
    Ok(())
}

pub(super) fn ensure_parent_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    if let Some(parent) = path.parent() {
        ensure_dir_for_user(parent, user, 0o755)?;
    }
    Ok(())
}

pub(super) fn ensure_install_dir_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    ensure_dir_for_user(path, user, 0o755)?;
    Ok(())
}

pub(super) fn ensure_pgpass_for_user(path: &Utf8PathBuf, user: &User) -> BootstrapResult<()> {
    use cap_std::fs::{OpenOptions, OpenOptionsExt};
    use nix::sys::stat::{Mode, fchmod};

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
    let mode = libc::mode_t::try_from(PGPASS_MODE).map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "invalid PGPASSFILE mode 0o{:03o}: {err}",
            PGPASS_MODE
        ))
    })?;
    fchmod(&file, Mode::from_bits_truncate(mode)).map_err(|err| {
        BootstrapError::from(color_eyre::eyre::eyre!(
            "fchmod {} failed (mode=0o{:03o}): {err}",
            path.as_str(),
            PGPASS_MODE
        ))
    })?;
    Ok(())
}

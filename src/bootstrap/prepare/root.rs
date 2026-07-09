//! Root-privileged bootstrap preparation.
//!
//! Prepares filesystem state when running as root: paths are handed to the
//! unprivileged `nobody` user so the bundled `PostgreSQL` binaries can
//! initialize safely after privileges drop.

use std::net::TcpListener;

use camino::Utf8PathBuf;
use color_eyre::eyre::{Context, eyre};
use nix::unistd::User;
use postgresql_embedded::Settings;

use super::{
    PreparedBootstrap,
    ensure_parents_for_paths,
    log_sanitized_settings,
    prepare_xdg_dirs,
    resolve_settings_paths_for_uid,
    unix_user::{ensure_install_dir_for_user, ensure_pgpass_for_user},
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

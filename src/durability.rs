//! Non-durable settings for disposable test clusters (#297).
//!
//! A cluster started for a test is thrown away, so its durability is cost
//! without benefit: `initdb` syncs its whole data directory, the server fsyncs
//! its WAL, and shutdown checkpoints. On a loaded or shared disk that cost
//! dominated cluster start times, so test bootstraps run `initdb --no-sync` and
//! set `fsync`, `synchronous_commit` and `full_page_writes` off (the server already
//! runs with `-F`, so `initdb` is the part that matters most). `PG_EMBED_DURABLE=1`
//! opts out and states `fsync = on` explicitly. The setup-only `run` function and the binary build
//! clusters meant to outlive the process and keep `PostgreSQL`'s durable defaults.
//!
//! The `fsync = off` entry in `Settings::configuration` is also the marker the
//! `initdb` step reads to know the cluster is disposable, so the setting is the
//! one source of truth and travels to the worker in the settings snapshot.

use std::{ffi::OsString, io, path::PathBuf, process::Command};

use postgresql_embedded::{BOOTSTRAP_SUPERUSER, Settings};

/// Environment variable that, set to `1`, keeps a test cluster durable.
pub const DURABLE_VAR: &str = "PG_EMBED_DURABLE";

/// The server settings a disposable cluster turns off.
pub(crate) const DISPOSABLE_SETTINGS: [(&str, &str); 3] = [
    ("fsync", "off"),
    ("synchronous_commit", "off"),
    ("full_page_writes", "off"),
];

/// Whether the environment asks for a durable test cluster.
pub(crate) fn is_durable_requested(lookup: impl Fn(&str) -> Option<OsString>) -> bool {
    lookup(DURABLE_VAR).is_some_and(|value| value == "1")
}

/// Turns durability off in `settings`, or on again when `durable` is set.
///
/// `postgresql_embedded` starts every server with `-F`, which turns `fsync` off
/// on the command line, so a durable cluster needs `fsync = on` stated
/// explicitly: a `-c fsync=on` given after `-F` wins. A value the caller already
/// put in the configuration wins over either, as with the worker limits.
pub(crate) fn apply_disposable_defaults(settings: &mut Settings, durable: bool) {
    if durable {
        settings
            .configuration
            .entry("fsync".to_owned())
            .or_insert_with(|| "on".to_owned());
        return;
    }
    for (key, value) in DISPOSABLE_SETTINGS {
        settings
            .configuration
            .entry(key.to_owned())
            .or_insert_with(|| value.to_owned());
    }
}

/// Whether `settings` describe a disposable cluster.
pub(crate) fn is_disposable(settings: &Settings) -> bool {
    settings
        .configuration
        .get("fsync")
        .is_some_and(|value| value == "off")
}

/// Runs `initdb --no-sync` for a disposable cluster whose binaries are already
/// installed and whose data directory is not yet initialized.
///
/// Returns whether it initialized the directory. It does nothing, and leaves the
/// work to `PostgreSQL::setup`, when the cluster is not disposable, is already
/// initialized, or has no installed binaries yet (a cold install tree, where
/// `setup` installs and runs `initdb` in one step and cannot be interleaved).
/// `setup` skips initialization once the directory holds `postgresql.conf`, so
/// handing off afterwards is safe.
///
/// # Errors
///
/// Returns the I/O error when the password file cannot be written or `initdb`
/// cannot run or fails, with its standard error in the message.
pub fn initialize_without_sync(settings: &Settings) -> io::Result<bool> {
    if !is_disposable(settings) || settings.data_dir.join("postgresql.conf").exists() {
        return Ok(false);
    }
    let Some(initdb) = installed_initdb(settings) else {
        return Ok(false);
    };
    if !settings.password_file.exists() {
        std::fs::write(&settings.password_file, settings.password.as_bytes())?;
    }
    let output = Command::new(initdb)
        .arg("--pgdata")
        .arg(&settings.data_dir)
        .arg("--username")
        .arg(BOOTSTRAP_SUPERUSER)
        .arg("--auth=password")
        .arg("--pwfile")
        .arg(&settings.password_file)
        .arg("--encoding=UTF8")
        .arg("--no-sync")
        .output()?;
    if output.status.success() {
        return Ok(true);
    }
    Err(io::Error::other(format!(
        "initdb --no-sync failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/// Sets a cluster up in process: `initdb --no-sync` for a disposable cluster
/// that can have it, then the ordinary `PostgreSQL::setup`.
///
/// The blocking `initdb` runs on the blocking pool so it does not stall the
/// caller's runtime.
///
/// # Errors
///
/// Returns `initdb`'s failure as an I/O error, or `setup`'s own error.
pub(crate) async fn setup_disposable(
    embedded: &mut postgresql_embedded::PostgreSQL,
) -> Result<(), postgresql_embedded::Error> {
    let settings = embedded.settings().clone();
    tokio::task::spawn_blocking(move || initialize_without_sync(&settings))
        .await
        .map_err(|err| postgresql_embedded::Error::IoError(err.to_string()))?
        .map_err(|err| postgresql_embedded::Error::IoError(err.to_string()))?;
    embedded.setup().await
}

/// The installed `initdb`, if the binaries are in place.
fn installed_initdb(settings: &Settings) -> Option<PathBuf> {
    let installed = crate::cluster::resolve_installed_dir(settings)?;
    let initdb = installed.join("bin").join("initdb");
    initdb.is_file().then_some(initdb)
}

#[cfg(test)]
#[path = "durability_tests.rs"]
mod tests;

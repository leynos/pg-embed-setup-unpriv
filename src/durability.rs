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

use std::{
    ffi::OsString,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use postgresql_embedded::{BOOTSTRAP_SUPERUSER, Settings, Version};

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

/// How often a running `initdb` is checked for completion, a deadline, and
/// cancellation.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Runs `initdb --no-sync` for a disposable cluster whose binaries are already
/// installed and whose data directory is not yet initialized.
///
/// Returns whether it initialized the directory. It does nothing, and leaves the
/// work to `PostgreSQL::setup`, when the cluster is not disposable, is already
/// initialized, or has no installed binaries for `settings.version` yet (a cold
/// install tree, where `setup` installs and runs `initdb` in one step and cannot
/// be interleaved). `setup` skips initialization once the directory holds
/// `postgresql.conf`, so handing off afterwards is safe.
///
/// `initdb` is bounded by `settings.timeout`, as upstream bounds its own
/// commands: on expiry it is killed and reaped before the error is returned.
///
/// # Errors
///
/// Returns the I/O error when the password file cannot be written or `initdb`
/// cannot run, times out or fails, with its standard error in the message.
pub fn initialize_without_sync(settings: &Settings) -> io::Result<bool> {
    initialize_cancellable(settings, &AtomicBool::new(false))
}

/// [`initialize_without_sync`], ending `initdb` early once `cancel` is set.
fn initialize_cancellable(settings: &Settings, cancel: &AtomicBool) -> io::Result<bool> {
    if !is_disposable(settings) || settings.data_dir.join("postgresql.conf").exists() {
        return Ok(false);
    }
    let Some(initdb) = installed_initdb(settings) else {
        return Ok(false);
    };
    if !settings.password_file.exists() {
        std::fs::write(&settings.password_file, settings.password.as_bytes())?;
    }
    let mut command = Command::new(initdb);
    command
        .arg("--pgdata")
        .arg(&settings.data_dir)
        .arg("--username")
        .arg(BOOTSTRAP_SUPERUSER)
        .arg("--auth=password")
        .arg("--pwfile")
        .arg(&settings.password_file)
        .arg("--encoding=UTF8")
        .arg("--no-sync");
    run_to_completion(command, settings.timeout, cancel)?;
    Ok(true)
}

/// Runs `command` to completion, killing and reaping it on a deadline or when
/// `cancel` is set, and returning its standard error in the failure message.
fn run_to_completion(
    mut command: Command,
    timeout: Option<Duration>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut text = String::new();
            // A read failure only shortens the message.
            let _read = pipe.read_to_string(&mut text);
            text
        })
    });
    let deadline = timeout.map(|limit| Instant::now() + limit);
    let outcome = loop {
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        if cancel.load(Ordering::Acquire) {
            break Err("was cancelled".to_owned());
        }
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            break Err(format!("timed out after {timeout:?}"));
        }
        std::thread::sleep(POLL_INTERVAL);
    };
    if outcome.is_err() {
        let _killed = child.kill();
        let _reaped = child.wait();
    }
    let text = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    match outcome {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(io::Error::other(format!(
            "initdb --no-sync failed ({status}): {}",
            text.trim()
        ))),
        Err(reason) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("initdb --no-sync {reason}: {}", text.trim()),
        )),
    }
}

/// Ends a running `initdb` when dropped, so cancelling the future that awaits
/// it leaves no process behind.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) { self.0.store(true, Ordering::Release); }
}

/// Sets a cluster up in process: `initdb --no-sync` for a disposable cluster
/// that can have it, then the ordinary `PostgreSQL::setup`.
///
/// The blocking `initdb` runs on the blocking pool so it does not stall the
/// caller's runtime. A blocking task cannot be aborted, so `initdb` itself
/// watches for cancellation: dropping this future (a setup timeout) kills and
/// reaps it rather than leaving it to modify the data directory.
///
/// # Errors
///
/// Returns `initdb`'s failure as an I/O error, or `setup`'s own error.
pub(crate) async fn setup_disposable(
    embedded: &mut postgresql_embedded::PostgreSQL,
) -> Result<(), postgresql_embedded::Error> {
    let settings = embedded.settings().clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let _on_drop = CancelOnDrop(Arc::clone(&cancel));
    tokio::task::spawn_blocking(move || initialize_cancellable(&settings, &cancel))
        .await
        .map_err(|err| postgresql_embedded::Error::IoError(err.to_string()))?
        .map_err(|err| postgresql_embedded::Error::IoError(err.to_string()))?;
    embedded.setup().await
}

/// The installed `initdb` for `settings.version`, if the binaries are in place.
fn installed_initdb(settings: &Settings) -> Option<PathBuf> {
    let initdb = installed_dir(settings)?
        .join("bin")
        .join(format!("initdb{}", std::env::consts::EXE_SUFFIX));
    initdb.is_file().then_some(initdb)
}

/// The installation `PostgreSQL::setup` will use, chosen as it chooses.
///
/// A trusted installation directory is used as it stands. Otherwise the
/// directory itself if it is named for a version the requirement matches, else
/// the highest matching version among its children. A version outside the
/// requirement is never picked, so `initdb` and `setup` cannot disagree on the
/// major version and leave a data directory the server refuses to start.
fn installed_dir(settings: &Settings) -> Option<PathBuf> {
    let root = &settings.installation_dir;
    if settings.trust_installation_dir {
        return Some(root.clone());
    }
    let named_version = |path: &Path| {
        path.file_name()
            .and_then(|name| Version::parse(&name.to_string_lossy()).ok())
            .filter(|version| settings.version.matches(version))
    };
    if named_version(root).is_some() && root.exists() {
        return Some(root.clone());
    }
    std::fs::read_dir(root)
        .ok()?
        .filter_map(|listed| {
            let entry = listed.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let path = entry.path();
            named_version(&path).map(|version| (version, path))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, path)| path)
}

#[cfg(test)]
#[path = "durability_tests.rs"]
mod tests;

//! Serializes cluster startup within one install tree, across processes.
//!
//! Per-cluster data directories (#261) let processes run clusters side by
//! side, but they still share the install tree. In a cold root, two
//! processes would otherwise copy cached binaries into it, or download and
//! extract an archive into it, at the same time, and the extension hook
//! writes into it too. The startup lifecycle therefore holds an exclusive
//! lock on `<install>/.pg-embed-setup.lock` from the cache copy through the
//! server start. Each process still gets its own server; only their
//! startups take turns.
//!
//! The lock is an `fs4` kernel lock on a file `std` opened with
//! `O_CLOEXEC`, so the `postgres` children started under it do not inherit
//! it, and the kernel drops it if the holder dies.

use std::fs::{File, OpenOptions};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Report, eyre};
use fs4::FileExt;
use tracing::debug;

use crate::{
    TestBootstrapSettings,
    error::{BootstrapError, BootstrapResult},
    observability::LOG_TARGET,
};

/// The lock file's name inside the install tree.
pub(crate) const SETUP_LOCK_FILE: &str = ".pg-embed-setup.lock";

/// Holds the install tree's setup lock until dropped.
#[derive(Debug)]
pub(super) struct SetupLock {
    _file: File,
}

impl SetupLock {
    /// Takes the install tree's setup lock, blocking until it is free.
    ///
    /// # Errors
    ///
    /// Returns an error when the install directory is not UTF-8, or the lock
    /// file cannot be created or locked.
    pub(super) fn acquire(bootstrap: &TestBootstrapSettings) -> BootstrapResult<Self> {
        let install_dir =
            Utf8PathBuf::from_path_buf(bootstrap.settings.installation_dir.clone())
                .map_err(|path| eyre!("installation_dir is not UTF-8: {}", path.display()))?;
        std::fs::create_dir_all(&install_dir).map_err(|err| lock_error(&install_dir, err))?;
        let path = install_dir.join(SETUP_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| lock_error(&path, err))?;
        FileExt::lock(&file).map_err(|err| lock_error(&path, err))?;
        debug!(target: LOG_TARGET, lock = %path, "took the install tree's setup lock");
        Ok(Self { _file: file })
    }

    /// Takes the setup lock on the blocking pool, for async callers.
    ///
    /// # Errors
    ///
    /// As [`Self::acquire`], or when the blocking task cannot be joined.
    #[cfg(feature = "async-api")]
    pub(super) async fn acquire_async(bootstrap: &TestBootstrapSettings) -> BootstrapResult<Self> {
        let owned = bootstrap.clone();
        tokio::task::spawn_blocking(move || Self::acquire(&owned))
            .await
            .map_err(|err| {
                BootstrapError::from(Report::new(err).wrap_err("setup lock task failed"))
            })?
    }
}

/// Wraps an I/O failure on the setup lock.
fn lock_error(path: &Utf8PathBuf, err: std::io::Error) -> BootstrapError {
    BootstrapError::from(Report::new(err).wrap_err(format!("cannot take the setup lock at {path}")))
}

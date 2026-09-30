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

use camino::{Utf8Path, Utf8PathBuf};
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
    /// Returns the install root the lock lives in, which is where the
    /// installation directory points before a cache hit moves it to a version
    /// directory. Taking it once keeps every acquisition on one lock file.
    ///
    /// # Errors
    ///
    /// Returns an error when the install directory is not UTF-8.
    pub(super) fn install_root(bootstrap: &TestBootstrapSettings) -> BootstrapResult<Utf8PathBuf> {
        Utf8PathBuf::from_path_buf(bootstrap.settings.installation_dir.clone())
            .map_err(|path| eyre!("installation_dir is not UTF-8: {}", path.display()).into())
    }

    /// Takes the install tree's setup lock, blocking until it is free.
    ///
    /// # Errors
    ///
    /// Returns an error when the install directory is not UTF-8, or the lock
    /// file cannot be created or locked.
    pub(super) fn acquire(bootstrap: &TestBootstrapSettings) -> BootstrapResult<Self> {
        Self::acquire_at(&Self::install_root(bootstrap)?)
    }

    /// Takes the setup lock in `install_dir`, blocking until it is free.
    ///
    /// # Errors
    ///
    /// Returns an error when the lock file cannot be created or locked.
    pub(super) fn acquire_at(install_dir: &Utf8Path) -> BootstrapResult<Self> {
        std::fs::create_dir_all(install_dir).map_err(|err| lock_error(install_dir, err))?;
        let path = install_dir.join(SETUP_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|err| lock_error(&path, err))?;
        let started = std::time::Instant::now();
        FileExt::lock(&file).map_err(|err| lock_error(&path, err))?;
        // The wait is the cost of sharing an install tree, so a caller can tell
        // contention from the cost of the start itself (#289).
        debug!(
            target: LOG_TARGET,
            lock = %path,
            waited_ms = started.elapsed().as_millis(),
            "took the install tree's setup lock"
        );
        Ok(Self { _file: file })
    }

    /// Takes the setup lock in `install_dir` on the blocking pool.
    ///
    /// # Errors
    ///
    /// As [`Self::acquire_at`], or when the blocking task cannot be joined.
    #[cfg(feature = "async-api")]
    pub(super) async fn acquire_async_at(install_dir: Utf8PathBuf) -> BootstrapResult<Self> {
        tokio::task::spawn_blocking(move || Self::acquire_at(&install_dir))
            .await
            .map_err(|err| {
                BootstrapError::from(Report::new(err).wrap_err("setup lock task failed"))
            })?
    }
}

/// Whether a start still holds the install tree's setup lock when the
/// post-setup hook runs.
///
/// The lock is needed while the tree is populated. After a binary-cache hit
/// the tree is complete before `Setup`, whose `initdb` touches only the
/// cluster's own data directory, so the start lets go and other processes'
/// `initdb` run alongside it (#289). The hook writes into the tree, so it
/// takes the lock again when extensions were declared.
#[derive(Debug, Clone, Copy)]
pub(super) enum InstallLock<'a> {
    /// The start holds the lock through the hook, as on a cache miss, where
    /// `Setup` populates the tree.
    Held,
    /// The lock was released after a cache hit; the hook retakes it in `root`.
    Released {
        /// The install root the lock file lives in.
        root: &'a Utf8Path,
    },
}

impl<'a> InstallLock<'a> {
    /// Decides whether the start keeps the setup lock after the cache lookup.
    ///
    /// On a hit the install tree is complete and the lock is released, which
    /// lets other processes' `initdb` run alongside this one; the hook retakes
    /// it if extensions are declared. On a miss `Setup` populates the tree, so
    /// the lock stays held through it. Returns the lock still held, if any,
    /// and how the hook should treat it.
    pub(super) fn after_cache_lookup(
        setup: SetupLock,
        cache_hit: bool,
        install_root: &'a Utf8Path,
    ) -> (Option<SetupLock>, Self) {
        if cache_hit {
            drop(setup);
            (None, Self::Released { root: install_root })
        } else {
            (Some(setup), Self::Held)
        }
    }
}

impl InstallLock<'_> {
    /// Returns the lock for a hook that writes into the tree, or None when the
    /// start already holds it.
    ///
    /// # Errors
    ///
    /// Returns an error when the lock cannot be taken.
    pub(super) fn for_hook(self) -> BootstrapResult<Option<SetupLock>> {
        match self {
            Self::Released { root } => SetupLock::acquire_at(root).map(Some),
            Self::Held => Ok(None),
        }
    }

    /// The async twin of [`Self::for_hook`].
    ///
    /// # Errors
    ///
    /// Returns an error when the lock cannot be taken.
    #[cfg(feature = "async-api")]
    pub(super) async fn for_hook_async(self) -> BootstrapResult<Option<SetupLock>> {
        match self {
            Self::Released { root } => SetupLock::acquire_async_at(root.to_owned()).await.map(Some),
            Self::Held => Ok(None),
        }
    }
}

/// Wraps an I/O failure on the setup lock.
fn lock_error(path: &Utf8Path, err: std::io::Error) -> BootstrapError {
    BootstrapError::from(Report::new(err).wrap_err(format!("cannot take the setup lock at {path}")))
}

#[cfg(test)]
#[path = "setup_lock_tests.rs"]
mod tests;

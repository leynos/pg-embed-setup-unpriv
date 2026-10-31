//! Cross-process file locking for cache coordination.
//!
//! Provides exclusive and shared locks to coordinate binary downloads across
//! parallel test runners. Locking goes through `fs4`, which uses `flock(2)` on
//! Unix and `LockFileEx` on Windows, so both platforms exclude rather than one
//! excluding and the other merely holding a file open.
//!
//! The locks are advisory: they coordinate every process that uses
//! [`CacheLock`], but they cannot stop a process that ignores them from
//! touching the cache.

use std::{
    fs::{File, OpenOptions},
    io,
};

use camino::Utf8Path;
use fs4::FileExt;

/// Subdirectory within the cache for lock files.
///
/// The lock lives beside the cache it guards, so two caches that happen to
/// share a version string do not contend.
const LOCKS_SUBDIR: &str = ".locks";

/// Guard that holds a file lock until dropped.
///
/// The lock is automatically released when the guard goes out of scope.
///
/// # Limits
///
/// - Acquisition blocks until the lock is available; there is no timeout.
/// - Upgrading a shared guard to exclusive is unsupported. Drop the shared guard and acquire an
///   exclusive one instead.
/// - Taking a conflicting guard for the same key on the same thread blocks that thread
///   indefinitely, because nothing can release the first guard whilst it waits.
#[derive(Debug)]
pub struct CacheLock {
    _file: File,
}

impl CacheLock {
    /// Acquires an exclusive lock for a specific version.
    ///
    /// Use exclusive locks when downloading or populating the cache to prevent
    /// concurrent writes.
    ///
    /// # Errors
    ///
    /// Returns an error if the lock file cannot be created or the lock cannot
    /// be acquired.
    ///
    /// # Blocking
    ///
    /// Blocks until every other exclusive and shared guard for this key has
    /// been released. There is no timeout, and a shared guard cannot be
    /// upgraded in place: drop it first.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use camino::Utf8Path;
    /// use pg_embedded_setup_unpriv::cache::CacheLock;
    ///
    /// let cache_dir = Utf8Path::new("/tmp/pg-cache");
    /// let _lock = CacheLock::acquire_exclusive(cache_dir, "17.4.0")?;
    /// // Exclusive access to version 17.4.0 cache entry
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn acquire_exclusive(cache_dir: &Utf8Path, version: &str) -> io::Result<Self> {
        Self::acquire(cache_dir, version, LockType::Exclusive)
    }

    /// Acquires a shared lock for a specific version.
    ///
    /// Use shared locks when reading from the cache to allow concurrent reads
    /// whilst blocking writes.
    ///
    /// # Errors
    ///
    /// Returns an error if the lock file cannot be created or the lock cannot
    /// be acquired.
    ///
    /// # Blocking
    ///
    /// Blocks whilst an exclusive guard for this key is held. Other shared
    /// guards do not block it. There is no timeout, and a shared guard cannot
    /// be upgraded in place: drop it before requesting exclusive access.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use camino::Utf8Path;
    /// use pg_embedded_setup_unpriv::cache::CacheLock;
    ///
    /// let cache_dir = Utf8Path::new("/tmp/pg-cache");
    /// let _lock = CacheLock::acquire_shared(cache_dir, "17.4.0")?;
    /// // Shared access to version 17.4.0 cache entry
    /// # Ok::<(), std::io::Error>(())
    /// ```
    pub fn acquire_shared(cache_dir: &Utf8Path, version: &str) -> io::Result<Self> {
        Self::acquire(cache_dir, version, LockType::Shared)
    }

    /// Opens the version's lock file and takes the requested kernel lock.
    ///
    /// Blocks until the lock is available. The lock is released when the file
    /// handle drops with the guard, so `unlock` is not called explicitly.
    fn acquire(cache_dir: &Utf8Path, version: &str, lock_type: LockType) -> io::Result<Self> {
        validate_version(version)?;
        let locks_dir = cache_dir.join(LOCKS_SUBDIR);
        std::fs::create_dir_all(&locks_dir)?;

        let lock_path = locks_dir.join(format!("{version}.lock"));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;

        // `fs4` names the exclusive lock `lock`, so both calls are spelled
        // out through the trait rather than relying on the shorter name to
        // read as "exclusive" at the call site. The fully qualified form also
        // keeps these calls on `fs4` rather than the inherent `File::lock`
        // and `File::lock_shared` methods stabilised in Rust 1.89, so both
        // platforms share one implementation.
        match lock_type {
            LockType::Exclusive => FileExt::lock(&file)?,
            LockType::Shared => FileExt::lock_shared(&file)?,
        }

        Ok(Self { _file: file })
    }
}

/// Type of lock to acquire.
#[derive(Debug, Clone, Copy)]
enum LockType {
    /// Exclusive lock for writes.
    Exclusive,
    /// Shared lock for reads.
    Shared,
}

/// Validates that a version string is a single path component.
///
/// Rejects versions containing path separators or parent directory references
/// that could escape the cache directory.
fn validate_version(version: &str) -> io::Result<()> {
    use std::path::Component;

    let mut components = std::path::Path::new(version).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "version must be a single path component",
        )),
    }
}

#[cfg(test)]
#[path = "lock_tests.rs"]
mod tests;

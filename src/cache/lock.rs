//! Cross-process file locking for cache coordination.
//!
//! Provides exclusive and shared locks to coordinate binary downloads across
//! parallel test runners. Locking goes through `fs4`, which uses `flock(2)` on
//! Unix and `LockFileEx` on Windows, so both platforms exclude rather than one
//! excluding and the other merely holding a file open.

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
        // read as "exclusive" at the call site.
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
mod tests {
    //! Tests for cache lock acquisition.
    use rstest::{fixture, rstest};
    use tempfile::TempDir;

    use super::*;

    /// Fixture providing a temporary cache directory as a UTF-8 path.
    #[fixture]
    fn cache_fixture() -> io::Result<(TempDir, camino::Utf8PathBuf)> {
        let temp = tempfile::tempdir()?;
        let cache_dir = camino::Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
            .map_err(|path| io::Error::other(format!("non-UTF-8 temp path: {}", path.display())))?;
        Ok((temp, cache_dir))
    }

    /// The lock file lives beside the cache it guards, on every platform.
    ///
    /// Keeping it under the cache directory means two caches that happen to
    /// share a version string do not contend.
    #[rstest]
    #[case::exclusive("17.4.0", true)]
    #[case::shared("16.3.0", false)]
    fn acquire_lock_creates_lock_file(
        cache_fixture: io::Result<(TempDir, camino::Utf8PathBuf)>,
        #[case] version: &str,
        #[case] exclusive: bool,
    ) {
        let (temp, cache_dir) = cache_fixture.expect("cache fixture");
        let _lock = if exclusive {
            CacheLock::acquire_exclusive(&cache_dir, version).expect("acquire lock")
        } else {
            CacheLock::acquire_shared(&cache_dir, version).expect("acquire lock")
        };

        let lock_path = temp
            .path()
            .join(LOCKS_SUBDIR)
            .join(format!("{version}.lock"));
        assert!(lock_path.exists(), "lock file should be created");
    }

    /// An exclusive lock excludes a second caller until the first releases.
    ///
    /// This is the property the type's name promises, and it runs on every
    /// platform rather than only Unix: before `fs4` the non-Unix arm held the
    /// file open without taking a kernel lock, so two callers could enter the
    /// critical section together, observe the same cache miss, and download
    /// the same archive independently.
    ///
    /// The second acquisition is attempted from another thread, because an
    /// exclusive lock blocks: the test asserts it does not complete while the
    /// first guard lives, then that it completes once the guard drops.
    #[rstest]
    fn an_exclusive_lock_excludes_a_second_caller(
        cache_fixture: io::Result<(TempDir, camino::Utf8PathBuf)>,
    ) {
        use std::{
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };

        let (_temp, cache_dir) = cache_fixture.expect("cache fixture");
        let held = CacheLock::acquire_exclusive(&cache_dir, "17.4.0").expect("first lock");

        let (tx, rx) = mpsc::channel();
        let contender_dir = cache_dir.clone();
        let contender = thread::spawn(move || {
            let lock = CacheLock::acquire_exclusive(&contender_dir, "17.4.0");
            tx.send(Instant::now()).ok();
            lock
        });

        // While the first guard lives the contender must not get through.
        assert!(
            rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "a second exclusive lock must not be granted while the first is held"
        );

        drop(held);
        rx.recv_timeout(Duration::from_secs(10))
            .expect("the contender must acquire once the first lock is released");
        contender
            .join()
            .expect("contender thread")
            .expect("second lock");
    }

    #[rstest]
    fn multiple_shared_locks_can_coexist(
        cache_fixture: io::Result<(TempDir, camino::Utf8PathBuf)>,
    ) {
        let (_temp, cache_dir) = cache_fixture.expect("cache fixture");

        let lock1 = CacheLock::acquire_shared(&cache_dir, "17.4.0").expect("acquire lock 1");
        let lock2 = CacheLock::acquire_shared(&cache_dir, "17.4.0").expect("acquire lock 2");

        // Both locks should be held successfully
        drop(lock1);
        drop(lock2);
    }

    #[rstest]
    fn different_versions_have_separate_locks(
        cache_fixture: io::Result<(TempDir, camino::Utf8PathBuf)>,
    ) {
        let (_temp, cache_dir) = cache_fixture.expect("cache fixture");

        let lock1 = CacheLock::acquire_exclusive(&cache_dir, "17.4.0").expect("acquire lock 1");
        let lock2 = CacheLock::acquire_exclusive(&cache_dir, "16.3.0").expect("acquire lock 2");

        // Different versions should not block each other
        drop(lock1);
        drop(lock2);
    }

    #[rstest]
    #[case::parent_dir_exclusive("..")]
    #[case::parent_dir_shared("..")]
    #[case::path_separator_exclusive("foo/bar")]
    #[case::path_separator_shared("foo/bar")]
    #[case::parent_in_path_exclusive("../17.4.0")]
    #[case::absolute_path_exclusive("/etc/passwd")]
    fn acquire_rejects_invalid_version_strings(
        cache_fixture: io::Result<(TempDir, camino::Utf8PathBuf)>,
        #[case] invalid_version: &str,
    ) {
        let (_temp, cache_dir) = cache_fixture.expect("cache fixture");

        let exclusive_err = CacheLock::acquire_exclusive(&cache_dir, invalid_version)
            .expect_err("acquire_exclusive should reject invalid version");
        assert_eq!(
            exclusive_err.kind(),
            io::ErrorKind::InvalidInput,
            "error kind should be InvalidInput for: {invalid_version}"
        );

        let shared_err = CacheLock::acquire_shared(&cache_dir, invalid_version)
            .expect_err("acquire_shared should reject invalid version");
        assert_eq!(
            shared_err.kind(),
            io::ErrorKind::InvalidInput,
            "error kind should be InvalidInput for: {invalid_version}"
        );
    }
}

//! Cross-process file locking for cache coordination.
//!
//! Provides exclusive and shared locks to coordinate binary downloads across
//! parallel test runners. On Unix systems, uses `flock(2)` for advisory locking.
//! On non-Unix platforms no kernel lock is taken, but the same lock file is
//! created and held under the cache directory, so callers see one layout.
//! Issue #232 tracks locking there too.

#[cfg(unix)]
use std::os::unix::io::AsRawFd;
use std::{
    fs::{File, OpenOptions},
    io,
};

use camino::Utf8Path;

/// Subdirectory within the cache for lock files.
///
/// Both platforms use it: Unix takes a kernel lock on the file, and non-Unix
/// merely holds it, but the path is the same so a cache is self-contained.
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

    /// Acquires a lock with the specified type.
    #[cfg(unix)]
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

        let flock_arg = match lock_type {
            LockType::Exclusive => libc::LOCK_EX,
            LockType::Shared => libc::LOCK_SH,
        };

        // SAFETY: The file descriptor obtained from `file.as_raw_fd()` is valid
        // because `file` was opened via `OpenOptions::open` and remains owned by
        // this scope until after the `flock` call completes. No other code moves
        // or closes the descriptor while this block runs.
        //
        // Retry loop handles EINTR, which can occur when the process receives a
        // signal while blocked on flock.
        loop {
            let result = unsafe { libc::flock(file.as_raw_fd(), flock_arg) };
            if result == 0 {
                break;
            }
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
            // EINTR: signal interrupted syscall, retry.
        }

        Ok(Self { _file: file })
    }

    /// Lock acquisition on non-Unix platforms, which holds the file but takes
    /// no kernel lock.
    ///
    /// There is no `flock` here, so this does not serialize anything: it
    /// creates and holds the same lock file the Unix arm uses, under the cache
    /// directory it guards, so the two platforms at least agree on where the
    /// file lives and callers see the same failure when the cache is
    /// unwritable. Issue #232 tracks making the two arms equivalent with a
    /// real exclusive lock.
    ///
    /// The previous implementation put the file in the process-wide temp
    /// directory keyed only by `version`, then deleted it immediately. Two
    /// callers using the same version collided there whatever cache they were
    /// working on, and on Windows a delete leaves the name unopenable while a
    /// handle remains, so the second caller failed with "Access is denied"
    /// rather than proceeding.
    #[cfg(not(unix))]
    fn acquire(cache_dir: &Utf8Path, version: &str, _lock_type: LockType) -> io::Result<Self> {
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
    /// Windows takes no kernel lock, but it creates the same file in the same
    /// place, so a caller cannot collide with an unrelated cache that happens
    /// to use the same version string.
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

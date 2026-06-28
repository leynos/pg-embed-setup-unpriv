//! Serialization guard shared by behavioural test suites.
//!
//! Acquire this guard **before** calling environment helpers such as
//! [`crate::test_support::with_scoped_env`] to maintain the lock-ordering
//! contract used throughout the integration scenarios (process lock, scenario
//! mutex, then environment mutex). Following this order prevents deadlocks when
//! multiple suites mutate process-wide state.

use rstest::fixture;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

#[cfg(all(not(unix), windows))]
use std::ffi::c_void;
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::io::AsRawFd;
#[cfg(not(unix))]
use std::time::{Duration, Instant};

static SCENARIO_MUTEX: std::sync::LazyLock<Mutex<()>> = std::sync::LazyLock::new(|| Mutex::new(()));

#[cfg(unix)]
type ProcessLock = std::fs::File;

#[cfg(not(unix))]
#[derive(Debug)]
struct ProcessLock {
    path: PathBuf,
    owner_path: PathBuf,
}

#[cfg(not(unix))]
const PROCESS_LOCK_OWNER_GRACE: Duration = Duration::from_secs(2);

#[cfg(not(unix))]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ProcessLockState {
    Active,
    PendingOwner(ProcessLockOwnerIssue),
    Stale(ProcessLockOwnerIssue),
}

#[cfg(not(unix))]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ProcessLockOwnerIssue {
    Missing,
    Unreadable(std::io::ErrorKind),
    Malformed,
    Exited(u32),
}

#[cfg(not(unix))]
impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _unused = std::fs::remove_file(&self.owner_path);
        let _unused = std::fs::remove_dir(&self.path);
    }
}

#[cfg(all(not(unix), windows))]
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
#[cfg(all(not(unix), windows))]
const STILL_ACTIVE: u32 = 259;

#[cfg(all(not(unix), windows))]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
    fn GetExitCodeProcess(process: *mut c_void, exit_code: *mut u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[derive(Debug)]
#[must_use = "Hold this guard for the duration of the serialized scenario"]
pub struct ScenarioSerialGuard {
    _guard: MutexGuard<'static, ()>,
    _lock_file: ProcessLock,
}

#[derive(Debug)]
#[must_use = "Hold this guard for the duration of the serialized scenario"]
pub struct ScenarioLocalGuard {
    _guard: MutexGuard<'static, ()>,
}

/// Provides a serialization guard for behavioural test scenarios.
///
/// Acquires a global mutex to ensure that scenarios relying on shared state
/// (such as process environment variables or singleton resources) execute
/// serially, preventing cross-test interference. A cross-process lock is also
/// acquired so independent test binaries coordinate access to the shared
/// `PostgreSQL` cache and installation directories.
///
/// # Behaviour
///
/// - Acquires the global `SCENARIO_MUTEX` and wraps the guard.
/// - If the mutex is poisoned (a previous test panicked whilst holding the lock),
///   the poison is cleared and execution continues.
/// - The guard is automatically released when dropped at the end of the test.
///
/// # Examples
///
/// ```rust,ignore
/// use rstest::rstest;
/// use tests::support::serial::{serial_guard, ScenarioSerialGuard};
///
/// #[rstest]
/// fn my_scenario(serial_guard: ScenarioSerialGuard) {
///     let _guard = serial_guard;
///     // Test code that mutates shared state
/// }
/// ```
#[fixture]
pub fn serial_guard() -> ScenarioSerialGuard {
    let lock_file = acquire_process_lock();
    let guard = acquire_scenario_guard();
    ScenarioSerialGuard {
        _guard: guard,
        _lock_file: lock_file,
    }
}

/// Provides a local-only serialization guard for behavioural scenarios.
///
/// Use this guard when a test only needs in-process serialization (for example,
/// it uses sandboxed directories) and does not require a cross-process file
/// lock.
///
/// # Behaviour
///
/// - Acquires the global `SCENARIO_MUTEX` and wraps the guard.
/// - If the mutex is poisoned (a previous test panicked whilst holding the lock),
///   the poison is cleared and execution continues.
/// - The guard is automatically released when dropped at the end of the test.
///
/// # Examples
///
/// ```rust,ignore
/// use rstest::rstest;
/// use tests::support::serial::{local_serial_guard, ScenarioLocalGuard};
///
/// #[rstest]
/// fn my_scenario(local_serial_guard: ScenarioLocalGuard) {
///     let _guard = local_serial_guard;
///     // Test code that uses sandboxed directories
/// }
/// ```
#[fixture]
pub fn local_serial_guard() -> ScenarioLocalGuard {
    let guard = acquire_scenario_guard();
    ScenarioLocalGuard { _guard: guard }
}

fn acquire_scenario_guard() -> MutexGuard<'static, ()> {
    SCENARIO_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(unix)]
fn acquire_process_lock() -> ProcessLock {
    let target_dir =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
    std::fs::create_dir_all(&target_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create target dir for scenario lock at {}: {err}",
            target_dir.display()
        );
    });
    let lock_path = target_dir.join("pg-embed-setup-unpriv.serial.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|err| {
            panic!(
                "failed to open scenario lock file at {}: {err}",
                lock_path.display()
            );
        });
    // SAFETY: The file descriptor obtained from `lock_file.as_raw_fd()` is valid
    // because `lock_file` was opened via `OpenOptions::open` and remains owned by
    // this scope until after the `flock` call completes. No other code moves or
    // closes the descriptor while this block runs. The `libc::flock` syscall
    // operates on the OS-level file descriptor and does not access Rust memory,
    // so there are no data-race concerns from Rust's perspective.
    let result = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
    assert!(
        result == 0,
        "failed to acquire scenario lock at {}: {}",
        lock_path.display(),
        std::io::Error::last_os_error()
    );
    lock_file
}

#[cfg(not(unix))]
fn acquire_process_lock() -> ProcessLock {
    let lock_path = scenario_lock_path();
    let deadline = Instant::now() + Duration::from_secs(120);

    loop {
        if let Some(lock) = try_acquire_process_lock_once(&lock_path, deadline) {
            return lock;
        }
    }
}

#[cfg(not(unix))]
fn scenario_lock_path() -> PathBuf {
    let target_dir =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| PathBuf::from("target"), PathBuf::from);
    std::fs::create_dir_all(&target_dir).unwrap_or_else(|err| {
        panic!(
            "failed to create target dir for scenario lock at {}: {err}",
            target_dir.display()
        );
    });
    target_dir.join("pg-embed-setup-unpriv.serial.lockdir")
}

#[cfg(not(unix))]
fn try_acquire_process_lock_once(
    lock_path: &std::path::Path,
    deadline: Instant,
) -> Option<ProcessLock> {
    match std::fs::create_dir(lock_path) {
        Ok(()) => Some(write_process_lock_owner(lock_path)),
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            handle_contended_process_lock(lock_path, deadline);
            None
        }
        Err(err) => {
            panic!(
                "failed to acquire scenario lock at {}: {err}",
                lock_path.display()
            );
        }
    }
}

#[cfg(not(unix))]
fn handle_contended_process_lock(lock_path: &std::path::Path, deadline: Instant) {
    match process_lock_state(lock_path) {
        ProcessLockState::Stale(_reason) => {
            let _unused = std::fs::remove_dir_all(lock_path);
            return;
        }
        ProcessLockState::Active | ProcessLockState::PendingOwner(_) => {}
    }

    assert!(
        Instant::now() < deadline,
        "timed out waiting to acquire scenario lock at {}",
        lock_path.display()
    );
    std::thread::sleep(Duration::from_millis(50));
}

#[cfg(not(unix))]
fn write_process_lock_owner(lock_path: &std::path::Path) -> ProcessLock {
    let owner_path = process_lock_owner_path(lock_path);
    std::fs::write(&owner_path, process_lock_owner_contents()).unwrap_or_else(|err| {
        let _unused = std::fs::remove_dir(lock_path);
        panic!(
            "failed to record scenario lock owner at {}: {err}",
            owner_path.display()
        );
    });
    ProcessLock {
        path: lock_path.to_path_buf(),
        owner_path,
    }
}

#[cfg(not(unix))]
fn process_lock_owner_path(lock_path: &std::path::Path) -> PathBuf {
    lock_path.join("owner")
}

#[cfg(not(unix))]
fn process_lock_owner_contents() -> String {
    format!("pid={}\n", std::process::id())
}

#[cfg(not(unix))]
fn process_lock_state(lock_path: &std::path::Path) -> ProcessLockState {
    let owner_path = process_lock_owner_path(lock_path);
    let owner = match std::fs::read_to_string(&owner_path) {
        Ok(owner) => owner,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return process_lock_pending_or_stale(lock_path, ProcessLockOwnerIssue::Missing);
        }
        Err(err) => {
            return process_lock_pending_or_stale(
                lock_path,
                ProcessLockOwnerIssue::Unreadable(err.kind()),
            );
        }
    };
    let Some(pid) = parse_lock_owner_pid(&owner) else {
        return process_lock_pending_or_stale(lock_path, ProcessLockOwnerIssue::Malformed);
    };
    if owner_process_is_running(pid) {
        ProcessLockState::Active
    } else {
        ProcessLockState::Stale(ProcessLockOwnerIssue::Exited(pid))
    }
}

#[cfg(not(unix))]
fn process_lock_pending_or_stale(
    lock_path: &std::path::Path,
    reason: ProcessLockOwnerIssue,
) -> ProcessLockState {
    if process_lock_is_within_owner_grace(lock_path) {
        ProcessLockState::PendingOwner(reason)
    } else {
        ProcessLockState::Stale(reason)
    }
}

#[cfg(not(unix))]
fn process_lock_is_within_owner_grace(lock_path: &std::path::Path) -> bool {
    let Ok(metadata) = std::fs::metadata(lock_path) else {
        return false;
    };
    let Ok(modified_at) = metadata.modified() else {
        return false;
    };
    modified_at
        .elapsed()
        .is_ok_and(|age| age <= PROCESS_LOCK_OWNER_GRACE)
}

#[cfg(not(unix))]
fn parse_lock_owner_pid(owner: &str) -> Option<u32> {
    owner.trim().strip_prefix("pid=")?.parse().ok()
}

#[cfg(all(not(unix), windows))]
fn owner_process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    // SAFETY: `OpenProcess` receives a concrete process id from the lock owner
    // file. Handle inheritance is disabled, and a null return is treated as an
    // inactive owner.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return false;
    }

    let mut exit_code = 0_u32;
    // SAFETY: `handle` is a non-null process handle owned by this function,
    // and `exit_code` is valid writable storage for the duration of the call.
    let is_running = unsafe { GetExitCodeProcess(handle, std::ptr::addr_of_mut!(exit_code)) } != 0
        && exit_code == STILL_ACTIVE;
    // SAFETY: `handle` is owned by this function and is closed exactly once.
    unsafe {
        CloseHandle(handle);
    }
    is_running
}

#[cfg(all(not(unix), not(windows)))]
fn owner_process_is_running(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    //! Unit tests for scenario serialization guards.

    use rstest::rstest;

    use super::*;

    #[test]
    fn serial_guard_is_not_reentrant() {
        let guard = serial_guard();
        assert!(SCENARIO_MUTEX.try_lock().is_err());
        drop(guard);
        let reacquired = SCENARIO_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        drop(reacquired);
    }

    #[cfg(unix)]
    #[rstest]
    #[expect(
        clippy::let_underscore_must_use,
        reason = "best-effort cleanup where errors are intentionally ignored"
    )]
    fn acquire_process_lock_places_lock_file_in_cargo_target_dir(
        serial_guard: ScenarioSerialGuard,
    ) {
        use std::ffi::OsString;
        use std::{env, fs};

        use pg_embedded_setup_unpriv::test_support::scoped_env;

        let _guard = serial_guard;

        let tmp_dir = env::temp_dir().join("pg_scenario_lock_test");
        // Best-effort cleanup of any previous test run; errors are expected if
        // the directory does not exist.
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir)
            .expect("failed to create temporary CARGO_TARGET_DIR for acquire_process_lock test");

        // Set CARGO_TARGET_DIR to our test directory using the shared scoped_env
        // helper, which restores the original value when the guard is dropped.
        let _env_guard = scoped_env(vec![(
            OsString::from("CARGO_TARGET_DIR"),
            Some(tmp_dir.clone().into_os_string()),
        )]);
        let _lock = acquire_process_lock();

        let entries: Vec<_> = fs::read_dir(&tmp_dir)
            .expect("failed to read temporary CARGO_TARGET_DIR for acquire_process_lock test")
            .collect();
        assert!(
            !entries.is_empty(),
            "expected acquire_process_lock to create a lock file in {tmp_dir:?}, but directory was empty"
        );

        // Best-effort cleanup; errors are non-fatal in test teardown.
        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[cfg(not(unix))]
    #[rstest]
    #[expect(
        clippy::let_underscore_must_use,
        reason = "best-effort cleanup where errors are intentionally ignored"
    )]
    fn acquire_process_lock_places_lockdir_in_cargo_target_dir(serial_guard: ScenarioSerialGuard) {
        use std::ffi::OsString;
        use std::{env, fs};

        use pg_embedded_setup_unpriv::test_support::scoped_env;

        let _guard = serial_guard;

        let tmp_dir = env::temp_dir().join("pg_scenario_lockdir_test");
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&tmp_dir)
            .expect("failed to create temporary CARGO_TARGET_DIR for acquire_process_lock test");

        let _env_guard = scoped_env(vec![(
            OsString::from("CARGO_TARGET_DIR"),
            Some(tmp_dir.clone().into_os_string()),
        )]);
        {
            let _lock = acquire_process_lock();
            let lock_path = tmp_dir.join("pg-embed-setup-unpriv.serial.lockdir");
            assert!(
                lock_path.is_dir(),
                "expected acquire_process_lock to create lockdir at {lock_path:?}"
            );
            assert!(
                process_lock_owner_path(&lock_path).is_file(),
                "expected acquire_process_lock to record a lock owner in {lock_path:?}"
            );
        }

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[cfg(not(unix))]
    #[rstest]
    #[expect(
        clippy::let_underscore_must_use,
        reason = "best-effort cleanup where errors are intentionally ignored"
    )]
    fn partial_process_lock_owner_respects_owner_grace(serial_guard: ScenarioSerialGuard) {
        use std::{env, fs};

        let _guard = serial_guard;

        let tmp_dir = env::temp_dir().join("pg_scenario_partial_lock_owner_test");
        let lock_path = tmp_dir.join("pg-embed-setup-unpriv.serial.lockdir");
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&lock_path)
            .expect("failed to create lock directory for malformed owner test");
        fs::write(process_lock_owner_path(&lock_path), "pid=")
            .expect("failed to write malformed process lock owner");

        assert_eq!(
            process_lock_state(&lock_path),
            ProcessLockState::PendingOwner(ProcessLockOwnerIssue::Malformed),
            "partial process lock owners inside the grace window must remain pending"
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }

    #[cfg(not(unix))]
    #[rstest]
    #[case("")]
    #[case("pid=")]
    #[expect(
        clippy::let_underscore_must_use,
        reason = "best-effort cleanup where errors are intentionally ignored"
    )]
    fn malformed_process_lock_owner_becomes_stale_after_grace(
        serial_guard: ScenarioSerialGuard,
        #[case] owner: &str,
    ) {
        use std::time::{Duration, SystemTime};
        use std::{env, fs};

        let _guard = serial_guard;

        let tmp_dir = env::temp_dir().join("pg_scenario_stale_lock_owner_test");
        let lock_path = tmp_dir.join("pg-embed-setup-unpriv.serial.lockdir");
        let _ = fs::remove_dir_all(&tmp_dir);
        fs::create_dir_all(&lock_path)
            .expect("failed to create lock directory for stale owner test");
        fs::write(process_lock_owner_path(&lock_path), owner)
            .expect("failed to write malformed process lock owner");
        let stale_time = SystemTime::now() - PROCESS_LOCK_OWNER_GRACE - Duration::from_secs(1);
        let stale_file_time = filetime::FileTime::from_system_time(stale_time);
        filetime::set_file_mtime(&lock_path, stale_file_time)
            .expect("failed to set stale lock directory mtime");

        assert_eq!(
            process_lock_state(&lock_path),
            ProcessLockState::Stale(ProcessLockOwnerIssue::Malformed)
        );

        let _ = fs::remove_dir_all(&tmp_dir);
    }
}

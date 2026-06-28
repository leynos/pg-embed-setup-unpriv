//! Non-Unix process-lock implementation for behavioural test serialization.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{fs::OpenOptions, io::Write};

#[cfg(windows)]
use std::ffi::c_void;

pub(super) const PROCESS_LOCK_OWNER_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(super) struct ProcessLock {
    path: PathBuf,
    owner_path: PathBuf,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ProcessLockState {
    Active,
    PendingOwner(ProcessLockOwnerIssue),
    Stale(ProcessLockOwnerIssue),
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ProcessLockOwnerIssue {
    Missing,
    Unreadable(std::io::ErrorKind),
    Malformed,
    Exited(u32),
}

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _unused = std::fs::remove_file(&self.owner_path);
        let _unused = std::fs::remove_dir(&self.path);
    }
}

#[cfg(windows)]
const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
#[cfg(windows)]
const STILL_ACTIVE: u32 = 259;

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
    fn GetExitCodeProcess(process: *mut c_void, exit_code: *mut u32) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

pub(super) fn acquire_process_lock() -> ProcessLock {
    let lock_path = scenario_lock_path();
    let deadline = Instant::now() + Duration::from_secs(120);

    loop {
        if let Some(lock) = try_acquire_process_lock_once(&lock_path, deadline) {
            return lock;
        }
    }
}

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

fn try_acquire_process_lock_once(lock_path: &Path, deadline: Instant) -> Option<ProcessLock> {
    match std::fs::create_dir(lock_path) {
        Ok(()) => write_process_lock_owner(lock_path),
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

fn handle_contended_process_lock(lock_path: &Path, deadline: Instant) {
    match process_lock_state(lock_path) {
        ProcessLockState::Stale(_reason) => {
            if std::fs::remove_dir_all(lock_path).is_ok() {
                return;
            }
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

fn write_process_lock_owner(lock_path: &Path) -> Option<ProcessLock> {
    let owner_path = process_lock_owner_path(lock_path);
    let mut owner_file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&owner_path)
    {
        Ok(owner_file) => owner_file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return None,
        Err(err) => {
            let _unused = std::fs::remove_dir(lock_path);
            panic!(
                "failed to create scenario lock owner at {}: {err}",
                owner_path.display()
            );
        }
    };
    owner_file
        .write_all(process_lock_owner_contents().as_bytes())
        .unwrap_or_else(|err| {
            let _unused = std::fs::remove_file(&owner_path);
            let _unused = std::fs::remove_dir(lock_path);
            panic!(
                "failed to record scenario lock owner at {}: {err}",
                owner_path.display()
            );
        });
    Some(ProcessLock {
        path: lock_path.to_path_buf(),
        owner_path,
    })
}

fn process_lock_owner_path(lock_path: &Path) -> PathBuf {
    lock_path.join("owner")
}

fn process_lock_owner_contents() -> String {
    format!("pid={}\n", std::process::id())
}

fn process_lock_state(lock_path: &Path) -> ProcessLockState {
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

fn process_lock_pending_or_stale(
    lock_path: &Path,
    reason: ProcessLockOwnerIssue,
) -> ProcessLockState {
    if process_lock_is_within_owner_grace(lock_path) {
        ProcessLockState::PendingOwner(reason)
    } else {
        ProcessLockState::Stale(reason)
    }
}

fn process_lock_is_within_owner_grace(lock_path: &Path) -> bool {
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

fn parse_lock_owner_pid(owner: &str) -> Option<u32> {
    owner.trim().strip_prefix("pid=")?.parse().ok()
}

#[cfg(windows)]
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

#[cfg(not(windows))]
fn owner_process_is_running(_pid: u32) -> bool {
    false
}

#[cfg(test)]
mod tests {
    //! Unit tests for non-Unix scenario lock ownership.

    use rstest::rstest;

    use super::super::{ScenarioSerialGuard, serial_guard};
    use super::*;

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

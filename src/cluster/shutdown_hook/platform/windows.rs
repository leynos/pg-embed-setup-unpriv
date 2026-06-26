//! Windows process controls for the shutdown hook.

use std::{ffi::c_void, ptr::NonNull};

mod identity;
mod job;

pub(in crate::cluster::shutdown_hook) use self::identity::{
    PostmasterProcess, parse_postmaster_process,
};
use self::identity::{image_file_name_is_postgres, process_matches_postmaster};
use self::job::JobHandle;

/// Platform-specific process identifier stored in `postmaster.pid`.
pub(in crate::cluster::shutdown_hook) type PostmasterPid = u32;

const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
const PROCESS_SET_QUOTA: u32 = 0x0100;
const PROCESS_TERMINATE: u32 = 0x0001;
const SYNCHRONIZE: u32 = 0x0010_0000;
const STILL_ACTIVE: u32 = 259;
const TERMINATE_EXIT_CODE: u32 = 1;
const TERMINATION_WAIT_MS: u32 = 5_000;
const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;
const MAX_PATH: usize = 260;
const INVALID_HANDLE_VALUE: *mut c_void = -1_isize as *mut c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
    fn GetExitCodeProcess(process: *mut c_void, exit_code: *mut u32) -> i32;
    fn TerminateProcess(process: *mut c_void, exit_code: u32) -> i32;
    fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
    fn CreateToolhelp32Snapshot(flags: u32, process_id: u32) -> *mut c_void;
    fn Process32FirstW(snapshot: *mut c_void, process_entry: *mut ProcessEntry32W) -> i32;
    fn Process32NextW(snapshot: *mut c_void, process_entry: *mut ProcessEntry32W) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// Parses a strictly positive postmaster PID.
pub(in crate::cluster::shutdown_hook) fn parse_pid(raw: &str) -> Option<PostmasterPid> {
    let pid = raw.trim().parse::<PostmasterPid>().ok()?;
    (pid > 0).then_some(pid)
}

/// Retains Windows resources that should be closed only when the process exits.
pub(in crate::cluster::shutdown_hook) struct ProcessExitFailsafe {
    _job: Option<JobHandle>,
}

/// Assigns the postmaster tree to an OS job that kills members on close.
pub(in crate::cluster::shutdown_hook) fn prepare_process_exit_failsafe(
    process: Option<PostmasterProcess>,
) -> ProcessExitFailsafe {
    let job = process.and_then(JobHandle::create_for_process_tree);
    ProcessExitFailsafe { _job: job }
}

/// Requests `PostgreSQL` shutdown.
///
/// Windows has no POSIX-style signal that can be sent safely from the process
/// exit hook. Terminate the process tree immediately so a deliberately leaked
/// test cluster cannot survive the exiting test binary.
pub(in crate::cluster::shutdown_hook) fn request_shutdown(process: PostmasterProcess) {
    terminate_process_tree(process);
}

/// Forces `PostgreSQL` shutdown after the graceful timeout.
pub(in crate::cluster::shutdown_hook) fn force_shutdown(process: PostmasterProcess) {
    terminate_process_tree(process);
}

/// Returns `true` when the process exists and has not exited.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub(in crate::cluster::shutdown_hook) fn process_is_running_for_platform(
    pid: PostmasterPid,
) -> bool {
    if pid == 0 {
        return false;
    }

    ProcessHandle::open_query(pid).is_some_and(|process| process.is_active())
}

/// Returns `true` when the postmaster identity still matches a live process.
pub(in crate::cluster::shutdown_hook) fn postmaster_process_is_running(
    process: PostmasterProcess,
) -> bool {
    ProcessHandle::open_query(process.pid())
        .is_some_and(|handle| handle.matches_postmaster(process))
}

#[repr(C)]
struct ProcessEntry32W {
    size: u32,
    _usage_count: u32,
    process_id: u32,
    _default_heap_id: usize,
    _module_id: u32,
    _thread_count: u32,
    parent_process_id: u32,
    _priority_class_base: i32,
    _flags: u32,
    _exe_file: [u16; MAX_PATH],
}

impl ProcessEntry32W {
    fn new() -> Option<Self> {
        Some(Self {
            size: u32::try_from(std::mem::size_of::<Self>()).ok()?,
            _usage_count: 0,
            process_id: 0,
            _default_heap_id: 0,
            _module_id: 0,
            _thread_count: 0,
            parent_process_id: 0,
            _priority_class_base: 0,
            _flags: 0,
            _exe_file: [0; MAX_PATH],
        })
    }
}

#[derive(Clone, Copy)]
struct ProcessEntry {
    process_id: PostmasterPid,
    parent_process_id: PostmasterPid,
}

struct ProcessHandle(NonNull<c_void>);

impl ProcessHandle {
    fn open_query(pid: PostmasterPid) -> Option<Self> {
        // SAFETY: `OpenProcess` is called with query-only access, handle
        // inheritance disabled, and a concrete process id read from
        // `postmaster.pid`. A null return is handled as absence.
        let raw_handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        NonNull::new(raw_handle).map(Self)
    }

    fn open_terminate(pid: PostmasterPid) -> Option<Self> {
        let access = PROCESS_TERMINATE | SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION;
        Self::open_with_access(pid, access)
    }

    fn open_assign_to_job(pid: PostmasterPid) -> Option<Self> {
        let access = PROCESS_SET_QUOTA | PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION;
        Self::open_with_access(pid, access)
    }

    fn open_with_access(pid: PostmasterPid, access: u32) -> Option<Self> {
        // SAFETY:
        // - `OpenProcess` receives a concrete process id read from
        //   `postmaster.pid` or from the process snapshot.
        // - handle inheritance is disabled.
        // - a null return is handled as failure and no handle is retained.
        let raw_handle = unsafe { OpenProcess(access, 0, pid) };
        NonNull::new(raw_handle).map(Self)
    }

    fn raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    fn is_active(&self) -> bool {
        let mut exit_code = 0_u32;
        let exit_code_ptr = std::ptr::addr_of_mut!(exit_code);
        // SAFETY:
        // - `self.0` is a non-null process handle owned by this wrapper.
        // - `exit_code_ptr` points to valid writable storage for the duration
        //   of the call.
        let succeeded = unsafe { GetExitCodeProcess(self.raw(), exit_code_ptr) };
        succeeded != 0 && exit_code == STILL_ACTIVE
    }

    pub(super) fn matches_postmaster(&self, expected: PostmasterProcess) -> bool {
        self.is_active() && process_matches_postmaster(self.raw(), expected)
    }

    fn is_active_postgres(&self) -> bool {
        self.is_active() && image_file_name_is_postgres(self.raw())
    }

    fn terminate(&self) {
        // SAFETY:
        // - `self.0` is a non-null process handle opened with
        //   `PROCESS_TERMINATE | SYNCHRONIZE`.
        // - the callee does not retain pointers and no Rust references cross
        //   the FFI boundary.
        unsafe {
            TerminateProcess(self.raw(), TERMINATE_EXIT_CODE);
            WaitForSingleObject(self.raw(), TERMINATION_WAIT_MS);
        }
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: `ProcessHandle` owns the non-null handle returned by
        // `OpenProcess`; closing it exactly once in `Drop` releases the OS
        // resource. The return value cannot be acted on during best-effort
        // cleanup.
        unsafe {
            CloseHandle(self.raw());
        }
    }
}

struct SnapshotHandle(NonNull<c_void>);

impl SnapshotHandle {
    fn capture_processes() -> Option<Self> {
        // SAFETY:
        // - `CreateToolhelp32Snapshot` receives the process-snapshot flag and
        //   process id `0`, which requests all processes.
        // - invalid and null handles are rejected before constructing the
        //   owning wrapper.
        let raw_handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
        if raw_handle == INVALID_HANDLE_VALUE {
            return None;
        }
        NonNull::new(raw_handle).map(Self)
    }

    fn process_entries(&self) -> Vec<ProcessEntry> {
        let Some(mut raw_entry) = ProcessEntry32W::new() else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        let raw_entry_ptr = std::ptr::addr_of_mut!(raw_entry);

        // SAFETY:
        // - `self.0` is a non-null snapshot handle owned by this wrapper.
        // - `raw_entry_ptr` points to initialized writable storage and its
        //   `size` field is set as required by the Toolhelp API.
        let mut has_entry = unsafe { Process32FirstW(self.0.as_ptr(), raw_entry_ptr) } != 0;
        while has_entry {
            entries.push(ProcessEntry {
                process_id: raw_entry.process_id,
                parent_process_id: raw_entry.parent_process_id,
            });

            // SAFETY:
            // - same handle and writable `PROCESSENTRY32W` buffer invariants as
            //   for `Process32FirstW` above.
            has_entry = unsafe { Process32NextW(self.0.as_ptr(), raw_entry_ptr) } != 0;
        }

        entries
    }
}

impl Drop for SnapshotHandle {
    fn drop(&mut self) {
        // SAFETY: `SnapshotHandle` owns the non-null handle returned by
        // `CreateToolhelp32Snapshot`; closing it exactly once in `Drop`
        // releases the OS resource.
        unsafe {
            CloseHandle(self.0.as_ptr());
        }
    }
}

fn terminate_process_tree(process: PostmasterProcess) {
    let Some(root_process) = ProcessHandle::open_terminate(process.pid()) else {
        return;
    };
    if !root_process.matches_postmaster(process) {
        return;
    }

    let mut tree = process_tree(process.pid());
    tree.reverse();

    for process_id in tree {
        if process_id == process.pid() {
            root_process.terminate();
        } else {
            terminate_process(process_id);
        }
    }
}

fn process_tree(root: PostmasterPid) -> Vec<PostmasterPid> {
    let Some(snapshot) = SnapshotHandle::capture_processes() else {
        return vec![root];
    };
    collect_process_tree(root, &snapshot.process_entries())
}

fn collect_process_tree(root: PostmasterPid, entries: &[ProcessEntry]) -> Vec<PostmasterPid> {
    let mut tree = vec![root];
    let mut found_child = true;

    while found_child {
        found_child = false;
        for entry in entries {
            if tree.contains(&entry.parent_process_id) && !tree.contains(&entry.process_id) {
                tree.push(entry.process_id);
                found_child = true;
            }
        }
    }

    tree
}

fn terminate_process(pid: PostmasterPid) {
    if let Some(process) = ProcessHandle::open_terminate(pid) {
        if process.is_active_postgres() {
            process.terminate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcessEntry, collect_process_tree};

    #[test]
    fn collect_process_tree_returns_root_when_no_descendants() {
        let entries = [
            ProcessEntry {
                process_id: 20,
                parent_process_id: 10,
            },
            ProcessEntry {
                process_id: 30,
                parent_process_id: 20,
            },
        ];

        assert_eq!(collect_process_tree(99, &entries), vec![99]);
    }

    #[test]
    fn collect_process_tree_includes_nested_descendants() {
        let entries = [
            ProcessEntry {
                process_id: 40,
                parent_process_id: 30,
            },
            ProcessEntry {
                process_id: 20,
                parent_process_id: 10,
            },
            ProcessEntry {
                process_id: 30,
                parent_process_id: 20,
            },
            ProcessEntry {
                process_id: 50,
                parent_process_id: 99,
            },
        ];

        assert_eq!(collect_process_tree(10, &entries), vec![10, 20, 30, 40]);
    }
}

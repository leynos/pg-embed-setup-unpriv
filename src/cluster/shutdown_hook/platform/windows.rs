//! Windows process controls for the shutdown hook.

use std::{ffi::c_void, ptr::NonNull};

use crate::error::BootstrapResult;

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
const ERROR_INVALID_PARAMETER: u32 = 87;

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
    fn GetLastError() -> u32;
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
    if let Some(process) = process {
        tracing::debug!(
            pid = process.pid(),
            "preparing Windows process-exit Job Object failsafe"
        );
    }
    let job = process.and_then(JobHandle::create_for_process_tree);
    ProcessExitFailsafe { _job: job }
}

/// Requests `PostgreSQL` shutdown.
///
/// Windows has no POSIX-style signal that can be sent safely from the process
/// exit hook. Terminate the process tree immediately so a deliberately leaked
/// test cluster cannot survive the exiting test binary.
pub(in crate::cluster::shutdown_hook) fn request_shutdown(process: PostmasterProcess) {
    tracing::debug!(
        pid = process.pid(),
        "requesting Windows process-tree termination"
    );
    terminate_process_tree(process);
}

/// Forces `PostgreSQL` shutdown after the graceful timeout.
pub(in crate::cluster::shutdown_hook) fn force_shutdown(process: PostmasterProcess) {
    tracing::debug!(
        pid = process.pid(),
        "forcing Windows process-tree termination"
    );
    terminate_process_tree(process);
}

/// Returns `true` when the process exists and has not exited.
#[cfg(any(doc, test, feature = "cluster-unit-tests", feature = "dev-worker"))]
pub(in crate::cluster::shutdown_hook) fn process_is_running_for_platform(
    pid: PostmasterPid,
) -> BootstrapResult<bool> {
    if pid == 0 {
        return Ok(false);
    }

    let Some(process) = ProcessHandle::open_query_checked(pid)? else {
        return Ok(false);
    };
    process.is_active_checked()
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessEntry {
    process_id: PostmasterPid,
    parent_process_id: PostmasterPid,
}

struct ProcessHandle {
    raw: NonNull<c_void>,
    pid: PostmasterPid,
}

impl ProcessHandle {
    fn open_query_checked(pid: PostmasterPid) -> BootstrapResult<Option<Self>> {
        Self::open_with_access_checked(pid, PROCESS_QUERY_LIMITED_INFORMATION)
    }

    fn open_query(pid: PostmasterPid) -> Option<Self> {
        // SAFETY: `OpenProcess` is called with query-only access, handle
        // inheritance disabled, and a concrete process id read from
        // `postmaster.pid`. A null return is handled as absence.
        let raw_handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        NonNull::new(raw_handle).map(|raw| Self { raw, pid })
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
        NonNull::new(raw_handle).map(|raw| Self { raw, pid })
    }

    fn open_with_access_checked(pid: PostmasterPid, access: u32) -> BootstrapResult<Option<Self>> {
        // SAFETY: same handle invariants as `open_with_access`.
        let raw_handle = unsafe { OpenProcess(access, 0, pid) };
        if let Some(raw) = NonNull::new(raw_handle) {
            return Ok(Some(Self { raw, pid }));
        }

        // SAFETY: `GetLastError` has no preconditions and reads the thread's
        // last OS error after the failed `OpenProcess` call above.
        let code = unsafe { GetLastError() };
        if code == ERROR_INVALID_PARAMETER {
            return Ok(None);
        }
        Err(color_eyre::eyre::eyre!(
            "failed to open process {pid} for query access: Windows error {code}"
        )
        .into())
    }

    fn raw(&self) -> *mut c_void {
        self.raw.as_ptr()
    }

    fn pid(&self) -> PostmasterPid {
        self.pid
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

    fn is_active_checked(&self) -> BootstrapResult<bool> {
        let mut exit_code = 0_u32;
        let exit_code_ptr = std::ptr::addr_of_mut!(exit_code);
        // SAFETY:
        // - `self.0` is a non-null process handle owned by this wrapper.
        // - `exit_code_ptr` points to valid writable storage for the duration
        //   of the call.
        let succeeded = unsafe { GetExitCodeProcess(self.raw(), exit_code_ptr) };
        if succeeded == 0 {
            // SAFETY: `GetLastError` has no preconditions and reads the
            // thread's last OS error after the failed query above.
            let code = unsafe { GetLastError() };
            return Err(color_eyre::eyre::eyre!(
                "failed to query exit status for process {}: Windows error {code}",
                self.pid
            )
            .into());
        }
        Ok(exit_code == STILL_ACTIVE)
    }

    pub(super) fn matches_postmaster(&self, expected: PostmasterProcess) -> bool {
        self.is_active() && process_matches_postmaster(self.raw(), expected)
    }

    fn is_active_postgres(&self) -> bool {
        self.is_active() && image_file_name_is_postgres(self.raw())
    }

    fn terminate(&self) -> bool {
        // SAFETY:
        // - `self.0` is a non-null process handle opened with
        //   `PROCESS_TERMINATE | SYNCHRONIZE`.
        // - the callee does not retain pointers and no Rust references cross
        //   the FFI boundary.
        let terminated = unsafe { TerminateProcess(self.raw(), TERMINATE_EXIT_CODE) != 0 };
        // SAFETY: same handle invariant as the termination call above.
        let wait_result = unsafe { WaitForSingleObject(self.raw(), TERMINATION_WAIT_MS) };
        tracing::debug!(
            pid = self.pid,
            terminated,
            wait_result,
            "attempted Windows process termination"
        );
        terminated
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
        tracing::debug!(
            pid = process.pid(),
            "skipping Windows process-tree termination because root process could not be opened"
        );
        return;
    };
    if !root_process.matches_postmaster(process) {
        tracing::debug!(
            pid = process.pid(),
            "skipping Windows process-tree termination because root identity changed"
        );
        return;
    }

    let tree = process_tree(process.pid());
    let mut descendants = open_terminable_descendant_processes(process.pid(), &tree);
    descendants.reverse();

    for descendant in descendants {
        terminate_process(&descendant);
    }
    root_process.terminate();
}

fn process_tree(root: PostmasterPid) -> Vec<ProcessEntry> {
    let Some(snapshot) = SnapshotHandle::capture_processes() else {
        return vec![ProcessEntry {
            process_id: root,
            parent_process_id: root,
        }];
    };
    collect_process_tree(root, &snapshot.process_entries())
}

fn collect_process_tree(root: PostmasterPid, entries: &[ProcessEntry]) -> Vec<ProcessEntry> {
    let mut tree = vec![ProcessEntry {
        process_id: root,
        parent_process_id: root,
    }];
    let mut found_child = true;

    while found_child {
        found_child = false;
        for entry in entries {
            let parent_is_in_tree = tree
                .iter()
                .any(|member| member.process_id == entry.parent_process_id);
            let process_is_in_tree = tree
                .iter()
                .any(|member| member.process_id == entry.process_id);
            if parent_is_in_tree && !process_is_in_tree {
                tree.push(*entry);
                found_child = true;
            }
        }
    }

    tree
}

fn open_terminable_descendant_processes(
    root: PostmasterPid,
    tree: &[ProcessEntry],
) -> Vec<ProcessHandle> {
    open_validated_descendant_processes(root, tree, ProcessHandle::open_terminate)
}

fn open_assignable_descendant_processes(
    root: PostmasterPid,
    tree: &[ProcessEntry],
) -> Vec<ProcessHandle> {
    open_validated_descendant_processes(root, tree, ProcessHandle::open_assign_to_job)
}

fn open_validated_descendant_processes(
    root: PostmasterPid,
    tree: &[ProcessEntry],
    open_process: impl Fn(PostmasterPid) -> Option<ProcessHandle>,
) -> Vec<ProcessHandle> {
    let candidates = tree
        .iter()
        .copied()
        .filter(|member| member.process_id != root)
        .filter_map(|member| Some((member, open_process(member.process_id)?)))
        .collect::<Vec<_>>();
    let Some(snapshot) = SnapshotHandle::capture_processes() else {
        return Vec::new();
    };
    let current_entries = snapshot.process_entries();

    candidates
        .into_iter()
        .filter_map(|(member, process)| {
            let is_valid = process.is_active_postgres()
                && descendant_is_still_in_root_tree(root, member, &current_entries);
            if !is_valid {
                tracing::debug!(
                    pid = member.process_id,
                    parent_pid = member.parent_process_id,
                    root_pid = root,
                    "skipping Windows descendant because it is no longer in the validated postmaster tree"
                );
            }
            is_valid.then_some(process)
        })
        .collect()
}

fn descendant_is_still_in_root_tree(
    root: PostmasterPid,
    descendant: ProcessEntry,
    entries: &[ProcessEntry],
) -> bool {
    let Some(current) = entries
        .iter()
        .find(|entry| entry.process_id == descendant.process_id)
    else {
        return false;
    };
    current.parent_process_id == descendant.parent_process_id
        && process_has_root_ancestor(root, descendant.process_id, entries)
}

fn process_has_root_ancestor(
    root: PostmasterPid,
    mut process_id: PostmasterPid,
    entries: &[ProcessEntry],
) -> bool {
    for _ in 0..entries.len() {
        let Some(entry) = entries.iter().find(|entry| entry.process_id == process_id) else {
            return false;
        };
        if entry.parent_process_id == root {
            return true;
        }
        if entry.parent_process_id == process_id {
            return false;
        }
        process_id = entry.parent_process_id;
    }
    false
}

fn terminate_process(process: &ProcessHandle) {
    if process.is_active_postgres() {
        process.terminate();
    } else {
        tracing::debug!(
            pid = process.pid(),
            "skipping Windows process termination because process is no longer active postgres"
        );
    }
}

//! Windows Job Object failsafe for process-exit cleanup.

use std::{ffi::c_void, ptr::NonNull};

use super::{PostmasterPid, PostmasterProcess, ProcessHandle, process_tree};

const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: u32 = 9;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
    fn SetInformationJobObject(
        job: *mut c_void,
        info_class: u32,
        info: *mut c_void,
        info_length: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

pub(super) struct JobHandle(NonNull<c_void>);

// SAFETY: `JobHandle` owns a kernel handle. Windows permits closing and using
// job handles from any thread, and access is synchronized by the kernel.
unsafe impl Send for JobHandle {}

impl JobHandle {
    pub(super) fn create_for_process_tree(root: PostmasterProcess) -> Option<Self> {
        let root_process = ProcessHandle::open_assign_to_job(root.pid())?;
        if !root_process.matches_postmaster(root) {
            return None;
        }

        let job = Self::create_kill_on_close()?;
        job.assign_process_tree(root.pid(), &root_process)
            .then_some(job)
    }

    fn create_kill_on_close() -> Option<Self> {
        // SAFETY:
        // - null security attributes request the default descriptor.
        // - null name creates an unnamed private job.
        // - a null return is handled as failure.
        let raw_handle = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        let job = NonNull::new(raw_handle).map(Self)?;
        job.enable_kill_on_close().then_some(job)
    }

    fn enable_kill_on_close(&self) -> bool {
        let mut info = JobObjectExtendedLimitInformation::kill_on_close();
        let Ok(info_length) =
            u32::try_from(std::mem::size_of::<JobObjectExtendedLimitInformation>())
        else {
            return false;
        };
        let info_ptr = std::ptr::addr_of_mut!(info).cast::<c_void>();

        // SAFETY:
        // - `self.0` is a non-null job handle owned by this wrapper.
        // - `info_ptr` points to an initialized job-information value with a
        //   valid byte length for this process architecture.
        // - the callee reads the buffer only for the duration of the call.
        unsafe {
            SetInformationJobObject(
                self.0.as_ptr(),
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                info_ptr,
                info_length,
            ) != 0
        }
    }

    fn assign_process_tree(&self, root: PostmasterPid, root_process: &ProcessHandle) -> bool {
        let mut assigned_any = self.assign_process_handle(root_process);

        for process_id in process_tree(root) {
            if process_id == root {
                continue;
            }
            if self.assign_process(process_id) {
                assigned_any = true;
            }
        }

        assigned_any
    }

    fn assign_process(&self, pid: PostmasterPid) -> bool {
        let Some(process) = ProcessHandle::open_assign_to_job(pid) else {
            return false;
        };
        self.assign_process_handle(&process)
    }

    fn assign_process_handle(&self, process: &ProcessHandle) -> bool {
        // SAFETY:
        // - `self.0` is a valid job handle configured before assignment.
        // - `process` owns a process handle opened with the rights required by
        //   `AssignProcessToJobObject`.
        unsafe { AssignProcessToJobObject(self.0.as_ptr(), process.raw()) != 0 }
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        // SAFETY: `JobHandle` owns the non-null handle returned by
        // `CreateJobObjectW`; closing it exactly once releases the OS resource.
        unsafe {
            CloseHandle(self.0.as_ptr());
        }
    }
}

#[repr(C)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
struct JobObjectExtendedLimitInformation {
    basic_limit_information: JobObjectBasicLimitInformation,
    io_info: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

impl JobObjectExtendedLimitInformation {
    fn kill_on_close() -> Self {
        Self {
            basic_limit_information: JobObjectBasicLimitInformation {
                per_process_user_time_limit: 0,
                per_job_user_time_limit: 0,
                limit_flags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                minimum_working_set_size: 0,
                maximum_working_set_size: 0,
                active_process_limit: 0,
                affinity: 0,
                priority_class: 0,
                scheduling_class: 0,
            },
            io_info: IoCounters {
                read_operation_count: 0,
                write_operation_count: 0,
                other_operation_count: 0,
                read_transfer_count: 0,
                write_transfer_count: 0,
                other_transfer_count: 0,
            },
            process_memory_limit: 0,
            job_memory_limit: 0,
            peak_process_memory_used: 0,
            peak_job_memory_used: 0,
        }
    }
}

//! Windows postmaster identity checks for stale PID protection.

use std::ffi::c_void;

use super::PostmasterPid;

const PROCESS_IMAGE_BUFFER_CHARS: usize = 32_768;
const FILETIME_TICKS_PER_SECOND: u64 = 10_000_000;
const FILETIME_UNIX_EPOCH_OFFSET_SECONDS: u64 = 11_644_473_600;
const POSTMASTER_START_TIME_TOLERANCE_SECONDS: u64 = 2;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetProcessTimes(
        process: *mut c_void,
        creation_time: *mut FileTime,
        exit_time: *mut FileTime,
        kernel_time: *mut FileTime,
        user_time: *mut FileTime,
    ) -> i32;
    fn QueryFullProcessImageNameW(
        process: *mut c_void,
        flags: u32,
        exe_name: *mut u16,
        size: *mut u32,
    ) -> i32;
}

/// Windows postmaster identity parsed from `postmaster.pid`.
#[derive(Clone, Copy)]
pub(in crate::cluster::shutdown_hook) struct PostmasterProcess {
    pid: PostmasterPid,
    started_at_unix_seconds: u64,
}

impl PostmasterProcess {
    fn new(pid: PostmasterPid, started_at_unix_seconds: u64) -> Self {
        Self {
            pid,
            started_at_unix_seconds,
        }
    }

    pub(super) fn pid(self) -> PostmasterPid {
        self.pid
    }
}

/// Parses the Windows postmaster process identity.
pub(in crate::cluster::shutdown_hook) fn parse_postmaster_process(
    contents: &str,
) -> Option<PostmasterProcess> {
    let mut lines = contents.lines();
    let pid = super::parse_pid(lines.next()?)?;
    let _data_dir = lines.next()?;
    let started_at = lines.next()?.trim().parse::<u64>().ok()?;
    Some(PostmasterProcess::new(pid, started_at))
}

pub(super) fn process_matches_postmaster(
    process: *mut c_void,
    expected: PostmasterProcess,
) -> bool {
    image_file_name_is_postgres(process) && creation_time_matches(process, expected)
}

fn image_file_name_is_postgres(process: *mut c_void) -> bool {
    process_image_name(process)
        .as_deref()
        .and_then(|name| name.rsplit(['\\', '/']).next())
        .is_some_and(|file_name| {
            file_name.eq_ignore_ascii_case("postgres.exe")
                || file_name.eq_ignore_ascii_case("postgres")
        })
}

fn process_image_name(process: *mut c_void) -> Option<String> {
    let mut buffer = vec![0_u16; PROCESS_IMAGE_BUFFER_CHARS];
    let mut size = u32::try_from(buffer.len()).ok()?;
    let size_ptr = std::ptr::addr_of_mut!(size);

    // SAFETY:
    // - `process` is a non-null process handle opened with query access by the
    //   caller.
    // - `buffer` is writable UTF-16 storage with length reported through
    //   `size_ptr`.
    // - the callee writes at most `size` UTF-16 code units and does not retain
    //   the pointer.
    let succeeded =
        unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), size_ptr) };
    if succeeded == 0 {
        return None;
    }

    let used = usize::try_from(size).ok()?;
    Some(String::from_utf16_lossy(&buffer[..used]))
}

fn creation_time_matches(process: *mut c_void, expected: PostmasterProcess) -> bool {
    creation_unix_seconds(process).is_some_and(|actual| {
        actual.abs_diff(expected.started_at_unix_seconds) <= POSTMASTER_START_TIME_TOLERANCE_SECONDS
    })
}

fn creation_unix_seconds(process: *mut c_void) -> Option<u64> {
    let mut creation_time = FileTime::zero();
    let mut exit_time = FileTime::zero();
    let mut kernel_time = FileTime::zero();
    let mut user_time = FileTime::zero();

    // SAFETY:
    // - `process` is a non-null process handle opened with query access by the
    //   caller.
    // - all four pointers refer to writable `FILETIME`-layout values for the
    //   duration of the call.
    let succeeded = unsafe {
        GetProcessTimes(
            process,
            std::ptr::addr_of_mut!(creation_time),
            std::ptr::addr_of_mut!(exit_time),
            std::ptr::addr_of_mut!(kernel_time),
            std::ptr::addr_of_mut!(user_time),
        )
    };
    if succeeded == 0 {
        return None;
    }

    creation_time.unix_seconds()
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FileTime {
    low_date_time: u32,
    high_date_time: u32,
}

impl FileTime {
    fn zero() -> Self {
        Self {
            low_date_time: 0,
            high_date_time: 0,
        }
    }

    fn unix_seconds(self) -> Option<u64> {
        let ticks = (u64::from(self.high_date_time) << 32) | u64::from(self.low_date_time);
        let windows_seconds = ticks / FILETIME_TICKS_PER_SECOND;
        windows_seconds.checked_sub(FILETIME_UNIX_EPOCH_OFFSET_SECONDS)
    }
}

#[cfg(test)]
mod tests {
    use super::{FileTime, parse_postmaster_process};

    #[test]
    fn parse_postmaster_process_reads_pid_and_start_time() {
        let process =
            parse_postmaster_process("4242\r\nC:/tmp/pgdata\r\n1760000000\r\n5432\r\nready\r\n")
                .expect("valid postmaster.pid should parse");

        assert_eq!(process.pid, 4242);
        assert_eq!(process.started_at_unix_seconds, 1_760_000_000);
    }

    #[test]
    fn parse_postmaster_process_rejects_missing_start_time() {
        assert!(parse_postmaster_process("4242\r\nC:/tmp/pgdata\r\n").is_none());
    }

    #[test]
    fn filetime_converts_unix_epoch() {
        let ticks = 11_644_473_600_u64 * 10_000_000;
        let filetime = FileTime {
            low_date_time: ticks as u32,
            high_date_time: (ticks >> 32) as u32,
        };

        assert_eq!(filetime.unix_seconds(), Some(0));
    }
}

//! The watcher's process: its shell body and how it is spawned (#287).

use std::{
    io,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
};

/// The watcher's shell body: `$1` is the data directory.
///
/// It reads the PID on the first line of `postmaster.pid`, requires it to be
/// digits, requires `/proc/<pid>/comm` to be `postgres` and `/proc/<pid>/cwd` to
/// be the data directory's real path, and only then sends `SIGQUIT`, as
/// `pg_ctl stop -m immediate` does. Every other outcome exits quietly.
pub(super) const WATCHER_SCRIPT: &str = r#"
data_dir=$1
pid=$(head -n 1 "$data_dir/postmaster.pid" 2>/dev/null) || exit 0
case $pid in ''|*[!0-9]*) exit 0 ;; esac
[ "$(cat "/proc/$pid/comm" 2>/dev/null)" = postgres ] || exit 0
real=$(cd "$data_dir" 2>/dev/null && pwd -P) || exit 0
[ "$(readlink "/proc/$pid/cwd" 2>/dev/null)" = "$real" ] || exit 0
kill -QUIT "$pid" 2>/dev/null
exit 0
"#;

/// Marks every descriptor above the standard streams close-on-exec.
///
/// The watcher outlives its owner, so a descriptor it inherited, such as a
/// pipe a test harness gave the owner without `O_CLOEXEC`, would stay open for
/// as long as the watcher waits and could keep that pipe's reader waiting for
/// end-of-file. `close_range` with `CLOSE_RANGE_CLOEXEC` marks them all, and
/// the descriptor `std` keeps to report an `exec` failure is already
/// close-on-exec, so a missing `setsid` still surfaces as a spawn error.
/// Where `close_range` is unavailable the descriptors up to a fixed ceiling are
/// marked one by one.
///
/// Runs between `fork` and `exec`, so it only makes system calls.
fn mark_inherited_descriptors_close_on_exec() {
    // SAFETY: `close_range` and `fcntl` take plain integers and touch no memory
    // of this process; both are async-signal-safe.
    unsafe {
        if libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        ) == 0
        {
            return;
        }
        for descriptor in 3..FALLBACK_DESCRIPTOR_CEILING {
            libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
}

/// The highest descriptor the fallback marks.
const FALLBACK_DESCRIPTOR_CEILING: i32 = 4096;

/// Builds the watcher command for a slot: blocks on `lock`, then runs the
/// script against `data_dir`, in a session of its own with no standard streams.
fn watcher_command(lock: &Path, data_dir: &Path) -> Command {
    let mut command = Command::new("setsid");
    command
        .arg("flock")
        .arg("--exclusive")
        .arg(lock)
        .args(["sh", "-c", WATCHER_SCRIPT, "pg-embed-watch"])
        .arg(data_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the closure only makes async-signal-safe system calls and
    // allocates nothing, as `pre_exec` requires.
    unsafe {
        command.pre_exec(|| {
            mark_inherited_descriptors_close_on_exec();
            Ok(())
        });
    }
    command
}

/// Spawns the watcher for `data_dir` and returns its handle.
///
/// # Errors
///
/// Returns the spawn error, `NotFound` where `setsid` or `flock` is missing.
pub(crate) fn spawn_watcher(lock: &Path, data_dir: &Path) -> io::Result<Child> {
    watcher_command(lock, data_dir).spawn()
}

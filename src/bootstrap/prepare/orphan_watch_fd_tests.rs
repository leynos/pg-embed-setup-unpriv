//! The descriptor fallback used where `close_range` is unavailable (#287).

use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

use color_eyre::eyre::{Result, ensure};

const WAIT: Duration = Duration::from_secs(10);

/// The fallback marks up to the process's hard descriptor limit, so a descriptor
/// opened before the soft limit was lowered below it cannot survive into the
/// watcher.
///
/// The child opens a descriptor at 300, lowers its soft limit to 64, and runs
/// only the one-by-one marking before `exec`ing `sleep`. A fallback bounded by
/// the soft limit would leave 300 open.
#[test]
fn the_fallback_covers_a_descriptor_above_a_lowered_soft_limit() -> Result<()> {
    use std::os::unix::process::CommandExt;

    const HIGH: i32 = 300;
    let mut command = Command::new("sleep");
    command.arg("30");
    // SAFETY: the closure makes only async-signal-safe system calls and
    // allocates nothing, as `pre_exec` requires.
    unsafe {
        command.pre_exec(|| {
            if libc::dup2(0, HIGH) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            limit.rlim_cur = 64;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            super::process::mark_descriptors_one_by_one();
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let fd = format!("/proc/{}/fd/{HIGH}", child.id());
    let deadline = Instant::now() + WAIT;
    // `exec` replaces the image, closing the marked descriptors, so wait for it.
    while std::fs::read_to_string(format!("/proc/{}/comm", child.id()))?.trim() != "sleep" {
        ensure!(Instant::now() < deadline, "the child never became sleep");
        std::thread::sleep(Duration::from_millis(5));
    }
    let survived = Path::new(&fd).exists();
    child.kill()?;
    child.wait()?;
    ensure!(!survived, "descriptor {HIGH} survived the fallback");
    Ok(())
}

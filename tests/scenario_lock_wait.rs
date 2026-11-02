//! Behavioural coverage for the completed scenario-lock wait diagnostic.
//!
//! `tests/support/serial.rs` reports a completed `flock` wait once it
//! passes a second, because nextest stops counting a test's allowance
//! while it queues for a group slot but keeps counting a wait inside the
//! test. The unit tests there pin the formatter and the measurement; this
//! binary pins the behaviour in both directions. The parent takes the
//! lock, a child process for the same `CARGO_TARGET_DIR` blocks in
//! `acquire_process_lock`, and the parent releases: after a long hold the
//! child reports the wait, and after a short one it stays silent.
//!
//! The two processes share an isolated target directory, so the real
//! scenario lock is untouched and this binary needs no `serial` group
//! membership.

#![cfg(unix)]

#[path = "support/serial.rs"]
mod serial;

use std::{
    env,
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

use pg_embedded_setup_unpriv::test_support::scoped_env;

/// Printed before the child blocks, so the parent releases on a signal
/// rather than on a guessed startup delay.
const READY: &str = "LOCK-WAIT-CHILD: ready";

/// The lock file `acquire_process_lock` creates in `CARGO_TARGET_DIR`.
const LOCK_FILE: &str = "pg-embed-setup-unpriv.serial.lock";

/// Long enough for the child's wait to clear the one-second threshold
/// even if scheduling delays its `flock` call slightly.
const HOLD: Duration = Duration::from_secs(2);

/// Short enough that the child's wait stays below the reporting threshold.
const SHORT_HOLD: Duration = Duration::from_millis(100);

/// How many times the parent will hold the lock before giving up.
///
/// The child announces readiness before it blocks, so the parent cannot
/// know it has actually entered `flock`. If the child is descheduled
/// between the two, a single fixed hold can release the lock before the
/// wait clears the one-second threshold and the diagnostic is never
/// produced. Retrying with a longer hold separates that scheduling
/// accident from a diagnostic that does not appear at all.
const ATTEMPTS: u32 = 3;

/// Remove a directory this test owns, treating "already gone" as success.
///
/// The path is keyed on this process id, so an absent directory means
/// only that nothing was left behind; any other removal failure is a
/// real problem worth failing on.
fn remove_owned_dir(path: &Path) {
    if path.exists() {
        std::fs::remove_dir_all(path).expect("remove the test's own directory");
    }
}

/// Run one child against a held lock and return the child's stderr.
///
/// The parent takes the lock, spawns the child, waits for its readiness
/// marker, holds for `hold`, then releases. The child's success is part
/// of the contract, so a failing child is reported here rather than
/// being mistaken for a missing diagnostic.
fn child_stderr_after_held_wait(tmp_dir: &Path, hold: Duration) -> String {
    let parent_lock = serial::acquire_process_lock();
    let mut child = Command::new(env::current_exe().expect("resolve the test binary"))
        .args([
            "--exact",
            "lock_wait_child_blocks_until_released",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("CARGO_TARGET_DIR", tmp_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the lock-waiting child");

    let stdout = child.stdout.take().expect("child stdout is piped");
    let mut lines = BufReader::new(stdout).lines();
    let ready = lines
        .by_ref()
        .find(|line| line.as_ref().is_ok_and(|text| text.contains(READY)));
    assert!(
        ready.is_some(),
        "the child must announce readiness before it blocks on the lock"
    );

    thread::sleep(hold);
    drop(parent_lock);

    let status = child.wait().expect("wait for the lock-waiting child");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("child stderr is piped")
        .read_to_string(&mut stderr)
        .expect("read the child's diagnostic");
    assert!(status.success(), "the child failed: {stderr}");
    stderr
}

/// The child waits for the lock while the parent holds it.
///
/// The report names the lock path and the completed duration, and the
/// wait is real: the parent owns the descriptor until after the hold.
/// A child that was slow to reach `flock` gets a longer hold on the next
/// attempt, so a scheduling accident cannot be read as a missing
/// diagnostic.
#[test]
fn a_completed_lock_wait_reports_the_path_and_duration() {
    let tmp_dir = env::temp_dir().join(format!("pg-lock-wait-{}", std::process::id()));
    remove_owned_dir(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("create the isolated CARGO_TARGET_DIR");

    let _env_guard = scoped_env(vec![(
        OsString::from("CARGO_TARGET_DIR"),
        Some(tmp_dir.clone().into_os_string()),
    )]);

    let mut stderr = String::new();
    for attempt in 0..ATTEMPTS {
        stderr = child_stderr_after_held_wait(&tmp_dir, HOLD * (1 << attempt));
        if stderr.contains("completed after") {
            break;
        }
    }
    remove_owned_dir(&tmp_dir);

    let lock_path = tmp_dir.join(LOCK_FILE);
    assert!(
        stderr.contains(&lock_path.display().to_string()),
        "the completed wait must name the lock file {}: {stderr}",
        lock_path.display()
    );
    assert!(
        stderr.contains("completed after"),
        "the completed wait must state how long it blocked: {stderr}"
    );
}

/// A wait below the threshold is silent, so a report means something.
///
/// Reporting every short wait would bury the waits that matter, and never
/// reporting one would be indistinguishable from a detector that does not
/// fire. A scheduling delay that turns one attempt into a long wait is
/// retried rather than read as a broken threshold.
#[test]
fn a_short_lock_wait_is_not_reported() {
    let tmp_dir = env::temp_dir().join(format!("pg-lock-wait-short-{}", std::process::id()));
    remove_owned_dir(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("create the isolated CARGO_TARGET_DIR");

    let mut stderr = String::new();
    for _ in 0..ATTEMPTS {
        stderr = child_stderr_after_held_wait(&tmp_dir, SHORT_HOLD);
        if !stderr.contains("completed after") {
            break;
        }
    }
    remove_owned_dir(&tmp_dir);

    assert!(
        !stderr.contains("completed after"),
        "a wait below the one-second threshold must not be reported: {stderr}"
    );
}

/// Blocks in `acquire_process_lock` until the parent drops its lock.
///
/// Run only as a child of the test above; it is `#[ignore]` so a normal
/// run does not hold the lock while the parent test waits for it.
#[test]
#[ignore = "spawned by a_completed_lock_wait_reports_the_path_and_duration"]
fn lock_wait_child_blocks_until_released() {
    use std::io::Write as _;

    {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{READY}").expect("announce readiness");
        out.flush().expect("flush the readiness marker");
    }

    let _held = serial::acquire_process_lock();
}

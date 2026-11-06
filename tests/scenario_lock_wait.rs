//! Behavioural coverage for the completed scenario-lock wait diagnostic.
//!
//! `tests/support/serial.rs` reports a completed `flock` wait once it
//! passes a second, because nextest stops counting a test's allowance
//! while it queues for a group slot but keeps counting a wait inside the
//! test. The unit tests there pin the formatter and the measurement; this
//! binary pins the behaviour in both directions. The parent takes the
//! lock, a child process for the same `CARGO_TARGET_DIR` blocks in
//! `acquire_process_lock`, and the parent releases: after a long hold the
//! child reports the wait, and after a short one it stays silent. The
//! child also reports the duration it measured across its own
//! acquisition, so the silent case proves contention rather than merely
//! the absence of a message.
//!
//! Both processes receive the same isolated `CARGO_TARGET_DIR`, so the
//! real scenario lock is untouched and this binary needs no `serial`
//! group membership.

// nested-cargo-allow: target no cargo runs here; the variable only locates a lock file

#![cfg(unix)]

#[path = "support/serial.rs"]
mod serial;

use std::{
    env,
    ffi::OsString,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{Mutex, MutexGuard, mpsc},
    thread,
    time::{Duration, Instant},
};

use pg_embedded_setup_unpriv::{ScopedEnv, test_support::scoped_env};

/// Printed before the child blocks, so the parent releases on a signal
/// rather than on a guessed startup delay.
const READY: &str = "LOCK-WAIT-CHILD: ready";

/// Printed after `acquire_process_lock` returns, with the milliseconds
/// the child spent blocked. Seeing it proves the child reached the lock,
/// which readiness alone does not.
const WAITED: &str = "LOCK-WAIT-CHILD: waited";

/// The lock file `acquire_process_lock` creates in `CARGO_TARGET_DIR`.
const LOCK_FILE: &str = "pg-embed-setup-unpriv.serial.lock";

/// How long the parent waits for any one child signal or exit.
///
/// A child that never announces readiness, never returns from `flock`, or
/// never exits must fail this test rather than hang the suite, so every
/// synchronization point is bounded by the same generous allowance.
const SYNC_TIMEOUT: Duration = Duration::from_secs(30);

/// Serializes the tests that set `CARGO_TARGET_DIR` for the parent.
///
/// `scoped_env` writes a process-wide variable. nextest gives each test
/// its own process, but libtest runs them as threads of one, where the
/// two tests would otherwise read each other's target directory and stop
/// contending for the same lock.
static TARGET_DIR_MUTEX: Mutex<()> = Mutex::new(());

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

/// An isolated `CARGO_TARGET_DIR` that both the parent and its child use.
///
/// The guard holds the process-wide mutex for as long as it lives, because
/// `scoped_env` writes a process-wide variable and libtest runs these tests
/// as threads of one process. The directory is removed on drop, including
/// when an assertion fails, so a failed run leaves nothing behind.
///
/// Field order is drop order: `_env` restores `CARGO_TARGET_DIR` before
/// `_target_dir` unlocks, so the next test cannot observe the stale value.
struct IsolatedTargetDir {
    _env: ScopedEnv,
    _target_dir: MutexGuard<'static, ()>,
    path: PathBuf,
}

impl IsolatedTargetDir {
    fn new(label: &str) -> Self {
        let target_dir = TARGET_DIR_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = env::temp_dir().join(format!("{label}-{}", std::process::id()));
        remove_owned_dir(&path);
        std::fs::create_dir_all(&path).expect("create the isolated CARGO_TARGET_DIR");
        let scoped = scoped_env(vec![(
            OsString::from("CARGO_TARGET_DIR"),
            Some(path.clone().into_os_string()),
        )]);
        Self {
            _env: scoped,
            _target_dir: target_dir,
            path,
        }
    }

    fn path(&self) -> &Path { &self.path }
}

impl Drop for IsolatedTargetDir {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "cleanup must not panic while an assertion is already unwinding"
    )]
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.path); }
}

/// What one child run reported.
#[derive(Debug)]
struct ChildRun {
    /// Everything the child wrote to standard error.
    stderr: String,
    /// How long the child spent inside `acquire_process_lock`.
    waited: Duration,
}

/// Kill and reap a child so a failed assertion leaves no process behind.
#[expect(
    clippy::let_underscore_must_use,
    reason = "best-effort cleanup while a child is already failing the test"
)]
fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Read child output until the marker appears, or fail after cleaning up.
///
/// The child writes libtest's own progress lines to the same pipe, so
/// lines that do not carry the marker are skipped. Both failure modes —
/// the stream ending and the deadline passing — stop the child first, so
/// a hung child cannot outlive the test that spawned it.
fn expect_line(
    lines: &mpsc::Receiver<String>,
    marker: &str,
    expectation: &str,
    child: &mut Child,
) -> String {
    let deadline = Instant::now() + SYNC_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(remaining) {
            Ok(line) if line.contains(marker) => return line,
            Ok(_) => {}
            Err(error) => {
                terminate(child);
                panic!("the child must {expectation} within {SYNC_TIMEOUT:?}: {error}");
            }
        }
    }
}

/// Reap the child, killing it if it outstays the synchronization allowance.
fn wait_bounded(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + SYNC_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("query the child's status") {
            return status;
        }
        if Instant::now() >= deadline {
            terminate(child);
            panic!("the child must exit within {SYNC_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Read the milliseconds the child measured while it was blocked.
fn parse_waited(line: &str) -> Duration {
    let Some(millis) = line
        .rsplit_once(' ')
        .and_then(|(_, value)| value.strip_suffix("ms"))
        .and_then(|value| value.parse::<u64>().ok())
    else {
        panic!("the child's wait line must end in milliseconds: {line}");
    };
    Duration::from_millis(millis)
}

/// Run one child against a held lock and return what it observed.
///
/// The parent takes the lock, spawns the child, waits for its readiness
/// marker, holds for `hold`, then releases. The child's success is part
/// of the contract, so a failing child is reported here rather than being
/// mistaken for a missing diagnostic. The returned wait is the child's
/// own measurement across `acquire_process_lock`, which is what shows the
/// child actually queued behind the parent's descriptor.
fn child_run_against_held_lock(tmp_dir: &Path, hold: Duration) -> ChildRun {
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
    let (sender, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                break;
            }
        }
    });

    expect_line(
        &lines,
        READY,
        "announce readiness before it blocks on the lock",
        &mut child,
    );

    thread::sleep(hold);
    drop(parent_lock);

    let waited_line = expect_line(
        &lines,
        WAITED,
        "report the wait it measured across the acquisition",
        &mut child,
    );
    let status = wait_bounded(&mut child);

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("child stderr is piped")
        .read_to_string(&mut stderr)
        .expect("read the child's diagnostic");
    assert!(status.success(), "the child failed: {stderr}");
    ChildRun {
        stderr,
        waited: parse_waited(&waited_line),
    }
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
    let target_dir = IsolatedTargetDir::new("pg-lock-wait");

    let mut run = child_run_against_held_lock(target_dir.path(), HOLD);
    for attempt in 1..ATTEMPTS {
        if run.stderr.contains("completed after") {
            break;
        }
        run = child_run_against_held_lock(target_dir.path(), HOLD * (1 << attempt));
    }

    let lock_path = target_dir.path().join(LOCK_FILE);
    assert!(
        run.stderr.contains(&lock_path.display().to_string()),
        "the completed wait must name the lock file {}: {}",
        lock_path.display(),
        run.stderr
    );
    assert!(
        run.stderr.contains("completed after"),
        "the completed wait must state how long it blocked: {}",
        run.stderr
    );
}

/// A wait below the threshold is silent, so a report means something.
///
/// Reporting every short wait would bury the waits that matter, and never
/// reporting one would be indistinguishable from a detector that does not
/// fire. The child's own measurement is asserted to have spanned most of
/// the parent's hold, so silence cannot come from two processes locking
/// different files: the child demonstrably queued behind the parent. Two
/// scheduling accidents are retried rather than read as a broken
/// threshold: a delay that turns one attempt into a long wait, and a
/// delay that lets the parent release before the child blocks, which
/// leaves the measured wait too short to prove contention.
#[test]
fn a_short_lock_wait_is_not_reported() {
    let target_dir = IsolatedTargetDir::new("pg-lock-wait-short");

    let mut run = child_run_against_held_lock(target_dir.path(), SHORT_HOLD);
    for _ in 1..ATTEMPTS {
        let reported = run.stderr.contains("completed after");
        let contended = run.waited >= SHORT_HOLD / 2;
        if !reported && contended {
            break;
        }
        run = child_run_against_held_lock(target_dir.path(), SHORT_HOLD);
    }

    assert!(
        run.waited >= SHORT_HOLD / 2,
        "the child must block behind the parent's lock for about {SHORT_HOLD:?}, but it measured \
         {:?}: {}",
        run.waited,
        run.stderr
    );
    assert!(
        !run.stderr.contains("completed after"),
        "a wait below the one-second threshold must not be reported: {}",
        run.stderr
    );
}

/// Blocks in `acquire_process_lock` until the parent drops its lock, then
/// reports how long the acquisition took.
///
/// Run only as a child of the tests above; it is `#[ignore]` so a normal
/// run does not hold the lock while the parent test waits for it. The
/// reported duration is the evidence that this child, not some other
/// process, queued behind the parent's descriptor.
#[test]
#[ignore = "spawned by the lock-wait tests"]
fn lock_wait_child_blocks_until_released() {
    use std::io::Write as _;

    {
        let mut out = std::io::stdout().lock();
        writeln!(out, "{READY}").expect("announce readiness");
        out.flush().expect("flush the readiness marker");
    }

    let started = Instant::now();
    let _held = serial::acquire_process_lock();
    let waited = started.elapsed();

    let mut out = std::io::stdout().lock();
    writeln!(out, "{WAITED} {}ms", waited.as_millis()).expect("report the measured wait");
    out.flush().expect("flush the measured wait");
}

//! Child-process harness for the cross-process cache lock tests.
//!
//! The parent re-executes its own test binary with the ignored child case.
//! A reader thread forwards the child's report lines through a channel, so
//! every wait in the parent is bounded by `recv_timeout` rather than a
//! blocking read.

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdout, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use camino::Utf8Path;

/// Environment variable marking a child launched by the parent harness.
pub(super) const CHILD_MARKER: &str = "PG_EMBED_CACHE_LOCK_CHILD";

/// Environment variable naming the cache directory in the child.
pub(super) const CHILD_CACHE_DIR: &str = "PG_EMBED_CACHE_LOCK_DIR";

/// Environment variable naming the version key in the child.
pub(super) const CHILD_VERSION: &str = "PG_EMBED_CACHE_LOCK_VERSION";

/// Environment variable naming the lock mode in the child.
pub(super) const CHILD_MODE: &str = "PG_EMBED_CACHE_LOCK_MODE";

/// The ignored test case the parent re-executes.
const CHILD_TEST: &str = "cache::lock::tests::cache_lock_child";

/// Prefix of every line the child reports.
const REPORT: &str = "CACHE-LOCK-CHILD:";

/// How long a child gets to exit after its stdin closes, before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(30);

/// Writes one report line to stdout, where the parent reads it.
pub(super) fn report(line: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{REPORT} {line}")?;
    out.flush()
}

/// A running child and its report stream.
///
/// Dropping it closes the child's stdin, gives it [`EXIT_GRACE`] to exit, then
/// kills it, so a failed assertion never strands a child.
pub(super) struct ChildLock {
    child: Child,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl ChildLock {
    /// Starts the ignored child test against `cache_dir` and `version`.
    pub(super) fn spawn(cache_dir: &Utf8Path, version: &str, mode: &str) -> std::io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                CHILD_TEST,
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_MARKER, "1")
            .env(CHILD_CACHE_DIR, cache_dir.as_str())
            .env(CHILD_VERSION, version)
            .env(CHILD_MODE, mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = command.spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("child has no stdout"))?;
        let (tx, lines) = mpsc::channel();
        let reader = thread::spawn(move || forward(stdout, &tx));
        Ok(Self {
            child,
            lines,
            reader: Some(reader),
        })
    }

    /// Waits up to `timeout` for the child to report that it holds the lock.
    pub(super) fn acquired_within(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            match self.lines.recv_timeout(remaining) {
                Ok(line) if line == "acquired" => return true,
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return false,
            }
        }
    }

    /// Polls until the child exits, reporting whether it did so within
    /// `grace`.
    fn exited_within(&mut self, grace: Duration) -> bool {
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            // A failed `try_wait` also means the child is no longer running.
            if !matches!(self.child.try_wait(), Ok(None)) {
                return true;
            }
            thread::sleep(Duration::from_millis(100));
        }
        false
    }
}

/// Forwards every child line that carries a report to `tx`.
fn forward(stdout: ChildStdout, tx: &mpsc::Sender<String>) {
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        // libtest prints `test <name> ... ` on the same line before the
        // child's own output, so find the prefix anywhere.
        if let Some((_, report)) = line.split_once(REPORT)
            && tx.send(report.trim().to_owned()).is_err()
        {
            return;
        }
    }
}

impl Drop for ChildLock {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        if !self.exited_within(EXIT_GRACE) {
            let _killed = self.child.kill();
        }
        let _reaped = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _joined = reader.join();
        }
    }
}

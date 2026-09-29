//! Child-process harness for the per-cluster directory tests: spawning and
//! guarding children of the test binary, reading their reports, and the
//! capability-scoped filesystem helpers the cases share.

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use cap_std::{ambient_authority, fs::Dir};
use fs4::FileExt;

/// Set in a child's environment to the mode it runs in.
pub const CHILD_MODE: &str = "PG_EMBED_CLUSTER_CHILD_MODE";

/// Prefix of every line a child reports on.
pub const REPORT: &str = "CLUSTER-CHILD:";

/// Returns the mode this process was spawned in, if it is a child.
pub fn child_mode() -> Option<String> { std::env::var(CHILD_MODE).ok() }

/// Writes one report line to stdout, where the parent reads it.
pub fn report(line: &str) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{REPORT} {line}")?;
    out.flush()
}

/// How long a child gets to exit after its stdin closes, before it is killed.
pub const EXIT_GRACE: Duration = Duration::from_secs(30);

/// A running child and its report stream. Dropping it closes the child's
/// stdin, gives it [`EXIT_GRACE`] to exit, then kills it, so a failed
/// assertion never strands a child.
pub struct Running {
    pub child: Child,
    lines: BufReader<ChildStdout>,
}

impl Running {
    /// Reads until the child's report line, returning it.
    pub fn report(&mut self) -> std::io::Result<String> {
        let mut line = String::new();
        while self.lines.read_line(&mut line)? > 0 {
            // libtest prints `test cluster_child ... ` on the same line
            // before the child's own output, so find the prefix anywhere.
            if let Some((_, found)) = line.split_once(REPORT) {
                return Ok(found.trim().to_owned());
            }
            line.clear();
        }
        Ok(String::from("no report"))
    }

    /// Closes stdin and waits for the child, killing it after [`EXIT_GRACE`].
    pub fn finish(self) { drop(self); }
}

impl Drop for Running {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + EXIT_GRACE;
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _killed = self.child.kill();
        let _reaped = self.child.wait();
    }
}

/// Opens `path` as a capability directory.
pub fn ambient(path: &Path) -> std::io::Result<Dir> {
    Dir::open_ambient_dir(path, ambient_authority())
}

/// Returns the fixed root for one case, creating it if it is missing.
pub fn fixed_root(case: &str) -> std::io::Result<PathBuf> {
    let target = ambient(Path::new(env!("CARGO_TARGET_TMPDIR")))?;
    let relative = Path::new("per-cluster-directories").join(case);
    target.create_dir_all(&relative)?;
    Ok(Path::new(env!("CARGO_TARGET_TMPDIR")).join(relative))
}

/// Starts one child in `mode` against `root`, with extra variables.
pub fn spawn_child(root: &Path, mode: &str, extra: &[(&str, &Path)]) -> std::io::Result<Running> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "cluster_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD_MODE, mode)
        .env("PG_EMBED_ROOT", root)
        .env_remove("PG_DATA_DIR")
        .env_remove("PG_RUNTIME_DIR")
        .env_remove("PG_PASSWORD")
        .env_remove("PG_TEST_BACKEND")
        .env_remove("PG_EMBEDDED_WORKER")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    let mut child = command.spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("child has no stdout"))?;
    Ok(Running {
        child,
        lines: BufReader::new(stdout),
    })
}

/// Returns the data directory a `connected` report names, or the report
/// itself as the error.
pub fn connected_dir(said: &str) -> Result<PathBuf, String> {
    said.strip_prefix("connected ")
        .map(PathBuf::from)
        .ok_or_else(|| said.to_owned())
}

/// Returns whether the suite runs as root.
fn running_as_root() -> bool { nix::unistd::geteuid().is_root() }

/// Returns whether this case should run: not in a child, and not as root.
pub fn should_run() -> bool {
    if child_mode().is_some() {
        return false;
    }
    if running_as_root() {
        tracing::warn!("SKIP: root bootstraps use the worker path, which these cases do not drive");
        return false;
    }
    true
}

/// Returns the PID on the first line of a data directory's `postmaster.pid`.
pub fn postmaster_pid(data_dir: &Path) -> Option<i32> {
    let text = ambient(data_dir)
        .ok()?
        .read_to_string("postmaster.pid")
        .ok()?;
    text.lines().next()?.trim().parse().ok()
}

/// Returns whether a process exists.
pub fn alive(pid: i32) -> bool {
    matches!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Ok(()) | Err(nix::errno::Errno::EPERM)
    )
}

/// Kills a server this test may have left running, whatever the outcome.
pub struct KillOnDrop(pub Option<i32>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _gone = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

/// Opens a lock file for writing, creating it when `create` is set.
pub fn open_lock(path: &Path, create: bool) -> std::io::Result<impl FileExt + use<>> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("a lock path names a file"))?;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let mut options = cap_std::fs::OpenOptions::new();
    options.write(true).create(create).truncate(false);
    Ok(ambient(parent)?.open_with(name, &options)?.into_std())
}

/// Returns whether nothing holds a slot's lock.
pub fn lock_is_free(data_dir: &Path) -> std::io::Result<bool> {
    let lock = open_lock(Path::new(&format!("{}.lock", data_dir.display())), false)?;
    Ok(FileExt::try_lock(&lock).is_ok())
}

/// Waits until a dead slot's lock is free, which is when the grandchild of a
/// killed orchestrator has seen its stdin close and exited.
pub fn wait_until_unlocked(data_dir: &Path) -> std::io::Result<()> {
    let deadline = Instant::now() + EXIT_GRACE;
    while !lock_is_free(data_dir)? {
        if Instant::now() >= deadline {
            return Err(std::io::Error::other("the slot's lock was never freed"));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

/// Returns whether the kernel lists `pid` as blocked waiting for a file lock.
///
/// `/proc/locks` marks a waiter with `->` after the entry's index, and names
/// the waiting process after the lock's mode (`READ` or `WRITE`).
#[cfg(target_os = "linux")]
pub fn is_blocked_on_a_lock(pid: u32) -> std::io::Result<bool> {
    let locks = ambient(Path::new("/proc"))?.read_to_string("locks")?;
    Ok(locks
        .lines()
        .filter(|line| line.contains("->"))
        .filter_map(|line| {
            line.split_whitespace()
                .skip_while(|word| !matches!(*word, "READ" | "WRITE"))
                .nth(1)
        })
        .any(|word| word.parse() == Ok(pid)))
}

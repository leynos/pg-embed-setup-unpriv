//! Measures how cluster start time grows with the number of concurrent
//! processes sharing one root (#289).
//!
//! Run by hand: `cargo test --test start_ramp -- --ignored --nocapture`. For
//! each `N` in `START_RAMP_COUNTS` (default `1,4,8,16`) it starts `N` child
//! processes at once against one warm root, each booting one cluster, and
//! prints the wall time and the phases the crate's own debug events record:
//! the wait for the install tree's setup lock, the `setup` step (installation
//! and `initdb`) and the `start` step. The phase whose time grows with `N` is
//! the serial one.
#![cfg(unix)]

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use pg_embedded_setup_unpriv::test_support;

/// Set in a child's environment to make it boot one cluster and report.
const CHILD: &str = "PG_EMBED_START_RAMP_CHILD";

/// The counts to measure, from the environment or the default.
fn counts() -> Vec<usize> {
    std::env::var("START_RAMP_COUNTS")
        .unwrap_or_else(|_| String::from("1,4,8,16"))
        .split(',')
        .filter_map(|count| count.trim().parse().ok())
        .collect()
}

/// Child: boot one cluster with debug events on stderr, then print the wall
/// time.
#[test]
#[ignore = "run only as a child of `start_ramp`"]
fn ramp_child() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .init();
    let started = Instant::now();
    let booted = test_support::shared_cluster_handle().is_ok();
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "RAMP wall_ms={} ok={booted}",
        started.elapsed().as_millis()
    )
    .expect("stdout is writable");
}

/// Returns the number after `key` in `line`.
fn field(line: &str, key: &str) -> Option<u128> {
    let (_, after) = line.split_once(key)?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// One child's timings, in milliseconds.
#[derive(Default)]
struct Timings {
    wall: u128,
    lock_wait: u128,
    setup: u128,
    start: u128,
}

/// Which phase a debug line records, and how long it took.
fn phase_of(line: &str) -> Option<(&'static str, u128)> {
    if line.contains("took the install tree's setup lock") {
        return Some(("lock", field(line, "waited_ms=")?));
    }
    if !line.contains("lifecycle step finished") {
        return None;
    }
    let elapsed = field(line, "elapsed_ms=")?;
    if line.contains("step=\"setup\"") {
        Some(("setup", elapsed))
    } else if line.contains("step=\"start\"") {
        Some(("start", elapsed))
    } else {
        None
    }
}

/// Reads a child's timings out of its stdout and stderr.
fn read_timings(stdout: &str, stderr: &str) -> Timings {
    let mut timings = Timings::default();
    for line in stdout.lines().filter(|line| line.contains("RAMP wall_ms=")) {
        timings.wall = field(line, "wall_ms=").unwrap_or_default();
    }
    for (phase, elapsed) in stderr.lines().filter_map(phase_of) {
        match phase {
            "lock" => timings.lock_wait = elapsed,
            "setup" => timings.setup = elapsed,
            _ => timings.start = elapsed,
        }
    }
    timings
}

/// Runs `count` children at once against `root` and returns their timings.
fn round(root: &Path, count: usize) -> std::io::Result<Vec<Timings>> {
    let binary = std::env::current_exe()?;
    let mut children = Vec::new();
    for _ in 0..count {
        children.push(
            Command::new(&binary)
                .args([
                    "--exact",
                    "ramp_child",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .env("PG_EMBED_ROOT", root)
                .env_remove("PG_DATA_DIR")
                .env_remove("PG_RUNTIME_DIR")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
    }
    let mut all = Vec::new();
    for child in children {
        let output = child.wait_with_output()?;
        all.push(read_timings(
            &String::from_utf8_lossy(&output.stdout),
            &String::from_utf8_lossy(&output.stderr),
        ));
    }
    Ok(all)
}

/// Returns the median and the maximum of `values`, or zeros when there are none.
fn median_and_max(mut values: Vec<u128>) -> (u128, u128) {
    values.sort_unstable();
    let middle = values.len().checked_div(2).unwrap_or_default();
    (
        values.get(middle).copied().unwrap_or_default(),
        values.last().copied().unwrap_or_default(),
    )
}

/// The measurement itself; prints a table and asserts nothing about speed.
#[test]
#[ignore = "a manual measurement, run with --ignored --nocapture"]
fn start_ramp() -> std::io::Result<()> {
    // A RAM-backed root (for example under /dev/shm) takes disk contention out
    // of `initdb`, so the serial step shows up on its own.
    let root: PathBuf = std::env::var_os("START_RAMP_ROOT").map_or_else(
        || Path::new(env!("CARGO_TARGET_TMPDIR")).join("start-ramp"),
        PathBuf::from,
    );
    // Created through the filesystem root, since the root is an absolute path.
    let base = cap_std::fs::Dir::open_ambient_dir("/", cap_std::ambient_authority())?;
    base.create_dir_all(root.strip_prefix("/").map_err(std::io::Error::other)?)?;
    // Two single-process rounds warm the download, the binary cache and the
    // install tree, so the rest measure the warm case.
    round(&root, 1)?;
    round(&root, 1)?;
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "N  | wall med/max | lock wait med/max | setup med/max | start med/max (ms)"
    )?;
    for count in counts() {
        let timings = round(&root, count)?;
        let (wall, wall_max) = median_and_max(timings.iter().map(|t| t.wall).collect());
        let (lock, lock_max) = median_and_max(timings.iter().map(|t| t.lock_wait).collect());
        let (setup, setup_max) = median_and_max(timings.iter().map(|t| t.setup).collect());
        let (start, start_max) = median_and_max(timings.iter().map(|t| t.start).collect());
        writeln!(
            out,
            "{count:<2} | {wall}/{wall_max} | {lock}/{lock_max} | {setup}/{setup_max} | \
             {start}/{start_max}"
        )?;
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(())
}

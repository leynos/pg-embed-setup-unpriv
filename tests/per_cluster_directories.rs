//! Per-cluster data directories under one root, driven across processes
//! (#261).
//!
//! Each case re-runs this test binary as child processes that share one
//! `PG_EMBED_ROOT`, with the environment set only on each child `Command`:
//!
//! - two children started together both get working clusters;
//! - a live child's directory survives another child's sweep, while a killed child's directory is
//!   swept and the server it orphaned is stopped;
//! - the killed child's lock is free while its orphaned server still runs, so the server did not
//!   inherit it;
//! - an explicit `PG_DATA_DIR` keeps a single directory at that path;
//! - startups in one install tree wait for its setup lock;
//! - a run killed mid-test leaves nothing the next run's sweep cannot reclaim.
//!
//! Every case works in a fixed root under `CARGO_TARGET_TMPDIR`, never a
//! throwaway one, so a run that is killed leaves its leftovers where the next
//! run's sweep finds them instead of stranding them in `/tmp`.
//!
//! Root runs take the worker path and are skipped with a logged reason.
#![cfg(unix)]

#[cfg(target_os = "linux")]
use std::time::Instant;
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

#[cfg(target_os = "linux")]
use fs4::FileExt;
use pg_embedded_setup_unpriv::{BootstrapError, test_support};

#[path = "support/cluster_children.rs"]
mod cluster_children;

#[cfg(target_os = "linux")]
use cluster_children::is_blocked_on_a_lock;
use cluster_children::{
    KillOnDrop,
    alive,
    ambient,
    child_mode,
    connected_dir,
    fixed_root,
    lock_is_free,
    open_lock,
    postmaster_pid,
    report,
    should_run,
    spawn_child,
    wait_until_unlocked,
};

/// Bootstraps the shared cluster and returns its data directory, after
/// checking the admin database answers.
fn bootstrap_and_connect() -> Result<PathBuf, BootstrapError> {
    let handle = test_support::shared_cluster_handle()?;
    if !handle.database_exists("postgres")? {
        return Err(BootstrapError::from(color_eyre::eyre::eyre!(
            "no postgres database"
        )));
    }
    Ok(handle.settings().data_dir.clone())
}

/// Child: bootstrap, report, and in `hold` mode keep the cluster until
/// stdin closes. In `orchestrate` mode, first start a `hold` child of its
/// own and report that child's directory, so a test can kill the
/// orchestrator as a harness kill would.
#[test]
#[ignore = "run only as a child of the per-cluster directory tests"]
fn cluster_child() {
    let Some(mode) = child_mode() else {
        return;
    };
    if mode == "orchestrate" {
        orchestrate().expect("the orchestrator runs");
        return;
    }
    let line = match bootstrap_and_connect() {
        Ok(dir) => format!("connected {}", dir.display()),
        Err(err) => format!("failed {err:?}"),
    };
    report(&line).expect("stdout is writable");
    if mode == "hold" {
        let mut rest = String::new();
        let _eof = std::io::stdin().read_line(&mut rest);
    }
}

/// Starts a `hold` child under this child's own root and waits, holding the
/// grandchild's stdin, until it is killed. Its report is the grandchild's.
fn orchestrate() -> std::io::Result<()> {
    let root = std::env::var("PG_EMBED_ROOT").map_err(std::io::Error::other)?;
    let mut held = spawn_child(Path::new(&root), "hold", &[])?;
    report(&held.report()?)?;
    std::thread::sleep(Duration::from_secs(600));
    Ok(())
}

/// Two children started together in one root both get working clusters, in
/// distinct data directories.
#[test]
fn two_concurrent_children_both_connect() {
    if !should_run() {
        return;
    }
    let root = fixed_root("two_concurrent").expect("a fixed root");
    let mut first = spawn_child(&root, "connect", &[]).expect("first child");
    let mut second = spawn_child(&root, "connect", &[]).expect("second child");

    let first_dir = connected_dir(&first.report().expect("first report")).expect("first connects");
    let second_dir =
        connected_dir(&second.report().expect("second report")).expect("second connects");

    assert_ne!(
        first_dir, second_dir,
        "each cluster needs its own data directory"
    );
    first.finish();
    second.finish();
}

/// A sweep keeps a live child's directory and removes a killed child's,
/// stopping the server the killed child orphaned. The killed child's lock
/// is free while that server still runs, so the server did not inherit it.
#[test]
fn a_sweep_keeps_the_live_and_clears_the_dead() {
    if !should_run() {
        return;
    }
    let root = fixed_root("sweep").expect("a fixed root");
    let mut live = spawn_child(&root, "hold", &[]).expect("live child");
    let live_dir = connected_dir(&live.report().expect("live report")).expect("live connects");
    let mut doomed = spawn_child(&root, "hold", &[]).expect("doomed child");
    let dead_dir =
        connected_dir(&doomed.report().expect("doomed report")).expect("doomed connects");
    let orphan = postmaster_pid(&dead_dir).expect("the doomed child's server");
    let _cleanup = KillOnDrop(Some(orphan));

    doomed.child.kill().expect("kill the doomed child");
    doomed.child.wait().expect("reap it");
    assert!(alive(orphan), "the server outlives its killed parent");
    assert!(
        lock_is_free(&dead_dir).expect("the slot's lock file"),
        "the orphaned server must not hold its dead owner's lock"
    );

    let mut sweeper = spawn_child(&root, "connect", &[]).expect("sweeping child");
    connected_dir(&sweeper.report().expect("sweeper report")).expect("sweeper connects");
    sweeper.finish();

    assert!(
        live_dir.join("PG_VERSION").exists(),
        "a live cluster must be kept"
    );
    assert!(!dead_dir.exists(), "a dead cluster must be swept");
    assert!(!alive(orphan), "the orphaned server must be stopped first");
    live.finish();
}

/// An explicit `PG_DATA_DIR` keeps a single data directory at that path,
/// with no per-cluster slot beside it.
#[test]
fn an_explicit_data_dir_wins() {
    if !should_run() {
        return;
    }
    let root = fixed_root("explicit").expect("a fixed root");
    let explicit = root.join("explicit-data");
    // A previous run's copy would make `initdb` refuse the directory.
    let _cleared = ambient(&root).and_then(|dir| dir.remove_dir_all("explicit-data"));
    let mut child = spawn_child(&root, "connect", &[("PG_DATA_DIR", &explicit)]).expect("child");

    let dir = connected_dir(&child.report().expect("report")).expect("connects");
    child.finish();

    assert_eq!(dir, explicit);
    let slots = ambient(&root)
        .and_then(|root_dir| root_dir.read_dir("data"))
        .map_or(0, Iterator::count);
    assert_eq!(slots, 0, "no per-cluster slot may be claimed");
}

/// A startup waits for the install tree's setup lock, so a cold root is
/// set up by one process at a time.
///
/// The kernel lists a process blocked on a lock in `/proc/locks`, so the
/// case waits for that evidence rather than for a timed window: a child that
/// ignored the lock would never appear there, and a slow host cannot make it.
#[cfg(target_os = "linux")]
#[test]
fn startup_waits_for_the_setup_lock() {
    if !should_run() {
        return;
    }
    let root = fixed_root("setup_lock").expect("a fixed root");
    ambient(&root)
        .and_then(|dir| dir.create_dir_all("install"))
        .expect("the install tree");
    let lock = open_lock(&root.join("install/.pg-embed-setup.lock"), true).expect("the lock file");
    FileExt::lock(&lock).expect("hold the setup lock");

    let mut child = spawn_child(&root, "connect", &[]).expect("child");
    let deadline = Instant::now() + Duration::from_secs(60);
    while !is_blocked_on_a_lock(child.child.id()).expect("read /proc/locks") {
        assert!(
            child.child.try_wait().expect("poll the child").is_none(),
            "the child finished without waiting for the setup lock"
        );
        assert!(
            Instant::now() < deadline,
            "the child never waited for the setup lock"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    FileExt::unlock(&lock).expect("release the setup lock");

    connected_dir(&child.report().expect("report")).expect("connects once released");
    child.finish();
}

/// A run killed mid-test leaves nothing the next run's sweep cannot reclaim.
///
/// The orchestrator stands for the harness: it starts a `hold` child and is
/// then killed with `SIGKILL`, so no guard runs. The child sees its stdin
/// close and exits, leaving its server running in an unswept slot. The next
/// run, here a fresh child, must clear both.
#[test]
fn a_killed_run_leaves_only_what_the_next_sweep_reclaims() {
    if !should_run() {
        return;
    }
    let root = fixed_root("killed_run").expect("a fixed root");
    let mut orchestrator = spawn_child(&root, "orchestrate", &[]).expect("orchestrator");
    let leftover = connected_dir(&orchestrator.report().expect("orchestrator report"))
        .expect("the grandchild connects");
    let orphan = postmaster_pid(&leftover).expect("the grandchild's server");
    let _cleanup = KillOnDrop(Some(orphan));

    orchestrator.child.kill().expect("kill the orchestrator");
    orchestrator.child.wait().expect("reap it");
    wait_until_unlocked(&leftover).expect("the orphaned lock is freed");

    let mut next = spawn_child(&root, "connect", &[]).expect("next run's child");
    connected_dir(&next.report().expect("next report")).expect("the next run connects");
    next.finish();

    assert!(!leftover.exists(), "the leftover slot must be swept");
    assert!(!alive(orphan), "the leftover server must be stopped");
}

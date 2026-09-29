//! Tests for per-cluster slots and the dead-slot sweep.

use std::{cell::RefCell, fs::OpenOptions};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};
use fs4::FileExt;
use rstest::{fixture, rstest};

use super::{claim_guard_for, claim_slot, has_live_peers, sweep_dead_slots};
use crate::bootstrap::prepare::orphan::{OrphanStop, ProcessId};

/// A data parent directory under a temporary root.
struct Parent {
    _temp: tempfile::TempDir,
    dir: Utf8PathBuf,
}

/// An empty data parent.
#[fixture]
fn parent() -> Result<Parent> {
    let temp = tempfile::tempdir()?;
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    let dir = root.join("data");
    std::fs::create_dir_all(&dir)?;
    Ok(Parent { _temp: temp, dir })
}

/// Records the PIDs it is asked to stop and answers with a fixed result.
struct RecordingStop {
    stopped: RefCell<Vec<ProcessId>>,
    succeeds: bool,
}

impl RecordingStop {
    /// A stop that always reports `succeeds`.
    fn new(succeeds: bool) -> Self {
        Self {
            stopped: RefCell::new(Vec::new()),
            succeeds,
        }
    }
}

impl OrphanStop for RecordingStop {
    fn stop(&self, pid: ProcessId) -> bool {
        self.stopped.borrow_mut().push(pid);
        self.succeeds
    }
}

/// Writes a slot named `name` whose lock file exists but is not held, as a
/// dead owner leaves it, with a data directory and a password file.
fn plant_dead_slot(parent: &Utf8PathBuf, name: &str) -> Result<()> {
    std::fs::create_dir_all(parent.join(name))?;
    std::fs::write(parent.join(name).join("PG_VERSION"), "17\n")?;
    std::fs::write(parent.join(format!("{name}.pgpass")), "secret")?;
    std::fs::write(parent.join(format!("{name}.lock")), "")?;
    Ok(())
}

/// Two claims in one process get distinct slots, each with its lock held.
#[rstest]
fn each_claim_gets_its_own_slot(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    let first = claim_slot(&dir.dir).expect("first claim");
    let second = claim_slot(&dir.dir).expect("second claim");

    assert_ne!(first.data_dir, second.data_dir);
    assert_eq!(first.data_dir.parent(), Some(dir.dir.as_path()));
    assert_eq!(
        first.password_file,
        Utf8PathBuf::from(format!("{}.pgpass", first.data_dir))
    );
    let lock = OpenOptions::new()
        .write(true)
        .open(format!("{}.lock", first.data_dir))
        .expect("the lock file exists");
    assert!(
        FileExt::try_lock(&lock).is_err(),
        "a claimed slot's lock must be held for the life of the process"
    );
}

/// A slot claim waits for the claim guard, so a full cleanup that holds it
/// across its probe and removal cannot be overtaken by a cluster claiming a
/// slot in between.
#[rstest]
fn a_claim_waits_for_the_claim_guard(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    let own = claim_slot(&dir.dir).expect("own slot");
    let guard = claim_guard_for(own.data_dir.as_std_path())
        .expect("the slot has a guard")
        .expect("take the guard");
    let (sender, receiver) = std::sync::mpsc::channel();
    let parent_dir = dir.dir.clone();
    let claimer = std::thread::spawn(move || {
        let claimed = claim_slot(&parent_dir);
        sender.send(()).expect("report the claim");
        claimed
    });

    assert!(
        receiver
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_err(),
        "a claim must wait while the guard is held"
    );
    drop(guard);
    receiver
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("the claim proceeds once the guard is free");
    claimer
        .join()
        .expect("join the claimer")
        .expect("the claim succeeds");
}

/// The claim guard sits among the slots but is not one: a sweep leaves it, and
/// counts nothing removed for it.
#[rstest]
fn the_claim_guard_is_not_a_slot(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    let own = claim_slot(&dir.dir).expect("own slot");
    assert!(dir.dir.join(".claim-guard").is_file(), "a claim leaves it");
    drop(claim_guard_for(own.data_dir.as_std_path()));

    let swept = sweep_dead_slots(&dir.dir, &RecordingStop::new(true)).expect("sweep");

    assert_eq!(swept, 0, "nothing here is a dead slot");
    assert!(dir.dir.join(".claim-guard").is_file(), "the sweep keeps it");
    assert!(
        !has_live_peers(own.data_dir.as_std_path()),
        "the guard is not a peer"
    );
}

/// A directory that is not a slot has no claim guard to take.
#[rstest]
fn only_a_slot_has_a_claim_guard(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    assert!(claim_guard_for(dir.dir.join("explicit").as_std_path()).is_none());
}

/// A cluster has live peers only when another slot beside it holds its lock,
/// so a directory that is not a slot, a lone slot and a dead neighbour have
/// none, and a held neighbour is one.
#[rstest]
fn peers_are_the_other_slots_whose_locks_are_held(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    let own = claim_slot(&dir.dir).expect("own slot");
    assert!(
        !has_live_peers(own.data_dir.as_std_path()),
        "a lone slot has no peers"
    );
    assert!(
        !has_live_peers(dir.dir.join("explicit").as_std_path()),
        "a directory that is not a slot has no peers"
    );

    plant_dead_slot(&dir.dir, "4242-3-0").expect("plant a dead neighbour");
    assert!(
        !has_live_peers(own.data_dir.as_std_path()),
        "a dead neighbour is not a peer"
    );

    let peer = claim_slot(&dir.dir).expect("a live peer");
    assert!(
        has_live_peers(own.data_dir.as_std_path()),
        "a held neighbouring lock is a peer"
    );
    assert!(has_live_peers(peer.data_dir.as_std_path()));
}

/// A slot whose lock nobody holds is swept: directory, password file and
/// lock file.
#[rstest]
fn a_dead_slot_is_swept(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    plant_dead_slot(&dir.dir, "4242-1-0").expect("plant a dead slot");

    let swept = sweep_dead_slots(&dir.dir, &RecordingStop::new(true)).expect("sweep");

    assert_eq!(swept, 1);
    assert!(!dir.dir.join("4242-1-0").exists());
    assert!(!dir.dir.join("4242-1-0.pgpass").exists());
    assert!(!dir.dir.join("4242-1-0.lock").exists());
}

/// A slot whose lock is held is live, whatever its name says, so it is
/// kept. Here the name carries a PID that cannot be alive, standing for a
/// PID reused by an unrelated process: the held lock, not the PID, decides.
#[rstest]
fn a_held_lock_keeps_the_slot_whatever_its_pid(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    plant_dead_slot(&dir.dir, "999999999-1-0").expect("plant a slot");
    let holder = OpenOptions::new()
        .write(true)
        .open(dir.dir.join("999999999-1-0.lock"))
        .expect("open the lock");
    FileExt::lock(&holder).expect("hold the lock");

    let swept = sweep_dead_slots(&dir.dir, &RecordingStop::new(true)).expect("sweep");

    assert_eq!(swept, 0);
    assert!(dir.dir.join("999999999-1-0").join("PG_VERSION").exists());
}

/// Conversely a slot named with this live process's PID but with no lock
/// held is dead, and is swept: a live PID does not keep a slot.
#[rstest]
fn a_live_pid_in_the_name_does_not_keep_an_unlocked_slot(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    let name = format!("{}-7-0", std::process::id());
    plant_dead_slot(&dir.dir, &name).expect("plant a slot");

    let swept = sweep_dead_slots(&dir.dir, &RecordingStop::new(true)).expect("sweep");

    assert_eq!(swept, 1);
    assert!(!dir.dir.join(&name).exists());
}

/// A dead slot whose server cannot be stopped is left in place.
#[cfg(target_os = "linux")]
#[rstest]
fn a_dead_slot_with_an_unstoppable_server_is_kept(parent: Result<Parent>) {
    let dir = parent.expect("data parent");
    plant_dead_slot(&dir.dir, "4242-2-0").expect("plant a dead slot");
    let slot = dir.dir.join("4242-2-0");
    let server = crate::bootstrap::prepare::orphan::tests::FakePostgres::spawn(&slot)
        .expect("a postgres for the slot");
    std::fs::write(slot.join("postmaster.pid"), format!("{}\n", server.pid()))
        .expect("point postmaster.pid at it");
    let stop = RecordingStop::new(false);
    assert_eq!(
        crate::bootstrap::prepare::orphan::server_state(&dir.dir.join("4242-2-0")),
        crate::bootstrap::prepare::orphan::ServerState::Running(server.pid()),
        "the stand-in must read as a running postgres"
    );

    let swept = sweep_dead_slots(&dir.dir, &stop).expect("sweep");

    assert_eq!(swept, 0);
    assert_eq!(*stop.stopped.borrow(), vec![server.pid()]);
    assert!(
        dir.dir.join("4242-2-0").exists(),
        "the directory must survive"
    );
}

//! Tests for per-cluster slots and the dead-slot sweep.

use std::{cell::RefCell, fs::OpenOptions};

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};
use fs4::FileExt;
use rstest::{fixture, rstest};

use super::{claim_slot, sweep_dead_slots};
use crate::bootstrap::prepare::orphan::OrphanStop;

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
    stopped: RefCell<Vec<u32>>,
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
    fn stop(&self, pid: u32) -> bool {
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
    let server = crate::bootstrap::prepare::orphan::tests::FakePostgres::spawn()
        .expect("a process named postgres");
    std::fs::write(
        dir.dir.join("4242-2-0").join("postmaster.pid"),
        format!("{}\n", server.pid()),
    )
    .expect("point postmaster.pid at it");
    let stop = RecordingStop::new(false);
    let comm = std::fs::read_to_string(format!("/proc/{}/comm", server.pid()));
    assert_eq!(
        crate::bootstrap::prepare::orphan::server_state(&dir.dir.join("4242-2-0")),
        crate::bootstrap::prepare::orphan::ServerState::Running(server.pid()),
        "the stand-in must read as a running postgres; comm was {comm:?}"
    );

    let swept = sweep_dead_slots(&dir.dir, &stop).expect("sweep");

    assert_eq!(swept, 0);
    assert_eq!(*stop.stopped.borrow(), vec![server.pid()]);
    assert!(
        dir.dir.join("4242-2-0").exists(),
        "the directory must survive"
    );
}

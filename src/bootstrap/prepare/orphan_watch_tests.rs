//! The watcher stops a server only after its owner's slot lock is released,
//! and only a process that is provably this slot's `postgres`.
//!
//! The owner is stood in for by a lock this test holds and then drops, which is
//! what the kernel does when a real owner dies. The "server" is a copy of
//! `sleep` named `postgres`, run in the data directory, so `/proc/<pid>/comm` and
//! `/proc/<pid>/cwd` read as a postmaster's do.

use std::{
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, Instant},
};

use color_eyre::eyre::{Result, ensure, eyre};
use fs4::FileExt;

use super::spawn_watcher;

/// A slot stand-in: a data directory, its lock file held by the "owner", and a
/// directory holding the renamed shell.
struct Slot {
    _root: tempfile::TempDir,
    data_dir: PathBuf,
    lock: PathBuf,
    owner: Option<File>,
}

impl Slot {
    fn new() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let data_dir = root.path().join("cluster");
        std::fs::create_dir(&data_dir)?;
        let lock = root.path().join("cluster.lock");
        let owner = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock)?;
        FileExt::lock(&owner)?;
        Ok(Self {
            _root: root,
            data_dir,
            lock,
            owner: Some(owner),
        })
    }

    /// Releases the lock, as the kernel does when the owner dies.
    fn owner_dies(&mut self) { self.owner = None; }

    fn name_pid_file(&self, pid: u32) -> Result<()> {
        std::fs::write(self.data_dir.join("postmaster.pid"), format!("{pid}\n"))?;
        Ok(())
    }
}

/// A long-lived child, killed and reaped on drop.
struct Stand(Child);

impl Drop for Stand {
    fn drop(&mut self) {
        let _killed = self.0.kill();
        let _reaped = self.0.wait();
    }
}

/// Starts `<name>` (a copy of `sleep`) in `cwd`.
///
/// A shell would do, but a non-interactive bash ignores `SIGQUIT`, which is the
/// signal a postmaster is stopped with; `sleep` takes it as a server does.
fn stand_in(name: &str, cwd: &Path, holder: &Path) -> Result<Stand> {
    let binary = holder.join(name);
    if !binary.exists() {
        std::fs::copy("/usr/bin/sleep", &binary)?;
    }
    let child = Command::new(&binary).arg("600").current_dir(cwd).spawn()?;
    let stand = Stand(child);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let comm =
            std::fs::read_to_string(format!("/proc/{}/comm", stand.0.id())).unwrap_or_default();
        if comm.trim() == name {
            return Ok(stand);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(eyre!("the stand-in never exec'd as {name}"))
}

fn wait_exit(child: &mut Child, within: Duration) -> Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(None)
}

const WAIT: Duration = Duration::from_secs(20);

/// How long a process that must survive is watched after the watcher has
/// ended: a signal takes a moment to end its target, so an immediate look
/// would pass a watcher that had just sent one.
const SETTLE: Duration = Duration::from_millis(500);

/// While the owner holds the slot lock the watcher does nothing, even to a
/// server that is provably this slot's; once the lock is released it sends the
/// signal.
#[test]
fn a_matching_server_is_signalled_only_after_the_owner_dies() -> Result<()> {
    let mut slot = Slot::new()?;
    let holder = tempfile::tempdir()?;
    let mut server = stand_in("postgres", &slot.data_dir, holder.path())?;
    slot.name_pid_file(server.0.id())?;
    let mut watcher = spawn_watcher(&slot.lock, &slot.data_dir)?;

    std::thread::sleep(Duration::from_millis(300));
    ensure!(
        server.0.try_wait()?.is_none() && watcher.try_wait()?.is_none(),
        "the watcher must wait for the owner's lock"
    );

    slot.owner_dies();

    ensure!(
        wait_exit(&mut watcher, WAIT)?.is_some(),
        "the watcher ends once it has acted"
    );
    let status = wait_exit(&mut server.0, WAIT)?
        .ok_or_else(|| eyre!("the matching server was not signalled"))?;
    ensure!(!status.success(), "the server ends by signal: {status:?}");
    Ok(())
}

/// A live process the pid file names that is not called `postgres` survives,
/// however the file got there.
#[test]
fn a_live_process_not_named_postgres_survives() -> Result<()> {
    let mut slot = Slot::new()?;
    let holder = tempfile::tempdir()?;
    let mut bystander = stand_in("bystander", &slot.data_dir, holder.path())?;
    slot.name_pid_file(bystander.0.id())?;
    let mut watcher = spawn_watcher(&slot.lock, &slot.data_dir)?;

    slot.owner_dies();

    ensure!(wait_exit(&mut watcher, WAIT)?.is_some(), "the watcher ends");
    ensure!(
        wait_exit(&mut bystander.0, SETTLE)?.is_none(),
        "a process that is not postgres must not be signalled"
    );
    Ok(())
}

/// A `postgres` running somewhere else, such as another cluster's server that
/// a recycled PID now names, survives.
#[test]
fn a_postgres_in_another_directory_survives() -> Result<()> {
    let mut slot = Slot::new()?;
    let holder = tempfile::tempdir()?;
    let elsewhere = tempfile::tempdir()?;
    let mut stranger = stand_in("postgres", elsewhere.path(), holder.path())?;
    slot.name_pid_file(stranger.0.id())?;
    let mut watcher = spawn_watcher(&slot.lock, &slot.data_dir)?;

    slot.owner_dies();

    ensure!(wait_exit(&mut watcher, WAIT)?.is_some(), "the watcher ends");
    ensure!(
        wait_exit(&mut stranger.0, SETTLE)?.is_none(),
        "a postgres serving another directory must not be signalled"
    );
    Ok(())
}

/// With no `postmaster.pid`, the normal teardown case, or a garbage one, the
/// watcher ends without acting.
#[rstest::rstest]
#[case::absent(None)]
#[case::garbage(Some("not a pid\n"))]
fn a_missing_or_garbage_pid_file_ends_quietly(#[case] contents: Option<&str>) -> Result<()> {
    let mut slot = Slot::new()?;
    if let Some(text) = contents {
        std::fs::write(slot.data_dir.join("postmaster.pid"), text)?;
    }
    let mut watcher = spawn_watcher(&slot.lock, &slot.data_dir)?;
    slot.owner_dies();
    let status = wait_exit(&mut watcher, WAIT)?.ok_or_else(|| eyre!("watcher did not end"))?;
    ensure!(status.success(), "the watcher ends cleanly: {status:?}");
    Ok(())
}

/// A cluster stopped normally ends its watcher, which then cannot act on
/// the server it was guarding: a later owner death signals nothing.
#[test]
fn a_released_watcher_no_longer_acts_on_owner_death() -> Result<()> {
    use super::{release_watcher, watch_slot_owner};

    let mut slot = Slot::new()?;
    let holder = tempfile::tempdir()?;
    let mut server = stand_in("postgres", &slot.data_dir, holder.path())?;
    slot.name_pid_file(server.0.id())?;
    ensure!(watch_slot_owner(&slot.data_dir), "a slot gets a watcher");

    ensure!(release_watcher(&slot.data_dir), "the watcher is released");
    ensure!(
        !release_watcher(&slot.data_dir),
        "a second release finds nothing"
    );
    slot.owner_dies();

    ensure!(
        wait_exit(&mut server.0, SETTLE)?.is_none(),
        "a released watcher must not signal the server"
    );
    Ok(())
}

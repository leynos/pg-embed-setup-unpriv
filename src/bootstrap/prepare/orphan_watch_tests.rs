//! The watcher stops a server only after its owner's slot lock is released,
//! and only a process that is provably this slot's `postgres`.
//!
//! The owner is stood in for by a lock this test holds and then drops, which is
//! what the kernel does when a real owner dies. The "server" is a copy of
//! `sleep` named `postgres`, run in the data directory, so `/proc/<pid>/comm` and
//! `/proc/<pid>/cwd` read as a postmaster's do. It is `sleep` and not a shell
//! because a non-interactive bash ignores `SIGQUIT`, the signal a postmaster is
//! stopped with, so a shell stand-in would survive a watcher that worked.

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

/// Finds `sleep` on `PATH`, so a host that keeps it elsewhere than `/usr/bin`
/// (NixOS, Guix, a minimal container) still runs these tests.
fn sleep_on_path() -> Result<PathBuf> {
    let paths = std::env::var_os("PATH").ok_or_else(|| eyre!("PATH is not set"))?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join("sleep"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| eyre!("no `sleep` binary on PATH"))
}

/// Starts `<name>` (a copy of `sleep`) in `cwd`.
///
/// A shell would do, but a non-interactive bash ignores `SIGQUIT`, which is the
/// signal a postmaster is stopped with; `sleep` takes it as a server does.
fn stand_in(name: &str, cwd: &Path, holder: &Path) -> Result<Stand> {
    let binary = holder.join(name);
    if !binary.exists() {
        std::fs::copy(sleep_on_path()?, &binary)?;
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

/// A live process the pid file names survives when it is not this slot's
/// `postgres`: one not called `postgres`, or a `postgres` serving another
/// directory, as a recycled PID would be.
#[rstest::rstest]
#[case::not_named_postgres("bystander", false)]
#[case::postgres_elsewhere("postgres", true)]
fn a_live_process_that_is_not_this_slots_postgres_survives(
    #[case] name: &str,
    #[case] elsewhere: bool,
) -> Result<()> {
    let mut slot = Slot::new()?;
    let holder = tempfile::tempdir()?;
    let other_dir = tempfile::tempdir()?;
    let cwd = if elsewhere {
        other_dir.path()
    } else {
        &slot.data_dir
    };
    let mut process = stand_in(name, cwd, holder.path())?;
    slot.name_pid_file(process.0.id())?;
    let mut watcher = spawn_watcher(&slot.lock, &slot.data_dir)?;

    slot.owner_dies();

    ensure!(wait_exit(&mut watcher, WAIT)?.is_some(), "the watcher ends");
    ensure!(
        wait_exit(&mut process.0, SETTLE)?.is_none(),
        "{name} (elsewhere: {elsewhere}) must not be signalled"
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

/// The watcher inherits nothing from this process but its three standard
/// streams, which are `/dev/null`, and the slot's lock file it waits on.
///
/// A descriptor leaked into a detached process outlives its owner for as long
/// as the watcher waits, so a pipe or file the owner holds would be kept open
/// by it. The check reads `/proc/<pid>/fd` of the process once it has become
/// `flock`, so it sees what `setsid` and the exec left.
#[test]
fn the_watcher_inherits_only_dev_null_and_the_lock_file() -> Result<()> {
    let slot = Slot::new()?;
    // A pipe without close-on-exec, as a test harness gives its test process:
    // a plain `pipe(2)` leaves both ends inheritable, so a watcher that did not
    // close them would hold the pipe open.
    let (_read_end, _write_end) = nix::unistd::pipe()?;
    let mut watcher = spawn_watcher(&slot.lock, &slot.data_dir)?;
    let pid = watcher.id();
    let deadline = Instant::now() + WAIT;
    while std::fs::read_to_string(format!("/proc/{pid}/comm"))?.trim() != "flock" {
        ensure!(Instant::now() < deadline, "the watcher never became flock");
        std::thread::sleep(Duration::from_millis(5));
    }
    let lock = std::fs::canonicalize(&slot.lock)?;
    let mut leaked = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))? {
        let target = std::fs::read_link(entry?.path())?;
        if target != Path::new("/dev/null") && target != lock {
            leaked.push(target);
        }
    }
    watcher.kill()?;
    watcher.wait()?;
    ensure!(
        leaked.is_empty(),
        "descriptors leaked into the watcher: {leaked:?}"
    );
    Ok(())
}

/// Only `off` turns the watcher off, whatever else the variable holds.
#[rstest::rstest]
#[case::off(Some("off"), true)]
#[case::unset(None, false)]
#[case::on(Some("on"), false)]
#[case::empty(Some(""), false)]
fn only_the_value_off_disables_the_watcher(#[case] value: Option<&str>, #[case] disabled: bool) {
    let lookup = |key: &str| {
        (key == super::OPT_OUT_VAR)
            .then(|| value.map(std::ffi::OsString::from))
            .flatten()
    };
    assert_eq!(super::is_disabled(lookup), disabled);
}

mod metrics {
    //! Each watcher decision reaches a consumer's recorder as one bounded
    //! outcome. The recorder is process-wide, so these carry the same
    //! `serial` key as the crate's other metric tests, and assert containment
    //! because the cleanup tests call the watcher without that key.

    use std::sync::{Arc, Mutex, PoisonError};

    use color_eyre::eyre::{Result, ensure};
    use serial_test::serial;

    use super::{Slot, release_watcher_for_test};
    use crate::{
        bootstrap::prepare::orphan_watch::{release_watcher, watch_slot_owner},
        observability::{
            Metric,
            MetricsRecorder,
            OrphanWatcherOutcomeMetric as Outcome,
            install_metrics_recorder,
        },
    };

    #[derive(Default)]
    struct Collected(Mutex<Vec<Metric>>);

    impl MetricsRecorder for Collected {
        fn record(&self, metric: Metric) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(metric);
        }
    }

    fn collected(body: impl FnOnce()) -> Vec<Metric> {
        let recorder = Arc::new(Collected::default());
        let guard = install_metrics_recorder(Arc::clone(&recorder) as Arc<dyn MetricsRecorder>);
        body();
        drop(guard);
        let seen = recorder.0.lock().unwrap_or_else(PoisonError::into_inner);
        seen.clone()
    }

    /// A directory that is not a slot is counted as such.
    #[test]
    #[serial(metrics_recorder)]
    fn a_directory_that_is_not_a_slot_is_counted() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let seen = collected(|| {
            let _watching = watch_slot_owner(dir.path());
        });
        ensure!(
            seen.contains(&Metric::OrphanWatcher(Outcome::NotASlot)),
            "{seen:?}"
        );
        Ok(())
    }

    /// A slot is counted as spawned, and releasing it as released.
    #[test]
    #[serial(metrics_recorder)]
    fn spawning_and_releasing_a_watcher_are_counted() -> Result<()> {
        let slot = Slot::new()?;
        let seen = collected(|| {
            let _watching = watch_slot_owner(&slot.data_dir);
            let _released = release_watcher(&slot.data_dir);
        });
        release_watcher_for_test(&slot.data_dir);
        ensure!(
            seen.contains(&Metric::OrphanWatcher(Outcome::Spawned)),
            "{seen:?}"
        );
        ensure!(
            seen.contains(&Metric::OrphanWatcher(Outcome::Released)),
            "{seen:?}"
        );
        Ok(())
    }
}

/// Ends any watcher left registered for `data_dir`, so a failed assertion does
/// not leave one waiting for the test process to exit.
fn release_watcher_for_test(data_dir: &Path) { let _released = super::release_watcher(data_dir); }

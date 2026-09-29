//! Tests for stopping a server orphaned in a dead slot.

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};

use super::{OrphanStop, ServerState, server_state, stop_orphaned_server};

/// A child process whose name is `postgres`, standing in for an orphaned
/// server: `sleep`, copied under that name so `/proc/<pid>/comm` reads
/// `postgres`. Killed and reaped on drop.
#[cfg(target_os = "linux")]
pub(crate) struct FakePostgres {
    child: std::process::Child,
    _dir: tempfile::TempDir,
}

#[cfg(target_os = "linux")]
impl FakePostgres {
    /// Starts the stand-in.
    pub(crate) fn spawn() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let binary = dir.path().join("postgres");
        std::fs::copy("/bin/sleep", &binary)?;
        let child = std::process::Command::new(&binary).arg("60").spawn()?;
        let server = Self { child, _dir: dir };
        server.wait_for_name()?;
        Ok(server)
    }

    /// Waits until the process has exec'd and reads as `postgres`.
    ///
    /// A spawned child can be observed between the fork and the exec, when
    /// `/proc/<pid>/comm` still holds the parent's name. That window is wide
    /// enough on a busy CI runner to make a fresh stand-in read as some other
    /// process, so the constructor does not return until the name is right.
    fn wait_for_name(&self) -> Result<()> {
        let comm = format!("/proc/{}/comm", self.pid());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if std::fs::read_to_string(&comm).is_ok_and(|name| name.trim_end() == "postgres") {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Err(eyre!("the stand-in never read as postgres"))
    }

    /// Returns the stand-in's PID.
    pub(crate) fn pid(&self) -> u32 { self.child.id() }
}

#[cfg(target_os = "linux")]
impl Drop for FakePostgres {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _reaped = self.child.wait();
    }
}

/// A stop that never succeeds, and one that always does.
struct FixedStop(bool);

impl OrphanStop for FixedStop {
    fn stop(&self, _pid: u32) -> bool { self.0 }
}

/// A data directory with an optional `postmaster.pid` naming `pid`.
fn data_dir(pid: Option<u32>) -> Result<(tempfile::TempDir, Utf8PathBuf)> {
    let temp = tempfile::tempdir()?;
    let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    if let Some(value) = pid {
        std::fs::write(dir.join("postmaster.pid"), format!("{value}\n/data\n"))?;
    }
    Ok((temp, dir))
}

/// No `postmaster.pid` means no server, so the directory is free.
#[test]
fn no_pid_file_means_no_server() {
    let (_temp, dir) = data_dir(None).expect("data dir");
    assert_eq!(server_state(&dir), ServerState::Absent);
    assert!(stop_orphaned_server(&dir, &FixedStop(false)));
}

/// A PID file naming a process that is gone means no server.
#[cfg(unix)]
#[test]
fn a_dead_pid_means_no_server() {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn true");
    let pid = child.id();
    child.wait().expect("reap true");
    let (_temp, dir) = data_dir(Some(pid)).expect("data dir");
    assert_eq!(server_state(&dir), ServerState::Absent);
}

/// A live process that is not `postgres` holds no server: its PID was
/// reused, so the directory is free and nothing is signalled.
#[cfg(target_os = "linux")]
#[test]
fn a_live_non_postgres_pid_means_no_server() {
    let (_temp, dir) = data_dir(Some(std::process::id())).expect("data dir");
    assert_eq!(server_state(&dir), ServerState::Absent);
    assert!(stop_orphaned_server(&dir, &FixedStop(false)));
}

/// A live `postgres` process is a running server, and the directory is
/// free only if stopping it succeeds.
#[cfg(target_os = "linux")]
#[test]
fn a_live_postgres_must_be_stopped_first() {
    let server = FakePostgres::spawn().expect("a process named postgres");
    let (_temp, dir) = data_dir(Some(server.pid())).expect("data dir");

    assert_eq!(server_state(&dir), ServerState::Running(server.pid()));
    assert!(!stop_orphaned_server(&dir, &FixedStop(false)));
    assert!(stop_orphaned_server(&dir, &FixedStop(true)));
}

/// The real stop sends `SIGQUIT` and waits for the process to exit.
#[cfg(target_os = "linux")]
#[test]
fn the_signal_stop_ends_the_process() {
    use super::SignalStop;

    // An orphan's parent is init, which reaps it; a child of this test is
    // not reaped until waited on, so reap it on a thread as init would.
    let mut child = std::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .expect("spawn sleep");
    let pid = child.id();
    let reaper = std::thread::spawn(move || child.wait());

    assert!(
        SignalStop.stop(pid),
        "the process should exit after SIGQUIT"
    );
    let _status = reaper.join().expect("join the reaper");
}

/// A command name counts as `postgres` by its final path component, which is
/// what `ps -o comm=` prints on macOS; an empty name is a process that is
/// gone.
#[cfg(unix)]
#[rstest::rstest]
#[case::bare("postgres\n", true)]
#[case::path("/opt/pg/17.4.0/bin/postgres\n", true)]
#[case::other("sleep\n", false)]
#[case::prefix("/usr/bin/postgres-helper\n", false)]
#[case::suffix("/usr/bin/notpostgres\n", false)]
#[case::gone("", false)]
fn a_command_name_is_postgres_by_its_last_component(#[case] comm: &str, #[case] expected: bool) {
    assert_eq!(super::names_postgres(comm), expected);
}

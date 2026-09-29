//! Tests for stopping a server orphaned in a dead slot.

use camino::{Utf8Path, Utf8PathBuf};
use color_eyre::eyre::{Result, eyre};

use super::{OrphanStop, ProcessId, ServerState, server_state, stop_orphaned_server};

/// A child process standing in for an orphaned server: a shell copied under
/// the name `postgres`, so `/proc/<pid>/comm` reads `postgres`, and started in
/// `data_dir` with `-D <data_dir>`, as a postmaster is. Killed and reaped on
/// drop.
#[cfg(target_os = "linux")]
pub(crate) struct FakePostgres {
    child: std::process::Child,
    _dir: tempfile::TempDir,
}

#[cfg(target_os = "linux")]
impl FakePostgres {
    /// Starts the stand-in for a server running `data_dir`.
    pub(crate) fn spawn(data_dir: &Utf8Path) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let binary = dir.path().join("postgres");
        std::fs::copy("/bin/sh", &binary)?;
        // With `-c`, the words after the script are `$0` and `$1`, so the
        // command line reads `postgres -c <script> -D <data_dir>`.
        // `read` blocks on the piped stdin, which this struct keeps open, so the
        // stand-in starts no child of its own that could outlive it.
        let child = std::process::Command::new(&binary)
            .args(["-c", "read line", "-D", data_dir.as_str()])
            .current_dir(data_dir)
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        let server = Self { child, _dir: dir };
        server.wait_for_identity(data_dir)?;
        Ok(server)
    }

    /// Waits until the process has exec'd and reads as a `postgres` server
    /// for `data_dir`.
    ///
    /// A spawned child can be observed between the fork and the exec, when
    /// `/proc` still shows the parent's name and command line. That window is
    /// wide enough on a busy CI runner to make a fresh stand-in read as some
    /// other process, so the constructor does not return until it is right.
    fn wait_for_identity(&self, data_dir: &Utf8Path) -> Result<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if super::is_server_for(self.pid(), data_dir) == Some(true) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Err(eyre!("the stand-in never read as a postgres server"))
    }

    /// Returns the stand-in's PID.
    pub(crate) fn pid(&self) -> ProcessId { ProcessId::new(self.child.id()) }
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
    fn stop(&self, _pid: ProcessId) -> bool { self.0 }
}

/// An empty data directory.
fn data_dir() -> Result<(tempfile::TempDir, Utf8PathBuf)> {
    let temp = tempfile::tempdir()?;
    let dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    Ok((temp, dir))
}

/// Writes a `postmaster.pid` naming `pid` into `dir`.
fn write_pid(dir: &Utf8Path, pid: ProcessId) -> Result<()> {
    std::fs::write(dir.join("postmaster.pid"), format!("{pid}\n{dir}\n"))?;
    Ok(())
}

/// No `postmaster.pid` means no server, so the directory is free.
#[test]
fn no_pid_file_means_no_server() {
    let (_temp, dir) = data_dir().expect("data dir");
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
    let (_temp, dir) = data_dir().expect("data dir");
    write_pid(&dir, ProcessId::new(pid)).expect("pid file");
    assert_eq!(server_state(&dir), ServerState::Absent);
}

/// A live process that is not `postgres` holds no server: its PID was
/// reused, so the directory is free and nothing is signalled.
#[cfg(target_os = "linux")]
#[test]
fn a_live_non_postgres_pid_means_no_server() {
    let (_temp, dir) = data_dir().expect("data dir");
    write_pid(&dir, ProcessId::new(std::process::id())).expect("pid file");
    assert_eq!(server_state(&dir), ServerState::Absent);
    assert!(stop_orphaned_server(&dir, &FixedStop(false)));
}

/// A live `postgres` serving some other data directory is not this slot's
/// server, whatever the stale `postmaster.pid` says: its PID was reused by an
/// unrelated database, which must be left alone.
#[cfg(target_os = "linux")]
#[test]
fn a_postgres_serving_another_directory_means_no_server() {
    let (_other_temp, other) = data_dir().expect("the other server's directory");
    let server = FakePostgres::spawn(&other).expect("a postgres for another directory");
    let (_temp, dir) = data_dir().expect("data dir");
    write_pid(&dir, server.pid()).expect("pid file");

    assert_eq!(server_state(&dir), ServerState::Absent);
    assert!(stop_orphaned_server(&dir, &FixedStop(false)));
}

/// A `postgres` whose working directory cannot be read, as for another
/// user's process, is not shown to be absent: the slot stays unconfirmed. A
/// killed child that has not been reaped is a zombie, which keeps its name but
/// has no working directory to read.
#[cfg(target_os = "linux")]
#[test]
fn a_server_whose_directory_cannot_be_read_is_unconfirmed() {
    let (_temp, dir) = data_dir().expect("data dir");
    let mut server = FakePostgres::spawn(&dir).expect("a postgres for this directory");
    write_pid(&dir, server.pid()).expect("pid file");
    server.child.kill().expect("kill without reaping");
    let cwd = format!("/proc/{}/cwd", server.pid());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::fs::read_link(&cwd).is_ok() {
        assert!(
            std::time::Instant::now() < deadline,
            "the killed stand-in never became a zombie"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    assert_eq!(server_state(&dir), ServerState::Unconfirmed(server.pid()));
    assert!(!stop_orphaned_server(&dir, &FixedStop(true)));
}

/// A live `postgres` process is a running server, and the directory is
/// free only if stopping it succeeds.
#[cfg(target_os = "linux")]
#[test]
fn a_live_postgres_must_be_stopped_first() {
    let (_temp, dir) = data_dir().expect("data dir");
    let server = FakePostgres::spawn(&dir).expect("a postgres for this directory");
    write_pid(&dir, server.pid()).expect("pid file");

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
        SignalStop.stop(ProcessId::new(pid)),
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

/// A command line names a data directory only as a whole argument: after it
/// comes the end of the line or a space, as `ps` prints.
#[cfg(all(unix, not(target_os = "linux")))]
#[rstest::rstest]
#[case::spaces("postgres -D /d/1-2-0 -p 5432", true)]
#[case::end("postgres -D /d/1-2-0", true)]
#[case::longer_name("postgres -D /d/1-2-01", false)]
#[case::other_dir("postgres -D /d/9-9-9", false)]
#[case::none("postgres", false)]
fn a_command_line_names_a_directory_as_a_whole_argument(
    #[case] command_line: &str,
    #[case] expected: bool,
) {
    assert_eq!(
        super::names_data_dir(command_line, Utf8Path::new("/d/1-2-0")),
        expected
    );
}

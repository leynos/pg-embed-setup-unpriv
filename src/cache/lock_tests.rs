//! Tests for cache lock acquisition.
//!
//! The in-process cases hold a guard on one thread and contend for the same
//! key from another. A kernel lock is the property under test, so every
//! contender opens its own handle rather than sharing one: `flock(2)` and
//! `LockFileEx` agree on separate handles and diverge on re-entrant ones.

use std::{sync::mpsc, thread, time::Duration};

use camino::{Utf8Path, Utf8PathBuf};
use rstest::{fixture, rstest};
use tempfile::TempDir;

use super::*;

#[path = "lock_process_tests.rs"]
mod process;

/// Time a contender gets to report that it is about to acquire.
const READY_TIMEOUT: Duration = Duration::from_secs(10);

/// Window in which a conflicting contender must stay blocked.
///
/// The window is evidence, not proof, of kernel blocking: a contender whose
/// thread is merely slow also misses it. The positive check after the holder
/// releases is what proves the contender eventually makes progress.
const CONFLICT_WINDOW: Duration = Duration::from_millis(250);

/// Time a contender gets to acquire once it may proceed.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// Fixture providing a temporary cache directory as a UTF-8 path.
#[fixture]
fn cache_fixture() -> io::Result<(TempDir, Utf8PathBuf)> {
    let temp = tempfile::tempdir()?;
    let cache_dir = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| io::Error::other(format!("non-UTF-8 temp path: {}", path.display())))?;
    Ok((temp, cache_dir))
}

/// Acquires `lock_type` for `version`, dispatching without spelling the
/// constructors at every call site.
fn acquire(cache_dir: &Utf8Path, version: &str, lock_type: LockType) -> io::Result<CacheLock> {
    match lock_type {
        LockType::Exclusive => CacheLock::acquire_exclusive(cache_dir, version),
        LockType::Shared => CacheLock::acquire_shared(cache_dir, version),
    }
}

/// One side of a contention experiment.
#[derive(Clone, Copy)]
struct Side<'a> {
    /// Version key whose lock this side requests.
    version: &'a str,
    /// Whether the side requests exclusive or shared access.
    lock_type: LockType,
}

impl<'a> Side<'a> {
    const fn new(version: &'a str, lock_type: LockType) -> Self { Self { version, lock_type } }
}

/// A held guard and the caller that tries to enter whilst it lives.
struct Contention<'a> {
    holder: Side<'a>,
    contender: Side<'a>,
    /// Whether the two sides must exclude one another.
    expect_exclusion: bool,
}

/// Asserts how a held guard affects a second caller.
///
/// The contender runs on its own thread and opens its own handle, so the test
/// exercises kernel contention rather than the re-entrancy rules of a single
/// handle.
fn assert_contention(cache_dir: &Utf8Path, case: &Contention<'_>) {
    let held = acquire(cache_dir, case.holder.version, case.holder.lock_type).expect("holder lock");

    let (tx, rx) = mpsc::channel();
    let contender_dir = cache_dir.to_owned();
    let contender_version = case.contender.version.to_owned();
    let contender_type = case.contender.lock_type;
    let contender = thread::spawn(move || {
        tx.send("ready").ok();
        let lock = acquire(&contender_dir, &contender_version, contender_type);
        tx.send("acquired").ok();
        lock
    });

    assert_eq!(
        rx.recv_timeout(READY_TIMEOUT),
        Ok("ready"),
        "the contender must start before the holder releases"
    );

    if case.expect_exclusion {
        assert!(
            rx.recv_timeout(CONFLICT_WINDOW).is_err(),
            "a conflicting lock must not be granted while the holder lives"
        );
        drop(held);
        assert_eq!(
            rx.recv_timeout(ACQUIRE_TIMEOUT),
            Ok("acquired"),
            "the contender must acquire once the holder releases"
        );
    } else {
        assert_eq!(
            rx.recv_timeout(ACQUIRE_TIMEOUT),
            Ok("acquired"),
            "a compatible lock must be granted while the holder lives"
        );
        drop(held);
    }

    contender
        .join()
        .expect("contender thread")
        .expect("contender lock");
}

/// The lock file lives beside the cache it guards, on every platform.
///
/// Keeping it under the cache directory means two caches that happen to
/// share a version string do not contend.
#[rstest]
#[case::exclusive("17.4.0", true)]
#[case::shared("16.3.0", false)]
fn acquire_lock_creates_lock_file(
    cache_fixture: io::Result<(TempDir, Utf8PathBuf)>,
    #[case] version: &str,
    #[case] exclusive: bool,
) {
    let (temp, cache_dir) = cache_fixture.expect("cache fixture");
    let _lock = if exclusive {
        CacheLock::acquire_exclusive(&cache_dir, version).expect("acquire lock")
    } else {
        CacheLock::acquire_shared(&cache_dir, version).expect("acquire lock")
    };

    let lock_path = temp
        .path()
        .join(LOCKS_SUBDIR)
        .join(format!("{version}.lock"));
    assert!(lock_path.exists(), "lock file should be created");
}

/// A held guard controls whether a second caller may enter.
///
/// All four mode pairs run on every platform, so neither `flock(2)` nor
/// `LockFileEx` can silently weaken one direction.
#[rstest]
#[case::exclusive_exclusive(LockType::Exclusive, LockType::Exclusive, true)]
#[case::shared_shared(LockType::Shared, LockType::Shared, false)]
#[case::shared_exclusive(LockType::Shared, LockType::Exclusive, true)]
#[case::exclusive_shared(LockType::Exclusive, LockType::Shared, true)]
fn a_held_lock_controls_a_second_caller(
    cache_fixture: io::Result<(TempDir, Utf8PathBuf)>,
    #[case] holder: LockType,
    #[case] contender: LockType,
    #[case] expect_exclusion: bool,
) {
    let (_temp, cache_dir) = cache_fixture.expect("cache fixture");
    assert_contention(
        &cache_dir,
        &Contention {
            holder: Side::new("17.4.0", holder),
            contender: Side::new("17.4.0", contender),
            expect_exclusion,
        },
    );
}

/// Different versions never contend, whatever the lock types.
#[rstest]
#[case::exclusive(LockType::Exclusive)]
#[case::shared(LockType::Shared)]
fn different_versions_have_separate_locks(
    cache_fixture: io::Result<(TempDir, Utf8PathBuf)>,
    #[case] lock_type: LockType,
) {
    let (_temp, cache_dir) = cache_fixture.expect("cache fixture");
    assert_contention(
        &cache_dir,
        &Contention {
            holder: Side::new("17.4.0", lock_type),
            contender: Side::new("16.3.0", lock_type),
            expect_exclusion: false,
        },
    );
}

/// Version strings that name anything but one path component are rejected,
/// for both lock types.
#[rstest]
#[case::parent_dir("..")]
#[case::path_separator("foo/bar")]
#[case::parent_in_path("../17.4.0")]
#[case::absolute_path("/etc/passwd")]
fn acquire_rejects_invalid_version_strings(
    cache_fixture: io::Result<(TempDir, Utf8PathBuf)>,
    #[case] invalid_version: &str,
) {
    let (_temp, cache_dir) = cache_fixture.expect("cache fixture");

    let exclusive_err = CacheLock::acquire_exclusive(&cache_dir, invalid_version)
        .expect_err("acquire_exclusive should reject invalid version");
    assert_eq!(
        exclusive_err.kind(),
        io::ErrorKind::InvalidInput,
        "error kind should be InvalidInput for: {invalid_version}"
    );

    let shared_err = CacheLock::acquire_shared(&cache_dir, invalid_version)
        .expect_err("acquire_shared should reject invalid version");
    assert_eq!(
        shared_err.kind(),
        io::ErrorKind::InvalidInput,
        "error kind should be InvalidInput for: {invalid_version}"
    );
}

/// Child process for the cross-process cases.
///
/// Reads its cache coordinates from the environment, acquires the named lock,
/// reports that fact, then holds the guard until stdin reaches EOF. The
/// parent harness starts it with `--ignored`, so a normal run never executes
/// it as a standalone case.
#[test]
#[ignore = "spawned by the cross-process cache lock tests"]
fn cache_lock_child() {
    use std::io::Read;

    let cache_dir =
        Utf8PathBuf::from(std::env::var(process::CHILD_CACHE_DIR).expect("child cache directory"));
    let version = std::env::var(process::CHILD_VERSION).expect("child version");
    let mode = std::env::var(process::CHILD_MODE).expect("child mode");
    let lock_type = match mode.as_str() {
        "exclusive" => LockType::Exclusive,
        "shared" => LockType::Shared,
        other => panic!("unknown child lock mode: {other}"),
    };

    let _lock = acquire(&cache_dir, &version, lock_type).expect("child lock");
    process::report("acquired").expect("report acquisition");

    let mut sink = String::new();
    std::io::stdin()
        .read_to_string(&mut sink)
        .expect("read child stdin to EOF");
}

/// An exclusive parent blocks an exclusive child until the parent releases.
#[rstest]
fn an_exclusive_parent_blocks_an_exclusive_child(
    cache_fixture: io::Result<(TempDir, Utf8PathBuf)>,
) {
    let (_temp, cache_dir) = cache_fixture.expect("cache fixture");
    let held = CacheLock::acquire_exclusive(&cache_dir, "17.4.0").expect("parent lock");
    let child = process::ChildLock::spawn(&cache_dir, "17.4.0", "exclusive").expect("spawn child");

    assert!(
        !child.acquired_within(CONFLICT_WINDOW),
        "an exclusive child must not acquire while the parent holds the lock"
    );

    drop(held);
    assert!(
        child.acquired_within(ACQUIRE_TIMEOUT),
        "the child must acquire once the parent releases"
    );
}

/// A shared parent lets a shared child acquire at the same time.
#[rstest]
fn a_shared_parent_lets_a_shared_child_acquire(cache_fixture: io::Result<(TempDir, Utf8PathBuf)>) {
    let (_temp, cache_dir) = cache_fixture.expect("cache fixture");
    let _held = CacheLock::acquire_shared(&cache_dir, "17.4.0").expect("parent lock");
    let child = process::ChildLock::spawn(&cache_dir, "17.4.0", "shared").expect("spawn child");

    assert!(
        child.acquired_within(ACQUIRE_TIMEOUT),
        "a shared child must acquire while the parent holds a shared lock"
    );
}

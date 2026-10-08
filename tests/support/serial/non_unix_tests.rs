//! Unit tests for non-Unix scenario lock ownership.

use rstest::rstest;

use super::{
    super::{ScenarioSerialGuard, serial_guard},
    *,
};

/// What one acquisition attempt does with each way creating the lock
/// directory can fail, driven through the real retry path (#279): creation
/// that fails with "already exists", or on Windows "access denied", is
/// contention and the attempt reports `None` so the caller retries; any
/// other failure is a real error and panics at once.
#[rstest]
#[case::already_exists(std::io::ErrorKind::AlreadyExists, true, false)]
#[case::access_denied(std::io::ErrorKind::PermissionDenied, cfg!(windows), !cfg!(windows))]
#[case::not_found(std::io::ErrorKind::NotFound, false, true)]
fn an_attempt_retries_on_contention_and_panics_on_a_real_failure(
    #[case] kind: std::io::ErrorKind,
    #[case] retries: bool,
    #[case] panics: bool,
) {
    let missing = std::env::temp_dir().join("pg_scenario_attempt_missing/lockdir");
    let deadline = Instant::now() + Duration::from_secs(30);
    let outcome = std::panic::catch_unwind(|| {
        try_acquire_with(&missing, deadline, |_| Err(std::io::Error::from(kind)))
    });
    match outcome {
        Ok(lock) => {
            assert!(!panics, "{kind:?} must not be retried");
            assert_eq!(lock.is_none(), retries, "{kind:?} reports contention");
        }
        Err(_) => assert!(panics, "{kind:?} must be retried, not panic"),
    }
}

/// Contention that outlasts the deadline stops retrying and names the last
/// error, so a lock path that is really unusable is still reported.
#[rstest]
fn contention_past_the_deadline_names_the_last_error() {
    let missing = std::env::temp_dir().join("pg_scenario_deadline_missing/lockdir");
    let deadline = Instant::now();
    std::thread::sleep(Duration::from_millis(5));
    let outcome = std::panic::catch_unwind(|| {
        try_acquire_with(&missing, deadline, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "sentinel lock error",
            ))
        })
    });
    let payload = outcome.expect_err("an expired deadline must panic");
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .unwrap_or_default();
    assert!(
        message.contains("timed out") && message.contains("(last error: sentinel lock error)"),
        "unexpected panic message: {message}"
    );
}

/// A real contender at the acquisition boundary waits while the lock is held and acquires it
/// once the holder releases it: the retry loop, the owner-state check and the release through
/// `Drop`, with no injected errors (#279).
#[rstest]
fn a_contender_waits_for_the_holder_and_then_acquires(serial_guard: ScenarioSerialGuard) {
    use std::{ffi::OsString, sync::mpsc, thread};

    use pg_embedded_setup_unpriv::test_support::scoped_env;

    let _guard = serial_guard;
    let tmp_dir = tempfile::tempdir().expect("failed to create the contender test target dir");
    let _env_guard = scoped_env(vec![(
        OsString::from("CARGO_TARGET_DIR"),
        Some(tmp_dir.path().to_path_buf().into_os_string()),
    )]);

    let holder = acquire_process_lock();
    let (acquired_tx, acquired_rx) = mpsc::channel();
    let contender = thread::spawn(move || {
        let lock = acquire_process_lock();
        acquired_tx.send(()).expect("the test is listening");
        drop(lock);
    });

    assert!(
        acquired_rx
            .recv_timeout(Duration::from_millis(500))
            .is_err(),
        "the contender must wait while the lock is held"
    );
    drop(holder);
    acquired_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the contender must acquire the lock once it is released");
    contender.join().expect("the contender thread finished");
}

/// A failed lock-directory creation counts as contention when the path
/// exists or, on Windows, is delete-pending and reports access denied;
/// anything else is a real failure that must not be retried silently.
#[rstest]
#[case::exists(std::io::ErrorKind::AlreadyExists, true)]
#[case::access_denied(std::io::ErrorKind::PermissionDenied, cfg!(windows))]
#[case::not_found(std::io::ErrorKind::NotFound, false)]
#[case::other(std::io::ErrorKind::Other, false)]
fn lock_contention_is_classified_by_error_kind(
    #[case] kind: std::io::ErrorKind,
    #[case] expected: bool,
) {
    assert_eq!(is_lock_contention(kind), expected);
}

#[rstest]
#[expect(
    clippy::let_underscore_must_use,
    reason = "best-effort cleanup where errors are intentionally ignored"
)]
fn acquire_process_lock_places_lockdir_in_cargo_target_dir(serial_guard: ScenarioSerialGuard) {
    use std::{env, ffi::OsString, fs};

    use pg_embedded_setup_unpriv::test_support::scoped_env;

    let _guard = serial_guard;

    let tmp_dir = env::temp_dir().join("pg_scenario_lockdir_test");
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&tmp_dir)
        .expect("failed to create temporary CARGO_TARGET_DIR for acquire_process_lock test");

    let _env_guard = scoped_env(vec![(
        OsString::from("CARGO_TARGET_DIR"),
        Some(tmp_dir.clone().into_os_string()),
    )]);
    {
        let _lock = acquire_process_lock();
        let lock_path = tmp_dir.join("pg-embed-setup-unpriv.serial.lockdir");
        assert!(
            lock_path.is_dir(),
            "expected acquire_process_lock to create lockdir at {lock_path:?}"
        );
        assert!(
            process_lock_owner_path(&lock_path).is_file(),
            "expected acquire_process_lock to record a lock owner in {lock_path:?}"
        );
    }

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[rstest]
#[expect(
    clippy::let_underscore_must_use,
    reason = "best-effort cleanup where errors are intentionally ignored"
)]
fn partial_process_lock_owner_respects_owner_grace(serial_guard: ScenarioSerialGuard) {
    use std::{env, fs};

    let _guard = serial_guard;

    let tmp_dir = env::temp_dir().join("pg_scenario_partial_lock_owner_test");
    let lock_path = tmp_dir.join("pg-embed-setup-unpriv.serial.lockdir");
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&lock_path)
        .expect("failed to create lock directory for malformed owner test");
    fs::write(process_lock_owner_path(&lock_path), "pid=")
        .expect("failed to write malformed process lock owner");

    assert_eq!(
        process_lock_state(&lock_path),
        ProcessLockState::PendingOwner(ProcessLockOwnerIssue::Malformed),
        "partial process lock owners inside the grace window must remain pending"
    );

    let _ = fs::remove_dir_all(&tmp_dir);
}

#[rstest]
#[case("")]
#[case("pid=")]
#[expect(
    clippy::let_underscore_must_use,
    reason = "best-effort cleanup where errors are intentionally ignored"
)]
fn malformed_process_lock_owner_becomes_stale_after_grace(
    serial_guard: ScenarioSerialGuard,
    #[case] owner: &str,
) {
    use std::{env, fs};

    let _guard = serial_guard;

    let tmp_dir = env::temp_dir().join("pg_scenario_stale_lock_owner_test");
    let lock_path = tmp_dir.join("pg-embed-setup-unpriv.serial.lockdir");
    let _ = fs::remove_dir_all(&tmp_dir);
    fs::create_dir_all(&lock_path).expect("failed to create lock directory for stale owner test");
    fs::write(process_lock_owner_path(&lock_path), owner)
        .expect("failed to write malformed process lock owner");
    let after_grace = SystemTime::now() + PROCESS_LOCK_OWNER_GRACE + Duration::from_secs(1);

    assert_eq!(
        process_lock_state_at(&lock_path, after_grace),
        ProcessLockState::Stale(ProcessLockOwnerIssue::Malformed)
    );

    let _ = fs::remove_dir_all(&tmp_dir);
}

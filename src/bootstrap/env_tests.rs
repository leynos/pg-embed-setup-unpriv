//! Tests for bootstrap environment discovery helpers.

#[cfg(all(unix, not(target_os = "macos")))]
use std::ffi::OsString;
#[cfg(all(unix, not(target_os = "macos")))]
use std::os::unix::ffi::OsStringExt;
#[cfg(all(unix, not(target_os = "macos")))]
use std::os::unix::fs::PermissionsExt;

#[cfg(all(unix, not(target_os = "macos")))]
use super::{BootstrapErrorKind, WORKER_BINARY_NAME};
use super::{
    DEFAULT_SHUTDOWN_TIMEOUT,
    discover_worker_from_path_value,
    find_timezone_dir,
    parse_shutdown_timeout,
    prepare_timezone_env,
    shutdown_timeout_from_env,
    validate_shutdown_timeout_secs,
    validate_worker_path,
    worker_binary_from_env,
};
use crate::{bootstrap::mode::ExecutionPrivileges, env::ScopedEnv};

#[cfg(all(unix, not(target_os = "macos")))]
#[expect(
    clippy::expect_used,
    reason = "test setup helper; a failure staging the fixture binary should fail the test"
)]
fn write_executable(path: &std::path::Path) {
    std::fs::write(path, b"#!/bin/sh\nexit 0\n").expect("write worker binary");
    let mut perms = std::fs::metadata(path)
        .expect("stat worker binary")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("chmod worker binary");
}

#[test]
fn shutdown_timeout_defaults_when_env_absent() {
    let _guard = ScopedEnv::apply(&[(String::from("PG_SHUTDOWN_TIMEOUT_SECS"), None)]);
    let timeout = shutdown_timeout_from_env().expect("absent env yields default");
    assert_eq!(timeout, DEFAULT_SHUTDOWN_TIMEOUT);
}

#[test]
fn shutdown_timeout_reads_valid_value() {
    let _guard = ScopedEnv::apply(&[(
        String::from("PG_SHUTDOWN_TIMEOUT_SECS"),
        Some(String::from(" 42 ")),
    )]);
    let timeout = shutdown_timeout_from_env().expect("valid env parses");
    assert_eq!(timeout, std::time::Duration::from_secs(42));
}

#[test]
fn parse_shutdown_timeout_rejects_empty_and_non_numeric() {
    assert!(parse_shutdown_timeout("").is_err(), "empty must fail");
    assert!(
        parse_shutdown_timeout("abc").is_err(),
        "non-numeric must fail"
    );
}

#[test]
fn validate_shutdown_timeout_secs_enforces_bounds() {
    assert!(
        validate_shutdown_timeout_secs(0, "0").is_err(),
        "zero rejected"
    );
    assert!(
        validate_shutdown_timeout_secs(601, "601").is_err(),
        "over-limit rejected"
    );
    assert_eq!(
        validate_shutdown_timeout_secs(30, "30").expect("in-range accepted"),
        std::time::Duration::from_secs(30),
    );
}

#[test]
fn validate_worker_path_rejects_empty_and_root() {
    assert!(
        validate_worker_path(&camino::Utf8PathBuf::from("")).is_err(),
        "empty path rejected"
    );
    assert!(
        validate_worker_path(&camino::Utf8PathBuf::from("/")).is_err(),
        "filesystem root rejected"
    );
}

#[test]
fn find_timezone_dir_returns_existing_candidate() {
    // The probe must return an existing directory when one is present, or
    // `None` when the platform lacks a time zone database.
    assert!(
        find_timezone_dir().is_none_or(camino::Utf8Path::exists),
        "reported timezone dir must exist when present"
    );
}

#[test]
fn prepare_timezone_env_defaults_zone_to_utc() {
    let _guard = ScopedEnv::apply(&[(String::from("TZ"), None), (String::from("TZDIR"), None)]);
    // Only assert when a timezone database is discoverable in the environment.
    if find_timezone_dir().is_some() {
        let tz = prepare_timezone_env().expect("timezone env prepared");
        assert_eq!(tz.zone, "UTC");
        assert!(tz.dir.is_some(), "discovered TZDIR should be populated");
    }
}

#[test]
fn prepare_timezone_env_honours_explicit_zone_and_dir() {
    let temp = tempfile::tempdir().expect("tempdir");
    let tzdir = temp.path().to_str().expect("utf8 tempdir");
    let _guard = ScopedEnv::apply(&[
        (String::from("TZ"), Some(String::from("Europe/London"))),
        (String::from("TZDIR"), Some(tzdir.to_owned())),
    ]);
    let tz = prepare_timezone_env().expect("timezone env prepared");
    assert_eq!(tz.zone, "Europe/London");
    assert_eq!(tz.dir.as_deref().map(camino::Utf8Path::as_str), Some(tzdir),);
}

#[test]
fn prepare_timezone_env_rejects_missing_tzdir() {
    let _guard = ScopedEnv::apply(&[(
        String::from("TZDIR"),
        Some(String::from("/definitely/missing/zoneinfo")),
    )]);
    assert!(
        prepare_timezone_env().is_err(),
        "missing TZDIR should be rejected"
    );
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn worker_binary_from_env_accepts_executable_override() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worker = temp.path().join("pg_worker");
    write_executable(&worker);
    let _guard = ScopedEnv::apply(&[(
        String::from("PG_EMBEDDED_WORKER"),
        Some(worker.to_str().expect("utf8 path").to_owned()),
    )]);

    let resolved = worker_binary_from_env(ExecutionPrivileges::Root)
        .expect("executable override should validate")
        .expect("override should resolve to a path");
    assert_eq!(resolved.as_std_path(), worker);
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn worker_binary_from_env_reports_missing_binary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing = temp.path().join("pg_worker");
    let _guard = ScopedEnv::apply(&[(
        String::from("PG_EMBEDDED_WORKER"),
        Some(missing.to_str().expect("utf8 path").to_owned()),
    )]);

    let err = worker_binary_from_env(ExecutionPrivileges::Root)
        .expect_err("missing worker binary should error");
    assert_eq!(err.kind(), BootstrapErrorKind::WorkerBinaryMissing);
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn worker_binary_from_env_rejects_non_executable_file() {
    let temp = tempfile::tempdir().expect("tempdir");
    let worker = temp.path().join("pg_worker");
    std::fs::write(&worker, b"not executable").expect("write file");
    let mut perms = std::fs::metadata(&worker).expect("stat").permissions();
    perms.set_mode(0o644);
    std::fs::set_permissions(&worker, perms).expect("chmod");
    let _guard = ScopedEnv::apply(&[(
        String::from("PG_EMBEDDED_WORKER"),
        Some(worker.to_str().expect("utf8 path").to_owned()),
    )]);

    let err = worker_binary_from_env(ExecutionPrivileges::Root)
        .expect_err("non-executable worker binary should error");
    assert!(
        err.to_string().contains("executable"),
        "expected executability error, got: {err}"
    );
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn worker_binary_from_env_discovers_worker_on_path() {
    let temp = tempfile::tempdir().expect("tempdir");
    let bin_dir = temp.path().join("bin");
    std::fs::create_dir_all(&bin_dir).expect("create bin dir");
    write_executable(&bin_dir.join(WORKER_BINARY_NAME));

    let path_value = std::env::join_paths([bin_dir.clone()])
        .expect("join PATH")
        .to_str()
        .expect("utf8 PATH")
        .to_owned();
    let _guard = ScopedEnv::apply(&[
        (String::from("PG_EMBEDDED_WORKER"), None),
        (String::from("PATH"), Some(path_value)),
    ]);

    let resolved = worker_binary_from_env(ExecutionPrivileges::Root)
        .expect("discovery should validate")
        .expect("discovery should resolve to a path");
    assert_eq!(resolved.as_std_path(), bin_dir.join(WORKER_BINARY_NAME));
}

#[test]
fn discover_worker_returns_none_when_path_is_absent() {
    let result = discover_worker_from_path_value(None).expect("None PATH should not error");
    assert!(result.is_none(), "expected Ok(None) when PATH is absent");
}

#[test]
fn worker_binary_from_env_ignores_env_for_unprivileged_bootstrap() {
    let _guard = ScopedEnv::apply(&[(
        String::from("PG_EMBEDDED_WORKER"),
        Some(String::from("/definitely/missing/pg_worker")),
    )]);

    let result = worker_binary_from_env(ExecutionPrivileges::Unprivileged)
        .expect("unprivileged bootstrap should not validate PG_EMBEDDED_WORKER");

    assert!(
        result.is_none(),
        "unprivileged bootstrap should not use PG_EMBEDDED_WORKER"
    );
}

#[test]
#[cfg(all(unix, not(target_os = "macos")))]
fn discover_worker_errors_on_non_utf8_path_entry() {
    let temp = tempfile::tempdir().expect("tempdir");
    let valid_dir = temp.path().join("valid");
    std::fs::create_dir_all(&valid_dir).expect("create valid dir");

    let non_utf8_component = OsString::from_vec(vec![0xff, 0xfe, 0xfd]);
    let non_utf8_dir = temp.path().join(&non_utf8_component);
    std::fs::create_dir_all(&non_utf8_dir).expect("create non-utf8 dir");

    let worker_path = valid_dir.join(WORKER_BINARY_NAME);
    std::fs::write(&worker_path, b"#!/bin/sh\nexit 0\n").expect("create pg_worker");
    let mut perms = std::fs::metadata(&worker_path)
        .expect("stat pg_worker")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&worker_path, perms).expect("chmod pg_worker");

    let path_value = std::env::join_paths([non_utf8_dir, valid_dir]).expect("join PATH");
    let err = discover_worker_from_path_value(Some(path_value))
        .expect_err("discover_worker_from_path should reject non-UTF-8 PATH entries");
    let message = err.to_string().to_lowercase();
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::WorkerBinaryPathNonUtf8,
        "expected PATH UTF-8 error kind"
    );
    assert!(
        message.contains("path") && message.contains("non-utf-8"),
        "expected error mentioning PATH non-UTF-8, got: {message}"
    );
}

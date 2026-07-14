//! Unit tests for filesystem helpers.

use std::{fs::File, io::ErrorKind};

use camino::{Utf8Path, Utf8PathBuf};
use rstest::rstest;
use tempfile::tempdir;

#[cfg(unix)]
use super::ensure_dir_exists;
use super::{ensure_existing_path_is_dir, find_existing_ancestor, set_permissions};

#[cfg(unix)]
#[test]
fn set_permissions_applies_mode_to_existing_directory() {
    use std::os::unix::fs::PermissionsExt as _;

    let temp = tempdir().expect("tempdir");
    let base = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir path");
    let dir = base.join("perm");
    ensure_dir_exists(&dir).expect("create dir");

    set_permissions(&dir, 0o750).expect("apply permissions");

    let mode = std::fs::metadata(dir.as_std_path())
        .expect("stat dir")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o750);
}

#[cfg(unix)]
#[test]
fn set_permissions_is_noop_for_ambient_root() {
    set_permissions(Utf8Path::new("/"), 0o755).expect("root permission update should be a no-op");
}

#[cfg(unix)]
#[test]
fn set_permissions_errors_for_missing_target() {
    let temp = tempdir().expect("tempdir");
    let base = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir path");
    let missing = base.join("missing");

    let err = set_permissions(&missing, 0o700).expect_err("missing target should error");
    let has_not_found = err
        .chain()
        .filter_map(|source| source.downcast_ref::<std::io::Error>())
        .any(|source| source.kind() == ErrorKind::NotFound);
    assert!(has_not_found, "expected NotFound in chain, got {err:?}");
}

/// Test-case container: `create_file` selects file vs directory, and
/// `error_kind` records the expected `ErrorKind` outcome.
struct ExistingPathCase {
    create_file: bool,
    error_kind: Option<ErrorKind>,
}

#[rstest]
#[case::directory(ExistingPathCase {
    create_file: false,
    error_kind: None,
})]
#[case::file(ExistingPathCase {
    create_file: true,
    error_kind: Some(ErrorKind::NotADirectory),
})]
fn ensure_existing_path_is_dir_handles_existing_paths(#[case] case: ExistingPathCase) {
    let temp = tempdir().expect("tempdir");
    let path = if case.create_file {
        let file_path = temp.path().join("file");
        File::create(&file_path).expect("create file");
        Utf8PathBuf::from_path_buf(file_path).expect("utf8 file path")
    } else {
        Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir path")
    };

    let result = ensure_existing_path_is_dir(&path);

    match case.error_kind {
        Some(expected_kind) => {
            let err = result.expect_err("existing file should be rejected as a directory path");
            let has_kind = err
                .chain()
                .filter_map(|source| source.downcast_ref::<std::io::Error>())
                .any(|source| source.kind() == expected_kind);
            assert!(
                has_kind,
                "expected error kind {expected_kind:?} in chain, got {err:?}"
            );
        }
        None => {
            result.expect("existing directory should be accepted");
        }
    }
}

#[cfg(unix)]
#[test]
fn ensure_existing_path_is_dir_allows_ambient_root() {
    ensure_existing_path_is_dir(Utf8Path::new("/"))
        .expect("ambient root should be treated as a directory");
}

#[test]
fn find_existing_ancestor_handles_relative_paths() {
    // For relative paths, find_existing_ancestor should open "." and return
    // the path as the relative component.
    let path = Utf8Path::new("some/relative/path");
    let (dir, relative) = find_existing_ancestor(path).expect("find ancestor for relative path");
    // The relative path should be the original path.
    assert_eq!(relative.as_str(), "some/relative/path");
    // The directory should be openable (it's the current directory).
    assert!(
        dir.exists("."),
        "directory handle should reference an existing directory"
    );
}

#[cfg(unix)]
#[test]
fn find_existing_ancestor_walks_up_to_existing_directory() {
    // Create a temp directory to serve as the existing ancestor.
    let temp = tempdir().expect("tempdir");
    let base = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir path");

    // Construct a path with non-existent intermediate directories.
    let nested = base.join("does/not/exist/nested/dir");

    let (dir, relative) = find_existing_ancestor(&nested).expect("find ancestor");

    // The directory should be opened at the temp directory (the existing ancestor).
    // The relative path should be "does/not/exist/nested/dir".
    assert_eq!(relative.as_str(), "does/not/exist/nested/dir");
    assert!(
        dir.exists("."),
        "directory handle should reference an existing directory"
    );
}

#[cfg(unix)]
#[test]
fn find_existing_ancestor_handles_root_path() {
    let path = Utf8Path::new("/");
    let (dir, relative) = find_existing_ancestor(path).expect("find ancestor for root");
    // For root, the relative path should be empty.
    assert!(
        relative.as_str().is_empty(),
        "expected empty relative path for root, got: {relative}"
    );
    assert!(dir.exists("."), "root directory should exist");
}

#[cfg(unix)]
#[test]
fn ensure_dir_exists_creates_nested_directories() {
    let temp = tempdir().expect("tempdir");
    let base = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir path");

    let nested = base.join("level1/level2/level3");
    assert!(!nested.exists(), "nested path should not exist before test");

    ensure_dir_exists(&nested).expect("ensure_dir_exists should create nested directories");

    assert!(nested.exists(), "nested path should exist after creation");
    assert!(nested.is_dir(), "nested path should be a directory");
}

#[cfg(unix)]
#[test]
fn ensure_dir_exists_is_idempotent() {
    let temp = tempdir().expect("tempdir");
    let base = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir path");

    let target = base.join("idempotent_dir");

    // Create once.
    ensure_dir_exists(&target).expect("first creation");
    assert!(target.exists(), "directory should exist after first call");

    // Create again - should succeed without error.
    ensure_dir_exists(&target).expect("second creation should be idempotent");
    assert!(
        target.exists(),
        "directory should still exist after second call"
    );
}

#[cfg(not(unix))]
#[test]
fn set_permissions_checks_target_exists_before_skipping_posix_permissions() {
    let temp = tempdir().expect("tempdir");
    let base = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 tempdir path");
    let existing = base.join("existing");
    std::fs::create_dir(&existing).expect("create existing target");

    set_permissions(&existing, 0o700).expect("existing target should skip permissions");

    let missing = base.join("missing");
    let err =
        set_permissions(&missing, 0o700).expect_err("missing target should still return NotFound");
    let has_not_found = err
        .chain()
        .filter_map(|source| source.downcast_ref::<std::io::Error>())
        .any(|source| source.kind() == ErrorKind::NotFound);
    assert!(
        has_not_found,
        "expected NotFound in error chain for missing target, got {err:?}"
    );
}

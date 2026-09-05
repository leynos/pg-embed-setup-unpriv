//! Unit tests for privilege-management helpers.
//!
//! These exercise the ownership and permission helpers by chowning to the
//! *current* user, which is permitted without elevated privileges, so the
//! logic is covered without requiring root.

use std::{ffi::CString, os::unix::fs::PermissionsExt as _, path::PathBuf};

use nix::unistd::{User, getegid, geteuid};
use rstest::rstest;

use super::*;
use crate::test_support::capture_info_logs;

/// Builds a synthetic `User` owned by the current process, avoiding any
/// passwd/NSS lookup (which can be absent in minimal containers). Only the
/// effective UID/GID matter for the chown-to-self ownership tests; the
/// remaining fields carry deterministic placeholders.
fn current_user() -> User {
    User {
        name: String::from("pg-embed-test"),
        passwd: CString::default(),
        uid: geteuid(),
        gid: getegid(),
        #[cfg(not(all(target_os = "android", target_pointer_width = "32")))]
        gecos: CString::default(),
        dir: PathBuf::from("/nonexistent"),
        shell: PathBuf::from("/nonexistent"),
        #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "dragonfly"))]
        class: CString::default(),
        #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "dragonfly"))]
        change: 0,
        #[cfg(any(target_os = "freebsd", target_os = "openbsd", target_os = "dragonfly"))]
        expire: 0,
    }
}

fn utf8(path: &std::path::Path) -> Option<&Utf8Path> { Utf8Path::from_path(path) }

fn mode_of(path: &Utf8Path) -> std::io::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

// Named wrapper functions monomorphise the generic helpers to a bare
// function pointer whose borrow lifetimes stay late-bound (higher-ranked),
// so they coerce to the case parameter type. A turbofish such as
// `make_dir_accessible::<&Utf8Path>`, or an inline closure, would instead
// fix the `dir` lifetime and fail to coerce.
fn apply_world_readable(dir: &Utf8Path, user: &User) -> PrivilegeResult<()> {
    make_dir_accessible(dir, user)
}

fn apply_owner_only(dir: &Utf8Path, user: &User) -> PrivilegeResult<()> {
    make_data_dir_private(dir, user)
}

#[rstest]
#[case::world_readable(apply_world_readable, "install", 0o755)]
#[case::owner_only(apply_owner_only, "data", 0o700)]
fn directory_permission_helper_sets_expected_mode(
    #[case] helper: fn(&Utf8Path, &User) -> PrivilegeResult<()>,
    #[case] subdir: &str,
    #[case] expected_mode: u32,
) {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = utf8(temp.path()).expect("temp path is UTF-8").join(subdir);
    let user = current_user();
    helper(&dir, &user).expect("apply directory permissions");
    assert!(dir.is_dir(), "directory should exist");
    assert_eq!(
        mode_of(&dir).expect("stat directory"),
        expected_mode,
        "unexpected directory mode"
    );
}

#[test]
fn ensure_dir_for_user_is_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = utf8(temp.path())
        .expect("temp path is UTF-8")
        .join("nested/child");
    let user = current_user();
    ensure_dir_for_user(&dir, &user, 0o750).expect("first call");
    ensure_dir_for_user(&dir, &user, 0o750).expect("second call");
    assert_eq!(mode_of(&dir).expect("stat directory"), 0o750);
}

#[test]
fn ensure_tree_owned_by_user_walks_nested_entries() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = utf8(temp.path()).expect("temp path is UTF-8");
    let nested = root.join("a/b");
    std::fs::create_dir_all(nested.as_std_path()).expect("create nested dirs");
    std::fs::write(nested.join("file.txt").as_std_path(), b"data").expect("write file");
    std::fs::write(root.join("top.txt").as_std_path(), b"data").expect("write top file");

    // Ownership is reassigned to the current user, so it does not visibly
    // change on disk. Observe the traversal instead via the summary log,
    // which reports the number of chowned entries: the four descendants
    // `a`, `a/b`, `a/b/file.txt`, and `top.txt`. This fails if traversal is
    // skipped or nested entries are missed.
    let user = current_user();
    let (logs, result) = capture_info_logs(|| ensure_tree_owned_by_user(root, &user));
    result.expect("chown tree to self");
    assert!(
        logs.iter().any(|line| {
            line.contains("ensured tree ownership for user") && line.contains("updated_entries=4")
        }),
        "expected all four nested entries to be chowned, got logs: {logs:?}"
    );
}

#[test]
fn ensure_tree_owned_by_user_ignores_missing_root() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing = utf8(temp.path())
        .expect("temp path is UTF-8")
        .join("does-not-exist");
    ensure_tree_owned_by_user(&missing, &current_user())
        .expect("missing root should be treated as an empty tree");
}

#[test]
fn nobody_uid_is_non_root() {
    assert!(nobody_uid().as_raw() > 0, "nobody must not map to root");
}

#[test]
fn default_paths_for_derive_install_and_data_dirs() {
    let (install, data) = default_paths_for(Uid::from_raw(4321));
    assert_eq!(install.as_str(), "/var/tmp/pg-embed-4321/install");
    assert_eq!(data.as_str(), "/var/tmp/pg-embed-4321/data");
}

/// The per-user default root is unchanged by the `PG_EMBED_ROOT` override.
#[test]
fn default_root_for_is_the_var_tmp_per_uid_tree() {
    assert_eq!(
        default_root_for(Uid::from_raw(4321)).as_str(),
        "/var/tmp/pg-embed-4321"
    );
}

/// An explicit root yields `install` and `data` leaves directly beneath it.
#[test]
fn default_paths_under_derive_leaves_from_root() {
    let (install, data) = default_paths_under(camino::Utf8Path::new("/srv/project/pg"));
    assert_eq!(install.as_str(), "/srv/project/pg/install");
    assert_eq!(data.as_str(), "/srv/project/pg/data");
}

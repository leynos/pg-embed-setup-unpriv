//! Tests for cluster cleanup behaviour.
use std::{
    fs,
    path::{Path, PathBuf},
};

use postgresql_embedded::Settings;
use rstest::rstest;
use tempfile::tempdir;

use super::{cleanup_in_process, is_dangerous_cleanup_path, should_remove_install_root};
use crate::CleanupMode;

#[rstest]
#[case::data_only(CleanupMode::DataOnly, false, true)]
#[case::full(CleanupMode::Full, false, false)]
#[case::none(CleanupMode::None, true, true)]
fn cleanup_in_process_respects_mode(
    #[case] mode: CleanupMode,
    #[case] expect_data_exists: bool,
    #[case] expect_install_exists: bool,
) {
    let sandbox = tempdir().expect("tempdir");
    let data_dir = sandbox.path().join("data");
    let install_dir = sandbox.path().join("install");
    fs::create_dir_all(&data_dir).expect("create data dir");
    fs::create_dir_all(&install_dir).expect("create install dir");
    fs::write(data_dir.join("marker"), b"data").expect("write data marker");
    fs::write(install_dir.join("marker"), b"install").expect("write install marker");

    let settings = Settings {
        data_dir,
        installation_dir: install_dir,
        ..Settings::default()
    };

    cleanup_in_process(mode, &settings, "cleanup-test");
    cleanup_in_process(mode, &settings, "cleanup-test");

    assert_eq!(
        settings.data_dir.exists(),
        expect_data_exists,
        "data directory presence should match cleanup mode",
    );
    assert_eq!(
        settings.installation_dir.exists(),
        expect_install_exists,
        "installation directory presence should match cleanup mode",
    );
}

// A full cleanup of one cluster must not pull the install tree from under
// another cluster that is still running from it (ADR 005).
#[rstest]
#[case::peer_running(true, true)]
#[case::alone(false, false)]
fn full_cleanup_keeps_an_install_tree_that_a_peer_runs_from(
    #[case] has_peer: bool,
    #[case] expect_install_exists: bool,
) {
    let sandbox = tempdir().expect("tempdir");
    let slots = sandbox.path().join("data");
    let own = slots.join("1-1-0");
    let install_dir = sandbox.path().join("install");
    fs::create_dir_all(&own).expect("own slot");
    fs::create_dir_all(&install_dir).expect("install dir");
    fs::write(slots.join("1-1-0.lock"), b"").expect("own lock file");
    let peer_lock = fs::File::create(slots.join("2-2-0.lock")).expect("peer lock file");
    if has_peer {
        fs4::FileExt::lock(&peer_lock).expect("hold the peer's lock");
    }
    let settings = Settings {
        data_dir: own,
        installation_dir: install_dir,
        ..Settings::default()
    };

    cleanup_in_process(CleanupMode::Full, &settings, "peer-test");

    assert!(!settings.data_dir.exists(), "the cluster's own data goes");
    assert_eq!(settings.installation_dir.exists(), expect_install_exists);
}

// A full cleanup that cannot take the root's claim guard cannot rule out a
// cluster claiming a slot mid-cleanup, so it leaves the install tree.
#[test]
fn full_cleanup_keeps_the_install_tree_when_the_guard_is_unavailable() {
    let sandbox = tempdir().expect("tempdir");
    let slots = sandbox.path().join("data");
    let own = slots.join("1-1-0");
    let install_dir = sandbox.path().join("install");
    fs::create_dir_all(&own).expect("own slot");
    fs::create_dir_all(&install_dir).expect("install dir");
    fs::write(slots.join("1-1-0.lock"), b"").expect("own lock file");
    // A directory where the guard file belongs makes opening it fail.
    fs::create_dir_all(slots.join(".claim-guard")).expect("obstruct the guard");
    let settings = Settings {
        data_dir: own,
        installation_dir: install_dir,
        ..Settings::default()
    };

    cleanup_in_process(CleanupMode::Full, &settings, "guard-test");

    assert!(!settings.data_dir.exists(), "the cluster's own data goes");
    assert!(settings.installation_dir.exists(), "the install tree stays");
}

// A data directory whose slot lock cannot be looked up, because its parent
// cannot be searched, may be a slot with peers, so a full cleanup keeps the
// install tree. Root searches any directory, so the case is skipped there.
#[cfg(unix)]
#[test]
fn full_cleanup_keeps_the_install_tree_when_the_slot_lookup_is_denied() {
    use std::os::unix::fs::PermissionsExt;

    if nix::unistd::geteuid().is_root() {
        return;
    }
    let sandbox = tempdir().expect("tempdir");
    let slots = sandbox.path().join("data");
    let own = slots.join("1-1-0");
    let install_dir = sandbox.path().join("install");
    fs::create_dir_all(&own).expect("own slot");
    fs::create_dir_all(&install_dir).expect("install dir");
    fs::write(slots.join("1-1-0.lock"), b"").expect("own lock file");
    fs::set_permissions(&slots, fs::Permissions::from_mode(0o600)).expect("deny search");
    let settings = Settings {
        data_dir: own,
        installation_dir: install_dir,
        ..Settings::default()
    };

    cleanup_in_process(CleanupMode::Full, &settings, "denied-test");

    fs::set_permissions(&slots, fs::Permissions::from_mode(0o700)).expect("restore search");
    assert!(settings.installation_dir.exists(), "the install tree stays");
}

// The installation root is only ever removed when it lives under the
// installation directory, so removing the installation directory always
// cascades to it. That makes filesystem state an unreliable oracle for the
// dedicated installation-root branch, so assert the decision directly; this
// fails if `should_remove_install_root` stops guarding the branch.
#[rstest]
#[case::nested_under_install("/opt/pg/install", "/opt/pg/install/secrets", true)]
#[case::equal_to_install("/opt/pg/install", "/opt/pg/install", false)]
#[case::outside_install("/opt/pg/install", "/elsewhere/secrets", false)]
#[case::parent_dir_traversal("/opt/pg/install", "/opt/pg/install/../evil", false)]
fn should_remove_install_root_classifies_parent(
    #[case] install: &str,
    #[case] parent: &str,
    #[case] expected: bool,
) {
    let settings = Settings {
        installation_dir: PathBuf::from(install),
        ..Settings::default()
    };
    assert_eq!(
        should_remove_install_root(Path::new(parent), &settings),
        expected,
        "unexpected installation-root removal decision",
    );
}

// Validate the dangerous-path guard directly rather than driving
// `cleanup_in_process` against the real filesystem root. The root is
// resolved at runtime because a literal "/" is not an absolute path on
// Windows; the test fails if the guard stops flagging the root or an empty
// path.
#[test]
fn is_dangerous_cleanup_path_flags_root_and_empty() {
    assert!(
        is_dangerous_cleanup_path(Path::new("")),
        "an empty path must be flagged as dangerous"
    );

    let root = std::env::current_dir()
        .expect("resolve current dir")
        .ancestors()
        .last()
        .expect("an absolute directory has a root ancestor")
        .to_path_buf();
    assert!(
        is_dangerous_cleanup_path(&root),
        "filesystem root {root:?} must be flagged as dangerous"
    );

    assert!(
        !is_dangerous_cleanup_path(Path::new("data/pg-embed")),
        "an ordinary relative path must not be flagged"
    );
}

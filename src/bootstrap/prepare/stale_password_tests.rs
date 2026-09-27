//! Tests for discarding a password file left by a reaped cluster.

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, eyre};

use super::discard_orphaned_password_file;

/// A data directory and the password-file path beside it.
struct Scratch {
    _temp: tempfile::TempDir,
    data_dir: Utf8PathBuf,
    password_file: Utf8PathBuf,
}

/// Builds an empty data directory with no password file.
fn scratch() -> Result<Scratch> {
    let temp = tempfile::tempdir()?;
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir)?;
    Ok(Scratch {
        _temp: temp,
        data_dir,
        password_file: root.join(".pgpass"),
    })
}

/// A reaped cluster's password file is removed, so `initdb` gets a fresh one.
#[test]
fn a_password_file_without_a_cluster_is_removed() {
    let dir = scratch().expect("scratch directory");
    std::fs::write(&dir.password_file, "previous-cluster-password").expect("seed the stale file");

    let removed = discard_orphaned_password_file(&dir.data_dir, &dir.password_file)
        .expect("a stale file is removable");

    assert!(removed, "the stale file should be reported as removed");
    assert!(!dir.password_file.exists(), "the stale file must be gone");
}

/// A live cluster's password file is kept, because reuse reads it.
#[test]
fn a_password_file_beside_a_cluster_is_kept() {
    let dir = scratch().expect("scratch directory");
    std::fs::write(dir.data_dir.join("PG_VERSION"), "17\n").expect("mark a cluster");
    std::fs::write(&dir.password_file, "live-cluster-password").expect("seed the live file");

    let removed = discard_orphaned_password_file(&dir.data_dir, &dir.password_file)
        .expect("probing a cluster succeeds");

    assert!(!removed);
    assert_eq!(
        std::fs::read_to_string(&dir.password_file).expect("the file is still there"),
        "live-cluster-password"
    );
}

/// Nothing to remove is not an error.
#[test]
fn no_password_file_is_not_an_error() {
    let dir = scratch().expect("scratch directory");
    let removed = discard_orphaned_password_file(&dir.data_dir, &dir.password_file)
        .expect("a missing file is fine");
    assert!(!removed);
}

/// A stale path that cannot be removed fails the bootstrap with the remedy,
/// rather than letting `initdb` read it.
#[test]
fn an_unremovable_stale_path_is_an_error() {
    let dir = scratch().expect("scratch directory");
    // A directory in the file's place cannot be removed with `remove_file`.
    std::fs::create_dir_all(&dir.password_file).expect("put a directory in the way");

    let err = discard_orphaned_password_file(&dir.data_dir, &dir.password_file)
        .expect_err("a directory cannot be removed as a file");

    assert!(format!("{err:?}").contains("cannot be removed"), "{err:?}");
}

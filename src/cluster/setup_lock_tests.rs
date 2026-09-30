//! Tests for when a start keeps the install tree's setup lock (#289).

use camino::Utf8PathBuf;
use color_eyre::eyre::{Result, ensure, eyre};
use fs4::FileExt;
use rstest::rstest;

use super::{InstallLock, SETUP_LOCK_FILE, SetupLock};

/// A temporary install root.
fn install_root() -> Result<(tempfile::TempDir, Utf8PathBuf)> {
    let temp = tempfile::tempdir()?;
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf())
        .map_err(|path| eyre!("non-UTF-8 tempdir {}", path.display()))?;
    Ok((temp, root))
}

/// Returns whether another handle can take the lock in `root` at once.
fn is_lock_free(root: &Utf8PathBuf) -> Result<bool> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join(SETUP_LOCK_FILE))?;
    Ok(FileExt::try_lock(&file).is_ok())
}

/// A cache hit leaves a complete tree, so the start lets the lock go before
/// `initdb`; a miss lets `Setup` populate the tree, so it holds the lock.
#[rstest]
#[case::hit(true, false)]
#[case::miss(false, true)]
fn the_lock_is_kept_only_while_setup_populates_the_tree(
    #[case] cache_hit: bool,
    #[case] expect_held: bool,
) -> Result<()> {
    let (_temp, root) = install_root()?;
    let setup = SetupLock::acquire_at(&root)?;

    let (held, hook) = InstallLock::after_cache_lookup(setup, cache_hit, &root);

    ensure!(held.is_some() == expect_held, "lock kept after the lookup");
    ensure!(
        is_lock_free(&root)? != expect_held,
        "lock visible to others"
    );
    ensure!(
        matches!(hook, InstallLock::Held) == expect_held,
        "hook mode"
    );
    Ok(())
}

/// The hook writes into the tree, so after a released lock it takes the lock
/// again; when the start still holds it, it takes nothing.
#[test]
fn the_hook_retakes_a_released_lock_and_never_a_held_one() -> Result<()> {
    let (_temp, root) = install_root()?;

    let retaken = InstallLock::Released { root: &root }.for_hook()?;
    ensure!(retaken.is_some(), "a released lock is retaken");
    ensure!(!is_lock_free(&root)?, "the hook holds it while it writes");
    drop(retaken);
    ensure!(is_lock_free(&root)?, "and lets it go afterwards");

    ensure!(
        InstallLock::Held.for_hook()?.is_none(),
        "a held lock is not retaken"
    );
    Ok(())
}

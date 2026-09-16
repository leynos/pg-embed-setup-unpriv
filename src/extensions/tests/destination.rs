//! What the destination inspection reports, case by case.
//!
//! Every outcome but [`Destination::Identical`] leads the writer to the same
//! place, a fresh file and a rename, so a query returning `Option<File>`
//! produced the right behaviour for four different reasons. The reasons are
//! not alike: an absent or differing destination is the ordinary course of an
//! install, while one that cannot be read, or that holds something other than
//! a regular file, is a fault an operator should be able to see named.
//!
//! These cases hold the query to naming each one. They are written against
//! the query rather than through `install`, because a test driving the whole
//! installer cannot tell the four apart either: all four produce an installed
//! file, which is precisely the problem.

use camino::{Utf8Path, Utf8PathBuf};
use rstest::rstest;
use tempfile::TempDir;

use crate::extensions::tree::{Destination, InstallTree};

const BYTES: &[u8] = b"the planned contents";

/// A tree rooted at a fresh temporary directory, and the root's path.
fn tree() -> (TempDir, InstallTree, Utf8PathBuf) {
    let root = TempDir::new().expect("create a temporary installation root");
    let path = Utf8PathBuf::from_path_buf(root.path().to_path_buf())
        .expect("the temporary root must be UTF-8");
    let tree = InstallTree::open(&path).expect("open the installation tree");
    (root, tree, path)
}

#[rstest]
fn an_absent_destination_is_named_absent() {
    let (_root, tree, _path) = tree();

    let found = tree.inspect_destination(Utf8Path::new("lib/ext.so"), BYTES);

    assert!(
        matches!(found, Destination::Absent),
        "nothing is there, and that is the ordinary first install"
    );
    assert_eq!(
        found.rewrite_reason().as_deref(),
        Some("nothing is there"),
        "the reason reaches the log"
    );
}

#[rstest]
fn a_matching_file_is_named_identical() {
    let (_root, tree, path) = tree();
    std::fs::write(path.join("ext.so"), BYTES).expect("plant the identical file");

    let found = tree.inspect_destination(Utf8Path::new("ext.so"), BYTES);

    assert!(
        matches!(found, Destination::Identical(_)),
        "a regular file holding exactly these bytes is reusable"
    );
    assert!(
        found.rewrite_reason().is_none(),
        "nothing is rewritten, so there is no reason to report"
    );
}

#[rstest]
fn a_file_holding_other_bytes_is_named_different() {
    let (_root, tree, path) = tree();
    std::fs::write(path.join("ext.so"), b"an older build").expect("plant the stale file");

    let found = tree.inspect_destination(Utf8Path::new("ext.so"), BYTES);

    assert!(
        matches!(found, Destination::Different),
        "a regular file holding other bytes is replaced, and says so"
    );
}

/// A directory where a file belongs is refused, and the two platforms
/// refuse it at different steps.
///
/// On Unix the open succeeds and the stat reports a directory, so the
/// outcome is `NotRegular`. On Windows a directory cannot be opened for
/// reading without backup semantics, which this deliberately does not ask
/// for, so the refusal arrives from the open itself as `Unreadable`. Both
/// are correct and both lead to the same rewrite; the case asserts the one
/// its platform actually produces rather than the weaker "either of these",
/// which would survive a mutation swapping the two.
#[cfg(unix)]
#[rstest]
fn a_directory_at_the_destination_is_named_not_regular() {
    let (_root, tree, path) = tree();
    std::fs::create_dir(path.join("ext.so")).expect("plant a directory");

    let found = tree.inspect_destination(Utf8Path::new("ext.so"), BYTES);

    assert!(
        matches!(found, Destination::NotRegular),
        "something other than a regular file is not a differing file"
    );
}

/// The Windows half of the case above.
#[cfg(windows)]
#[rstest]
fn a_directory_at_the_destination_is_refused_by_the_open() {
    let (_root, tree, path) = tree();
    std::fs::create_dir(path.join("ext.so")).expect("plant a directory");

    let found = tree.inspect_destination(Utf8Path::new("ext.so"), BYTES);

    assert!(
        matches!(found, Destination::Unreadable(_)),
        "a directory cannot be opened for reading here, so the open refuses it"
    );
    assert!(
        found
            .rewrite_reason()
            .is_some_and(|reason| reason.starts_with("what is there cannot be read")),
        "the operator is told the destination could not be read"
    );
}

/// A symlink at the destination is refused by the open, not followed.
///
/// `O_NOFOLLOW` makes this a failed open rather than a successful one onto
/// the target, so it arrives as unreadable. That is the outcome that matters:
/// the digest, the `chmod` and the `chown` never reach the link's target, and
/// the rename that follows replaces the link itself.
#[cfg(unix)]
#[rstest]
fn a_symlinked_destination_is_named_unreadable() {
    let (_root, tree, path) = tree();
    let outside = path.join("elsewhere");
    std::fs::write(&outside, BYTES).expect("write the link's target");
    std::os::unix::fs::symlink(outside.as_std_path(), path.join("ext.so").as_std_path())
        .expect("plant the symlink");

    let found = tree.inspect_destination(Utf8Path::new("ext.so"), BYTES);

    assert!(
        matches!(found, Destination::Unreadable(_)),
        "a symlink is refused by the open even when its target holds the bytes"
    );
    assert!(
        found
            .rewrite_reason()
            .is_some_and(|reason| reason.starts_with("what is there cannot be read")),
        "the operator is told the destination could not be read"
    );
}

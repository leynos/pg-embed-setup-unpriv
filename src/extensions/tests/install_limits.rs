//! What installation refuses: escapes, caps and swapped archives.
//!
//! Split from `install`, which the limit cases pushed past the 400-line
//! module limit, and because these answer a different question. `install`
//! covers what a well-formed archive does to the tree. This covers the
//! inputs that must never reach the tree at all, and in every case asserts
//! that nothing was written as well as which error was returned: the error
//! kind alone would pass for an installer that refused after writing.

use rstest::rstest;

use super::{
    fixture::Entry,
    install::{fixture_entries, install, prepared},
};
use crate::{
    error::BootstrapErrorKind,
    extensions::install::{
        ARCHIVE_DECOMPRESSED_CAP,
        ENTRY_DECOMPRESSED_CAP,
        check_decompressed_caps,
    },
};

/// A symlinked parent component under the install tree is refused.
///
/// `classify_entry_path` vets the name the archive carries, which says
/// nothing about the tree it lands in. With `lib` a symlink to somewhere
/// else, `create_dir_all`, `NamedTempFile::new_in` and `persist` all follow
/// it and the shared object is installed outside `install_dir` entirely. The
/// assertion that the escape directory stayed empty is the one that fails if
/// the parent walk is removed; the error kind alone would not.
#[cfg(unix)]
#[test]
fn a_symlinked_parent_component_is_refused() {
    let prepared = prepared(&fixture_entries()).expect("fixture");
    let escape = prepared
        .install_dir
        .parent()
        .expect("install tree has a parent")
        .join("escape");
    std::fs::create_dir(&escape).expect("escape directory");
    let lib = prepared.install_dir.join("lib");
    std::fs::remove_dir_all(&lib).expect("clear lib");
    std::os::unix::fs::symlink(escape.as_std_path(), lib.as_std_path()).expect("symlink lib");

    let err = install(&prepared).expect_err("a symlinked parent must be refused");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionInstallFailed,
        "{err}"
    );
    assert!(
        !escape.join("fixture.so").exists(),
        "nothing may be installed through the symlink"
    );
    assert!(
        std::fs::read_dir(&escape)
            .expect("read escape")
            .next()
            .is_none(),
        "the escape directory must stay empty"
    );
}

/// A file that decompresses past the per-file cap is refused before any write.
///
/// The compressed cap says nothing about what an archive expands to: this
/// fixture is a run of zeroes just over the per-file limit, which gzip packs
/// into a few kilobytes, so it passes every compressed-size check and would
/// otherwise have been read into memory whole. The refusal comes from the tar
/// header in pass one, so the assertion that nothing reached the tree is the
/// part of this test that would fail if the cap moved back into the write
/// loop: the control file is planned after the oversized one and would be on
/// disk by the time the breach was noticed.
#[test]
fn install_refuses_an_entry_that_decompresses_past_the_cap() {
    // `Entry::File` borrows for `'static`, and this body outlives the fixture
    // by construction, so it is leaked rather than reshaping the enum for one
    // case. The test process reclaims it on exit.
    let oversized: &'static [u8] = Vec::leak(vec![
        0_u8;
        usize::try_from(ENTRY_DECOMPRESSED_CAP)
            .expect("cap fits")
            + 1
    ]);
    let entries = [
        Entry::File("lib/fixture.so", oversized),
        Entry::File(
            "share/extension/fixture.control",
            b"default_version = '1'\n",
        ),
    ];
    let mut prepared = prepared(&entries).expect("fixture");
    prepared.artifact.files = vec![
        "lib/fixture.so".to_owned(),
        "share/extension/fixture.control".to_owned(),
    ];
    let compressed = std::fs::metadata(&prepared.archive)
        .expect("archive metadata")
        .len();
    assert!(
        compressed < ENTRY_DECOMPRESSED_CAP,
        "the fixture must pass the compressed-size checks to be a fair test, was {compressed}"
    );
    let err = install(&prepared).expect_err("an entry over the cap is refused");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveInvalid);
    assert!(
        err.to_string().contains("per-file limit"),
        "the refusal must name the per-file cap: {err}"
    );
    // `install_tree` creates `lib/` and `share/extension/` empty, so the
    // files are what the assertion can be about, not the directories.
    for relative in ["lib/fixture.so", "share/extension/fixture.control"] {
        assert!(
            !prepared.install_dir.join(relative).exists(),
            "{relative} must not be written: the cap is enforced before pass two"
        );
    }
}

/// The decompressed caps are charged per entry and across the archive.
///
/// Exercised directly because reaching the archive-wide cap through a fixture
/// would mean four entries of sixty-four mebibytes each, and the accounting
/// is what these cases are about rather than the tar reading around it.
#[rstest]
#[case::first_entry_fits(0, 1, Ok(1))]
#[case::entry_at_the_cap(0, ENTRY_DECOMPRESSED_CAP, Ok(ENTRY_DECOMPRESSED_CAP))]
#[case::entry_over_the_cap(0, ENTRY_DECOMPRESSED_CAP + 1, Err("per-file limit"))]
#[case::total_at_the_cap(
    ARCHIVE_DECOMPRESSED_CAP - ENTRY_DECOMPRESSED_CAP,
    ENTRY_DECOMPRESSED_CAP,
    Ok(ARCHIVE_DECOMPRESSED_CAP)
)]
#[case::total_over_the_cap(
    ARCHIVE_DECOMPRESSED_CAP - ENTRY_DECOMPRESSED_CAP + 1,
    ENTRY_DECOMPRESSED_CAP,
    Err("limit for one archive")
)]
#[case::size_cannot_overflow_the_total(u64::MAX, 1, Err("limit for one archive"))]
fn decompressed_caps_are_charged_per_entry_and_per_archive(
    #[case] declared: u64,
    #[case] size: u64,
    #[case] expected: Result<u64, &str>,
) {
    let archive = camino::Utf8Path::new("/tmp/fixture.tar.gz");
    let relative = camino::Utf8Path::new("lib/fixture.so");
    match (
        check_decompressed_caps(archive, relative, size, declared),
        expected,
    ) {
        (Ok(total), Ok(want)) => assert_eq!(total, want, "running total"),
        (Err(err), Err(needle)) => {
            assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveInvalid);
            assert!(err.to_string().contains(needle), "{err}");
        }
        (got, want) => panic!("expected {want:?}, got {got:?}"),
    }
}

/// An archive swapped for a larger file after acquisition is rejected after
/// reading no more than the manifest size plus one byte.
#[test]
fn install_refuses_an_oversized_swapped_archive() {
    let prepared = prepared(&fixture_entries()).expect("fixture");
    let mut oversized = std::fs::read(&prepared.archive).expect("read");
    oversized.extend(std::iter::repeat_n(0_u8, 4096));
    std::fs::write(&prepared.archive, &oversized).expect("swap");
    let err = install(&prepared).expect_err("rejected");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionArchiveDigestMismatch
    );
}

/// A symlinked installation root is refused.
///
/// `install_dir` is a parent of every destination, so a symlink there routes
/// the whole tree elsewhere in one step, and it does so invisibly to a walk
/// that starts below it: `lib` resolves through the link, `create_dir_all`
/// creates it inside the link's target, and every component the walk inspects
/// is a real directory. The empty escape directory is the assertion that fails
/// if the walk starts below `install_dir` rather than at it; the error kind
/// alone would not.
#[cfg(unix)]
#[test]
fn a_symlinked_installation_root_is_refused() {
    let prepared = prepared(&fixture_entries()).expect("fixture");
    let escape = prepared
        .install_dir
        .parent()
        .expect("install tree has a parent")
        .join("escape");
    std::fs::create_dir(&escape).expect("escape directory");
    std::fs::remove_dir_all(&prepared.install_dir).expect("clear the install tree");
    std::os::unix::fs::symlink(escape.as_std_path(), prepared.install_dir.as_std_path())
        .expect("symlink the install tree");

    let err = install(&prepared).expect_err("a symlinked installation root must be refused");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionInstallFailed,
        "{err}"
    );
    assert!(
        std::fs::read_dir(&escape)
            .expect("read escape")
            .next()
            .is_none(),
        "the escape directory must stay empty"
    );
}

/// An identical file behind a symlinked destination is replaced, not followed.
///
/// A destination symlink whose target already holds the planned bytes takes
/// the identical-bytes branch, where a path-based digest, mode repair and
/// chown all resolve the link. A bootstrap running as root would then set the
/// tree's mode and ownership on a file outside the tree, without ever writing
/// a byte through the link. The outside file keeping mode `0o600` is the
/// assertion that fails when that branch works from the path instead of the
/// opened handle, and the destination being a regular file afterwards is what
/// the atomic replacement guarantees in its place.
#[cfg(unix)]
#[test]
fn an_identical_file_behind_a_symlinked_destination_is_not_followed() {
    use std::os::unix::fs::PermissionsExt;

    use super::fixture::FIXTURE_FILES;

    let prepared = prepared(&fixture_entries()).expect("fixture");
    let body = FIXTURE_FILES
        .iter()
        .find(|(name, _)| *name == "lib/fixture.so")
        .map(|(_, body)| *body)
        .expect("the fixture carries a shared object");
    let outside = prepared
        .install_dir
        .parent()
        .expect("install tree has a parent")
        .join("outside.so");
    std::fs::write(&outside, body).expect("write the file outside the tree");
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    let destination = prepared.install_dir.join("lib/fixture.so");
    std::os::unix::fs::symlink(outside.as_std_path(), destination.as_std_path())
        .expect("symlink the destination");

    install(&prepared).expect("install over a symlinked destination");

    assert_eq!(
        std::fs::symlink_metadata(&outside)
            .expect("outside metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600,
        "the file outside the tree must not be chmodded through the link"
    );
    assert!(
        !std::fs::symlink_metadata(&destination)
            .expect("destination metadata")
            .file_type()
            .is_symlink(),
        "the symlink must be replaced by a real file"
    );
}

/// A parent swapped for an escaping symlink after the tree is opened cannot
/// redirect the write.
///
/// This is the check-to-use race stated as a deterministic case. The other
/// symlink tests plant the link before installation begins, so a path-based
/// writer refuses them at the parent walk and passes. Here the link is planted
/// *after* the handle is open, which is what an attacker who owns the tree can
/// do between the check and the write, and no ordering of the two is required
/// to observe it: the tree's own root path is replaced, so a writer that
/// resolves paths afresh lands in `escape` while one that resolves through the
/// handle lands on the original directory.
///
/// The installation tree is owned by the demoted worker while this write may
/// run as root, so redirecting it is a privilege boundary crossing rather than
/// a tidiness problem. Asserting the file's location is what discriminates:
/// both writers report success.
#[cfg(unix)]
#[test]
fn a_parent_swapped_after_the_tree_is_opened_cannot_redirect_the_write() {
    use camino::Utf8Path;

    use crate::extensions::tree::InstallTree;

    let prepared = prepared(&fixture_entries()).expect("fixture");
    let root = prepared.install_dir.clone();
    let escape = root
        .parent()
        .expect("install tree has a parent")
        .join("escape");
    std::fs::create_dir_all(escape.join("lib")).expect("escape directory");

    // The handle is taken while the tree is still the real directory.
    let tree = InstallTree::open(&root).expect("open the installation tree");

    // The attacker now replaces the whole tree with a link to their own.
    let moved = root.with_file_name("moved-aside");
    std::fs::rename(root.as_std_path(), moved.as_std_path()).expect("move the tree aside");
    std::os::unix::fs::symlink(escape.as_std_path(), root.as_std_path()).expect("symlink the root");

    let relative = Utf8Path::new("lib/swapped.so");
    tree.create_parents(relative).expect("create parents");
    let (file, temp) = tree.create_temp_beside(relative).expect("temporary file");
    std::io::Write::write_all(&mut (&file), b"payload").expect("write payload");
    drop(file);
    tree.place(&temp, relative).expect("place the file");

    assert!(
        moved.join(relative).exists(),
        "the write must land in the directory the handle was opened on"
    );
    assert!(
        !escape.join(relative).exists(),
        "the write must not follow the symlink planted after the handle was opened"
    );
}

/// A validated parent swapped for an escaping symlink inside the race window
/// is refused, not followed.
///
/// This is the check-to-use race itself, driven deterministically. The writer
/// validates parents and then creates, writes and renames, and an attacker who
/// owns the installation tree — which the demoted worker does, while this write
/// may run as root — gets to act between those two steps. The test occupies
/// that window explicitly: it validates through the same handle the writer
/// uses, then replaces the validated `lib` with a link out of the tree, then
/// performs the write steps.
///
/// Refusal is the assertion, and the escape directory staying empty is what
/// discriminates: under the path-based standard-library calls this replaced,
/// every one of these steps succeeds and the file lands in `escape`.
#[cfg(unix)]
#[test]
fn a_validated_parent_swapped_inside_the_race_window_is_refused() {
    use camino::Utf8Path;

    use crate::extensions::tree::InstallTree;

    let prepared = prepared(&fixture_entries()).expect("fixture");
    let root = prepared.install_dir.clone();
    let escape = root
        .parent()
        .expect("install tree has a parent")
        .join("escape");
    std::fs::create_dir_all(&escape).expect("escape directory");

    let tree = InstallTree::open(&root).expect("open the installation tree");
    let relative = Utf8Path::new("lib/swapped.so");

    // The check half: lib is a real directory and the walk accepts it.
    tree.require_real_parents(relative)
        .expect("a real parent must validate");

    // The window: the owner of the tree replaces the validated parent.
    let lib = root.join("lib");
    std::fs::remove_dir_all(&lib).expect("clear lib");
    std::os::unix::fs::symlink(escape.as_std_path(), lib.as_std_path()).expect("symlink lib");

    // The use half: every step must refuse the link rather than follow it.
    let reported = match (
        tree.create_parents(relative),
        tree.create_temp_beside(relative),
    ) {
        (Err(err), _) | (Ok(()), Err(err)) => format!("{err:#}"),
        (Ok(()), Ok(_)) => {
            panic!("creating under a parent swapped for an escaping symlink must be refused")
        }
    };
    assert!(
        reported.contains("lib"),
        "the refusal must name the offending path, got: {reported}"
    );

    assert!(
        !escape.join("swapped.so").exists(),
        "nothing may be written through the swapped parent"
    );
    assert!(
        std::fs::read_dir(&escape)
            .expect("read escape")
            .next()
            .is_none(),
        "the escape directory must stay empty"
    );
}

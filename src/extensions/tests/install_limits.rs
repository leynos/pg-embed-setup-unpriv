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

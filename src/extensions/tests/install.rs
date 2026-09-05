//! Tests for archive validation, extraction and acquisition.

use std::path::Path;

use camino::Utf8PathBuf;
use color_eyre::eyre::Result;
use proptest::prelude::*;
use rstest::rstest;

use super::fixture::{
    Entry,
    FIXTURE_FILES,
    archive_bytes,
    artifact_for,
    install_tree,
    temp_root,
    write_file,
};
use crate::{
    error::BootstrapErrorKind,
    extensions::{
        ALLOWED_PREFIXES,
        ManifestArtifact,
        classify_entry_path,
        install::install_archive,
    },
};

/// A scratch tree with an archive written next to it and a matching artefact.
struct Prepared {
    _temp: tempfile::TempDir,
    install_dir: Utf8PathBuf,
    archive: Utf8PathBuf,
    artifact: ManifestArtifact,
}

/// Builds the archive from `entries` and stages it beside a fresh tree.
fn prepared(entries: &[Entry]) -> Result<Prepared> {
    let (temp, root) = temp_root()?;
    let install_dir = install_tree(&root)?;
    let bytes = archive_bytes(entries)?;
    let archive = write_file(&root, "fixture.tar.gz", &bytes)?;
    let artifact = artifact_for(&bytes, "fixture.tar.gz", "unused");
    Ok(Prepared {
        _temp: temp,
        install_dir,
        archive,
        artifact,
    })
}

fn fixture_entries() -> Vec<Entry> {
    FIXTURE_FILES
        .iter()
        .map(|(name, body)| Entry::File(name, body))
        .collect()
}

/// Runs the install against a prepared tree and returns the report.
fn install(prepared: &Prepared) -> crate::error::BootstrapResult<Vec<Utf8PathBuf>> {
    install_archive(&prepared.archive, &prepared.artifact, &prepared.install_dir)
}

#[cfg(unix)]
fn inode(path: &Utf8PathBuf) -> std::io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    Ok(std::fs::metadata(path)?.ino())
}

#[cfg(unix)]
fn mode(path: &Utf8PathBuf) -> std::io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o777)
}

/// Only plain files directly under lib/ or under share/extension/ are accepted.
#[rstest]
#[case::lib("lib/vector.so", Some("lib/vector.so"))]
#[case::dot_slash("./lib/vector.so", Some("lib/vector.so"))]
#[case::control(
    "share/extension/vector.control",
    Some("share/extension/vector.control")
)]
#[case::nested_share("share/extension/sub/x.sql", Some("share/extension/sub/x.sql"))]
#[case::bare_prefix("lib/", None)]
#[case::nested_lib("lib/bitcode/vector.bc", None)]
#[case::share_root("share/vector.control", None)]
#[case::headers("include/server/extension/vector/vector.h", None)]
#[case::bin("bin/psql", None)]
#[case::parent("lib/../bin/psql", None)]
#[case::absolute("/lib/vector.so", None)]
#[case::lookalike("libx/vector.so", None)]
#[case::empty("", None)]
fn classify_entry_path_cases(#[case] raw: &str, #[case] expected: Option<&str>) {
    assert_eq!(
        classify_entry_path(Path::new(raw))
            .as_deref()
            .map(camino::Utf8Path::as_str),
        expected
    );
}

proptest! {
    /// Whatever the input, an accepted path is canonical and under an allowed prefix.
    #[test]
    fn classify_entry_path_accepts_only_canonical_allowed_paths(raw in "[a-z./\\\\_-]{0,40}") {
        if let Some(accepted) = classify_entry_path(Path::new(&raw)) {
            let text = accepted.as_str();
            prop_assert!(ALLOWED_PREFIXES.iter().any(|prefix| text.starts_with(prefix)));
            prop_assert!(text.split('/').all(|part| !part.is_empty() && part != "." && part != ".."));
            prop_assert!(!text.contains('\\'));
            if text.starts_with("lib/") {
                prop_assert_eq!(text.matches('/').count(), 1);
            }
        }
    }
}

/// A well-formed archive installs its files with the expected modes.
#[test]
fn install_archive_writes_files_with_modes() {
    let prepared = prepared(&fixture_entries()).expect("fixture");
    let files = install(&prepared).expect("install");
    let names: Vec<&str> = files.iter().map(|f| f.as_str()).collect();
    assert_eq!(
        names,
        [
            "lib/fixture.so",
            "share/extension/fixture--1.0.sql",
            "share/extension/fixture.control"
        ]
    );
    for (name, body) in FIXTURE_FILES {
        assert_eq!(
            std::fs::read(prepared.install_dir.join(name)).expect("read"),
            body
        );
    }
    #[cfg(unix)]
    {
        assert_eq!(
            mode(&prepared.install_dir.join("lib/fixture.so")).expect("mode"),
            0o755
        );
        assert_eq!(
            mode(&prepared.install_dir.join("share/extension/fixture.control")).expect("mode"),
            0o644
        );
    }
}

/// Re-installing an identical archive keeps the files and inodes untouched.
#[cfg(unix)]
#[test]
fn install_archive_is_idempotent() {
    let prepared = prepared(&fixture_entries()).expect("fixture");
    install(&prepared).expect("first install");
    let module = prepared.install_dir.join("lib/fixture.so");
    let before = inode(&module).expect("inode");
    let files = install(&prepared).expect("second install");
    assert_eq!(
        before,
        inode(&module).expect("inode"),
        "an identical file must not be rewritten"
    );
    assert_eq!(files.len(), 3, "the report still lists every file");
}

/// A changed file is replaced through a new inode, never truncated in place.
#[cfg(unix)]
#[test]
fn install_archive_replaces_changed_files_atomically() {
    let prepared = prepared(&fixture_entries()).expect("fixture");
    let module = prepared.install_dir.join("lib/fixture.so");
    write_file(&prepared.install_dir, "lib/fixture.so", b"old contents").expect("seed");
    let before = inode(&module).expect("inode");
    install(&prepared).expect("install");
    assert_ne!(
        before,
        inode(&module).expect("inode"),
        "the shared object must land on a new inode"
    );
}

/// Every forbidden entry is `ExtensionArchiveInvalid` and nothing is written.
#[rstest]
#[case::symlink(Entry::Symlink("lib/link.so", "fixture.so"))]
#[case::hardlink(Entry::HardLink("lib/link.so", "lib/fixture.so"))]
#[case::absolute(Entry::File("/lib/evil.so", b"x"))]
#[case::parent(Entry::File("lib/../bin/psql", b"x"))]
#[case::third_prefix(Entry::File("include/server/x.h", b"x"))]
#[case::extra_file(Entry::File("lib/extra.so", b"x"))]
fn forbidden_entries_are_invalid_and_write_nothing(#[case] extra: Entry) {
    let mut entries = fixture_entries();
    entries.push(extra);
    let prepared = prepared(&entries).expect("fixture");
    let err = install(&prepared).expect_err("rejected");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionArchiveInvalid,
        "{err}"
    );
    assert!(
        !prepared.install_dir.join("lib/fixture.so").exists(),
        "validation must finish before any file is written"
    );
}

/// An archive missing a file the manifest lists is invalid.
#[test]
fn archive_missing_manifest_file_is_invalid() {
    let mut entries = fixture_entries();
    entries.pop();
    let prepared = prepared(&entries).expect("fixture");
    let err = install(&prepared).expect_err("rejected");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveInvalid);
    assert!(
        err.to_string().contains("differ from the manifest"),
        "{err}"
    );
}

/// Directory entries are tolerated; the files under them still install.
#[test]
fn directory_entries_are_tolerated() {
    let mut entries = vec![
        Entry::Dir("lib/"),
        Entry::Dir("share/"),
        Entry::Dir("share/extension/"),
    ];
    entries.extend(fixture_entries());
    let prepared = prepared(&entries).expect("fixture");
    assert_eq!(install(&prepared).expect("install").len(), 3);
}

/// Manifest lists and archive contents are compared in the same byte order.
#[test]
fn nested_share_paths_sort_consistently_with_the_manifest() {
    let entries = [
        Entry::File("share/extension/x/y.sql", b"nested"),
        Entry::File("share/extension/x-1.sql", b"flat"),
        Entry::File("share/extension/x.control", b"default_version = '1'\n"),
    ];
    let mut prepared = prepared(&entries).expect("fixture");
    prepared.artifact.files = entries
        .iter()
        .filter_map(|entry| match entry {
            Entry::File(name, _) => Some((*name).to_owned()),
            _ => None,
        })
        .collect();
    let files = install(&prepared).expect("a valid nested layout installs");
    assert_eq!(files.len(), 3);
}

/// The archive on disk must still hash to the manifest digest when installed.
#[test]
fn install_refuses_an_archive_changed_after_acquisition() {
    let prepared = prepared(&fixture_entries()).expect("fixture");
    std::fs::write(&prepared.archive, b"swapped after verification").expect("swap");
    let err = install(&prepared).expect_err("rejected");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionArchiveDigestMismatch
    );
}

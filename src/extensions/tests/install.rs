//! Tests for archive validation, extraction and acquisition.

use std::path::Path;

use proptest::prelude::*;
use rstest::rstest;

use super::fixture::{
    Entry,
    FIXTURE_FILES,
    archive_bytes,
    artifact_for,
    fixture_archive,
    install_tree,
    serve_once,
    temp_root,
    unreachable_url,
    write_file,
};
use crate::{
    error::BootstrapErrorKind,
    extensions::{
        ALLOWED_PREFIXES,
        ArchiveOrigin,
        Sha256Hex,
        archive::acquire,
        classify_entry_path,
        install::install_archive,
    },
};

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
    let (_temp, root) = temp_root().expect("fixture");
    let install_dir = install_tree(&root).expect("fixture");
    let bytes = fixture_archive().expect("fixture");
    let archive = write_file(&root, "fixture.tar.gz", &bytes).expect("fixture");
    let artifact = artifact_for(&bytes, "fixture.tar.gz", "unused");

    let files = install_archive(&archive, &artifact, &install_dir).expect("install");
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
        assert_eq!(std::fs::read(install_dir.join(name)).expect("read"), body);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let so_mode = std::fs::metadata(install_dir.join("lib/fixture.so"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        let ctl_mode = std::fs::metadata(install_dir.join("share/extension/fixture.control"))
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!((so_mode, ctl_mode), (0o755, 0o644));
    }
}

/// Re-installing an identical archive keeps the files and inodes untouched.
#[cfg(unix)]
#[test]
fn install_archive_is_idempotent() {
    use std::os::unix::fs::MetadataExt;
    let (_temp, root) = temp_root().expect("fixture");
    let install_dir = install_tree(&root).expect("fixture");
    let bytes = fixture_archive().expect("fixture");
    let archive = write_file(&root, "fixture.tar.gz", &bytes).expect("fixture");
    let artifact = artifact_for(&bytes, "fixture.tar.gz", "unused");
    install_archive(&archive, &artifact, &install_dir).expect("first install");
    let before = std::fs::metadata(install_dir.join("lib/fixture.so"))
        .expect("meta")
        .ino();
    let files = install_archive(&archive, &artifact, &install_dir).expect("second install");
    let after = std::fs::metadata(install_dir.join("lib/fixture.so"))
        .expect("meta")
        .ino();
    assert_eq!(before, after, "an identical file must not be rewritten");
    assert_eq!(files.len(), 3, "the report still lists every file");
}

/// A changed file is replaced through a new inode, never truncated in place.
#[cfg(unix)]
#[test]
fn install_archive_replaces_changed_files_atomically() {
    use std::os::unix::fs::MetadataExt;
    let (_temp, root) = temp_root().expect("fixture");
    let install_dir = install_tree(&root).expect("fixture");
    write_file(&install_dir, "lib/fixture.so", b"old contents").expect("fixture");
    let before = std::fs::metadata(install_dir.join("lib/fixture.so"))
        .expect("meta")
        .ino();
    let bytes = fixture_archive().expect("fixture");
    let archive = write_file(&root, "fixture.tar.gz", &bytes).expect("fixture");
    let artifact = artifact_for(&bytes, "fixture.tar.gz", "unused");
    install_archive(&archive, &artifact, &install_dir).expect("install");
    let after = std::fs::metadata(install_dir.join("lib/fixture.so"))
        .expect("meta")
        .ino();
    assert_ne!(before, after, "the shared object must land on a new inode");
}

fn fixture_entries() -> Vec<Entry> {
    FIXTURE_FILES
        .iter()
        .map(|(name, body)| Entry::File(name, body))
        .collect()
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
    let (_temp, root) = temp_root().expect("fixture");
    let install_dir = install_tree(&root).expect("fixture");
    let mut entries = fixture_entries();
    entries.push(extra);
    let bytes = archive_bytes(&entries).expect("fixture");
    let archive = write_file(&root, "bad.tar.gz", &bytes).expect("fixture");
    let artifact = artifact_for(&bytes, "bad.tar.gz", "unused");

    let err = install_archive(&archive, &artifact, &install_dir).expect_err("rejected");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionArchiveInvalid,
        "{err}"
    );
    assert!(
        !install_dir.join("lib/fixture.so").exists(),
        "validation must finish before any file is written"
    );
}

/// An archive missing a file the manifest lists is invalid.
#[test]
fn archive_missing_manifest_file_is_invalid() {
    let (_temp, root) = temp_root().expect("fixture");
    let install_dir = install_tree(&root).expect("fixture");
    let mut entries = fixture_entries();
    entries.pop();
    let bytes = archive_bytes(&entries).expect("fixture");
    let archive = write_file(&root, "short.tar.gz", &bytes).expect("fixture");
    let artifact = artifact_for(&bytes, "short.tar.gz", "unused");
    let err = install_archive(&archive, &artifact, &install_dir).expect_err("rejected");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveInvalid);
    assert!(
        err.to_string().contains("differ from the manifest"),
        "{err}"
    );
}

/// Directory entries are tolerated; the files under them still install.
#[test]
fn directory_entries_are_tolerated() {
    let (_temp, root) = temp_root().expect("fixture");
    let install_dir = install_tree(&root).expect("fixture");
    let mut entries = vec![
        Entry::Dir("lib/"),
        Entry::Dir("share/"),
        Entry::Dir("share/extension/"),
    ];
    entries.extend(fixture_entries());
    let bytes = archive_bytes(&entries).expect("fixture");
    let archive = write_file(&root, "dirs.tar.gz", &bytes).expect("fixture");
    let artifact = artifact_for(&bytes, "dirs.tar.gz", "unused");
    let files = install_archive(&archive, &artifact, &install_dir).expect("install");
    assert_eq!(files.len(), 3);
}

/// A verified cache entry is reused without touching the network.
#[test]
fn acquire_uses_valid_cached_copy() {
    let (_temp, root) = temp_root().expect("fixture");
    let cache = root.join("cache");
    let bytes = fixture_archive().expect("fixture");
    let artifact = artifact_for(
        &bytes,
        "fixture.tar.gz",
        &unreachable_url().expect("fixture"),
    );
    write_file(
        &cache.join(artifact.sha256.as_str()),
        "fixture.tar.gz",
        &bytes,
    )
    .expect("fixture");
    let acquired = acquire(&cache, &artifact).expect("cached");
    assert_eq!(acquired.origin, ArchiveOrigin::Cached);
    assert_eq!(
        acquired.path,
        cache.join(artifact.sha256.as_str()).join("fixture.tar.gz")
    );
}

/// A corrupt cache entry is replaced by a fresh, verified download.
#[test]
fn acquire_replaces_corrupt_cache_entry_by_download() {
    let (_temp, root) = temp_root().expect("fixture");
    let cache = root.join("cache");
    let bytes = fixture_archive().expect("fixture");
    let artifact = artifact_for(
        &bytes,
        "fixture.tar.gz",
        &serve_once(bytes.clone()).expect("fixture"),
    );
    write_file(
        &cache.join(artifact.sha256.as_str()),
        "fixture.tar.gz",
        b"corrupt",
    )
    .expect("fixture");
    let acquired = acquire(&cache, &artifact).expect("downloaded");
    assert_eq!(acquired.origin, ArchiveOrigin::Downloaded);
    assert_eq!(
        Sha256Hex::of_file(&acquired.path).expect("hash"),
        artifact.sha256
    );
}

/// Downloaded bytes that do not match the manifest digest are refused and discarded.
#[test]
fn acquire_rejects_digest_mismatch() {
    let (_temp, root) = temp_root().expect("fixture");
    let cache = root.join("cache");
    let bytes = fixture_archive().expect("fixture");
    let mut tampered = bytes.clone();
    tampered.push(0);
    let mut artifact = artifact_for(
        &bytes,
        "fixture.tar.gz",
        &serve_once(tampered).expect("fixture"),
    );
    artifact.size += 1;
    let err = acquire(&cache, &artifact).expect_err("mismatch");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionArchiveDigestMismatch
    );
    assert!(
        !cache
            .join(artifact.sha256.as_str())
            .join("fixture.tar.gz")
            .exists(),
        "a mismatching download must not be kept"
    );
}

/// A download failure with no cached copy is `ExtensionArchiveUnavailable`.
#[test]
fn acquire_reports_unreachable_download() {
    let (_temp, root) = temp_root().expect("fixture");
    let cache = root.join("cache");
    let bytes = fixture_archive().expect("fixture");
    let artifact = artifact_for(
        &bytes,
        "fixture.tar.gz",
        &unreachable_url().expect("fixture"),
    );
    let err = acquire(&cache, &artifact).expect_err("unreachable");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveUnavailable);
}

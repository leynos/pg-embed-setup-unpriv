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
        ManifestArtifact,
        Sha256Hex,
        archive::acquire,
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

/// A cache directory plus an artefact pointing at `url`.
struct CacheCase {
    _temp: tempfile::TempDir,
    cache: Utf8PathBuf,
    bytes: Vec<u8>,
    artifact: ManifestArtifact,
}

fn cache_case(url: &str) -> Result<CacheCase> {
    let (temp, root) = temp_root()?;
    let bytes = fixture_archive()?;
    let artifact = artifact_for(&bytes, "fixture.tar.gz", url);
    Ok(CacheCase {
        _temp: temp,
        cache: root.join("cache"),
        bytes,
        artifact,
    })
}

impl CacheCase {
    fn entry_path(&self) -> Utf8PathBuf {
        self.cache
            .join(self.artifact.sha256.as_str())
            .join("fixture.tar.gz")
    }

    fn seed(&self, bytes: &[u8]) -> Result<Utf8PathBuf> {
        write_file(
            &self.cache.join(self.artifact.sha256.as_str()),
            "fixture.tar.gz",
            bytes,
        )
    }
}

/// A verified cache entry is reused without touching the network.
#[test]
fn acquire_uses_valid_cached_copy() {
    let case = cache_case(&unreachable_url().expect("port")).expect("fixture");
    case.seed(&case.bytes).expect("seed");
    let acquired = acquire(&case.cache, &case.artifact).expect("cached");
    assert_eq!(acquired.origin, ArchiveOrigin::Cached);
    assert_eq!(acquired.path, case.entry_path());
}

/// A corrupt cache entry is replaced by a fresh, verified download.
#[test]
fn acquire_replaces_corrupt_cache_entry_by_download() {
    let bytes = fixture_archive().expect("fixture");
    let case = cache_case(&serve_once(bytes).expect("server")).expect("fixture");
    case.seed(b"corrupt").expect("seed");
    let acquired = acquire(&case.cache, &case.artifact).expect("downloaded");
    assert_eq!(acquired.origin, ArchiveOrigin::Downloaded);
    assert_eq!(
        Sha256Hex::of_file(&acquired.path).expect("hash"),
        case.artifact.sha256
    );
}

/// Downloaded bytes that do not match the manifest digest are refused and discarded.
#[test]
fn acquire_rejects_digest_mismatch() {
    let mut tampered = fixture_archive().expect("fixture");
    tampered.push(0);
    let mut case = cache_case(&serve_once(tampered).expect("server")).expect("fixture");
    case.artifact.size += 1;
    let err = acquire(&case.cache, &case.artifact).expect_err("mismatch");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionArchiveDigestMismatch
    );
    assert!(
        !case.entry_path().exists(),
        "a mismatching download must not be kept"
    );
}

/// A download failure with no cached copy is `ExtensionArchiveUnavailable`.
#[test]
fn acquire_reports_unreachable_download() {
    let case = cache_case(&unreachable_url().expect("port")).expect("fixture");
    let err = acquire(&case.cache, &case.artifact).expect_err("unreachable");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveUnavailable);
}

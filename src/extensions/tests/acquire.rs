//! Tests for archive acquisition: cache reuse, downloads, retries and URL policy.

use camino::Utf8PathBuf;
use color_eyre::eyre::Result;
use rstest::rstest;

use super::fixture::{
    CannedResponse,
    artifact_for,
    fixture_archive,
    serve_once,
    serve_sequence,
    temp_root,
    unreachable_url,
    write_file,
};
use crate::{
    error::BootstrapErrorKind,
    extensions::{ArchiveOrigin, ManifestArtifact, Sha256Hex, archive::acquire},
};

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

/// A valid cache entry is reused; a corrupt one is replaced by a verified download.
#[rstest]
#[case::valid_entry(true, false, ArchiveOrigin::Cached)]
#[case::corrupt_entry(false, true, ArchiveOrigin::Downloaded)]
fn acquire_uses_cache_or_redownloads(
    #[case] seed_real_bytes: bool,
    #[case] serve: bool,
    #[case] expected: ArchiveOrigin,
) {
    let bytes = fixture_archive().expect("fixture");
    let url = if serve {
        serve_once(bytes.clone()).expect("server")
    } else {
        unreachable_url().expect("port")
    };
    let case = cache_case(&url).expect("fixture");
    let seed: &[u8] = if seed_real_bytes {
        &case.bytes
    } else {
        b"corrupt"
    };
    case.seed(seed).expect("seed");
    let acquired = acquire(&case.cache, &case.artifact).expect("acquired");
    assert_eq!(acquired.origin, expected);
    assert_eq!(acquired.path, case.entry_path());
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

/// Transient failures are retried; the archive then downloads and verifies.
#[test]
fn acquire_retries_after_a_transient_server_error() {
    let bytes = fixture_archive().expect("fixture");
    let url = serve_sequence(vec![
        CannedResponse::status("503 Service Unavailable"),
        CannedResponse::ok(bytes),
    ])
    .expect("server");
    let case = cache_case(&url).expect("fixture");
    let acquired = acquire(&case.cache, &case.artifact).expect("downloaded on retry");
    assert_eq!(acquired.origin, ArchiveOrigin::Downloaded);
}

/// A 4xx is final: no retry, and the archive stays unavailable.
#[test]
fn acquire_does_not_retry_client_errors() {
    let bytes = fixture_archive().expect("fixture");
    let url = serve_sequence(vec![
        CannedResponse::status("404 Not Found"),
        CannedResponse::ok(bytes),
    ])
    .expect("server");
    let case = cache_case(&url).expect("fixture");
    let err = acquire(&case.cache, &case.artifact).expect_err("404 is final");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveUnavailable);
    assert!(err.to_string().contains("404"), "{err}");
}

/// A redirect away from HTTPS (or loopback HTTP) is refused.
#[test]
fn acquire_refuses_redirect_to_plain_http() {
    let url = serve_sequence(vec![CannedResponse::redirect(
        "http://example.invalid/fixture.tar.gz",
    )])
    .expect("server");
    let case = cache_case(&url).expect("fixture");
    let err = acquire(&case.cache, &case.artifact).expect_err("downgrade refused");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveUnavailable);
    assert!(err.to_string().contains("redirect"), "{err}");
}

/// Only https, or http to a loopback host, may be fetched.
#[rstest]
#[case::https("https://github.com/x/y.tar.gz", true)]
#[case::loopback_v4("http://127.0.0.1:1234/x.tar.gz", true)]
#[case::loopback_v6("http://[::1]:1234/x.tar.gz", true)]
#[case::localhost("http://localhost/x.tar.gz", true)]
#[case::plain_http("http://example.com/x.tar.gz", false)]
#[case::ftp("ftp://example.com/x.tar.gz", false)]
#[case::garbage("not a url", false)]
fn permitted_url_rules(#[case] url: &str, #[case] expected: bool) {
    assert_eq!(crate::extensions::is_permitted_url(url), expected);
}

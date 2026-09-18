//! Tests for reading manifest bytes from a path or a permitted URL.

use rstest::rstest;

use super::{
    super::fixture::{CannedResponse, serve_once, serve_sequence, temp_root, unreachable_url},
    path_source,
    sample_manifest,
};
use crate::{
    error::BootstrapErrorKind,
    extensions::{ManifestSource, Sha256Hex, running_version, version::parse_pg_config_version},
};

/// Loading from a path verifies the optional digest and reports a missing file.
#[test]
fn load_from_path_verifies_digest_and_reports_missing() {
    let (_temp, dir) = temp_root().expect("fixture");
    let text = sample_manifest().expect("fixture");
    let path = dir.join("manifest.json");
    std::fs::write(&path, &text).expect("write manifest");

    let good = ManifestSource::Path {
        path: path.clone(),
        sha256: Some(Sha256Hex::of_bytes(text.as_bytes())),
    };
    crate::extensions::manifest::load(&good).expect("digest matches");

    let bad = ManifestSource::Path {
        path: path.clone(),
        sha256: Some(Sha256Hex::of_bytes(b"other")),
    };
    let err = crate::extensions::manifest::load(&bad).expect_err("digest mismatch");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionManifestDigestMismatch
    );

    let missing = path_source(dir.join("absent.json"));
    let missing_err = crate::extensions::manifest::load(&missing).expect_err("missing file");
    assert_eq!(
        missing_err.kind(),
        BootstrapErrorKind::ExtensionManifestUnavailable
    );
}

/// Builds a `Url` manifest source for `url`, pinned to `digest`.
fn url_source(url: &str, digest: Sha256Hex) -> ManifestSource {
    ManifestSource::Url {
        url: url.to_owned(),
        sha256: digest,
    }
}

/// A loopback manifest served over `http://` loads when its digest matches.
///
/// The URL arm of `load` had no test at all: every manifest reached it from
/// disk, so the fetch, the size cap and the digest check over HTTP were only
/// ever exercised for archives. `is_permitted_url` admits loopback `http://`
/// for the manifest exactly as it does for the archives a manifest names, so
/// a local mirror is serving both here.
#[test]
fn load_from_a_loopback_url_verifies_the_digest() {
    let text = sample_manifest().expect("fixture");
    let url = serve_once(text.clone().into_bytes()).expect("server");
    let manifest =
        crate::extensions::manifest::load(&url_source(&url, Sha256Hex::of_bytes(text.as_bytes())))
            .expect("a pinned loopback manifest loads");
    assert_eq!(
        manifest.release, "v1.0.0",
        "the served release must survive"
    );
    assert_eq!(
        manifest.extensions.len(),
        1,
        "the served manifest declares one extension"
    );
}

/// A loopback manifest whose bytes do not match the pin is refused.
///
/// The pin is the whole point of requiring a digest for a URL source: the
/// bytes arrive over a connection the caller does not control, and nothing
/// else establishes that they are the manifest that was pinned.
#[test]
fn a_loopback_manifest_that_misses_its_pin_is_refused() {
    let text = sample_manifest().expect("fixture");
    let url = serve_once(text.into_bytes()).expect("server");
    let err = crate::extensions::manifest::load(&url_source(
        &url,
        Sha256Hex::of_bytes(b"a manifest that was never served"),
    ))
    .expect_err("the pin must be enforced over HTTP");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionManifestDigestMismatch
    );
}

/// A URL manifest that cannot be fetched or parsed reports the right kind.
///
/// Unavailable and invalid are different failures with different operator
/// responses, and the URL arm reached neither in any test: a refused
/// connection is the transport failing, a 404 is the origin answering, and
/// bytes that are not a manifest are the origin answering with the wrong
/// thing.
#[rstest]
#[case::refused(None, BootstrapErrorKind::ExtensionManifestUnavailable)]
#[case::not_found(Some(b"" as &[u8]), BootstrapErrorKind::ExtensionManifestUnavailable)]
#[case::not_a_manifest(Some(b"{}" as &[u8]), BootstrapErrorKind::ExtensionManifestInvalid)]
fn a_url_manifest_failure_keeps_its_kind(
    #[case] served: Option<&[u8]>,
    #[case] expected: BootstrapErrorKind,
) {
    let url = match served {
        None => unreachable_url().expect("port"),
        Some(b"") => serve_sequence(vec![CannedResponse::status("404 Not Found")]).expect("server"),
        Some(body) => serve_once(body.to_vec()).expect("server"),
    };
    let digest = Sha256Hex::of_bytes(served.unwrap_or_default());
    let err = crate::extensions::manifest::load(&url_source(&url, digest))
        .expect_err("the load must fail");
    assert_eq!(err.kind(), expected, "{err}");
}

/// `pg_config --version` output parses into a three-part version.
#[rstest]
#[case::plain("PostgreSQL 17.11\n", Some((17, 11)))]
#[case::debian("PostgreSQL 16.4 (Debian 16.4-1)", Some((16, 4)))]
#[case::devel("PostgreSQL 19devel", Some((19, 0)))]
#[case::garbage("nothing here", None)]
#[case::empty("", None)]
fn pg_config_version_parses(#[case] text: &str, #[case] expected: Option<(u64, u64)>) {
    let parsed = parse_pg_config_version(text).map(|v| (v.major, v.minor));
    assert_eq!(parsed, expected);
}

/// The versioned directory name identifies the server; otherwise fail closed.
#[test]
fn running_version_from_dir_name_or_fails_closed() {
    let (_temp, root) = temp_root().expect("fixture");
    let versioned = root.join("17.11.0");
    std::fs::create_dir_all(&versioned).expect("mkdir");
    let version = running_version(&versioned).expect("dir name parses");
    assert_eq!((version.major, version.minor), (17, 11));

    let unnamed = root.join("install");
    std::fs::create_dir_all(&unnamed).expect("mkdir");
    let err = running_version(&unnamed).expect_err("no pg_config either");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionUnavailable);
}

/// When the directory name is not a version, `bin/pg_config --version` decides.
#[cfg(unix)]
#[test]
fn running_version_falls_back_to_pg_config() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, root) = temp_root().expect("tempdir");
    let install = root.join("install");
    std::fs::create_dir_all(install.join("bin")).expect("mkdir");
    let script = install.join("bin/pg_config");
    std::fs::write(
        &script,
        "#!/bin/sh\necho 'PostgreSQL 16.4 (Debian 16.4-1)'\n",
    )
    .expect("write");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let version = running_version(&install).expect("pg_config answers");
    assert_eq!((version.major, version.minor), (16, 4));

    std::fs::write(&script, "#!/bin/sh\necho 'broken' >&2\nexit 3\n").expect("write");
    let err = running_version(&install).expect_err("non-zero exit fails closed");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionUnavailable);
    assert!(err.to_string().contains("broken"), "{err}");
}

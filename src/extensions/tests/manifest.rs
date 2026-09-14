//! Tests for manifest parsing, verification, selection and version detection.

use camino::Utf8PathBuf;
use color_eyre::eyre::Result;
use postgresql_embedded::Version;
use rstest::rstest;

use super::fixture::{
    CannedResponse,
    artifact_for,
    fixture_archive,
    manifest_json,
    serve_once,
    serve_sequence,
    unreachable_url,
};
use crate::{
    error::BootstrapErrorKind,
    extensions::{
        ArtifactQuery,
        ExtensionName,
        Manifest,
        ManifestSource,
        Sha256Hex,
        compile_target,
        manifest::ARCHIVE_SIZE_CAP,
        running_version,
        version::parse_pg_config_version,
    },
};

fn sample_manifest() -> Result<String> {
    let bytes = fixture_archive()?;
    Ok(manifest_json(
        "fixture",
        &[artifact_for(
            &bytes,
            "fixture.tar.gz",
            "https://example.invalid/fixture.tar.gz",
        )],
    ))
}

fn path_source(path: Utf8PathBuf) -> ManifestSource { ManifestSource::Path { path, sha256: None } }

/// A well-formed manifest parses and keeps its artefact digest.
#[test]
fn valid_manifest_parses() {
    let manifest =
        Manifest::parse(sample_manifest().expect("fixture").as_bytes()).expect("valid manifest");
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.extensions.len(), 1);
    let artifact = manifest
        .extensions
        .first()
        .and_then(|extension| extension.artifacts.first())
        .expect("fixture has one artefact");
    assert_eq!(
        artifact.sha256,
        Sha256Hex::of_bytes(&fixture_archive().expect("fixture"))
    );
}

/// Removing any required field is `ExtensionManifestInvalid`.
#[rstest]
#[case::release("release")]
#[case::generated_at("generated_at")]
#[case::extensions("extensions")]
#[case::name("extensions.0.name")]
#[case::package("extensions.0.package")]
#[case::version("extensions.0.version")]
#[case::source("extensions.0.source")]
#[case::commit("extensions.0.source.commit")]
#[case::artifacts("extensions.0.artifacts")]
#[case::postgresql("extensions.0.artifacts.0.postgresql")]
#[case::target("extensions.0.artifacts.0.target")]
#[case::file("extensions.0.artifacts.0.file")]
#[case::url("extensions.0.artifacts.0.url")]
#[case::sha256("extensions.0.artifacts.0.sha256")]
#[case::size("extensions.0.artifacts.0.size")]
#[case::files("extensions.0.artifacts.0.files")]
fn missing_required_field_is_invalid(#[case] pointer: &str) {
    let mut value: serde_json::Value =
        serde_json::from_str(&sample_manifest().expect("fixture")).expect("fixture is JSON");
    let (parent, key) = pointer.rsplit_once('.').unwrap_or(("", pointer));
    let parent_pointer = if parent.is_empty() {
        String::new()
    } else {
        format!("/{}", parent.replace('.', "/"))
    };
    let container = value
        .pointer_mut(&parent_pointer)
        .expect("parent exists")
        .as_object_mut()
        .expect("parent is an object");
    container.remove(key).expect("field present in fixture");
    let err = Manifest::parse(value.to_string().as_bytes()).expect_err("must be rejected");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionManifestInvalid,
        "{pointer}"
    );
}

/// Wrong schema versions, bad digests and bad artefact fields are invalid.
#[rstest]
#[case::schema_zero(r#""schema_version":1"#, r#""schema_version":0"#)]
#[case::schema_two(r#""schema_version":1"#, r#""schema_version":2"#)]
#[case::upper_digest(r#""sha256":""#, r#""sha256":"ABCDEF""#)]
#[case::short_digest(r#""sha256":""#, r#""sha256":"abc""#)]
#[case::bad_pg_version(r#""postgresql":"17.11.0""#, r#""postgresql":"seventeen""#)]
#[case::file_with_slash(r#""file":"fixture.tar.gz""#, r#""file":"../fixture.tar.gz""#)]
#[case::empty_files(r#""files":["#, r#""files":[],"ignored":["#)]
#[case::bad_name(r#""name":"fixture""#, r#""name":"Fixture""#)]
#[case::plain_http_url(
    r#""url":"https://example.invalid/fixture.tar.gz""#,
    r#""url":"http://example.invalid/fixture.tar.gz""#
)]
fn malformed_manifest_is_invalid(#[case] needle: &str, #[case] replacement: &str) {
    let text = sample_manifest().expect("fixture");
    assert!(text.contains(needle), "fixture must contain {needle}");
    let mutated = text.replacen(needle, replacement, 1);
    let err = Manifest::parse(mutated.as_bytes()).expect_err("must be rejected");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionManifestInvalid);
}

/// A declared archive size outside the accepted range is rejected at parse
/// time, so no later stage sees a size it cannot read or buffer.
///
/// `u64::MAX` is the case that matters: before the bound existed it reached
/// `read_verified`, where the read limit `size + 1` overflowed and the buffer
/// was preallocated from the size.
#[rstest]
#[case::zero(0)]
#[case::one_over_cap(ARCHIVE_SIZE_CAP + 1)]
#[case::u64_max(u64::MAX)]
fn artifact_size_outside_the_range_is_invalid(#[case] size: u64) {
    let bytes = fixture_archive().expect("fixture");
    let mut artifact = artifact_for(&bytes, "fixture.tar.gz", "https://example.invalid/f.tar.gz");
    artifact.size = size;
    let text = manifest_json("fixture", &[artifact]);
    let err = Manifest::parse(text.as_bytes()).expect_err("must be rejected");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionManifestInvalid);
}

/// The cap itself is accepted, so the bound is inclusive and a large but
/// plausible archive is not refused.
#[test]
fn artifact_size_at_the_cap_is_accepted() {
    let bytes = fixture_archive().expect("fixture");
    let mut artifact = artifact_for(&bytes, "fixture.tar.gz", "https://example.invalid/f.tar.gz");
    artifact.size = ARCHIVE_SIZE_CAP;
    let text = manifest_json("fixture", &[artifact]);
    Manifest::parse(text.as_bytes()).expect("the cap is within the range");
}

/// Bytes that are not JSON at all are `ExtensionManifestInvalid`.
#[test]
fn non_json_manifest_is_invalid() {
    let err = Manifest::parse(b"not json at all").expect_err("must be rejected");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionManifestInvalid);
}

/// Selection matches name, running major and target; the minor is ignored.
#[rstest]
#[case::match_(Version::new(17, 11, 0), true, "fixture", true)]
#[case::patch_ignored(Version::new(17, 11, 7), true, "fixture", true)]
#[case::older_minor(Version::new(17, 10, 0), true, "fixture", true)]
#[case::newer_minor(Version::new(17, 12, 3), true, "fixture", true)]
#[case::major_mismatch(Version::new(18, 11, 0), true, "fixture", false)]
#[case::target_mismatch(Version::new(17, 11, 0), false, "fixture", false)]
#[case::unknown_name(Version::new(17, 11, 0), true, "other", false)]
fn selection_rules(
    #[case] running: Version,
    #[case] same_target: bool,
    #[case] raw_name: &str,
    #[case] expected: bool,
) {
    let manifest =
        Manifest::parse(sample_manifest().expect("fixture").as_bytes()).expect("valid manifest");
    let target = if same_target {
        compile_target().to_owned()
    } else {
        "mips64-unknown-linux-gnuabi64".to_owned()
    };
    let name = ExtensionName::new(raw_name).expect("valid name");
    let query = ArtifactQuery {
        name: &name,
        running: &running,
        target: &target,
    };
    let source = path_source(Utf8PathBuf::from("/srv/manifest.json"));
    match manifest.select(query, &source) {
        Ok(selection) => {
            assert!(expected, "unexpected match");
            assert_eq!(selection.extension.name, "fixture");
        }
        Err(err) => {
            assert!(!expected, "unexpected miss: {err}");
            assert_eq!(err.kind(), BootstrapErrorKind::ExtensionUnavailable);
            assert!(err.to_string().contains("/srv/manifest.json"), "{err}");
        }
    }
}

/// The unavailable message lists what the manifest does offer.
#[test]
fn unavailable_message_lists_offers() {
    let manifest =
        Manifest::parse(sample_manifest().expect("fixture").as_bytes()).expect("valid manifest");
    let name = ExtensionName::new("fixture").expect("valid");
    let running = Version::new(16, 15, 0);
    let query = ArtifactQuery {
        name: &name,
        running: &running,
        target: compile_target(),
    };
    let err = manifest
        .select(query, &path_source(Utf8PathBuf::from("m.json")))
        .expect_err("no 16.x artifact");
    let message = err.to_string();
    assert!(message.contains("PostgreSQL 16 on"), "{message}");
    assert!(
        message.contains(&format!("17.11.0 on {}", compile_target())),
        "{message}"
    );
}

/// Loading from a path verifies the optional digest and reports a missing file.
#[test]
fn load_from_path_verifies_digest_and_reports_missing() {
    let (_temp, dir) = super::fixture::temp_root().expect("fixture");
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
    let (_temp, root) = super::fixture::temp_root().expect("fixture");
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
    let (_temp, root) = super::fixture::temp_root().expect("tempdir");
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

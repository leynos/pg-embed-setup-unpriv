//! Tests for manifest parsing, verification, selection and version detection.

use camino::Utf8PathBuf;
use color_eyre::eyre::Result;
use postgresql_embedded::Version;
use rstest::rstest;

use super::fixture::{artifact_for, fixture_archive, manifest_json};
use crate::{
    error::BootstrapErrorKind,
    extensions::{
        ArtifactQuery,
        ExtensionName,
        Manifest,
        ManifestSource,
        Sha256Hex,
        compile_target,
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

/// Bytes that are not JSON at all are `ExtensionManifestInvalid`.
#[test]
fn non_json_manifest_is_invalid() {
    let err = Manifest::parse(b"not json at all").expect_err("must be rejected");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionManifestInvalid);
}

/// Selection matches name, running major.minor and target exactly.
#[rstest]
#[case::match_(Version::new(17, 11, 0), true, "fixture", true)]
#[case::patch_ignored(Version::new(17, 11, 7), true, "fixture", true)]
#[case::minor_mismatch(Version::new(17, 10, 0), true, "fixture", false)]
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
    assert!(message.contains("PostgreSQL 16.15"), "{message}");
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

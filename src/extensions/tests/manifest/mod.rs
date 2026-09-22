//! Tests for manifest parsing and validation.
//!
//! Selection lives in `select.rs` and acquisition in `load.rs`, matching the
//! split of the module under test: what the schema accepts, which row
//! describes the running server, and where the bytes come from are three
//! questions and three sets of cases.

mod load;
mod select;

use camino::Utf8PathBuf;
use color_eyre::eyre::Result;
use rstest::rstest;

use super::fixture::{artifact_for, fixture_archive, manifest_json};
use crate::{
    error::BootstrapErrorKind,
    extensions::{Manifest, ManifestSource, Sha256Hex, manifest::ARCHIVE_SIZE_CAP},
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
///
/// Each case replaces one value through a JSON pointer rather than splicing
/// text, so the mutated manifest is still valid JSON by construction and the
/// rejection can only come from this crate's validation.
///
/// It was a text splice, and two cases were passing for the wrong reason.
/// The digest needle `"sha256":"` matched the opening of the field rather
/// than the whole `"sha256":"<64 hex>"` pair, so substituting
/// `"sha256":"ABCDEF"` left `"sha256":"ABCDEF"<the original digest>"` and the
/// manifest stopped being JSON. `Manifest::parse` refused it, the assertion
/// on the error kind held, and neither the uppercase nor the short digest
/// ever reached `Sha256Hex`.
///
/// The JSON round-trip below is what keeps that from coming back. It is
/// trivially true while the mutation goes through `pointer_mut`, which is
/// the point: it pins the property a splice would break. The error text
/// cannot serve instead, because `Sha256Hex` validates inside `Deserialize`
/// and its refusal reaches the caller through the same "not valid JSON for
/// schema 1" wrapper a syntax error does.
#[rstest]
#[case::schema_zero("/schema_version", serde_json::json!(0))]
#[case::schema_two("/schema_version", serde_json::json!(2))]
#[case::upper_digest(
    "/extensions/0/artifacts/0/sha256",
    serde_json::json!("AABBCCDD00112233445566778899AABBCCDDEEFF00112233445566778899AABB")
)]
#[case::short_digest("/extensions/0/artifacts/0/sha256", serde_json::json!("abc"))]
#[case::bad_pg_version(
    "/extensions/0/artifacts/0/postgresql",
    serde_json::json!("seventeen")
)]
#[case::file_with_slash(
    "/extensions/0/artifacts/0/file",
    serde_json::json!("../fixture.tar.gz")
)]
#[case::empty_files("/extensions/0/artifacts/0/files", serde_json::json!([]))]
#[case::bad_name("/extensions/0/name", serde_json::json!("Fixture"))]
#[case::plain_http_url(
    "/extensions/0/artifacts/0/url",
    serde_json::json!("http://example.invalid/fixture.tar.gz")
)]
fn malformed_manifest_is_invalid(#[case] pointer: &str, #[case] replacement: serde_json::Value) {
    let mut value: serde_json::Value =
        serde_json::from_str(&sample_manifest().expect("fixture")).expect("fixture is JSON");
    let slot = value
        .pointer_mut(pointer)
        .expect("the field exists in the fixture");
    *slot = replacement;
    let mutated = value.to_string();
    assert!(
        serde_json::from_str::<serde_json::Value>(&mutated).is_ok(),
        "{pointer}: the mutation must leave valid JSON, or the case reduces to serde refusing \
         nonsense and discriminates nothing"
    );
    let err = Manifest::parse(mutated.as_bytes()).expect_err("must be rejected");
    assert_eq!(
        err.kind(),
        BootstrapErrorKind::ExtensionManifestInvalid,
        "{pointer}"
    );
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

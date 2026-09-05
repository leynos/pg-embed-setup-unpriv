//! Tests for `PG_EXTENSIONS*` parsing and cache directory resolution.

use std::ffi::OsString;

use camino::Utf8PathBuf;
use rstest::rstest;

use crate::{
    PgEnvCfg,
    error::BootstrapErrorKind,
    extensions::{ExtensionName, ExtensionRequest, ManifestSource, resolve_extension_cache_dir},
    test_support::scoped_env,
};

const DIGEST: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
const UPPER_DIGEST: &str = "2CF24DBA5FB0A30E26E83B2AC5B9E29E1B161E5C1FA7425E73043362938B9824";

fn cfg(extensions: Option<&str>, manifest: Option<&str>, digest: Option<&str>) -> PgEnvCfg {
    PgEnvCfg {
        extensions: extensions.map(str::to_owned),
        extensions_manifest: manifest.map(str::to_owned),
        extensions_manifest_sha256: digest.map(str::to_owned),
        ..PgEnvCfg::default()
    }
}

/// Unset, empty and whitespace-only `PG_EXTENSIONS` all leave the hook inert.
#[rstest]
#[case::unset(None)]
#[case::empty(Some(""))]
#[case::whitespace(Some("  , ,\t"))]
fn no_extensions_declared_yields_none(#[case] raw: Option<&str>) {
    let request = ExtensionRequest::from_config(&cfg(raw, None, None)).expect("no error");
    assert!(request.is_none(), "expected None for {raw:?}");
}

/// Names are trimmed, validated and deduplicated in declaration order.
#[test]
fn names_are_trimmed_and_deduplicated_in_order() {
    let request = ExtensionRequest::from_config(&cfg(
        Some(" vector, postgis ,vector,, hstore "),
        Some("/srv/manifest.json"),
        None,
    ))
    .expect("valid config")
    .expect("names declared");
    let names: Vec<&str> = request.names.iter().map(ExtensionName::as_str).collect();
    assert_eq!(names, ["vector", "postgis", "hstore"]);
}

/// Each malformed declaration is `ExtensionConfigInvalid`.
#[rstest]
#[case::uppercase_name(Some("Vector"), Some("/srv/m.json"), None, "invalid")]
#[case::space_in_name(Some("vec tor"), Some("/srv/m.json"), None, "invalid")]
#[case::missing_manifest(Some("vector"), None, None, "PG_EXTENSIONS_MANIFEST")]
#[case::blank_manifest(Some("vector"), Some("   "), None, "PG_EXTENSIONS_MANIFEST")]
#[case::https_without_digest(
    Some("vector"),
    Some("https://x.invalid/m.json"),
    None,
    "required to pin"
)]
#[case::bad_digest(
    Some("vector"),
    Some("https://x.invalid/m.json"),
    Some("abc"),
    "64 lower-case hex"
)]
#[case::upper_digest(
    Some("vector"),
    Some("/srv/m.json"),
    Some(UPPER_DIGEST),
    "64 lower-case hex"
)]
#[case::other_scheme(
    Some("vector"),
    Some("ftp://x.invalid/m.json"),
    Some(DIGEST),
    "https:// URL or a filesystem path"
)]
fn malformed_declaration_is_config_invalid(
    #[case] extensions: Option<&str>,
    #[case] manifest: Option<&str>,
    #[case] digest: Option<&str>,
    #[case] needle: &str,
) {
    let err = ExtensionRequest::from_config(&cfg(extensions, manifest, digest))
        .expect_err("must be rejected");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionConfigInvalid);
    assert!(err.to_string().contains(needle), "{err}");
}

/// An HTTPS manifest with a digest becomes a `Url` source.
#[test]
fn https_manifest_with_digest_is_url_source() {
    let request = ExtensionRequest::from_config(&cfg(
        Some("vector"),
        Some("https://example.invalid/manifest.json"),
        Some(DIGEST),
    ))
    .expect("valid")
    .expect("declared");
    match request.manifest {
        ManifestSource::Url { url, sha256 } => {
            assert_eq!(url, "https://example.invalid/manifest.json");
            assert_eq!(sha256.as_str(), DIGEST);
        }
        ManifestSource::Path { .. } => panic!("expected a URL source"),
    }
}

/// A path manifest keeps an optional digest.
#[rstest]
#[case::without_digest(None)]
#[case::with_digest(Some(DIGEST))]
fn path_manifest_keeps_optional_digest(#[case] digest: Option<&str>) {
    let request = ExtensionRequest::from_config(&cfg(Some("vector"), Some("/srv/m.json"), digest))
        .expect("valid")
        .expect("declared");
    match request.manifest {
        ManifestSource::Path { path, sha256 } => {
            assert_eq!(path, Utf8PathBuf::from("/srv/m.json"));
            assert_eq!(
                sha256.map(|d| d.as_str().to_owned()),
                digest.map(str::to_owned)
            );
        }
        ManifestSource::Url { .. } => panic!("expected a path source"),
    }
}

/// An explicit `PG_EXTENSIONS_CACHE_DIR` in the config wins over resolution.
#[test]
fn explicit_cache_dir_is_used() {
    let request = ExtensionRequest::from_config(&PgEnvCfg {
        extensions_cache_dir: Some(Utf8PathBuf::from("/custom/ext-cache")),
        ..cfg(Some("vector"), Some("/srv/m.json"), None)
    })
    .expect("valid")
    .expect("declared");
    assert_eq!(request.cache_dir, Utf8PathBuf::from("/custom/ext-cache"));
}

/// Cache directory resolution mirrors the binary cache precedence.
#[rstest]
#[case::env_var(Some("/env/cache"), Some("/xdg"), "/env/cache")]
#[case::xdg(None, Some("/xdg"), "/xdg/pg-embedded/extensions")]
#[case::blank_env_uses_xdg(Some("  "), Some("/xdg"), "/xdg/pg-embedded/extensions")]
fn resolve_cache_dir_precedence(
    #[case] env: Option<&str>,
    #[case] xdg: Option<&str>,
    #[case] expected: &str,
) {
    let _guard = scoped_env([
        (
            OsString::from("PG_EXTENSIONS_CACHE_DIR"),
            env.map(OsString::from),
        ),
        (OsString::from("XDG_CACHE_HOME"), xdg.map(OsString::from)),
    ]);
    assert_eq!(resolve_extension_cache_dir(), Utf8PathBuf::from(expected));
}

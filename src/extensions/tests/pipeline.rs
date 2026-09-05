//! End-to-end tests for `install_extensions` against a scratch tree.

use camino::Utf8PathBuf;
use color_eyre::eyre::Result;

use super::fixture::{
    FIXTURE_FILES,
    artifact_for,
    fixture_archive,
    install_tree,
    manifest_json,
    temp_root,
    unreachable_url,
    write_file,
};
use crate::{
    error::BootstrapErrorKind,
    extensions::{
        ArchiveOrigin,
        ExtensionName,
        ExtensionRequest,
        ManifestSource,
        install_extensions,
        install_extensions_async,
    },
};

struct Scratch {
    _temp: tempfile::TempDir,
    root: Utf8PathBuf,
    install_dir: Utf8PathBuf,
    cache: Utf8PathBuf,
}

fn scratch() -> Result<Scratch> {
    let (temp, root) = temp_root()?;
    let install_dir = install_tree(&root)?;
    Ok(Scratch {
        _temp: temp,
        root: root.clone(),
        install_dir,
        cache: root.join("ext-cache"),
    })
}

/// Seeds the cache with the fixture archive and writes a matching manifest.
fn seeded_request(scratch: &Scratch, names: &[&str]) -> Result<ExtensionRequest> {
    let bytes = fixture_archive()?;
    let artifact = artifact_for(&bytes, "fixture.tar.gz", &unreachable_url()?);
    write_file(
        &scratch.cache.join(artifact.sha256.as_str()),
        "fixture.tar.gz",
        &bytes,
    )?;
    let manifest = write_file(
        &scratch.root,
        "manifest.json",
        manifest_json("fixture", &[artifact]).as_bytes(),
    )?;
    let parsed_names = names
        .iter()
        .map(|name| ExtensionName::new(*name))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ExtensionRequest {
        names: parsed_names,
        manifest: ManifestSource::Path {
            path: manifest,
            sha256: None,
        },
        cache_dir: scratch.cache.clone(),
    })
}

/// The full pipeline installs the files and reports what it did.
#[test]
fn install_extensions_reports_installed_files() {
    let scratch = scratch().expect("fixture");
    let request = seeded_request(&scratch, &["fixture"]).expect("fixture");
    let installed = install_extensions(&request, &scratch.install_dir).expect("install");
    assert_eq!(installed.len(), 1);
    let report = installed.first().expect("one report");
    assert_eq!(report.name.as_str(), "fixture");
    assert_eq!(report.version, "1.0.0");
    assert_eq!(report.postgresql, "17.11.0");
    assert_eq!(report.origin, ArchiveOrigin::Cached);
    assert_eq!(report.files.len(), FIXTURE_FILES.len());
    for (name, body) in FIXTURE_FILES {
        assert_eq!(
            std::fs::read(scratch.install_dir.join(name)).expect("read"),
            body
        );
    }
}

/// The async form produces the same result from inside a Tokio runtime.
#[test]
fn install_extensions_async_matches_sync() {
    let scratch = scratch().expect("fixture");
    let request = seeded_request(&scratch, &["fixture"]).expect("fixture");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let installed = runtime
        .block_on(install_extensions_async(
            request.clone(),
            scratch.install_dir.clone(),
        ))
        .expect("async install");
    assert_eq!(installed.len(), 1);
    // The sync entry point must also cope with being called from a runtime thread.
    let again = runtime
        .block_on(async { install_extensions(&request, &scratch.install_dir) })
        .expect("sync inside runtime");
    assert_eq!(
        again.first().map(|r| &r.files),
        installed.first().map(|r| &r.files)
    );
}

/// An unknown name fails closed before anything is written.
#[test]
fn unknown_extension_is_unavailable() {
    let scratch = scratch().expect("fixture");
    let request = seeded_request(&scratch, &["fixture", "missing"]).expect("fixture");
    let err = install_extensions(&request, &scratch.install_dir).expect_err("missing name");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionUnavailable);
    for (name, _) in FIXTURE_FILES {
        assert!(
            !scratch.install_dir.join(name).exists(),
            "{name} must not be written when a later name cannot be resolved"
        );
    }
}

/// A missing manifest is reported as unavailable.
#[test]
fn missing_manifest_is_unavailable() {
    let scratch = scratch().expect("fixture");
    let request = ExtensionRequest {
        names: vec![ExtensionName::new("fixture").expect("valid")],
        manifest: ManifestSource::Path {
            path: scratch.root.join("absent.json"),
            sha256: None,
        },
        cache_dir: scratch.cache.clone(),
    };
    let err = install_extensions(&request, &scratch.install_dir).expect_err("no manifest");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionManifestUnavailable);
    assert!(!scratch.install_dir.join("lib/fixture.so").exists());
}

/// A cached archive whose digest no longer matches the manifest is not installed.
#[test]
fn stale_cache_entry_is_not_trusted() {
    let scratch = scratch().expect("fixture");
    let request = seeded_request(&scratch, &["fixture"]).expect("fixture");
    let bytes = fixture_archive().expect("fixture");
    let digest = crate::extensions::Sha256Hex::of_bytes(&bytes);
    write_file(
        &scratch.cache.join(digest.as_str()),
        "fixture.tar.gz",
        b"tampered",
    )
    .expect("fixture");
    let err = install_extensions(&request, &scratch.install_dir).expect_err("re-download fails");
    assert_eq!(err.kind(), BootstrapErrorKind::ExtensionArchiveUnavailable);
    assert!(!scratch.install_dir.join("lib/fixture.so").exists());
}

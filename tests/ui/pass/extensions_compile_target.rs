//! The public extension surface compiles, and the compile target is usable in
//! a const context because `build.rs` exports `PG_EMBED_TARGET`.

use pg_embedded_setup_unpriv::extensions::{
    ALLOWED_PREFIXES,
    ArchiveOrigin,
    ExtensionName,
    ExtensionRequest,
    InstalledExtension,
    ManifestSource,
    Sha256Hex,
    compile_target,
    is_permitted_url,
};

const TARGET: &str = compile_target();

/// Exercises the public extension surface; returns a message on the first
/// expectation that fails so the Windows smoke path can report it.
pub fn verify_surface() -> Result<(), String> {
    if TARGET.is_empty() || !TARGET.contains('-') {
        return Err(format!("compile target {TARGET:?} is not a triple"));
    }
    if ALLOWED_PREFIXES != ["lib/", "share/extension/"] {
        return Err("allowed prefixes changed".to_owned());
    }
    let name = ExtensionName::new("vector").map_err(|err| err.to_string())?;
    let request = ExtensionRequest {
        names: vec![name.clone()],
        manifest: ManifestSource::Path {
            path: "manifest.json".into(),
            sha256: Some(Sha256Hex::of_bytes(b"pin")),
        },
        cache_dir: "cache".into(),
    };
    let report = InstalledExtension {
        name,
        version: "0.8.6".into(),
        postgresql: "17.11.0".into(),
        target: TARGET.into(),
        archive_sha256: Sha256Hex::of_bytes(b"archive"),
        origin: ArchiveOrigin::Cached,
        files: Vec::new(),
    };
    if report.target != compile_target() || request.names.len() != 1 {
        return Err("report and request disagree".to_owned());
    }
    if !is_permitted_url("https://example.invalid/x.tar.gz") {
        return Err("https must be permitted".to_owned());
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() { verify_surface().expect("extension surface should compile and run"); }

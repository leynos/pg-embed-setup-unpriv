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

fn main() {
    assert!(!TARGET.is_empty());
    assert!(TARGET.contains('-'));
    assert_eq!(ALLOWED_PREFIXES, ["lib/", "share/extension/"]);
    let request = ExtensionRequest {
        names: vec![ExtensionName::new("vector").expect("valid name")],
        manifest: ManifestSource::Path {
            path: "manifest.json".into(),
            sha256: Some(Sha256Hex::of_bytes(b"pin")),
        },
        cache_dir: "cache".into(),
    };
    let report = InstalledExtension {
        name: request.names[0].clone(),
        version: "0.8.6".into(),
        postgresql: "17.11.0".into(),
        target: TARGET.into(),
        archive_sha256: Sha256Hex::of_bytes(b"archive"),
        origin: ArchiveOrigin::Cached,
        files: Vec::new(),
    };
    assert_eq!(report.target, compile_target());
    assert!(is_permitted_url("https://example.invalid/x.tar.gz"));
}

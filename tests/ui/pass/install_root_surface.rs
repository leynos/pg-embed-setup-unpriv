//! Compile-time fixture for the install-root public surface.
//!
//! `tests/ui.rs` uses this as a non-Windows trybuild pass fixture and as a
//! directly included Windows smoke-compile module, so the exported
//! `default_paths_under`, `MIN_MAX_CONNECTIONS` and the `PgEnvCfg` fields
//! stay reachable from a consumer crate.

use camino::Utf8Path;
use pg_embedded_setup_unpriv::{MIN_MAX_CONNECTIONS, PgEnvCfg, default_paths_under};

pub fn verify_surface() -> Result<(), Box<dyn std::error::Error>> {
    let root = Utf8Path::new("/srv/project/pg");
    let (install, data) = default_paths_under(root);
    assert!(install.starts_with(root) && install.ends_with("install"));
    assert!(data.starts_with(root) && data.ends_with("data"));

    let cfg = PgEnvCfg {
        embed_root: Some(Utf8Path::new("/srv/project/pg").to_path_buf()),
        max_connections: Some(MIN_MAX_CONNECTIONS),
        ..PgEnvCfg::default()
    };
    let settings = cfg.to_settings()?;
    assert_eq!(
        settings
            .configuration
            .get("max_connections")
            .map(String::as_str),
        Some("4")
    );
    Ok(())
}

#[cfg(not(windows))]
fn main() { verify_surface().expect("install-root surface should compile and run"); }

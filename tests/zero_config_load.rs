//! `PgEnvCfg::load()` works with no configuration at all (#317).
//!
//! The crate's promise is zero configuration: with no `PG_*` variable and no
//! `.pg.toml`, loading must yield the default configuration, not an error.
//! `ortho_config` 0.9 prunes an all-`None` command-line layer to `null` when no
//! other source contributes a field, and deserializing that into the struct
//! fails, so the case needs its own test. A CI runner that happens to export a
//! `PG_*` variable, or a checkout that holds a `.pg.toml`, hides the defect, so
//! every test here loads through an injected environment that holds no `PG_*`
//! variable (which includes the prefixed configuration path selector) or
//! unprefixed `CONFIG_PATH`, points the XDG and home locations at an empty
//! directory, and pins that directory as the working directory. The process
//! environment and working directory are never touched.

use std::sync::Arc;

use cap_std::{ambient_authority, fs::Dir};
use color_eyre::eyre::{Result, eyre};
use ortho_config::MapEnv;
use pg_embedded_setup_unpriv::PgEnvCfg;
use tempfile::TempDir;

/// An environment with nothing configured, rooted at `empty`.
///
/// The XDG and home locations name `empty`, so no user-level file is
/// discovered, and `empty` is the working directory.
fn isolated_environment(empty: &std::path::Path) -> Arc<MapEnv> {
    let env = ["XDG_CONFIG_HOME", "HOME", "APPDATA", "USERPROFILE"]
        .into_iter()
        .fold(MapEnv::new().with_current_dir(empty), |env, name| {
            env.with_var(name, empty)
        });
    Arc::new(env)
}

/// With nothing configured anywhere, `load()` returns the default configuration.
#[test]
fn load_succeeds_with_no_configuration_at_all() -> Result<()> {
    let empty = TempDir::new()?;
    let cfg = PgEnvCfg::load_with_env(isolated_environment(empty.path()))
        .map_err(|err| eyre!("{err:?}"))?;
    let default = PgEnvCfg::default();
    if format!("{cfg:?}") != format!("{default:?}") {
        return Err(eyre!("expected the default configuration, got {cfg:?}"));
    }
    Ok(())
}

/// A malformed `.pg.toml` in the working directory is a mistake the user should
/// hear about, not an absent source: `load()` must still fail rather than fall
/// back to the defaults.
#[test]
fn load_still_fails_for_an_unreadable_file() -> Result<()> {
    let empty = TempDir::new()?;
    let root = Dir::open_ambient_dir(empty.path(), ambient_authority())?;
    root.write(".pg.toml", "= not toml")?;
    PgEnvCfg::load_with_env(isolated_environment(empty.path())).map_or_else(
        |_| Ok(()),
        |cfg| Err(eyre!("an unreadable file was defaulted to {cfg:?}")),
    )
}

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
use rstest::{fixture, rstest};
use tempfile::TempDir;

/// An environment with nothing configured, rooted at `empty`.
///
/// The XDG and home locations name `empty`, so no user-level file is
/// discovered, and `empty` is the working directory.
fn isolated_environment(empty: &std::path::Path) -> MapEnv {
    ["XDG_CONFIG_HOME", "HOME", "APPDATA", "USERPROFILE"]
        .into_iter()
        .fold(MapEnv::new().with_current_dir(empty), |env, name| {
            env.with_var(name, empty)
        })
}

/// An empty directory that doubles as the injected working directory.
#[fixture]
fn empty() -> TempDir {
    let dir = TempDir::new().expect("create an empty working directory");
    assert!(dir.path().is_dir(), "the working directory should exist");
    dir
}

/// Load with `port = 5543` in `.pg.toml` and, optionally, `PG_PORT` injected, and
/// return the resulting port. The expected port proves the second-stage load
/// reads the injected source: a process-backed load would see neither.
fn load_port(empty: &TempDir, pg_port: Option<&str>) -> Result<Option<u16>> {
    let root = Dir::open_ambient_dir(empty.path(), ambient_authority())?;
    root.write(".pg.toml", "port = 5543\n")?;
    let base = isolated_environment(empty.path());
    let env = match pg_port {
        Some(port) => base.with_var("PG_PORT", port),
        None => base,
    };
    let cfg = PgEnvCfg::load_with_env(Arc::new(env)).map_err(|err| eyre!("{err:?}"))?;
    Ok(cfg.port)
}

/// The injected file and `PG_PORT` feed the load, and the variable wins.
#[rstest]
#[case::file_only(None, Some(5543))]
#[case::environment_over_file(Some("5544"), Some(5544))]
fn load_reads_the_injected_sources(
    empty: TempDir,
    #[case] pg_port: Option<&str>,
    #[case] expected: Option<u16>,
) -> Result<()> {
    let port = load_port(&empty, pg_port)?;
    if port == expected {
        return Ok(());
    }
    Err(eyre!(
        "PG_PORT={pg_port:?} over a file port of 5543 gave {port:?}, expected {expected:?}"
    ))
}

/// With nothing configured anywhere, `load()` returns the default configuration.
#[rstest]
fn load_succeeds_with_no_configuration_at_all(empty: TempDir) -> Result<()> {
    let cfg = PgEnvCfg::load_with_env(Arc::new(isolated_environment(empty.path())))
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
#[rstest]
fn load_still_fails_for_an_unreadable_file(empty: TempDir) -> Result<()> {
    let root = Dir::open_ambient_dir(empty.path(), ambient_authority())?;
    root.write(".pg.toml", "= not toml")?;
    PgEnvCfg::load_with_env(Arc::new(isolated_environment(empty.path()))).map_or_else(
        |_| Ok(()),
        |cfg| Err(eyre!("an unreadable file was defaulted to {cfg:?}")),
    )
}

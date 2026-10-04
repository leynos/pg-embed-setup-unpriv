//! `PgEnvCfg::load()` works with no configuration at all (#317).
//!
//! The crate's promise is zero configuration: with no `PG_*` variable and no
//! `.pg.toml`, loading must yield the default configuration, not an error.
//! `ortho_config` 0.9 prunes an all-`None` command-line layer to `null` when no
//! other source contributes a field, and deserializing that into the struct
//! fails, so the case needs its own test. A CI runner that happens to export a
//! `PG_*` variable, or a checkout that holds a `.pg.toml`, hides the defect, so
//! every test here clears the `PG_*` variables (which include the configuration
//! path selector), the XDG and home locations, and runs from an empty directory.

use std::{ffi::OsString, path::PathBuf};

use cap_std::{ambient_authority, fs::Dir};
use color_eyre::eyre::{Result, eyre};
use pg_embedded_setup_unpriv::{PgEnvCfg, test_support::scoped_env};
use tempfile::TempDir;

/// Restores the working directory when dropped.
struct CwdGuard(PathBuf);

impl CwdGuard {
    fn enter(dir: &std::path::Path) -> Result<Self> {
        let previous = std::env::current_dir()?;
        std::env::set_current_dir(dir)?;
        Ok(Self(previous))
    }
}

impl Drop for CwdGuard {
    fn drop(&mut self) {
        // Best effort: a failure here cannot be reported from `drop`, and the
        // test process is about to end or enter another isolated test.
        drop(std::env::set_current_dir(&self.0));
    }
}

/// Every `PG_*` variable in the environment, set to unset, plus the XDG and
/// home locations pointed at `empty` so no user-level file is discovered.
fn isolated_environment(empty: &std::path::Path) -> Vec<(OsString, Option<OsString>)> {
    let mut vars: Vec<(OsString, Option<OsString>)> = std::env::vars_os()
        .filter(|(name, _)| name.to_string_lossy().starts_with("PG_"))
        .map(|(name, _)| (name, None))
        .collect();
    for name in ["XDG_CONFIG_HOME", "HOME", "APPDATA", "USERPROFILE"] {
        vars.push((name.into(), Some(empty.as_os_str().to_owned())));
    }
    vars.push(("XDG_CONFIG_DIRS".into(), None));
    vars
}

/// With nothing configured anywhere, `load()` returns the default configuration.
#[test]
fn load_succeeds_with_no_configuration_at_all() -> Result<()> {
    let empty = TempDir::new()?;
    let _env = scoped_env(isolated_environment(empty.path()));
    let _cwd = CwdGuard::enter(empty.path())?;
    let cfg = PgEnvCfg::load().map_err(|err| eyre!("{err:?}"))?;
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
    let _env = scoped_env(isolated_environment(empty.path()));
    let _cwd = CwdGuard::enter(empty.path())?;
    PgEnvCfg::load().map_or_else(
        |_| Ok(()),
        |cfg| Err(eyre!("an unreadable file was defaulted to {cfg:?}")),
    )
}

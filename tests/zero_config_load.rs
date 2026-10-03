//! `PgEnvCfg::load()` works with no configuration at all (#317).
//!
//! The crate's promise is zero configuration: with no `PG_*` variable and no
//! `.pg.toml`, loading must yield the default configuration, not an error.
//! `ortho_config` 0.9 prunes an all-`None` command-line layer to `null` when no
//! other source contributes a field, and deserializing that into the struct
//! fails, so the case needs its own test. A CI runner that happens to export a
//! `PG_*` variable hides the defect, so this test clears every one.

use std::ffi::OsString;

use color_eyre::eyre::{Result, eyre};
use pg_embedded_setup_unpriv::{PgEnvCfg, test_support::scoped_env};

/// Every `PG_*` variable in the environment, set to unset.
fn cleared_pg_variables() -> Vec<(OsString, Option<OsString>)> {
    std::env::vars_os()
        .filter(|(name, _)| name.to_string_lossy().starts_with("PG_"))
        .map(|(name, _)| (name, None))
        .collect()
}

/// With no `PG_*` variable set, `load()` returns the default configuration.
#[test]
fn load_succeeds_with_no_configuration_at_all() -> Result<()> {
    let _env = scoped_env(cleared_pg_variables());
    let cfg = PgEnvCfg::load().map_err(|err| eyre!("{err:?}"))?;
    let default = PgEnvCfg::default();
    if format!("{cfg:?}") != format!("{default:?}") {
        return Err(eyre!("expected the default configuration, got {cfg:?}"));
    }
    Ok(())
}

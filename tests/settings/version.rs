//! Tests for default and overridden PostgreSQL version requirements.

use color_eyre::eyre::{ensure, eyre};
use pg_embedded_setup_unpriv::PgEnvCfg;
use postgresql_embedded::VersionReq;
use rstest::rstest;

/// Without `PG_VERSION_REQ` the settings carry the reviewed default pin.
///
/// The pin keeps the crate off archives whose `share/timezone` is packed as tar
/// hard links, which the pinned `postgresql_archive` drops silently; see
/// `DEFAULT_VERSION_REQ` in `src/lib.rs` and `tests/timezone_regression.rs`.
#[rstest]
fn to_settings_defaults_to_the_pinned_postgresql_version() -> color_eyre::Result<()> {
    let settings = PgEnvCfg::default().to_settings()?;
    let expected = VersionReq::parse("=18.6.0").map_err(|err| eyre!(err))?;

    ensure!(
        settings.version == expected,
        "expected the default pin =18.6.0, found {}",
        settings.version,
    );
    Ok(())
}

/// `PG_VERSION_REQ` takes precedence over the default pin.
#[rstest]
fn version_req_override_wins_over_the_default_pin() -> color_eyre::Result<()> {
    let cfg = PgEnvCfg {
        version_req: Some("=17.4.0".into()),
        ..PgEnvCfg::default()
    };
    let settings = cfg.to_settings()?;
    let expected = VersionReq::parse("=17.4.0").map_err(|err| eyre!(err))?;

    ensure!(
        settings.version == expected,
        "expected the PG_VERSION_REQ override, found {}",
        settings.version,
    );
    Ok(())
}

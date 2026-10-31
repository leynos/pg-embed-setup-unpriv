//! Tests for default and overridden PostgreSQL version requirements.

use color_eyre::eyre::{ensure, eyre};
use pg_embedded_setup_unpriv::PgEnvCfg;
use postgresql_embedded::VersionReq;
use rstest::rstest;

/// Without `PG_VERSION_REQ` the settings accept any release, so upstream's
/// newest listed release wins.
///
/// There is no default pin: `postgresql_archive` 0.22.0 extracts the tar hard
/// links in the 18.6.1 and later archives, so named time zones resolve on
/// every release (see `tests/timezone_regression.rs`).
#[rstest]
fn to_settings_defaults_to_any_postgresql_version() -> color_eyre::Result<()> {
    let settings = PgEnvCfg::default().to_settings()?;

    ensure!(
        settings.version == VersionReq::STAR,
        "expected an unpinned default requirement, found {}",
        settings.version,
    );
    Ok(())
}

/// `PG_VERSION_REQ` takes precedence over the default.
#[rstest]
fn version_req_override_wins_over_the_default() -> color_eyre::Result<()> {
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

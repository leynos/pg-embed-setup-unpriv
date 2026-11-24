//! `PgEnvCfg::load()` works with no configuration at all (#317).
//!
//! The crate's promise is zero configuration: with no `PG_*` variable and no
//! `.pg.toml`, loading must yield the default configuration, not an error.
//! `ortho_config` 0.9 prunes an all-`None` command-line layer to `null` when no
//! other source contributes a field, and deserializing that into the struct
//! fails, so the case needs its own test. A CI runner that happens to export a
//! `PG_*` variable, or a checkout that holds a `.pg.toml`, hides the defect, so
//! the in-process tests load through an injected environment that holds no
//! `PG_*` variable (which includes the prefixed configuration path selector) or
//! unprefixed `CONFIG_PATH`, points the XDG and home locations at an empty
//! directory, and pins that directory as the working directory. The
//! process-backed `load()` entry point is covered by a child process, so the
//! test process never changes its own environment or working directory.

use std::{io::Write, path::Path, process::Command, sync::Arc};

use cap_std::{ambient_authority, fs::Dir};
use color_eyre::eyre::{Result, eyre};
use ortho_config::MapEnv;
use pg_embedded_setup_unpriv::PgEnvCfg;
use rstest::{fixture, rstest};
use tempfile::TempDir;

/// Names the variable whose presence makes the test binary act as the child.
///
/// It must not start with `PG_`: the loader would read it as configuration and
/// the child would no longer exercise the empty-configuration branch.
const CHILD_MARKER: &str = "ZERO_CONFIG_LOAD_CHILD";

/// Locations where `ortho_config` looks for a user-level configuration file.
const DISCOVERY_VARS: [&str; 5] = [
    "XDG_CONFIG_HOME",
    "XDG_CONFIG_DIRS",
    "HOME",
    "APPDATA",
    "USERPROFILE",
];

/// An environment with nothing configured, rooted at `empty`.
///
/// Every discovery location names `empty` (an unset `XDG_CONFIG_DIRS` would
/// make `ortho_config` search `/etc/xdg`), so no user-level file is
/// discovered, and `empty` is the working directory.
fn isolated_environment(empty: &Path) -> MapEnv {
    DISCOVERY_VARS
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

/// Write `.pg.toml` with `contents` into `dir`.
fn write_pg_toml(dir: &TempDir, contents: &str) -> Result<()> {
    let root = Dir::open_ambient_dir(dir.path(), ambient_authority())?;
    root.write(".pg.toml", contents)?;
    Ok(())
}

/// The injected `.pg.toml` and `PG_PORT` feed the load, and the variable wins.
///
/// The expected port proves the second-stage load reads the injected sources:
/// a process-backed load would see neither.
#[rstest]
#[case::file_only(Some("port = 5543\n"), None, Some(5543))]
#[case::environment_over_file(Some("port = 5543\n"), Some("5544"), Some(5544))]
#[case::environment_only(None, Some("5544"), Some(5544))]
fn load_reads_the_injected_sources(
    empty: TempDir,
    #[case] file: Option<&str>,
    #[case] pg_port: Option<&str>,
    #[case] expected: Option<u16>,
) -> Result<()> {
    if let Some(contents) = file {
        write_pg_toml(&empty, contents)?;
    }
    let base = isolated_environment(empty.path());
    let env = match pg_port {
        Some(port) => base.with_var("PG_PORT", port),
        None => base,
    };
    let cfg = PgEnvCfg::load_with_env(Arc::new(env)).map_err(|err| eyre!("{err:?}"))?;
    if cfg.port == expected {
        return Ok(());
    }
    Err(eyre!(
        "file {file:?} with PG_PORT={pg_port:?} gave port {:?}, expected {expected:?}",
        cfg.port
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
/// back to the defaults, and the failure must come from that file.
#[rstest]
fn load_still_fails_for_an_unreadable_file(empty: TempDir) -> Result<()> {
    write_pg_toml(&empty, "= not toml")?;
    match PgEnvCfg::load_with_env(Arc::new(isolated_environment(empty.path()))) {
        Ok(cfg) => Err(eyre!("an unreadable file was defaulted to {cfg:?}")),
        Err(err) => {
            let report = format!("{err:?}");
            if report.contains(".pg.toml") {
                Ok(())
            } else {
                Err(eyre!(
                    "the error does not name the malformed file: {report}"
                ))
            }
        }
    }
}

/// Body of the child process: run the process-backed `load()` entry point.
///
/// Outside the child this returns at once; the parent test below re-runs this
/// binary with the marker set.
#[test]
fn child_load_returns_the_default_configuration() -> Result<()> {
    if std::env::var_os(CHILD_MARKER).is_none() {
        return Ok(());
    }
    let cfg = PgEnvCfg::load().map_err(|err| eyre!("{err:?}"))?;
    let default = PgEnvCfg::default();
    if format!("{cfg:?}") != format!("{default:?}") {
        return Err(eyre!("expected the default configuration, got {cfg:?}"));
    }
    writeln!(
        std::io::stdout().lock(),
        "{CHILD_MARKER}: load() returned the default configuration"
    )?;
    Ok(())
}

/// The process-backed `load()` yields the defaults in a clean child process
/// whose environment names only the discovery locations.
#[rstest]
fn process_backed_load_returns_defaults_in_a_clean_child(empty: TempDir) -> Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "child_load_returns_the_default_configuration",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD_MARKER, "1")
        .current_dir(empty.path());
    for name in DISCOVERY_VARS {
        command.env(name, empty.path());
    }
    let output = command.output()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && stdout.contains(&format!("{CHILD_MARKER}: load() returned")) {
        return Ok(());
    }
    Err(eyre!(
        "the child did not confirm the default configuration ({}): {stdout}{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    ))
}

//! Downloads the specified `PostgreSQL` distribution, initialises the data
//! directory via `initdb`, and prepares the filesystem for unprivileged use.
//!
//! The server is **not** started — the installation is left ready for
//! subsequent use by [`TestCluster`](pg_embedded_setup_unpriv::TestCluster) or
//! other tools. Configuration is provided via environment variables parsed by
//! [`OrthoConfig`](https://github.com/leynos/ortho-config). The binary exits
//! with status code `0` on success and `1` on error.

use clap::{CommandFactory, Parser};
use std::io::Write;

const CONFIGURATION_HELP: &str = concat!(
    "Configuration is read from environment variables:\n",
    "  PG_VERSION_REQ          PostgreSQL semver requirement.\n",
    "  PG_PORT                 PostgreSQL port.\n",
    "  PG_SUPERUSER            Administrative PostgreSQL user.\n",
    "  PG_PASSWORD             Administrative PostgreSQL password.\n",
    "  PG_DATA_DIR             PostgreSQL data directory.\n",
    "  PG_RUNTIME_DIR          PostgreSQL binary installation directory.\n",
    "  PG_LOCALE               initdb locale.\n",
    "  PG_ENCODING             initdb encoding.\n",
    "  PG_BINARY_CACHE_DIR     Shared PostgreSQL binary cache directory."
);

#[derive(Debug, Parser)]
#[command(version, about, long_about = None, after_help = CONFIGURATION_HELP)]
struct Cli;

fn main() -> color_eyre::eyre::Result<()> {
    if std::env::args_os().any(|arg| arg == "--help" || arg == "-h") {
        Cli::command().print_help()?;
        std::io::stdout().write_all(b"\n")?;
        return Ok(());
    }
    if std::env::args_os().any(|arg| arg == "--version" || arg == "-V") {
        writeln!(std::io::stdout(), "{}", Cli::command().render_version())?;
        return Ok(());
    }
    pg_embedded_setup_unpriv::run().map_err(|err| color_eyre::eyre::eyre!(err))?;
    Ok(())
}

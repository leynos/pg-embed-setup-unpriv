//! Downloads the specified `PostgreSQL` distribution, initialises the data
//! directory via `initdb`, and prepares a platform-appropriate test cluster.
//!
//! The server is **not** started — the installation is left ready for
//! subsequent use by [`TestCluster`](pg_embedded_setup_unpriv::TestCluster) or
//! other tools. Configuration is provided via environment variables parsed by
//! [`OrthoConfig`](https://github.com/leynos/ortho-config). The binary exits
//! with status code `0` on success and `1` on error.

use clap::Parser;

const CLI_ABOUT: &str = "Initialises postgresql_embedded clusters with platform-appropriate setup";

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
#[command(version, about = CLI_ABOUT, long_about = None, after_help = CONFIGURATION_HELP)]
struct Cli;

fn main() -> color_eyre::eyre::Result<()> {
    let _cli = Cli::parse();
    pg_embedded_setup_unpriv::run().map_err(|err| color_eyre::eyre::eyre!(err))?;
    Ok(())
}

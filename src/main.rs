//! Downloads the specified `PostgreSQL` distribution, initialises the data
//! directory via `initdb`, and prepares the filesystem for unprivileged use.
//!
//! The server is **not** started — the installation is left ready for
//! subsequent use by [`TestCluster`](pg_embedded_setup_unpriv::TestCluster) or
//! other tools. Configuration is provided via environment variables parsed by
//! [`OrthoConfig`](https://github.com/leynos/ortho-config). The binary exits
//! with status code `0` on success and `1` on error.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(version, about, long_about = None)]
struct Cli;

fn main() -> color_eyre::eyre::Result<()> {
    Cli::parse();
    pg_embedded_setup_unpriv::run().map_err(|err| color_eyre::eyre::eyre!(err))?;
    Ok(())
}

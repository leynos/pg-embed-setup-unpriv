//! Identifies the `PostgreSQL` release installed in an embedded tree.

use std::process::Command;

use camino::Utf8Path;
use color_eyre::eyre::eyre;
use postgresql_embedded::Version;

use super::extension_error;
use crate::error::{BootstrapErrorKind, BootstrapResult};

/// Returns the version of the `PostgreSQL` installed at `install_dir`.
///
/// The versioned installation directory produced by both the binary cache
/// and a fresh download is named after the Theseus release (`17.11.0`), so
/// the directory name is tried first. When it does not parse, the output of
/// `bin/pg_config --version` is used instead. Anything else fails closed.
///
/// # Errors
///
/// Returns `ExtensionUnavailable` when neither route yields a version.
///
/// # Examples
///
/// ```no_run
/// use camino::Utf8Path;
/// use pg_embedded_setup_unpriv::extensions::running_version;
///
/// let version = running_version(Utf8Path::new("/var/tmp/pg/install/17.11.0"))?;
/// assert_eq!((version.major, version.minor), (17, 11));
/// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
/// ```
pub fn running_version(install_dir: &Utf8Path) -> BootstrapResult<Version> {
    if let Some(version) = version_from_dir_name(install_dir) {
        return Ok(version);
    }
    let output = Command::new(install_dir.join("bin").join("pg_config"))
        .arg("--version")
        .output()
        .map_err(|err| unknown(install_dir, &format!("pg_config could not run: {err}")))?;
    let text = String::from_utf8_lossy(&output.stdout);
    parse_pg_config_version(&text).ok_or_else(|| {
        unknown(
            install_dir,
            &format!("pg_config reported {:?}", text.trim()),
        )
    })
}

fn version_from_dir_name(install_dir: &Utf8Path) -> Option<Version> {
    Version::parse(install_dir.file_name()?).ok()
}

/// Parses `pg_config --version` output such as `PostgreSQL 17.11` or
/// `PostgreSQL 16.4 (Debian 16.4-1)` into a three-part version.
#[must_use]
pub fn parse_pg_config_version(text: &str) -> Option<Version> {
    let token = text.split_whitespace().nth(1)?;
    let numeric: String = token
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    let mut parts = numeric.split('.').filter(|part| !part.is_empty());
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().map_or(Some(0), |part| part.parse().ok())?;
    Some(Version::new(major, minor, 0))
}

fn unknown(install_dir: &Utf8Path, detail: &str) -> crate::error::BootstrapError {
    extension_error(
        BootstrapErrorKind::ExtensionUnavailable,
        eyre!("cannot determine the PostgreSQL version installed at {install_dir}: {detail}"),
    )
}

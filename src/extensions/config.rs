//! Translates `PG_EXTENSIONS*` into an [`ExtensionRequest`] and resolves the
//! extension cache directory.

use std::path::PathBuf;

use camino::Utf8PathBuf;
use color_eyre::eyre::eyre;

use super::{ExtensionName, ExtensionRequest, ManifestSource, Sha256Hex, extension_error};
use crate::{
    PgEnvCfg,
    error::{BootstrapErrorKind, BootstrapResult},
};

/// Subdirectory path within the XDG cache home.
const CACHE_SUBDIR: &str = "pg-embedded/extensions";

/// Configuration for the extension archive cache.
#[derive(Debug, Clone)]
pub struct ExtensionCacheConfig {
    /// Root directory for verified extension archives.
    pub cache_dir: Utf8PathBuf,
}

impl ExtensionCacheConfig {
    /// Creates a configuration using the resolved cache directory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache_dir: resolve_extension_cache_dir(),
        }
    }

    /// Creates a configuration with an explicit directory.
    #[must_use]
    pub const fn with_dir(cache_dir: Utf8PathBuf) -> Self { Self { cache_dir } }
}

impl Default for ExtensionCacheConfig {
    fn default() -> Self { Self::new() }
}

/// Resolves the extension cache directory.
///
/// The resolution order mirrors the binary cache:
///
/// 1. `PG_EXTENSIONS_CACHE_DIR` if set, non-empty and valid UTF-8
/// 2. `$XDG_CACHE_HOME/pg-embedded/extensions` if `XDG_CACHE_HOME` is set
/// 3. `~/.cache/pg-embedded/extensions`
/// 4. `std::env::temp_dir()/pg-embedded/extensions` as a last resort
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::extensions::resolve_extension_cache_dir;
///
/// assert!(!resolve_extension_cache_dir().as_str().is_empty());
/// ```
#[must_use]
pub fn resolve_extension_cache_dir() -> Utf8PathBuf {
    non_empty_env("PG_EXTENSIONS_CACHE_DIR")
        .map(Utf8PathBuf::from)
        .or_else(|| {
            non_empty_env("XDG_CACHE_HOME").map(|home| Utf8PathBuf::from(home).join(CACHE_SUBDIR))
        })
        .or_else(home_cache_dir)
        .unwrap_or_else(temp_cache_dir)
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|raw| raw.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn home_cache_dir() -> Option<Utf8PathBuf> {
    let home = Utf8PathBuf::from_path_buf(dirs::home_dir()?).ok()?;
    Some(home.join(".cache").join(CACHE_SUBDIR))
}

fn temp_cache_dir() -> Utf8PathBuf {
    let temp: PathBuf = std::env::temp_dir().join("pg-embedded").join("extensions");
    Utf8PathBuf::from_path_buf(temp)
        .unwrap_or_else(|path| Utf8PathBuf::from(path.to_string_lossy().into_owned()))
}

impl ExtensionRequest {
    /// Builds a request from the `PG_EXTENSIONS*` variables captured in `cfg`.
    ///
    /// Returns `Ok(None)` when `PG_EXTENSIONS` is unset, empty or whitespace,
    /// so the hook stays inert for consumers that do not declare extensions.
    ///
    /// # Errors
    ///
    /// Returns `ExtensionConfigInvalid` when names are declared without a
    /// manifest, when an HTTPS manifest has no digest, or when a name or
    /// digest is malformed.
    ///
    /// # Examples
    ///
    /// ```
    /// use pg_embedded_setup_unpriv::{PgEnvCfg, extensions::ExtensionRequest};
    ///
    /// let cfg = PgEnvCfg {
    ///     extensions: Some("vector, vector".into()),
    ///     extensions_manifest: Some("/srv/extensions/manifest.json".into()),
    ///     ..PgEnvCfg::default()
    /// };
    /// let request = ExtensionRequest::from_config(&cfg)?.expect("names were declared");
    /// assert_eq!(request.names.len(), 1, "duplicates collapse in order");
    /// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
    /// ```
    pub fn from_config(cfg: &PgEnvCfg) -> BootstrapResult<Option<Self>> {
        let names = parse_names(cfg.extensions.as_deref().unwrap_or_default())?;
        if names.is_empty() {
            return Ok(None);
        }
        let manifest = parse_manifest_source(
            cfg.extensions_manifest.as_deref(),
            cfg.extensions_manifest_sha256.as_deref(),
        )?;
        let cache_dir = cfg
            .extensions_cache_dir
            .clone()
            .unwrap_or_else(resolve_extension_cache_dir);
        Ok(Some(Self {
            names,
            manifest,
            cache_dir,
        }))
    }
}

/// Splits `PG_EXTENSIONS` on commas, trims, validates and deduplicates in order.
fn parse_names(raw: &str) -> BootstrapResult<Vec<ExtensionName>> {
    let mut names: Vec<ExtensionName> = Vec::new();
    for item in raw
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let name = ExtensionName::new(item)?;
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

fn parse_manifest_source(
    raw_location: Option<&str>,
    digest: Option<&str>,
) -> BootstrapResult<ManifestSource> {
    let Some(location) = raw_location
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Err(config_error(
            "PG_EXTENSIONS is set but PG_EXTENSIONS_MANIFEST is not; the manifest pins which \
             archives may be installed",
        ));
    };
    let pinned = digest
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(parse_digest)
        .transpose()?;
    if location.starts_with("https://") {
        let Some(sha256) = pinned else {
            return Err(config_error(
                "PG_EXTENSIONS_MANIFEST is an https:// URL, so PG_EXTENSIONS_MANIFEST_SHA256 is \
                 required to pin it",
            ));
        };
        return Ok(ManifestSource::Url {
            url: location.to_owned(),
            sha256,
        });
    }
    if location.contains("://") {
        return Err(config_error(
            "PG_EXTENSIONS_MANIFEST must be an https:// URL or a filesystem path",
        ));
    }
    Ok(ManifestSource::Path {
        path: Utf8PathBuf::from(location),
        sha256: pinned,
    })
}

fn parse_digest(value: &str) -> BootstrapResult<Sha256Hex> {
    Sha256Hex::parse(value)
        .map_err(|err| config_error(&format!("PG_EXTENSIONS_MANIFEST_SHA256: {err}")))
}

fn config_error(message: &str) -> crate::error::BootstrapError {
    extension_error(
        BootstrapErrorKind::ExtensionConfigInvalid,
        eyre!("{message}"),
    )
}

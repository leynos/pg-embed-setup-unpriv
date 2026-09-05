//! Post-setup hook shared by every lifecycle: cache the pristine binaries,
//! then install declared extensions before the server starts.
//!
//! Running the hook after `Setup` and before `Start` means the files are in
//! place when `CREATE EXTENSION` runs, and populating the binary cache first
//! keeps that cache free of extension files.

use camino::Utf8PathBuf;
use color_eyre::eyre::eyre;

use super::{cache_integration, installation};
#[cfg(feature = "async-api")]
use crate::extensions::install_extensions_async;
use crate::{
    TestBootstrapSettings,
    cache::BinaryCacheConfig,
    error::{BootstrapError, BootstrapErrorKind, BootstrapResult},
    extensions::{ExtensionRequest, install_extensions},
};

/// Binary-cache state carried from the pre-setup lookup into the hook.
#[derive(Debug, Clone, Copy)]
pub(super) struct PostSetup<'a> {
    /// Cache the lifecycle consulted before `Setup`.
    pub(super) cache_config: &'a BinaryCacheConfig,
    /// Whether `Setup` reused cached binaries.
    pub(super) cache_hit: bool,
}

/// Runs the post-setup steps: resolve the versioned directory, populate the
/// binary cache on a miss, then install declared extensions.
pub(super) fn run_post_setup(
    bootstrap: &mut TestBootstrapSettings,
    post: PostSetup<'_>,
) -> BootstrapResult<()> {
    installation::refresh_worker_installation_dir(bootstrap);
    populate_cache_on_miss(post, bootstrap);
    let Some((request, install_dir)) = extension_target(bootstrap)? else {
        return Ok(());
    };
    bootstrap.installed_extensions = install_extensions(&request, &install_dir)?;
    Ok(())
}

/// Async twin of [`run_post_setup`]; the install runs on the blocking pool.
#[cfg(feature = "async-api")]
pub(super) async fn run_post_setup_async(
    bootstrap: &mut TestBootstrapSettings,
    post: PostSetup<'_>,
) -> BootstrapResult<()> {
    installation::refresh_worker_installation_dir(bootstrap);
    populate_cache_on_miss(post, bootstrap);
    let Some((request, install_dir)) = extension_target(bootstrap)? else {
        return Ok(());
    };
    bootstrap.installed_extensions = install_extensions_async(request, install_dir).await?;
    Ok(())
}

/// Populates the binary cache after a successful setup on a cache miss.
pub(super) fn populate_cache_on_miss(post: PostSetup<'_>, bootstrap: &TestBootstrapSettings) {
    if !post.cache_hit {
        cache_integration::try_populate_binary_cache(post.cache_config, &bootstrap.settings);
    }
}

/// Returns the declared request and the versioned install root, or `None`
/// when no extensions were declared.
fn extension_target(
    bootstrap: &TestBootstrapSettings,
) -> BootstrapResult<Option<(ExtensionRequest, Utf8PathBuf)>> {
    let Some(request) = bootstrap.extensions.as_ref() else {
        return Ok(None);
    };
    let resolved = installation::resolve_installed_dir(&bootstrap.settings).ok_or_else(|| {
        install_failed(format!(
            "no PostgreSQL installation with a bin/ directory found under {}",
            bootstrap.settings.installation_dir.display()
        ))
    })?;
    let install_dir = Utf8PathBuf::from_path_buf(resolved).map_err(|path| {
        install_failed(format!(
            "installation directory {} is not valid UTF-8",
            path.display()
        ))
    })?;
    Ok(Some((request.clone(), install_dir)))
}

fn install_failed(message: String) -> BootstrapError {
    BootstrapError::new(BootstrapErrorKind::ExtensionInstallFailed, eyre!(message))
}

//! Decides whether a derived data directory becomes a per-cluster slot.
//!
//! A test bootstrap takes a slot of its own under the derived `data`
//! directory, so concurrent processes never share one (ADR 005). The
//! setup-only `run` keeps `data` itself, because its cluster must outlive its
//! process, and an explicit `PG_DATA_DIR` is never a slot.

use camino::Utf8Path;
use postgresql_embedded::Settings;

use super::{SettingsPaths, cluster_slot};
use crate::{error::BootstrapResult, observability::LOG_TARGET};

/// How a derived data directory is used (ADR 005).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DataLayout {
    /// A per-cluster slot under the derived `data` directory, swept once its
    /// owner exits: for a cluster a test process runs.
    PerCluster,
    /// The derived `data` directory itself, kept after the process exits: for
    /// [`run`](crate::run), whose initialized cluster is its product.
    Persistent,
}

/// Gives a derived data directory a per-cluster slot of its own (#261).
///
/// A data directory derived from the root (`PG_EMBED_ROOT` or the per-user
/// default) becomes the parent of one directory per cluster, so concurrent
/// processes never share one. An explicit `PG_DATA_DIR` is left exactly as
/// given, with its password file in the install tree as before, and so is a
/// [`DataLayout::Persistent`] bootstrap, whose cluster must outlive the process
/// that made it: a slot is swept once its owner exits.
///
/// Returns whether a slot was claimed.
pub(super) fn claim_derived_slot(
    settings: &mut Settings,
    paths: &mut SettingsPaths,
    layout: DataLayout,
) -> BootstrapResult<bool> {
    if layout == DataLayout::Persistent || !paths.data_default {
        return Ok(false);
    }
    warn_on_old_layout(&paths.data_dir);
    let slot = cluster_slot::claim_slot(&paths.data_dir)?;
    settings.data_dir = slot.data_dir.clone().into_std_path_buf();
    settings.password_file = slot.password_file.clone().into_std_path_buf();
    paths.data_dir = slot.data_dir;
    paths.password_file = slot.password_file;
    Ok(true)
}

/// Warns when a cluster from the single-directory layout of 0.6.0 and
/// earlier, or one that [`run`](crate::run) initialized, still sits directly in
/// the data parent, where no per-cluster cluster uses it.
fn warn_on_old_layout(data_parent: &Utf8Path) {
    if data_parent.join("PG_VERSION").is_file() {
        tracing::warn!(
            target: LOG_TARGET,
            data_parent = %data_parent,
            "a cluster from the 0.6.0 layout, or one initialized by `run`, remains here; \
             test clusters now use per-cluster directories beneath it, so it is not used by \
             them (set PG_DATA_DIR to share it)"
        );
    }
}

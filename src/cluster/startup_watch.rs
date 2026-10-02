//! The orphan watcher's hooks into the start lifecycle (#287).
//!
//! Linux arms a watcher before the server starts and releases it when the
//! start fails; elsewhere both are no-ops and the sweep is the reclaim.

use super::TestBootstrapSettings;

/// Starts the watcher that stops the server if this process is killed (#287).
///
/// It is armed before the lifecycle runs, so it is in place before the server
/// starts; the server's own `postmaster.pid` is read only when the owner dies.
///
/// Linux only; elsewhere the next bootstrap's sweep reclaims the server.
pub(super) fn watch_for_owner_death(bootstrap: &TestBootstrapSettings) {
    #[cfg(target_os = "linux")]
    let _watching = crate::bootstrap::watch_slot_owner(&bootstrap.settings.data_dir);
    #[cfg(not(target_os = "linux"))]
    let _ = bootstrap;
}

/// Ends the watcher armed for a start that failed, so a cluster that never
/// came up does not leave a watcher waiting for the process to exit.
pub(super) fn release_owner_watcher(bootstrap: &TestBootstrapSettings) {
    #[cfg(target_os = "linux")]
    let _released = crate::bootstrap::release_watcher(&bootstrap.settings.data_dir);
    #[cfg(not(target_os = "linux"))]
    let _ = bootstrap;
}

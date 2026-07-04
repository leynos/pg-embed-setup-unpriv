//! Best-effort cleanup helpers for the process-exit shutdown hook.

/// Best-effort directory cleanup after the postmaster has stopped.
pub(super) fn best_effort_cleanup(state: &super::ShutdownWork) {
    super::super::cleanup::cleanup_in_process(
        state.cleanup_mode,
        &state.settings,
        "atexit-shutdown-hook",
    );
}

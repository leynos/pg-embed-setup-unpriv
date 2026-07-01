//! Compile-time fixture for the shutdown-hook public test surface.
//!
//! `tests/ui.rs` uses this as a non-Windows trybuild pass fixture and as a
//! directly included Windows smoke-compile module.

use pg_embedded_setup_unpriv::BootstrapResult;
use pg_embedded_setup_unpriv::test_support::{
    PostmasterPid, process_is_running, read_postmaster_pid,
};
use std::path::Path;

pub fn verify_surface() -> BootstrapResult<()> {
    let missing_pid =
        read_postmaster_pid(Path::new("target/nonexistent-shutdown-hook-ui-fixture"))?;
    assert!(missing_pid.is_none());

    let zero_pid: PostmasterPid = 0;
    assert!(!process_is_running(zero_pid)?);
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    verify_surface().expect("shutdown-hook test-support surface should compile and run");
}

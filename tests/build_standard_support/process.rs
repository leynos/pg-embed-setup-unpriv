//! The contract's process boundary: the one place it spawns `make`. Everything
//! else reads text it is handed, so a test of the policy or the parsers runs no
//! process; a test that wants the repository's own recipes passes [`real_make`].

use std::process::Command;

use super::make::{Host, Target};

/// The integration adapter: runs the real `make -n` in the crate's directory and
/// reports a spawn failure or an undefined target as an error.
///
/// # Parameters
///
/// - `target`: the Makefile target to print the commands of.
/// - `host`: the host `make` is told it runs on, through `BUILD_HOST_OS`.
///
/// # Returns
///
/// The commands `make -n` printed for the target.
///
/// # Errors
///
/// Returns the reason when `make` cannot run or the target is not defined.
pub fn real_make(target: Target<'_>, host: Host) -> Result<String, String> {
    let name = target.name();
    let output = Command::new("make")
        .args([
            "-n",
            "-B",
            &format!("BUILD_HOST_OS={}", host.make_value()),
            name,
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .map_err(|error| format!("running make: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "`make -n {name}` failed, so it is not defined: {stderr}"
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

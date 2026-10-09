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
    decode_stdout(output.stdout, target)
}

/// Decodes what `make -n` printed, refusing bytes that are not UTF-8 instead of replacing them: a
/// replaced byte could turn a flag the contract looks for into text it reads as something else.
///
/// # Errors
///
/// Returns the reason when the bytes are not valid UTF-8.
fn decode_stdout(stdout: Vec<u8>, target: Target<'_>) -> Result<String, String> {
    String::from_utf8(stdout)
        .map_err(|error| format!("`make -n {target}` printed bytes that are not UTF-8: {error}"))
}

#[cfg(test)]
mod tests {
    //! The process boundary refuses bytes that are not UTF-8 and passes text through unchanged.

    use super::{Target, decode_stdout};

    #[test]
    fn valid_utf8_passes_through_unchanged() {
        let text = "RUSTFLAGS=\"-D warnings\" cargo test \u{e9}\n";
        assert_eq!(
            decode_stdout(text.as_bytes().to_vec(), Target("test")),
            Ok(text.to_owned())
        );
    }

    #[test]
    fn invalid_utf8_is_refused_not_replaced() {
        let bytes = b"cargo test \xff\xfe\n".to_vec();
        let refused = decode_stdout(bytes, Target("test"));
        assert!(
            refused
                .as_ref()
                .is_err_and(|reason| reason.contains("not UTF-8")),
            "{refused:?}"
        );
    }
}

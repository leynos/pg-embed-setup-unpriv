//! The port through which the preflight asks the host about shared libraries.
//!
//! Production code uses [`LddResolver`], which runs the platform's `ldd`.
//! Tests supply their own implementation, so no real host is needed.

use std::process::Command;

use camino::Utf8Path;

/// The host cannot answer the resolution question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolverUnavailable {
    /// A short, human-readable cause.
    pub(crate) reason: String,
}

/// Resolves an object's shared-library dependencies against the host loader.
pub(crate) trait SharedLibraryResolver {
    /// Returns the SONAMEs `object` needs that the host cannot resolve.
    ///
    /// An empty list means every dependency resolves. A statically linked
    /// object has no dependencies and so also yields an empty list.
    ///
    /// # Errors
    /// Returns [`ResolverUnavailable`] when the host cannot answer.
    fn unresolved(&self, object: &Utf8Path) -> Result<Vec<String>, ResolverUnavailable>;
}

/// Resolver that runs `ldd`, which reports every dependency the loader sees.
#[derive(Debug, Clone)]
pub(crate) struct LddResolver {
    program: String,
}

impl LddResolver {
    /// Uses the `ldd` found on `PATH`.
    pub(crate) fn new() -> Self { Self::with_program("ldd") }

    /// Uses another program, so a test can model a host without `ldd`.
    pub(crate) fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl SharedLibraryResolver for LddResolver {
    fn unresolved(&self, object: &Utf8Path) -> Result<Vec<String>, ResolverUnavailable> {
        let output = Command::new(&self.program)
            .arg(object.as_str())
            .output()
            .map_err(|err| ResolverUnavailable {
                reason: format!("could not run `{}`: {err}", self.program),
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if output.status.success() || is_static_object(&stdout, &stderr) {
            return Ok(parse_missing_sonames(&stdout));
        }
        Err(ResolverUnavailable {
            reason: format!(
                "`{} {object}` exited with {}: {}",
                self.program,
                output.status,
                stderr.trim()
            ),
        })
    }
}

/// Whether `ldd` said the object has no dynamic dependencies at all.
fn is_static_object(stdout: &str, stderr: &str) -> bool {
    const MARKER: &str = "not a dynamic executable";
    stdout.contains(MARKER) || stderr.contains(MARKER)
}

/// Extracts the SONAMEs from `lib.so.N => not found` lines, sorted and unique.
pub(crate) fn parse_missing_sonames(ldd_output: &str) -> Vec<String> {
    let mut missing: Vec<String> = ldd_output
        .lines()
        .filter_map(|line| line.trim().strip_suffix("=> not found"))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

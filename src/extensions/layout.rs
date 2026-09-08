//! Archive layout policy: which paths a prebuilt extension archive may carry.
//!
//! This is the pure half of installation. It decides nothing about the
//! filesystem and touches nothing; `install` applies these decisions.

use std::path::{Component, Path};

use camino::Utf8PathBuf;

/// Prefixes a file may live under, relative to the install root.
pub const ALLOWED_PREFIXES: [&str; 2] = ["lib/", "share/extension/"];

/// Returns the canonical relative path when `raw` is a regular file path the
/// hook accepts, or `None` otherwise.
///
/// Accepted paths are relative, contain only normal components (a leading
/// `./` is tolerated), and lie directly under `lib/` or anywhere under
/// `share/extension/`.
///
/// # Examples
///
/// ```
/// use std::path::Path;
///
/// use pg_embedded_setup_unpriv::extensions::classify_entry_path;
///
/// assert_eq!(
///     classify_entry_path(Path::new("./lib/vector.so"))
///         .as_deref()
///         .map(|p| p.as_str()),
///     Some("lib/vector.so")
/// );
/// assert!(classify_entry_path(Path::new("lib/../bin/psql")).is_none());
/// assert!(classify_entry_path(Path::new("lib/bitcode/vector.bc")).is_none());
/// ```
#[must_use]
pub fn classify_entry_path(raw: &Path) -> Option<Utf8PathBuf> {
    let parts = normal_components(raw)?;
    is_allowed_layout(&parts).then(|| Utf8PathBuf::from(parts.join("/")))
}

/// Splits `raw` into plain UTF-8 components, tolerating only a leading `./`.
fn normal_components(raw: &Path) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    for (index, component) in raw.components().enumerate() {
        match component {
            Component::CurDir if index == 0 => {}
            Component::Normal(part) => parts.push(plain_component(part.to_str()?)?),
            _ => return None,
        }
    }
    Some(parts)
}

/// Rejects components that smuggle separators on platforms that allow them.
fn plain_component(part: &str) -> Option<&str> {
    (!part.contains('\\') && !part.contains('/')).then_some(part)
}

/// Accepts `lib/<file>` and `share/extension/<path...>` only.
fn is_allowed_layout(parts: &[&str]) -> bool {
    matches!(parts, ["lib", _] | ["share", "extension", _, ..])
}

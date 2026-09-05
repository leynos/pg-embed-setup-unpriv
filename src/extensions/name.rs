//! Validated `CREATE EXTENSION` names.

use std::fmt;

use color_eyre::eyre::eyre;

use super::extension_error;
use crate::error::{BootstrapErrorKind, BootstrapResult};

/// A `CREATE EXTENSION` name: one or more of `a-z`, `0-9` and `_`.
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::extensions::ExtensionName;
///
/// let name = ExtensionName::new("vector")?;
/// assert_eq!(name.as_str(), "vector");
/// assert!(ExtensionName::new("Vector").is_err());
/// assert!(ExtensionName::new("vec tor").is_err());
/// # Ok::<(), pg_embedded_setup_unpriv::BootstrapError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionName(String);

impl ExtensionName {
    /// Validates and wraps a name.
    ///
    /// # Errors
    ///
    /// Returns `ExtensionConfigInvalid` when the name is empty or contains a
    /// character outside `[a-z0-9_]`.
    pub fn new(name: impl Into<String>) -> BootstrapResult<Self> {
        let value = name.into();
        if value.is_empty() || !value.bytes().all(is_name_byte) {
            return Err(extension_error(
                BootstrapErrorKind::ExtensionConfigInvalid,
                eyre!(
                    "extension name {value:?} is invalid; names use lower-case letters, digits \
                     and underscores"
                ),
            ));
        }
        Ok(Self(value))
    }

    /// Returns the name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

const fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'
}

impl AsRef<str> for ExtensionName {
    fn as_ref(&self) -> &str { &self.0 }
}

impl TryFrom<&str> for ExtensionName {
    type Error = crate::error::BootstrapError;

    fn try_from(value: &str) -> Result<Self, Self::Error> { Self::new(value) }
}

impl fmt::Display for ExtensionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

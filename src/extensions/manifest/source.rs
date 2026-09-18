//! Where the manifest comes from and how its bytes are pinned.
//!
//! Its own module because the enum and its rendering are the one place the
//! redaction rule is stated, and it belongs beside the reader that consumes
//! it rather than in the feature's root.

use camino::Utf8PathBuf;

use crate::extensions::{Sha256Hex, http};

/// Where the manifest comes from and how it is verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    /// Fetched from a permitted URL: `https://` anywhere, or `http://` to a
    /// loopback address. The digest is mandatory either way, because it is
    /// what makes the fetched bytes checkable and loopback does not supply
    /// that. See [`crate::extensions::is_permitted_url`], which the fetch and the redirect
    /// policy both consult.
    Url {
        /// The URL of `manifest.json`.
        url: String,
        /// Expected SHA-256 of the manifest bytes.
        sha256: Sha256Hex,
    },
    /// Read from the filesystem; a local manifest is trusted like local source,
    /// so the digest is optional.
    Path {
        /// Path of `manifest.json`.
        path: Utf8PathBuf,
        /// Expected SHA-256 of the manifest bytes, when pinned.
        sha256: Option<Sha256Hex>,
    },
}

impl ManifestSource {
    /// Renders the location for error messages and logs.
    ///
    /// A URL is redacted, because a consumer's manifest URL can carry
    /// userinfo, a signed query parameter or a token in its fragment, and this
    /// string reaches log fields and error messages. A path is rendered as
    /// written.
    ///
    /// # Examples
    ///
    /// ```
    /// use camino::Utf8PathBuf;
    /// use pg_embedded_setup_unpriv::extensions::ManifestSource;
    ///
    /// let path = ManifestSource::Path {
    ///     path: Utf8PathBuf::from("/srv/extensions/manifest.json"),
    ///     sha256: None,
    /// };
    /// assert_eq!(path.location(), "/srv/extensions/manifest.json");
    /// ```
    #[must_use]
    pub fn location(&self) -> String {
        match self {
            // Redacted: this is used in log fields and error messages, and a
            // consumer's URL can carry userinfo or a signed query parameter.
            Self::Url { url, .. } => http::redact_url(url),
            Self::Path { path, .. } => path.to_string(),
        }
    }
}

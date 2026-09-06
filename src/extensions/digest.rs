//! SHA-256 digests as lower-case hex, validated at the boundary.

use std::{fmt, io};

use sha2::{Digest, Sha256};

/// A SHA-256 digest rendered as 64 lower-case hex characters.
///
/// # Examples
///
/// ```
/// use pg_embedded_setup_unpriv::extensions::Sha256Hex;
///
/// let digest = Sha256Hex::of_bytes(b"hello");
/// assert_eq!(
///     digest.as_str(),
///     "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
/// );
/// assert!(Sha256Hex::parse("2CF24DBA").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Sha256Hex(String);

/// Why a digest string was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidDigest {
    /// The offending value, truncated for messages.
    pub value: String,
}

impl fmt::Display for InvalidDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "digest {:?} must be 64 lower-case hex characters",
            self.value
        )
    }
}

impl std::error::Error for InvalidDigest {}

impl Sha256Hex {
    /// Parses a digest, requiring exactly 64 lower-case hex characters.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidDigest`] for any other input, including upper-case
    /// hex, so a pin is always written in one canonical form.
    pub fn parse(value: &str) -> Result<Self, InvalidDigest> {
        let is_valid = value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if is_valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidDigest {
                value: value.chars().take(80).collect(),
            })
        }
    }

    /// Computes the digest of an in-memory byte slice.
    #[must_use]
    pub fn of_bytes(bytes: &[u8]) -> Self { Self::from_hasher(Sha256::new_with_prefix(bytes)) }

    /// Computes the digest of everything read from `reader`.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when reading fails.
    pub fn of_reader<R: io::Read>(mut reader: R) -> io::Result<Self> {
        let mut hasher = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(buffer.get(..count).unwrap_or_default());
        }
        Ok(Self::from_hasher(hasher))
    }

    /// Computes the digest of the file at `path`.
    ///
    /// # Errors
    ///
    /// Returns the underlying I/O error when the file cannot be read.
    pub fn of_file(path: &camino::Utf8Path) -> io::Result<Self> {
        Self::of_reader(std::fs::File::open(path)?)
    }

    fn from_hasher(hasher: Sha256) -> Self {
        let bytes = hasher.finalize();
        let mut encoded = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            encoded.push(lower_hex_digit(byte >> 4));
            encoded.push(lower_hex_digit(byte & 0x0f));
        }
        Self(encoded)
    }

    /// Returns the hex digest.
    #[must_use]
    pub fn as_str(&self) -> &str { &self.0 }
}

impl AsRef<str> for Sha256Hex {
    fn as_ref(&self) -> &str { &self.0 }
}

impl fmt::Display for Sha256Hex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(&self.0) }
}

/// Renders one nibble as a lower-case hex digit.
const fn lower_hex_digit(nibble: u8) -> char {
    if nibble < 10 {
        (b'0' + nibble) as char
    } else {
        (b'a' + nibble - 10) as char
    }
}

/// Writer adaptor that hashes everything written through it.
pub(super) struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W: io::Write> HashingWriter<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            written: 0,
        }
    }

    /// Finishes hashing and returns the inner writer, the digest and the byte count.
    pub(super) fn finish(self) -> (W, Sha256Hex, u64) {
        (
            self.inner,
            Sha256Hex::from_hasher(self.hasher),
            self.written,
        )
    }
}

impl<W: io::Write> io::Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let count = self.inner.write(buf)?;
        self.hasher.update(buf.get(..count).unwrap_or_default());
        self.written += count as u64;
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
}

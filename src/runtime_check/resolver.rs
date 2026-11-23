//! The port through which the preflight asks the host about shared libraries.
//!
//! Production code uses [`LddResolver`], which runs the platform's `ldd`.
//! Tests supply their own implementation, so no real host is needed.

use std::{
    io::{Read, Seek, SeekFrom},
    process::Command,
};

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
        let is_static_elf = is_static_object(&stdout, &stderr) && is_elf(object);
        if output.status.success() || is_static_elf {
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

/// Whether the file is a well-formed ELF with no interpreter, so no dynamic
/// dependencies.
///
/// `ldd` says "not a dynamic executable" for any file it cannot load,
/// including an empty, truncated or non-ELF one, so only a well-formed ELF
/// without a `PT_INTERP` program header may take the static fallback.
fn is_elf(object: &Utf8Path) -> bool {
    std::fs::File::open(object).is_ok_and(|mut file| is_static_elf(&mut file))
}

/// The `PT_INTERP` program header type.
const PT_INTERP: u32 = 3;

/// Parses the ELF and program headers from `reader`; `true` only when they
/// are well-formed and none is `PT_INTERP`. Any read or parse failure is `false`.
fn is_static_elf<R: Read + Seek>(reader: &mut R) -> bool {
    parse_program_header_types(reader).is_some_and(|types| !types.contains(&PT_INTERP))
}

/// Returns the `p_type` of every program header, or `None` if malformed.
fn parse_program_header_types<R: Read + Seek>(reader: &mut R) -> Option<Vec<u32>> {
    let mut ident = [0_u8; 16];
    reader.read_exact(&mut ident).ok()?;
    let [0x7f, b'E', b'L', b'F', class, data, ..] = ident else {
        return None;
    };
    let is_64 = match class {
        1 => false,
        2 => true,
        _ => return None,
    };
    let is_little = match data {
        1 => true,
        2 => false,
        _ => return None,
    };
    let field =
        |source: &mut R, offset: u64, length: usize| read_uint(source, offset, length, is_little);
    // e_phoff, e_phentsize and e_phnum sit at fixed offsets per class.
    let (phoff_at, phoff_len, phentsize_at) = if is_64 { (32, 8, 54) } else { (28, 4, 42) };
    let table_at = field(reader, phoff_at, phoff_len)?;
    let entry_size = field(reader, phentsize_at, 2)?;
    let entry_count = field(reader, phentsize_at + 2, 2)?;
    let minimum_entry = if is_64 { 56 } else { 32 };
    if entry_count == 0 || entry_size < minimum_entry {
        return None;
    }
    (0..entry_count)
        .map(|index| {
            let p_type = field(reader, table_at + index * entry_size, 4)?;
            u32::try_from(p_type).ok()
        })
        .collect()
}

/// Reads an unsigned integer of `length` bytes at `offset`.
fn read_uint<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    length: usize,
    is_little: bool,
) -> Option<u64> {
    reader.seek(SeekFrom::Start(offset)).ok()?;
    let mut bytes = vec![0_u8; length];
    reader.read_exact(&mut bytes).ok()?;
    if is_little {
        bytes.reverse();
    }
    Some(
        bytes
            .into_iter()
            .fold(0_u64, |acc, byte| (acc << 8) | u64::from(byte)),
    )
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

#[cfg(test)]
mod elf_tests {
    //! The static-ELF decision, on hand-built headers.

    use std::io::Cursor;

    use super::is_static_elf;

    /// Overwrites `bytes` at `offset`, growing the buffer with zeros as needed.
    fn put(bytes: &mut Vec<u8>, offset: usize, data: &[u8]) {
        if bytes.len() < offset + data.len() {
            bytes.resize(offset + data.len(), 0);
        }
        bytes.splice(offset..offset + data.len(), data.iter().copied());
    }

    /// A little-endian 64-bit ELF header followed by program headers of the
    /// given types, each 56 bytes as the format requires.
    fn elf64(types: &[u32]) -> Vec<u8> {
        let mut bytes = Vec::new();
        put(&mut bytes, 0, b"\x7fELF\x02\x01");
        put(&mut bytes, 32, &[64, 0, 0, 0, 0, 0, 0, 0]);
        put(&mut bytes, 54, &[56, 0]);
        let count = u8::try_from(types.len()).expect("few headers");
        put(&mut bytes, 56, &[count, 0]);
        put(&mut bytes, 64, &[]);
        for (index, kind) in types.iter().enumerate() {
            put(
                &mut bytes,
                64 + index * 56,
                &[u8::try_from(*kind).expect("small"), 0, 0, 0],
            );
            put(&mut bytes, 64 + index * 56 + 52, &[0; 4]);
        }
        bytes
    }

    #[test]
    fn a_well_formed_elf_without_an_interpreter_is_static() {
        assert!(is_static_elf(&mut Cursor::new(elf64(&[1, 4]))));
    }

    #[test]
    fn an_elf_with_an_interpreter_is_dynamic() {
        assert!(!is_static_elf(&mut Cursor::new(elf64(&[3, 1]))));
    }

    #[test]
    fn a_truncated_elf_is_not_static() {
        let whole = elf64(&[1, 4]);
        for length in [4, 20, 40, 64, 100] {
            let cut: Vec<u8> = whole.iter().copied().take(length).collect();
            assert!(!is_static_elf(&mut Cursor::new(cut)), "length {length}");
        }
    }

    #[test]
    fn non_elf_bytes_are_not_static() {
        assert!(!is_static_elf(&mut Cursor::new(b"plain text".to_vec())));
        assert!(!is_static_elf(&mut Cursor::new(Vec::new())));
    }
}

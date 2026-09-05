//! Property tests for the streaming digest writer.

use std::io::Write;

use proptest::prelude::*;

use crate::extensions::{Sha256Hex, digest::HashingWriter};

/// An inner writer that accepts at most `limit` bytes per call, so the
/// adaptor must hash exactly what was accepted, not what was offered.
struct Trickle {
    limit: usize,
    buffer: Vec<u8>,
}

impl Write for Trickle {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let count = buf.len().min(self.limit).max(usize::from(!buf.is_empty()));
        self.buffer
            .extend_from_slice(buf.get(..count).unwrap_or_default());
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

proptest! {
    /// Whatever the chunking and however the inner writer trickles, the digest
    /// and byte count describe exactly the bytes the inner writer accepted.
    #[test]
    fn hashing_writer_tracks_accepted_bytes(
        data in proptest::collection::vec(any::<u8>(), 0..2048),
        chunk in 1_usize..64,
        limit in 1_usize..17,
    ) {
        let mut writer = HashingWriter::new(Trickle { limit, buffer: Vec::new() });
        for piece in data.chunks(chunk) {
            writer.write_all(piece).expect("write_all retries partial writes");
        }
        let (inner, digest, written) = writer.finish();
        prop_assert_eq!(&inner.buffer, &data);
        prop_assert_eq!(written, data.len() as u64);
        prop_assert_eq!(digest, Sha256Hex::of_bytes(&data));
    }
}

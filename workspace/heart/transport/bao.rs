//! Bao (bao-tree) outboards for verified range streaming.

use bao_tree::io::outboard::PreOrderMemOutboard;
use bao_tree::io::sync::{decode_ranges, encode_ranges_validated};
use bao_tree::{BlockSize, ChunkNum, ChunkRanges};
use bytes::Bytes;
use thiserror::Error;

/// The Bao block size used for every outboard.
///
/// [`BlockSize::ZERO`] means one chunk group == one 1 KiB BLAKE3 chunk (no
/// grouping) — the iroh-blobs default, so an outboard generated here is
/// directly interoperable with an iroh-blobs verified stream. It is a **format
/// constant**: changing it changes every outboard's bytes.
pub const BAO_BLOCK_SIZE: BlockSize = BlockSize::ZERO;

/// The number of uncompressed bytes covered by one BLAKE3 chunk at
/// [`BAO_BLOCK_SIZE`] (1024). Used to translate a byte range into a chunk range.
pub const BAO_CHUNK_BYTES: u64 = 1024;

/// A bao encode/decode/range failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum Error {
    /// The requested `[start, end)` range was invalid against the blob length.
    #[error("range out of bounds")]
    RangeOutOfBounds { start: u64, end: u64, length: u64 },

    /// The outboard could not encode the range (corruption between blob and
    /// outboard).
    #[error("bao encode failed")]
    Encode(#[source] ErrorText),

    /// The encoded slice failed verification against the root (a tampered byte).
    #[error("bao decode/verify failed")]
    Decode(#[source] ErrorText),
}

/// Back-compat alias so cross-crate callers keep `bao::BaoError` resolvable.
pub use self::Error as BaoError;

/// Wraps a `std::io::Error`-like value that is both `Clone + Eq` — bao_tree
/// returns an error type that supports `Display` but not `Error`. We preserve
/// its string rendering while keeping it as a typed source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorText(String);

impl ErrorText {
    fn new(e: impl std::fmt::Display) -> Self {
        Self(e.to_string())
    }
}

impl std::fmt::Display for ErrorText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ErrorText {}

/// A generated outboard: the root hash, the blob length, and the pre-order
/// interior-hash bytes (without the 8-byte length prefix — the length is
/// carried separately in `length`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outboard {
    /// BLAKE3 root hash of the blob (equals `blake3::hash(bytes)`).
    pub root: [u8; 32],
    /// Total length of the blob, in bytes (the Bao tree size).
    pub length: u64,
    /// Pre-order outboard bytes (interior BLAKE3 pair hashes).
    pub bytes: Vec<u8>,
}

/// Generate an outboard over the whole blob `bytes`.
///
/// The root hash equals `blake3::hash(bytes)`, so it can double as the blob's
/// content-address anchor.
pub fn generate(bytes: &[u8]) -> Outboard {
    let outboard = PreOrderMemOutboard::create(bytes, BAO_BLOCK_SIZE);
    Outboard {
        root: *outboard.root.as_bytes(),
        length: bytes.len() as u64,
        bytes: outboard.data,
    }
}

/// Produce a verified Bao encoding of the byte range `[start, end)` of a blob.
///
/// `blob` must be the same whole bytes the `outboard` was generated over. The
/// returned bytes are a self-verifying Bao slice; feed them (with the same
/// range) to [`verify_range`] on the receiver.
pub fn encode_range(
    outboard: &Outboard,
    blob: &[u8],
    start: u64,
    end: u64,
) -> Result<Bytes, Error> {
    if start > end || end > outboard.length {
        return Err(Error::RangeOutOfBounds {
            start,
            end,
            length: outboard.length,
        });
    }
    let ranges = byte_range_to_chunk_ranges(start, end);
    let pre_order = PreOrderMemOutboard {
        root: blake3::Hash::from_bytes(outboard.root),
        tree: bao_tree::BaoTree::new(outboard.length, BAO_BLOCK_SIZE),
        data: outboard.bytes.as_slice(),
    };
    let mut encoded: Vec<u8> = Vec::new();
    encode_ranges_validated(blob, pre_order, &ranges, &mut encoded)
        .map_err(|error| Error::Encode(ErrorText::new(error)))?;
    Ok(Bytes::from(encoded))
}

/// Verify a Bao-encoded range on the receiver and return the *exact* requested
/// bytes `[start, end)`.
///
/// Decoding fails with [`Error::Decode`] if any leaf or interior hash does
/// not match `root` — a tampered byte is rejected.
pub fn verify_range(
    root: &[u8; 32],
    length: u64,
    encoded: &[u8],
    start: u64,
    end: u64,
) -> Result<Bytes, Error> {
    if start > end || end > length {
        return Err(Error::RangeOutOfBounds { start, end, length });
    }
    if start == end {
        return Ok(Bytes::new());
    }
    let ranges = byte_range_to_chunk_ranges(start, end);
    let tree = bao_tree::BaoTree::new(length, BAO_BLOCK_SIZE);
    let root = blake3::Hash::from_bytes(*root);

    let mut target: Vec<u8> = vec![0u8; length as usize];
    let empty_outboard = bao_tree::io::outboard::EmptyOutboard { tree, root };
    decode_ranges(encoded, &ranges, &mut target[..], empty_outboard)
        .map_err(|error| Error::Decode(ErrorText::new(error)))?;

    Ok(Bytes::copy_from_slice(
        &target[start as usize..end as usize],
    ))
}

/// Translate a byte range `[start, end)` into the BLAKE3 chunk range that fully
/// covers it (chunks are 1 KiB at [`BAO_BLOCK_SIZE`]). Verified streaming
/// operates on whole chunks; the receiver slices out the exact bytes.
fn byte_range_to_chunk_ranges(start: u64, end: u64) -> ChunkRanges {
    if start >= end {
        return ChunkRanges::empty();
    }
    let first_chunk = start / BAO_CHUNK_BYTES;
    let last_chunk = end.div_ceil(BAO_CHUNK_BYTES);
    ChunkRanges::from(ChunkNum(first_chunk)..ChunkNum(last_chunk))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big() -> Vec<u8> {
        (0u8..=255).cycle().take(1024 * 1024 + 777).collect()
    }

    #[test]
    fn root_equals_blake3() {
        let data = big();
        let outboard = generate(&data);
        assert_eq!(outboard.root, *blake3::hash(&data).as_bytes());
    }

    #[test]
    fn verified_slice_roundtrips() {
        let data = big();
        let outboard = generate(&data);
        let (start, end) = (500_000u64, 500_512u64);
        let encoded = encode_range(&outboard, &data, start, end).unwrap();
        let got = verify_range(&outboard.root, outboard.length, &encoded, start, end).unwrap();
        assert_eq!(got.as_ref(), &data[start as usize..end as usize]);
    }

    #[test]
    fn tampered_byte_fails() {
        let data = big();
        let outboard = generate(&data);
        let (start, end) = (300_000u64, 300_100u64);
        let mut encoded = encode_range(&outboard, &data, start, end).unwrap().to_vec();
        let victim = encoded.len() / 2;
        encoded[victim] ^= 0xFF;
        let result = verify_range(&outboard.root, outboard.length, &encoded, start, end);
        assert!(matches!(result, Err(Error::Decode(_))), "got {result:?}");
    }

    #[test]
    fn out_of_bounds_is_typed() {
        let data = big();
        let outboard = generate(&data);
        let err = encode_range(&outboard, &data, 0, outboard.length + 1).unwrap_err();
        assert!(matches!(err, Error::RangeOutOfBounds { .. }));
    }
}

//! Bao outboards for verified range streaming of large pack members
//! (INDEX-PLAN §6.2).
//!
//! # Why an outboard, and why over the *uncompressed* bytes
//!
//! A Bao (bao-tree) outboard is a BLAKE3 Merkle tree over a blob laid out so a
//! peer can request an arbitrary byte range and receive, alongside the range
//! bytes, exactly the interior hashes needed to verify that slice against the
//! blob's single root hash — without holding the whole blob. This is what makes
//! "fetch one snippet, prove it's genuine" possible over iroh.
//!
//! The outboard is generated over the member's **uncompressed** bytes, not its
//! zstd-framed on-disk form, because:
//!
//! 1. Verified *range* streaming addresses ranges of the logical content. A
//!    consumer asks for uncompressed bytes `[start, end)` (a source snippet);
//!    the Bao tree must be indexed the same way, or a range request could not be
//!    mapped to a verifiable subtree.
//! 2. The whole-member digest already recorded in the TOC
//!    ([`heart::object_pack::MemberRecord::content`]) is BLAKE3 of the
//!    uncompressed bytes. Generating the outboard over the same bytes makes the
//!    outboard's root hash equal that TOC digest, so one identity anchors both
//!    the pack-level integrity check and the streaming proof.
//! 3. zstd frame boundaries are a storage detail; they must not leak into the
//!    verification tree, or re-chunking would invalidate proofs.
//!
//! # Placement (see FORMAT.md)
//!
//! Outboards are **never** written inside the pack: pack bytes are frozen at
//! seal and their id commits only to the TOC + policy. An outboard is a
//! *sidecar* — a separate `.ndob` file the store owns beside the `.ndpk` — so
//! it can be (re)generated at any time from the pack without changing the pack
//! bytes or the [`ObjectPackId`].
//!
//! [`ObjectPackId`]: heart::object_pack::ObjectPackId

use bao_tree::io::outboard::PreOrderMemOutboard;
use bao_tree::io::sync::{decode_ranges, encode_ranges_validated};
use bao_tree::{BlockSize, ChunkNum, ChunkRanges};
use bytes::Bytes;
use heart::content::ContentHash;
use heart::object_pack::MemberKey;
use serde::{Deserialize, Serialize};

use crate::error::{PackError, PackResult};

/// The Bao block size used for every outboard we generate.
///
/// [`BlockSize::ZERO`] means one chunk group == one 1 KiB BLAKE3 chunk (no
/// grouping). This is the iroh-blobs default, so an outboard we generate is
/// directly interoperable with an iroh-blobs verified stream. It is a **format
/// constant** for the sidecar: changing it changes every outboard's bytes.
pub const BAO_BLOCK_SIZE: BlockSize = BlockSize::ZERO;

/// The number of uncompressed bytes covered by one BLAKE3 chunk at
/// [`BAO_BLOCK_SIZE`] (1024). Used to translate a byte range into a chunk range.
pub const BAO_CHUNK_BYTES: u64 = 1024;

/// The on-disk sidecar file extension (NDPK OutBoard).
pub const OUTBOARD_FILE_EXTENSION: &str = "ndob";

/// One member's pre-order Bao outboard plus the metadata needed to stream it.
///
/// The `root_hash` equals the member's TOC content digest (both are BLAKE3 over
/// the uncompressed bytes), which ties the streaming proof to the pack's own
/// integrity anchor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberOutboard {
    /// The member this outboard verifies.
    pub key: MemberKey,
    /// BLAKE3 root hash of the uncompressed member bytes. Equal to the member's
    /// TOC `content` digest.
    pub root_hash: ContentHash,
    /// Total uncompressed length of the member, in bytes (the Bao tree size).
    pub uncompressed_length: u64,
    /// Pre-order outboard bytes (interior BLAKE3 pair hashes), *without* the
    /// 8-byte little-endian length prefix — the length is carried separately in
    /// `uncompressed_length` so the sidecar stays self-describing.
    pub outboard_bytes: Vec<u8>,
}

impl MemberOutboard {
    /// Generate a member outboard from the member's full uncompressed bytes.
    ///
    /// The caller must pass the *whole* uncompressed member (all chunks
    /// concatenated in order), because the Bao tree spans the entire logical
    /// content.
    pub fn generate(key: MemberKey, uncompressed: &[u8]) -> Self {
        let outboard = PreOrderMemOutboard::create(uncompressed, BAO_BLOCK_SIZE);
        let root_hash = ContentHash::from_bytes(*outboard.root.as_bytes());
        MemberOutboard {
            key,
            root_hash,
            uncompressed_length: uncompressed.len() as u64,
            outboard_bytes: outboard.data,
        }
    }

    /// Reconstruct a [`PreOrderMemOutboard`] from the stored parts so it can
    /// drive [`encode_ranges_validated`].
    fn to_pre_order(&self) -> PreOrderMemOutboard<&[u8]> {
        PreOrderMemOutboard {
            root: blake3::Hash::from_bytes(*self.root_hash.as_bytes()),
            tree: bao_tree::BaoTree::new(self.uncompressed_length, BAO_BLOCK_SIZE),
            data: self.outboard_bytes.as_slice(),
        }
    }

    /// Produce a verified Bao encoding of the uncompressed byte range
    /// `[start, end)` of this member.
    ///
    /// The returned bytes are a self-verifying Bao slice: the root hash plus the
    /// interior hashes and leaf data needed to prove the requested range against
    /// [`Self::root_hash`]. Feed them (with the same range) to
    /// [`verify_bao_range`] on the receiver.
    ///
    /// `member_uncompressed` must be the same whole uncompressed member the
    /// outboard was generated over.
    ///
    /// # Errors
    ///
    /// - [`PackError::RangeOutOfBounds`] — `start > end` or `end` beyond the
    ///   member length.
    /// - [`PackError::BaoEncode`] — the outboard did not verify the data
    ///   (corruption between pack and outboard).
    pub fn encode_range(
        &self,
        member_uncompressed: &[u8],
        start: u64,
        end: u64,
    ) -> PackResult<Bytes> {
        if start > end || end > self.uncompressed_length {
            return Err(PackError::RangeOutOfBounds {
                start,
                end,
                member_length: self.uncompressed_length,
            });
        }

        let ranges = byte_range_to_chunk_ranges(start, end);
        let mut encoded: Vec<u8> = Vec::new();
        encode_ranges_validated(member_uncompressed, self.to_pre_order(), &ranges, &mut encoded)
            .map_err(|error| PackError::BaoEncode { detail: error.to_string() })?;
        Ok(Bytes::from(encoded))
    }
}

/// Translate an uncompressed byte range `[start, end)` into the BLAKE3 chunk
/// range that fully covers it (chunks are 1 KiB at [`BAO_BLOCK_SIZE`]).
///
/// Verified streaming operates on whole chunks, so we round the start down and
/// the end up to chunk boundaries; the receiver then slices out the exact bytes.
fn byte_range_to_chunk_ranges(start: u64, end: u64) -> ChunkRanges {
    if start >= end {
        return ChunkRanges::empty();
    }
    let first_chunk = start / BAO_CHUNK_BYTES;
    // Round the exclusive end up to the next chunk boundary.
    let last_chunk = end.div_ceil(BAO_CHUNK_BYTES);
    ChunkRanges::from(ChunkNum(first_chunk)..ChunkNum(last_chunk))
}

/// Verify a Bao-encoded range on the receiver and return the *exact* requested
/// uncompressed bytes `[start, end)`.
///
/// Decodes `encoded` against `root_hash` for `uncompressed_length`, which fails
/// with [`PackError::BaoDecode`] if any leaf or interior hash does not match the
/// root — i.e. a tampered byte is rejected. On success the covered chunk bytes
/// are reconstructed and sliced down to the requested sub-range.
///
/// # Errors
///
/// - [`PackError::RangeOutOfBounds`] — invalid range against the length.
/// - [`PackError::BaoDecode`] — the encoded slice failed verification.
pub fn verify_bao_range(
    root_hash: &ContentHash,
    uncompressed_length: u64,
    encoded: &[u8],
    start: u64,
    end: u64,
) -> PackResult<Bytes> {
    if start > end || end > uncompressed_length {
        return Err(PackError::RangeOutOfBounds {
            start,
            end,
            member_length: uncompressed_length,
        });
    }
    if start == end {
        return Ok(Bytes::new());
    }

    let ranges = byte_range_to_chunk_ranges(start, end);
    let tree = bao_tree::BaoTree::new(uncompressed_length, BAO_BLOCK_SIZE);
    let root = blake3::Hash::from_bytes(*root_hash.as_bytes());

    // We only need to *verify* (not persist) the outboard on the receiver, so we
    // decode into a scratch buffer sized to the whole blob with an EmptyOutboard
    // that discards interior hashes after checking them.
    let mut target: Vec<u8> = vec![0u8; uncompressed_length as usize];
    let empty_outboard = bao_tree::io::outboard::EmptyOutboard { tree, root };
    decode_ranges(encoded, &ranges, &mut target[..], empty_outboard)
        .map_err(|error| PackError::BaoDecode { detail: error.to_string() })?;

    // `target` now holds verified bytes in the covered chunk range; the bytes
    // outside the covered chunks are still zero, but the requested sub-range is
    // fully inside the covered chunks by construction.
    let requested_start = start as usize;
    let requested_end = end as usize;
    Ok(Bytes::copy_from_slice(&target[requested_start..requested_end]))
}

/// The complete set of outboards for one pack, serialized as a single sidecar
/// file (`<pack-hex>.ndob`) beside the `.ndpk` in the store.
///
/// A pack with no large members has an empty sidecar (or none written at all).
/// Lookups are by [`MemberKey`]; the vec is kept in canonical key order so the
/// sidecar bytes are deterministic for the same pack.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OutboardSidecar {
    /// Per-member outboards, sorted ascending by [`MemberKey`].
    pub members: Vec<MemberOutboard>,
}

impl OutboardSidecar {
    /// Build a sidecar from the outboards a seal produced, sorting into
    /// canonical key order for deterministic bytes.
    pub fn from_members(mut members: Vec<MemberOutboard>) -> Self {
        members.sort_by(|left, right| left.key.cmp(&right.key));
        Self { members }
    }

    /// Whether any member carries an outboard.
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Look up the outboard for a member key, if present.
    pub fn get(&self, key: &MemberKey) -> Option<&MemberOutboard> {
        self.members
            .binary_search_by(|candidate| candidate.key.cmp(key))
            .ok()
            .map(|index| &self.members[index])
    }

    /// Serialize to sidecar bytes (postcard).
    pub fn encode(&self) -> PackResult<Vec<u8>> {
        postcard::to_allocvec(self).map_err(|error| PackError::Codec {
            detail: format!("outboard sidecar encode: {error}"),
        })
    }

    /// Deserialize sidecar bytes (postcard).
    pub fn decode(bytes: &[u8]) -> PackResult<Self> {
        postcard::from_bytes(bytes).map_err(|error| PackError::Codec {
            detail: format!("outboard sidecar decode: {error}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smol_str::SmolStr;

    fn source_key(path: &str) -> MemberKey {
        MemberKey::Source { path: heart::object_pack::RelativePath(SmolStr::new(path)) }
    }

    fn big_member() -> Vec<u8> {
        // 1 MiB + a bit, deterministic pattern.
        (0u8..=255).cycle().take(1024 * 1024 + 777).collect()
    }

    #[test]
    fn root_hash_equals_content_digest() {
        let data = big_member();
        let outboard = MemberOutboard::generate(source_key("big.bin"), &data);
        assert_eq!(outboard.root_hash, ContentHash::of_bytes(&data));
    }

    #[test]
    fn verified_middle_slice_roundtrips() {
        let data = big_member();
        let outboard = MemberOutboard::generate(source_key("big.bin"), &data);

        let start = 500_000u64;
        let end = 500_512u64;
        let encoded = outboard.encode_range(&data, start, end).unwrap();
        let got = verify_bao_range(
            &outboard.root_hash,
            outboard.uncompressed_length,
            &encoded,
            start,
            end,
        )
        .unwrap();
        assert_eq!(got.as_ref(), &data[start as usize..end as usize]);
    }

    #[test]
    fn tampered_encoded_byte_fails_verification() {
        let data = big_member();
        let outboard = MemberOutboard::generate(source_key("big.bin"), &data);

        let start = 300_000u64;
        let end = 300_100u64;
        let mut encoded = outboard.encode_range(&data, start, end).unwrap().to_vec();

        // Flip a byte deep in the encoded slice (past the leading interior
        // hashes) so it lands in verified leaf data.
        let victim = encoded.len() / 2;
        encoded[victim] ^= 0xFF;

        let result = verify_bao_range(
            &outboard.root_hash,
            outboard.uncompressed_length,
            &encoded,
            start,
            end,
        );
        assert!(
            matches!(result, Err(PackError::BaoDecode { .. })),
            "expected BaoDecode, got {result:?}"
        );
    }

    #[test]
    fn out_of_bounds_range_is_typed() {
        let data = big_member();
        let outboard = MemberOutboard::generate(source_key("big.bin"), &data);
        let err = outboard
            .encode_range(&data, 0, outboard.uncompressed_length + 1)
            .unwrap_err();
        assert!(matches!(err, PackError::RangeOutOfBounds { .. }));
    }
}

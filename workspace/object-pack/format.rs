//! The on-disk NDPK v1 contract: the fixed header and the table of contents.
//!
//! This module is the single source of truth for how bytes are laid out and how
//! the [`ObjectPackId`] is derived. Builder and reader both go through it, so
//! the two can never disagree. Every routine here is total: adversarial input
//! yields a typed [`PackError`], never a panic.
//!
//! See `FORMAT.md` for the byte-level narrative.
//!
//! [`ObjectPackId`]: heart::object_pack::ObjectPackId

use heart::content::ContentHash;
use heart::object_pack::{MemberKey, MemberRecord, ObjectPackId};
use serde::{Deserialize, Serialize};

use crate::error::{PackError, PackResult};
use crate::{NDPK_MAGIC, NDPK_VERSION_CURRENT, NDPK_VERSION_MIN_READ};

/// The fixed size of the container header in bytes.
///
/// `magic(4) + version(2) + flags(2) + toc_offset(8) + toc_len(8) = 24`.
pub const HEADER_LENGTH_BYTES: usize = 24;

/// Header flag bits. The full mask of *known* bits; any bit outside this mask
/// set in a pack is a forward-incompatibility ([`PackError::UnknownFlags`]).
pub mod flags {
    /// No optional features. Every v1 pack this engine writes uses this.
    pub const NONE: u16 = 0;

    /// The mask of all flag bits this build understands.
    pub const KNOWN_MASK: u16 = NONE;
}

/// The parsed fixed header.
///
/// Field order and encoding are frozen: little-endian, packed, 24 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackHeader {
    /// Container format version (`1` for this engine's output).
    pub version: u16,
    /// Header flag bits (see [`flags`]).
    pub flags: u16,
    /// Byte offset of the TOC from the start of the file.
    pub toc_offset: u64,
    /// Length of the TOC in bytes.
    pub toc_length: u64,
}

impl PackHeader {
    /// Encode this header into its fixed 24-byte little-endian form.
    pub fn encode(&self) -> [u8; HEADER_LENGTH_BYTES] {
        let mut buffer = [0u8; HEADER_LENGTH_BYTES];
        buffer[0..4].copy_from_slice(&NDPK_MAGIC);
        buffer[4..6].copy_from_slice(&self.version.to_le_bytes());
        buffer[6..8].copy_from_slice(&self.flags.to_le_bytes());
        buffer[8..16].copy_from_slice(&self.toc_offset.to_le_bytes());
        buffer[16..24].copy_from_slice(&self.toc_length.to_le_bytes());
        buffer
    }

    /// Decode and validate a header from the start of `bytes`.
    ///
    /// Rejects short input, wrong magic, unreadable versions (per N/N-1 support,
    /// INDEX-PLAN ID-22), and unknown flag bits.
    pub fn decode(bytes: &[u8]) -> PackResult<PackHeader> {
        if bytes.len() < HEADER_LENGTH_BYTES {
            return Err(PackError::Truncated {
                offset: 0,
                needed: HEADER_LENGTH_BYTES as u64,
                available: bytes.len() as u64,
            });
        }

        let mut magic = [0u8; 4];
        magic.copy_from_slice(&bytes[0..4]);
        if magic != NDPK_MAGIC {
            return Err(PackError::BadMagic { found: magic });
        }

        let version = u16::from_le_bytes([bytes[4], bytes[5]]);
        if version < NDPK_VERSION_MIN_READ || version > NDPK_VERSION_CURRENT {
            return Err(PackError::UnsupportedVersion {
                found: version,
                min_read: NDPK_VERSION_MIN_READ,
                current: NDPK_VERSION_CURRENT,
            });
        }

        let flag_bits = u16::from_le_bytes([bytes[6], bytes[7]]);
        if flag_bits & !flags::KNOWN_MASK != 0 {
            return Err(PackError::UnknownFlags { bits: flag_bits });
        }

        let mut offset_bytes = [0u8; 8];
        offset_bytes.copy_from_slice(&bytes[8..16]);
        let toc_offset = u64::from_le_bytes(offset_bytes);

        let mut length_bytes = [0u8; 8];
        length_bytes.copy_from_slice(&bytes[16..24]);
        let toc_length = u64::from_le_bytes(length_bytes);

        Ok(PackHeader { version, flags: flag_bits, toc_offset, toc_length })
    }
}

/// The policy bytes mixed into every [`ObjectPackId`] alongside the TOC.
///
/// These are the format constants that, if changed, must change the id. They
/// are hashed in a fixed, length-prefixed order so a decoder and encoder agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackPolicy {
    /// The container version the pack was written at.
    pub version: u16,
    /// The zstd level every frame was compressed at.
    pub zstd_compression_level: i32,
    /// The uncompressed source-chunk size in bytes.
    pub source_chunk_size_bytes: u64,
}

impl PackPolicy {
    /// Fold the policy into a hasher in a fixed, length-agnostic order.
    fn feed(&self, hasher: &mut blake3::Hasher) {
        hasher.update(b"ndpk-policy-v1");
        hasher.update(&self.version.to_le_bytes());
        hasher.update(&self.zstd_compression_level.to_le_bytes());
        hasher.update(&self.source_chunk_size_bytes.to_le_bytes());
    }
}

/// The table of contents: the sorted member index plus format policy.
///
/// Serialized with postcard (deterministic, no map iteration, stable field
/// order). The [`records`](TableOfContents::records) vector is **required** to
/// be strictly ascending by [`MemberKey`]; this is enforced on both encode and
/// decode so a corrupt or adversarial pack cannot smuggle an unsorted TOC past
/// the reader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableOfContents {
    /// Format policy for id derivation and reader sanity.
    pub policy: PackPolicy,
    /// Member rows, strictly ascending by [`MemberRecord::key`].
    pub records: Vec<MemberRecord>,
}

impl TableOfContents {
    /// Encode the TOC to its canonical postcard bytes.
    ///
    /// Returns [`PackError::TocNotSorted`] if `records` is not strictly
    /// ascending — callers must sort before sealing.
    pub fn encode(&self) -> PackResult<Vec<u8>> {
        self.assert_sorted()?;
        postcard::to_allocvec(self)
            .map_err(|e| PackError::TocDecode(format!("encode: {e}")))
    }

    /// Decode a TOC from its postcard bytes and verify ordering.
    pub fn decode(bytes: &[u8]) -> PackResult<TableOfContents> {
        let toc: TableOfContents = postcard::from_bytes(bytes)
            .map_err(|e| PackError::TocDecode(format!("decode: {e}")))?;
        toc.assert_sorted()?;
        Ok(toc)
    }

    /// Verify strict ascending order (also rejects duplicate keys).
    fn assert_sorted(&self) -> PackResult<()> {
        for window in self.records.windows(2) {
            if window[0].key >= window[1].key {
                return Err(PackError::TocNotSorted);
            }
        }
        Ok(())
    }

    /// Binary-search for a member by key.
    pub fn find(&self, key: &MemberKey) -> Option<&MemberRecord> {
        self.records
            .binary_search_by(|record| record.key.cmp(key))
            .ok()
            .map(|index| &self.records[index])
    }
}

/// Derive the [`ObjectPackId`] from canonical TOC bytes and policy.
///
/// `id = BLAKE3("ndpk-id-v1" ‖ policy ‖ len(toc_bytes) ‖ toc_bytes)`. Because
/// the TOC bytes already commit (via each row's `content` digest and the frame
/// offsets) to every member's decompressed bytes, hashing the TOC transitively
/// commits to the whole pack. Same input tree ⇒ same TOC bytes ⇒ same id.
pub fn derive_object_pack_id(policy: &PackPolicy, canonical_toc_bytes: &[u8]) -> ObjectPackId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"ndpk-id-v1");
    policy.feed(&mut hasher);
    hasher.update(&(canonical_toc_bytes.len() as u64).to_le_bytes());
    hasher.update(canonical_toc_bytes);
    ObjectPackId(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

/// The BLAKE3 digest of the canonical TOC bytes, used to detect corruption
/// independently of the id (INDEX-PLAN §6.2 "blake3 of TOC").
pub fn toc_digest(canonical_toc_bytes: &[u8]) -> ContentHash {
    ContentHash::of_bytes(canonical_toc_bytes)
}

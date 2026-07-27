//! In-memory index builders and zero-copy readers used by [`crate::archive::seal`] and
//! [`crate::archive::view`].
//!
//! # IntroIndex
//!
//! A sorted array of `(IntroId[32], ArenaIdx u32)` pairs stored in the
//! [`crate::archive::header::SectionId::IntroIndex`] section. Binary-search on the
//! 32-byte key gives O(log n) lookup of any intro.
//!
//! # NameIndex
//!
//! Maps `StrId → ArenaIdx` postings. Wire layout:
//!   `u32le(n_postings) || Posting[n] { str_id: u32le, arena_idx: u32le }`
//!
//! Sorted by `str_id` for binary-search. A name with multiple postings (aliases)
//! will appear as consecutive identical `str_id` entries.
//!
//! # TypeSkeletonIndex
//!
//! Maps `TypeFingerprintId → ArenaIdx` postings. Wire layout identical to
//! NameIndex but keyed by fingerprint u32.
//!
//! # IntroPayloadHash
//!
//! Dense column: `ContentBlake3[32] × entry_count` (no header).

use ir::change::{ContentBlake3, IntroId};
use crate::vcs_types::{ArenaIdx, TypeFingerprintId};

use crate::archive::error::ArchiveError;

// ---------------------------------------------------------------------------
// IntroIndex builder
// ---------------------------------------------------------------------------

/// Builds the `IntroIndex` section: a sorted list of `(IntroId, ArenaIdx)` pairs.
pub struct IntroIndexBuilder {
    pairs: Vec<([u8; 32], u32)>,
}

impl IntroIndexBuilder {
    pub fn new() -> Self { Self { pairs: Vec::new() } }

    /// Add an `(IntroId, ArenaIdx)` mapping.
    pub fn push(&mut self, intro: IntroId, idx: ArenaIdx) {
        self.pairs.push((*intro.as_bytes(), idx.0));
    }

    /// Emit the sorted wire bytes:
    ///   `u32le(n) || (intro[32] || arena_idx u32le)[n]`
    pub fn finish(mut self) -> Vec<u8> {
        self.pairs.sort_unstable_by_key(|(k, _)| *k);
        let n = self.pairs.len() as u32;
        let mut out = Vec::with_capacity(4 + self.pairs.len() * 36);
        out.extend_from_slice(&n.to_le_bytes());
        for (intro, idx) in &self.pairs {
            out.extend_from_slice(intro);
            out.extend_from_slice(&idx.to_le_bytes());
        }
        out
    }
}

impl Default for IntroIndexBuilder { fn default() -> Self { Self::new() } }

// ---------------------------------------------------------------------------
// IntroIndex view
// ---------------------------------------------------------------------------

/// Zero-copy view of the `IntroIndex` section.
#[derive(Clone, Copy)]
pub struct IntroIndexView<'a> {
    bytes: &'a [u8],
    n: u32,
}

impl<'a> IntroIndexView<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        if bytes.len() < 4 { return Err(ArchiveError::Truncated); }
        let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let expected = 4 + n as usize * 36;
        if bytes.len() < expected { return Err(ArchiveError::Truncated); }
        Ok(Self { bytes, n })
    }

    /// Binary-search for `intro`, returning the [`ArenaIdx`] if found.
    pub fn lookup(&self, intro: IntroId) -> Option<ArenaIdx> {
        let key = intro.as_bytes();
        // Binary search over entries starting at offset 4.
        let mut lo = 0usize;
        let mut hi = self.n as usize;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = 4 + mid * 36;
            let entry_key = &self.bytes[off..off + 32];
            match entry_key.cmp(key.as_ref()) {
                std::cmp::Ordering::Equal => {
                    let idx = u32::from_le_bytes([
                        self.bytes[off + 32],
                        self.bytes[off + 33],
                        self.bytes[off + 34],
                        self.bytes[off + 35],
                    ]);
                    return Some(ArenaIdx(idx));
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// NameIndex / TypeSkeletonIndex (shared posting format)
// ---------------------------------------------------------------------------

// Posting entry: `key u32le || arena_idx u32le` (8 bytes each), sorted by key
// for binary-search range.

/// Builds a u32-keyed postings index.
pub struct PostingIndexBuilder {
    pairs: Vec<(u32, u32)>,
}

impl PostingIndexBuilder {
    pub fn new() -> Self { Self { pairs: Vec::new() } }

    pub fn push(&mut self, key: u32, arena_idx: ArenaIdx) {
        self.pairs.push((key, arena_idx.0));
    }

    /// Emit: `u32le(n) || (key u32le || arena_idx u32le)[n]`, sorted by key.
    pub fn finish(mut self) -> Vec<u8> {
        // Sort by (key, arena_idx) for determinism.
        self.pairs.sort_unstable();
        let n = self.pairs.len() as u32;
        let mut out = Vec::with_capacity(4 + self.pairs.len() * 8);
        out.extend_from_slice(&n.to_le_bytes());
        for (k, v) in &self.pairs {
            out.extend_from_slice(&k.to_le_bytes());
            out.extend_from_slice(&v.to_le_bytes());
        }
        out
    }
}

impl Default for PostingIndexBuilder { fn default() -> Self { Self::new() } }

// ---------------------------------------------------------------------------
// PostingIndexView
// ---------------------------------------------------------------------------

/// Zero-copy view of a u32-keyed postings index (NameIndex or TypeSkeletonIndex).
#[derive(Clone, Copy)]
pub struct PostingIndexView<'a> {
    bytes: &'a [u8],
    n: u32,
}

impl<'a> PostingIndexView<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        if bytes.len() < 4 { return Err(ArchiveError::Truncated); }
        let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let expected = 4 + n as usize * 8;
        if bytes.len() < expected { return Err(ArchiveError::Truncated); }
        Ok(Self { bytes, n })
    }

    // NOTE: postings are interleaved `[key u32le][arena_idx u32le]` pairs, so a
    // borrowed `&[ArenaIdx]` slice per key is impossible without a dense column
    // (that's what `DensePostingIndexView` is for). Lookups go through
    // `iter_key`.

    /// Return `(lo, hi)` indices into the posting array for `key`.
    pub fn range(&self, key: u32) -> (usize, usize) {
        let lo = self.lower_bound(key);
        let hi = self.lower_bound(key.saturating_add(1));
        (lo, hi)
    }

    /// Iterate over all `ArenaIdx` postings for `key`.
    pub fn iter_key(&self, key: u32) -> impl Iterator<Item = ArenaIdx> + 'a {
        let (lo, hi) = self.range(key);
        let bytes = self.bytes;
        (lo..hi).map(move |i| {
            let off = 4 + i * 8 + 4; // skip 4-byte header, then 4 bytes key per posting
            let v = u32::from_le_bytes([bytes[off], bytes[off+1], bytes[off+2], bytes[off+3]]);
            ArenaIdx(v)
        })
    }

    fn lower_bound(&self, key: u32) -> usize {
        let mut lo = 0usize;
        let mut hi = self.n as usize;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = 4 + mid * 8;
            let k = u32::from_le_bytes([
                self.bytes[off], self.bytes[off+1], self.bytes[off+2], self.bytes[off+3]
            ]);
            if k < key { lo = mid + 1; } else { hi = mid; }
        }
        lo
    }
}

// ---------------------------------------------------------------------------
// DensePostingIndex — for TypeSkeletonIndex returning &[ArenaIdx]
// ---------------------------------------------------------------------------
//
// This variant stores postings as:
//   u32le(n_groups) || Group[n_groups]
//   Group = u32le(fp) || u32le(count) || u32le(arena_idx)[count]
//
// This allows `by_type_fingerprint` to return a `&[ArenaIdx]` without
// allocating, by pointing directly into the packed values sub-slice.

/// Builds the DensePostingIndex (used for TypeSkeletonIndex).
pub struct DensePostingIndexBuilder {
    pairs: Vec<(u32, u32)>,
}

impl DensePostingIndexBuilder {
    pub fn new() -> Self { Self { pairs: Vec::new() } }

    pub fn push(&mut self, key: u32, arena_idx: ArenaIdx) {
        self.pairs.push((key, arena_idx.0));
    }

    pub fn finish(mut self) -> Vec<u8> {
        // Sort by (key, arena_idx) for determinism.
        self.pairs.sort_unstable();
        // Group by key.
        let mut out: Vec<u8> = Vec::new();
        // We'll compute groups inline.
        let mut groups: Vec<(u32, Vec<u32>)> = Vec::new();
        for (k, v) in self.pairs {
            if let Some(last) = groups.last_mut()
                && last.0 == k {
                    last.1.push(v);
                    continue;
                }
            groups.push((k, vec![v]));
        }
        let n_groups = groups.len() as u32;
        out.extend_from_slice(&n_groups.to_le_bytes());
        for (fp, vals) in &groups {
            out.extend_from_slice(&fp.to_le_bytes());
            out.extend_from_slice(&(vals.len() as u32).to_le_bytes());
            for v in vals {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }
}

impl Default for DensePostingIndexBuilder { fn default() -> Self { Self::new() } }

/// Zero-copy view of a DensePostingIndex.
#[derive(Clone, Copy)]
pub struct DensePostingIndexView<'a> {
    bytes: &'a [u8],
    n_groups: u32,
}

impl<'a> DensePostingIndexView<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        if bytes.len() < 4 { return Err(ArchiveError::Truncated); }
        let n_groups = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        Ok(Self { bytes, n_groups })
    }

    /// Return the slice of `ArenaIdx` postings for `fp`. O(n_groups) scan.
    ///
    /// For archives with up to a few thousand distinct type fingerprints this is
    /// acceptable; a sorted+binary-search variant can replace it if profiling
    /// shows it matters.
    /// Return the raw LE-encoded value bytes (length is a multiple of 4) for the
    /// posting list of `fp`, or an empty slice. Callers decode 4-byte words with
    /// `u32::from_le_bytes` — we return `&[u8]` rather than `&[ArenaIdx]` because
    /// the archive buffer has no 4-byte alignment guarantee (an aligned cast
    /// would be UB).
    pub fn lookup(&self, fp: TypeFingerprintId) -> &'a [u8] {
        let mut off = 4usize;
        for _ in 0..self.n_groups {
            if off + 8 > self.bytes.len() { return &[]; }
            let group_fp = u32::from_le_bytes([
                self.bytes[off], self.bytes[off+1], self.bytes[off+2], self.bytes[off+3]
            ]);
            let count = u32::from_le_bytes([
                self.bytes[off+4], self.bytes[off+5], self.bytes[off+6], self.bytes[off+7]
            ]) as usize;
            off += 8;
            if group_fp == fp.0 {
                if off + count * 4 > self.bytes.len() { return &[]; }
                return &self.bytes[off..off + count * 4];
            }
            off += count * 4;
        }
        &[]
    }
}

// ---------------------------------------------------------------------------
// IntroPayloadHash column
// ---------------------------------------------------------------------------

/// Builds the `IntroPayloadHash` section: a dense array of 32-byte hashes,
/// one per entry in ArenaIdx order.
pub struct PayloadHashColumnBuilder {
    hashes: Vec<[u8; 32]>,
}

impl PayloadHashColumnBuilder {
    pub fn new() -> Self { Self { hashes: Vec::new() } }
    pub fn push(&mut self, hash: ContentBlake3) { self.hashes.push(*hash.as_bytes()); }
    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.hashes.len() * 32);
        for h in &self.hashes {
            out.extend_from_slice(h);
        }
        out
    }
}

impl Default for PayloadHashColumnBuilder { fn default() -> Self { Self::new() } }

/// Zero-copy view of the `IntroPayloadHash` section.
#[derive(Clone, Copy)]
pub struct PayloadHashColumnView<'a> {
    bytes: &'a [u8],
}

impl<'a> PayloadHashColumnView<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        if !bytes.len().is_multiple_of(32) { return Err(ArchiveError::Truncated); }
        Ok(Self { bytes })
    }

    pub fn get(&self, idx: ArenaIdx) -> Option<ContentBlake3> {
        let off = idx.0 as usize * 32;
        if off + 32 > self.bytes.len() { return None; }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&self.bytes[off..off + 32]);
        Some(ContentBlake3::from_raw(arr))
    }
}

// ---------------------------------------------------------------------------
// Meta section (postcard-encoded)
// ---------------------------------------------------------------------------

use serde::{Deserialize, Serialize};

/// Archive metadata stored in the [`crate::archive::header::SectionId::Meta`] section
/// as a postcard-encoded struct.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArchiveMeta {
    /// Version of the `KindDiscriminant` wire table.
    pub kind_table_version: u16,
    /// Total live entry count.
    pub entry_count: u32,
    /// Total interned string count.
    pub string_count: u32,
    /// Total link record count.
    pub link_count: u32,
}

/// Postcard-encoded [`LinkCsrEntry`] list for the LinkCsr section.
///
/// Wire layout of the LinkCsr section:
///   `u32le(n_entries) || LinkCsrEntry[n] { arena_idx: u32le, other_idx: u32le,
///   kind_self: u16le, kind_other: u16le }`
///
/// Sorted by (arena_idx, other_idx, kind_self, kind_other) for determinism.
/// Each directed edge appears twice (once per endpoint).
#[derive(Clone, Copy, Debug)]
pub struct LinkCsrEntry {
    pub arena_idx: u32,
    pub other_idx: u32,
    pub kind_self: u16,
    pub kind_other: u16,
}

/// Builds the LinkCsr section.
pub struct LinkCsrBuilder {
    entries: Vec<LinkCsrEntry>,
}

impl LinkCsrBuilder {
    pub fn new() -> Self { Self { entries: Vec::new() } }

    /// Add a directed edge from `from` to `to` with kind discriminants.
    pub fn add(&mut self, from: ArenaIdx, to: ArenaIdx, kind_self: u16, kind_other: u16) {
        self.entries.push(LinkCsrEntry {
            arena_idx: from.0,
            other_idx: to.0,
            kind_self,
            kind_other,
        });
    }

    pub fn finish(mut self) -> Vec<u8> {
        // Sort for determinism: (arena_idx, other_idx, kind_self, kind_other).
        self.entries.sort_unstable_by_key(|e| {
            (e.arena_idx, e.other_idx, e.kind_self, e.kind_other)
        });
        let n = self.entries.len() as u32;
        // 4 (count) + 12 bytes per entry (4+4+2+2).
        let mut out = Vec::with_capacity(4 + self.entries.len() * 12);
        out.extend_from_slice(&n.to_le_bytes());
        for e in &self.entries {
            out.extend_from_slice(&e.arena_idx.to_le_bytes());
            out.extend_from_slice(&e.other_idx.to_le_bytes());
            out.extend_from_slice(&e.kind_self.to_le_bytes());
            out.extend_from_slice(&e.kind_other.to_le_bytes());
        }
        out
    }
}

impl Default for LinkCsrBuilder { fn default() -> Self { Self::new() } }

/// Zero-copy view of the LinkCsr section.
#[derive(Clone, Copy)]
pub struct LinkCsrView<'a> {
    bytes: &'a [u8],
    n: u32,
}

impl<'a> LinkCsrView<'a> {
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        if bytes.len() < 4 { return Err(ArchiveError::Truncated); }
        let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let expected = 4 + n as usize * 12;
        if bytes.len() < expected { return Err(ArchiveError::Truncated); }
        Ok(Self { bytes, n })
    }

    /// Return an iterator over all link entries where `arena_idx == from`.
    ///
    /// `use<'a>` (not `+ 'a`) makes the capture precise: the iterator borrows
    /// only the `'a` section bytes, NOT the `&self` view — so callers may drop
    /// the transient `LinkCsrView` while iterating (Rust 2024 capture rules).
    pub fn links_for(&self, from: ArenaIdx) -> impl Iterator<Item = LinkCsrEntry> + use<'a> {
        let bytes = self.bytes;
        let n = self.n as usize;
        // Linear scan; use binary search for large archives.
        let lo = self.lower_bound_for(from.0);
        (lo..n).map_while(move |i| {
            let off = 4 + i * 12;
            let arena = u32::from_le_bytes([bytes[off], bytes[off+1], bytes[off+2], bytes[off+3]]);
            if arena != from.0 { return None; }
            let other = u32::from_le_bytes([bytes[off+4], bytes[off+5], bytes[off+6], bytes[off+7]]);
            let ks = u16::from_le_bytes([bytes[off+8], bytes[off+9]]);
            let ko = u16::from_le_bytes([bytes[off+10], bytes[off+11]]);
            Some(LinkCsrEntry { arena_idx: arena, other_idx: other, kind_self: ks, kind_other: ko })
        })
    }

    fn lower_bound_for(&self, from: u32) -> usize {
        let mut lo = 0usize;
        let mut hi = self.n as usize;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let off = 4 + mid * 12;
            let k = u32::from_le_bytes([
                self.bytes[off], self.bytes[off+1], self.bytes[off+2], self.bytes[off+3]
            ]);
            if k < from { lo = mid + 1; } else { hi = mid; }
        }
        lo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intro(b: u8) -> IntroId {
        IntroId::from_raw([b; 32])
    }

    #[test]
    fn intro_index_binary_search() {
        let mut b = IntroIndexBuilder::new();
        b.push(intro(5), ArenaIdx(0));
        b.push(intro(1), ArenaIdx(1));
        b.push(intro(3), ArenaIdx(2));
        let bytes = b.finish();
        let view = IntroIndexView::from_bytes(&bytes).unwrap();
        assert_eq!(view.lookup(intro(5)), Some(ArenaIdx(0)));
        assert_eq!(view.lookup(intro(1)), Some(ArenaIdx(1)));
        assert_eq!(view.lookup(intro(3)), Some(ArenaIdx(2)));
        assert_eq!(view.lookup(intro(7)), None);
    }

    #[test]
    fn posting_index_lookup() {
        let mut b = PostingIndexBuilder::new();
        b.push(42, ArenaIdx(10));
        b.push(42, ArenaIdx(20));
        b.push(99, ArenaIdx(30));
        let bytes = b.finish();
        let view = PostingIndexView::from_bytes(&bytes).unwrap();
        let hits: Vec<_> = view.iter_key(42).collect();
        assert_eq!(hits, vec![ArenaIdx(10), ArenaIdx(20)]);
        let hits2: Vec<_> = view.iter_key(99).collect();
        assert_eq!(hits2, vec![ArenaIdx(30)]);
        let hits3: Vec<_> = view.iter_key(0).collect();
        assert!(hits3.is_empty());
    }

    #[test]
    fn dense_posting_index_roundtrip() {
        let mut b = DensePostingIndexBuilder::new();
        b.push(7, ArenaIdx(1));
        b.push(7, ArenaIdx(2));
        b.push(9, ArenaIdx(3));
        let bytes = b.finish();
        let view = DensePostingIndexView::from_bytes(&bytes).unwrap();
        let decode = |fp: u32| -> Vec<u32> {
            view.lookup(TypeFingerprintId(fp))
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        };
        assert_eq!(decode(7), vec![1u32, 2u32]);
        assert_eq!(decode(9), vec![3u32]);
        assert_eq!(decode(0), Vec::<u32>::new());
    }

    #[test]
    fn link_csr_roundtrip() {
        let mut b = LinkCsrBuilder::new();
        b.add(ArenaIdx(0), ArenaIdx(1), 1, 2);
        b.add(ArenaIdx(1), ArenaIdx(0), 2, 1);
        b.add(ArenaIdx(0), ArenaIdx(2), 1, 3);
        let bytes = b.finish();
        let view = LinkCsrView::from_bytes(&bytes).unwrap();
        let links: Vec<_> = view.links_for(ArenaIdx(0)).collect();
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].other_idx, 1);
        assert_eq!(links[1].other_idx, 2);
    }
}

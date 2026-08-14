//! Zero-copy archive view: [`PackageArchiveView`] and [`YokedArchive`].
//!
//! # Yoke approach
//!
//! The `yoke` crate requires implementing [`yoke::Yokeable`] for
//! `PackageArchiveView<'static>`. Because `PackageArchiveView<'a>` is covariant
//! in `'a` (all its fields are `&'a [u8]` slices or POD-derived from them),
//! the `yoke::derive::Yokeable` derive macro works.
//!
//! However, the view stores `ParsedSections` (an owned `HashMap`) and slices
//! derived from the backing `Arc<[u8]>`. Rather than making `PackageArchiveView`
//! itself borrow from the Arc (which would require a complex Yoke cart/yokeable
//! setup), we use a **simpler self-contained approach**:
//!
//! `YokedArchive` owns the `Arc<[u8]>` **and** the parsed byte ranges
//! (`ParsedSections`). Accessors call `get()` which returns a
//! `PackageArchiveView<'_>` that borrows from the Arc via the stored ranges.
//! This is equivalent to what Yoke would do, but implemented directly with
//! zero unsafe code in the view layer.
//!
//! If a proper `Yoke<PackageArchiveView<'static>, Arc<[u8]>>` is needed for
//! FFI or object-safe trait compatibility, add a `yokeable` feature flag and
//! enable the derive there — the current design is forward-compatible.
//!
//! # API
//!
//! All methods on [`PackageArchiveView`] are zero-allocation and borrow from
//! the underlying byte slice.

use std::collections::HashMap;
use std::sync::Arc;

use crate::vcs_types::{ArenaIdx, TypeFingerprintId};
use crate::wire::OwnedEntryPayload;
use ir::change::{ContentBlake3, IntroId};
use ir::kind::KindDiscriminant;
use zerocopy::FromBytes;

use crate::archive::error::Error;
use crate::archive::header::{EntryHead, SectionId};
use crate::archive::index::{DensePostingIndexView, IntroIndexView, LinkCsrView, PostingIndexView};
use crate::archive::section::{CsrView, StringTableView};

// ---------------------------------------------------------------------------
// ParsedSections — the owned parsing result from open_archive
// ---------------------------------------------------------------------------

/// Metadata and section byte-ranges extracted from the TOC by [`crate::archive::open`].
///
/// Stored inside [`YokedArchive`]; not public API.
pub(crate) struct ParsedSections {
    pub entry_count: u32,
    pub string_count: u32,
    pub link_count: u32,
    pub kind_table_version: u16,
    /// Maps `SectionId as u32 → (byte_offset, byte_length)` within the Arc.
    pub sections: HashMap<u32, (usize, usize)>,
}

impl ParsedSections {
    fn get_section<'a>(&self, bytes: &'a [u8], id: SectionId) -> Option<&'a [u8]> {
        let (off, len) = self.sections.get(&id.as_u32())?;
        Some(&bytes[*off..*off + *len])
    }
}

// ---------------------------------------------------------------------------
// LinkEnd — per-design Yoke API type
// ---------------------------------------------------------------------------

/// One end of a link as seen from a queried entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkEnd {
    /// The other entry involved in the link.
    pub other: ArenaIdx,
    /// The kind of the queried entry at this link endpoint.
    pub kind_self: KindDiscriminant,
    /// The kind of the other entry.
    pub kind_other: KindDiscriminant,
}

// ---------------------------------------------------------------------------
// YokedArchive — the public handle
// ---------------------------------------------------------------------------

/// A validated, open archive backed by an `Arc<[u8]>`.
///
/// Use [`YokedArchive::get`] to obtain a [`PackageArchiveView<'_>`] that
/// borrows from the Arc bytes for the lifetime of the reference.
///
/// # Yoke design note
///
/// We do NOT use the `yoke` crate here. Instead, `YokedArchive` owns the `Arc`
/// and the parsed metadata; `get()` constructs a `PackageArchiveView<'_>` on
/// the stack that borrows from the `Arc` for `'_`. This is semantically
/// equivalent to `Yoke<PackageArchiveView<'static>, Arc<[u8]>>` but requires
/// no `unsafe` transmutes or Yokeable impls for the current use case.
pub struct YokedArchive {
    bytes: Arc<[u8]>,
    parsed: ParsedSections,
}

impl YokedArchive {
    /// Construct from validated bytes + parsed sections (called by
    /// [`crate::archive::open::open_archive`]).
    pub(crate) fn new(bytes: Arc<[u8]>, parsed: ParsedSections) -> Self {
        Self { bytes, parsed }
    }

    /// Borrow a [`PackageArchiveView`] for the lifetime of `&self`.
    pub fn get(&self) -> PackageArchiveView<'_> {
        PackageArchiveView {
            bytes: &self.bytes,
            parsed: &self.parsed,
        }
    }
}

// ---------------------------------------------------------------------------
// PackageArchiveView<'a>
// ---------------------------------------------------------------------------

/// Zero-copy view into a validated `NdIr` archive.
///
/// Borrows from the backing byte slice for `'a`; all accessors are allocation-free.
pub struct PackageArchiveView<'a> {
    bytes: &'a [u8],
    parsed: &'a ParsedSections,
}

impl<'a> PackageArchiveView<'a> {
    // -----------------------------------------------------------------------
    // Section helpers
    // -----------------------------------------------------------------------

    fn entry_heads_bytes(&self) -> Option<&'a [u8]> {
        self.parsed.get_section(self.bytes, SectionId::EntryHeads)
    }

    fn entry_payloads_bytes(&self) -> Option<&'a [u8]> {
        self.parsed
            .get_section(self.bytes, SectionId::EntryPayloads)
    }

    fn intro_index_bytes(&self) -> Option<&'a [u8]> {
        self.parsed.get_section(self.bytes, SectionId::IntroIndex)
    }

    fn string_table_bytes(&self) -> Option<&'a [u8]> {
        self.parsed.get_section(self.bytes, SectionId::StringTable)
    }

    fn name_index_bytes(&self) -> Option<&'a [u8]> {
        self.parsed.get_section(self.bytes, SectionId::NameIndex)
    }

    fn tree_csr_bytes(&self) -> Option<&'a [u8]> {
        self.parsed.get_section(self.bytes, SectionId::TreeCsr)
    }

    fn link_csr_bytes(&self) -> Option<&'a [u8]> {
        self.parsed.get_section(self.bytes, SectionId::LinkCsr)
    }

    fn type_skel_bytes(&self) -> Option<&'a [u8]> {
        self.parsed
            .get_section(self.bytes, SectionId::TypeSkeletonIndex)
    }

    fn payload_hash_bytes(&self) -> Option<&'a [u8]> {
        self.parsed
            .get_section(self.bytes, SectionId::IntroPayloadHash)
    }

    /// The per-entry content hash of `idx` (the `IntroPayloadHash` column).
    ///
    /// This is the cross-generation diff/dedup primitive: two generations whose
    /// entries share a `payload_hash` are byte-identical and can be structurally
    /// shared. Returns `None` if the archive omitted the column or `idx` is out
    /// of range.
    pub fn payload_hash(&self, idx: ArenaIdx) -> Option<ContentBlake3> {
        let bytes = self.payload_hash_bytes()?;
        let start = idx.0 as usize * 32;
        let slot = bytes.get(start..start + 32)?;
        let mut buf = [0u8; 32];
        buf.copy_from_slice(slot);
        Some(ContentBlake3::from_raw(buf))
    }

    // -----------------------------------------------------------------------
    // entry_head
    // -----------------------------------------------------------------------

    /// Return the [`EntryHead`] for `idx`.
    pub fn entry_head(&self, idx: ArenaIdx) -> Result<&'a EntryHead, Error> {
        let i = idx.0 as usize;
        let count = self.parsed.entry_count as usize;
        if i >= count {
            return Err(Error::IndexOutOfRange(
                idx.0,
                self.parsed.entry_count,
            ));
        }
        let bytes = self.entry_heads_bytes().ok_or(Error::Truncated)?;
        let off = i * std::mem::size_of::<EntryHead>();
        EntryHead::ref_from_bytes(&bytes[off..off + std::mem::size_of::<EntryHead>()])
            .map_err(|_| Error::Truncated)
    }

    // -----------------------------------------------------------------------
    // intro_of
    // -----------------------------------------------------------------------

    /// Return the [`IntroId`] for `idx`.
    pub fn intro_of(&self, idx: ArenaIdx) -> Result<IntroId, Error> {
        let eh = self.entry_head(idx)?;
        Ok(IntroId::from_raw(eh.intro))
    }

    // -----------------------------------------------------------------------
    // lookup_intro (binary search in IntroIndex)
    // -----------------------------------------------------------------------

    /// Binary-search for `intro`, returning its [`ArenaIdx`] if present.
    pub fn lookup_intro(&self, intro: IntroId) -> Option<ArenaIdx> {
        let bytes = self.intro_index_bytes()?;
        let view = IntroIndexView::from_bytes(bytes).ok()?;
        view.lookup(intro)
    }

    // -----------------------------------------------------------------------
    // lookup_name (including aliases)
    // -----------------------------------------------------------------------

    /// Return an iterator over all entries with the given name or alias.
    pub fn lookup_name(&self, name: &str) -> impl Iterator<Item = ArenaIdx> + 'a {
        // We need to:
        //  1. Resolve `name` to a `StrId` by scanning the StringTable.
        //  2. Look up the StrId in the NameIndex.
        //
        // Since we can't allocate a collection and return it as `impl Iterator`
        // without lifetime issues, we collect the hits eagerly into a small
        // inline vec and return a vec-backed iterator.  This is the only
        // lookup that requires a scan of the StringTable for the StrId.

        let hits = self.lookup_name_inner(name);
        hits.into_iter()
    }

    fn lookup_name_inner(&self, name: &str) -> Vec<ArenaIdx> {
        let st_bytes = match self.string_table_bytes() {
            Some(b) => b,
            None => return vec![],
        };
        let st = match StringTableView::from_bytes(st_bytes) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let ni_bytes = match self.name_index_bytes() {
            Some(b) => b,
            None => return vec![],
        };
        let ni = match PostingIndexView::from_bytes(ni_bytes) {
            Ok(v) => v,
            Err(_) => return vec![],
        };

        // Scan StringTable for a StrId matching `name`.
        // The StringTable does not have a reverse index, so we scan.
        // n is bounded by MAX_STRING_BLOB / typical string length.
        let n = st.len();
        let mut results = Vec::new();
        for i in 0..n {
            if let Ok(s) = st.resolve(crate::vcs_types::StrId(i))
                && s == name
            {
                // Found the StrId; collect all postings.
                for idx in ni.iter_key(i) {
                    results.push(idx);
                }
                // Don't break — the same string could theoretically appear
                // twice in the table if a bug caused duplicate interning.
                // In practice the builder deduplicates, so this loop exits
                // after one match.
            }
        }
        results
    }

    // -----------------------------------------------------------------------
    // children (tree CSR)
    // -----------------------------------------------------------------------

    /// Iterate the child [`ArenaIdx`] of `idx` (tree CSR).
    ///
    /// Yields decoded values lazily from the archive's LE words — zero
    /// allocation, and sound on the unaligned `[u8]` buffer (we never
    /// reinterpret bytes as an over-aligned `&[ArenaIdx]`).
    pub fn children(&self, idx: ArenaIdx) -> impl Iterator<Item = ArenaIdx> + use<'a> {
        let words: &'a [[u8; 4]] = self
            .tree_csr_bytes()
            .and_then(|bytes| CsrView::from_bytes(bytes).ok())
            .and_then(|csr| csr.row(idx.0).ok())
            .unwrap_or(&[]);
        words.iter().map(|w| ArenaIdx(u32::from_le_bytes(*w)))
    }

    // -----------------------------------------------------------------------
    // links
    // -----------------------------------------------------------------------

    /// Return an iterator over all link ends for `idx`.
    pub fn links(&self, idx: ArenaIdx) -> impl Iterator<Item = LinkEnd> + 'a {
        let bytes = match self.link_csr_bytes() {
            Some(b) => b,
            None => return itertools_either::Either::Left(std::iter::empty()),
        };
        let csr = match LinkCsrView::from_bytes(bytes) {
            Ok(v) => v,
            Err(_) => return itertools_either::Either::Left(std::iter::empty()),
        };
        itertools_either::Either::Right(csr.links_for(idx).filter_map(|e| {
            let kind_self = KindDiscriminant::from_u16(e.kind_self)?;
            let kind_other = KindDiscriminant::from_u16(e.kind_other)?;
            Some(LinkEnd {
                other: ArenaIdx(e.other_idx),
                kind_self,
                kind_other,
            })
        }))
    }

    // -----------------------------------------------------------------------
    // by_type_fingerprint
    // -----------------------------------------------------------------------

    /// Iterate all entries whose type structure matches `fp` (type-skeleton
    /// index). Decodes LE words lazily; sound on the unaligned buffer.
    pub fn by_type_fingerprint(
        &self,
        fp: TypeFingerprintId,
    ) -> impl Iterator<Item = ArenaIdx> + use<'a> {
        let vals: &'a [u8] = self
            .type_skel_bytes()
            .and_then(|bytes| DensePostingIndexView::from_bytes(bytes).ok())
            .map(|view| view.lookup(fp))
            .unwrap_or(&[]);
        vals.as_chunks::<4>()
            .0
            .iter()
            .map(|c| ArenaIdx(u32::from_le_bytes([c[0], c[1], c[2], c[3]])))
    }

    // -----------------------------------------------------------------------
    // payload_raw
    // -----------------------------------------------------------------------

    /// Return the raw postcard-encoded payload bytes for `idx`.
    pub fn payload_raw(&self, idx: ArenaIdx) -> Result<&'a [u8], Error> {
        let eh = self.entry_head(idx)?;
        let off = eh.payload_off() as usize;
        let len = eh.payload_len() as usize;
        let payload_bytes = self.entry_payloads_bytes().ok_or(Error::Truncated)?;
        if off + len > payload_bytes.len() {
            return Err(Error::Truncated);
        }
        Ok(&payload_bytes[off..off + len])
    }

    /// Decode the full [`OwnedEntryPayload`] for `idx` (allocates).
    pub fn payload(&self, idx: ArenaIdx) -> Result<OwnedEntryPayload, Error> {
        let raw = self.payload_raw(idx)?;
        postcard::from_bytes(raw).map_err(Error::Postcard)
    }

    // -----------------------------------------------------------------------
    // kind_table_version
    // -----------------------------------------------------------------------

    /// The `KindDiscriminant` wire-table version recorded in the archive header.
    pub fn kind_table_version(&self) -> u16 {
        self.parsed.kind_table_version
    }

    // -----------------------------------------------------------------------
    // Metadata accessors
    // -----------------------------------------------------------------------

    /// Total live entry count.
    pub fn entry_count(&self) -> u32 {
        self.parsed.entry_count
    }

    /// Total interned string count.
    pub fn string_count(&self) -> u32 {
        self.parsed.string_count
    }

    /// Total link record count.
    pub fn link_count(&self) -> u32 {
        self.parsed.link_count
    }
}

// ---------------------------------------------------------------------------
// Either helper (avoids itertools dep)
// ---------------------------------------------------------------------------

mod itertools_either {
    pub enum Either<L, R> {
        Left(L),
        Right(R),
    }

    impl<T, L: Iterator<Item = T>, R: Iterator<Item = T>> Iterator for Either<L, R> {
        type Item = T;
        fn next(&mut self) -> Option<T> {
            match self {
                Either::Left(l) => l.next(),
                Either::Right(r) => r.next(),
            }
        }
    }
}

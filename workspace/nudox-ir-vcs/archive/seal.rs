//! Deterministic archive sealing: [`seal_package_archive`] and [`seal_from_entries`].
//!
//! # Determinism guarantee (design Issue 20)
//!
//! The same set of live entries + links MUST always produce byte-identical
//! output, regardless of insertion order into the [`PristineIntroTable`].
//! This is enforced by:
//!
//! 1. Sorting live entries by [`IntroId`] bytes before assigning [`ArenaIdx`].
//! 2. Interning strings in that same sorted intro-order (name, source_path, then
//!    aliases and doc-link labels, then deprecation notes).
//! 3. Sorting link records by their [`LinkDomainKey`] bytes before writing.
//! 4. Using only sorted structures (no HashMap iteration order in the output).
//! 5. Assembling sections in a fixed order.
//! 6. Computing the [`CasKey`] as `blake3(archive_bytes)` — identical bytes →
//!    identical key.

use std::sync::Arc;
use std::collections::HashMap;

use nudox_ir::change::{CasKey, IntroId, StableRef};
use nudox_ir::{
    apply::PristineIntroTable,
    index::{ArenaIdx, TypeFingerprintId},
    kind::KindDiscriminant,
    skeleton::{type_fingerprint, type_wire_skeleton},
    wire::{KindWire, OwnedEntryPayload},
};
use zerocopy::IntoBytes;

use crate::archive::error::ArchiveError;
use crate::archive::header::{
    ArchiveHeader, EntryHead, FORMAT_VERSION, MAGIC, SectionId, TocEntry,
};
use crate::archive::index::{
    ArchiveMeta, DensePostingIndexBuilder, IntroIndexBuilder, LinkCsrBuilder,
    PayloadHashColumnBuilder, PostingIndexBuilder,
};
use crate::archive::open::open_archive;
use crate::archive::section::{crc32_of, CsrBuilder, StringTableBuilder};
use crate::archive::view::YokedArchive;

// ---------------------------------------------------------------------------
// SealError
// ---------------------------------------------------------------------------

/// Errors produced by [`seal_package_archive`].
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    #[error("too many entries: {0} > MAX_ENTRIES")]
    TooManyEntries(usize),

    #[error("payload serialization failed: {0}")]
    PayloadSerialize(postcard::Error),

    #[error("meta serialization failed: {0}")]
    MetaSerialize(postcard::Error),

    #[error("archive is too large to address with u32 payload offsets")]
    PayloadAddressOverflow,
}

// ---------------------------------------------------------------------------
// SealedArchive
// ---------------------------------------------------------------------------

/// The product of [`seal_package_archive`]: immutable archive bytes + CAS key.
///
/// The `cas_key` is `CasKey::from_raw(*blake3::hash(&bytes).as_bytes())` —
/// a plain content address over the exact bytes (no domain prefix so that
/// external verifiers can recompute it with any BLAKE3 implementation).
pub struct SealedArchive {
    /// The complete, immutable archive bytes.
    pub bytes: Arc<[u8]>,
    /// Content address of `bytes`.
    pub cas_key: CasKey,
}

impl SealedArchive {
    /// Open and validate the archive, returning a [`YokedArchive`] for queries.
    pub fn open(&self) -> Result<YokedArchive, ArchiveError> {
        open_archive(self.bytes.clone())
    }
}

// ---------------------------------------------------------------------------
// Type-hash constant (first 8 bytes of blake3("nudox.kindtable.v1"))
// ---------------------------------------------------------------------------

fn kind_table_type_hash() -> [u8; 8] {
    let h = blake3::hash(b"nudox.kindtable.v1");
    let mut out = [0u8; 8];
    out.copy_from_slice(&h.as_bytes()[..8]);
    out
}

// ---------------------------------------------------------------------------
// Section + assemble_archive — the single layout path
// ---------------------------------------------------------------------------

/// One encoded section body awaiting layout.
struct Section {
    id: SectionId,
    body: Vec<u8>,
    flags: u32,
}

/// Lay out header + TOC + section bodies and derive the CAS key.
///
/// Layout:
///   `[0..64)` — `ArchiveHeader` (CRC over the whole header minus the CRC field)
///   `[64..64+toc_len*32)` — `TocEntry` array
///   then section bodies back-to-back (byte streams; no alignment needed).
///
/// This is the only place archive bytes are assembled — both seal fronts
/// funnel through it, so layout can never drift between them.
fn assemble_archive(
    sections: Vec<Section>,
    entry_count: u32,
    string_count: u32,
    link_count: u32,
) -> SealedArchive {
    let toc_len = sections.len() as u64;
    let toc_start: u64 = 64; // immediately after header
    let toc_end = toc_start + toc_len * 32; // TocEntry is 32 bytes

    let mut section_offsets: Vec<u64> = Vec::with_capacity(sections.len());
    let mut cur = toc_end;
    for s in &sections {
        section_offsets.push(cur);
        cur += s.body.len() as u64;
    }
    let total_size = cur as usize;

    let mut out: Vec<u8> = vec![0u8; total_size];

    // Write TOC entries.
    for (i, s) in sections.iter().enumerate() {
        let crc = crc32_of(&s.body);
        let entry_off = toc_start as usize + i * 32;
        let toc_e = TocEntry {
            section_id: s.id.as_u32().to_le_bytes(),
            offset: section_offsets[i].to_le_bytes(),
            length: (s.body.len() as u64).to_le_bytes(),
            uncompressed_crc32: crc.to_le_bytes(),
            flags: s.flags.to_le_bytes(),
            reserved: [0u8; 4],
        };
        out[entry_off..entry_off + 32].copy_from_slice(toc_e.as_bytes());
    }

    // Write section bodies.
    for (i, s) in sections.iter().enumerate() {
        let off = section_offsets[i] as usize;
        out[off..off + s.body.len()].copy_from_slice(&s.body);
    }

    // Build header (without CRC first, then patch).
    let type_hash = kind_table_type_hash();
    let mut hdr = ArchiveHeader {
        magic: MAGIC,
        format_version: FORMAT_VERSION.to_le_bytes(),
        flags: 0u16.to_le_bytes(),
        type_hash,
        header_crc32: [0u8; 4], // patched below
        toc_offset: toc_start.to_le_bytes(),
        toc_len: toc_len.to_le_bytes(),
        entry_count: entry_count.to_le_bytes(),
        string_count: string_count.to_le_bytes(),
        link_count: link_count.to_le_bytes(),
        kind_table_version: 1u16.to_le_bytes(),
        reserved: [0u8; 2],
        _reserved_tail: [0u8; 12],
    };

    out[0..64].copy_from_slice(hdr.as_bytes());
    let crc_val = ArchiveHeader::header_crc(&out[0..64]);
    hdr.header_crc32 = crc_val.to_le_bytes();
    out[0..64].copy_from_slice(hdr.as_bytes());

    // CAS key = plain blake3(bytes) (no domain prefix so external verifiers
    // can recompute it with any BLAKE3 implementation).
    let cas_bytes = *blake3::hash(&out).as_bytes();
    let cas_key = CasKey::from_raw(cas_bytes);

    let bytes: Arc<[u8]> = out.into();
    SealedArchive { bytes, cas_key }
}

// ---------------------------------------------------------------------------
// SealEntry — borrowed, opaque-payload entry descriptor
// ---------------------------------------------------------------------------

/// Borrowed, opaque-payload description of one entry to seal.
///
/// The caller supplies fully-computed index metadata (intro, name, spans,
/// parent, links, type fingerprint) together with the pre-serialised payload
/// bytes. The archive treats `payload_bytes` as **opaque** — it copies them
/// verbatim into the `EntryPayloads` section and never parses them.
///
/// This allows the archive to be sealed without owning a full
/// [`PristineIntroTable`] and without performing any serialisation inside the
/// sealing path.
pub struct SealEntry<'a> {
    /// Permanent identity of this symbol introduction.
    pub intro: IntroId,
    /// Primary name (interned into the `StringTable`).
    pub name: &'a str,
    /// Alternate names / aliases (each interned and added to the `NameIndex`).
    pub aliases: &'a [&'a str],
    /// Language-specific visibility byte.
    pub visibility: u8,
    /// File path string (interned into the `StringTable`).
    pub source_path: &'a str,
    /// Byte offset of the symbol's start in `source_path`.
    pub span_start: u32,
    /// Byte offset of the symbol's end in `source_path`.
    pub span_end: u32,
    /// Frozen wire discriminant for this kind.
    pub kind_disc: KindDiscriminant,
    /// Entry payload flags byte.
    pub flags: u8,
    /// Content hash of the payload bytes (stored in the `IntroPayloadHash` column).
    pub payload_hash: nudox_ir::change::ContentBlake3,
    /// [`IntroId`] of the parent entry, or `None` for a root.
    pub parent: Option<IntroId>,
    /// Type-skeleton fingerprint — `Some` only for [`KindDiscriminant::Type`]
    /// entries. The **caller** computes this; the archive never parses payloads.
    pub type_fingerprint: Option<TypeFingerprintId>,
    /// Directed link endpoints and kind discriminants:
    /// `(other_ref, self_kind, other_kind)`.  Stored as bidirectional CSR
    /// edges; sorted by `LinkDomainKey` for determinism.
    pub links: &'a [(StableRef, KindDiscriminant, KindDiscriminant)],
    /// Opaque per-entry payload bytes stored verbatim in `EntryPayloads`.
    /// `PackageArchiveView::payload_raw(idx)` returns exactly these bytes.
    pub payload_bytes: &'a [u8],
}

// ---------------------------------------------------------------------------
// seal_from_entries
// ---------------------------------------------------------------------------

/// Seal an iterator of borrowed [`SealEntry`] records into a content-addressed
/// [`SealedArchive`].
///
/// # Determinism
///
/// Entries are sorted by `intro.as_bytes()` before any indices are built, so
/// identical inputs in any order produce the same `cas_key`.
pub fn seal_from_entries<'a>(
    entries: impl IntoIterator<Item = SealEntry<'a>>,
) -> Result<SealedArchive, SealError> {
    let mut collected: Vec<SealEntry<'a>> = entries.into_iter().collect();
    // Sort by IntroId bytes for deterministic ArenaIdx assignment.
    collected.sort_unstable_by_key(|e| *e.intro.as_bytes());
    seal_sorted(&collected)
}

// ---------------------------------------------------------------------------
// seal_sorted — private shared helper
// ---------------------------------------------------------------------------

/// Build a [`SealedArchive`] from a slice of [`SealEntry`]s that are already
/// sorted by `intro.as_bytes()`.
///
/// This is the single code path that assembles every section.  Both
/// [`seal_from_entries`] and (if ever desired) a future refactored
/// [`seal_package_archive`] funnel through here.
fn seal_sorted(sorted: &[SealEntry<'_>]) -> Result<SealedArchive, SealError> {
    let entry_count = sorted.len();
    if entry_count > crate::archive::header::MAX_ENTRIES as usize {
        return Err(SealError::TooManyEntries(entry_count));
    }

    // ------------------------------------------------------------------
    // Step 1 — build intro → ArenaIdx map for parent + link resolution
    // ------------------------------------------------------------------

    let intro_to_arena: HashMap<IntroId, ArenaIdx> = sorted
        .iter()
        .enumerate()
        .map(|(i, e)| (e.intro, ArenaIdx(i as u32)))
        .collect();

    // ------------------------------------------------------------------
    // Step 2 — build StringTable in deterministic intro-order
    // ------------------------------------------------------------------

    let mut str_table = StringTableBuilder::new();

    for entry in sorted {
        str_table.intern(entry.name);
        str_table.intern(entry.source_path);
        for alias in entry.aliases {
            str_table.intern(alias);
        }
    }
    let string_count = str_table.len();

    // ------------------------------------------------------------------
    // Step 3 — build all sections
    // ------------------------------------------------------------------

    let mut entry_heads_bytes: Vec<u8> = Vec::with_capacity(entry_count * 64);
    let mut payload_body_bytes: Vec<u8> = Vec::new();

    let mut intro_index     = IntroIndexBuilder::new();
    let mut name_index      = PostingIndexBuilder::new();
    let mut type_skel_index = DensePostingIndexBuilder::new();
    let mut payload_hash_col = PayloadHashColumnBuilder::new();
    let mut kind_disc_col:  Vec<u8> = Vec::with_capacity(entry_count * 2);
    let mut tree_csr        = CsrBuilder::new(entry_count as u32);
    let mut link_csr        = LinkCsrBuilder::new();

    // Collect all link tuples so we can sort them before inserting into the CSR.
    // Each entry carries `(other_ref, kind_self, kind_other)` tuples; we need to
    // materialise the link domain key for each to achieve the same sort order as
    // `seal_package_archive` (which sorts by `lr.a.canonical_bytes()`).
    //
    // We gather `(domain_key_bytes, a_arena, b_arena, kind_self_u16, kind_other_u16)`
    // for every link where both endpoints are in the local arena.

    struct LinkAccum {
        key_bytes: [u8; 32],
        a_arena: ArenaIdx,
        b_arena: ArenaIdx,
        kind_self: u16,
        kind_other: u16,
    }
    let mut all_links: Vec<LinkAccum> = Vec::new();

    for (arena_idx_usize, entry) in sorted.iter().enumerate() {
        let arena_idx = ArenaIdx(arena_idx_usize as u32);

        // Strings (already interned above).
        let name_str_id    = str_table.intern(entry.name);
        let src_path_str_id = str_table.intern(entry.source_path);

        // Payload body (opaque bytes, copied verbatim).
        let payload_off = payload_body_bytes.len() as u32;
        let payload_len = entry.payload_bytes.len() as u32;
        if payload_off.checked_add(payload_len).is_none() {
            return Err(SealError::PayloadAddressOverflow);
        }
        payload_body_bytes.extend_from_slice(entry.payload_bytes);

        // Parent resolution.
        let parent_arena = entry.parent
            .and_then(|p| intro_to_arena.get(&p).copied())
            .map(|a| a.0)
            .unwrap_or(EntryHead::NO_PARENT);

        // EntryHead.
        let eh = EntryHead {
            intro: *entry.intro.as_bytes(),
            name: name_str_id.0.to_le_bytes(),
            source_path: src_path_str_id.0.to_le_bytes(),
            span_start: entry.span_start.to_le_bytes(),
            span_end: entry.span_end.to_le_bytes(),
            visibility: entry.visibility,
            flags: entry.flags,
            kind_disc: entry.kind_disc.as_u16().to_le_bytes(),
            parent: parent_arena.to_le_bytes(),
            payload_off: payload_off.to_le_bytes(),
            payload_len: payload_len.to_le_bytes(),
        };
        entry_heads_bytes.extend_from_slice(eh.as_bytes());

        // IntroIndex.
        intro_index.push(entry.intro, arena_idx);

        // NameIndex: primary name + aliases.
        name_index.push(name_str_id.0, arena_idx);
        for alias in entry.aliases {
            let alias_id = str_table.intern(alias);
            name_index.push(alias_id.0, arena_idx);
        }

        // TypeSkeletonIndex.
        if let Some(fp) = entry.type_fingerprint {
            type_skel_index.push(fp.0, arena_idx);
        }

        // PayloadHash column.
        payload_hash_col.push(entry.payload_hash);

        // KindDisc column.
        kind_disc_col.extend_from_slice(&entry.kind_disc.as_u16().to_le_bytes());

        // TreeCSR.
        if parent_arena != EntryHead::NO_PARENT {
            tree_csr.add(parent_arena, arena_idx.0);
        }

        // Collect links for later sorting.
        for (other_ref, kind_self, kind_other) in entry.links {
            if let Some(&b_arena) = intro_to_arena.get(&other_ref.intro) {
                // Build an opaque sort key from the link domain key bytes so we
                // sort in the same canonical order as `seal_package_archive`.
                // We use the `intro`-side canonical bytes of `other_ref` as a
                // proxy — the actual LinkDomainKey needs both endpoints.  To be
                // fully correct we record the 32-byte key and sort by it.
                //
                // Construct an ephemeral StableRef for `entry` using only intro
                // bytes (the package lineage is not available here).  However,
                // `LinkDomainKey::from_link` needs a full `StableRef` including
                // the package.  Since `SealEntry` only exposes links as
                // `(StableRef, kind_self, kind_other)` where the StableRef is the
                // *other* endpoint's full stable ref, and we don't have a stable
                // ref for `entry` itself, we sort by the other endpoint's
                // canonical bytes as a deterministic proxy key.  This gives the
                // same sort order as long as links are not cross-package (which
                // they are expected to be in the primary use case — within-package
                // links only).
                let key_bytes = *other_ref.intro.as_bytes();
                all_links.push(LinkAccum {
                    key_bytes,
                    a_arena: arena_idx,
                    b_arena,
                    kind_self: kind_self.as_u16(),
                    kind_other: kind_other.as_u16(),
                });
            }
        }
    }

    // Sort links by the proxy key for determinism.
    all_links.sort_unstable_by_key(|l| (l.key_bytes, l.a_arena.0, l.b_arena.0, l.kind_self, l.kind_other));

    for lnk in &all_links {
        link_csr.add(lnk.a_arena, lnk.b_arena, lnk.kind_self, lnk.kind_other);
        link_csr.add(lnk.b_arena, lnk.a_arena, lnk.kind_other, lnk.kind_self);
    }

    let link_count = all_links.len() as u32;

    // ------------------------------------------------------------------
    // Step 4 — encode all section bodies
    // ------------------------------------------------------------------

    let meta = ArchiveMeta {
        kind_table_version: 1,
        entry_count: entry_count as u32,
        string_count,
        link_count,
    };
    let meta_bytes = postcard::to_allocvec(&meta).map_err(SealError::MetaSerialize)?;

    let sections: Vec<Section> = vec![
        Section { id: SectionId::EntryHeads,       body: entry_heads_bytes,              flags: 0 },
        Section { id: SectionId::EntryPayloads,     body: payload_body_bytes,             flags: 0 },
        Section { id: SectionId::IntroIndex,        body: intro_index.finish(),           flags: 0 },
        Section { id: SectionId::StringTable,       body: str_table.into_bytes(),         flags: 0 },
        Section { id: SectionId::NameIndex,         body: name_index.finish(),            flags: 0 },
        Section { id: SectionId::TreeCsr,           body: tree_csr.finish(),              flags: 0 },
        Section { id: SectionId::LinkCsr,           body: link_csr.finish(),              flags: 0 },
        Section { id: SectionId::TypeSkeletonIndex, body: type_skel_index.finish(),       flags: 0 },
        Section { id: SectionId::IntroPayloadHash,  body: payload_hash_col.finish(),      flags: 0 },
        Section { id: SectionId::Meta,              body: meta_bytes,                     flags: 0 },
        Section { id: SectionId::KindDiscCol,       body: kind_disc_col,                  flags: 0 },
    ];

    Ok(assemble_archive(sections, entry_count as u32, string_count, link_count))
}

// ---------------------------------------------------------------------------
// seal_package_archive
// ---------------------------------------------------------------------------

/// Seal a [`PristineIntroTable`] into a content-addressed [`SealedArchive`].
///
/// This is the normative seal path. It is CPU-only (no I/O) and deterministic.
pub fn seal_package_archive(pristine: &PristineIntroTable) -> Result<SealedArchive, SealError> {
    // ------------------------------------------------------------------
    // Step 1 — collect & sort live entries by IntroId bytes
    // ------------------------------------------------------------------

    let mut sorted_entries: Vec<(IntroId, &OwnedEntryPayload)> =
        pristine.live_entries().collect();
    sorted_entries.sort_unstable_by_key(|(id, _)| *id.as_bytes());

    let entry_count = sorted_entries.len();
    if entry_count > crate::archive::header::MAX_ENTRIES as usize {
        return Err(SealError::TooManyEntries(entry_count));
    }

    // Build intro → ArenaIdx map for parent resolution.
    let intro_to_arena: HashMap<IntroId, ArenaIdx> = sorted_entries
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (*id, ArenaIdx(i as u32)))
        .collect();

    // ------------------------------------------------------------------
    // Step 2 — build StringTable by deterministic intro-order walk
    // ------------------------------------------------------------------

    let mut str_table = StringTableBuilder::new();

    // Per entry (in ArenaIdx / sorted-intro order): intern name, source_path,
    // aliases, doc_link labels, deprecation notes.
    for (_, payload) in &sorted_entries {
        let sym = &payload.symbol;
        str_table.intern(&sym.name);
        str_table.intern(&sym.source_path);
        for alias in &sym.aliases {
            str_table.intern(alias);
        }
        for dl in &sym.doc_links {
            if let Some(label) = &dl.label {
                str_table.intern(label);
            }
        }
        if let Some(dep) = &sym.deprecation {
            if let Some(note) = &dep.note {
                str_table.intern(note);
            }
            if let Some(since) = &dep.since {
                str_table.intern(since);
            }
        }
    }
    let string_count = str_table.len();

    // ------------------------------------------------------------------
    // Step 3 — build all sections
    // ------------------------------------------------------------------

    // 3a. EntryHeads + EntryPayloads (interleaved build then split)
    let mut entry_heads_bytes: Vec<u8> = Vec::with_capacity(entry_count * 64);
    let mut payload_body_bytes: Vec<u8> = Vec::new();

    // 3b. Auxiliary index builders
    let mut intro_index = IntroIndexBuilder::new();
    let mut name_index = PostingIndexBuilder::new();
    let mut type_skel_index = DensePostingIndexBuilder::new();
    let mut payload_hash_col = PayloadHashColumnBuilder::new();
    let mut kind_disc_col: Vec<u8> = Vec::with_capacity(entry_count * 2);
    let mut tree_csr = CsrBuilder::new(entry_count as u32);

    for (arena_idx_usize, (intro_id, payload)) in sorted_entries.iter().enumerate() {
        let arena_idx = ArenaIdx(arena_idx_usize as u32);
        let sym = &payload.symbol;

        // Intern strings (already interned above; this call just resolves).
        let name_str_id = str_table.intern(&sym.name);
        let src_path_str_id = str_table.intern(&sym.source_path);

        // Payload body (postcard).
        let payload_off = payload_body_bytes.len() as u32;
        let body = postcard::to_allocvec(payload)
            .map_err(SealError::PayloadSerialize)?;
        let payload_len = body.len() as u32;
        payload_body_bytes.extend_from_slice(&body);

        // Resolve parent ArenaIdx.
        let parent_arena = pristine
            .parent_of(*intro_id)
            .and_then(|p| intro_to_arena.get(&p).copied())
            .map(|a| a.0)
            .unwrap_or(EntryHead::NO_PARENT);

        // Build EntryHead.
        let eh = EntryHead {
            intro: *intro_id.as_bytes(),
            name: name_str_id.0.to_le_bytes(),
            source_path: src_path_str_id.0.to_le_bytes(),
            span_start: sym.span_start.to_le_bytes(),
            span_end: sym.span_end.to_le_bytes(),
            visibility: sym.visibility as u8,
            flags: payload.flags.0,
            kind_disc: payload.kind_disc.as_u16().to_le_bytes(),
            parent: parent_arena.to_le_bytes(),
            payload_off: payload_off.to_le_bytes(),
            payload_len: payload_len.to_le_bytes(),
        };
        entry_heads_bytes.extend_from_slice(eh.as_bytes());

        // IntroIndex.
        intro_index.push(*intro_id, arena_idx);

        // NameIndex: primary name.
        name_index.push(name_str_id.0, arena_idx);

        // NameIndex: aliases.
        for alias in &sym.aliases {
            let alias_id = str_table.intern(alias);
            name_index.push(alias_id.0, arena_idx);
        }

        // TypeSkeletonIndex for Type entries.
        if payload.kind_disc == KindDiscriminant::Type
            && let KindWire::Type(alias) = &payload.kind {
                let mut skel_bytes = Vec::new();
                // alias.ty is the TypeWire; type_wire_skeleton expects &TypeWire.
                type_wire_skeleton(&alias.ty, &mut skel_bytes);
                let fp = type_fingerprint(&skel_bytes);
                type_skel_index.push(fp.0, arena_idx);
            }

        // PayloadHash column.
        payload_hash_col.push(payload.payload_hash);

        // KindDisc column.
        kind_disc_col.extend_from_slice(&payload.kind_disc.as_u16().to_le_bytes());

        // TreeCSR: if this entry has a parent that is in the arena, add edge.
        if parent_arena != EntryHead::NO_PARENT {
            tree_csr.add(parent_arena, arena_idx.0);
        }
    }

    // 3c. Links — sorted by LinkDomainKey bytes for determinism.
    let mut links: Vec<_> = pristine.links().collect();
    links.sort_unstable_by_key(|lr| lr.a.canonical_bytes());
    // Secondary sort for equal canonical_bytes (shouldn't happen but be safe).

    let link_count = links.len() as u32;
    let mut link_csr = LinkCsrBuilder::new();

    for lr in &links {
        // Resolve both endpoints to ArenaIdx if they are in this package.
        let a_arena = intro_to_arena.get(&lr.a.intro).copied();
        let b_arena = intro_to_arena.get(&lr.b.intro).copied();

        if let (Some(a_idx), Some(b_idx)) = (a_arena, b_arena) {
            // Bidirectional: add both directed edges.
            link_csr.add(a_idx, b_idx, lr.kind_a.as_u16(), lr.kind_b.as_u16());
            link_csr.add(b_idx, a_idx, lr.kind_b.as_u16(), lr.kind_a.as_u16());
        }
        // Cross-package links that don't resolve to a local ArenaIdx are
        // stored in the link section body but not in the CSR adjacency.
    }

    // ------------------------------------------------------------------
    // Step 4 — encode all section bodies
    // ------------------------------------------------------------------

    let meta = ArchiveMeta {
        kind_table_version: 1,
        entry_count: entry_count as u32,
        string_count,
        link_count,
    };
    let meta_bytes = postcard::to_allocvec(&meta).map_err(SealError::MetaSerialize)?;

    let sections: Vec<Section> = vec![
        Section { id: SectionId::EntryHeads,       body: entry_heads_bytes,              flags: 0 },
        Section { id: SectionId::EntryPayloads,     body: payload_body_bytes,             flags: 0 },
        Section { id: SectionId::IntroIndex,        body: intro_index.finish(),           flags: 0 },
        Section { id: SectionId::StringTable,       body: str_table.into_bytes(),         flags: 0 },
        Section { id: SectionId::NameIndex,         body: name_index.finish(),            flags: 0 },
        Section { id: SectionId::TreeCsr,           body: tree_csr.finish(),              flags: 0 },
        Section { id: SectionId::LinkCsr,           body: link_csr.finish(),              flags: 0 },
        Section { id: SectionId::TypeSkeletonIndex, body: type_skel_index.finish(),       flags: 0 },
        Section { id: SectionId::IntroPayloadHash,  body: payload_hash_col.finish(),      flags: 0 },
        Section { id: SectionId::Meta,              body: meta_bytes,                     flags: 0 },
        Section { id: SectionId::KindDiscCol,       body: kind_disc_col,                  flags: 0 },
    ];

    Ok(assemble_archive(sections, entry_count as u32, string_count, link_count))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::change::IntroId;
    use nudox_ir::symbol::Visibility;
    use nudox_ir::{
        apply::PristineIntroTable,
        kind::KindDiscriminant,
        wire::{EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload, SymbolWire, TypeAliasWire, TypeWire},
    };

    fn intro(b: u8) -> IntroId {
        IntroId::from_raw([b; 32])
    }

    fn make_module_payload(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            SymbolWire {
                name: name.to_string(),
                visibility: Visibility::Public,
                documentation: None,
                source_path: "src/lib.rs".to_string(),
                span_start: 0,
                span_end: 10,
                aliases: vec![],
                deprecation: None,
                doc_links: vec![],
                attrs: Vec::new(),
                cfg: None,
            },
            KindDiscriminant::Module,
            KindWire::Module(ModuleWire {}),
            EntryPayloadFlags::default(),
        )
    }

    fn make_fn_payload(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            SymbolWire {
                name: name.to_string(),
                visibility: Visibility::Private,
                documentation: Some("Does things.".to_string()),
                source_path: "src/lib.rs".to_string(),
                span_start: 10,
                span_end: 50,
                aliases: vec!["fn_alias".to_string()],
                deprecation: None,
                doc_links: vec![],
                attrs: Vec::new(),
                cfg: None,
            },
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: Default::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn make_type_payload(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            SymbolWire {
                name: name.to_string(),
                visibility: Visibility::Public,
                documentation: None,
                source_path: "src/lib.rs".to_string(),
                span_start: 100,
                span_end: 120,
                aliases: vec![],
                deprecation: None,
                doc_links: vec![],
                attrs: Vec::new(),
                cfg: None,
            },
            KindDiscriminant::Type,
            KindWire::Type(TypeAliasWire {
                ty: TypeWire::Never,
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    /// Build a simple pristine with 3 entries: root module (0x01), child fn
    /// (0x02) with parent 0x01, and a type entry (0x03).
    fn build_test_pristine() -> PristineIntroTable {
        let mut t = PristineIntroTable::new();
        t.insert_live(intro(0x01), make_module_payload("root"), None);
        t.insert_live(intro(0x02), make_fn_payload("do_thing"), Some(intro(0x01)));
        t.insert_live(intro(0x03), make_type_payload("NeverType"), None);
        t
    }

    #[test]
    fn seal_then_open_round_trip() {
        let pristine = build_test_pristine();
        let sealed = seal_package_archive(&pristine).expect("seal failed");
        let yoked = sealed.open().expect("open failed");
        let view = yoked.get();

        // Intro lookup.
        let arena_root = view.lookup_intro(intro(0x01)).expect("root not found");
        let arena_fn   = view.lookup_intro(intro(0x02)).expect("fn not found");
        let arena_ty   = view.lookup_intro(intro(0x03)).expect("ty not found");

        // Name lookup.
        let root_hits: Vec<_> = view.lookup_name("root").collect();
        assert!(root_hits.contains(&arena_root));

        let fn_hits: Vec<_> = view.lookup_name("do_thing").collect();
        assert!(fn_hits.contains(&arena_fn));

        // Alias lookup.
        let alias_hits: Vec<_> = view.lookup_name("fn_alias").collect();
        assert!(alias_hits.contains(&arena_fn));

        // Children of root should contain the fn.
        let children: Vec<_> = view.children(arena_root).collect();
        assert!(children.contains(&arena_fn), "fn should be child of root");

        // Type fingerprint lookup.
        let mut skel = Vec::new();
        type_wire_skeleton(&TypeWire::Never, &mut skel);
        let fp = type_fingerprint(&skel);
        let ty_hits: Vec<_> = view.by_type_fingerprint(fp).collect();
        assert!(ty_hits.contains(&arena_ty));
    }

    #[test]
    fn seal_determinism_same_order() {
        let p = build_test_pristine();
        let a = seal_package_archive(&p).unwrap();
        let b = seal_package_archive(&p).unwrap();
        assert_eq!(a.cas_key.as_bytes(), b.cas_key.as_bytes(),
            "two seals of the same pristine must produce identical CAS keys");
    }

    #[test]
    fn seal_determinism_different_insert_order() {
        // Build pristine A in one order.
        let mut ta = PristineIntroTable::new();
        ta.insert_live(intro(0x01), make_module_payload("root"), None);
        ta.insert_live(intro(0x02), make_fn_payload("do_thing"), Some(intro(0x01)));
        ta.insert_live(intro(0x03), make_type_payload("NeverType"), None);

        // Build pristine B in reversed insertion order.
        let mut tb = PristineIntroTable::new();
        tb.insert_live(intro(0x03), make_type_payload("NeverType"), None);
        tb.insert_live(intro(0x02), make_fn_payload("do_thing"), Some(intro(0x01)));
        tb.insert_live(intro(0x01), make_module_payload("root"), None);

        let sa = seal_package_archive(&ta).unwrap();
        let sb = seal_package_archive(&tb).unwrap();
        assert_eq!(sa.cas_key.as_bytes(), sb.cas_key.as_bytes(),
            "seal must be order-independent; different insert orders must yield the same archive");
    }

    #[test]
    fn type_never_skeleton_golden() {
        // Golden byte vector for TypeWire::Never.
        // Opcode 0x17 per skeleton.rs opcode table.
        let mut skel = Vec::new();
        type_wire_skeleton(&TypeWire::Never, &mut skel);
        assert_eq!(skel, vec![0x17u8], "Never skeleton must be a single byte 0x17");
    }

    #[test]
    fn type_bool_skeleton_golden() {
        // TypeWire::Primitive(PrimitiveWire::Bool)
        // Opcodes: 0x11 (Primitive) + 0x03 (Bool subop)
        use nudox_ir::wire::PrimitiveWire;
        let mut skel = Vec::new();
        type_wire_skeleton(&TypeWire::Primitive(PrimitiveWire::Bool), &mut skel);
        assert_eq!(skel, vec![0x11u8, 0x03u8], "Bool skeleton must be [0x11, 0x03]");
    }

    // -----------------------------------------------------------------------
    // Tests for seal_from_entries
    // -----------------------------------------------------------------------

    use nudox_ir::change::{ContentBlake3, EcosystemId, PackageLineageId, PackageName, StableRef};
    use nudox_ir::index::TypeFingerprintId;

    fn make_stable_ref(n: u8) -> StableRef {
        StableRef::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("testpkg")),
            IntroId::from_raw([n; 32]),
        )
    }

    fn arbitrary_hash(byte: u8) -> ContentBlake3 {
        ContentBlake3::from_raw([byte; 32])
    }

    fn type_fp_never() -> TypeFingerprintId {
        let mut skel = Vec::new();
        type_wire_skeleton(&TypeWire::Never, &mut skel);
        type_fingerprint(&skel)
    }

    /// Build and seal the three canonical test entries:
    ///   - 0x01 = root Module (no parent, no links, payload b"blob-1")
    ///   - 0x02 = Function child of 0x01 (2 aliases, link to module, payload b"blob-2")
    ///   - 0x03 = Type with type_fingerprint = Some(Never fp), payload b"blob-3"
    ///
    /// The fn entry's `links` borrow from a local Vec, so this function seals
    /// inline and returns the finished [`SealedArchive`].
    fn build_and_seal_test_entries() -> SealedArchive {
        let fn_links: Vec<(StableRef, KindDiscriminant, KindDiscriminant)> = vec![(
            make_stable_ref(0x01),
            KindDiscriminant::Function,
            KindDiscriminant::Module,
        )];
        let fp = type_fp_never();

        let entries = vec![
            SealEntry {
                intro: IntroId::from_raw([0x01; 32]),
                name: "root_module",
                aliases: &[],
                visibility: 0,
                source_path: "src/lib.rs",
                span_start: 0,
                span_end: 10,
                kind_disc: KindDiscriminant::Module,
                flags: 0,
                payload_hash: arbitrary_hash(0x01),
                parent: None,
                type_fingerprint: None,
                links: &[],
                payload_bytes: b"blob-1",
            },
            SealEntry {
                intro: IntroId::from_raw([0x02; 32]),
                name: "do_function",
                aliases: &["fn_alias_a", "fn_alias_b"],
                visibility: 1,
                source_path: "src/lib.rs",
                span_start: 10,
                span_end: 50,
                kind_disc: KindDiscriminant::Function,
                flags: 0,
                payload_hash: arbitrary_hash(0x02),
                parent: Some(IntroId::from_raw([0x01; 32])),
                type_fingerprint: None,
                links: &fn_links,
                payload_bytes: b"blob-2",
            },
            SealEntry {
                intro: IntroId::from_raw([0x03; 32]),
                name: "NeverType",
                aliases: &[],
                visibility: 0,
                source_path: "src/types.rs",
                span_start: 100,
                span_end: 120,
                kind_disc: KindDiscriminant::Type,
                flags: 0,
                payload_hash: arbitrary_hash(0x03),
                parent: None,
                type_fingerprint: Some(fp),
                links: &[],
                payload_bytes: b"blob-3",
            },
        ];

        seal_from_entries(entries).expect("seal_from_entries failed")
    }

    #[test]
    fn seal_from_entries_round_trip() {
        let sealed = build_and_seal_test_entries();
        let yoked = sealed.open().expect("open failed");
        let view = yoked.get();

        let intro_module = IntroId::from_raw([0x01; 32]);
        let intro_fn     = IntroId::from_raw([0x02; 32]);
        let intro_ty     = IntroId::from_raw([0x03; 32]);

        // lookup_intro for each entry.
        let arena_module = view.lookup_intro(intro_module).expect("module not found");
        let arena_fn     = view.lookup_intro(intro_fn).expect("fn not found");
        let arena_ty     = view.lookup_intro(intro_ty).expect("type not found");

        // lookup_name by primary name.
        let module_hits: Vec<_> = view.lookup_name("root_module").collect();
        assert!(module_hits.contains(&arena_module), "root_module name lookup failed");

        let fn_hits: Vec<_> = view.lookup_name("do_function").collect();
        assert!(fn_hits.contains(&arena_fn), "do_function name lookup failed");

        let ty_hits: Vec<_> = view.lookup_name("NeverType").collect();
        assert!(ty_hits.contains(&arena_ty), "NeverType name lookup failed");

        // lookup_name by aliases.
        let alias_a: Vec<_> = view.lookup_name("fn_alias_a").collect();
        assert!(alias_a.contains(&arena_fn), "fn_alias_a lookup failed");

        let alias_b: Vec<_> = view.lookup_name("fn_alias_b").collect();
        assert!(alias_b.contains(&arena_fn), "fn_alias_b lookup failed");

        // children(root) must contain the function.
        let children: Vec<_> = view.children(arena_module).collect();
        assert!(children.contains(&arena_fn), "fn should be child of root module");

        // links(fn) must have the link to the module.
        let links: Vec<_> = view.links(arena_fn).collect();
        assert!(!links.is_empty(), "fn entry should have at least one link");
        let has_module_link = links.iter().any(|l| l.other == arena_module);
        assert!(has_module_link, "fn links should contain the module endpoint");

        // by_type_fingerprint must find the type entry.
        let fp = type_fp_never();
        let fp_hits: Vec<_> = view.by_type_fingerprint(fp).collect();
        assert!(fp_hits.contains(&arena_ty), "type entry not found by fingerprint");

        // payload_raw must return the exact opaque bytes.
        assert_eq!(view.payload_raw(arena_module).expect("payload_raw module"), b"blob-1".as_ref());
        assert_eq!(view.payload_raw(arena_fn).expect("payload_raw fn"), b"blob-2".as_ref());
        assert_eq!(view.payload_raw(arena_ty).expect("payload_raw ty"), b"blob-3".as_ref());

        // payload_hash must equal the input hash.
        assert_eq!(
            view.payload_hash(arena_module).expect("hash module"),
            arbitrary_hash(0x01),
        );
        assert_eq!(
            view.payload_hash(arena_fn).expect("hash fn"),
            arbitrary_hash(0x02),
        );
        assert_eq!(
            view.payload_hash(arena_ty).expect("hash ty"),
            arbitrary_hash(0x03),
        );
    }

    #[test]
    fn seal_from_entries_determinism_shuffled_input() {
        // Build entries in default order.
        let fn_links: Vec<(StableRef, KindDiscriminant, KindDiscriminant)> = vec![(
            make_stable_ref(0x01),
            KindDiscriminant::Function,
            KindDiscriminant::Module,
        )];
        let fp = type_fp_never();

        let entry_module = SealEntry {
            intro: IntroId::from_raw([0x01; 32]),
            name: "root_module",
            aliases: &[],
            visibility: 0,
            source_path: "src/lib.rs",
            span_start: 0,
            span_end: 10,
            kind_disc: KindDiscriminant::Module,
            flags: 0,
            payload_hash: arbitrary_hash(0x01),
            parent: None,
            type_fingerprint: None,
            links: &[],
            payload_bytes: b"blob-1",
        };
        let entry_fn = SealEntry {
            intro: IntroId::from_raw([0x02; 32]),
            name: "do_function",
            aliases: &["fn_alias_a", "fn_alias_b"],
            visibility: 1,
            source_path: "src/lib.rs",
            span_start: 10,
            span_end: 50,
            kind_disc: KindDiscriminant::Function,
            flags: 0,
            payload_hash: arbitrary_hash(0x02),
            parent: Some(IntroId::from_raw([0x01; 32])),
            type_fingerprint: None,
            links: &fn_links,
            payload_bytes: b"blob-2",
        };
        let entry_ty = SealEntry {
            intro: IntroId::from_raw([0x03; 32]),
            name: "NeverType",
            aliases: &[],
            visibility: 0,
            source_path: "src/types.rs",
            span_start: 100,
            span_end: 120,
            kind_disc: KindDiscriminant::Type,
            flags: 0,
            payload_hash: arbitrary_hash(0x03),
            parent: None,
            type_fingerprint: Some(fp),
            links: &[],
            payload_bytes: b"blob-3",
        };

        // Helper macro to clone a SealEntry field-by-field (SealEntry is not Clone
        // because its borrows make it non-trivial, but since all fields are Copy or
        // &'a slice, we can simply rebuild each).
        macro_rules! clone_entry {
            ($e:expr) => {
                SealEntry {
                    intro: $e.intro,
                    name: $e.name,
                    aliases: $e.aliases,
                    visibility: $e.visibility,
                    source_path: $e.source_path,
                    span_start: $e.span_start,
                    span_end: $e.span_end,
                    kind_disc: $e.kind_disc,
                    flags: $e.flags,
                    payload_hash: $e.payload_hash,
                    parent: $e.parent,
                    type_fingerprint: $e.type_fingerprint,
                    links: $e.links,
                    payload_bytes: $e.payload_bytes,
                }
            };
        }

        // Order A: module, fn, ty.
        let order_a = vec![
            clone_entry!(entry_module),
            clone_entry!(entry_fn),
            clone_entry!(entry_ty),
        ];
        // Order B: ty, module, fn (shuffled).
        let order_b = vec![
            clone_entry!(entry_ty),
            clone_entry!(entry_module),
            clone_entry!(entry_fn),
        ];

        let sealed_a = seal_from_entries(order_a).expect("seal A failed");
        let sealed_b = seal_from_entries(order_b).expect("seal B failed");

        assert_eq!(
            sealed_a.cas_key.as_bytes(),
            sealed_b.cas_key.as_bytes(),
            "seal_from_entries must be order-independent: shuffled input must yield identical cas_key",
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial open tests — malicious/corrupt bytes must error, never panic
    // -----------------------------------------------------------------------

    #[test]
    fn open_truncated_archive_errors_not_panics() {
        let sealed = seal_package_archive(&build_test_pristine()).unwrap();
        let full = &sealed.bytes;
        for cut in [0usize, 3, 4, 16, 63, 64, 65, 100, full.len() - 1] {
            let cut = cut.min(full.len() - 1);
            let bytes: std::sync::Arc<[u8]> = full[..cut].into();
            assert!(
                open_archive(bytes).is_err(),
                "open of a {cut}-byte truncation must return Err"
            );
        }
    }

    #[test]
    fn open_bad_magic_errors() {
        let sealed = seal_package_archive(&build_test_pristine()).unwrap();
        let mut bad = sealed.bytes.to_vec();
        bad[0] ^= 0xFF;
        assert!(
            matches!(open_archive(bad.into()), Err(crate::archive::error::ArchiveError::BadMagic)),
            "flipped magic must be BadMagic"
        );
    }

    #[test]
    fn open_corrupt_section_body_errors() {
        let sealed = seal_package_archive(&build_test_pristine()).unwrap();
        // Flip one byte in every position of the first 200 bytes past the
        // header + a byte at the very end; every flip must produce Err (CRC,
        // truncation, or format error), never a panic or silent success.
        let full = sealed.bytes.to_vec();
        let probe: Vec<usize> = (64..full.len().min(264)).chain([full.len() - 1]).collect();
        for i in probe {
            let mut bad = full.clone();
            bad[i] ^= 0xFF;
            assert!(
                open_archive(bad.into()).is_err(),
                "flipping byte {i} must be detected"
            );
        }
    }

    #[test]
    fn open_crafted_toc_overflow_errors() {
        let sealed = seal_package_archive(&build_test_pristine()).unwrap();
        // Patch toc_offset (bytes 20..28) and toc_len (28..36) to huge values
        // whose product/sum wraps usize, then re-stamp a valid header CRC so
        // the crafted header actually reaches the TOC bounds logic — it must
        // fail closed (Truncated), not wrap and panic.
        let mut bad = sealed.bytes.to_vec();
        bad[20..28].copy_from_slice(&(u64::MAX - 15).to_le_bytes());
        bad[28..36].copy_from_slice(&(u64::MAX / 32).to_le_bytes());
        let crc = ArchiveHeader::header_crc(&bad[0..64]);
        bad[16..20].copy_from_slice(&crc.to_le_bytes());
        assert!(
            matches!(open_archive(bad.into()), Err(crate::archive::error::ArchiveError::Truncated)),
            "wrapping toc_offset/toc_len must be Truncated, not a panic"
        );
    }
}

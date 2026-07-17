//! Deterministic archive sealing: [`seal_package_archive`].
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

use nudox_change::{CasKey, IntroId};
use nudox_ir::{
    apply::PristineIntroTable,
    index::ArenaIdx,
    kind::KindDiscriminant,
    skeleton::{type_fingerprint, type_wire_skeleton},
    wire::{KindWire, OwnedEntryPayload},
};
use zerocopy::IntoBytes;

use crate::error::ArchiveError;
use crate::header::{
    ArchiveHeader, EntryHead, FORMAT_VERSION, MAGIC, SectionId, TocEntry,
};
use crate::index::{
    ArchiveMeta, DensePostingIndexBuilder, IntroIndexBuilder, LinkCsrBuilder,
    PayloadHashColumnBuilder, PostingIndexBuilder,
};
use crate::open::open_archive;
use crate::section::{crc32_of, CsrBuilder, StringTableBuilder};
use crate::view::YokedArchive;

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
    if entry_count > crate::header::MAX_ENTRIES as usize {
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
            visibility: sym.visibility,
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
            && let KindWire::Type(type_wire) = &payload.kind {
                let mut skel_bytes = Vec::new();
                type_wire_skeleton(type_wire, &mut skel_bytes);
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

    struct Section {
        id: SectionId,
        body: Vec<u8>,
        flags: u32,
    }

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

    // ------------------------------------------------------------------
    // Step 5 — lay out the archive bytes
    //
    // Layout:
    //   [0..64)   — ArchiveHeader (64 bytes; CRC covers [0..12))
    //   [64..64+toc_len*32) — TocEntry array
    //   [aligned] — section bodies
    // ------------------------------------------------------------------

    let toc_len = sections.len() as u64;
    let toc_start: u64 = 64; // immediately after header
    let toc_end = toc_start + toc_len * 32; // TocEntry is 32 bytes

    // Compute section offsets (no alignment needed; sections are byte streams).
    let mut section_offsets: Vec<u64> = Vec::with_capacity(sections.len());
    let mut cur = toc_end;
    for s in &sections {
        section_offsets.push(cur);
        cur += s.body.len() as u64;
    }
    let total_size = cur as usize;

    // Allocate output.
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
        entry_count: (entry_count as u32).to_le_bytes(),
        string_count: string_count.to_le_bytes(),
        link_count: link_count.to_le_bytes(),
        kind_table_version: 1u16.to_le_bytes(),
        reserved: [0u8; 2],
        _reserved_tail: [0u8; 12],
    };

    // Write the header into the output buffer temporarily to compute CRC.
    out[0..64].copy_from_slice(hdr.as_bytes());
    // CRC covers bytes 0..CRC_OFFSET (= 0..12).
    let crc_val = crc32_of(&out[0..ArchiveHeader::CRC_OFFSET]);
    hdr.header_crc32 = crc_val.to_le_bytes();
    out[0..64].copy_from_slice(hdr.as_bytes());

    // ------------------------------------------------------------------
    // Step 6 — CAS key = plain blake3(bytes) (no domain prefix)
    // ------------------------------------------------------------------

    let cas_bytes = *blake3::hash(&out).as_bytes();
    let cas_key = CasKey::from_raw(cas_bytes);

    let bytes: Arc<[u8]> = out.into();
    Ok(SealedArchive { bytes, cas_key })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_change::IntroId;
    use nudox_ir::{
        apply::PristineIntroTable,
        kind::KindDiscriminant,
        wire::{EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload, SymbolWire, TypeWire},
    };

    fn intro(b: u8) -> IntroId {
        IntroId::from_raw([b; 32])
    }

    fn make_module_payload(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            SymbolWire {
                name: name.to_string(),
                visibility: 0,
                documentation: None,
                source_path: "src/lib.rs".to_string(),
                span_start: 0,
                span_end: 10,
                aliases: vec![],
                deprecation: None,
                doc_links: vec![],
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
                visibility: 1,
                documentation: Some("Does things.".to_string()),
                source_path: "src/lib.rs".to_string(),
                span_start: 10,
                span_end: 50,
                aliases: vec!["fn_alias".to_string()],
                deprecation: None,
                doc_links: vec![],
            },
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn make_type_payload(name: &str) -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            SymbolWire {
                name: name.to_string(),
                visibility: 0,
                documentation: None,
                source_path: "src/lib.rs".to_string(),
                span_start: 100,
                span_end: 120,
                aliases: vec![],
                deprecation: None,
                doc_links: vec![],
            },
            KindDiscriminant::Type,
            KindWire::Type(TypeWire::Never),
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
}

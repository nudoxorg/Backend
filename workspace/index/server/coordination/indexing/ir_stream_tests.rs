use super::{super::identity_toolchain, *};
use ir_vcs::protocol::{
    BodyWire, FailureKindWire, FrameWriter, IR_STREAM_VERSION, ProducerId, StreamFrame,
};

// ── W1: ingest_ir_bytes must distinguish producer breakage from a
// genuinely empty package ──────────────────────────────────────────────

fn test_package() -> heart::PackageId {
    heart::PackageId::from_uuid(uuid::Uuid::from_bytes([7u8; 16]))
}

fn hello_frame() -> StreamFrame {
    StreamFrame::Hello {
        version: IR_STREAM_VERSION,
        job: heart::content::JobKey::derive(b"producer", b"toolchain", b"source", b"deplock"),
        producer: ProducerId::from_static("test-producer"),
    }
}

fn encode_frames(frames: &[StreamFrame]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut w = FrameWriter::new(&mut buf);
    for f in frames {
        w.write_frame(f).expect("encode frame");
    }
    buf
}

/// A well-behaved producer that honestly emits zero symbols still closes
/// with `Finish`. This must succeed cleanly — it is the case the fix must
/// NOT break.
#[test]
fn w1_clean_finish_with_zero_symbols_is_not_degraded() {
    let bytes = encode_frames(&[hello_frame(), StreamFrame::Finish {
        emitted: 0,
        producer_digest: heart::content::ContentHash::from_bytes([0u8; 32]),
    }]);
    let mut builder = BlobBuilder::new(
        test_package(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    let outcome = ingest_ir_bytes(&mut builder, &bytes, &[]);
    assert_eq!(outcome.degraded_reason, None);
    assert!(outcome.identifiers.is_empty());
}

/// Failing-first case (documents the pre-fix bug, now asserts the fix):
/// a stream that ends right after `Hello` — no `Finish`, no `Abort` —
/// is a truncated producer, not a legitimate empty package.
#[test]
fn w1_truncated_after_hello_is_degraded() {
    let bytes = encode_frames(&[hello_frame()]);
    let mut builder = BlobBuilder::new(
        test_package(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    let outcome = ingest_ir_bytes(&mut builder, &bytes, &[]);
    assert!(
        outcome.degraded_reason.is_some(),
        "a stream truncated before Finish must be reported as degraded, not silent success"
    );
}

/// An explicit `Abort` frame is a producer-signaled failure.
#[test]
fn w1_abort_frame_is_degraded() {
    let bytes = encode_frames(&[hello_frame(), StreamFrame::Abort {
        failure: FailureKindWire::Internal,
        message: "producer crashed mid-parse".to_owned(),
    }]);
    let mut builder = BlobBuilder::new(
        test_package(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    let outcome = ingest_ir_bytes(&mut builder, &bytes, &[]);
    let reason = outcome.degraded_reason.expect("Abort must degrade the job");
    assert!(reason.contains("aborted"), "reason: {reason}");
}

/// A corrupted postcard payload after a valid Hello must degrade, not
/// silently truncate to "whatever decoded before the corruption".
#[test]
fn w1_corrupt_frame_after_hello_is_degraded() {
    let mut bytes = encode_frames(&[hello_frame()]);
    // Append a frame whose declared length is 4 bytes of bytes that are
    // not a valid postcard-encoded `StreamFrame`.
    bytes.extend_from_slice(&4u32.to_le_bytes());
    bytes.extend_from_slice(&[0xff, 0xff, 0xff, 0xff]);
    let mut builder = BlobBuilder::new(
        test_package(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    let outcome = ingest_ir_bytes(&mut builder, &bytes, &[]);
    let reason = outcome
        .degraded_reason
        .expect("corrupt frame must degrade the job");
    assert!(reason.contains("decode error"), "reason: {reason}");
}

/// A stream with no valid Hello at all (garbage from byte 0) must
/// degrade rather than silently attach an empty-but-"successful" IR
/// section.
#[test]
fn w1_missing_hello_is_degraded() {
    let bytes = vec![0xde, 0xad, 0xbe, 0xef, 0x00, 0x01, 0x02];
    let mut builder = BlobBuilder::new(
        test_package(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    let outcome = ingest_ir_bytes(&mut builder, &bytes, &[]);
    assert!(outcome.degraded_reason.is_some());
    assert!(outcome.identifiers.is_empty());
}

fn module_entry(name: &str, docs: &str, span_start: u32) -> ir_vcs::protocol::WireEntry {
    use ir::{
        change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        entry::Visibility,
        kind::KindDiscriminant,
    };
    use ir_vcs::wire::{EntryPayloadFlags, KindWire, ModuleWire, OwnedEntryPayload, SymbolWire};

    let symbol = SymbolWire {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: Some(docs.to_owned()),
        source_path: "src/lib.rs".to_owned(),
        span_start,
        span_end: span_start + 8,
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
        attrs: Vec::new(),
        cfg: None,
    };
    ir_vcs::protocol::WireEntry {
        stable: StableRef::new(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("acme")),
            IntroId::from_domain("nudox.test.intro", name.as_bytes()),
        ),
        payload: OwnedEntryPayload::sealed(
            symbol,
            KindDiscriminant::Module,
            KindWire::Module(ModuleWire {}),
            EntryPayloadFlags::default(),
        ),
        parent: None,
        links: Vec::new(),
    }
}

fn finish(emitted: u64) -> StreamFrame {
    StreamFrame::Finish {
        emitted,
        producer_digest: heart::content::ContentHash::from_bytes([0u8; 32]),
    }
}

fn ingest_symbols(
    entries: &[ir_vcs::protocol::WireEntry],
    prior: &[crate::frontier::ir::IrEntryKey],
) -> (
    Option<heart::content::ContentHash>,
    Vec<crate::blob::creation::PendingSection>,
    IrIngestOutcome,
) {
    let frames = [
        hello_frame(),
        StreamFrame::Symbols {
            batch: entries.to_vec(),
        },
        finish(entries.len() as u64),
    ];
    let bytes = encode_frames(&frames);
    let mut builder = BlobBuilder::new(
        test_package(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    builder
        .push_file(
            "src/lib.rs".into(),
            bytes::Bytes::from_static(b"fn main() {}\n"),
        )
        .expect("file");
    let outcome = ingest_ir_bytes(&mut builder, &bytes, prior);
    assert_eq!(outcome.degraded_reason, None);
    let (manifest, sections) = builder.finalize().expect("finalize");
    (manifest.root_ref, sections, outcome)
}

#[test]
fn generation_root_skips_an_unchanged_payload_and_a_pure_move() {
    let alpha = module_entry("alpha", "The alpha module.", 0);
    let beta = module_entry("beta", "The beta module.", 20);
    let (first_root, first_sections, first) = ingest_symbols(&[alpha.clone(), beta.clone()], &[]);
    assert!(first_root.is_some());
    assert_eq!(first.generation.len(), 2);
    let root_bytes = first_sections
        .iter()
        .find(|section| Some(section.hash) == first_root)
        .expect("root section")
        .bytes
        .clone();
    let decoded = ir::generation::GenerationRoot::decode(&root_bytes).expect("decode root");
    let alpha_hash = decoded
        .entries
        .iter()
        .find(|row| row.span.start == 0)
        .expect("alpha starts at 0")
        .content
        .as_bytes();

    let moved = module_entry("alpha", "The alpha module.", 400);
    let rewritten = module_entry("beta", "The beta module, rewritten.", 20);
    let (second_root, second_sections, second) =
        ingest_symbols(&[moved, rewritten], &first.generation);
    let queued = second.generation.iter().filter(|key| {
        let hash = heart::content::ContentHash::from_bytes(key.content_hash);
        second_sections.iter().any(|section| section.hash == hash)
    });
    assert_eq!(
        queued.count(),
        1,
        "a move must not queue a payload; a rewrite must"
    );
    let second_bytes = second_sections
        .iter()
        .find(|section| Some(section.hash) == second_root)
        .expect("second root")
        .bytes
        .clone();
    let decoded = ir::generation::GenerationRoot::decode(&second_bytes).expect("decode");
    let moved_row = decoded
        .entries
        .iter()
        .find(|row| row.content.as_bytes() == alpha_hash)
        .expect("moved row keeps its content hash");
    assert_eq!(moved_row.span.start, 400);
    assert!(
        second
            .generation
            .iter()
            .any(|key| key.content_hash == *alpha_hash)
    );
}

#[tokio::test]
async fn prior_keys_loaded_from_the_store_skip_unchanged_payloads() {
    use crate::ecosystem::PackageNameExt;
    use heart::{PackageName, PackageVersion, RegistryOrigin, connection::Connect};
    use std::sync::Arc;

    let coordinates = crate::package::Coordinates {
        origin: RegistryOrigin::CratesIo,
        name: PackageName::new(crate::ecosystem::Language::Rust, "acme").expect("name"),
        version: PackageVersion::try_from((crate::ecosystem::Language::Rust, "1.0.0"))
            .expect("version"),
    };
    let alpha = module_entry("alpha", "The alpha module.", 0);
    let beta = module_entry("beta", "The beta module.", 20);
    let frames = [
        hello_frame(),
        StreamFrame::Symbols {
            batch: vec![alpha.clone(), beta.clone()],
        },
        finish(2),
    ];
    let bytes = encode_frames(&frames);
    let mut builder = BlobBuilder::new(
        coordinates.id(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    builder
        .push_file(
            "src/lib.rs".into(),
            bytes::Bytes::from_static(b"fn main() {}\n"),
        )
        .expect("file");

    let backend: Arc<dyn object_store::ObjectStore> =
        Arc::new(object_store::memory::InMemory::new());
    let store = crate::cas::Store::new(backend);
    let store = Connect::connect(store).await.expect("connect");
    let before = prior_entry_keys(&store, &coordinates).await;
    assert!(
        before.is_empty(),
        "a package with no manifest has an empty prior"
    );

    let outcome = ingest_ir_bytes(&mut builder, &bytes, &before);
    assert_eq!(outcome.degraded_reason, None);
    assert_eq!(outcome.generation.len(), 2);
    let (manifest, sections) = builder.finalize().expect("finalize");
    crate::blob::emit::emit(&store, manifest, sections)
        .await
        .expect("emit");

    let prior = prior_entry_keys(&store, &coordinates).await;
    assert_eq!(prior.len(), 2);
    for key in &outcome.generation {
        assert!(
            prior
                .iter()
                .any(|loaded| loaded.intro_id == key.intro_id
                    && loaded.content_hash == key.content_hash),
            "loaded prior must match the root that was stored"
        );
    }

    let mut again = BlobBuilder::new(
        coordinates.id(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    again
        .push_file(
            "src/lib.rs".into(),
            bytes::Bytes::from_static(b"fn main() {}\n"),
        )
        .expect("file");
    let second = ingest_ir_bytes(&mut again, &bytes, &prior);
    assert_eq!(second.degraded_reason, None);
    let (_manifest, sections) = again.finalize().expect("finalize");
    let queued = second.generation.iter().filter(|key| {
        let hash = heart::content::ContentHash::from_bytes(key.content_hash);
        sections.iter().any(|section| section.hash == hash)
    });
    assert_eq!(queued.count(), 0, "unchanged payloads must not be queued");
}

fn semantic_module(name: &str, docs: &str, span: std::ops::Range<usize>) -> ir::entry::Entry {
    use ir::{
        entry::{Entry, Node, Symbol, Visibility},
        index::RawRef,
        kind::Kind,
        kinds::Module,
    };
    Entry::new(
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: docs.to_owned(),
            source: std::path::PathBuf::from("src/lib.rs"),
            span,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        },
        Node::build(None::<RawRef>, []),
        Kind::Module(Module),
    )
}

fn queue_count(
    keys: &[crate::frontier::ir::IrEntryKey],
    sections: &[crate::blob::creation::PendingSection],
) -> usize {
    keys.iter()
        .filter(|key| {
            let hash = heart::content::ContentHash::from_bytes(key.content_hash);
            sections.iter().any(|section| section.hash == hash)
        })
        .count()
}

#[test]
fn table_generation_root_skips_a_pure_move() {
    use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};

    let package = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("acme"));
    let alpha = IntroId::from_domain("nudox.test.intro", b"alpha");
    let beta = IntroId::from_domain("nudox.test.intro", b"beta");
    let mut table = ir::apply::PristineIntroTable::new();
    table.insert_live(
        alpha,
        semantic_module("alpha", "The alpha module.", 0..8),
        None,
    );
    table.insert_live(
        beta,
        semantic_module("beta", "The beta module.", 20..28),
        Some(alpha),
    );

    let mut builder = BlobBuilder::new(
        test_package(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    builder
        .push_file(
            "src/lib.rs".into(),
            bytes::Bytes::from_static(b"fn main() {}\n"),
        )
        .expect("file");
    let keys = attach_table_generation_root(&mut builder, &table, &package, &[]).expect("root");
    super::super::set_empty_ir_section(&mut builder);
    let _ = builder.set_references(&crate::server::registry::blob::ReferenceSet {
        by_file: Vec::new(),
    });
    let (manifest, sections) = builder.finalize().expect("finalize");
    assert!(manifest.root_ref.is_some());
    assert_eq!(queue_count(&keys, &sections), 2);

    let mut moved = ir::apply::PristineIntroTable::new();
    moved.insert_live(
        alpha,
        semantic_module("alpha", "The alpha module.", 400..408),
        None,
    );
    moved.insert_live(
        beta,
        semantic_module("beta", "The beta module, rewritten.", 20..28),
        Some(alpha),
    );
    let mut again = BlobBuilder::new(
        test_package(),
        identity_toolchain(crate::ecosystem::Language::Rust),
    );
    again
        .push_file(
            "src/lib.rs".into(),
            bytes::Bytes::from_static(b"fn main() {}\n"),
        )
        .expect("file");
    let second = attach_table_generation_root(&mut again, &moved, &package, &keys).expect("root");
    super::super::set_empty_ir_section(&mut again);
    let _ = again.set_references(&crate::server::registry::blob::ReferenceSet {
        by_file: Vec::new(),
    });
    let (_manifest, sections) = again.finalize().expect("finalize");
    assert_eq!(queue_count(&second, &sections), 1);
    let root_bytes = sections
        .iter()
        .find(|section| section.hash == _manifest.root_ref.expect("root"))
        .expect("root section")
        .bytes
        .clone();
    let decoded = ir::generation::GenerationRoot::decode(&root_bytes).expect("decode");
    let alpha_row = decoded
        .entries
        .iter()
        .find(|row| row.intro == alpha)
        .expect("alpha");
    assert_eq!(alpha_row.span.start, 400);
    assert_eq!(alpha_row.parent, None);
    let beta_row = decoded
        .entries
        .iter()
        .find(|row| row.intro == beta)
        .expect("beta");
    assert_eq!(beta_row.parent, Some(alpha));
}

// ── In-process path: real reference edges flow from the sealed table ──
//
// The end-to-end sanity check for the macOS/dev fix: a sealed
// `PristineIntroTable` (what `nudox_languages::produce` returns in-process)
// with real cross-symbol references must yield a NON-EMPTY `ReferenceSet`
// with real Local *and* External edges — the section that was previously
// attached empty, blinding `Target::Usages` for every macOS-indexed package.
//
// Filterable in isolation (`coordination::indexing::ir_stream::tests::
// in_process_sealed_table_yields_real_reference_edges`); it never touches
// `index::pack`, so the `--features server` zstd duplicate-symbol clash
// never runs.
#[test]
fn in_process_sealed_table_yields_real_reference_edges() {
    use crate::server::registry::blob::RefTarget;
    use ir::{
        build::{
            EcosystemId, Impl, Lowering, PackageId, PackageLineageId, PackageName, Record, Symbol,
            Type, Visibility,
        },
        foreign::{ForeignKey, Unlinked},
        index::Ref,
    };

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::from("src/lib.rs"),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture"));
    let core = PackageLineageId::new(EcosystemId::new("rust-sysroot"), PackageName::new("core"));

    // `struct Widget;` + `impl Clone for Widget` — the impl's `self_ty`
    // references the same-package `Widget` (→ a `Ref::Intro`, i.e. a Local
    // edge post-seal), and its `of` names a cross-package trait (→ a
    // `Ref::Foreign`, i.e. an External edge under `Unlinked`).
    let mut low: Lowering<&'static str> = Lowering::new(PackageId::path("fixture"), sym("fixture"));
    let self_ref = low.refer::<Record>("Widget");
    low.declare("Widget", None, sym("Widget"), Record::builder().build());
    let of = low.nominal_import(ForeignKey::in_package(
        core.clone(),
        "core::clone::Clone",
        "Clone",
    ));
    low.declare(
        "impl#clone",
        None,
        sym("impl Clone for Widget"),
        Impl::builder()
            .of(of)
            .self_ty(Type::Nominal(self_ref.into_raw()))
            .build(),
    );

    let table = low
        .finish()
        .expect("lowering must succeed")
        .seal(&lineage, &Unlinked)
        .table;

    let ref_set =
        build_reference_set_from_table(&table, std::path::Path::new(""), Some("cargo:fixture"));

    let edges: Vec<&crate::server::registry::blob::Reference> = ref_set
        .by_file
        .iter()
        .flat_map(|f| f.references.iter())
        .collect();

    assert!(
        !edges.is_empty(),
        "a sealed table with cross-symbol type references must yield a \
         non-empty reference set — this is the section the in-process path \
         used to leave empty, blinding Target::Usages on macOS"
    );
    assert!(
        edges
            .iter()
            .any(|r| matches!(&r.target, RefTarget::Local(_))),
        "the impl's self_ty referencing same-package `Widget` must be a \
         Local edge; got {edges:?}"
    );
    assert!(
        edges.iter().any(|r| matches!(
            &r.target,
            RefTarget::External { dependency, .. } if dependency == "rust-sysroot:core"
        )),
        "the impl's foreign `Clone` trait must be an External edge naming \
         its owning package; got {edges:?}"
    );
    // And the whole set must round-trip the on-disk codec the audit uses.
    let encoded = ref_set.encode().expect("reference set must encode");
    let decoded = crate::server::registry::blob::ReferenceSet::decode(&encoded)
        .expect("reference set must decode (matches save::blobs audit)");
    assert_eq!(decoded.by_file.len(), ref_set.by_file.len());
}

// ── reviewer adversarial tests for the in-process reference seam ──

#[test]
fn adversarial_table_with_no_type_references_yields_empty_but_valid_set() {
    // The empty-vs-degraded boundary (W1): a package whose declarations
    // name no other types (a bare fieldless struct, no impl) is honest
    // ABSENCE, not breakage. build_reference_set_from_table must return an
    // empty set that still encodes/decodes, so the caller reports a clean
    // success — NOT a degraded job, and NOT a false "has usages".
    use ir::{
        build::{
            EcosystemId, Lowering, PackageId, PackageLineageId, PackageName, Record, Symbol,
            Visibility,
        },
        foreign::Unlinked,
    };

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::from("src/lib.rs"),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture"));
    let mut low: Lowering<&'static str> = Lowering::new(PackageId::path("fixture"), sym("fixture"));
    low.declare("Lonely", None, sym("Lonely"), Record::builder().build());
    let table = low
        .finish()
        .expect("lowering must succeed")
        .seal(&lineage, &Unlinked)
        .table;

    let ref_set =
        build_reference_set_from_table(&table, std::path::Path::new(""), Some("cargo:fixture"));
    let edges = ref_set
        .by_file
        .iter()
        .flat_map(|f| f.references.iter())
        .count();
    assert_eq!(
        edges, 0,
        "a package that references no other types must yield zero usage edges (honest absence)"
    );
    let encoded = ref_set.encode().expect("empty set still encodes");
    let decoded =
        crate::server::registry::blob::ReferenceSet::decode(&encoded).expect("empty set decodes");
    assert!(decoded.by_file.iter().all(|f| f.references.is_empty()));
}

#[test]
fn adversarial_reference_edges_carry_exact_target_and_degenerate_spans() {
    // Tighter contract than the sanity test: the External edge's `path`
    // must be the foreign key's canonical path (the durable resolver join
    // key), every edge's span must be the documented degenerate 0..0 (the
    // sealed table carries no occurrence span), and encode/decode must
    // preserve the exact edge count (no silent drop in the codec).
    use crate::server::registry::blob::RefTarget;
    use ir::{
        build::{
            EcosystemId, Impl, Lowering, PackageId, PackageLineageId, PackageName, Record, Symbol,
            Type, Visibility,
        },
        foreign::{ForeignKey, Unlinked},
    };

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: std::path::PathBuf::from("src/lib.rs"),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("fixture"));
    let core = PackageLineageId::new(EcosystemId::new("rust-sysroot"), PackageName::new("core"));
    let mut low: Lowering<&'static str> = Lowering::new(PackageId::path("fixture"), sym("fixture"));
    let self_ref = low.refer::<Record>("Widget");
    low.declare("Widget", None, sym("Widget"), Record::builder().build());
    let of = low.nominal_import(ForeignKey::in_package(
        core.clone(),
        "core::clone::Clone",
        "Clone",
    ));
    low.declare(
        "impl#clone",
        None,
        sym("impl Clone for Widget"),
        Impl::builder()
            .of(of)
            .self_ty(Type::Nominal(self_ref.into_raw()))
            .build(),
    );
    let table = low
        .finish()
        .expect("lowering must succeed")
        .seal(&lineage, &Unlinked)
        .table;

    let ref_set =
        build_reference_set_from_table(&table, std::path::Path::new(""), Some("cargo:fixture"));
    let edges: Vec<&crate::server::registry::blob::Reference> = ref_set
        .by_file
        .iter()
        .flat_map(|f| f.references.iter())
        .collect();

    // Exactly one Local (self_ty → same-package Widget) and one External
    // (foreign Clone trait) — no phantom duplicates from the Node tree.
    assert_eq!(
        edges
            .iter()
            .filter(|r| matches!(r.target, RefTarget::Local(_)))
            .count(),
        1,
        "exactly one Local edge; got {edges:?}"
    );
    let external: Vec<_> = edges
        .iter()
        .filter_map(|r| match &r.target {
            RefTarget::External { path, dependency } => Some((path.clone(), dependency.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        external.len(),
        1,
        "exactly one External edge; got {edges:?}"
    );
    assert_eq!(
        external[0],
        (
            "core::clone::Clone".to_owned(),
            "rust-sysroot:core".to_owned()
        ),
        "External edge must carry the foreign key's canonical path + owning package"
    );
    // Documented degeneracy: no per-reference span from a sealed table.
    assert!(
        edges.iter().all(|r| r.span_start == 0 && r.span_end == 0),
        "sealed-table references have degenerate 0..0 spans"
    );
    // Codec must preserve every edge.
    let round =
        crate::server::registry::blob::ReferenceSet::decode(&ref_set.encode().expect("encode"))
            .expect("decode");
    assert_eq!(
        round
            .by_file
            .iter()
            .map(|f| f.references.len())
            .sum::<usize>(),
        edges.len(),
        "encode/decode must preserve the exact edge count"
    );
}

// ── build_occurrences_from_bodies: the cage-path usage-query projection ──
//
// The same Bodies-frame oracle facts `build_reference_set_from_bodies`
// projects into `Reference`s (for the reference-set blob section) must
// also project into `ir::vocab::Occurrence`s (for the usage-query scope
// `indexing/mod.rs` loads via `load_usage_scope`). These tests exercise
// that second projection directly, in isolation from the stream decode.

fn occ_pkg() -> ir::change::PackageLineageId {
    ir::change::PackageLineageId::new(
        ir::change::EcosystemId::new("cargo"),
        ir::change::PackageName::new("fixture"),
    )
}

fn occ_intro(n: u8) -> ir::change::IntroId {
    ir::change::IntroId::from_raw([n; 32])
}

fn oracle_body(oracle: ir::body::OracleBody) -> ir::body::BodyEmbed {
    ir::body::BodyEmbed::Present(ir::body::BodyFacts {
        language: ir::body::Language::Rust,
        tree: ir::body::TreesitterBody::default(),
        oracle,
        merge: ir::body::BodyMergeNote::oracle_only(),
    })
}

/// A caller's body with a graph-worthy oracle call to a callee target
/// projects into exactly one `Occurrence`, owned by the caller, with the
/// call's exact kind/confidence/span preserved — the content a `/usages`
/// query on the callee must return.
#[test]
fn caller_calling_callee_projects_one_occurrence_owned_by_caller() {
    use ir::{
        body::{OracleBody, OracleCall},
        change::StableRef,
        vocab::{Confidence, ReferenceKind, RelSpan},
    };

    let caller = occ_intro(2);
    let callee = StableRef::new(occ_pkg(), occ_intro(1));

    let bodies = vec![BodyWire {
        intro: caller,
        body: oracle_body(OracleBody {
            calls: vec![OracleCall {
                target: Some(callee.clone()),
                kind: ReferenceKind::FunctionCall,
                confidence: Confidence::Index,
                rel_span: RelSpan::new(4, 9),
            }],
            type_mentions: Vec::new(),
            reads_writes: Vec::new(),
        }),
    }];

    let occurrences = build_occurrences_from_bodies(&bodies);
    assert_eq!(occurrences.len(), 1, "one call, one occurrence");
    let (owner, occurrence) = &occurrences[0];
    assert_eq!(*owner, caller, "occurrence must be owned by the caller");
    assert_eq!(occurrence.target, callee, "target must be the callee");
    assert_eq!(occurrence.kind, ReferenceKind::FunctionCall);
    assert_eq!(occurrence.confidence, Confidence::Index);
    assert_eq!(occurrence.span, RelSpan::new(4, 9));
}

/// A call with no resolved target (tree-sitter name-only fact riding in
/// `OracleCall.target: None`) contributes no occurrence — there is no
/// stable cross-package identity to key a usages query on.
#[test]
fn unresolved_call_target_is_skipped() {
    use ir::{
        body::{OracleBody, OracleCall},
        vocab::{Confidence, ReferenceKind, RelSpan},
    };

    let bodies = vec![BodyWire {
        intro: occ_intro(2),
        body: oracle_body(OracleBody {
            calls: vec![OracleCall {
                target: None,
                kind: ReferenceKind::FunctionCall,
                confidence: Confidence::Oracle,
                rel_span: RelSpan::new(0, 3),
            }],
            type_mentions: Vec::new(),
            reads_writes: Vec::new(),
        }),
    }];

    assert!(build_occurrences_from_bodies(&bodies).is_empty());
}

/// A call resolved below the graph floor (`Confidence::Suffix`) is
/// excluded — same policy as the reference-set projection, and the same
/// floor `ReversePositionIndex::build` enforces on the read side.
#[test]
fn below_floor_confidence_call_is_skipped() {
    use ir::{
        body::{OracleBody, OracleCall},
        change::StableRef,
        vocab::{Confidence, ReferenceKind, RelSpan},
    };

    let bodies = vec![BodyWire {
        intro: occ_intro(2),
        body: oracle_body(OracleBody {
            calls: vec![OracleCall {
                target: Some(StableRef::new(occ_pkg(), occ_intro(1))),
                kind: ReferenceKind::FunctionCall,
                confidence: Confidence::Suffix,
                rel_span: RelSpan::new(0, 3),
            }],
            type_mentions: Vec::new(),
            reads_writes: Vec::new(),
        }),
    }];

    assert!(
        build_occurrences_from_bodies(&bodies).is_empty(),
        "Suffix confidence is below the graph floor and must not project"
    );
}

/// A type mention (no confidence field of its own — every mention the
/// oracle records is already resolved) projects at `Confidence::Oracle`
/// with `ReferenceKind::TypeReference`.
#[test]
fn type_mention_projects_at_oracle_confidence() {
    use ir::{
        body::{OracleBody, OracleTypeMention},
        change::StableRef,
        vocab::{Confidence, ReferenceKind, RelSpan},
    };

    let owner = occ_intro(5);
    let ty = StableRef::new(occ_pkg(), occ_intro(6));

    let bodies = vec![BodyWire {
        intro: owner,
        body: oracle_body(OracleBody {
            calls: Vec::new(),
            type_mentions: vec![OracleTypeMention {
                ty: ty.clone(),
                rel_span: RelSpan::new(2, 8),
            }],
            reads_writes: Vec::new(),
        }),
    }];

    let occurrences = build_occurrences_from_bodies(&bodies);
    assert_eq!(occurrences.len(), 1);
    let (owner_out, occurrence) = &occurrences[0];
    assert_eq!(*owner_out, owner);
    assert_eq!(occurrence.target, ty);
    assert_eq!(occurrence.kind, ReferenceKind::TypeReference);
    assert_eq!(occurrence.confidence, Confidence::Oracle);
    assert_eq!(occurrence.span, RelSpan::new(2, 8));
}

/// An `Absent` body contributes nothing (mirrors the reference-set
/// projection's handling of entries with no body facts at all).
#[test]
fn absent_body_contributes_no_occurrences() {
    let bodies = vec![BodyWire {
        intro: occ_intro(2),
        body: ir::body::BodyEmbed::Absent,
    }];
    assert!(build_occurrences_from_bodies(&bodies).is_empty());
}

#[test]
fn references_and_occurrences_count_the_same_oracle_edges() {
    use ir::{
        body::{OracleBody, OracleCall, OracleTypeMention},
        change::StableRef,
        vocab::{Confidence, ReferenceKind, RelSpan},
    };
    use std::collections::HashMap;

    let owner = occ_intro(2);
    let callee = StableRef::new(occ_pkg(), occ_intro(1));
    let ty = StableRef::new(occ_pkg(), occ_intro(3));
    let bodies = vec![BodyWire {
        intro: owner,
        body: oracle_body(OracleBody {
            calls: vec![
                OracleCall {
                    target: Some(callee),
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Index,
                    rel_span: RelSpan::new(4, 9),
                },
                OracleCall {
                    target: Some(StableRef::new(occ_pkg(), occ_intro(9))),
                    kind: ReferenceKind::FunctionCall,
                    confidence: Confidence::Suffix,
                    rel_span: RelSpan::new(0, 1),
                },
            ],
            type_mentions: vec![OracleTypeMention {
                ty,
                rel_span: RelSpan::new(2, 8),
            }],
            reads_writes: Vec::new(),
        }),
    }];
    let mut paths = HashMap::new();
    paths.insert(owner, "src/lib.rs".to_owned());
    let references = build_reference_set_from_bodies(&bodies, &paths, None);
    let occurrences = build_occurrences_from_bodies(&bodies);
    let ref_count: usize = references
        .by_file
        .iter()
        .map(|file| file.references.len())
        .sum();
    assert_eq!(ref_count, 2);
    assert_eq!(occurrences.len(), ref_count);
    assert_eq!(references.by_file[0].path.as_str(), "src/lib.rs");
}

#[test]
fn reference_section_bytes_follow_path_order() {
    use crate::server::registry::blob::{RefTarget, Reference};
    use smol_str::SmolStr;

    let edge = |path: &str| {
        (SmolStr::new(path), vec![Reference {
            target: RefTarget::Local("intro".to_owned()),
            span_start: 0,
            span_end: 1,
            kind: 2,
        }])
    };
    let mut forward = BTreeMap::new();
    forward.insert(edge("b.rs").0, edge("b.rs").1);
    let (path, refs) = edge("a.rs");
    forward.insert(path, refs);
    let mut backward = BTreeMap::new();
    let (path, refs) = edge("a.rs");
    backward.insert(path, refs);
    let (path, refs) = edge("b.rs");
    backward.insert(path, refs);
    let left = super::seal_references(forward);
    let right = super::seal_references(backward);
    assert_eq!(left.by_file[0].path.as_str(), "a.rs");
    assert_eq!(left.by_file[1].path.as_str(), "b.rs");
    assert_eq!(
        left.encode().expect("encode"),
        right.encode().expect("encode")
    );
}

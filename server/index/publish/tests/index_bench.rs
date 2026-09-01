//! Measures the complete sealed-snapshot ingest and query path over a deterministic 64-entity fixture.
//! Every printed numeric value is explicitly marked measured, analytic, or unknown.
//! The test is an executable correctness check before it is a performance observation.
//! Publish and reopen are inherently cold single-shot setup rows; their wall values are cold.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use core::mem::MaybeUninit;
use std::{process::Command, time::Instant};

use allocation_counter::{AllocationInfo, measure};
use compiler_driver::CompiledFragment;
use compiler_ir::{
    Atom, AtomInput, EntityKind, EntityRecord, FragmentView, PreparedFragment, SourceIdentity,
    TypeNode,
};
use compiler_ir_vocabulary::{AtomId, EntityId, TypeId};
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use compiler_vocabulary::{CompileRecipeFact, Language, NativeTool, Stage};
use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};
use server_index_build::{
    CoordinateLane, EntityEmbedder, EntityProjection, FacetCell, FacetTable,
    GraphProjectionScratch, IndexBuildScratch, NodeKind, NodeWorkspace, OwnerCount,
    ReferenceTarget, SEMANTIC_TYPE_REFERENCE_PROJECTION, VectorProjectionScratch, build,
    build_graph_projection, build_vector_projection, join_facets,
};
use server_index_core::{
    EntityDocumentId, ExactManifest, ExactOperation, ExactResolution, LexicalManifest,
    LexicalOperation, LexicalSnapshotHit, LexicalTopK,
};
use server_index_graph_vector::{
    GraphAuthority, Metric, ModelId, PartitionId, ValidatedVectorSegment, VectorAuthority,
    exact_vector_query,
};
use server_index_publish::{CompilationIndexScratch, seal_compilation_index};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use thiserror::Error;

const ENTITY_COUNT: usize = 64;
const ATOM_COUNT: usize = ENTITY_COUNT;
const TYPE_COUNT: usize = ENTITY_COUNT;
const FRAGMENT_BYTES: usize = 32_768;
const SCRATCH_BYTES: usize = 32_768;
const RUNS: usize = 21;
const WARMUPS: usize = 3;
const VECTOR_DIMENSION: usize = 8;
const VECTOR_POINTS: usize = 64;
const VECTOR_SEGMENTS: usize = 4;
const GRAPH_ROWS: usize = 64;
const GRAPH_EDGES: usize = 64;

#[derive(Debug, Error)]
enum BenchError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("limits: {0}")]
    Limits(#[from] server_journal::PublicationLimitError),
    #[error("publisher: {0}")]
    Publisher(#[from] server_journal::PublicationOpenError),
    #[error("shutdown: {0}")]
    Shutdown(#[from] server_journal::ShutdownError),
    #[error("prepare: {0}")]
    Prepare(#[from] compiler_ir::PrepareError),
    #[error("write: {0}")]
    Write(#[from] compiler_ir::WriteError),
    #[error("fragment: {0}")]
    Fragment(#[from] compiler_ir::FragmentError),
    #[error("publish: {0}")]
    Publish(#[source] Box<compiler_publication::PublishCompiledError>),
    #[error("open: {0}")]
    Open(#[source] Box<compiler_publication::OpenPublishedError>),
    #[error("opened fragment: {0}")]
    Opened(#[source] Box<compiler_publication::OpenedFragmentError>),
    #[error("graph: {0}")]
    Graph(#[from] server_index_build::GraphProjectionError),
    #[error("vector: {0}")]
    Vector(#[from] server_index_build::VectorProjectionError),
    #[error("seal: {0}")]
    Seal(#[from] server_index_publish::CompilationIndexError),
    #[error("manifests: {0}")]
    Manifest(String),
    #[error("query failed: {0}")]
    Query(String),
}

impl From<compiler_publication::PublishCompiledError> for BenchError {
    fn from(error: compiler_publication::PublishCompiledError) -> Self {
        Self::Publish(Box::new(error))
    }
}
impl From<compiler_publication::OpenPublishedError> for BenchError {
    fn from(error: compiler_publication::OpenPublishedError) -> Self {
        Self::Open(Box::new(error))
    }
}
impl From<compiler_publication::OpenedFragmentError> for BenchError {
    fn from(error: compiler_publication::OpenedFragmentError) -> Self {
        Self::Opened(Box::new(error))
    }
}

#[derive(Clone, Copy)]
struct Reading {
    wall: u128,
    alloc: AllocationInfo,
}

fn median(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    match values.get(values.len() / 2) {
        Some(value) => *value,
        None => 0,
    }
}

fn median_u64(values: &mut [u64]) -> u64 {
    values.sort_unstable();
    match values.get(values.len() / 2) {
        Some(value) => *value,
        None => 0,
    }
}

fn readings<F>(mut action: F) -> Reading
where
    F: FnMut(),
{
    for _ in 0..WARMUPS {
        action();
    }
    let mut walls = [0_u128; RUNS];
    let mut counts = [0_u64; RUNS];
    let mut bytes = [0_u64; RUNS];
    for (index, wall) in walls.iter_mut().enumerate() {
        let start = Instant::now();
        let allocation = measure(&mut action);
        *wall = start.elapsed().as_nanos();
        if let Some(slot) = counts.get_mut(index) {
            *slot = allocation.count_total;
        }
        if let Some(slot) = bytes.get_mut(index) {
            *slot = allocation.bytes_total;
        }
    }
    Reading {
        wall: median(&mut walls),
        alloc: AllocationInfo {
            count_total: median_u64(&mut counts),
            bytes_total: median_u64(&mut bytes),
            ..AllocationInfo::default()
        },
    }
}

fn rss() -> Result<u64, &'static str> {
    let output = Command::new("ps")
        .args(["-o", "rss=", "-p"])
        .arg(std::process::id().to_string())
        .output()
        .map_err(|_| "ps command failed")?;
    if !output.status.success() {
        return Err("ps exited unsuccessfully");
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| "ps output was not utf8")?;
    text.trim()
        .parse::<u64>()
        .map_err(|_| "ps rss was not an integer")
}

struct Embedder;
impl EntityEmbedder for Embedder {
    const DIMENSION: usize = VECTOR_DIMENSION;
    fn model(&self) -> ModelId {
        ModelId::new([9; 16])
    }
    fn metric(&self) -> Metric {
        Metric::SquaredEuclidean
    }
    fn embed(&self, fact: server_index_build::EntityFactView<'_>, coordinates: &mut [i16]) {
        if let Ok(entity) = i16::try_from(fact.entity.raw)
            && let Some(cell) = coordinates.first_mut()
        {
            *cell = entity;
        }
    }
}

fn source() -> SourceIdentity {
    SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"index-bench-source"),
        byte_len: 18,
    }
}
fn recipe() -> CompileRecipeFact {
    CompileRecipeFact::derive(
        Language::Rust,
        Stage::LowerIr,
        NativeTool::Rustc,
        source().identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"index-bench-toolchain"),
    )
}

fn fixture_fragment(storage: &mut [u8]) -> Result<FragmentView<'_>, BenchError> {
    let entities: [EntityRecord; ENTITY_COUNT] = core::array::from_fn(|ordinal| {
        let value = u32::from(u8::try_from(ordinal).map_or(0, |value| value));
        EntityRecord {
            semantic_type: TypeId::new(value),
            name: AtomId::new(value),
            kind: match ordinal % 3 {
                0 => EntityKind::Function,
                1 => EntityKind::Constant,
                _ => EntityKind::Record,
            },
        }
    });
    let types: [TypeNode; TYPE_COUNT] = core::array::from_fn(|ordinal| {
        let value = u32::from(u8::try_from((ordinal + 1) % ENTITY_COUNT).map_or(0, |value| value));
        TypeNode::Reference(TypeId::new(value))
    });
    let atoms: [AtomInput<'_>; ATOM_COUNT] = core::array::from_fn(|ordinal| AtomInput {
        bytes: match ordinal % 4 {
            0 => b"prefix-alpha",
            1 => b"prefix-beta",
            2 => b"prefix-gamma",
            _ => b"other-delta",
        },
    });
    let prepared = PreparedFragment::prepare(source(), recipe(), &entities, &types, &atoms)?;
    Ok(FragmentView::validate(prepared.write_into(storage)?)?)
}

#[test]
fn measured_sealed_snapshot_ingest_and_query() -> Result<(), BenchError> {
    let directory = std::env::temp_dir().join(format!("server-index-bench-{}", std::process::id()));
    std::fs::create_dir(&directory)?;
    let result = journey(&directory);
    let cleanup = std::fs::remove_dir_all(&directory);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

fn journey(directory: &std::path::Path) -> Result<(), BenchError> {
    let limits =
        PublicationLimits::new(core::num::NonZeroUsize::MIN, core::num::NonZeroUsize::MIN)?;
    let publisher = DurablePublisher::create(
        &PublicationPaths::in_directory(&directory.join("journal")),
        limits,
    )?;
    let mut fragment_storage = vec![0_u8; FRAGMENT_BYTES];
    let fragment = fixture_fragment(&mut fragment_storage)?;
    let compiled = [CompiledFragment {
        source: fragment.source,
        recipe: fragment.recipe,
        fragment,
    }];
    let mut manifest = [0_u8; 2048];
    let mut facts = [None; 1];
    let mut ordinals = [0_usize; 1];
    let mut locality = [0_u8; 2048];
    let mut binding = [0_u8; compiler_publication::binding::COMPILATION_BINDING_BYTES];
    let publish_start = Instant::now();
    let selected = publish_compiled(
        &publisher,
        &directory.join("artifacts"),
        &compiled,
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )?;
    let publish_wall = publish_start.elapsed().as_nanos();
    publisher.shutdown()?;
    let reopened = DurablePublisher::reopen(
        &PublicationPaths::in_directory(&directory.join("journal")),
        limits,
    )?;
    let mut open_manifest = [0_u8; 2048];
    let mut open_facts = [None; 1];
    let mut open_fragment = vec![0_u8; SCRATCH_BYTES];
    let mut open_locality = [0_u8; 2048];
    let opened = open_published(
        &reopened,
        &directory.join("artifacts"),
        OpenPublicationScratch {
            manifest_output: &mut open_manifest,
            manifest_facts: &mut open_facts,
            fragment_output: &mut open_fragment,
            locality_output: &mut open_locality,
        },
    )?
    .ok_or_else(|| BenchError::Query("published compilation absent".to_owned()))?;
    let mut cursor = opened.fragments();
    let fragment = cursor
        .next()
        .ok_or_else(|| BenchError::Query("fragment absent".to_owned()))??;
    assert_eq!(
        selected.publication.generation.pinned_root,
        opened.publication.generation.pinned_root
    );
    let fragment_view = &fragment.view;

    let mut first = BuildSlots::new();
    let prepared = build(&fragment, first.scratch())
        .map_err(|error| BenchError::Query(format!("build rejected: {error:?}")))?;
    assert_eq!(prepared.entities.len(), ENTITY_COUNT);
    let first_entity = prepared
        .entities
        .first()
        .ok_or_else(|| BenchError::Query("entity absent".to_owned()))?;
    let mut exact_ids = [prepared.exact.id];
    let mut lexical_ids = [prepared.lexical.id];
    let prepared_indexes = [prepared];
    let sealed = seal_compilation_index(
        opened,
        &prepared_indexes,
        CompilationIndexScratch {
            exact: &mut exact_ids,
            lexical: &mut lexical_ids,
        },
    )
    .map_err(|rejected| rejected.error)?;
    let exact_segments = [prepared.exact];
    let lexical_segments = [prepared.lexical];
    let exact_manifest = ExactManifest::new(sealed.snapshot, &exact_segments, &[])
        .map_err(|_| BenchError::Manifest("exact".to_owned()))?;
    let lexical_manifest = LexicalManifest::new(sealed.snapshot, &lexical_segments, &[])
        .map_err(|_| BenchError::Manifest("lexical".to_owned()))?;
    let graph_authority =
        GraphAuthority::new(sealed.snapshot.id, SEMANTIC_TYPE_REFERENCE_PROJECTION);
    let vector_authority = VectorAuthority::new(
        sealed.snapshot.id,
        Embedder.model(),
        u8::try_from(VECTOR_DIMENSION)
            .map_err(|_| BenchError::Query("dimension".to_owned()))?
            .into(),
        Embedder.metric(),
    );

    let mut graph_rows = [MaybeUninit::uninit(); GRAPH_ROWS];
    let mut graph_edges = [MaybeUninit::uninit(); GRAPH_EDGES];
    let mut graph_kinds = [NodeKind::PLACEHOLDER; GRAPH_ROWS];
    let mut graph_targets = [ReferenceTarget::PLACEHOLDER; GRAPH_ROWS];
    let mut graph_owners = [OwnerCount::PLACEHOLDER; GRAPH_ROWS];
    let mut graph_workspace =
        NodeWorkspace::new(&mut graph_kinds, &mut graph_targets, &mut graph_owners)
            .map_err(|_| BenchError::Query("graph workspace".to_owned()))?;
    let graph_projection = build_graph_projection(
        fragment_view,
        graph_authority,
        PartitionId::new(0),
        GraphProjectionScratch {
            rows: &mut graph_rows,
            edges: &mut graph_edges,
        },
        &mut graph_workspace,
    )?;
    assert_eq!(graph_projection.edge_count(), ENTITY_COUNT);

    let build_reading = readings(|| {
        let mut slots = BuildSlots::new();
        let _ = build(&fragment, slots.scratch());
    });
    let graph_reading = readings(|| {
        let mut rows = [MaybeUninit::uninit(); GRAPH_ROWS];
        let mut edges = [MaybeUninit::uninit(); GRAPH_EDGES];
        let mut kinds = [NodeKind::PLACEHOLDER; GRAPH_ROWS];
        let mut targets = [ReferenceTarget::PLACEHOLDER; GRAPH_ROWS];
        let mut owners = [OwnerCount::PLACEHOLDER; GRAPH_ROWS];
        let Ok(mut workspace) = NodeWorkspace::new(&mut kinds, &mut targets, &mut owners) else {
            return;
        };
        let _ = build_graph_projection(
            fragment_view,
            graph_authority,
            PartitionId::new(0),
            GraphProjectionScratch {
                rows: &mut rows,
                edges: &mut edges,
            },
            &mut workspace,
        );
    });
    let vector_reading = readings(|| {
        let mut segments = [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); VECTOR_SEGMENTS];
        let mut points = [MaybeUninit::uninit(); VECTOR_POINTS];
        let mut coordinates = [0_i16; VECTOR_POINTS * VECTOR_DIMENSION];
        let _ = build_vector_projection(
            &prepared,
            vector_authority,
            PartitionId::new(0),
            &Embedder,
            VectorProjectionScratch {
                segments: &mut segments,
                points: &mut points,
                coordinates: CoordinateLane::new(&mut coordinates),
            },
        );
    });
    assert_eq!(prepared.entities.len(), ENTITY_COUNT);

    let exact_reading = readings(|| {
        let present = exact_manifest.execute(ExactOperation::new(first_entity.exact_key.as_ref()));
        assert!(matches!(
            present,
            server_index_core::ExactTerminal::Complete {
                resolution: ExactResolution::Present { .. },
                ..
            }
        ));
        let absent = exact_manifest.execute(ExactOperation::new(b"absent-key"));
        assert!(matches!(
            absent,
            server_index_core::ExactTerminal::Complete {
                resolution: ExactResolution::Absent,
                ..
            }
        ));
    });
    let first_document = EntityDocumentId {
        fragment: prepared.fragment.fragment,
        entity: EntityId::new(0),
    };
    let mut lexical_seen = [None; ENTITY_COUNT];
    let mut lexical_out =
        [LexicalSnapshotHit::new(prepared.lexical.id, b"", first_document, 0.into()); 8];
    let lexical_reading = readings(|| {
        let Ok(top_k) = LexicalTopK::new(8) else {
            return;
        };
        let result = lexical_manifest.execute(
            LexicalOperation::prefix(b"prefix-"),
            top_k,
            &mut lexical_seen,
            &mut lexical_out,
        );
        assert!(result.is_ok());
    });
    let top_k = LexicalTopK::new(8).map_err(|_| BenchError::Query("lexical top-k".to_owned()))?;
    let lexical_terminal = lexical_manifest
        .execute(
            LexicalOperation::prefix(b"prefix-"),
            top_k,
            &mut lexical_seen,
            &mut lexical_out,
        )
        .map_err(|_| BenchError::Query("lexical".to_owned()))?;
    let lexical_hits = match lexical_terminal {
        server_index_core::LexicalTerminal::Complete { hits, .. }
        | server_index_core::LexicalTerminal::Partial { hits, .. }
        | server_index_core::LexicalTerminal::Degraded { hits, .. } => hits,
    };
    assert_eq!(lexical_hits.len(), 8);
    // Canonical name ordering places the eight prefix-alpha rows at entity ordinals 16..23.
    let expected_entities = [16_u32, 17, 18, 19, 20, 21, 22, 23];
    for (hit, expected) in lexical_hits.iter().zip(expected_entities) {
        assert_eq!(hit.document.entity, EntityId::new(expected));
    }
    let mut previous_score = None;
    for hit in lexical_hits {
        if let Some(previous) = previous_score {
            assert!(previous >= hit.score);
        }
        previous_score = Some(hit.score);
    }
    let mut table = FacetTable::new();
    let facet = join_facets(&lexical_terminal, &exact_manifest, &mut table)
        .map_err(|_| BenchError::Query("facet".to_owned()))?;
    assert_eq!(facet.counts.count(FacetCell::Function), 3);
    assert_eq!(facet.counts.count(FacetCell::Constant), 3);
    assert_eq!(facet.counts.count(FacetCell::Record), 2);
    assert_eq!(facet.counts.count(FacetCell::Unresolved), 0);
    let facet_reading = readings(|| {
        let mut table = FacetTable::new();
        let _ = join_facets(&lexical_terminal, &exact_manifest, &mut table);
    });

    let mut vector_segments =
        [MaybeUninit::<ValidatedVectorSegment<'_>>::uninit(); VECTOR_SEGMENTS];
    let mut vector_points = [MaybeUninit::uninit(); VECTOR_POINTS];
    let mut coordinates = [0_i16; VECTOR_POINTS * VECTOR_DIMENSION];
    let projection = build_vector_projection(
        &prepared,
        vector_authority,
        PartitionId::new(0),
        &Embedder,
        VectorProjectionScratch {
            segments: &mut vector_segments,
            points: &mut vector_points,
            coordinates: CoordinateLane::new(&mut coordinates),
        },
    )?;
    assert_eq!(projection.len(), VECTOR_SEGMENTS);
    let vector_segments = projection;
    let mut vector_out = [None; 8];
    let vector_reading_query = readings(|| {
        let result = exact_vector_query(
            vector_authority,
            &[
                PartitionId::new(0),
                PartitionId::new(1),
                PartitionId::new(2),
                PartitionId::new(3),
            ],
            vector_segments,
            &[0, 0, 0, 0, 0, 0, 0, 0],
            8,
            &mut vector_out,
        );
        assert!(result.is_ok());
    });
    let result = exact_vector_query(
        vector_authority,
        &[
            PartitionId::new(0),
            PartitionId::new(1),
            PartitionId::new(2),
            PartitionId::new(3),
        ],
        vector_segments,
        &[0; VECTOR_DIMENSION],
        8,
        &mut vector_out,
    )
    .map_err(|_| BenchError::Query("vector".to_owned()))?;
    assert_eq!(result.written, 8);
    assert_eq!(vector_out[0].map(|hit| hit.entity), Some(EntityId::new(0)));
    let rss_before = rss();
    let rss_after = rss();
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!(
        "environment | os={} (measured) | arch={} (measured) | profile={} (declared)",
        std::env::consts::OS,
        std::env::consts::ARCH,
        profile
    );
    println!(
        "stage | wall_ns_median (measured) | alloc_count (measured) | alloc_bytes (measured) | rss (measured|unknown) | work_label (analytic)"
    );
    row_unknown_alloc("publish cold", publish_wall, rss_before, "O(F·durability)");
    row(
        "build exact+lexical",
        build_reading.wall,
        build_reading.alloc,
        rss_after,
        "O(E·log E)",
    );
    row(
        "graph projection",
        graph_reading.wall,
        graph_reading.alloc,
        rss_after,
        "O(E + R)",
    );
    row(
        "vector projection",
        vector_reading.wall,
        vector_reading.alloc,
        rss_after,
        "O(E·D)",
    );
    row(
        "exact lookup",
        exact_reading.wall,
        exact_reading.alloc,
        rss_after,
        "O(log R)",
    );
    row(
        "lexical prefix ranking",
        lexical_reading.wall,
        lexical_reading.alloc,
        rss_after,
        "O(log R + H·log K)",
    );
    row(
        "facet join",
        facet_reading.wall,
        facet_reading.alloc,
        rss_after,
        "O(H·(log R + decode))",
    );
    row(
        "local vector NN",
        vector_reading_query.wall,
        vector_reading_query.alloc,
        rss_after,
        "O(P·D + P·K)",
    );
    Ok(())
}

fn row(
    stage: &str,
    wall: u128,
    alloc: AllocationInfo,
    rss_value: Result<u64, &'static str>,
    work: &str,
) {
    match rss_value {
        Ok(value) => println!(
            "{stage} | {wall} (measured) | {} (measured) | {} (measured) | {value} (measured) | {work} (analytic)",
            alloc.count_total, alloc.bytes_total
        ),
        Err(reason) => println!(
            "{stage} | {wall} (measured) | {} (measured) | {} (measured) | unknown ({reason}) | {work} (analytic)",
            alloc.count_total, alloc.bytes_total
        ),
    }
}

fn row_unknown_alloc(stage: &str, wall: u128, rss_value: Result<u64, &'static str>, work: &str) {
    match rss_value {
        Ok(value) => println!(
            "{stage} | {wall} (measured) | unknown (cold single-shot; not measured) | unknown (cold single-shot; not measured) | {value} (measured) | {work} (analytic)"
        ),
        Err(reason) => println!(
            "{stage} | {wall} (measured) | unknown (cold single-shot; not measured) | unknown (cold single-shot; not measured) | unknown ({reason}) | {work} (analytic)"
        ),
    }
}

struct BuildSlots<'bytes> {
    projections: [MaybeUninit<EntityProjection<'bytes>>; ENTITY_COUNT],
    entities: [MaybeUninit<server_index_build::EntityFact<'bytes>>; ENTITY_COUNT],
    exact: [MaybeUninit<server_index_core::ExactRow<'bytes>>; ENTITY_COUNT],
    lexical: [MaybeUninit<server_index_core::LexicalRow<'bytes>>; ENTITY_COUNT],
    atoms: [MaybeUninit<Atom<'bytes>>; ATOM_COUNT],
    types: [MaybeUninit<TypeNode>; TYPE_COUNT],
}
impl<'bytes> BuildSlots<'bytes> {
    const fn new() -> Self {
        Self {
            projections: [MaybeUninit::uninit(); ENTITY_COUNT],
            entities: [MaybeUninit::uninit(); ENTITY_COUNT],
            exact: [MaybeUninit::uninit(); ENTITY_COUNT],
            lexical: [MaybeUninit::uninit(); ENTITY_COUNT],
            atoms: [MaybeUninit::uninit(); ATOM_COUNT],
            types: [MaybeUninit::uninit(); TYPE_COUNT],
        }
    }
    const fn scratch(&'bytes mut self) -> IndexBuildScratch<'bytes> {
        IndexBuildScratch {
            projections: &mut self.projections,
            entities: &mut self.entities,
            exact_rows: &mut self.exact,
            lexical_rows: &mut self.lexical,
            atoms: &mut self.atoms,
            type_nodes: &mut self.types,
        }
    }
}

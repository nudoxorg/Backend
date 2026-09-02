//! Exercises the `server-index-retrieval` tests support contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Shared typed fixtures for sealed retrieval integration journeys.

#![allow(
    clippy::expect_used,
    dead_code,
    reason = "checked fixture setup preserves one borrowing lifetime, and each top-level target imports only the helpers it exercises"
)]

use std::{
    fs,
    mem::size_of,
    num::NonZeroUsize,
    path::PathBuf,
    pin::pin,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll, Waker},
};

use compiler_ir::{AtomId, EntityId, TypeId};
use compiler_ir::{
    AtomInput, EntityKind, EntityRecord, FragmentView, PreparedFragment, PrimitiveType,
    SourceIdentity, TypeNode,
};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use heart_hydration::{PlanScratch, Projection, demand, plan};
use heart_identity::{
    ArtifactId, ContentId, IrFragmentDomain, IrFragmentEncoding, ObjectDomain, SourceFactDomain,
    ToolchainDomain,
};
use heart_memory::{InsertOutcome, MemoryStore, StoreCapacity};
use heart_object::ObjectRef;
use heart_root::{ClosureScratch, GenerationRoot, GenerationView, PreparedLocality, RootEntry};
use heart_schema::SchemaId;
use server_index_core::{
    EntityDocumentId, ExactRow, ExactSegment, IndexSnapshot, LexicalRow, LexicalScore,
    LexicalSegment,
};
use server_index_graph_vector::{
    Cancellation, GraphAuthority, GraphDegradation, GraphEdge, GraphLease, GraphStreamEvent,
    GraphTerminal, LeaseCapacity, LeaseStateCell, Metric, ModelId, PartitionId, ProjectionId,
    StreamCapacityError, TraceProbe, ValidatedVectorSegment, VectorAuthority, VectorPoint,
};
use server_index_publish::PublishedIndexSnapshot;
use server_index_qdrant::{QdrantBlockingAdapter, QdrantDataKey, QdrantError};
use server_index_retrieval::{
    RetrievalBoundary, RetrievalFailure, RetrievalOperationTerminal, RetrievalResult, VectorRoute,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

pub(crate) fn lexical_document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            b"sealed-retrieval-fixture-fragment",
        ),
        entity: EntityId::new(entity),
    }
}

/// All immutable borrowed facts shared by one sealed retrieval test closure.
#[derive(Clone, Copy)]
pub(crate) struct SealedFixture<'fixture> {
    /// Durable snapshot authority, retained behind its publication witness.
    pub sealed: &'fixture PublishedIndexSnapshot<'fixture, 'fixture, Box<[u8]>>,
    /// Exact canonical segments selected by the sealed snapshot.
    pub exact: [ExactSegment<'fixture>; 2],
    /// Lexical canonical segments selected by the sealed snapshot.
    pub lexical: [LexicalSegment<'fixture>; 2],
    /// Full pinned graph projection authority.
    pub graph_authority: GraphAuthority,
    /// Full pinned vector projection authority.
    pub vector_authority: VectorAuthority,
    /// Validated vector segment facts selected by the vector projection.
    pub vector_segments: &'fixture [ValidatedVectorSegment<'fixture>; 2],
}

/// Executes one test closure with a published immutable snapshot and cleans its durable fixture.
pub(crate) fn with_sealed_fixture(exercise: impl for<'fixture> FnOnce(&SealedFixture<'fixture>)) {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"retrieval-fixture-source"),
        byte_len: 24,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"retrieval-fixture-toolchain"),
    );
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Constant,
    }];
    let types = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput {
        bytes: b"published",
    }];
    let prepared = PreparedFragment::prepare(source, recipe, &entities, &types, &atoms)
        .expect("valid fragment");
    let mut fragment_storage = [0_u8; 512];
    let fragment = prepared
        .write_into(&mut fragment_storage)
        .expect("fragment storage is sufficient");
    let view = FragmentView::validate(fragment).expect("valid written fragment");
    assert_eq!(view.entities().count(), 1);

    let fragment_length = u64::try_from(fragment.len()).expect("fragment length fits u64");
    let object = ObjectRef {
        content: ContentId::<ObjectDomain>::from_canonical_bytes(fragment),
        length: fragment_length.into(),
        schema: SchemaId::Object,
        kind: 1_u16.into(),
    };
    let root = GenerationRoot::new(Vec::from([RootEntry {
        key: 1_u64.into(),
        parent: None,
        object,
    }]))
    .expect("valid generation root");
    let store = MemoryStore::new(StoreCapacity {
        bytes: fragment_length.into(),
        slots: 1_u32.into(),
    })
    .expect("valid store capacity");
    let mut store = store;
    assert!(matches!(
        store.insert_owned(object, Box::<[u8]>::from(fragment)),
        Ok(InsertOutcome::Inserted)
    ));

    let locality = PreparedLocality::prepare(&root, &[]).expect("valid locality");
    let mut locality_storage = [0_u8; 256];
    let locality = locality
        .write(&mut locality_storage)
        .expect("locality storage is sufficient");
    let generation = GenerationView::new(&root, &locality).expect("valid generation view");
    let mut closure = ClosureScratch::new(root.len()).expect("valid closure scratch");
    let mut planning = PlanScratch::new(root.entry_count.into()).expect("valid planning scratch");
    let hydration = plan(
        demand(&generation, Projection::CompleteGeneration),
        &mut closure,
        &mut planning,
        |_| true,
    )
    .expect("valid hydration plan");
    let verified = hydration
        .stage()
        .verify_store(&store)
        .expect("stored generation is verified");

    let directory = fixture_path();
    fs::create_dir_all(&directory).expect("fixture directory");
    let limits = PublicationLimits::new(
        NonZeroUsize::new(2).expect("nonzero queue capacity"),
        NonZeroUsize::new(2).expect("nonzero group capacity"),
    )
    .expect("valid publication limits");
    let paths = PublicationPaths::in_directory(&directory);
    let publisher = DurablePublisher::create(&paths, limits).expect("publisher creation");
    let publication = publisher
        .try_publish(verified)
        .expect("publication admission")
        .wait()
        .expect("publication completion");

    let exact_present_rows = [ExactRow::present(b"entity/0", b"published")];
    let exact_missing_rows = [ExactRow::present(b"entity/1", b"unavailable")];
    let lexical_present_rows = [LexicalRow::new(
        b"bool",
        lexical_document(0),
        LexicalScore::from(1),
    )];
    let lexical_missing_rows = [LexicalRow::new(
        b"other",
        lexical_document(1),
        LexicalScore::from(1),
    )];
    let exact = [
        ExactSegment::new(&exact_present_rows).expect("valid exact present segment"),
        ExactSegment::new(&exact_missing_rows).expect("valid exact missing segment"),
    ];
    let lexical = [
        LexicalSegment::new(&lexical_present_rows).expect("valid lexical present segment"),
        LexicalSegment::new(&lexical_missing_rows).expect("valid lexical missing segment"),
    ];
    let exact_ids = exact.map(|segment| segment.id);
    let lexical_ids = lexical.map(|segment| segment.id);
    let snapshot = IndexSnapshot::new(root.id, &exact_ids, &lexical_ids).expect("valid snapshot");
    let sealed = PublishedIndexSnapshot::seal(publication, snapshot);
    assert!(sealed.is_ok());
    let Ok(sealed) = sealed else {
        return;
    };

    let graph_authority = GraphAuthority::new(sealed.snapshot.id, ProjectionId::new(4));
    let vector_authority = VectorAuthority::new(
        sealed.snapshot.id,
        ModelId::new([0x71; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let first_coordinates = [1_i16, 0];
    let second_coordinates = [0_i16, 1];
    let first_points = [VectorPoint::new(EntityId::new(9), &first_coordinates)];
    let second_points = [VectorPoint::new(EntityId::new(3), &second_coordinates)];
    let vector_segments = [
        ValidatedVectorSegment::try_new(vector_authority, PartitionId::new(1), &first_points)
            .expect("valid first vector segment"),
        ValidatedVectorSegment::try_new(vector_authority, PartitionId::new(2), &second_points)
            .expect("valid second vector segment"),
    ];

    exercise(&SealedFixture {
        sealed: &sealed,
        exact,
        lexical,
        graph_authority,
        vector_authority,
        vector_segments: &vector_segments,
    });

    publisher.shutdown().expect("publisher shutdown");
    fs::remove_dir_all(directory).expect("fixture cleanup");
}

/// Graph acquisition outcome selected by one public graph-terminal test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GraphAcquisitionKind {
    /// All selected partitions arrived through the healthy route.
    Complete,
    /// One selected partition was absent.
    Partial,
    /// The graph route degraded while retaining all selected coverage.
    Degraded,
    /// The graph route degraded and retained one absent partition.
    DegradedPartial,
}

/// Produces a real bounded graph-acquisition terminal for a pinned authority.
pub(crate) fn acquired_terminal(
    authority: GraphAuthority,
    kind: GraphAcquisitionKind,
) -> GraphTerminal {
    let partition = PartitionId::new(1);
    let cancellation = Cancellation::new();
    let mut trace = TraceProbe::disabled();
    let lease = GraphLease::new(
        authority,
        &[partition],
        LeaseCapacity {
            edges_per_partition: 1,
            bytes_per_partition: size_of::<GraphEdge>(),
        },
        &cancellation,
        &mut trace,
    )
    .expect("valid graph lease");
    let (mut producer, stream) = lease.split().expect("one graph endpoint pair");
    let edge = GraphEdge::new(authority, partition, EntityId::new(0), EntityId::new(1));
    match kind {
        GraphAcquisitionKind::Complete => producer
            .settle(partition, &[edge])
            .and_then(|()| producer.finish()),
        GraphAcquisitionKind::Partial => producer.finish_partial(&[partition]),
        GraphAcquisitionKind::Degraded => producer
            .settle(partition, &[edge])
            .and_then(|()| producer.finish_degraded(&[], GraphDegradation::StaleRoute)),
        GraphAcquisitionKind::DegradedPartial => {
            producer.finish_degraded(&[partition], GraphDegradation::PartitionSourceUnavailable)
        }
    }
    .expect("terminal graph lease");

    let mut stream = pin!(stream);
    let mut context = Context::from_waker(Waker::noop());
    for _ in 0..2 {
        match stream.as_mut().poll_batch(&mut context, &mut trace) {
            Poll::Ready(GraphStreamEvent::Batch(_batch)) => {}
            Poll::Ready(GraphStreamEvent::Terminal(terminal)) => return terminal,
            Poll::Ready(GraphStreamEvent::Fused) | Poll::Pending => break,
        }
    }
    GraphTerminal::Failed {
        authority,
        cause: StreamCapacityError::CorruptState {
            cell: LeaseStateCell::Terminal,
        },
    }
}

#[allow(
    dead_code,
    reason = "the assertion's Debug rendering retains the concrete Qdrant cause on a conditional live failure"
)]
#[derive(Debug)]
enum LiveFacadeQdrantError {
    Qdrant(QdrantError),
    IncompleteTerminal,
    UnstableOrder,
}

/// Exercises a real Qdrant facade query only when the normal launcher supplied an endpoint.
pub(crate) fn query_qdrant_if_provisioned<PayloadOwner>(
    boundary: &RetrievalBoundary<'_, '_, '_, '_, PayloadOwner>,
    segments: &[ValidatedVectorSegment<'_>; 2],
) where
    PayloadOwner: AsRef<[u8]>,
{
    let Ok(endpoint) = std::env::var("QDRANT_URL") else {
        return;
    };
    let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let collection = format!("server_retrieval_{}_{}", std::process::id(), ordinal);
    let authority = boundary.vector_authority;
    let adapter = QdrantBlockingAdapter::new(&endpoint, &collection, authority)
        .expect("valid Qdrant adapter");
    let journey = (|| -> Result<(), LiveFacadeQdrantError> {
        adapter
            .ensure_collection()
            .map_err(LiveFacadeQdrantError::Qdrant)?;
        let receipt = adapter
            .upsert(segments)
            .map_err(LiveFacadeQdrantError::Qdrant)?;
        if receipt.verified != 2 {
            return Err(LiveFacadeQdrantError::IncompleteTerminal);
        }
        let descriptors = [segments[0].descriptor(), segments[1].descriptor()];
        let mut output = [None, None];
        let terminal = boundary.qdrant(
            VectorRoute::Healthy,
            &adapter,
            &descriptors,
            &[0, 0],
            2,
            &mut output,
        );
        match terminal {
            RetrievalOperationTerminal::Complete {
                result: RetrievalResult::Qdrant(count),
                ..
            } if count.count == 2 => {}
            RetrievalOperationTerminal::Failed {
                cause: RetrievalFailure::Qdrant(cause),
                ..
            } => return Err(LiveFacadeQdrantError::Qdrant(cause)),
            _ => return Err(LiveFacadeQdrantError::IncompleteTerminal),
        }
        if output.map(|hit| hit.map(|hit| (hit.authority, hit.entity)))
            != [
                Some((authority, EntityId::new(3))),
                Some((authority, EntityId::new(9))),
            ]
        {
            return Err(LiveFacadeQdrantError::UnstableOrder);
        }
        let keys = [
            QdrantDataKey::new(
                authority,
                segments[0].id,
                segments[0].partition,
                EntityId::new(9),
            ),
            QdrantDataKey::new(
                authority,
                segments[1].id,
                segments[1].partition,
                EntityId::new(3),
            ),
        ];
        let deleted = adapter
            .delete(&keys)
            .map_err(LiveFacadeQdrantError::Qdrant)?;
        (deleted.verified == 2)
            .then_some(())
            .ok_or(LiveFacadeQdrantError::IncompleteTerminal)
    })();
    let cleanup = adapter.delete_collection();
    assert!(journey.is_ok(), "facade Qdrant journey failed: {journey:?}");
    assert!(cleanup.is_ok(), "facade Qdrant collection cleanup failed");
}

fn fixture_path() -> PathBuf {
    let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "server-retrieval-boundary-{}-{ordinal}",
        std::process::id()
    ))
}

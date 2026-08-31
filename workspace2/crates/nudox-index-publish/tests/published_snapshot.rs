//! Durable canonical publication through immutable exact, Tantivy, and Trustfall queries.

use std::{
    fs,
    num::NonZeroUsize,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};
use nudox_durable_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use nudox_hydration::{PlanScratch, Projection, demand, plan};
use nudox_id::{
    ArtifactId, ContentId, GenerationId, IrFragmentDomain, IrFragmentEncoding, ObjectDomain,
    SourceFactDomain, ToolchainDomain,
};
use nudox_index_core::{
    EntityDocumentId, ExactManifest, ExactOperation, ExactResolution, ExactRow, ExactSegment,
    ExactTerminal, IndexSnapshot, IndexSnapshotId, LexicalManifest, LexicalRow, LexicalScore,
    LexicalSegment,
};
use nudox_index_graph_vector::{
    GraphAuthority, GraphEdge, GraphRow, Metric, ModelId, PartitionId, ProjectionId,
    ValidatedGraphView, ValidatedVectorSegment, VectorAuthority, VectorPoint, VectorSegmentError,
};
use nudox_index_publish::{PublishedIndexSnapshot, PublishedIndexSnapshotError};
use nudox_index_qdrant::{QdrantBlockingAdapter, QdrantDataKey, QdrantError};
use nudox_index_tantivy::{TantivyHit, TantivyLexical};
use nudox_index_trustfall::TrustfallGraph;
use nudox_ir_format::{
    AtomInput, EntityKind, EntityRecord, FragmentView, PreparedFragment, PrimitiveType,
    SourceIdentity, TypeNode,
};
use nudox_ir_vocab::{AtomId, EntityId, TypeId};
use nudox_object::ObjectRef;
use nudox_root::{ClosureScratch, GenerationRoot, GenerationView, PreparedLocality, RootEntry};
use nudox_schema::SchemaId;
use nudox_store_memory::{InsertOutcome, MemoryStore, StoreCapacity};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn lexical_document(entity: u32) -> EntityDocumentId {
    EntityDocumentId {
        fragment: ArtifactId::<IrFragmentEncoding, IrFragmentDomain>::from_encoded_bytes(
            b"published-index-test-fragment",
        ),
        entity: EntityId::new(entity),
    }
}

fn fixture_path() -> PathBuf {
    let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nudox-published-index-{}-{ordinal}",
        std::process::id()
    ))
}

#[derive(Debug, thiserror::Error)]
enum PublishedVectorJourneyError {
    #[error("published vector segment was invalid: {0:?}")]
    InvalidSegment(VectorSegmentError),
    #[error("published Qdrant projection failed")]
    Qdrant(#[from] QdrantError),
}

#[allow(
    clippy::result_large_err,
    clippy::too_many_lines,
    reason = "the one live cross-crate leg retains concrete cold causes without heap erasure"
)]
fn query_qdrant_if_provisioned(
    snapshot: IndexSnapshotId,
) -> Result<(), PublishedVectorJourneyError> {
    let Ok(endpoint) = std::env::var("QDRANT_URL") else {
        return Ok(());
    };
    let collection_ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let collection = format!(
        "nudox_published_{}_{}",
        std::process::id(),
        collection_ordinal
    );
    let authority = VectorAuthority::new(
        snapshot,
        ModelId::new([0x61; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let first_coordinates = [1_i16, 0];
    let second_coordinates = [0_i16, 1];
    let first_points = [VectorPoint::new(EntityId::new(9), &first_coordinates)];
    let second_points = [VectorPoint::new(EntityId::new(3), &second_coordinates)];
    let segments = [
        ValidatedVectorSegment::try_new(authority, PartitionId::new(1), &first_points)
            .map_err(PublishedVectorJourneyError::InvalidSegment)?,
        ValidatedVectorSegment::try_new(authority, PartitionId::new(2), &second_points)
            .map_err(PublishedVectorJourneyError::InvalidSegment)?,
    ];
    let adapter = QdrantBlockingAdapter::new(&endpoint, &collection, authority)?;
    let operation = (|| -> Result<(), PublishedVectorJourneyError> {
        adapter.ensure_collection()?;
        let receipt = adapter.upsert(&segments)?;
        assert_eq!(receipt.verified, 2);

        let descriptors = [segments[0].descriptor(), segments[1].descriptor()];
        let mut output = [None, None];
        let candidates = adapter.query(&descriptors, &[0, 0], 2, &mut output)?;
        assert_eq!(candidates.count, 2);
        assert_eq!(
            output.map(|hit| hit.map(|hit| (hit.authority, hit.entity))),
            [
                Some((authority, EntityId::new(3))),
                Some((authority, EntityId::new(9))),
            ]
        );

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
        assert_eq!(adapter.delete(&keys)?.verified, 2);
        Ok(())
    })();
    let cleanup = adapter.delete_collection();
    operation?;
    cleanup?;
    Ok(())
}

#[test]
#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "one chronological top-level journey deliberately retains every public boundary"
)]
fn durable_ir_publication_seals_exact_and_real_tantivy_queries() {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"published-index-source"),
        byte_len: 22,
    };
    let recipe = CompileRecipeFact::derive(
        Language::Rust,
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"published-index-toolchain"),
    );
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Constant,
    }];
    let type_nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput {
        bytes: b"published",
    }];
    let prepared = PreparedFragment::prepare(source, recipe, &entities, &type_nodes, &atoms);
    assert!(prepared.is_ok());
    let Ok(prepared) = prepared else {
        return;
    };
    let mut fragment_storage = [0_u8; 512];
    let fragment = prepared.write_into(&mut fragment_storage);
    assert!(fragment.is_ok());
    let Ok(fragment) = fragment else {
        return;
    };
    let view = FragmentView::validate(fragment);
    assert!(view.is_ok());
    let Ok(view) = view else {
        return;
    };
    assert_eq!(view.entities().count(), 1);

    let fragment_len = u64::try_from(fragment.len());
    assert!(fragment_len.is_ok());
    let Ok(fragment_len) = fragment_len else {
        return;
    };
    let object = ObjectRef {
        content: ContentId::<ObjectDomain>::from_canonical_bytes(fragment),
        length: fragment_len.into(),
        schema: SchemaId::Object,
        kind: 1_u16.into(),
    };
    let root = GenerationRoot::new(Vec::from([RootEntry {
        key: 1_u64.into(),
        parent: None,
        object,
    }]));
    assert!(root.is_ok());
    let Ok(root) = root else {
        return;
    };
    let store = MemoryStore::new(StoreCapacity {
        bytes: fragment_len.into(),
        slots: 1_u32.into(),
    });
    assert!(store.is_ok());
    let Ok(mut store) = store else {
        return;
    };
    let insertion = store.insert_owned(object, Box::<[u8]>::from(fragment));
    assert!(
        matches!(insertion, Ok(InsertOutcome::Inserted)),
        "IR object admission failed"
    );

    let locality = PreparedLocality::prepare(&root, &[]);
    assert!(locality.is_ok());
    let Ok(locality) = locality else {
        return;
    };
    let mut locality_storage = [0_u8; 256];
    let locality = locality.write(&mut locality_storage);
    assert!(locality.is_ok());
    let Ok(locality) = locality else {
        return;
    };
    let generation = GenerationView::new(&root, &locality);
    assert!(generation.is_ok());
    let Ok(generation) = generation else {
        return;
    };
    let closure = ClosureScratch::new(root.len());
    assert!(closure.is_ok());
    let Ok(mut closure) = closure else {
        return;
    };
    let planning = PlanScratch::new(root.entry_count.into());
    assert!(planning.is_ok());
    let Ok(mut planning) = planning else {
        return;
    };
    let hydration = plan(
        demand(&generation, Projection::CompleteGeneration),
        &mut closure,
        &mut planning,
        |_| true,
    );
    assert!(hydration.is_ok());
    let Ok(hydration) = hydration else {
        return;
    };
    let verified = hydration.stage().verify_store(&store);
    assert!(verified.is_ok());
    let Ok(verified) = verified else {
        return;
    };

    let directory = fixture_path();
    assert!(
        fs::create_dir_all(&directory).is_ok(),
        "publication directory creation failed"
    );
    let Some(queue_capacity) = NonZeroUsize::new(2) else {
        return;
    };
    let Some(group_capacity) = NonZeroUsize::new(2) else {
        return;
    };
    let limits = PublicationLimits::new(queue_capacity, group_capacity);
    assert!(limits.is_ok());
    let Ok(limits) = limits else {
        return;
    };
    let paths = PublicationPaths::in_directory(&directory);
    let publisher = DurablePublisher::create(&paths, limits);
    assert!(publisher.is_ok());
    let Ok(publisher) = publisher else {
        return;
    };
    let pending = publisher.try_publish(verified);
    assert!(pending.is_ok());
    let Ok(pending) = pending else {
        return;
    };
    let publication_result = pending.wait();
    assert!(publication_result.is_ok());
    let Ok(durable_publication) = publication_result else {
        return;
    };

    let exact_rows = [ExactRow::present(b"entity/0", fragment)];
    let lexical_rows = [LexicalRow::new(
        b"bool",
        lexical_document(0),
        LexicalScore::from(1),
    )];
    let exact = ExactSegment::new(&exact_rows);
    let lexical = LexicalSegment::new(&lexical_rows);
    assert!(exact.is_ok() && lexical.is_ok());
    let (Ok(exact), Ok(lexical)) = (exact, lexical) else {
        return;
    };
    let exact_ids = [exact.id];
    let lexical_ids = [lexical.id];
    let wrong_snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"unpublished-generation"),
        &exact_ids,
        &lexical_ids,
    );
    assert!(wrong_snapshot.is_ok());
    let Ok(wrong_snapshot) = wrong_snapshot else {
        return;
    };
    let rejected = PublishedIndexSnapshot::seal(durable_publication, wrong_snapshot);
    assert!(rejected.is_err());
    let Err(rejected) = rejected else {
        return;
    };
    assert_eq!(
        rejected.error,
        PublishedIndexSnapshotError::GenerationMismatch {
            published: root.id,
            snapshot: wrong_snapshot.generation,
        }
    );
    let snapshot = IndexSnapshot::new(root.id, &exact_ids, &lexical_ids);
    assert!(snapshot.is_ok());
    let Ok(snapshot) = snapshot else {
        return;
    };
    let sealed = PublishedIndexSnapshot::seal(rejected.published, snapshot);
    assert!(sealed.is_ok());
    let Ok(sealed) = sealed else {
        return;
    };
    assert_eq!(sealed.snapshot.generation, root.id);

    let exact_segments = [exact];
    let exact_manifest = ExactManifest::new(sealed.snapshot, &exact_segments, &[]);
    assert!(exact_manifest.is_ok());
    let Ok(exact_manifest) = exact_manifest else {
        return;
    };
    assert!(matches!(
        exact_manifest.execute(ExactOperation::new(b"entity/0")),
        ExactTerminal::Complete {
            resolution: ExactResolution::Present { value, .. },
            ..
        } if value == fragment
    ));

    let lexical_segments = [lexical];
    let lexical_manifest = LexicalManifest::new(sealed.snapshot, &lexical_segments, &[]);
    assert!(lexical_manifest.is_ok());
    let Ok(lexical_manifest) = lexical_manifest else {
        return;
    };
    let tantivy = TantivyLexical::build(lexical_manifest);
    assert!(tantivy.is_ok());
    let Ok(tantivy) = tantivy else {
        return;
    };
    let mut output = [None];
    let terminal = tantivy.search(sealed.snapshot.id, "bool", 1, &mut output);
    assert!(terminal.is_ok());
    assert_eq!(
        output,
        [Some(TantivyHit {
            document: lexical_document(0),
        })]
    );

    let graph_authority = GraphAuthority::new(sealed.snapshot.id, ProjectionId::new(1));
    let partition = PartitionId::new(0);
    let edges = [GraphEdge::new(
        graph_authority,
        partition,
        EntityId::new(0),
        EntityId::new(1),
    )];
    let graph_rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let graph_view = ValidatedGraphView::try_new(graph_authority, &graph_rows);
    assert!(graph_view.is_ok());
    let Ok(graph_view) = graph_view else {
        return;
    };
    let graph = TrustfallGraph::new(&graph_view);
    let mut graph_output = [None];
    let graph_terminal = graph.neighbors(EntityId::new(0), &mut graph_output);
    assert!(graph_terminal.is_ok());
    let Ok(graph_terminal) = graph_terminal else {
        return;
    };
    assert_eq!(graph_terminal.authority, graph_authority);
    assert_eq!(graph_terminal.written, 1);
    assert_eq!(
        graph_output
            .first()
            .copied()
            .flatten()
            .map(|hit| (hit.entity, hit.partition)),
        Some((EntityId::new(1), partition))
    );

    let vector_result = query_qdrant_if_provisioned(sealed.snapshot.id);
    assert!(
        vector_result.is_ok(),
        "published Qdrant leg failed: {vector_result:?}"
    );

    assert!(publisher.shutdown().is_ok(), "publisher shutdown failed");
    assert!(
        fs::remove_dir_all(directory).is_ok(),
        "publication fixture cleanup failed"
    );
}

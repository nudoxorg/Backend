//! Durable canonical publication through immutable exact, Tantivy, and Trustfall queries.

use std::{
    fs,
    num::NonZeroUsize,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use nudox_durable_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use nudox_hydration::{PlanScratch, Projection, demand, plan};
use nudox_id::{ContentId, GenerationId, ObjectDomain};
use nudox_index_core::{
    ExactManifest, ExactOperation, ExactResolution, ExactRow, ExactSegment, ExactTerminal,
    IndexSnapshot, LexicalManifest, LexicalRow, LexicalScore, LexicalSegment,
};
use nudox_index_graph_vector::{
    GraphAuthority, GraphEdge, GraphRow, PartitionId, ProjectionId, ValidatedGraphView,
};
use nudox_index_publish::{PublishedIndexSnapshot, PublishedIndexSnapshotError};
use nudox_index_tantivy::{TantivyHit, TantivyLexical};
use nudox_index_trustfall::TrustfallGraph;
use nudox_ir_format::{EntityRecord, FragmentView, PreparedFragment, PrimitiveType, TypeNode};
use nudox_ir_vocab::{EntityId, TypeId};
use nudox_object::ObjectRef;
use nudox_root::{ClosureScratch, GenerationRoot, GenerationView, PreparedLocality, RootEntry};
use nudox_schema::SchemaId;
use nudox_store_memory::{InsertOutcome, MemoryStore, StoreCapacity};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn fixture_path() -> PathBuf {
    let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nudox-published-index-{}-{ordinal}",
        std::process::id()
    ))
}

#[test]
#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "one chronological top-level journey deliberately retains every public boundary"
)]
fn durable_ir_publication_seals_exact_and_real_tantivy_queries() {
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
    }];
    let type_nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let prepared = PreparedFragment::prepare(&entities, &type_nodes);
    assert!(prepared.is_ok());
    let Ok(prepared) = prepared else {
        return;
    };
    let mut fragment_storage = [0_u8; 128];
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
    let lexical_rows = [LexicalRow::new(b"bool", 0, LexicalScore::from(1))];
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
    assert_eq!(output, [Some(TantivyHit { document: 0 })]);

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

    assert!(publisher.shutdown().is_ok(), "publisher shutdown failed");
    assert!(
        fs::remove_dir_all(directory).is_ok(),
        "publication fixture cleanup failed"
    );
}

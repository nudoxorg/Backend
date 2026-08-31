//! Focused sealed-publication tests for the server-only retrieval terminal boundary.

use std::{
    fs,
    num::NonZeroUsize,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};
use std::{
    mem::size_of,
    pin::pin,
    task::{Context, Poll, Waker},
};

use nudox_durable_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use nudox_hydration::{PlanScratch, Projection, demand, plan};
use nudox_id::{ContentId, GenerationId, ObjectDomain};
use nudox_index_core::{
    ExactDegradation, ExactOperation, ExactResolution, ExactRow, ExactSegment, IndexSnapshot,
    LexicalDegradation, LexicalDocumentId, LexicalManifest, LexicalOperation, LexicalRow,
    LexicalScore, LexicalSegment, LexicalSnapshotHit, LexicalTopK,
};
use nudox_index_graph_vector::{
    Cancellation, GraphAuthority, GraphDegradation, GraphEdge, GraphLease, GraphRow,
    GraphStreamEvent, GraphTerminal, LeaseCapacity, LeaseStateCell, Metric, ModelId, PartitionId,
    ProjectionId, StreamCapacityError, TraceProbe, ValidatedGraphView, VectorAuthority,
};
use nudox_index_publish::PublishedIndexSnapshot;
use nudox_index_qdrant::{PhysicalPointId, QdrantBlockingAdapter, QdrantCandidate};
use nudox_index_retrieval::{
    CancellationCause, ExactRoute, LexicalRoute, RetrievalAbsence, RetrievalBoundary,
    RetrievalCoverage, RetrievalFailure, RetrievalOperationTerminal, VectorAuthoritySurface,
};
use nudox_index_tantivy::{TantivyAdapterError, TantivyHit, TantivyLexical};
use nudox_index_trustfall::TrustfallHit;
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
        "nudox-retrieval-boundary-{}-{ordinal}",
        std::process::id()
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GraphAcquisitionKind {
    Complete,
    Partial,
    Degraded,
    DegradedPartial,
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded lease must remain live while its producer and pinned consumer prove one terminal"
)]
fn acquired_terminal(authority: GraphAuthority, kind: GraphAcquisitionKind) -> GraphTerminal {
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
    );
    assert!(lease.is_ok());
    let Ok(lease) = lease else {
        return GraphTerminal::Failed {
            authority,
            cause: StreamCapacityError::CorruptState {
                cell: LeaseStateCell::Terminal,
            },
        };
    };
    let endpoints = lease.split();
    assert!(endpoints.is_ok());
    let Ok((mut producer, stream)) = endpoints else {
        return GraphTerminal::Failed {
            authority,
            cause: StreamCapacityError::EndpointsAlreadyBorrowed,
        };
    };
    let edge = GraphEdge::new(authority, partition, EntityId::new(0), EntityId::new(1));
    let finish = match kind {
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
    };
    assert!(finish.is_ok());
    let mut stream = pin!(stream);
    let mut context = Context::from_waker(Waker::noop());
    let mut terminal = None;
    for _ in 0..2 {
        match stream.as_mut().poll_batch(&mut context, &mut trace) {
            Poll::Ready(GraphStreamEvent::Batch(_batch)) => {}
            Poll::Ready(GraphStreamEvent::Terminal(observed)) => {
                terminal = Some(observed);
                break;
            }
            Poll::Ready(GraphStreamEvent::Fused) | Poll::Pending => break,
        }
    }
    assert!(terminal.is_some());
    terminal.unwrap_or(GraphTerminal::Failed {
        authority,
        cause: StreamCapacityError::CorruptState {
            cell: LeaseStateCell::Terminal,
        },
    })
}

#[test]
#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "one chronological fixture retains the sealed publication witness for every boundary attack"
)]
fn sealed_boundary_classifies_retrieval_terminals_without_mutating_pre_cancelled_outputs() {
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
    }];
    let types = [TypeNode::Primitive(PrimitiveType::Bool)];
    let prepared = PreparedFragment::prepare(&entities, &types);
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

    let fragment_length = u64::try_from(fragment.len());
    assert!(fragment_length.is_ok());
    let Ok(fragment_length) = fragment_length else {
        return;
    };
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
    }]));
    assert!(root.is_ok());
    let Ok(root) = root else {
        return;
    };
    let store = MemoryStore::new(StoreCapacity {
        bytes: fragment_length.into(),
        slots: 1_u32.into(),
    });
    assert!(store.is_ok());
    let Ok(mut store) = store else {
        return;
    };
    assert!(matches!(
        store.insert_owned(object, Box::<[u8]>::from(fragment)),
        Ok(InsertOutcome::Inserted)
    ));

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
    assert!(fs::create_dir_all(&directory).is_ok());
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
    let publication = pending.wait();
    assert!(publication.is_ok());
    let Ok(publication) = publication else {
        return;
    };

    let exact_present_rows = [ExactRow::present(b"entity/0", b"published")];
    let exact_missing_rows = [ExactRow::present(b"entity/1", b"unavailable")];
    let lexical_present_rows = [LexicalRow::new(b"bool", 0, LexicalScore::from(1))];
    let lexical_missing_rows = [LexicalRow::new(b"other", 1, LexicalScore::from(1))];
    let exact_present = ExactSegment::new(&exact_present_rows);
    let exact_missing = ExactSegment::new(&exact_missing_rows);
    let lexical_present = LexicalSegment::new(&lexical_present_rows);
    let lexical_missing = LexicalSegment::new(&lexical_missing_rows);
    assert!(
        exact_present.is_ok()
            && exact_missing.is_ok()
            && lexical_present.is_ok()
            && lexical_missing.is_ok()
    );
    let (Ok(exact_present), Ok(exact_missing), Ok(lexical_present), Ok(lexical_missing)) = (
        exact_present,
        exact_missing,
        lexical_present,
        lexical_missing,
    ) else {
        return;
    };
    let exact_ids = [exact_present.id, exact_missing.id];
    let lexical_ids = [lexical_present.id, lexical_missing.id];
    let snapshot = IndexSnapshot::new(root.id, &exact_ids, &lexical_ids);
    assert!(snapshot.is_ok());
    let Ok(snapshot) = snapshot else {
        return;
    };
    let sealed = PublishedIndexSnapshot::seal(publication, snapshot);
    assert!(sealed.is_ok());
    let Ok(sealed) = sealed else {
        return;
    };

    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(&sealed, &cancellation);
    let exact_segments = [exact_present];
    let exact_missing_ids = [exact_missing.id];
    let mut exact_output = None;
    let exact_partial = boundary.exact(
        ExactRoute::Healthy,
        &exact_segments,
        &exact_missing_ids,
        ExactOperation::new(b"entity/0"),
        &mut exact_output,
    );
    assert!(matches!(
        exact_partial,
        RetrievalOperationTerminal::Partial {
            absence: RetrievalAbsence::Exact(missing),
            ..
        } if missing == exact_missing_ids
    ));
    assert!(matches!(
        exact_output,
        Some(ExactResolution::Present {
            value: b"published",
            ..
        })
    ));

    let exact_degraded = boundary.exact(
        ExactRoute::Degraded(ExactDegradation::StaleRoute),
        &exact_segments,
        &exact_missing_ids,
        ExactOperation::new(b"entity/0"),
        &mut exact_output,
    );
    assert!(matches!(
        exact_degraded,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Exact(missing)),
            ..
        } if missing == exact_missing_ids
    ));

    let lexical_segments = [lexical_present];
    let lexical_missing_ids = [lexical_missing.id];
    let top_k = LexicalTopK::new(1);
    assert!(top_k.is_ok());
    let Ok(top_k) = top_k else {
        return;
    };
    let mut lexical_scratch = [None];
    let mut lexical_output = [LexicalSnapshotHit::new(
        lexical_present.id,
        b"sentinel",
        LexicalDocumentId::from(99),
        LexicalScore::from(0),
    )];
    let lexical_partial = boundary.lexical(
        LexicalRoute::Healthy,
        &lexical_segments,
        &lexical_missing_ids,
        LexicalOperation::new(b"bool"),
        top_k,
        &mut lexical_scratch,
        &mut lexical_output,
    );
    assert!(matches!(
        lexical_partial,
        RetrievalOperationTerminal::Partial {
            absence: RetrievalAbsence::Lexical(missing),
            ..
        } if missing == lexical_missing_ids
    ));
    assert_eq!(lexical_output[0].term, b"bool");

    let mut degraded_scratch = [None];
    let mut degraded_output = lexical_output;
    let lexical_degraded = boundary.lexical(
        LexicalRoute::Degraded(LexicalDegradation::SegmentSourceUnavailable),
        &lexical_segments,
        &lexical_missing_ids,
        LexicalOperation::new(b"bool"),
        top_k,
        &mut degraded_scratch,
        &mut degraded_output,
    );
    assert!(matches!(
        lexical_degraded,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Lexical(missing)),
            ..
        } if missing == lexical_missing_ids
    ));

    let cancelled = Cancellation::new();
    cancelled.cancel();
    let cancelled_boundary = RetrievalBoundary::new(&sealed, &cancelled);
    let sentinel = Some(ExactResolution::Absent);
    let mut cancelled_output = sentinel;
    let pre_cancelled = cancelled_boundary.exact(
        ExactRoute::Healthy,
        &exact_segments,
        &exact_missing_ids,
        ExactOperation::new(b"entity/0"),
        &mut cancelled_output,
    );
    assert!(matches!(
        pre_cancelled,
        RetrievalOperationTerminal::Cancelled {
            snapshot,
            cause: CancellationCause::Preflight,
        } if snapshot == sealed.snapshot.id
    ));
    assert_eq!(cancelled_output, sentinel);

    let graph_authority = GraphAuthority::new(sealed.snapshot.id, ProjectionId::new(4));
    let graph_partition = PartitionId::new(1);
    let graph_edges = [GraphEdge::new(
        graph_authority,
        graph_partition,
        EntityId::new(0),
        EntityId::new(1),
    )];
    let graph_rows = [GraphRow {
        partition: graph_partition,
        edges: &graph_edges,
    }];
    let graph = ValidatedGraphView::try_new(graph_authority, &graph_rows);
    assert!(graph.is_ok());
    let Ok(graph) = graph else {
        return;
    };
    let no_graph_rows: [GraphRow<'_>; 0] = [];
    let partial_graph = ValidatedGraphView::try_new(graph_authority, &no_graph_rows);
    assert!(partial_graph.is_ok());
    let Ok(partial_graph) = partial_graph else {
        return;
    };
    let mut complete_output = [None];
    let complete_graph = boundary.trustfall(
        acquired_terminal(graph_authority, GraphAcquisitionKind::Complete),
        &graph,
        EntityId::new(0),
        &mut complete_output,
    );
    assert!(matches!(
        complete_graph,
        RetrievalOperationTerminal::Complete { .. }
    ));
    assert!(complete_output[0].is_some());

    let mut partial_output = [None];
    let partial_graph_terminal = boundary.trustfall(
        acquired_terminal(graph_authority, GraphAcquisitionKind::Partial),
        &partial_graph,
        EntityId::new(0),
        &mut partial_output,
    );
    assert!(matches!(
        partial_graph_terminal,
        RetrievalOperationTerminal::Partial {
            absence: RetrievalAbsence::Partitions(missing),
            ..
        } if missing.as_ref() == [graph_partition]
    ));
    assert_eq!(partial_output, [None]);

    let mut degraded_output = [None];
    let degraded_graph = boundary.trustfall(
        acquired_terminal(graph_authority, GraphAcquisitionKind::Degraded),
        &graph,
        EntityId::new(0),
        &mut degraded_output,
    );
    assert!(matches!(
        degraded_graph,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Complete,
            degradation: nudox_index_retrieval::RetrievalDegradation::Graph(
                GraphDegradation::StaleRoute
            ),
            ..
        }
    ));
    assert!(degraded_output[0].is_some());

    let mut degraded_partial_output = [None];
    let degraded_partial_graph = boundary.trustfall(
        acquired_terminal(graph_authority, GraphAcquisitionKind::DegradedPartial),
        &partial_graph,
        EntityId::new(0),
        &mut degraded_partial_output,
    );
    assert!(matches!(
        degraded_partial_graph,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Partitions(missing)),
            degradation: nudox_index_retrieval::RetrievalDegradation::Graph(
                GraphDegradation::PartitionSourceUnavailable
            ),
            ..
        } if missing.as_ref() == [graph_partition]
    ));
    assert_eq!(degraded_partial_output, [None]);

    let graph_sentinel = Some(TrustfallHit {
        authority: graph_authority,
        partition: graph_partition,
        entity: EntityId::new(1),
    });
    let mut cancelled_graph_output = [graph_sentinel];
    let cancelled_graph = boundary.trustfall(
        GraphTerminal::Cancelled {
            authority: graph_authority,
        },
        &graph,
        EntityId::new(0),
        &mut cancelled_graph_output,
    );
    assert!(matches!(
        cancelled_graph,
        RetrievalOperationTerminal::Cancelled {
            cause: CancellationCause::GraphAcquisition,
            ..
        }
    ));
    assert_eq!(cancelled_graph_output, [graph_sentinel]);

    let mut failed_graph_output = [graph_sentinel];
    let failed_graph = boundary.trustfall(
        GraphTerminal::Failed {
            authority: graph_authority,
            cause: StreamCapacityError::ProducerDisconnected,
        },
        &graph,
        EntityId::new(0),
        &mut failed_graph_output,
    );
    assert!(matches!(
        failed_graph,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::GraphStream(StreamCapacityError::ProducerDisconnected),
            ..
        }
    ));
    assert_eq!(failed_graph_output, [graph_sentinel]);

    let wrong_snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"other generation"),
        &exact_ids,
        &lexical_ids,
    );
    assert!(wrong_snapshot.is_ok());
    let Ok(wrong_snapshot) = wrong_snapshot else {
        return;
    };
    let wrong_graph_authority = GraphAuthority::new(wrong_snapshot.id, ProjectionId::new(4));
    let graph_partition = PartitionId::new(1);
    let edges = [GraphEdge::new(
        wrong_graph_authority,
        graph_partition,
        EntityId::new(0),
        EntityId::new(1),
    )];
    let rows = [GraphRow {
        partition: graph_partition,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(wrong_graph_authority, &rows);
    assert!(graph.is_ok());
    let Ok(graph) = graph else {
        return;
    };
    let graph_sentinel = Some(TrustfallHit {
        authority: wrong_graph_authority,
        partition: graph_partition,
        entity: EntityId::new(1),
    });
    let mut graph_output = [graph_sentinel];
    let wrong_graph = boundary.trustfall(
        GraphTerminal::Complete {
            authority: wrong_graph_authority,
        },
        &graph,
        EntityId::new(0),
        &mut graph_output,
    );
    assert!(matches!(
        wrong_graph,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::GraphAuthority { expected, observed },
            ..
        } if expected == sealed.snapshot.id && observed == wrong_graph_authority
    ));
    assert_eq!(graph_output, [graph_sentinel]);

    let wrong_vector_authority = VectorAuthority::new(
        wrong_snapshot.id,
        ModelId::new([3; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let qdrant = QdrantBlockingAdapter::new(
        "http://127.0.0.1:1",
        "wrong-sealed-authority",
        wrong_vector_authority,
    );
    assert!(qdrant.is_ok());
    let Ok(qdrant) = qdrant else {
        return;
    };
    let qdrant_sentinel = Some(QdrantCandidate {
        authority: wrong_vector_authority,
        segment: nudox_index_vocab::VectorSegmentId::from_canonical_bytes(b"sentinel"),
        partition: graph_partition,
        entity: EntityId::new(1),
        score: 0.0,
        physical_id: PhysicalPointId(7),
    });
    let mut qdrant_output = [qdrant_sentinel];
    let wrong_qdrant = boundary.qdrant(&qdrant, &[], &[0, 0], 1, &mut qdrant_output);
    assert!(matches!(
        wrong_qdrant,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::VectorAuthority {
                expected,
                surface: VectorAuthoritySurface::QdrantAdapter,
                observed,
            },
            ..
        } if expected == sealed.snapshot.id && observed == wrong_vector_authority
    ));
    assert_eq!(qdrant_output, [qdrant_sentinel]);

    let tantivy_ids = [lexical_present.id];
    let tantivy_snapshot = IndexSnapshot::new(root.id, &exact_ids, &tantivy_ids);
    assert!(tantivy_snapshot.is_ok());
    let Ok(tantivy_snapshot) = tantivy_snapshot else {
        return;
    };
    let tantivy_manifest = LexicalManifest::new(tantivy_snapshot, &lexical_segments, &[]);
    assert!(tantivy_manifest.is_ok());
    let Ok(tantivy_manifest) = tantivy_manifest else {
        return;
    };
    let tantivy = TantivyLexical::build(tantivy_manifest);
    assert!(tantivy.is_ok());
    let Ok(tantivy) = tantivy else {
        return;
    };
    let tantivy_sentinel = Some(TantivyHit { document: 77 });
    let mut tantivy_output = [tantivy_sentinel];
    let typed_adapter_failure = boundary.tantivy(&tantivy, "bool", 1, &mut tantivy_output);
    assert!(matches!(
        typed_adapter_failure,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::Tantivy(TantivyAdapterError::WrongSnapshot {
                expected,
                observed,
            }),
            ..
        } if expected == tantivy_snapshot.id && observed == sealed.snapshot.id
    ));
    assert_eq!(tantivy_output, [tantivy_sentinel]);

    assert!(publisher.shutdown().is_ok());
    assert!(fs::remove_dir_all(directory).is_ok());
}

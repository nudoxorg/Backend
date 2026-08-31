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
    ProjectionId, StreamCapacityError, TraceProbe, ValidatedGraphView, ValidatedVectorSegment,
    VectorAuthority, VectorPoint,
};
use nudox_index_publish::PublishedIndexSnapshot;
use nudox_index_qdrant::{
    PhysicalPointId, QdrantBlockingAdapter, QdrantCandidate, QdrantDataKey, QdrantError,
};
use nudox_index_retrieval::{
    CancellationCause, ExactRoute, LexicalRoute, RetrievalAbsence, RetrievalBoundary,
    RetrievalCoverage, RetrievalFailure, RetrievalOperationTerminal, RetrievalResult,
    VectorAuthoritySurface,
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

#[allow(
    clippy::too_many_lines,
    reason = "the conditional live leg retains provisioning, facade query, mutation cleanup, and typed causes together"
)]
fn query_qdrant_if_provisioned<PayloadOwner>(
    boundary: &RetrievalBoundary<'_, '_, '_, '_, PayloadOwner>,
    segments: &[ValidatedVectorSegment<'_>; 2],
) where
    PayloadOwner: AsRef<[u8]>,
{
    let Ok(endpoint) = std::env::var("QDRANT_URL") else {
        return;
    };
    let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let collection = format!("nudox_retrieval_{}_{}", std::process::id(), ordinal);
    let authority = boundary.vector_authority;
    let adapter = QdrantBlockingAdapter::new(&endpoint, &collection, authority);
    assert!(adapter.is_ok());
    let Ok(adapter) = adapter else {
        return;
    };
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
            nudox_index_retrieval::VectorRoute::Healthy,
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
        ValidatedVectorSegment::try_new(vector_authority, PartitionId::new(1), &first_points),
        ValidatedVectorSegment::try_new(vector_authority, PartitionId::new(2), &second_points),
    ];
    assert!(vector_segments[0].is_ok() && vector_segments[1].is_ok());
    let [Ok(first_vector_segment), Ok(second_vector_segment)] = vector_segments else {
        return;
    };
    let vector_segments = [first_vector_segment, second_vector_segment];
    let vector_selection = [
        vector_segments[0].descriptor(),
        vector_segments[1].descriptor(),
    ];
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        &sealed,
        &cancellation,
        graph_authority,
        vector_authority,
        &vector_selection,
    );
    assert!(boundary.is_ok());
    let Ok(boundary) = boundary else {
        return;
    };
    let exact_complete_segments = [exact_present, exact_missing];
    let mut exact_complete_output = None;
    let exact_complete = boundary.exact(
        ExactRoute::Healthy,
        &exact_complete_segments,
        &[],
        ExactOperation::new(b"entity/0"),
        &mut exact_complete_output,
    );
    assert!(matches!(
        exact_complete,
        RetrievalOperationTerminal::Complete {
            result: RetrievalResult::Exact(ExactResolution::Present {
                value: b"published",
                ..
            }),
            ..
        }
    ));
    assert!(matches!(
        exact_complete_output,
        Some(ExactResolution::Present {
            value: b"published",
            ..
        })
    ));

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

    let top_k = LexicalTopK::new(1);
    assert!(top_k.is_ok());
    let Ok(top_k) = top_k else {
        return;
    };
    let lexical_complete_segments = [lexical_present, lexical_missing];
    let mut lexical_complete_scratch = [None];
    let mut lexical_complete_output = [LexicalSnapshotHit::new(
        lexical_present.id,
        b"sentinel",
        LexicalDocumentId::from(99),
        LexicalScore::from(0),
    )];
    let lexical_complete = boundary.lexical(
        LexicalRoute::Healthy,
        &lexical_complete_segments,
        &[],
        LexicalOperation::new(b"bool"),
        top_k,
        &mut lexical_complete_scratch,
        &mut lexical_complete_output,
    );
    assert!(matches!(
        lexical_complete,
        RetrievalOperationTerminal::Complete {
            result: RetrievalResult::Lexical(hits),
            ..
        } if hits.first().is_some_and(|hit| hit.term == b"bool")
    ));
    assert_eq!(lexical_complete_output[0].term, b"bool");

    let lexical_segments = [lexical_present];
    let lexical_missing_ids = [lexical_missing.id];
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
    let cancelled_boundary = RetrievalBoundary::new(
        &sealed,
        &cancelled,
        graph_authority,
        vector_authority,
        &vector_selection,
    );
    assert!(cancelled_boundary.is_ok());
    let Ok(cancelled_boundary) = cancelled_boundary else {
        return;
    };
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
            cause: RetrievalFailure::PinnedGraphAuthority { expected, observed },
            ..
        } if expected == graph_authority && observed == wrong_graph_authority
    ));
    assert_eq!(graph_output, [graph_sentinel]);

    let other_projection_authority = GraphAuthority::new(sealed.snapshot.id, ProjectionId::new(5));
    let other_projection_edges = [GraphEdge::new(
        other_projection_authority,
        graph_partition,
        EntityId::new(0),
        EntityId::new(1),
    )];
    let other_projection_rows = [GraphRow {
        partition: graph_partition,
        edges: &other_projection_edges,
    }];
    let other_projection_graph =
        ValidatedGraphView::try_new(other_projection_authority, &other_projection_rows);
    assert!(other_projection_graph.is_ok());
    let Ok(other_projection_graph) = other_projection_graph else {
        return;
    };
    let mut other_projection_output = [graph_sentinel];
    let other_projection = boundary.trustfall(
        GraphTerminal::Complete {
            authority: other_projection_authority,
        },
        &other_projection_graph,
        EntityId::new(0),
        &mut other_projection_output,
    );
    assert!(matches!(
        other_projection,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::PinnedGraphAuthority { expected, observed },
            ..
        } if expected == graph_authority && observed == other_projection_authority
    ));
    assert_eq!(other_projection_output, [graph_sentinel]);

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
        authority: vector_authority,
        segment: nudox_index_vocab::VectorSegmentId::from_canonical_bytes(b"sentinel"),
        partition: graph_partition,
        entity: EntityId::new(1),
        score: 0.0,
        physical_id: PhysicalPointId(7),
    });
    let mut qdrant_output = [qdrant_sentinel];
    let wrong_qdrant = boundary.qdrant(
        nudox_index_retrieval::VectorRoute::Healthy,
        &qdrant,
        &[],
        &[0, 0],
        1,
        &mut qdrant_output,
    );
    assert!(matches!(
        wrong_qdrant,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::VectorAuthority {
                expected,
                surface: VectorAuthoritySurface::QdrantAdapter,
                observed,
            },
            ..
        } if expected == vector_authority && observed == wrong_vector_authority
    ));
    assert_eq!(qdrant_output, [qdrant_sentinel]);

    let same_snapshot_vector_mutants = [
        VectorAuthority::new(
            sealed.snapshot.id,
            ModelId::new([0x72; 16]),
            2,
            Metric::SquaredEuclidean,
        ),
        VectorAuthority::new(
            sealed.snapshot.id,
            ModelId::new([0x71; 16]),
            3,
            Metric::SquaredEuclidean,
        ),
        VectorAuthority::new(
            sealed.snapshot.id,
            ModelId::new([0x71; 16]),
            2,
            Metric::NegativeDotProduct,
        ),
    ];
    for (position, observed_authority) in same_snapshot_vector_mutants.into_iter().enumerate() {
        let adapter = QdrantBlockingAdapter::new(
            "http://127.0.0.1:1",
            "same-snapshot-vector-authority",
            observed_authority,
        );
        assert!(adapter.is_ok());
        let Ok(adapter) = adapter else {
            return;
        };
        let mut output = [qdrant_sentinel];
        let terminal = boundary.qdrant(
            nudox_index_retrieval::VectorRoute::Healthy,
            &adapter,
            &[],
            &[0, 0],
            1,
            &mut output,
        );
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Failed {
                cause: RetrievalFailure::VectorAuthority {
                    expected,
                    surface: VectorAuthoritySurface::QdrantAdapter,
                    observed,
                },
                ..
            } if expected == vector_authority && observed == observed_authority
        ));
        assert_eq!(
            output,
            [qdrant_sentinel],
            "mutant {position} changed output"
        );
    }

    let correct_qdrant = QdrantBlockingAdapter::new(
        "http://127.0.0.1:1",
        "sealed-vector-coverage",
        vector_authority,
    );
    assert!(correct_qdrant.is_ok());
    let Ok(correct_qdrant) = correct_qdrant else {
        return;
    };
    let mut partial_qdrant_output = [qdrant_sentinel];
    let partial_qdrant = boundary.qdrant(
        nudox_index_retrieval::VectorRoute::Healthy,
        &correct_qdrant,
        &[],
        &[0, 0],
        1,
        &mut partial_qdrant_output,
    );
    assert!(matches!(
        partial_qdrant,
        RetrievalOperationTerminal::Partial {
            absence: RetrievalAbsence::Partitions(missing),
            ..
        } if missing.as_ref() == [PartitionId::new(1), PartitionId::new(2)]
    ));
    assert_eq!(partial_qdrant_output, [None]);

    let mut degraded_qdrant_output = [qdrant_sentinel];
    let degraded_qdrant = boundary.qdrant(
        nudox_index_retrieval::VectorRoute::Degraded(
            nudox_index_retrieval::VectorDegradation::PartitionSourceUnavailable,
        ),
        &correct_qdrant,
        &[],
        &[0, 0],
        1,
        &mut degraded_qdrant_output,
    );
    assert!(matches!(
        degraded_qdrant,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Partitions(missing)),
            degradation: nudox_index_retrieval::RetrievalDegradation::Vector(
                nudox_index_retrieval::VectorDegradation::PartitionSourceUnavailable
            ),
            ..
        } if missing.as_ref() == [PartitionId::new(1), PartitionId::new(2)]
    ));
    assert_eq!(degraded_qdrant_output, [None]);

    let unselected_coordinates = [2_i16, 0];
    let unselected_points = [VectorPoint::new(EntityId::new(7), &unselected_coordinates)];
    let unselected_segment =
        ValidatedVectorSegment::try_new(vector_authority, PartitionId::new(3), &unselected_points);
    assert!(unselected_segment.is_ok());
    let Ok(unselected_segment) = unselected_segment else {
        return;
    };
    let unselected_descriptor = [unselected_segment.descriptor()];
    let mut unselected_output = [qdrant_sentinel];
    let unselected = boundary.qdrant(
        nudox_index_retrieval::VectorRoute::Healthy,
        &correct_qdrant,
        &unselected_descriptor,
        &[0, 0],
        1,
        &mut unselected_output,
    );
    assert!(matches!(
        unselected,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::UnpinnedVectorDescriptor { position: 0, observed },
            ..
        } if observed == unselected_descriptor[0]
    ));
    assert_eq!(unselected_output, [qdrant_sentinel]);

    let tantivy_complete_manifest =
        LexicalManifest::new(sealed.snapshot, &lexical_complete_segments, &[]);
    assert!(tantivy_complete_manifest.is_ok());
    let Ok(tantivy_complete_manifest) = tantivy_complete_manifest else {
        return;
    };
    let tantivy_complete = TantivyLexical::build(tantivy_complete_manifest);
    assert!(tantivy_complete.is_ok());
    let Ok(tantivy_complete) = tantivy_complete else {
        return;
    };
    let mut tantivy_complete_output = [None];
    let tantivy_complete_terminal =
        boundary.tantivy(&tantivy_complete, "bool", 1, &mut tantivy_complete_output);
    assert!(matches!(
        tantivy_complete_terminal,
        RetrievalOperationTerminal::Complete {
            result: RetrievalResult::Tantivy(terminal),
            ..
        } if terminal.written == 1
    ));
    assert_eq!(tantivy_complete_output, [Some(TantivyHit { document: 0 })]);

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

    query_qdrant_if_provisioned(&boundary, &vector_segments);

    assert!(publisher.shutdown().is_ok());
    assert!(fs::remove_dir_all(directory).is_ok());
}

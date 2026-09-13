//! Exercises the `server-index-retrieval` tests sealed-boundary-public contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Public sealed-snapshot retrieval journey, including the optional live Qdrant facade leg.

#![allow(
    clippy::cognitive_complexity,
    clippy::expect_used,
    reason = "each function exercises one typed public terminal family over local checked fixture setup"
)]

mod support;

use compiler_ir::EntityId;
use server_index_core::{
    ExactDegradation, ExactOperation, ExactResolution, LexicalDegradation, LexicalManifest,
    LexicalOperation, LexicalScore, LexicalSnapshotHit, LexicalTopK,
};
use server_index_graph_vector::{
    Cancellation, GraphDegradation, GraphEdge, GraphRow, GraphTerminal, PartitionId,
    StreamCapacityError, ValidatedGraphView,
};
use server_index_qdrant::{PhysicalPointId, QdrantBlockingAdapter, QdrantCandidate};
use server_index_retrieval::{
    CancellationCause, ExactRoute, LexicalRoute, RetrievalAbsence, RetrievalBoundary,
    RetrievalCoverage, RetrievalDegradation, RetrievalFailure, RetrievalOperationTerminal,
    RetrievalResult, VectorDegradation, VectorRoute,
};
use server_index_tantivy::{TantivyHit, TantivyLexical};
use server_index_trustfall::TrustfallHit;

use support::{
    GraphAcquisitionKind, SealedFixture, acquired_terminal, lexical_document,
    query_qdrant_if_provisioned, with_sealed_fixture,
};

#[test]
fn sealed_boundary_public_journey_classifies_retrieval_terminals() {
    with_sealed_fixture(assert_complete_exact_lexical_and_tantivy);
    with_sealed_fixture(assert_partial_and_degraded_exact_lexical);
    with_sealed_fixture(assert_pre_cancel_preserves_exact_sentinel);
    with_sealed_fixture(assert_graph_success_terminals);
    with_sealed_fixture(assert_graph_cancelled_and_failed_terminals);
    with_sealed_fixture(assert_qdrant_coverage_and_live_facade_journey);
}

fn assert_complete_exact_lexical_and_tantivy(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid retrieval boundary");
    let mut exact_output = None;
    let exact = boundary.exact(
        ExactRoute::Healthy,
        &fixture.exact,
        &[],
        ExactOperation::new(b"entity/0"),
        &mut exact_output,
    );
    assert!(matches!(
        exact,
        RetrievalOperationTerminal::Complete {
            result: RetrievalResult::Exact(ExactResolution::Present {
                value: b"published",
                ..
            }),
            ..
        }
    ));
    assert!(matches!(
        exact_output,
        Some(ExactResolution::Present {
            value: b"published",
            ..
        })
    ));

    let mut scratch = [None];
    let mut lexical_output = [lexical_sentinel(fixture)];
    let lexical = boundary.lexical(
        LexicalRoute::Healthy,
        &fixture.lexical,
        &[],
        LexicalOperation::new(b"bool"),
        top_k(),
        &mut scratch,
        &mut lexical_output,
    );
    assert!(matches!(
        lexical,
        RetrievalOperationTerminal::Complete {
            result: RetrievalResult::Lexical(hits),
            ..
        } if hits.first().is_some_and(|hit| hit.term == b"bool")
    ));
    assert_eq!(lexical_output[0].term, b"bool");

    let manifest = LexicalManifest::new(fixture.sealed.snapshot, &fixture.lexical, &[])
        .expect("complete lexical manifest");
    let tantivy = TantivyLexical::build(manifest).expect("Tantivy projection");
    let mut tantivy_output = [None];
    let terminal = boundary.tantivy(&tantivy, "bool", 1, &mut tantivy_output);
    assert!(matches!(
        terminal,
        RetrievalOperationTerminal::Complete {
            result: RetrievalResult::Tantivy(result),
            ..
        } if result.written == 1
    ));
    assert_eq!(
        tantivy_output,
        [Some(TantivyHit {
            document: lexical_document(0)
        })]
    );
}

fn assert_partial_and_degraded_exact_lexical(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid retrieval boundary");
    let exact_segments = [fixture.exact[0]];
    let exact_missing = [fixture.exact[1].id];
    let mut exact_output = None;
    let partial = boundary.exact(
        ExactRoute::Healthy,
        &exact_segments,
        &exact_missing,
        ExactOperation::new(b"entity/0"),
        &mut exact_output,
    );
    assert!(matches!(
        partial,
        RetrievalOperationTerminal::Partial {
            absence: RetrievalAbsence::Exact(missing),
            ..
        } if missing == exact_missing
    ));
    let degraded = boundary.exact(
        ExactRoute::Degraded(ExactDegradation::StaleRoute),
        &exact_segments,
        &exact_missing,
        ExactOperation::new(b"entity/0"),
        &mut exact_output,
    );
    assert!(matches!(
        degraded,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Exact(missing)),
            degradation: RetrievalDegradation::Exact(ExactDegradation::StaleRoute),
            ..
        } if missing == exact_missing
    ));

    let lexical_segments = [fixture.lexical[0]];
    let lexical_missing = [fixture.lexical[1].id];
    let mut scratch = [None];
    let mut output = [lexical_sentinel(fixture)];
    let partial = boundary.lexical(
        LexicalRoute::Healthy,
        &lexical_segments,
        &lexical_missing,
        LexicalOperation::new(b"bool"),
        top_k(),
        &mut scratch,
        &mut output,
    );
    assert!(matches!(
        partial,
        RetrievalOperationTerminal::Partial {
            absence: RetrievalAbsence::Lexical(missing),
            ..
        } if missing == lexical_missing
    ));
    let mut degraded_scratch = [None];
    let degraded = boundary.lexical(
        LexicalRoute::Degraded(LexicalDegradation::SegmentSourceUnavailable),
        &lexical_segments,
        &lexical_missing,
        LexicalOperation::new(b"bool"),
        top_k(),
        &mut degraded_scratch,
        &mut output,
    );
    assert!(matches!(
        degraded,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Lexical(missing)),
            degradation: RetrievalDegradation::Lexical(
                LexicalDegradation::SegmentSourceUnavailable
            ),
            ..
        } if missing == lexical_missing
    ));
}

fn assert_pre_cancel_preserves_exact_sentinel(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    cancellation.cancel();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid cancelled retrieval boundary");
    let sentinel = Some(ExactResolution::Absent);
    let mut output = sentinel;
    let exact_segments = [fixture.exact[0]];
    let exact_missing = [fixture.exact[1].id];
    let terminal = boundary.exact(
        ExactRoute::Healthy,
        &exact_segments,
        &exact_missing,
        ExactOperation::new(b"entity/0"),
        &mut output,
    );
    assert!(matches!(
        terminal,
        RetrievalOperationTerminal::Cancelled {
            snapshot,
            cause: CancellationCause::Preflight,
        } if snapshot == fixture.sealed.snapshot.id
    ));
    assert_eq!(output, sentinel);
}

fn assert_graph_success_terminals(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid retrieval boundary");
    let partition = PartitionId::new(1);
    let edges = [GraphEdge::new(
        fixture.graph_authority,
        partition,
        EntityId::new(0),
        EntityId::new(1),
    )];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(fixture.graph_authority, &rows).expect("graph view");
    let no_rows: [GraphRow<'_>; 0] = [];
    let empty = ValidatedGraphView::try_new(fixture.graph_authority, &no_rows).expect("empty view");
    let mut complete_output = [None];
    let complete = boundary.trustfall(
        acquired_terminal(fixture.graph_authority, GraphAcquisitionKind::Complete),
        &graph,
        EntityId::new(0),
        &mut complete_output,
    );
    assert!(matches!(
        complete,
        RetrievalOperationTerminal::Complete { .. }
    ));
    let mut partial_output = [None];
    let partial = boundary.trustfall(
        acquired_terminal(fixture.graph_authority, GraphAcquisitionKind::Partial),
        &empty,
        EntityId::new(0),
        &mut partial_output,
    );
    assert!(matches!(
        partial,
        RetrievalOperationTerminal::Partial {
            absence: RetrievalAbsence::Partitions(missing),
            ..
        } if missing.as_ref() == [partition]
    ));
    let mut degraded_output = [None];
    let degraded = boundary.trustfall(
        acquired_terminal(fixture.graph_authority, GraphAcquisitionKind::Degraded),
        &graph,
        EntityId::new(0),
        &mut degraded_output,
    );
    assert!(matches!(
        degraded,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Complete,
            degradation: RetrievalDegradation::Graph(GraphDegradation::StaleRoute),
            ..
        }
    ));
    let mut degraded_partial_output = [None];
    let degraded_partial = boundary.trustfall(
        acquired_terminal(
            fixture.graph_authority,
            GraphAcquisitionKind::DegradedPartial,
        ),
        &empty,
        EntityId::new(0),
        &mut degraded_partial_output,
    );
    assert!(matches!(
        degraded_partial,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Partitions(missing)),
            degradation: RetrievalDegradation::Graph(GraphDegradation::PartitionSourceUnavailable),
            ..
        } if missing.as_ref() == [partition]
    ));
}

fn assert_graph_cancelled_and_failed_terminals(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid retrieval boundary");
    let partition = PartitionId::new(1);
    let edges = [GraphEdge::new(
        fixture.graph_authority,
        partition,
        EntityId::new(0),
        EntityId::new(1),
    )];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(fixture.graph_authority, &rows).expect("graph view");
    let sentinel = Some(TrustfallHit {
        authority: fixture.graph_authority,
        partition,
        entity: EntityId::new(1),
    });
    let mut cancelled_output = [sentinel];
    let cancelled = boundary.trustfall(
        GraphTerminal::Cancelled {
            authority: fixture.graph_authority,
        },
        &graph,
        EntityId::new(0),
        &mut cancelled_output,
    );
    assert!(matches!(
        cancelled,
        RetrievalOperationTerminal::Cancelled {
            cause: CancellationCause::GraphAcquisition,
            ..
        }
    ));
    assert_eq!(cancelled_output, [sentinel]);
    let mut failed_output = [sentinel];
    let failed = boundary.trustfall(
        GraphTerminal::Failed {
            authority: fixture.graph_authority,
            cause: StreamCapacityError::ProducerDisconnected,
        },
        &graph,
        EntityId::new(0),
        &mut failed_output,
    );
    assert!(matches!(
        failed,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::GraphStream(StreamCapacityError::ProducerDisconnected),
            ..
        }
    ));
    assert_eq!(failed_output, [sentinel]);
}

fn assert_qdrant_coverage_and_live_facade_journey(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid retrieval boundary");
    let adapter = QdrantBlockingAdapter::new(
        "http://127.0.0.1:1",
        "sealed-vector-coverage",
        fixture.vector_authority,
    )
    .expect("valid offline Qdrant adapter");
    let sentinel = Some(QdrantCandidate {
        authority: fixture.vector_authority,
        segment: server_index_vocabulary::VectorSegmentId::from_canonical_bytes(b"sentinel"),
        partition: PartitionId::new(1),
        entity: EntityId::new(1),
        score: 0.0,
        physical_id: PhysicalPointId(7),
    });
    let mut partial_output = [sentinel];
    let partial = boundary.qdrant(
        VectorRoute::Healthy,
        &adapter,
        &[],
        &[0, 0],
        1,
        &mut partial_output,
    );
    assert!(matches!(
        partial,
        RetrievalOperationTerminal::Partial {
            absence: RetrievalAbsence::Partitions(missing),
            ..
        } if missing.as_ref() == [PartitionId::new(1), PartitionId::new(2)]
    ));
    assert_eq!(partial_output, [None]);
    let mut degraded_output = [sentinel];
    let degraded = boundary.qdrant(
        VectorRoute::Degraded(VectorDegradation::PartitionSourceUnavailable),
        &adapter,
        &[],
        &[0, 0],
        1,
        &mut degraded_output,
    );
    assert!(matches!(
        degraded,
        RetrievalOperationTerminal::Degraded {
            coverage: RetrievalCoverage::Missing(RetrievalAbsence::Partitions(missing)),
            degradation: RetrievalDegradation::Vector(VectorDegradation::PartitionSourceUnavailable),
            ..
        } if missing.as_ref() == [PartitionId::new(1), PartitionId::new(2)]
    ));
    query_qdrant_if_provisioned(&boundary, fixture.vector_segments);
}

fn top_k() -> LexicalTopK {
    LexicalTopK::new(1).expect("bounded lexical top-k")
}

fn lexical_sentinel<'fixture>(fixture: &SealedFixture<'fixture>) -> LexicalSnapshotHit<'fixture> {
    LexicalSnapshotHit::new(
        fixture.lexical[0].id,
        b"sentinel",
        lexical_document(99),
        LexicalScore::from(0),
    )
}

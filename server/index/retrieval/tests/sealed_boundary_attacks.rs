//! Exercises the `server-index-retrieval` tests sealed-boundary-attacks contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Authority and selection mutants against the sealed retrieval server boundary.

#![allow(
    clippy::expect_used,
    reason = "each function exercises one typed attack family over local checked fixture setup"
)]

mod support;

use compiler_ir_vocabulary::EntityId;
use heart_identity::GenerationId;
use server_index_core::{IndexSnapshot, LexicalManifest};
use server_index_graph_vector::{
    Cancellation, GraphAuthority, GraphEdge, GraphRow, GraphTerminal, Metric, ModelId, PartitionId,
    ProjectionId, ValidatedGraphView, ValidatedVectorSegment, VectorAuthority, VectorPoint,
};
use server_index_qdrant::{PhysicalPointId, QdrantBlockingAdapter, QdrantCandidate};
use server_index_retrieval::{
    RetrievalBoundary, RetrievalFailure, RetrievalOperationTerminal, VectorAuthoritySurface,
    VectorRoute,
};
use server_index_tantivy::{TantivyAdapterError, TantivyHit, TantivyLexical};
use server_index_trustfall::TrustfallHit;

use support::{SealedFixture, lexical_document, with_sealed_fixture};

#[test]
fn sealed_boundary_rejects_a_foreign_graph_authority_before_mutating_output() {
    with_sealed_fixture(reject_foreign_graph_authority);
}

#[test]
fn sealed_boundary_rejects_another_projection_of_the_same_snapshot() {
    with_sealed_fixture(reject_other_graph_projection);
}

#[test]
fn sealed_boundary_rejects_foreign_qdrant_adapter_authority_before_transport() {
    with_sealed_fixture(reject_foreign_qdrant_authority);
}

#[test]
fn sealed_boundary_rejects_qdrant_model_dimension_and_metric_mutants() {
    with_sealed_fixture(reject_same_snapshot_qdrant_authority_mutants);
}

#[test]
fn sealed_boundary_rejects_unselected_vector_descriptors_and_wrong_tantivy_snapshot() {
    with_sealed_fixture(reject_unselected_descriptor_and_wrong_tantivy);
}

fn reject_foreign_graph_authority(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid boundary");
    let exact_ids = fixture.exact.map(|segment| segment.id);
    let lexical_ids = fixture.lexical.map(|segment| segment.id);
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"other generation"),
        &exact_ids,
        &lexical_ids,
    )
    .expect("valid foreign snapshot");
    let observed = GraphAuthority::new(snapshot.id, ProjectionId::new(4));
    let partition = PartitionId::new(1);
    let edges = [GraphEdge::new(
        observed,
        partition,
        EntityId::new(0),
        EntityId::new(1),
    )];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(observed, &rows).expect("foreign graph view");
    let sentinel = Some(graph_sentinel(observed, partition));
    let mut output = [sentinel];
    let terminal = boundary.trustfall(
        GraphTerminal::Complete {
            authority: observed,
        },
        &graph,
        EntityId::new(0),
        &mut output,
    );
    assert!(matches!(
        terminal,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::PinnedGraphAuthority { expected, observed: actual },
            ..
        } if expected == fixture.graph_authority && actual == observed
    ));
    assert_eq!(output, [sentinel]);
}

fn reject_other_graph_projection(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid boundary");
    let observed = GraphAuthority::new(fixture.sealed.snapshot.id, ProjectionId::new(5));
    let partition = PartitionId::new(1);
    let edges = [GraphEdge::new(
        observed,
        partition,
        EntityId::new(0),
        EntityId::new(1),
    )];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(observed, &rows).expect("alternate graph view");
    let sentinel = Some(graph_sentinel(observed, partition));
    let mut output = [sentinel];
    let terminal = boundary.trustfall(
        GraphTerminal::Complete {
            authority: observed,
        },
        &graph,
        EntityId::new(0),
        &mut output,
    );
    assert!(matches!(
        terminal,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::PinnedGraphAuthority { expected, observed: actual },
            ..
        } if expected == fixture.graph_authority && actual == observed
    ));
    assert_eq!(output, [sentinel]);
}

fn reject_foreign_qdrant_authority(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid boundary");
    let exact_ids = fixture.exact.map(|segment| segment.id);
    let lexical_ids = fixture.lexical.map(|segment| segment.id);
    let snapshot = IndexSnapshot::new(
        GenerationId::from_canonical_bytes(b"other generation"),
        &exact_ids,
        &lexical_ids,
    )
    .expect("valid foreign snapshot");
    let observed = VectorAuthority::new(
        snapshot.id,
        ModelId::new([3; 16]),
        2,
        Metric::SquaredEuclidean,
    );
    let adapter = QdrantBlockingAdapter::new("http://127.0.0.1:1", "foreign", observed)
        .expect("valid foreign adapter");
    let sentinel = Some(qdrant_sentinel(fixture));
    let mut output = [sentinel];
    let terminal = boundary.qdrant(VectorRoute::Healthy, &adapter, &[], &[0, 0], 1, &mut output);
    assert!(matches!(
        terminal,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::VectorAuthority {
                expected,
                surface: VectorAuthoritySurface::QdrantAdapter,
                observed: actual,
            },
            ..
        } if expected == fixture.vector_authority && actual == observed
    ));
    assert_eq!(output, [sentinel]);
}

fn reject_same_snapshot_qdrant_authority_mutants(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid boundary");
    let mutants = [
        VectorAuthority::new(
            fixture.sealed.snapshot.id,
            ModelId::new([0x72; 16]),
            2,
            Metric::SquaredEuclidean,
        ),
        VectorAuthority::new(
            fixture.sealed.snapshot.id,
            ModelId::new([0x71; 16]),
            3,
            Metric::SquaredEuclidean,
        ),
        VectorAuthority::new(
            fixture.sealed.snapshot.id,
            ModelId::new([0x71; 16]),
            2,
            Metric::NegativeDotProduct,
        ),
    ];
    for observed in mutants {
        let adapter = QdrantBlockingAdapter::new("http://127.0.0.1:1", "mutant", observed)
            .expect("valid mutant adapter");
        let sentinel = Some(qdrant_sentinel(fixture));
        let mut output = [sentinel];
        let terminal =
            boundary.qdrant(VectorRoute::Healthy, &adapter, &[], &[0, 0], 1, &mut output);
        assert!(matches!(
            terminal,
            RetrievalOperationTerminal::Failed {
                cause: RetrievalFailure::VectorAuthority {
                    expected,
                    surface: VectorAuthoritySurface::QdrantAdapter,
                    observed: actual,
                },
                ..
            } if expected == fixture.vector_authority && actual == observed
        ));
        assert_eq!(output, [sentinel]);
    }
}

fn reject_unselected_descriptor_and_wrong_tantivy(fixture: &SealedFixture<'_>) {
    let selection = fixture.vector_segments.map(|segment| segment.descriptor());
    let cancellation = Cancellation::new();
    let boundary = RetrievalBoundary::new(
        fixture.sealed,
        &cancellation,
        fixture.graph_authority,
        fixture.vector_authority,
        &selection,
    )
    .expect("valid boundary");
    let adapter = QdrantBlockingAdapter::new(
        "http://127.0.0.1:1",
        "boundary-authority",
        fixture.vector_authority,
    )
    .expect("valid adapter");
    let coordinates = [2_i16, 0];
    let points = [VectorPoint::new(EntityId::new(7), &coordinates)];
    let segment =
        ValidatedVectorSegment::try_new(fixture.vector_authority, PartitionId::new(3), &points)
            .expect("valid unselected segment");
    let descriptors = [segment.descriptor()];
    let sentinel = Some(qdrant_sentinel(fixture));
    let mut output = [sentinel];
    let terminal = boundary.qdrant(
        VectorRoute::Healthy,
        &adapter,
        &descriptors,
        &[0, 0],
        1,
        &mut output,
    );
    assert!(matches!(
        terminal,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::UnpinnedVectorDescriptor { position: 0, observed },
            ..
        } if observed == descriptors[0]
    ));
    assert_eq!(output, [sentinel]);

    let ids = [fixture.lexical[0].id];
    let segments = [fixture.lexical[0]];
    let exact_ids = fixture.exact.map(|segment| segment.id);
    let snapshot = IndexSnapshot::new(fixture.sealed.snapshot.generation, &exact_ids, &ids)
        .expect("alternate lexical snapshot");
    let manifest = LexicalManifest::new(snapshot, &segments, &[]).expect("alternate manifest");
    let tantivy = TantivyLexical::build(manifest).expect("alternate Tantivy");
    let sentinel = Some(TantivyHit {
        document: lexical_document(77),
    });
    let mut output = [sentinel];
    let terminal = boundary.tantivy(&tantivy, "bool", 1, &mut output);
    assert!(matches!(
        terminal,
        RetrievalOperationTerminal::Failed {
            cause: RetrievalFailure::Tantivy(TantivyAdapterError::WrongSnapshot {
                expected,
                observed,
            }),
            ..
        } if expected == snapshot.id && observed == fixture.sealed.snapshot.id
    ));
    assert_eq!(output, [sentinel]);
}

const fn graph_sentinel(authority: GraphAuthority, partition: PartitionId) -> TrustfallHit {
    TrustfallHit {
        authority,
        partition,
        entity: EntityId::new(1),
    }
}

fn qdrant_sentinel(fixture: &SealedFixture<'_>) -> QdrantCandidate {
    QdrantCandidate {
        authority: fixture.vector_authority,
        segment: server_index_vocabulary::VectorSegmentId::from_canonical_bytes(b"sentinel"),
        partition: PartitionId::new(1),
        entity: EntityId::new(1),
        score: 0.0,
        physical_id: PhysicalPointId(7),
    }
}

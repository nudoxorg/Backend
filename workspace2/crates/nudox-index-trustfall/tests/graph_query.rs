use nudox_index_graph_vector::{
    GraphAuthority, GraphEdge, GraphRow, PartitionId, ProjectionId, ValidatedGraphView,
};
use nudox_index_trustfall::{TrustfallGraph, TrustfallHit};
use nudox_index_vocab::IndexSnapshotId;
use nudox_ir_vocab::EntityId;

fn graph_authority() -> GraphAuthority {
    GraphAuthority::new(
        IndexSnapshotId::from_canonical_bytes(b"trustfall-pinned-graph-snapshot"),
        ProjectionId::new(9),
    )
}

#[test]
fn synchronous_trustfall_reads_only_the_pinned_validated_graph_view() {
    let authority = graph_authority();
    let first = PartitionId::new(1);
    let second = PartitionId::new(2);
    let first_edges = [
        GraphEdge::new(authority, first, EntityId::new(4), EntityId::new(7)),
        GraphEdge::new(authority, first, EntityId::new(4), EntityId::new(9)),
    ];
    let second_edges = [GraphEdge::new(
        authority,
        second,
        EntityId::new(4),
        EntityId::new(11),
    )];
    let rows = [
        GraphRow {
            partition: first,
            edges: &first_edges,
        },
        GraphRow {
            partition: second,
            edges: &second_edges,
        },
    ];
    let view = ValidatedGraphView::try_new(authority, &rows).expect("valid pinned graph");
    let graph = TrustfallGraph::new(&view);
    let mut output = [None; 3];

    let terminal = graph
        .neighbors(EntityId::new(4), &mut output)
        .expect("static Trustfall query must execute");

    assert_eq!(terminal.authority, authority);
    assert_eq!(terminal.written, 3);
    assert_eq!(
        output.map(|hit| hit.map(|hit| (hit.entity, hit.partition))),
        [
            Some((EntityId::new(7), first)),
            Some((EntityId::new(9), first)),
            Some((EntityId::new(11), second)),
        ]
    );
}

#[test]
fn synchronous_trustfall_preserves_source_isolation_and_full_entity_identity() {
    let authority = graph_authority();
    let partition = PartitionId::new(3);
    let high_source = EntityId::new(u32::MAX);
    let edges = [
        GraphEdge::new(authority, partition, high_source, EntityId::new(5)),
        GraphEdge::new(authority, partition, EntityId::new(4), EntityId::new(6)),
    ];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let view = ValidatedGraphView::try_new(authority, &rows).expect("valid pinned graph");
    let graph = TrustfallGraph::new(&view);
    let mut output = [Some(TrustfallHit {
        authority,
        partition,
        entity: EntityId::new(0),
    })];

    let terminal = graph
        .neighbors(high_source, &mut output)
        .expect("full u32 source identity must survive Trustfall filters");

    assert_eq!(terminal.written, 1);
    assert_eq!(output[0].map(|hit| hit.entity), Some(EntityId::new(5)));
}

#[test]
fn insufficient_output_retains_caller_slots_before_trustfall_execution() {
    let authority = graph_authority();
    let partition = PartitionId::new(4);
    let edges = [
        GraphEdge::new(authority, partition, EntityId::new(8), EntityId::new(1)),
        GraphEdge::new(authority, partition, EntityId::new(8), EntityId::new(2)),
    ];
    let rows = [GraphRow {
        partition,
        edges: &edges,
    }];
    let view = ValidatedGraphView::try_new(authority, &rows).expect("valid pinned graph");
    let graph = TrustfallGraph::new(&view);
    let sentinel = TrustfallHit {
        authority,
        partition,
        entity: EntityId::new(99),
    };
    let mut output = [Some(sentinel)];

    let rejected = graph
        .neighbors(EntityId::new(8), &mut output)
        .expect_err("capacity must be checked before a query can mutate output");

    assert_eq!(
        rejected,
        nudox_index_trustfall::TrustfallGraphError::InsufficientOutput {
            required: 2,
            available: 1,
        }
    );
    assert_eq!(output, [Some(sentinel)]);
}

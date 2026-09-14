//! Exercises the `backend-semantic::graph_vector` tests graph-api contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_semantic::ir::EntityId;
use backend_semantic::graph_vector::{
    GraphAuthority, GraphEdge, GraphQueryTerminal, GraphRow, PartitionId, ProjectionId,
    ValidatedGraphView,
};
use backend_semantic::index_vocabulary::IndexSnapshotId;

fn authority() -> GraphAuthority {
    GraphAuthority {
        snapshot: IndexSnapshotId::from_canonical_bytes(&[1; 32]),
        projection: ProjectionId { raw: 3 },
    }
}

#[test]
fn terminal_cardinality_is_closed_and_preserves_selection_order() -> Result<(), String> {
    let authority = authority();
    let present = PartitionId { raw: 0 };
    let selected = [
        PartitionId { raw: 0 },
        PartitionId { raw: 1 },
        PartitionId { raw: 2 },
        PartitionId { raw: 3 },
    ];
    let edges = [GraphEdge {
        authority,
        partition: present,
        source: EntityId::new(1),
        target: EntityId::new(2),
    }];
    let rows = [GraphRow {
        partition: present,
        edges: &edges,
    }];
    let graph = ValidatedGraphView::try_new(authority, &rows)
        .map_err(|error| format!("valid graph rejected: {error:?}"))?;
    let mut output = [None; 1];
    let selections: [&[PartitionId]; 5] = [
        &[],
        &selected[..1],
        &selected[..2],
        &selected[..3],
        &selected[..4],
    ];
    for selection in selections {
        let outcome = graph
            .neighbors(selection, EntityId::new(99), &mut output)
            .map_err(|error| format!("valid query rejected: {error:?}"))?;
        assert_eq!(outcome.written, 0);
        match (selection.len(), outcome.terminal) {
            (
                0 | 1,
                GraphQueryTerminal::Complete {
                    authority: observed,
                },
            ) => {
                assert_eq!(observed, authority);
            }
            (
                _,
                GraphQueryTerminal::Partial {
                    authority: observed,
                    missing,
                },
            ) => {
                assert_eq!(observed, authority);
                assert_eq!(missing.as_ref(), &selection[1..]);
            }
            (_, GraphQueryTerminal::Complete { .. }) => {
                return Err("an absent selected partition produced a complete terminal".to_owned());
            }
        }
    }
    Ok(())
}

#[test]
fn empty_selection_and_available_empty_rows_are_complete() {
    let authority = authority();
    let present = PartitionId { raw: 0 };
    let empty_rows = [GraphRow {
        partition: present,
        edges: &[],
    }];
    let graph = ValidatedGraphView::try_new(authority, &empty_rows);
    assert!(graph.is_ok());
    let Ok(graph) = graph else {
        return;
    };
    let mut output = [None; 1];

    let empty = graph.neighbors(&[], EntityId::new(1), &mut output);
    assert!(empty.is_ok());
    let Ok(empty) = empty else {
        return;
    };
    assert_eq!(empty.written, 0);
    assert!(matches!(
        empty.terminal,
        GraphQueryTerminal::Complete { authority: observed } if observed == authority
    ));

    let available_empty = graph.neighbors(&[present], EntityId::new(1), &mut output);
    assert!(available_empty.is_ok());
    let Ok(available_empty) = available_empty else {
        return;
    };
    assert_eq!(available_empty.written, 0);
    assert!(matches!(
        available_empty.terminal,
        GraphQueryTerminal::Complete { authority: observed } if observed == authority
    ));
}

#[test]
fn independent_graph_facts_are_named_fields() {
    let authority = authority();
    let partition = PartitionId { raw: 4 };
    let edge = GraphEdge {
        authority,
        partition,
        source: EntityId::new(5),
        target: EntityId::new(6),
    };
    assert_eq!(edge.authority, authority);
    assert_eq!(edge.partition, partition);
    assert_eq!(edge.source, EntityId::new(5));
    assert_eq!(edge.target, EntityId::new(6));

    let row = GraphRow {
        partition,
        edges: &[edge],
    };
    assert_eq!(row.partition, partition);
    assert_eq!(row.edges, &[edge]);
}

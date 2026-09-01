//! Exercises the `server-index-build` graph projection through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//!
//! Corruption-prefix behavior is owned by the upstream boundaries this stage composes: fragment
//! byte corruption is rejected by the compiler-ir validator (its wire-attack suite) and derived
//! rows are re-validated by graph admission (`ValidatedGraphView`). The stage is typed to accept
//! only validated fragment views. These tests prove derivation order, chunking, exact capacity
//! rejections, honest absence, workspace reuse, the analytic count API, zero-allocation
//! derivation, and the Trustfall query terminal over derived rows.

mod support;

use core::mem::MaybeUninit;

use allocation_counter::{AllocationInfo, measure};
use compiler_ir::{AtomInput, EntityKind, EntityRecord, FragmentView, PrimitiveType, TypeNode};
use compiler_ir_vocabulary::{AtomId, TypeId};
use server_index_build::{
    GraphProjection, GraphProjectionError, GraphProjectionScratch, NodeKind, NodeWorkspace,
    OwnerCount, ReferenceTarget, SEMANTIC_TYPE_REFERENCE_PROJECTION, build_graph_projection,
    graph_reference_edge_count,
};
use server_index_graph_vector::{
    GraphAuthority, GraphEdge, GraphRow, PartitionId, ValidatedGraphView,
};
use server_index_trustfall::TrustfallGraph;
use server_index_vocabulary::IndexSnapshotId;
use support::{TestError, write_fragment, written};
use thiserror::Error;

const BYTES: usize = 2048;
const WORK_NODES: usize = 8;
const WORK_EDGES: usize = 64;
const WORK_ROWS: usize = 8;

#[derive(Debug, Error)]
enum GraphProof {
    #[error("derived edges diverged from the expected ordered facts")]
    EdgeOrder,
    #[error("derived row count was {observed}, expected {expected}")]
    RowCount { expected: usize, observed: usize },
    #[error("projection carried authority {observed:?}, expected {expected:?}")]
    Authority {
        expected: GraphAuthority,
        observed: GraphAuthority,
    },
    #[error("graph query result diverged from the derived facts")]
    QueryResult,
    #[error("neighbor hits did not match the derived reference edges")]
    Hits,
    #[error("rejection {expected:?} was not observed")]
    Rejection {
        expected: GraphProjectionError,
        observed: GraphProjectionError,
    },
    #[error("the projection unexpectedly succeeded")]
    UnexpectedSuccess,
    #[error("an unexpected projection rejection was observed")]
    UnexpectedRejection {
        #[source]
        observed: GraphProjectionError,
    },
    #[error("a caller node workspace was written before its capacity admission")]
    WorkspaceMutatedBeforeAdmission,
    #[error("count API reported {observed}, derivation reported {expected}")]
    CountDivergence { expected: usize, observed: usize },
    #[error("warm graph derivation allocated")]
    WarmAllocation,
    #[error("allocation measurement did not execute the derivation")]
    MeasurementSkipped,
}

#[derive(Debug, Error)]
enum TestFailure {
    #[error(transparent)]
    Fixture(#[from] TestError),
    #[error(transparent)]
    Projection(#[from] GraphProjectionError),
    #[error("graph admission rejected derived rows: {0:?}")]
    Admission(server_index_graph_vector::AdmissionError),
    #[error("graph query rejected its output contract: {0:?}")]
    Query(server_index_graph_vector::GraphQueryError),
    #[error(transparent)]
    Trustfall(#[from] server_index_trustfall::TrustfallGraphError),
    #[error(transparent)]
    Proof(#[from] GraphProof),
}

impl From<server_index_graph_vector::AdmissionError> for TestFailure {
    fn from(error: server_index_graph_vector::AdmissionError) -> Self {
        Self::Admission(error)
    }
}

impl From<server_index_graph_vector::GraphQueryError> for TestFailure {
    fn from(error: server_index_graph_vector::GraphQueryError) -> Self {
        Self::Query(error)
    }
}

fn authority() -> GraphAuthority {
    GraphAuthority::new(
        IndexSnapshotId::from_canonical_bytes(b"graph-projection-snapshot"),
        SEMANTIC_TYPE_REFERENCE_PROJECTION,
    )
}

/// One function referencing the record's node, one record, one constant referencing the function.
fn reference_fixture() -> Result<FragmentBytes, TestError> {
    let mut bytes = [0_u8; BYTES];
    let entities = [
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Function,
        },
        EntityRecord {
            semantic_type: TypeId::new(1),
            name: AtomId::new(0),
            kind: EntityKind::Record,
        },
        EntityRecord {
            semantic_type: TypeId::new(2),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        },
    ];
    let nodes = [
        TypeNode::Reference(TypeId::new(1)),
        TypeNode::Primitive(PrimitiveType::I32),
        TypeNode::Reference(TypeId::new(0)),
    ];
    let length = write_fragment(
        &mut bytes,
        b"graph-reference-fixture",
        &entities,
        &nodes,
        &[AtomInput { bytes: b"Alpha" }],
    )?;
    Ok(FragmentBytes { bytes, length })
}

/// Seventeen functions referencing one record's node: 17 edges, chunked 16 + 1.
fn chunked_fixture() -> Result<FragmentBytes, TestError> {
    let mut bytes = [0_u8; BYTES];
    let entities: Vec<EntityRecord> = (0..17)
        .map(|_| EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Function,
        })
        .chain(core::iter::once(EntityRecord {
            semantic_type: TypeId::new(1),
            name: AtomId::new(0),
            kind: EntityKind::Record,
        }))
        .collect();
    let nodes = [
        TypeNode::Reference(TypeId::new(1)),
        TypeNode::Primitive(PrimitiveType::I32),
    ];
    let length = write_fragment(
        &mut bytes,
        b"graph-chunked-fixture",
        &entities,
        &nodes,
        &[AtomInput { bytes: b"Beta" }],
    )?;
    Ok(FragmentBytes { bytes, length })
}

/// The driver reality today: primitive-only types derive zero reference edges.
fn primitives_only_fixture() -> Result<FragmentBytes, TestError> {
    let mut bytes = [0_u8; BYTES];
    let length = write_fragment(
        &mut bytes,
        b"graph-primitive-fixture",
        &[EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Constant,
        }],
        &[TypeNode::Primitive(PrimitiveType::Bool)],
        &[AtomInput { bytes: b"Gamma" }],
    )?;
    Ok(FragmentBytes { bytes, length })
}

/// A prepared fragment and its exact validated byte length.
struct FragmentBytes {
    bytes: [u8; BYTES],
    length: usize,
}

fn view(fixture: &FragmentBytes) -> Result<FragmentView<'_>, TestError> {
    let bytes = written(&fixture.bytes, fixture.length)?;
    Ok(FragmentView::validate(bytes)?)
}

#[test]
fn derived_reference_edges_keep_entity_order_pass_admission_and_allocate_nothing()
-> Result<(), TestFailure> {
    let fixture = reference_fixture()?;
    let borrowed = view(&fixture)?;
    let mut rows = [MaybeUninit::<GraphRow>::uninit(); WORK_ROWS];
    let mut edges = [MaybeUninit::<GraphEdge>::uninit(); WORK_EDGES];
    let mut kinds = [NodeKind::PLACEHOLDER; WORK_NODES];
    let mut targets = [ReferenceTarget::PLACEHOLDER; WORK_NODES];
    let mut owners = [OwnerCount::PLACEHOLDER; WORK_NODES];
    let mut derived = None;
    let allocation = measure(|| {
        let mut nodes = match NodeWorkspace::new(&mut kinds, &mut targets, &mut owners) {
            Ok(nodes) => nodes,
            Err(error) => {
                derived = Some(Err(error));
                return;
            }
        };
        derived = Some(build_graph_projection(
            &borrowed,
            authority(),
            PartitionId::new(40),
            GraphProjectionScratch {
                rows: &mut rows,
                edges: &mut edges,
            },
            &mut nodes,
        ));
    });
    let projection: GraphProjection<'_> = match derived {
        Some(Ok(projection)) => projection,
        Some(Err(error)) => return Err(error.into()),
        None => return Err(GraphProof::MeasurementSkipped.into()),
    };
    if allocation != AllocationInfo::default() {
        return Err(GraphProof::WarmAllocation.into());
    }
    expect_edges(&projection, &[(0, 1), (2, 0)], 1, PartitionId::new(40))?;
    let validated = ValidatedGraphView::try_new(authority(), projection.rows)?;
    let mut hits = [None; 4];
    let outcome = validated.neighbors(&[PartitionId::new(40)], entity(2), &mut hits)?;
    if outcome.written != 1
        || outcome.terminal.authority() != authority()
        || outcome.terminal.is_partial()
    {
        return Err(GraphProof::QueryResult.into());
    }
    let hit = hits.iter().find_map(|slot| *slot).ok_or(GraphProof::Hits)?;
    if hit.entity != entity(0) || hit.partition != PartitionId::new(40) {
        return Err(GraphProof::Hits.into());
    }
    let mut queried = [None; 4];
    let terminal = TrustfallGraph::new(&validated).neighbors(entity(2), &mut queried)?;
    if terminal.written != 1 || terminal.authority != authority() {
        return Err(GraphProof::QueryResult.into());
    }
    let graph_hit = queried
        .iter()
        .find_map(|slot| *slot)
        .ok_or(GraphProof::Hits)?;
    if graph_hit.entity != entity(0) || graph_hit.partition != PartitionId::new(40) {
        return Err(GraphProof::Hits.into());
    }
    Ok(())
}

#[test]
fn derived_rows_chunk_to_the_row_bound_and_guard_the_partition_space() -> Result<(), TestFailure> {
    let fixture = chunked_fixture()?;
    let borrowed = view(&fixture)?;
    let mut rows = [MaybeUninit::<GraphRow>::uninit(); WORK_ROWS];
    let mut edges = [MaybeUninit::<GraphEdge>::uninit(); WORK_EDGES];
    let mut kinds = [NodeKind::PLACEHOLDER; WORK_NODES];
    let mut targets = [ReferenceTarget::PLACEHOLDER; WORK_NODES];
    let mut owners = [OwnerCount::PLACEHOLDER; WORK_NODES];
    let mut nodes = NodeWorkspace::new(&mut kinds, &mut targets, &mut owners)?;
    let projection = build_graph_projection(
        &borrowed,
        authority(),
        PartitionId::new(65530),
        GraphProjectionScratch {
            rows: &mut rows,
            edges: &mut edges,
        },
        &mut nodes,
    )?;
    if projection.rows.len() != 2 {
        return Err(GraphProof::RowCount {
            expected: 2,
            observed: projection.rows.len(),
        }
        .into());
    }
    let [first, second] = projection.rows else {
        return Err(GraphProof::RowCount {
            expected: 2,
            observed: projection.rows.len(),
        }
        .into());
    };
    if first.partition != PartitionId::new(65530) || second.partition != PartitionId::new(65531) {
        return Err(GraphProof::RowCount {
            expected: 2,
            observed: projection.rows.len(),
        }
        .into());
    }
    if first.edges.len() != 16 || second.edges.len() != 1 {
        return Err(GraphProof::EdgeOrder.into());
    }
    ValidatedGraphView::try_new(authority(), projection.rows)?;

    let mut guard_rows = [MaybeUninit::<GraphRow>::uninit(); WORK_ROWS];
    let mut guard_edges = [MaybeUninit::<GraphEdge>::uninit(); WORK_EDGES];
    let rejected = build_graph_projection(
        &borrowed,
        authority(),
        PartitionId::new(65535),
        GraphProjectionScratch {
            rows: &mut guard_rows,
            edges: &mut guard_edges,
        },
        &mut nodes,
    );
    match rejected {
        Err(GraphProjectionError::PartitionSpace {
            base,
            required_rows,
        }) => {
            let expected = GraphProjectionError::PartitionSpace {
                base: PartitionId::new(65535),
                required_rows: 2,
            };
            if base != PartitionId::new(65535) || required_rows != 2 {
                return Err(GraphProof::Rejection {
                    expected,
                    observed: expected,
                }
                .into());
            }
        }
        other => return Err(unexpected(other)),
    }
    Ok(())
}

#[test]
fn capacity_rejections_are_exact_and_node_admission_precedes_every_write() -> Result<(), TestFailure>
{
    let fixture = reference_fixture()?;
    let borrowed = view(&fixture)?;
    let mut rows = [MaybeUninit::<GraphRow>::uninit(); WORK_ROWS];
    let mut edges = [MaybeUninit::<GraphEdge>::uninit(); WORK_EDGES];
    let mut short_kinds = [NodeKind::PLACEHOLDER; 2];
    let mut untouched_targets = [ReferenceTarget::PLACEHOLDER; 2];
    let mut untouched_owners = [OwnerCount::PLACEHOLDER; 2];
    let rejected = build_graph_projection(
        &borrowed,
        authority(),
        PartitionId::new(0),
        GraphProjectionScratch {
            rows: &mut rows,
            edges: &mut edges,
        },
        &mut NodeWorkspace::new(
            &mut short_kinds,
            &mut untouched_targets,
            &mut untouched_owners,
        )?,
    );
    match rejected {
        Err(GraphProjectionError::NodeWorkspaceCapacity {
            required: 3,
            available: 2,
        }) => {}
        other => return Err(unexpected(other)),
    }
    if untouched_targets != [ReferenceTarget::PLACEHOLDER; 2]
        || untouched_owners != [OwnerCount::PLACEHOLDER; 2]
    {
        return Err(GraphProof::WorkspaceMutatedBeforeAdmission.into());
    }

    let mut work_rows = [MaybeUninit::<GraphRow>::uninit(); WORK_ROWS];
    let mut short_edges = [MaybeUninit::<GraphEdge>::uninit(); 1];
    let mut kinds = [NodeKind::PLACEHOLDER; WORK_NODES];
    let mut targets = [ReferenceTarget::PLACEHOLDER; WORK_NODES];
    let mut owners = [OwnerCount::PLACEHOLDER; WORK_NODES];
    let mut nodes = NodeWorkspace::new(&mut kinds, &mut targets, &mut owners)?;
    let rejected = build_graph_projection(
        &borrowed,
        authority(),
        PartitionId::new(0),
        GraphProjectionScratch {
            rows: &mut work_rows,
            edges: &mut short_edges,
        },
        &mut nodes,
    );
    match rejected {
        Err(GraphProjectionError::EdgeCapacity {
            required: 2,
            available: 1,
        }) => {}
        other => return Err(unexpected(other)),
    }

    let mut no_rows: [MaybeUninit<GraphRow>; 0] = [];
    let mut reused_edges = [MaybeUninit::<GraphEdge>::uninit(); WORK_EDGES];
    let rejected = build_graph_projection(
        &borrowed,
        authority(),
        PartitionId::new(0),
        GraphProjectionScratch {
            rows: &mut no_rows,
            edges: &mut reused_edges,
        },
        &mut nodes,
    );
    match rejected {
        Err(GraphProjectionError::RowCapacity {
            required: 1,
            available: 0,
        }) => {}
        other => return Err(unexpected(other)),
    }
    Ok(())
}

#[test]
fn zero_edge_fragments_project_empty_rows_and_report_selected_absence() -> Result<(), TestFailure> {
    let fixture = primitives_only_fixture()?;
    let borrowed = view(&fixture)?;
    let mut rows = [MaybeUninit::<GraphRow>::uninit(); WORK_ROWS];
    let mut edges = [MaybeUninit::<GraphEdge>::uninit(); WORK_EDGES];
    let mut kinds = [NodeKind::PLACEHOLDER; WORK_NODES];
    let mut targets = [ReferenceTarget::PLACEHOLDER; WORK_NODES];
    let mut owners = [OwnerCount::PLACEHOLDER; WORK_NODES];
    let mut nodes = NodeWorkspace::new(&mut kinds, &mut targets, &mut owners)?;
    let projection = build_graph_projection(
        &borrowed,
        authority(),
        PartitionId::new(7),
        GraphProjectionScratch {
            rows: &mut rows,
            edges: &mut edges,
        },
        &mut nodes,
    )?;
    expect_edges(&projection, &[], 0, PartitionId::new(7))?;
    let count = graph_reference_edge_count(&borrowed, &mut nodes)?;
    if count != 0 {
        return Err(GraphProof::CountDivergence {
            expected: 0,
            observed: count,
        }
        .into());
    }
    let validated = ValidatedGraphView::try_new(authority(), projection.rows)?;
    let mut hits = [None; 4];
    let complete = validated.neighbors(&[], entity(0), &mut hits)?;
    if complete.written != 0 || complete.terminal.is_partial() {
        return Err(GraphProof::QueryResult.into());
    }
    let absent = validated.neighbors(&[PartitionId::new(7)], entity(0), &mut hits)?;
    if !(absent.terminal.is_partial() && absent.terminal.missing() == [PartitionId::new(7)]) {
        return Err(GraphProof::QueryResult.into());
    }
    Ok(())
}

#[test]
fn workspaces_fully_rewrite_across_reuse_and_the_count_api_matches_derivation()
-> Result<(), TestFailure> {
    let reference = reference_fixture()?;
    let chunked = chunked_fixture()?;
    let reference_view = view(&reference)?;
    let chunked_view = view(&chunked)?;
    let mut kinds = [NodeKind::PLACEHOLDER; WORK_NODES];
    let mut targets = [ReferenceTarget::PLACEHOLDER; WORK_NODES];
    let mut owners = [OwnerCount::PLACEHOLDER; WORK_NODES];
    let mut nodes = NodeWorkspace::new(&mut kinds, &mut targets, &mut owners)?;
    let mut first_rows = [MaybeUninit::<GraphRow>::uninit(); WORK_ROWS];
    let mut first_edges = [MaybeUninit::<GraphEdge>::uninit(); WORK_EDGES];
    let first = build_graph_projection(
        &reference_view,
        authority(),
        PartitionId::new(0),
        GraphProjectionScratch {
            rows: &mut first_rows,
            edges: &mut first_edges,
        },
        &mut nodes,
    )?;
    expect_edges(&first, &[(0, 1), (2, 0)], 1, PartitionId::new(0))?;
    let counted = graph_reference_edge_count(&reference_view, &mut nodes)?;
    if counted != first.edge_count() {
        return Err(GraphProof::CountDivergence {
            expected: first.edge_count(),
            observed: counted,
        }
        .into());
    }
    let mut second_rows = [MaybeUninit::<GraphRow>::uninit(); WORK_ROWS];
    let mut second_edges = [MaybeUninit::<GraphEdge>::uninit(); WORK_EDGES];
    let second = build_graph_projection(
        &chunked_view,
        authority(),
        PartitionId::new(0),
        GraphProjectionScratch {
            rows: &mut second_rows,
            edges: &mut second_edges,
        },
        &mut nodes,
    )?;
    let expected: Vec<(u32, u32)> = (0..17).map(|source| (source, 17)).collect();
    let mut observed = Vec::new();
    for row in second.rows {
        for edge in row.edges {
            observed.push((edge.source.raw, edge.target.raw));
        }
    }
    if observed != expected || second.edge_count() != 17 {
        return Err(GraphProof::EdgeOrder.into());
    }
    let counted = graph_reference_edge_count(&chunked_view, &mut nodes)?;
    if counted != 17 {
        return Err(GraphProof::CountDivergence {
            expected: 17,
            observed: counted,
        }
        .into());
    }
    Ok(())
}

const fn entity(raw: u32) -> compiler_ir_vocabulary::EntityId {
    compiler_ir_vocabulary::EntityId::new(raw)
}

fn expect_edges(
    projection: &GraphProjection<'_>,
    expected: &[(u32, u32)],
    expected_rows: usize,
    base: PartitionId,
) -> Result<(), TestFailure> {
    if projection.rows.len() != expected_rows {
        return Err(GraphProof::RowCount {
            expected: expected_rows,
            observed: projection.rows.len(),
        }
        .into());
    }
    let mut observed = Vec::new();
    for row in projection.rows {
        if row.partition != base || row.edges.iter().any(|edge| edge.partition != base) {
            return Err(GraphProof::RowCount {
                expected: expected_rows,
                observed: projection.rows.len(),
            }
            .into());
        }
        for edge in row.edges {
            if edge.authority != authority() {
                return Err(GraphProof::Authority {
                    expected: authority(),
                    observed: edge.authority,
                }
                .into());
            }
            observed.push((edge.source.raw, edge.target.raw));
        }
    }
    if observed != expected {
        return Err(GraphProof::EdgeOrder.into());
    }
    Ok(())
}

fn unexpected(observed: Result<GraphProjection<'_>, GraphProjectionError>) -> TestFailure {
    match observed {
        Ok(_) => GraphProof::UnexpectedSuccess.into(),
        Err(observed) => GraphProof::UnexpectedRejection { observed }.into(),
    }
}

//! Defines graph projection behavior for `server-index-build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the graph-projection invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Allocation-free semantic-type reference graph rows over one validated fragment.
//!
//! The projection derives one directed edge per resolvable semantic-type reference: an entity
//! whose own type node is `Reference(t)` points at every entity whose declaration owns node `t`.
//! Reference chains resolve exactly one hop, matching the compact-IR contract that makes
//! structural type identity a producer obligation rather than an index inference. Entities are
//! admitted at [`crate::MAX_INDEX_ROWS`], so the deterministic per-source target scans stay
//! within a bounded comparison budget.
use core::mem::MaybeUninit;

use compiler_ir::{EntityCursor, FragmentView, TypeNode};
use compiler_ir_vocabulary::{EntityId, TypeId};
use server_index_graph_vector::{
    GraphAuthority, GraphEdge, GraphRow, MAX_PARTITIONS, PartitionId, ProjectionId,
};
use thiserror::Error;

use crate::{
    initialized::{InitializationError, try_initialize},
    partition::{partition_at, partition_range_fits},
};

/// Immutable graph recipe identity of the semantic-type reference projection derived here.
pub const SEMANTIC_TYPE_REFERENCE_PROJECTION: ProjectionId = ProjectionId::new(1);

/// Maximum derived edges admitted into one graph projection row.
///
/// This mirrors the graph admission law consumed by
/// [`ValidatedGraphView`](server_index_graph_vector::ValidatedGraphView); the integration tests
/// prove derived rows pass that admission unchanged.
pub const MAX_GRAPH_EDGES_PER_ROW: usize = 16;

/// Workspace cell marking a reference type node.
const REFERENCE_NODE_KIND: u8 = 1;

/// Caller-owned output regions for one allocation-free graph projection.
pub struct GraphProjectionScratch<'slots> {
    /// Derived immutable rows, one per partition chunk.
    pub rows: &'slots mut [MaybeUninit<GraphRow<'slots>>],
    /// Derived directed edges in deterministic (source, target) entity order.
    pub edges: &'slots mut [MaybeUninit<GraphEdge>],
}

/// Classified per-node workspace with length-agreed parallel lanes.
///
/// The constructor rejects disagreeing lane lengths once, so derivation can index every lane by
/// node coordinate without per-access re-checking.
pub struct NodeWorkspace<'slots> {
    kinds: &'slots mut [NodeKind],
    targets: &'slots mut [ReferenceTarget],
    owners: &'slots mut [OwnerCount],
}

/// Classified kind tag of one fragment type node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NodeKind {
    cell: u8,
}

impl NodeKind {
    /// Placeholder cell for caller storage that derivation fully rewrites.
    pub const PLACEHOLDER: Self = Self { cell: 9 };

    /// The classification cell for a primitive type node.
    pub const PRIMITIVE: Self = Self { cell: 0 };

    /// The classification cell for a reference type node.
    pub const REFERENCE: Self = Self {
        cell: REFERENCE_NODE_KIND,
    };

    const fn is_reference(self) -> bool {
        self.cell == REFERENCE_NODE_KIND
    }
}

/// Validated reference target coordinate retained as a workspace cell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceTarget {
    coordinate: u32,
}

impl ReferenceTarget {
    /// Placeholder cell for caller storage that derivation fully rewrites.
    pub const PLACEHOLDER: Self = Self { coordinate: 9 };

    /// The cell written for a primitive node, which carries no reference target.
    pub const NONE: Self = Self { coordinate: 0 };

    const fn validated(target: TypeId) -> Self {
        Self {
            coordinate: target.raw,
        }
    }

    const fn typed(self) -> TypeId {
        TypeId::new(self.coordinate)
    }
}

/// Owner entity count for one type node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnerCount {
    entities: u32,
}

impl OwnerCount {
    /// Placeholder cell for caller storage that derivation fully rewrites.
    pub const PLACEHOLDER: Self = Self { entities: 9 };
}

impl<'slots> NodeWorkspace<'slots> {
    /// Bundles length-agreed parallel workspace lanes.
    ///
    /// # Errors
    ///
    /// Returns the exact lane disagreement when the lanes cannot address one shared node space.
    pub fn new(
        kinds: &'slots mut [NodeKind],
        targets: &'slots mut [ReferenceTarget],
        owners: &'slots mut [OwnerCount],
    ) -> Result<Self, GraphProjectionError> {
        let width = kinds.len();
        if targets.len() != width || owners.len() != width {
            return Err(GraphProjectionError::NodeWorkspaceCapacity {
                required: width,
                available: targets.len().min(owners.len()),
            });
        }
        Ok(Self {
            kinds,
            targets,
            owners,
        })
    }

    /// Reports the shared node-lane width.
    const fn width(&self) -> usize {
        self.kinds.len()
    }

    fn kind(&self, index: usize) -> Result<NodeKind, GraphProjectionError> {
        self.kinds
            .get(index)
            .copied()
            .ok_or(GraphProjectionError::NodeWorkspaceCapacity {
                required: index + 1,
                available: self.kinds.len(),
            })
    }

    fn target(&self, index: usize) -> Result<ReferenceTarget, GraphProjectionError> {
        self.targets
            .get(index)
            .copied()
            .ok_or(GraphProjectionError::NodeWorkspaceCapacity {
                required: index + 1,
                available: self.targets.len(),
            })
    }

    fn count(&self, index: usize) -> Result<usize, GraphProjectionError> {
        let owner = self
            .owners
            .get(index)
            .ok_or(GraphProjectionError::NodeWorkspaceCapacity {
                required: index + 1,
                available: self.owners.len(),
            })?;
        let coordinate = self.target(index)?.typed();
        usize::try_from(owner.entities).map_err(|source| GraphProjectionError::NodeAddressSpace {
            node: coordinate,
            source,
        })
    }

    fn bump(&mut self, index: usize) -> Result<(), GraphProjectionError> {
        let width = self.owners.len();
        let owner =
            self.owners
                .get_mut(index)
                .ok_or(GraphProjectionError::NodeWorkspaceCapacity {
                    required: index + 1,
                    available: width,
                })?;
        owner.entities += 1;
        Ok(())
    }

    fn reset_owners(&mut self) {
        for owner in self.owners.iter_mut() {
            owner.entities = 0;
        }
    }

    fn classify(
        &mut self,
        index: usize,
        kind: NodeKind,
        target: ReferenceTarget,
    ) -> Result<(), GraphProjectionError> {
        let kind_width = self.kinds.len();
        let kinds =
            self.kinds
                .get_mut(index)
                .ok_or(GraphProjectionError::NodeWorkspaceCapacity {
                    required: index + 1,
                    available: kind_width,
                })?;
        *kinds = kind;
        let target_width = self.targets.len();
        let targets =
            self.targets
                .get_mut(index)
                .ok_or(GraphProjectionError::NodeWorkspaceCapacity {
                    required: index + 1,
                    available: target_width,
                })?;
        *targets = target;
        Ok(())
    }
}

/// A count of caller-owned storage slots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Slots {
    count: usize,
}

/// Exact caller capacities inspected by no-write graph admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphProjectionCapacity {
    /// Slots available for derived rows.
    pub rows: Slots,
    /// Slots available for derived edges.
    pub edges: Slots,
}

impl GraphProjectionScratch<'_> {
    /// Reports the exact reusable capacities without writing any caller slot.
    #[must_use]
    pub const fn capacity(&self) -> GraphProjectionCapacity {
        self_capacity(self.rows.len(), self.edges.len())
    }
}

const fn self_capacity(rows: usize, edges: usize) -> GraphProjectionCapacity {
    GraphProjectionCapacity {
        rows: Slots { count: rows },
        edges: Slots { count: edges },
    }
}

/// Complete derived graph rows pinned to one graph authority and partition base.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphProjection<'facts> {
    /// Snapshot and recipe authority carried by every derived edge.
    pub authority: GraphAuthority,
    /// First partition coordinate; row `i` owns `base_partition + i`.
    pub base_partition: PartitionId,
    /// Derived rows in ascending partition order.
    pub rows: &'facts [GraphRow<'facts>],
}

impl GraphProjection<'_> {
    /// Reports the total derived edge count across all rows.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.rows.iter().map(|row| row.edges.len()).sum()
    }
}

/// Exact graph projection rejection retaining every checked operand.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GraphProjectionError {
    /// The derived edge count exceeded the supplied edge region.
    #[error("graph projection derived {required} edges, caller supplied {available} slots")]
    EdgeCapacity {
        /// Complete derived edge count.
        required: usize,
        /// Supplied edge slots.
        available: usize,
    },
    /// The derived row count exceeded the supplied row region.
    #[error("graph projection derived {required} rows, caller supplied {available} slots")]
    RowCapacity {
        /// Complete derived row count.
        required: usize,
        /// Supplied row slots.
        available: usize,
    },
    /// One per-node workspace lane cannot address every fragment type node.
    #[error("graph projection node workspaces have {available} slots, node count is {required}")]
    NodeWorkspaceCapacity {
        /// Complete fragment type-node count.
        required: usize,
        /// Supplied workspace slots.
        available: usize,
    },
    /// The derived partition count exceeded graph admission.
    #[error("graph projection requires {observed} partitions, limit is {maximum}")]
    PartitionLimit {
        /// Maximum admitted partitions.
        maximum: usize,
        /// Complete derived partition count.
        observed: usize,
    },
    /// The partition range required by the derived rows left the `u16` partition space.
    #[error(
        "graph projection partitions from {base:?} require {required_partitions} partitions and overflow the partition space"
    )]
    PartitionSpace {
        /// First required partition coordinate.
        base: PartitionId,
        /// Complete required partition count.
        required_partitions: usize,
    },
    /// A validated node coordinate could not address host storage.
    #[error("validated type-node coordinate {node:?} cannot address host storage")]
    NodeAddressSpace {
        /// Rejected validated coordinate.
        node: TypeId,
        /// Exact integer conversion cause.
        #[source]
        source: core::num::TryFromIntError,
    },
}

/// Derives the semantic-type reference graph rows of one validated fragment.
///
/// Rows are chunked to [`MAX_GRAPH_EDGES_PER_ROW`] with ascending partitions from
/// `base_partition`, so derived rows pass graph admission unchanged. Caller regions are checked
/// before the first write; every workspace slot is rewritten during derivation.
///
/// # Errors
///
/// Returns the exact caller-capacity, row-limit, partition-space, or address-space rejection.
pub fn build_graph_projection<'fragment: 'scratch, 'scratch>(
    view: &FragmentView<'fragment>,
    authority: GraphAuthority,
    base_partition: PartitionId,
    scratch: GraphProjectionScratch<'scratch>,
    nodes: &mut NodeWorkspace<'_>,
) -> Result<GraphProjection<'scratch>, GraphProjectionError> {
    let node_count = view.type_nodes().len();
    let GraphProjectionScratch { rows, edges } = scratch;

    let capacity = self_capacity(rows.len(), edges.len());
    if nodes.width() < node_count {
        return Err(GraphProjectionError::NodeWorkspaceCapacity {
            required: node_count,
            available: nodes.width(),
        });
    }

    classify_nodes(view, nodes)?;
    let edge_total = count_references(view, nodes)?;
    if capacity.edges.count < edge_total {
        return Err(GraphProjectionError::EdgeCapacity {
            required: edge_total,
            available: capacity.edges.count,
        });
    }
    let row_total = rows_for_edges(edge_total);
    if capacity.rows.count < row_total {
        return Err(GraphProjectionError::RowCapacity {
            required: row_total,
            available: capacity.rows.count,
        });
    }
    if row_total > MAX_PARTITIONS {
        return Err(GraphProjectionError::PartitionLimit {
            maximum: MAX_PARTITIONS,
            observed: row_total,
        });
    }
    if !partition_range_fits(base_partition, row_total) {
        return Err(GraphProjectionError::PartitionSpace {
            base: base_partition,
            required_partitions: row_total,
        });
    }

    let admitted_edges = edges
        .get_mut(..edge_total)
        .ok_or(GraphProjectionError::EdgeCapacity {
            required: edge_total,
            available: capacity.edges.count,
        })?;
    let mut ordinal = 0_usize;
    let derived = try_initialize(
        admitted_edges,
        ReferenceEdgeCursor::new(view, nodes, edge_total),
        |(source, target)| {
            let row = ordinal / MAX_GRAPH_EDGES_PER_ROW;
            ordinal += 1;
            let Some(partition) = partition_at(base_partition, row) else {
                return Err(GraphProjectionError::PartitionSpace {
                    base: base_partition,
                    required_partitions: row_total,
                });
            };
            Ok(GraphEdge::new(authority, partition, source, target))
        },
    )
    .map_err(initialization_error)?;
    let derived_edges: &'scratch [GraphEdge] = derived.into_shared();

    let admitted_rows = rows
        .get_mut(..row_total)
        .ok_or(GraphProjectionError::RowCapacity {
            required: row_total,
            available: capacity.rows.count,
        })?;
    let rows_initialized = try_initialize(
        admitted_rows,
        derived_edges
            .chunks(MAX_GRAPH_EDGES_PER_ROW)
            .zip(0..row_total),
        |(chunk, row_index)| {
            let Some(partition) = partition_at(base_partition, row_index) else {
                return Err(GraphProjectionError::PartitionSpace {
                    base: base_partition,
                    required_partitions: row_total,
                });
            };
            Ok(GraphRow {
                partition,
                edges: chunk,
            })
        },
    )
    .map_err(row_initialization_error)?;

    Ok(GraphProjection {
        authority,
        base_partition,
        rows: rows_initialized.into_shared(),
    })
}

/// Reports the exact derived edge count for one validated fragment.
///
/// The count is analytic: one classification pass and one owner-counting reference pass over
/// validated lanes, within the admitted [`crate::MAX_INDEX_ROWS`] entity budget. The workspace
/// is fully rewritten.
///
/// # Errors
///
/// Returns the exact workspace-capacity or address-space rejection.
pub fn graph_reference_edge_count(
    view: &FragmentView<'_>,
    nodes: &mut NodeWorkspace<'_>,
) -> Result<usize, GraphProjectionError> {
    if nodes.width() < view.type_nodes().len() {
        return Err(GraphProjectionError::NodeWorkspaceCapacity {
            required: view.type_nodes().len(),
            available: nodes.width(),
        });
    }
    classify_nodes(view, nodes)?;
    count_references(view, nodes)
}

/// Classifies every validated node into the workspace lanes.
///
/// Every lane cell addressed by a fragment type node is rewritten: cell `i` describes node `i`.
fn classify_nodes(
    view: &FragmentView<'_>,
    nodes: &mut NodeWorkspace<'_>,
) -> Result<(), GraphProjectionError> {
    if nodes.width() < view.type_nodes().len() {
        return Err(GraphProjectionError::NodeWorkspaceCapacity {
            required: view.type_nodes().len(),
            available: nodes.width(),
        });
    }
    for (index, node) in view.type_nodes().enumerate() {
        let (kind, target) = match node {
            TypeNode::Primitive(_) => (NodeKind::PRIMITIVE, ReferenceTarget::NONE),
            TypeNode::Reference(target) => {
                (NodeKind::REFERENCE, ReferenceTarget::validated(target))
            }
        };
        nodes.classify(index, kind, target)?;
    }
    Ok(())
}

/// Refills owner counts and counts derived edges in (source, target) entity order.
///
/// Owner cells are zeroed before counting; each entity contributes exactly one increment to its
/// own node's count and entity ordinals are `u32`, so no count can exceed `u32::MAX`.
fn count_references(
    view: &FragmentView<'_>,
    nodes: &mut NodeWorkspace<'_>,
) -> Result<usize, GraphProjectionError> {
    nodes.reset_owners();
    for entity in view.entities() {
        let index = node_index(entity.semantic_type)?;
        nodes.bump(index)?;
    }
    let mut total = 0_usize;
    for entity in view.entities() {
        let index = node_index(entity.semantic_type)?;
        if nodes.kind(index)?.is_reference() {
            let target = nodes.target(index)?.typed();
            total += nodes.count(node_index(target)?)?;
        }
    }
    Ok(total)
}

/// Wraps the edge-region initialization failure as its typed projection rejection.
///
/// The cursor length and every region are checked before initialization, so the exhaustion
/// paths retain the exact operands rather than an internal failure.
const fn initialization_error(
    error: InitializationError<GraphProjectionError>,
) -> GraphProjectionError {
    match error {
        InitializationError::Length {
            required,
            available,
        }
        | InitializationError::Exhausted {
            required,
            initialized: available,
        } => GraphProjectionError::EdgeCapacity {
            required,
            available,
        },
        InitializationError::Surplus { required } => GraphProjectionError::EdgeCapacity {
            required,
            available: required,
        },
        InitializationError::Value(error) => error,
    }
}

/// Wraps the row-region initialization failure as its typed projection rejection.
const fn row_initialization_error(
    error: InitializationError<GraphProjectionError>,
) -> GraphProjectionError {
    match error {
        InitializationError::Length {
            required,
            available,
        }
        | InitializationError::Exhausted {
            required,
            initialized: available,
        } => GraphProjectionError::RowCapacity {
            required,
            available,
        },
        InitializationError::Surplus { required } => GraphProjectionError::RowCapacity {
            required,
            available: required,
        },
        InitializationError::Value(error) => error,
    }
}

/// Row count for a derived edge count: every full chunk plus a partial tail.
const fn rows_for_edges(edges: usize) -> usize {
    edges.div_ceil(MAX_GRAPH_EDGES_PER_ROW)
}

/// Converts a validated node coordinate into a workspace index.
fn node_index(node: TypeId) -> Result<usize, GraphProjectionError> {
    usize::try_from(node.raw)
        .map_err(|source| GraphProjectionError::NodeAddressSpace { node, source })
}

/// Deterministic (source, target) stream over every derived reference edge.
///
/// Sources ascend by entity ordinal; each source's targets ascend by entity ordinal, so the
/// stream order equals the derived edge order and `remaining` reaches zero exactly at the end.
struct ReferenceEdgeCursor<'fragment, 'work> {
    nodes: &'work NodeWorkspace<'work>,
    view: &'fragment FragmentView<'fragment>,
    outer: EntityCursor<'fragment>,
    source: Option<(EntityId, TypeId)>,
    inner: Option<EntityCursor<'fragment>>,
    remaining: usize,
}

impl<'fragment, 'work> ReferenceEdgeCursor<'fragment, 'work> {
    fn new(
        view: &'fragment FragmentView<'fragment>,
        nodes: &'work NodeWorkspace<'work>,
        remaining: usize,
    ) -> Self {
        Self {
            nodes,
            view,
            outer: view.entities(),
            source: None,
            inner: None,
            remaining,
        }
    }
}

impl Iterator for ReferenceEdgeCursor<'_, '_> {
    type Item = (EntityId, EntityId);

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(inner) = self.inner.as_mut() {
                let (source, target_node) = self.source?;
                for candidate in inner.by_ref() {
                    if candidate.semantic_type == target_node {
                        self.remaining -= 1;
                        return Some((source, candidate.entity));
                    }
                }
                self.inner = None;
            }
            let mut advanced = None;
            for candidate in self.outer.by_ref() {
                let Ok(index) = node_index(candidate.semantic_type) else {
                    // A validated coordinate that cannot address host storage must not
                    // silently skip its entity; stopping early turns into the exact
                    // typed exhaustion failure with the admitted operands.
                    self.remaining = 0;
                    return None;
                };
                let lookup = self
                    .nodes
                    .kind(index)
                    .and_then(|kind| Ok((kind, self.nodes.target(index)?)));
                match lookup {
                    Ok((kind, target)) if kind.is_reference() => {
                        advanced = Some((candidate.entity, target.typed()));
                        break;
                    }
                    Ok(_) => {}
                    Err(_) => {
                        // A workspace cell that cannot be addressed must not silently skip
                        // its entity; stopping early turns into the exact typed exhaustion
                        // failure with the admitted operands.
                        self.remaining = 0;
                        return None;
                    }
                }
            }
            match advanced {
                Some((entity, target)) => {
                    self.source = Some((entity, target));
                    self.inner = Some(self.view.entities());
                }
                None => return None,
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for ReferenceEdgeCursor<'_, '_> {}
impl core::iter::FusedIterator for ReferenceEdgeCursor<'_, '_> {}

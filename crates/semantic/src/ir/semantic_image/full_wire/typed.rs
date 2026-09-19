//! Explicit generic typed-pool rows for the complete portable image.
//!
//! `TypeExpr` and all of its typed list tables are already one measured,
//! role-bearing dependency graph in `semantic_image::typed`.  This module
//! turns that graph into portable cells after every source coordinate has been
//! remapped.  It deliberately does not serialize `TypeHeader` or any native
//! enum representation.

use alloc::vec::Vec;

use super::fault::{FullSemanticImageFault, FullSemanticImageField};
use super::wire::{NONE, TYPED_EDGE_ROW_BYTES, TYPED_NODE_ROW_BYTES};
use crate::ir::semantic_image::{
    full::FullPlanError,
    typed::{TypedDependencyPlan, TypedEdgeRole, TypedPlanEdge, TypedPlanTarget},
};

/// One canonical typed-pool node. `domain` plus `coordinate` is the exact
/// typed list/type address; `edge_start..edge_start + edge_count` is a
/// contiguous ordered role run in [`FullTypedPlan::edges`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FullTypedPlanNode {
    pub(crate) domain: u8,
    pub(crate) coordinate: u32,
    pub(crate) edge_start: u32,
    pub(crate) edge_count: u32,
}

/// One explicit role-bearing portable typed edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FullTypedPlanEdge {
    pub(crate) role: u8,
    pub(crate) role_index: u32,
    pub(crate) target: FullTypedPlanTarget,
}

/// A canonical endpoint in the generic typed graph.  Every variant carries
/// a coordinate in its own canonical domain; a bare integer is impossible at
/// this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FullTypedPlanTarget {
    Node { domain: u8, coordinate: u32 },
    Atom(u32),
    Entity(u32),
    External(u32),
    Scalar(u64),
}

impl FullTypedPlanTarget {
    pub(crate) const fn tag(self) -> u8 {
        match self {
            Self::Node { .. } => 0,
            Self::Atom(_) => 1,
            Self::Entity(_) => 2,
            Self::External(_) => 3,
            Self::Scalar(_) => 4,
        }
    }

    pub(crate) const fn domain(self) -> u8 {
        match self {
            Self::Node { domain, .. } => domain,
            Self::Atom(_) | Self::Entity(_) | Self::External(_) | Self::Scalar(_) => 0,
        }
    }

    pub(crate) const fn low(self) -> u32 {
        match self {
            Self::Node { coordinate, .. }
            | Self::Atom(coordinate)
            | Self::Entity(coordinate)
            | Self::External(coordinate) => coordinate,
            Self::Scalar(value) => {
                let bytes = value.to_le_bytes();
                u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
            }
        }
    }

    pub(crate) const fn high(self) -> u32 {
        match self {
            Self::Node { .. } | Self::Atom(_) | Self::Entity(_) | Self::External(_) => 0,
            Self::Scalar(value) => {
                let bytes = value.to_le_bytes();
                u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]])
            }
        }
    }
}

/// Pre-admitted canonical rows for both typed directories.  The writer only
/// iterates these vectors and writes little-endian primitives, so no fallible
/// lookup remains after the caller output is observed.
pub(crate) struct FullTypedPlan {
    pub(crate) nodes: Vec<FullTypedPlanNode>,
    pub(crate) edges: Vec<FullTypedPlanEdge>,
}

impl FullTypedPlan {
    pub(crate) fn build(typed: &TypedDependencyPlan<'_>) -> Result<Self, FullPlanError> {
        let node_count = typed.wire_nodes().len();
        let mut nodes = Vec::with_capacity(node_count);
        let mut edges = Vec::new();
        for node in typed.wire_nodes().iter().copied() {
            let (domain, coordinate) = typed.wire_node_coordinate(node)?;
            let edge_start =
                u32::try_from(edges.len()).map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::TypedEdges,
                })?;
            for edge in typed.wire_edges(node)?.iter().copied() {
                edges.push(remap_edge(typed, edge)?);
            }
            let edge_end =
                u32::try_from(edges.len()).map_err(|_| FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::TypedEdges,
                })?;
            nodes.push(FullTypedPlanNode {
                domain,
                coordinate,
                edge_start,
                edge_count: edge_end.checked_sub(edge_start).ok_or(
                    FullSemanticImageFault::LengthOverflow {
                        field: FullSemanticImageField::TypedEdges,
                    },
                )?,
            });
        }
        validate_node_order(&nodes)?;
        Ok(Self { nodes, edges })
    }

    pub(crate) fn node_bytes_len(&self) -> Result<usize, FullSemanticImageFault> {
        self.nodes.len().checked_mul(TYPED_NODE_ROW_BYTES).ok_or(
            FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedNodes,
            },
        )
    }

    pub(crate) fn edge_bytes_len(&self) -> Result<usize, FullSemanticImageFault> {
        self.edges.len().checked_mul(TYPED_EDGE_ROW_BYTES).ok_or(
            FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            },
        )
    }
}

fn remap_edge(
    typed: &TypedDependencyPlan<'_>,
    edge: TypedPlanEdge,
) -> Result<FullTypedPlanEdge, FullPlanError> {
    let (role, role_index) = wire_role(edge.role);
    let target = match edge.target {
        TypedPlanTarget::Node(node) => {
            let (domain, coordinate) = typed.wire_node_coordinate(node)?;
            FullTypedPlanTarget::Node { domain, coordinate }
        }
        TypedPlanTarget::Atom(atom) => FullTypedPlanTarget::Atom(typed.canonical().atom(atom)?),
        TypedPlanTarget::Entity(entity) => {
            FullTypedPlanTarget::Entity(typed.canonical().entity(entity)?)
        }
        TypedPlanTarget::External(external) => {
            FullTypedPlanTarget::External(typed.canonical().external(external)?)
        }
        TypedPlanTarget::Scalar(value) => FullTypedPlanTarget::Scalar(value),
    };
    Ok(FullTypedPlanEdge {
        role,
        role_index,
        target,
    })
}

/// Closed stable wire tags for every role.  The `u32` index is a separate
/// cell: widening source list lengths cannot silently consume discriminator
/// bits or change a tag's interpretation.
pub(crate) fn wire_role(role: TypedEdgeRole) -> (u8, u32) {
    match role {
        TypedEdgeRole::TypeTag => (0, 0),
        TypedEdgeRole::TypeField(index) => (1, u32::from(index)),
        TypedEdgeRole::ListElement(index) => (2, index),
        TypedEdgeRole::TupleLabel(index) => (3, index),
        TypedEdgeRole::TupleType(index) => (4, index),
        TypedEdgeRole::TupleKind(index) => (5, index),
        TypedEdgeRole::ObjectKind(index) => (6, index),
        TypedEdgeRole::ObjectKey(index) => (7, index),
        TypedEdgeRole::ObjectType(index) => (8, index),
        TypedEdgeRole::ObjectOptional(index) => (9, index),
        TypedEdgeRole::ObjectReadonly(index) => (10, index),
        TypedEdgeRole::ObjectParameter(index) => (11, index),
        TypedEdgeRole::ObjectValue(index) => (12, index),
        TypedEdgeRole::TemplatePart(index) => (13, index),
        TypedEdgeRole::ParameterName(index) => (14, index),
        TypedEdgeRole::ParameterBound(index) => (15, index),
        TypedEdgeRole::ParameterDefault(index) => (16, index),
        TypedEdgeRole::ParameterVariance(index) => (17, index),
        TypedEdgeRole::ParameterKind(index) => (18, index),
        TypedEdgeRole::ParameterRequirement(index) => (19, index),
        TypedEdgeRole::ParameterConstructor(index) => (20, index),
        TypedEdgeRole::ParameterAllowsRefLike(index) => (21, index),
        TypedEdgeRole::BoundKind(index) => (22, index),
        TypedEdgeRole::BoundValue(index) => (23, index),
        TypedEdgeRole::PredicateSubject(index) => (24, index),
        TypedEdgeRole::PredicateBound(index) => (25, index),
    }
}

/// Decodes a role tag after the enclosing full-image validator checked the
/// surrounding node grammar.  Keeping it closed makes a future borrowed
/// reader reject a new producer tag rather than treating it as a list field.
pub(crate) fn role_from_wire(tag: u8, index: u32) -> Option<TypedEdgeRole> {
    Some(match tag {
        0 if index == 0 => TypedEdgeRole::TypeTag,
        1 => TypedEdgeRole::TypeField(u8::try_from(index).ok()?),
        2 => TypedEdgeRole::ListElement(index),
        3 => TypedEdgeRole::TupleLabel(index),
        4 => TypedEdgeRole::TupleType(index),
        5 => TypedEdgeRole::TupleKind(index),
        6 => TypedEdgeRole::ObjectKind(index),
        7 => TypedEdgeRole::ObjectKey(index),
        8 => TypedEdgeRole::ObjectType(index),
        9 => TypedEdgeRole::ObjectOptional(index),
        10 => TypedEdgeRole::ObjectReadonly(index),
        11 => TypedEdgeRole::ObjectParameter(index),
        12 => TypedEdgeRole::ObjectValue(index),
        13 => TypedEdgeRole::TemplatePart(index),
        14 => TypedEdgeRole::ParameterName(index),
        15 => TypedEdgeRole::ParameterBound(index),
        16 => TypedEdgeRole::ParameterDefault(index),
        17 => TypedEdgeRole::ParameterVariance(index),
        18 => TypedEdgeRole::ParameterKind(index),
        19 => TypedEdgeRole::ParameterRequirement(index),
        20 => TypedEdgeRole::ParameterConstructor(index),
        21 => TypedEdgeRole::ParameterAllowsRefLike(index),
        22 => TypedEdgeRole::BoundKind(index),
        23 => TypedEdgeRole::BoundValue(index),
        24 => TypedEdgeRole::PredicateSubject(index),
        25 => TypedEdgeRole::PredicateBound(index),
        _ => return None,
    })
}

fn validate_node_order(nodes: &[FullTypedPlanNode]) -> Result<(), FullSemanticImageFault> {
    let mut previous_domain = 0_u8;
    let mut previous_coordinate = NONE;
    for (index, node) in nodes.iter().copied().enumerate() {
        let row = u32::try_from(index).map_err(|_| FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::TypedNodes,
        })?;
        if node.domain > 8 {
            return Err(FullSemanticImageFault::TypedDomain {
                node: row,
                domain: node.domain,
            });
        }
        if index != 0
            && (node.domain < previous_domain
                || (node.domain == previous_domain && node.coordinate <= previous_coordinate))
        {
            return Err(FullSemanticImageFault::CanonicalOrder {
                field: FullSemanticImageField::TypedNodes,
                previous: row
                    .checked_sub(1)
                    .ok_or(FullSemanticImageFault::LengthOverflow {
                        field: FullSemanticImageField::TypedNodes,
                    })?,
                row,
            });
        }
        previous_domain = node.domain;
        previous_coordinate = node.coordinate;
    }
    Ok(())
}

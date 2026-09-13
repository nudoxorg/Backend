//! Iterative traversal and fixed-size typed dependency fingerprints.

use alloc::vec::Vec;
use core::{cmp::Ordering, fmt};

use crate::ir::{ArenaRange, DeclarationIdentity, Ir};

use super::*;

pub(crate) struct TypedDependencyPlan<'image> {
    pub(crate) canonical: CanonicalFullPlan<'image>,
    pub(crate) counts: TypedPlanCounts,
    bases: TypedPlanBases,
    pub(crate) scratch: TypedPlanScratch,
}

/// Exact full-only planning failure. A common-plan failure remains its own
/// typed error rather than becoming an opaque typed-pool diagnostic.
#[derive(Debug)]
pub enum TypedPlanError {
    Core(CoreSemanticImageFault),
    Typed(TypedPlanFault),
}

impl fmt::Display for TypedPlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(cause) => fmt::Display::fmt(cause, formatter),
            Self::Typed(cause) => fmt::Display::fmt(cause, formatter),
        }
    }
}

impl core::error::Error for TypedPlanError {}

impl From<CoreSemanticImageFault> for TypedPlanError {
    fn from(value: CoreSemanticImageFault) -> Self {
        Self::Core(value)
    }
}

impl From<TypedPlanFault> for TypedPlanError {
    fn from(value: TypedPlanFault) -> Self {
        Self::Typed(value)
    }
}

impl<'image> TypedDependencyPlan<'image> {
    /// Measures and canonicalizes the full type/list dependency subgraph.
    /// This owns only actual node, edge, and key scratch; no maximum geometry
    /// or per-child allocation is introduced.
    pub(crate) fn build(ir: &'image Ir) -> Result<Self, TypedPlanError> {
        let canonical = CanonicalFullPlan::build(ir)?;
        let counts = TypedPlanCounts::from_ir(ir)?;
        let bases = TypedPlanBases::new(counts.as_array())?;
        let node_count = counts.node_count();
        if bases.total()
            != u32::try_from(node_count)
                .map_err(|_| TypedPlanFault::GeometryOverflow { nodes: node_count })?
        {
            return Err(TypedPlanFault::GeometryOverflow { nodes: node_count }.into());
        }

        let mut edge_count = 0_usize;
        for_each_node(counts, |node| {
            for_each_edge(ir, node, &mut |_| {
                edge_count = edge_count
                    .checked_add(1)
                    .ok_or(TypedPlanFault::EdgeGeometryOverflow { edges: edge_count })?;
                Ok(())
            })
        })?;

        let mut scratch = TypedPlanScratch::with_capacity(node_count, edge_count)?;
        scratch.edge_offsets.push(0);
        for_each_node(counts, |node| {
            scratch.nodes.push(node);
            for_each_edge(ir, node, &mut |edge| {
                validate_edge(&canonical, bases, counts, node, edge)?;
                scratch.edges.push(edge);
                Ok(())
            })?;
            let end = u32::try_from(scratch.edges.len()).map_err(|_| {
                TypedPlanFault::EdgeGeometryOverflow {
                    edges: scratch.edges.len(),
                }
            })?;
            scratch.edge_offsets.push(end);
            Ok(())
        })?;
        if scratch.nodes.len() != node_count
            || scratch.edge_offsets.len()
                != node_count
                    .checked_add(1)
                    .ok_or(TypedPlanFault::GeometryOverflow { nodes: node_count })?
        {
            return Err(TypedPlanFault::GeometryOverflow { nodes: node_count }.into());
        }

        scratch.states.resize(node_count, 0);
        scratch
            .key_ranges
            .resize(node_count, ArenaRange { start: 0, len: 0 });
        scratch
            .fingerprints
            .resize(node_count, TypedFingerprint::ZERO);
        scratch.canonical_slots.resize(node_count, u32::MAX);

        let mut result = Self {
            canonical,
            counts,
            bases,
            scratch,
        };
        result.build_postorder()?;
        result.materialize_fingerprints()?;
        result.order_canonical_nodes()?;
        Ok(result)
    }

    pub(crate) fn slot(&self, node: TypedPlanNode) -> Result<usize, TypedPlanFault> {
        usize::try_from(self.bases.slot(node, self.counts.as_array())?).map_err(|_| {
            TypedPlanFault::GeometryOverflow {
                nodes: self.scratch.nodes.len(),
            }
        })
    }

    pub(crate) fn edge_range(&self, node: TypedPlanNode) -> Result<(usize, usize), TypedPlanFault> {
        let slot = self.slot(node)?;
        let start = *self
            .scratch
            .edge_offsets
            .get(slot)
            .ok_or(TypedPlanFault::MissingNode {
                node,
                count: self.counts.at(node.domain()),
            })?;
        let end = *self
            .scratch
            .edge_offsets
            .get(
                slot.checked_add(1)
                    .ok_or(TypedPlanFault::GeometryOverflow {
                        nodes: self.scratch.nodes.len(),
                    })?,
            )
            .ok_or(TypedPlanFault::MissingNode {
                node,
                count: self.counts.at(node.domain()),
            })?;
        let start = usize::try_from(start).map_err(|_| TypedPlanFault::EdgeGeometryOverflow {
            edges: self.scratch.edges.len(),
        })?;
        let end = usize::try_from(end).map_err(|_| TypedPlanFault::EdgeGeometryOverflow {
            edges: self.scratch.edges.len(),
        })?;
        if start > end || end > self.scratch.edges.len() {
            return Err(TypedPlanFault::EdgeGeometryOverflow {
                edges: self.scratch.edges.len(),
            });
        }
        Ok((start, end))
    }

    /// The full-image writer consumes this already canonical node order.  It
    /// stays private to the semantic-image transaction: raw interner
    /// coordinates never escape as a portable image API.
    pub(crate) fn wire_nodes(&self) -> &[TypedPlanNode] {
        &self.scratch.canonical_nodes
    }

    /// Borrows one source-ordered role-bearing edge run after validating the
    /// node coordinate against the measured graph.  The writer remaps every
    /// endpoint through the canonical plan before it emits a wire cell.
    pub(crate) fn wire_edges(
        &self,
        node: TypedPlanNode,
    ) -> Result<&[TypedPlanEdge], TypedPlanError> {
        let (start, end) = self.edge_range(node)?;
        self.scratch
            .edges
            .get(start..end)
            .ok_or(TypedPlanFault::EdgeGeometryOverflow {
                edges: self.scratch.edges.len(),
            })
            .map_err(Into::into)
    }

    /// Maps a type-bearing source vertex to its exact `(domain, canonical
    /// coordinate)` pair.  Generic typed edges use this rather than a global
    /// row ordinal, so the wire remains independently addressable by every
    /// typed list domain.
    pub(crate) fn wire_node_coordinate(
        &self,
        node: TypedPlanNode,
    ) -> Result<(u8, u32), TypedPlanError> {
        Ok((node.domain().code(), self.canonical_node(node)?))
    }

    pub(crate) fn build_postorder(&mut self) -> Result<(), TypedPlanFault> {
        for start_index in 0..self.scratch.nodes.len() {
            let start = self.scratch.nodes[start_index];
            let start_slot = self.slot(start)?;
            if self.scratch.states[start_slot] != 0 {
                continue;
            }
            self.scratch.states[start_slot] = 1;
            let (edge_start, edge_end) = self.edge_range(start)?;
            self.scratch.stack.push(TypedPlanFrame::new(
                start,
                u32::try_from(edge_start).map_err(|_| TypedPlanFault::EdgeGeometryOverflow {
                    edges: self.scratch.edges.len(),
                })?,
                u32::try_from(edge_end).map_err(|_| TypedPlanFault::EdgeGeometryOverflow {
                    edges: self.scratch.edges.len(),
                })?,
            ));

            loop {
                let Some(frame) = self.scratch.stack.last().copied() else {
                    break;
                };
                if frame.next < frame.edge_start || frame.next > frame.edge_end {
                    return Err(TypedPlanFault::EdgeGeometryOverflow {
                        edges: self.scratch.edges.len(),
                    });
                }
                if frame.next == frame.edge_end {
                    let node = frame.node;
                    let slot = self.slot(node)?;
                    self.scratch.states[slot] = 2;
                    self.scratch.postorder.push(node);
                    self.scratch.stack.pop();
                    continue;
                }
                let edge_index = usize::try_from(frame.next).map_err(|_| {
                    TypedPlanFault::EdgeGeometryOverflow {
                        edges: self.scratch.edges.len(),
                    }
                })?;
                let next =
                    frame
                        .next
                        .checked_add(1)
                        .ok_or(TypedPlanFault::EdgeGeometryOverflow {
                            edges: self.scratch.edges.len(),
                        })?;
                if let Some(active) = self.scratch.stack.last_mut() {
                    active.next = next;
                } else {
                    return Err(TypedPlanFault::EdgeGeometryOverflow {
                        edges: self.scratch.edges.len(),
                    });
                }
                let from = frame.node;
                let edge = *self.scratch.edges.get(edge_index).ok_or(
                    TypedPlanFault::EdgeGeometryOverflow {
                        edges: self.scratch.edges.len(),
                    },
                )?;
                let TypedPlanTarget::Node(target) = edge.target else {
                    continue;
                };
                let target_slot = self.slot(target)?;
                match self.scratch.states[target_slot] {
                    0 => {
                        self.scratch.states[target_slot] = 1;
                        let (edge_start, edge_end) = self.edge_range(target)?;
                        self.scratch.stack.push(TypedPlanFrame::new(
                            target,
                            u32::try_from(edge_start).map_err(|_| {
                                TypedPlanFault::EdgeGeometryOverflow {
                                    edges: self.scratch.edges.len(),
                                }
                            })?,
                            u32::try_from(edge_end).map_err(|_| {
                                TypedPlanFault::EdgeGeometryOverflow {
                                    edges: self.scratch.edges.len(),
                                }
                            })?,
                        ));
                    }
                    1 => return Err(TypedPlanFault::AnonymousCycle { from, to: target }),
                    2 => {}
                    _ => return Err(TypedPlanFault::AnonymousCycle { from, to: target }),
                }
            }
        }
        Ok(())
    }

    fn materialize_fingerprints(&mut self) -> Result<(), TypedPlanError> {
        for postorder_index in 0..self.scratch.postorder.len() {
            let node = self.scratch.postorder[postorder_index];
            let slot = self.slot(node)?;
            let (edge_start, edge_end) = self.edge_range(node)?;
            let edge_count =
                edge_end
                    .checked_sub(edge_start)
                    .ok_or(TypedPlanFault::EdgeGeometryOverflow {
                        edges: self.scratch.edges.len(),
                    })?;
            let local_len =
                local_key_len(edge_count).ok_or(TypedPlanFault::KeyLengthOverflow { node })?;
            let start = self.scratch.key_bytes.len();
            let end = start
                .checked_add(local_len)
                .ok_or(TypedPlanFault::KeyLengthOverflow { node })?;
            let start_wire =
                u32::try_from(start).map_err(|_| TypedPlanFault::KeyLengthOverflow { node })?;
            let len_wire =
                u32::try_from(local_len).map_err(|_| TypedPlanFault::KeyLengthOverflow { node })?;
            self.scratch.key_bytes.reserve(local_len);
            self.scratch.key_bytes.push(node.domain().code());
            self.scratch.key_bytes.extend_from_slice(
                &u32::try_from(edge_count)
                    .map_err(|_| TypedPlanFault::KeyLengthOverflow { node })?
                    .to_le_bytes(),
            );
            for edge in &self.scratch.edges[edge_start..edge_end] {
                append_role(&mut self.scratch.key_bytes, edge.role);
                let (target_tag, fingerprint) = self.target_fingerprint(edge.target)?;
                append_target(&mut self.scratch.key_bytes, target_tag, fingerprint);
            }
            if self.scratch.key_bytes.len() != end {
                return Err(TypedPlanFault::KeyLengthOverflow { node }.into());
            }
            let bytes = self
                .scratch
                .key_bytes
                .get(start..end)
                .ok_or(TypedPlanFault::KeyLengthOverflow { node })?;
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"nudox.semantic-image.typed-fingerprint.v1\0");
            hasher.update(bytes);
            self.scratch.fingerprints[slot] = TypedFingerprint(*hasher.finalize().as_bytes());
            self.scratch.key_ranges[slot] = ArenaRange {
                start: start_wire,
                len: len_wire,
            };
        }
        Ok(())
    }

    fn target_fingerprint(
        &self,
        target: TypedPlanTarget,
    ) -> Result<(u8, [u8; 32]), TypedPlanError> {
        match target {
            TypedPlanTarget::Node(node) => {
                let slot = self.slot(node)?;
                Ok((0, self.scratch.fingerprints[slot].as_bytes()))
            }
            TypedPlanTarget::Atom(atom) => Ok((1, self.canonical.atom_fingerprint(atom)?)),
            TypedPlanTarget::Entity(entity) => {
                let identity = self.canonical.entity_identity(entity)?;
                Ok((2, identity_bytes(identity)))
            }
            TypedPlanTarget::External(external) => {
                Ok((3, self.canonical.external_fingerprint(external)?))
            }
            TypedPlanTarget::Scalar(value) => {
                let mut bytes = [0_u8; 32];
                bytes[..8].copy_from_slice(&value.to_le_bytes());
                Ok((4, bytes))
            }
        }
    }
}

const PLAN_DOMAINS: [TypedPlanDomain; 8] = [
    TypedPlanDomain::Type,
    TypedPlanDomain::TypeList,
    TypedPlanDomain::TupleElements,
    TypedPlanDomain::ObjectMembers,
    TypedPlanDomain::TemplateParts,
    TypedPlanDomain::AtomList,
    TypedPlanDomain::TypeParameters,
    TypedPlanDomain::TypeParameterBounds,
];

fn for_each_node(
    counts: TypedPlanCounts,
    mut visit: impl FnMut(TypedPlanNode) -> Result<(), TypedPlanError>,
) -> Result<(), TypedPlanError> {
    for domain in PLAN_DOMAINS {
        for raw in 0..counts.at(domain) {
            visit(TypedPlanNode::from_domain_raw(domain, raw))?;
        }
    }
    Ok(())
}

fn validate_edge(
    canonical: &CanonicalFullPlan<'_>,
    bases: TypedPlanBases,
    counts: TypedPlanCounts,
    from: TypedPlanNode,
    edge: TypedPlanEdge,
) -> Result<(), TypedPlanError> {
    match edge.target {
        TypedPlanTarget::Node(node) => {
            bases
                .slot(node, counts.as_array())
                .map_err(|fault| match fault {
                    TypedPlanFault::MissingNode { node, count } => {
                        TypedPlanFault::EdgeOutOfBounds {
                            from,
                            to: node,
                            count,
                        }
                    }
                    other => other,
                })?;
        }
        TypedPlanTarget::Atom(atom) => {
            let count = u32::try_from(canonical.core.atoms.len()).map_err(|_| {
                TypedPlanFault::GeometryOverflow {
                    nodes: canonical.core.atoms.len(),
                }
            })?;
            if atom.index()
                >= usize::try_from(count).map_err(|_| TypedPlanFault::GeometryOverflow {
                    nodes: canonical.core.atoms.len(),
                })?
            {
                return Err(TypedPlanFault::TerminalOutOfBounds {
                    from,
                    target: TypedPlanTerminal::Atom(atom),
                    count,
                }
                .into());
            }
        }
        TypedPlanTarget::Entity(entity) => {
            let count = u32::try_from(canonical.core.entities.len()).map_err(|_| {
                TypedPlanFault::GeometryOverflow {
                    nodes: canonical.core.entities.len(),
                }
            })?;
            if entity.index()
                >= usize::try_from(count).map_err(|_| TypedPlanFault::GeometryOverflow {
                    nodes: canonical.core.entities.len(),
                })?
            {
                return Err(TypedPlanFault::TerminalOutOfBounds {
                    from,
                    target: TypedPlanTerminal::Entity(entity),
                    count,
                }
                .into());
            }
        }
        TypedPlanTarget::External(external) => {
            let count = u32::try_from(canonical.externals.len()).map_err(|_| {
                TypedPlanFault::GeometryOverflow {
                    nodes: canonical.externals.len(),
                }
            })?;
            if external.index()
                >= usize::try_from(count).map_err(|_| TypedPlanFault::GeometryOverflow {
                    nodes: canonical.externals.len(),
                })?
            {
                return Err(TypedPlanFault::TerminalOutOfBounds {
                    from,
                    target: TypedPlanTerminal::External(external),
                    count,
                }
                .into());
            }
        }
        TypedPlanTarget::Scalar(_) => {}
    }
    Ok(())
}

fn local_key_len(edge_count: usize) -> Option<usize> {
    const PREFIX: usize = 1 + 4;
    const EDGE: usize = 1 + 4 + 1 + 32;
    PREFIX.checked_add(edge_count.checked_mul(EDGE)?)
}

fn append_role(out: &mut Vec<u8>, role: TypedEdgeRole) {
    let (tag, index) = role_key(role);
    out.push(tag);
    out.extend_from_slice(&index.to_le_bytes());
}

fn append_target(out: &mut Vec<u8>, tag: u8, fingerprint: [u8; 32]) {
    out.push(tag);
    out.extend_from_slice(&fingerprint);
}

pub(crate) fn target_tag(target: TypedPlanTarget) -> u8 {
    match target {
        TypedPlanTarget::Node(_) => 0,
        TypedPlanTarget::Atom(_) => 1,
        TypedPlanTarget::Entity(_) => 2,
        TypedPlanTarget::External(_) => 3,
        TypedPlanTarget::Scalar(_) => 4,
    }
}

pub(crate) fn compare_role(left: TypedEdgeRole, right: TypedEdgeRole) -> Ordering {
    role_key(left).cmp(&role_key(right))
}

pub(crate) fn role_key(role: TypedEdgeRole) -> (u8, u32) {
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
    }
}

pub(crate) fn identity_bytes(identity: DeclarationIdentity) -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(identity.family.as_bytes());
    bytes[16..].copy_from_slice(identity.variant.as_bytes());
    bytes
}

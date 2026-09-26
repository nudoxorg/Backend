//! Grouped cursor over one typed node's role-bearing edges.
//!
//! Sequential `take` walks the wire in emission order. Random field lookup
//! builds one grouped index for that node and then answers by binary search.

use crate::ir::{
    AtomId, AtomListId, EntityId, ExternalId, ObjectMemberListId, TemplatePartListId,
    TupleElementListId, TypeId, TypeListId,
};

use super::super::{
    fault::{FullSemanticImageFault, FullSemanticImageField},
    typed::role_from_wire,
    validate::TypedLayout,
    wire::{
        FullDirectoryKind, FullImageLayout, TYPED_EDGE_ROW_BYTES, TYPED_NODE_ROW_BYTES, get_u32,
    },
};
use super::{
    ATOM_LIST, FREE_PREDICATES, OBJECT_MEMBERS, TEMPLATE_PARTS, TUPLE_ELEMENTS, TYPE, TYPE_LIST,
    TYPE_PARAMETER_BOUNDS, TYPE_PARAMETERS,
};

/// Logical item count for the already-opened node cursor.  Tuple, object,
/// template, parameter, and bound lists carry several role-bearing edges per
/// logical item, so their raw edge counts are never cursor lengths.  The
/// grouped adjacency answers this from the last group of the role (its keys
/// are sorted, so one role occupies contiguous groups) instead of rescanning
/// the whole run.
pub(crate) fn logical_count(
    edges: &mut Edges<'_>,
    domain: u8,
) -> Result<u32, FullSemanticImageFault> {
    let role: u8 = match domain {
        TYPE_LIST | ATOM_LIST => 2,
        TUPLE_ELEMENTS => 3,
        OBJECT_MEMBERS => 6,
        TEMPLATE_PARTS => 13,
        TYPE_PARAMETERS => 14,
        TYPE_PARAMETER_BOUNDS => 22,
        FREE_PREDICATES => 24,
        _ => {
            return Err(FullSemanticImageFault::TypedDomain {
                node: edges.node,
                domain,
            });
        }
    };
    if edges.grouped.is_none() {
        edges.grouped = Some(GroupedEdges::build(edges)?);
    }
    let grouped = edges.grouped.as_ref().expect("grouped index was built");
    let after = grouped
        .keys
        .partition_point(|key| (*key >> 32) <= u64::from(role));
    if after == 0 || (grouped.keys[after - 1] >> 32) != u64::from(role) {
        return Ok(0);
    }
    let maximum = (grouped.keys[after - 1] & 0xffff_ffff) as u32;
    maximum
        .checked_add(1)
        .ok_or(FullSemanticImageFault::LengthOverflow {
            field: FullSemanticImageField::TypedEdges,
        })
}

pub(in crate::ir::semantic_image::full_wire) struct Edges<'a> {
    bytes: &'a [u8],
    layout: FullImageLayout,
    pub(super) node: u32,
    start: u32,
    pub(super) count: u32,
    pub(super) next: u32,
    /// Grouped `(role, index)` adjacency, built once on the first random
    /// access.  The wire keeps edges in per-item emission order, so a random
    /// field lookup otherwise rescans every edge of the node; decoding a
    /// length-`n` list row by row would then cost `O(n^2)` edge parses for
    /// one node (the observation digests iterate whole lists).  The index is
    /// allocation-bounded by the already-admitted edge run of this node
    /// (`<= 16` bytes per edge row, one small vector triple per node), the
    /// decoded bytes stay immutable, and sequential `take` decoding never
    /// builds it at all.
    grouped: Option<GroupedEdges>,
}

/// Sorted `(role, index)` groups over one node's edge run.  `order` holds the
/// edge ordinals grouped by key with each group in stored (emission) order, so
/// the k-th entry of a group is exactly the k-th match of the former linear
/// scan.
struct GroupedEdges {
    keys: Vec<u64>,
    starts: Vec<u32>,
    order: Vec<u32>,
}

impl GroupedEdges {
    fn key(role_tag: u8, role_index: u32) -> u64 {
        (u64::from(role_tag) << 32) | u64::from(role_index)
    }

    fn build(edges: &Edges<'_>) -> Result<Self, FullSemanticImageFault> {
        let mut ordinals: Vec<(u64, u32)> = Vec::with_capacity(edges.count as usize);
        for ordinal in 0..edges.count {
            let edge = edges.edge(ordinal)?;
            ordinals.push((Self::key(edge.role_tag, edge.role_index), ordinal));
        }
        // A stable sort keeps each `(role, index)` group in stored edge
        // order, preserving the linear scan's occurrence semantics.
        ordinals.sort_by_key(|(key, _)| *key);
        let mut keys = Vec::with_capacity(ordinals.len());
        let mut starts = Vec::new();
        let mut order = Vec::with_capacity(ordinals.len());
        for (position, (key, ordinal)) in ordinals.into_iter().enumerate() {
            if keys.last().map_or(true, |previous| *previous != key) {
                starts.push(u32::try_from(position).map_err(|_| {
                    FullSemanticImageFault::LengthOverflow {
                        field: FullSemanticImageField::TypedEdges,
                    }
                })?);
                keys.push(key);
            }
            order.push(ordinal);
        }
        starts.push(u32::try_from(order.len()).map_err(|_| {
            FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            }
        })?);
        Ok(Self {
            keys,
            starts,
            order,
        })
    }

    fn find(&self, role: u8, index: u32) -> Option<(u32, u32)> {
        let key = Self::key(role, index);
        let group = self.keys.binary_search(&key).ok()?;
        let start = *self.starts.get(group)?;
        let end = *self.starts.get(group.checked_add(1)?)?;
        Some((start, end))
    }
}

#[derive(Clone, Copy)]
pub(super) struct Edge {
    role_tag: u8,
    role_index: u32,
    target_tag: u8,
    domain: u8,
    low: u32,
    high: u32,
}

impl<'a> Edges<'a> {
    pub(in crate::ir::semantic_image::full_wire) fn for_node(
        bytes: &'a [u8],
        layout: FullImageLayout,
        typed: TypedLayout,
        domain: u8,
        coordinate: u32,
    ) -> Result<Self, FullSemanticImageFault> {
        let count = count(typed, domain)?;
        if coordinate >= count {
            return Err(reference(
                FullSemanticImageField::TypedNodes,
                coordinate,
                count,
                coordinate,
            ));
        }
        let start = typed
            .start(domain)
            .ok_or(FullSemanticImageFault::TypedDomain {
                node: coordinate,
                domain,
            })?;
        let node = start
            .checked_add(coordinate)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedNodes,
            })?;
        let offset = node_offset(layout, node)?;
        if bytes[offset] != domain
            || get_u32(bytes, offset + 4, FullSemanticImageField::TypedNodes)? != coordinate
        {
            return Err(shape(node));
        }
        Ok(Self {
            bytes,
            layout,
            node,
            start: get_u32(bytes, offset + 8, FullSemanticImageField::TypedNodes)?,
            count: get_u32(bytes, offset + 12, FullSemanticImageField::TypedNodes)?,
            next: 0,
            grouped: None,
        })
    }

    fn edge(&self, ordinal: u32) -> Result<Edge, FullSemanticImageFault> {
        if ordinal >= self.count {
            return Err(shape(self.node));
        }
        let row =
            self.start
                .checked_add(ordinal)
                .ok_or(FullSemanticImageFault::LengthOverflow {
                    field: FullSemanticImageField::TypedEdges,
                })?;
        let offset = edge_offset(self.layout, row)?;
        let role_tag = self.bytes[offset];
        let role_index = get_u32(self.bytes, offset + 4, FullSemanticImageField::TypedEdges)?;
        if role_from_wire(role_tag, role_index).is_none() {
            return Err(FullSemanticImageFault::TypedRole {
                node: self.node,
                edge: row,
                role: role_tag,
            });
        }
        Ok(Edge {
            role_tag,
            role_index,
            target_tag: self.bytes[offset + 1],
            domain: self.bytes[offset + 2],
            low: get_u32(self.bytes, offset + 8, FullSemanticImageField::TypedEdges)?,
            high: get_u32(self.bytes, offset + 12, FullSemanticImageField::TypedEdges)?,
        })
    }

    pub(super) fn take(&mut self, role: u8, index: u32) -> Result<Edge, FullSemanticImageFault> {
        let edge = self.edge(self.next)?;
        self.next = self
            .next
            .checked_add(1)
            .ok_or(FullSemanticImageFault::LengthOverflow {
                field: FullSemanticImageField::TypedEdges,
            })?;
        if edge.role_tag != role || edge.role_index != index {
            return Err(shape(self.node));
        }
        Ok(edge)
    }
    pub(super) fn finish(&self) -> Result<(), FullSemanticImageFault> {
        if self.next == self.count {
            Ok(())
        } else {
            Err(shape(self.node))
        }
    }
    pub(super) fn scalar(&mut self, role: u8, index: u32) -> Result<u64, FullSemanticImageFault> {
        scalar(self.take(role, index)?, self.node)
    }
    pub(super) fn atom(&mut self, role: u8, index: u32) -> Result<AtomId, FullSemanticImageFault> {
        atom(self.take(role, index)?, self.node)
    }
    pub(super) fn entity(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<EntityId, FullSemanticImageFault> {
        entity(self.take(role, index)?, self.node)
    }
    pub(super) fn external(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<ExternalId, FullSemanticImageFault> {
        external(self.take(role, index)?, self.node)
    }
    pub(super) fn type_node(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<TypeId, FullSemanticImageFault> {
        node(self.take(role, index)?, TYPE, self.node)
    }
    pub(super) fn type_list(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<TypeListId, FullSemanticImageFault> {
        Ok(TypeListId::new(
            node(self.take(role, index)?, TYPE_LIST, self.node)?.raw,
        ))
    }
    pub(super) fn tuple_elements(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<TupleElementListId, FullSemanticImageFault> {
        Ok(TupleElementListId::new(
            node(self.take(role, index)?, TUPLE_ELEMENTS, self.node)?.raw,
        ))
    }
    pub(super) fn object_members(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<ObjectMemberListId, FullSemanticImageFault> {
        Ok(ObjectMemberListId::new(
            node(self.take(role, index)?, OBJECT_MEMBERS, self.node)?.raw,
        ))
    }
    pub(super) fn template_parts(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<TemplatePartListId, FullSemanticImageFault> {
        Ok(TemplatePartListId::new(
            node(self.take(role, index)?, TEMPLATE_PARTS, self.node)?.raw,
        ))
    }
    pub(super) fn atom_list(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<AtomListId, FullSemanticImageFault> {
        Ok(AtomListId::new(
            node(self.take(role, index)?, ATOM_LIST, self.node)?.raw,
        ))
    }

    /// Returns the `occurrence`-th stored edge whose role is exactly
    /// `(role, index)`.  The grouped adjacency turns the former full-run
    /// scan into one binary search; the returned edge and every fault are
    /// identical to the linear interpretation.
    fn matching(
        &mut self,
        role: u8,
        index: u32,
        occurrence: u32,
    ) -> Result<Edge, FullSemanticImageFault> {
        if self.grouped.is_none() {
            self.grouped = Some(GroupedEdges::build(self)?);
        }
        let grouped = self.grouped.as_ref().expect("grouped index was built");
        let Some((start, end)) = grouped.find(role, index) else {
            return Err(shape(self.node));
        };
        let position = start.checked_add(occurrence).ok_or(shape(self.node))?;
        if position >= end {
            return Err(shape(self.node));
        }
        let ordinal = grouped
            .order
            .get(position as usize)
            .copied()
            .ok_or_else(|| shape(self.node))?;
        self.edge(ordinal)
    }
    pub(super) fn scalar_at(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<u64, FullSemanticImageFault> {
        scalar(self.matching(role, index, 0)?, self.node)
    }
    pub(super) fn scalar_at_n(
        &mut self,
        role: u8,
        index: u32,
        occurrence: u32,
    ) -> Result<u64, FullSemanticImageFault> {
        scalar(self.matching(role, index, occurrence)?, self.node)
    }
    pub(super) fn atom_at(
        &mut self,
        role: u8,
        index: u32,
    ) -> Result<AtomId, FullSemanticImageFault> {
        atom(self.matching(role, index, 0)?, self.node)
    }
    pub(super) fn atom_at_n(
        &mut self,
        role: u8,
        index: u32,
        occurrence: u32,
    ) -> Result<AtomId, FullSemanticImageFault> {
        atom(self.matching(role, index, occurrence)?, self.node)
    }
    pub(super) fn node_at(
        &mut self,
        role: u8,
        index: u32,
        domain: u8,
    ) -> Result<TypeId, FullSemanticImageFault> {
        node(self.matching(role, index, 0)?, domain, self.node)
    }
    pub(super) fn node_at_n(
        &mut self,
        role: u8,
        index: u32,
        occurrence: u32,
        domain: u8,
    ) -> Result<TypeId, FullSemanticImageFault> {
        node(self.matching(role, index, occurrence)?, domain, self.node)
    }
}

fn scalar(edge: Edge, node: u32) -> Result<u64, FullSemanticImageFault> {
    if edge.target_tag != 4 || edge.domain != 0 {
        return Err(shape(node));
    }
    Ok(u64::from(edge.low) | (u64::from(edge.high) << 32))
}
fn atom(edge: Edge, node: u32) -> Result<AtomId, FullSemanticImageFault> {
    if edge.target_tag != 1 || edge.domain != 0 || edge.high != 0 {
        return Err(shape(node));
    }
    Ok(AtomId::new(edge.low))
}
fn entity(edge: Edge, node: u32) -> Result<EntityId, FullSemanticImageFault> {
    if edge.target_tag != 2 || edge.domain != 0 || edge.high != 0 {
        return Err(shape(node));
    }
    Ok(EntityId::new(edge.low))
}
fn external(edge: Edge, node: u32) -> Result<ExternalId, FullSemanticImageFault> {
    if edge.target_tag != 3 || edge.domain != 0 || edge.high != 0 {
        return Err(shape(node));
    }
    Ok(ExternalId::new(edge.low))
}
pub(super) fn node(edge: Edge, expected: u8, from: u32) -> Result<TypeId, FullSemanticImageFault> {
    if edge.target_tag != 0 || edge.domain != expected || edge.high != 0 {
        return Err(shape(from));
    }
    Ok(TypeId::new(edge.low))
}

fn node_offset(layout: FullImageLayout, row: u32) -> Result<usize, FullSemanticImageFault> {
    super::super::decode::fixed_offset(
        layout,
        FullDirectoryKind::TypedNodes,
        row,
        TYPED_NODE_ROW_BYTES,
    )
}
fn edge_offset(layout: FullImageLayout, row: u32) -> Result<usize, FullSemanticImageFault> {
    super::super::decode::fixed_offset(
        layout,
        FullDirectoryKind::TypedEdges,
        row,
        TYPED_EDGE_ROW_BYTES,
    )
}
pub(super) fn count(typed: TypedLayout, domain: u8) -> Result<u32, FullSemanticImageFault> {
    typed
        .count(domain)
        .ok_or(FullSemanticImageFault::TypedDomain { node: 0, domain })
}
pub(super) fn shape(node: u32) -> FullSemanticImageFault {
    FullSemanticImageFault::TypedShape { node }
}
fn reference(
    field: FullSemanticImageField,
    row: u32,
    expected: u32,
    observed: u32,
) -> FullSemanticImageFault {
    FullSemanticImageFault::Reference {
        field,
        row,
        expected,
        observed,
    }
}

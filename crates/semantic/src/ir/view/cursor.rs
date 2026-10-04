//! Defines view cursor behavior for `backend-semantic::ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the view cursor invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::ir::{AtomId, EntityId};

use crate::ir::{
    EntityType, TypeNode,
    view::FragmentView,
    wire::{
        ATOM_RECORD_BYTES, ENTITY_BYTES, TYPE_NODE_BYTES, decode_validated_entity,
        decode_validated_type_node, read_u32,
    },
};

impl<'fragment> FragmentView<'fragment> {
    /// Iterates validated entity rows in wire order, assigning their zero-based entity ordinals.
    pub fn entities(&self) -> EntityCursor<'fragment> {
        EntityCursor {
            remaining: self.entities,
            next_ordinal: 0,
        }
    }

    /// Iterates validated type-node rows in their canonical wire order.
    pub fn type_nodes(&self) -> TypeNodeCursor<'fragment> {
        TypeNodeCursor {
            remaining: self.type_nodes,
        }
    }

    /// Iterates atom records and borrows each atom's bytes from the validated pool.
    pub fn atoms(&self) -> AtomCursor<'fragment> {
        AtomCursor {
            records: self.atoms,
            bytes: self.atom_bytes,
            next_ordinal: 0,
        }
    }
}

/// Exact-size, fused iterator over validated entity records.
///
/// Each yielded [`EntityType`] carries the row ordinal as its [`EntityId`].
pub struct EntityCursor<'fragment> {
    remaining: &'fragment [u8],
    next_ordinal: u32,
}

impl Iterator for EntityCursor<'_> {
    type Item = EntityType;

    fn next(&mut self) -> Option<Self::Item> {
        let (record, remaining) = self.remaining.split_first_chunk::<ENTITY_BYTES>()?;
        let entity = EntityId::new(self.next_ordinal);
        self.next_ordinal += 1;
        self.remaining = remaining;
        let decoded = decode_validated_entity(record);
        Some(EntityType {
            entity,
            semantic_type: decoded.semantic_type,
            name: decoded.name,
            kind: decoded.kind,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.remaining.len() / ENTITY_BYTES;
        (count, Some(count))
    }
}

impl ExactSizeIterator for EntityCursor<'_> {}
impl core::iter::FusedIterator for EntityCursor<'_> {}

/// Exact-size, fused iterator over validated type-node records.
///
/// Items retain the canonical row order established by fragment validation.
pub struct TypeNodeCursor<'fragment> {
    remaining: &'fragment [u8],
}

impl Iterator for TypeNodeCursor<'_> {
    type Item = TypeNode;

    fn next(&mut self) -> Option<Self::Item> {
        let (record, remaining) = self.remaining.split_first_chunk::<TYPE_NODE_BYTES>()?;
        self.remaining = remaining;
        Some(decode_validated_type_node(record))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.remaining.len() / TYPE_NODE_BYTES;
        (count, Some(count))
    }
}

impl ExactSizeIterator for TypeNodeCursor<'_> {}
impl core::iter::FusedIterator for TypeNodeCursor<'_> {}

/// One atom borrowing the fragment's validated atom-byte pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Atom<'fragment> {
    /// Zero-based atom-record ordinal from the fragment's atom lane.
    pub ordinal: AtomId,
    /// Borrowed bytes selected by this record's validated start and length.
    pub bytes: &'fragment [u8],
}

/// Exact-size, fused iterator over atom records and their borrowed byte slices.
pub struct AtomCursor<'fragment> {
    records: &'fragment [u8],
    bytes: &'fragment [u8],
    next_ordinal: u32,
}

impl<'fragment> Iterator for AtomCursor<'fragment> {
    type Item = Atom<'fragment>;

    fn next(&mut self) -> Option<Self::Item> {
        let (record, remaining) = self.records.split_first_chunk::<ATOM_RECORD_BYTES>()?;
        let ordinal = AtomId::new(self.next_ordinal);
        self.next_ordinal += 1;
        self.records = remaining;
        let start = validated_wire_index(read_u32(record, 0));
        let length = validated_wire_index(read_u32(record, size_of::<u32>()));
        let end = start + length;
        Some(Atom {
            ordinal,
            bytes: &self.bytes[start..end],
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.records.len() / ATOM_RECORD_BYTES;
        (count, Some(count))
    }
}

impl ExactSizeIterator for AtomCursor<'_> {}
impl core::iter::FusedIterator for AtomCursor<'_> {}

#[allow(
    clippy::as_conversions,
    reason = "the target-width gate admits u32 coordinates and FragmentView validation proves every atom coordinate"
)]
fn validated_wire_index(value: u32) -> usize {
    value as usize
}

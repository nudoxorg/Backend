use nudox_ir_vocab::{AtomId, EntityId};

use crate::{
    EntityType, SourceIdentity, TypeNode,
    view::FragmentView,
    wire::{
        ATOM_RECORD_BYTES, ENTITY_BYTES, TYPE_NODE_BYTES, decode_validated_entity,
        decode_validated_type_node, read_u32,
    },
};

impl<'fragment> FragmentView<'fragment> {
    /// Returns the source fact validated from this fragment's required identity lane.
    #[must_use]
    pub fn source_identity(&self) -> SourceIdentity {
        self.source_identity
    }

    pub fn entities(&self) -> EntityCursor<'fragment> {
        EntityCursor {
            remaining: self.entities,
            next_ordinal: 0,
        }
    }

    pub fn type_nodes(&self) -> TypeNodeCursor<'fragment> {
        TypeNodeCursor {
            remaining: self.type_nodes,
        }
    }

    pub fn atoms(&self) -> AtomCursor<'fragment> {
        AtomCursor {
            records: self.atoms,
            bytes: self.atom_bytes,
            next_ordinal: 0,
        }
    }
}

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
    pub ordinal: AtomId,
    pub bytes: &'fragment [u8],
}

pub struct AtomCursor<'fragment> {
    records: &'fragment [u8],
    bytes: &'fragment [u8],
    next_ordinal: u32,
}

impl<'fragment> Iterator for AtomCursor<'fragment> {
    type Item = Atom<'fragment>;

    fn next(&mut self) -> Option<Self::Item> {
        let (record, remaining) = self.records.split_first_chunk::<ATOM_RECORD_BYTES>()?;
        // `FragmentView::validate` rejects any record whose u32 coordinates lie
        // outside this already borrowed atom pool.  The pool itself was admitted
        // through checked wire-to-native conversion, so these values are exact
        // native indices rather than a fallible stream terminal.
        let start = read_u32(record, 0) as usize;
        let length = read_u32(record, size_of::<u32>()) as usize;
        let end = start + length;
        let bytes = &self.bytes[start..end];
        let ordinal = AtomId::new(self.next_ordinal);
        self.next_ordinal += 1;
        self.records = remaining;
        Some(Atom { ordinal, bytes })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let count = self.records.len() / ATOM_RECORD_BYTES;
        (count, Some(count))
    }
}

impl ExactSizeIterator for AtomCursor<'_> {}
impl core::iter::FusedIterator for AtomCursor<'_> {}

use nudox_ir_vocab::{EntityId, TypeId};

use crate::{
    EntityType, TypeNode,
    view::FragmentView,
    wire::{ENTITY_BYTES, TYPE_NODE_BYTES, decode_validated_type_node},
};

impl<'fragment> FragmentView<'fragment> {
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
        Some(EntityType {
            entity,
            semantic_type: TypeId::new(u32::from_le_bytes(*record)),
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

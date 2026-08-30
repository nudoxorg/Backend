use core::num::TryFromIntError;

use nudox_ir_vocab::{EntityId, TypeId};
use thiserror::Error;

use crate::{
    EntityFault, EntityRecord, TypeNode, TypeNodeFault,
    wire::{
        ByteLength, ByteOffset, DIRECTORY_ENTRY_LAYOUT, ENTITY_BYTES, FragmentLayout,
        HEADER_LAYOUT, ItemCount, LaneLayout, SectionKind, SectionRequirement, TYPE_NODE_BYTES,
        WRITTEN_SECTION_COUNT, entity_fault, type_node_fault, write_type_node, write_u16,
        write_u32,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutStep {
    Directory,
    EntityLane,
    TypeNodeLane,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PrepareError {
    #[error("entity count {actual} exceeds the fragment count width")]
    EntityCount {
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error("type node count {actual} exceeds the fragment count width")]
    TypeNodeCount {
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error(
        "fragment layout overflow at {step:?} for {entity_count} entities and {type_node_count} type nodes"
    )]
    LayoutOverflow {
        step: LayoutStep,
        entity_count: u32,
        type_node_count: u32,
    },
    #[error("{step:?} item count {actual} exceeds the native address width")]
    NativeCount {
        step: LayoutStep,
        actual: u32,
        #[source]
        source: TryFromIntError,
    },
    #[error("fragment output length {actual} exceeds the wire byte-coordinate width")]
    OutputLength {
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error("entity {ordinal:?} is invalid: {fault}")]
    Entity {
        ordinal: EntityId,
        #[source]
        fault: EntityFault,
    },
    #[error("type node {ordinal:?} is invalid: {fault}")]
    TypeNode {
        ordinal: TypeId,
        #[source]
        fault: TypeNodeFault,
    },
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WriteError {
    #[error("fragment output needs {required} bytes but only {available} are available")]
    OutputTooSmall { required: usize, available: usize },
}

pub struct PreparedFragment<'facts> {
    entities: &'facts [EntityRecord],
    type_nodes: &'facts [TypeNode],
    layout: FragmentLayout,
}

impl<'facts> PreparedFragment<'facts> {
    pub fn prepare(
        entities: &'facts [EntityRecord],
        type_nodes: &'facts [TypeNode],
    ) -> Result<Self, PrepareError> {
        let entity_count =
            ItemCount::try_from(entities.len()).map_err(|source| PrepareError::EntityCount {
                actual: entities.len(),
                source,
            })?;
        let type_node_count = ItemCount::try_from(type_nodes.len()).map_err(|source| {
            PrepareError::TypeNodeCount {
                actual: type_nodes.len(),
                source,
            }
        })?;
        let layout = layout(entity_count, type_node_count)?;

        for (ordinal, entity) in (0..u32::from(entity_count)).zip(entities) {
            if let Some(fault) = entity_fault(entity.semantic_type, type_node_count) {
                return Err(PrepareError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault,
                });
            }
        }
        for (ordinal, node) in (0..u32::from(type_node_count)).zip(type_nodes.iter().copied()) {
            if let Some(fault) = type_node_fault(node, type_node_count) {
                return Err(PrepareError::TypeNode {
                    ordinal: TypeId::new(ordinal),
                    fault,
                });
            }
        }

        Ok(Self {
            entities,
            type_nodes,
            layout,
        })
    }

    #[must_use]
    pub const fn required_capacity(&self) -> usize {
        self.layout.output_len
    }

    pub fn write_into<'output>(
        &self,
        output: &'output mut [u8],
    ) -> Result<&'output [u8], WriteError> {
        if output.len() < self.layout.output_len {
            return Err(WriteError::OutputTooSmall {
                required: self.layout.output_len,
                available: output.len(),
            });
        }

        let written = &mut output[..self.layout.output_len];
        written[HEADER_LAYOUT.magic..HEADER_LAYOUT.schema].copy_from_slice(&crate::FRAGMENT_MAGIC);
        write_u16(written, HEADER_LAYOUT.schema, crate::FRAGMENT_SCHEMA);
        write_u16(
            written,
            HEADER_LAYOUT.section_count,
            u16::from(WRITTEN_SECTION_COUNT),
        );
        write_u32(
            written,
            HEADER_LAYOUT.declared_length,
            u32::from(self.layout.output_wire_len),
        );
        write_directory_entry(written, 0, SectionKind::EntityTypes, self.layout.entities);
        write_directory_entry(written, 1, SectionKind::TypeNodes, self.layout.type_nodes);

        let mut entity_cursor = self.layout.entities.start_index;
        for entity in self.entities {
            write_u32(written, entity_cursor, entity.semantic_type.raw);
            entity_cursor += ENTITY_BYTES;
        }
        let mut type_cursor = self.layout.type_nodes.start_index;
        for node in self.type_nodes {
            write_type_node(
                &mut written[type_cursor..type_cursor + TYPE_NODE_BYTES],
                *node,
            );
            type_cursor += TYPE_NODE_BYTES;
        }
        Ok(written)
    }
}

fn layout(
    entity_count: ItemCount,
    type_node_count: ItemCount,
) -> Result<FragmentLayout, PrepareError> {
    let mut cursor = LayoutCursor::new(entity_count, type_node_count)?;
    let entities = cursor.lane(LayoutStep::EntityLane, entity_count, ENTITY_BYTES)?;
    let type_nodes = cursor.lane(LayoutStep::TypeNodeLane, type_node_count, TYPE_NODE_BYTES)?;
    cursor.finish(entities, type_nodes)
}

struct LayoutCursor {
    next_index: usize,
    entity_count: ItemCount,
    type_node_count: ItemCount,
}

impl LayoutCursor {
    fn new(entity_count: ItemCount, type_node_count: ItemCount) -> Result<Self, PrepareError> {
        let mut cursor = Self {
            next_index: HEADER_LAYOUT.encoded_len,
            entity_count,
            type_node_count,
        };
        let directory_bytes = usize::from(WRITTEN_SECTION_COUNT)
            .checked_mul(DIRECTORY_ENTRY_LAYOUT.encoded_len)
            .ok_or_else(|| cursor.overflow(LayoutStep::Directory))?;
        cursor.advance(LayoutStep::Directory, directory_bytes)?;
        Ok(cursor)
    }

    fn lane(
        &mut self,
        step: LayoutStep,
        count: ItemCount,
        item_width: usize,
    ) -> Result<LaneLayout, PrepareError> {
        let native_count = usize::try_from(count).map_err(|source| PrepareError::NativeCount {
            step,
            actual: u32::from(count),
            source,
        })?;
        let length = native_count
            .checked_mul(item_width)
            .ok_or_else(|| self.overflow(step))?;
        let start_index = self.next_index;
        self.advance(step, length)?;
        let start =
            ByteOffset::try_from(start_index).map_err(|source| PrepareError::OutputLength {
                actual: start_index,
                source,
            })?;
        let length = ByteLength::try_from(length).map_err(|source| PrepareError::OutputLength {
            actual: length,
            source,
        })?;
        Ok(LaneLayout {
            count,
            start,
            length,
            start_index,
            end_index: self.next_index,
        })
    }

    fn advance(&mut self, step: LayoutStep, length: usize) -> Result<(), PrepareError> {
        self.next_index = self
            .next_index
            .checked_add(length)
            .ok_or_else(|| self.overflow(step))?;
        Ok(())
    }

    fn finish(
        self,
        entities: LaneLayout,
        type_nodes: LaneLayout,
    ) -> Result<FragmentLayout, PrepareError> {
        let output_wire_len =
            ByteLength::try_from(self.next_index).map_err(|source| PrepareError::OutputLength {
                actual: self.next_index,
                source,
            })?;
        Ok(FragmentLayout {
            entities,
            type_nodes,
            output_len: self.next_index,
            output_wire_len,
        })
    }

    fn overflow(&self, step: LayoutStep) -> PrepareError {
        PrepareError::LayoutOverflow {
            step,
            entity_count: u32::from(self.entity_count),
            type_node_count: u32::from(self.type_node_count),
        }
    }
}

fn write_directory_entry(output: &mut [u8], ordinal: usize, kind: SectionKind, lane: LaneLayout) {
    let start = HEADER_LAYOUT.encoded_len + ordinal * DIRECTORY_ENTRY_LAYOUT.encoded_len;
    write_u16(output, start + DIRECTORY_ENTRY_LAYOUT.kind, u16::from(kind));
    write_u16(
        output,
        start + DIRECTORY_ENTRY_LAYOUT.requirement,
        u16::from(SectionRequirement::Required),
    );
    write_u32(
        output,
        start + DIRECTORY_ENTRY_LAYOUT.item_count,
        u32::from(lane.count),
    );
    write_u32(
        output,
        start + DIRECTORY_ENTRY_LAYOUT.byte_offset,
        u32::from(lane.start),
    );
    write_u32(
        output,
        start + DIRECTORY_ENTRY_LAYOUT.byte_length,
        u32::from(lane.length),
    );
}

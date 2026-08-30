use core::num::TryFromIntError;

use nudox_ir_vocab::{EntityId, TypeId};
use thiserror::Error;

use crate::{
    EntityFault, EntityRecord, TypeNode, TypeNodeFault,
    wire::{
        ByteLength, ByteOffset, DIRECTORY_ENTRY_BYTES, ENTITY_BYTES, FragmentLayout, HEADER_BYTES,
        ItemCount, LaneLayout, SectionKind, SectionRequirement, TYPE_NODE_BYTES,
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
            ItemCount::from_len(entities.len()).map_err(|source| PrepareError::EntityCount {
                actual: entities.len(),
                source,
            })?;
        let type_node_count = ItemCount::from_len(type_nodes.len()).map_err(|source| {
            PrepareError::TypeNodeCount {
                actual: type_nodes.len(),
                source,
            }
        })?;
        let layout = layout(entity_count, type_node_count)?;

        for (ordinal, entity) in (0..entity_count.get()).zip(entities) {
            if let Some(fault) = entity_fault(entity.semantic_type, type_node_count) {
                return Err(PrepareError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault,
                });
            }
        }
        for (ordinal, node) in (0..type_node_count.get()).zip(type_nodes.iter().copied()) {
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

    pub const fn output_len(&self) -> usize {
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
        written[..4].copy_from_slice(&crate::FRAGMENT_MAGIC);
        write_u16(written, 4, crate::FRAGMENT_SCHEMA);
        write_u16(written, 6, WRITTEN_SECTION_COUNT.get());
        write_u32(written, 8, self.layout.output_wire_len.get());
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
    let directory_bytes = WRITTEN_SECTION_COUNT
        .as_usize()
        .checked_mul(DIRECTORY_ENTRY_BYTES)
        .ok_or(PrepareError::LayoutOverflow {
            step: LayoutStep::Directory,
            entity_count: entity_count.get(),
            type_node_count: type_node_count.get(),
        })?;
    let directory_end =
        HEADER_BYTES
            .checked_add(directory_bytes)
            .ok_or(PrepareError::LayoutOverflow {
                step: LayoutStep::Directory,
                entity_count: entity_count.get(),
                type_node_count: type_node_count.get(),
            })?;
    let entity_bytes = entity_count
        .as_usize()
        .map_err(|source| PrepareError::OutputLength {
            actual: usize::MAX,
            source,
        })?
        .checked_mul(ENTITY_BYTES)
        .ok_or(PrepareError::LayoutOverflow {
            step: LayoutStep::EntityLane,
            entity_count: entity_count.get(),
            type_node_count: type_node_count.get(),
        })?;
    let entity_end =
        directory_end
            .checked_add(entity_bytes)
            .ok_or(PrepareError::LayoutOverflow {
                step: LayoutStep::EntityLane,
                entity_count: entity_count.get(),
                type_node_count: type_node_count.get(),
            })?;
    let type_node_bytes = type_node_count
        .as_usize()
        .map_err(|source| PrepareError::OutputLength {
            actual: usize::MAX,
            source,
        })?
        .checked_mul(TYPE_NODE_BYTES)
        .ok_or(PrepareError::LayoutOverflow {
            step: LayoutStep::TypeNodeLane,
            entity_count: entity_count.get(),
            type_node_count: type_node_count.get(),
        })?;
    let output_len =
        entity_end
            .checked_add(type_node_bytes)
            .ok_or(PrepareError::LayoutOverflow {
                step: LayoutStep::TypeNodeLane,
                entity_count: entity_count.get(),
                type_node_count: type_node_count.get(),
            })?;
    let output_wire_len =
        ByteLength::from_usize(output_len).map_err(|source| PrepareError::OutputLength {
            actual: output_len,
            source,
        })?;
    let entity_start =
        ByteOffset::from_usize(directory_end).map_err(|source| PrepareError::OutputLength {
            actual: directory_end,
            source,
        })?;
    let entity_length =
        ByteLength::from_usize(entity_bytes).map_err(|source| PrepareError::OutputLength {
            actual: entity_bytes,
            source,
        })?;
    let type_node_start =
        ByteOffset::from_usize(entity_end).map_err(|source| PrepareError::OutputLength {
            actual: entity_end,
            source,
        })?;
    let type_node_length =
        ByteLength::from_usize(type_node_bytes).map_err(|source| PrepareError::OutputLength {
            actual: type_node_bytes,
            source,
        })?;

    Ok(FragmentLayout {
        entities: LaneLayout {
            count: entity_count,
            start: entity_start,
            length: entity_length,
            start_index: directory_end,
            end_index: entity_end,
        },
        type_nodes: LaneLayout {
            count: type_node_count,
            start: type_node_start,
            length: type_node_length,
            start_index: entity_end,
            end_index: output_len,
        },
        output_len,
        output_wire_len,
    })
}

fn write_directory_entry(output: &mut [u8], ordinal: usize, kind: SectionKind, lane: LaneLayout) {
    let start = HEADER_BYTES + ordinal * DIRECTORY_ENTRY_BYTES;
    write_u16(output, start, kind.code());
    write_u16(output, start + 2, SectionRequirement::Required.code());
    write_u32(output, start + 4, lane.count.get());
    write_u32(output, start + 8, lane.start.get());
    write_u32(output, start + 12, lane.length.get());
}

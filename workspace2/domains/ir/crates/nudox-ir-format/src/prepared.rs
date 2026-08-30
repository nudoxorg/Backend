use nudox_ir_vocab::{EntityId, TypeId};

use crate::{
    TypeNode, TypeNodeFault,
    wire::{
        DIRECTORY_ENTRY_BYTES, ENTITY_BYTES, ENTITY_SECTION, FragmentLayout, HEADER_BYTES,
        LaneLayout, REQUIRED_SECTION, TYPE_NODE_BYTES, TYPE_NODE_SECTION, WRITTEN_SECTION_COUNT,
        type_node_fault, write_type_node, write_u16, write_u32,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrepareError {
    EntityCount {
        actual: usize,
    },
    TypeNodeCount {
        actual: usize,
    },
    LayoutOverflow {
        entity_count: u32,
        type_node_count: u32,
    },
    TypeNode {
        ordinal: TypeId,
        fault: TypeNodeFault,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WriteError {
    OutputTooSmall { required: usize, available: usize },
}

pub struct PreparedFragment<'facts> {
    entities: &'facts [EntityId],
    type_nodes: &'facts [TypeNode],
    layout: FragmentLayout,
}

impl<'facts> PreparedFragment<'facts> {
    pub fn prepare(
        entities: &'facts [EntityId],
        type_nodes: &'facts [TypeNode],
    ) -> Result<Self, PrepareError> {
        let entity_count = match u32::try_from(entities.len()) {
            Ok(count) => count,
            Err(_) => {
                return Err(PrepareError::EntityCount {
                    actual: entities.len(),
                });
            }
        };
        let type_node_count = match u32::try_from(type_nodes.len()) {
            Ok(count) => count,
            Err(_) => {
                return Err(PrepareError::TypeNodeCount {
                    actual: type_nodes.len(),
                });
            }
        };
        let layout = layout(entity_count, type_node_count)?;

        for (ordinal, node) in type_nodes.iter().copied().enumerate() {
            if let Some(fault) = type_node_fault(node, type_node_count) {
                return Err(PrepareError::TypeNode {
                    ordinal: TypeId::new(ordinal as u32),
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

    pub fn write_into(self, output: &mut [u8]) -> Result<&[u8], WriteError> {
        if output.len() < self.layout.output_len {
            return Err(WriteError::OutputTooSmall {
                required: self.layout.output_len,
                available: output.len(),
            });
        }

        let written = &mut output[..self.layout.output_len];
        written[..4].copy_from_slice(&crate::FRAGMENT_MAGIC);
        write_u16(written, 4, crate::FRAGMENT_SCHEMA);
        write_u16(written, 6, WRITTEN_SECTION_COUNT);
        write_u32(written, 8, self.layout.output_len as u32);
        write_directory_entry(written, 0, ENTITY_SECTION, self.layout.entities);
        write_directory_entry(written, 1, TYPE_NODE_SECTION, self.layout.type_nodes);

        let mut entity_cursor = self.layout.entities.start;
        for entity in self.entities {
            write_u32(written, entity_cursor, entity.raw);
            entity_cursor += ENTITY_BYTES;
        }
        let mut type_cursor = self.layout.type_nodes.start;
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

fn layout(entity_count: u32, type_node_count: u32) -> Result<FragmentLayout, PrepareError> {
    let directory_end = HEADER_BYTES + usize::from(WRITTEN_SECTION_COUNT) * DIRECTORY_ENTRY_BYTES;
    let entity_bytes = (entity_count as usize).checked_mul(ENTITY_BYTES);
    let type_node_bytes = (type_node_count as usize).checked_mul(TYPE_NODE_BYTES);
    let Some(entity_end) = entity_bytes.and_then(|bytes| directory_end.checked_add(bytes)) else {
        return Err(PrepareError::LayoutOverflow {
            entity_count,
            type_node_count,
        });
    };
    let Some(output_len) = type_node_bytes.and_then(|bytes| entity_end.checked_add(bytes)) else {
        return Err(PrepareError::LayoutOverflow {
            entity_count,
            type_node_count,
        });
    };
    if u32::try_from(output_len).is_err() {
        return Err(PrepareError::LayoutOverflow {
            entity_count,
            type_node_count,
        });
    }
    Ok(FragmentLayout {
        entities: LaneLayout {
            count: entity_count,
            start: directory_end,
            end: entity_end,
        },
        type_nodes: LaneLayout {
            count: type_node_count,
            start: entity_end,
            end: output_len,
        },
        output_len,
    })
}

fn write_directory_entry(output: &mut [u8], ordinal: usize, kind: u16, lane: LaneLayout) {
    let start = HEADER_BYTES + ordinal * DIRECTORY_ENTRY_BYTES;
    write_u16(output, start, kind);
    write_u16(output, start + 2, REQUIRED_SECTION);
    write_u32(output, start + 4, lane.count);
    write_u32(output, start + 8, lane.start as u32);
    write_u32(output, start + 12, (lane.end - lane.start) as u32);
}

use core::num::TryFromIntError;

use nudox_ir_vocab::{AtomId, EntityId, TypeId};
use thiserror::Error;

use crate::{
    AtomInput, EntityRecord, EntityRecordFault, RecipeFact, SourceIdentity, TypeNode,
    TypeNodeFault,
    wire::{
        ATOM_RECORD_BYTES, ByteLength, ByteOffset, DIRECTORY_ENTRY_LAYOUT, ENTITY_BYTES,
        FragmentLayout, HEADER_LAYOUT, ItemCount, LaneLayout, RECIPE_FACT_BYTES,
        SOURCE_IDENTITY_BYTES, SectionKind, SectionRequirement, TYPE_NODE_BYTES,
        WRITTEN_SECTION_COUNT, entity_fault, entity_name_fault, type_node_fault,
        write_atom_record, write_entity, write_recipe_fact, write_source_identity, write_type_node,
        write_u16, write_u32,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LayoutStep {
    Directory,
    EntityLane,
    TypeNodeLane,
    AtomRecordLane,
    AtomByteLane,
    SourceIdentityLane,
    RecipeFactLane,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PrepareError {
    #[error("{lane:?} count {actual} exceeds the fragment count width")]
    Count {
        lane: LayoutStep,
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error("atom byte pool overflowed while adding atom {ordinal:?}")]
    AtomBytePoolOverflow { ordinal: AtomId },
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
        fault: EntityRecordFault,
    },
    #[error("type node {ordinal:?} is invalid: {fault}")]
    TypeNode {
        ordinal: TypeId,
        #[source]
        fault: TypeNodeFault,
    },
}

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("fragment output needs {required} bytes but only {available} are available")]
    OutputTooSmall { required: usize, available: usize },
    #[error("prepared atom {ordinal:?} no longer fits the compact byte width")]
    AtomLength {
        ordinal: AtomId,
        actual: usize,
        #[source]
        source: TryFromIntError,
    },
    #[error("prepared atom {ordinal:?} no longer fits its prepared byte region")]
    AtomExtent { ordinal: AtomId },
}

pub struct PreparedFragment<'facts> {
    source: SourceIdentity,
    recipe: RecipeFact,
    entities: &'facts [EntityRecord],
    type_nodes: &'facts [TypeNode],
    atoms: &'facts [AtomInput<'facts>],
    layout: FragmentLayout,
}

impl<'facts> PreparedFragment<'facts> {
    pub fn prepare(
        source: SourceIdentity,
        recipe: RecipeFact,
        entities: &'facts [EntityRecord],
        type_nodes: &'facts [TypeNode],
        atoms: &'facts [AtomInput<'facts>],
    ) -> Result<Self, PrepareError> {
        let entity_count = count(LayoutStep::EntityLane, entities.len())?;
        let type_node_count = count(LayoutStep::TypeNodeLane, type_nodes.len())?;
        let atom_count = count(LayoutStep::AtomRecordLane, atoms.len())?;
        let atom_byte_count = atom_byte_count(atoms)?;
        let layout = layout(
            source,
            recipe,
            entity_count,
            type_node_count,
            atom_count,
            atom_byte_count,
        )?;

        for (ordinal, entity) in (0..u32::from(entity_count)).zip(entities) {
            if let Some(fault) = entity_fault(entity.semantic_type, type_node_count) {
                return Err(PrepareError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault: fault.into(),
                });
            }
            if let Some(fault) = entity_name_fault(entity.name, atom_count) {
                return Err(PrepareError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault: fault.into(),
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
            source,
            recipe,
            entities,
            type_nodes,
            atoms,
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
        write_directory_entry(written, 2, SectionKind::AtomRecords, self.layout.atoms);
        write_directory_entry(written, 3, SectionKind::AtomBytes, self.layout.atom_bytes);
        write_directory_entry(
            written,
            4,
            SectionKind::SourceIdentity,
            self.layout.source_identity,
        );
        write_directory_entry(written, 5, SectionKind::RecipeFact, self.layout.recipe_fact);

        let mut entity_cursor = self.layout.entities.start_index;
        for entity in self.entities {
            write_entity(
                &mut written[entity_cursor..entity_cursor + ENTITY_BYTES],
                *entity,
            );
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
        write_atoms(written, self.layout, self.atoms)?;
        write_source_identity(
            &mut written[self.layout.source_identity.range()],
            self.source,
        );
        write_recipe_fact(&mut written[self.layout.recipe_fact.range()], self.recipe);
        Ok(written)
    }
}

fn count(step: LayoutStep, actual: usize) -> Result<ItemCount, PrepareError> {
    ItemCount::try_from(actual).map_err(|source| PrepareError::Count {
        lane: step,
        actual,
        source,
    })
}

fn atom_byte_count(atoms: &[AtomInput<'_>]) -> Result<ItemCount, PrepareError> {
    let mut total = 0_usize;
    for (ordinal, atom) in (0..u32::MAX).zip(atoms) {
        total = total
            .checked_add(atom.bytes.len())
            .ok_or(PrepareError::AtomBytePoolOverflow {
                ordinal: AtomId::new(ordinal),
            })?;
    }
    count(LayoutStep::AtomByteLane, total)
}

fn layout(
    source: SourceIdentity,
    recipe: RecipeFact,
    entity_count: ItemCount,
    type_node_count: ItemCount,
    atom_count: ItemCount,
    atom_byte_count: ItemCount,
) -> Result<FragmentLayout, PrepareError> {
    let mut cursor = LayoutCursor::new(entity_count, type_node_count)?;
    let entities = cursor.lane(LayoutStep::EntityLane, entity_count, ENTITY_BYTES)?;
    let type_nodes = cursor.lane(LayoutStep::TypeNodeLane, type_node_count, TYPE_NODE_BYTES)?;
    let atoms = cursor.lane(LayoutStep::AtomRecordLane, atom_count, ATOM_RECORD_BYTES)?;
    let atom_bytes = cursor.lane(LayoutStep::AtomByteLane, atom_byte_count, 1)?;
    let source_identity = cursor.lane(
        LayoutStep::SourceIdentityLane,
        ItemCount::from(1),
        SOURCE_IDENTITY_BYTES,
    )?;
    let recipe_fact = cursor.lane(
        LayoutStep::RecipeFactLane,
        ItemCount::from(1),
        RECIPE_FACT_BYTES,
    )?;
    cursor.finish(
        FragmentFacts { source, recipe },
        FragmentLanes {
            entities,
            type_nodes,
            atoms,
            atom_bytes,
            source_identity,
            recipe_fact,
        },
    )
}

#[derive(Clone, Copy)]
struct FragmentFacts {
    source: SourceIdentity,
    recipe: RecipeFact,
}

#[derive(Clone, Copy)]
struct FragmentLanes {
    entities: LaneLayout,
    type_nodes: LaneLayout,
    atoms: LaneLayout,
    atom_bytes: LaneLayout,
    source_identity: LaneLayout,
    recipe_fact: LaneLayout,
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
        facts: FragmentFacts,
        lanes: FragmentLanes,
    ) -> Result<FragmentLayout, PrepareError> {
        let output_wire_len =
            ByteLength::try_from(self.next_index).map_err(|source| PrepareError::OutputLength {
                actual: self.next_index,
                source,
            })?;
        Ok(FragmentLayout {
            entities: lanes.entities,
            type_nodes: lanes.type_nodes,
            atoms: lanes.atoms,
            atom_bytes: lanes.atom_bytes,
            source_identity: lanes.source_identity,
            recipe_fact: lanes.recipe_fact,
            source: facts.source,
            recipe: facts.recipe,
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

fn write_atoms(
    output: &mut [u8],
    layout: FragmentLayout,
    atoms: &[AtomInput<'_>],
) -> Result<(), WriteError> {
    let mut record_cursor = layout.atoms.start_index;
    let mut bytes_cursor = layout.atom_bytes.start_index;
    let mut relative_start = 0_u32;
    for (ordinal, atom) in (0..u32::MAX).zip(atoms) {
        let atom_length =
            u32::try_from(atom.bytes.len()).map_err(|source| WriteError::AtomLength {
                ordinal: AtomId::new(ordinal),
                actual: atom.bytes.len(),
                source,
            })?;
        let bytes_end = bytes_cursor
            .checked_add(atom.bytes.len())
            .filter(|end| *end <= layout.atom_bytes.end_index)
            .ok_or(WriteError::AtomExtent {
                ordinal: AtomId::new(ordinal),
            })?;
        write_atom_record(
            &mut output[record_cursor..record_cursor + ATOM_RECORD_BYTES],
            relative_start,
            atom_length,
        );
        output[bytes_cursor..bytes_end].copy_from_slice(atom.bytes);
        record_cursor += ATOM_RECORD_BYTES;
        bytes_cursor = bytes_end;
        relative_start = relative_start
            .checked_add(atom_length)
            .ok_or(WriteError::AtomExtent {
                ordinal: AtomId::new(ordinal),
            })?;
    }
    Ok(())
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

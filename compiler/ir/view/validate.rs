//! Defines view validate behavior for `compiler-ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the view validate invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::{AtomId, EntityId, TypeId};

use crate::{
    view::{
        DirectoryFault, FragmentError, FragmentView, WireField,
        directory::{DirectoryState, ParsedDirectoryEntry, directory_fault},
    },
    wire::{
        ATOM_RECORD_BYTES, ByteLength, DIRECTORY_ENTRY_LAYOUT, ENTITY_BYTES, FragmentLayout,
        HEADER_LAYOUT, RECIPE_FACT_BYTES, SOURCE_IDENTITY_BYTES, SectionCount, SectionKind,
        SectionRequirement, TYPE_NODE_BYTES, atom_fault, decode_entity, decode_recipe_fact,
        decode_source_identity, decode_type_node, entity_fault, entity_name_fault, read_u16,
        read_u32, type_node_fault,
    },
};

impl<'fragment> FragmentView<'fragment> {
    pub fn validate(envelope: &'fragment [u8]) -> Result<Self, FragmentError> {
        let layout = validate_fragment_layout(envelope)?;
        Ok(Self::from_validated_layout(envelope, layout))
    }

    pub(crate) fn from_validated_layout(envelope: &'fragment [u8], layout: FragmentLayout) -> Self {
        let entity_lane = &envelope[layout.entities.range()];
        let atom_lane = &envelope[layout.atoms.range()];
        Self {
            envelope,
            entities: entity_lane,
            type_nodes: &envelope[layout.type_nodes.range()],
            atoms: atom_lane,
            atom_bytes: &envelope[layout.atom_bytes.range()],
            source: layout.source,
            recipe: layout.recipe,
            layout,
        }
    }
}

pub(crate) fn validate_fragment_layout(envelope: &[u8]) -> Result<FragmentLayout, FragmentError> {
    let layout = validate_layout(envelope)?;
    {
        let entity_lane = &envelope[layout.entities.range()];
        for (ordinal, record) in
            (0..u32::from(layout.entities.count)).zip(entity_lane.chunks_exact(ENTITY_BYTES))
        {
            let entity = match decode_entity(record) {
                Ok(entity) => entity,
                Err(fault) => {
                    return Err(FragmentError::EntityRecord {
                        ordinal: EntityId::new(ordinal),
                        fault,
                    });
                }
            };
            if let Some(fault) = entity_fault(entity.semantic_type, layout.type_nodes.count) {
                return Err(FragmentError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault,
                });
            }
            if let Some(fault) = entity_name_fault(entity.name, layout.atoms.count) {
                return Err(FragmentError::EntityRecord {
                    ordinal: EntityId::new(ordinal),
                    fault: fault.into(),
                });
            }
        }
    }
    {
        let atom_lane = &envelope[layout.atoms.range()];
        for (ordinal, record) in
            (0..u32::from(layout.atoms.count)).zip(atom_lane.chunks_exact(ATOM_RECORD_BYTES))
        {
            let ordinal = AtomId::new(ordinal);
            if let Some(fault) = atom_fault(ordinal, record, layout.atom_bytes.count) {
                return Err(FragmentError::Atom { fault });
            }
            let start = read_u32(record, 0);
            usize::try_from(start).map_err(|source| FragmentError::WireWidth {
                field: WireField::AtomStart { ordinal },
                actual: start,
                source,
            })?;
            let length = read_u32(record, size_of::<u32>());
            usize::try_from(length).map_err(|source| FragmentError::WireWidth {
                field: WireField::AtomLength { ordinal },
                actual: length,
                source,
            })?;
        }
    }
    {
        let type_lane = &envelope[layout.type_nodes.range()];
        for (ordinal, record) in
            (0..u32::from(layout.type_nodes.count)).zip(type_lane.chunks_exact(TYPE_NODE_BYTES))
        {
            let node = match decode_type_node(record) {
                Ok(node) => node,
                Err(fault) => {
                    return Err(FragmentError::TypeNode {
                        ordinal: TypeId::new(ordinal),
                        fault,
                    });
                }
            };
            if let Some(fault) = type_node_fault(node, layout.type_nodes.count) {
                return Err(FragmentError::TypeNode {
                    ordinal: TypeId::new(ordinal),
                    fault,
                });
            }
        }
    }
    Ok(layout)
}

fn validate_layout(envelope: &[u8]) -> Result<FragmentLayout, FragmentError> {
    if envelope.len() < HEADER_LAYOUT.encoded_len {
        return Err(FragmentError::TruncatedHeader {
            required: HEADER_LAYOUT.encoded_len,
            actual: envelope.len(),
        });
    }
    let mut actual_magic = [0; size_of::<[u8; 4]>()];
    actual_magic.copy_from_slice(&envelope[HEADER_LAYOUT.magic..HEADER_LAYOUT.schema]);
    if actual_magic != crate::FRAGMENT_MAGIC {
        return Err(FragmentError::Magic {
            actual: actual_magic,
        });
    }
    let schema = read_u16(envelope, HEADER_LAYOUT.schema);
    if schema != crate::FRAGMENT_SCHEMA {
        return Err(FragmentError::Schema { actual: schema });
    }
    let section_count = SectionCount::from(read_u16(envelope, HEADER_LAYOUT.section_count));
    let declared_wire_length = ByteLength::from(read_u32(envelope, HEADER_LAYOUT.declared_length));
    let declared_length =
        usize::try_from(declared_wire_length).map_err(|source| FragmentError::WireWidth {
            field: WireField::DeclaredLength,
            actual: u32::from(declared_wire_length),
            source,
        })?;
    if declared_length != envelope.len() {
        return Err(FragmentError::DeclaredLength {
            declared: declared_length,
            actual: envelope.len(),
        });
    }
    let directory_bytes = usize::from(section_count)
        .checked_mul(DIRECTORY_ENTRY_LAYOUT.encoded_len)
        .ok_or(FragmentError::Extent {
            required: usize::MAX,
            actual: envelope.len(),
        })?;
    let directory_end = HEADER_LAYOUT
        .encoded_len
        .checked_add(directory_bytes)
        .ok_or(FragmentError::Extent {
            required: usize::MAX,
            actual: envelope.len(),
        })?;
    if directory_end > envelope.len() {
        return Err(FragmentError::Extent {
            required: directory_end,
            actual: envelope.len(),
        });
    }

    let mut state = DirectoryState::first(directory_end);
    let mut entities = None;
    let mut type_nodes = None;
    let mut source_identity = None;
    let mut atoms = None;
    let mut atom_bytes = None;
    let mut recipe_fact = None;
    for _ in 0..u16::from(section_count) {
        let (next_state, entry) = state.parse(envelope)?;
        match SectionKind::try_from(entry.kind) {
            Ok(SectionKind::EntityTypes) => {
                validate_known_length(&entry, ENTITY_BYTES)?;
                require_known(&entry)?;
                entities = Some(entry.lane);
            }
            Ok(SectionKind::TypeNodes) => {
                validate_known_length(&entry, TYPE_NODE_BYTES)?;
                require_known(&entry)?;
                type_nodes = Some(entry.lane);
            }
            Ok(SectionKind::SourceIdentity) => {
                validate_known_length(&entry, SOURCE_IDENTITY_BYTES)?;
                require_known(&entry)?;
                if u32::from(entry.lane.count) != 1 {
                    return Err(directory_fault(
                        entry.ordinal,
                        DirectoryFault::Count {
                            kind: entry.kind,
                            expected: 1,
                            actual: u32::from(entry.lane.count),
                        },
                    ));
                }
                source_identity = Some(entry.lane);
            }
            Ok(SectionKind::AtomRecords) => {
                validate_known_length(&entry, ATOM_RECORD_BYTES)?;
                require_known(&entry)?;
                atoms = Some(entry.lane);
            }
            Ok(SectionKind::AtomBytes) => {
                validate_known_length(&entry, 1)?;
                require_known(&entry)?;
                atom_bytes = Some(entry.lane);
            }
            Ok(SectionKind::RecipeFact) => {
                validate_known_length(&entry, RECIPE_FACT_BYTES)?;
                require_known(&entry)?;
                if u32::from(entry.lane.count) != 1 {
                    return Err(directory_fault(
                        entry.ordinal,
                        DirectoryFault::Count {
                            kind: entry.kind,
                            expected: 1,
                            actual: u32::from(entry.lane.count),
                        },
                    ));
                }
                recipe_fact = Some(entry.lane);
            }
            Err(kind) if entry.requirement == SectionRequirement::Required => {
                return Err(directory_fault(
                    entry.ordinal,
                    DirectoryFault::RequiredUnknown { kind },
                ));
            }
            Err(_) => {}
        }
        state = next_state;
    }
    if state.expected_offset != envelope.len() {
        return Err(FragmentError::Extent {
            required: state.expected_offset,
            actual: envelope.len(),
        });
    }
    let entities = entities.ok_or(FragmentError::MissingSection {
        section: SectionKind::EntityTypes,
    })?;
    let type_nodes = type_nodes.ok_or(FragmentError::MissingSection {
        section: SectionKind::TypeNodes,
    })?;
    let source_identity = source_identity.ok_or(FragmentError::MissingSection {
        section: SectionKind::SourceIdentity,
    })?;
    let atoms = atoms.ok_or(FragmentError::MissingSection {
        section: SectionKind::AtomRecords,
    })?;
    let atom_bytes = atom_bytes.ok_or(FragmentError::MissingSection {
        section: SectionKind::AtomBytes,
    })?;
    let recipe_fact = recipe_fact.ok_or(FragmentError::MissingSection {
        section: SectionKind::RecipeFact,
    })?;
    let source = decode_source_identity(&envelope[source_identity.range()])
        .map_err(|fault| FragmentError::SourceIdentity { fault })?;
    let recipe = decode_recipe_fact(&envelope[recipe_fact.range()])
        .map_err(|fault| FragmentError::RecipeFact { fault })?;
    let expected_recipe = compiler_vocabulary::CompileRecipeFact::derive(
        recipe.language,
        recipe.stage,
        recipe.tool,
        source.identity,
        recipe.toolchain,
    );
    if recipe.identity != expected_recipe.identity {
        return Err(FragmentError::RecipeFact {
            fault: crate::RecipeFactFault::IdentityRelation {
                expected: expected_recipe.identity,
                observed: recipe.identity,
            },
        });
    }
    Ok(FragmentLayout {
        entities,
        type_nodes,
        atoms,
        atom_bytes,
        source_identity,
        recipe_fact,
        source,
        recipe,
        output_len: envelope.len(),
        output_wire_len: declared_wire_length,
    })
}

fn require_known(entry: &ParsedDirectoryEntry) -> Result<(), FragmentError> {
    if entry.requirement != SectionRequirement::Required {
        return Err(directory_fault(
            entry.ordinal,
            DirectoryFault::Flags {
                kind: entry.kind,
                actual: u16::from(entry.requirement),
            },
        ));
    }
    Ok(())
}

fn validate_known_length(entry: &ParsedDirectoryEntry, width: usize) -> Result<(), FragmentError> {
    let count_index =
        usize::try_from(entry.lane.count).map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionItemCount {
                ordinal: entry.ordinal,
            },
            actual: u32::from(entry.lane.count),
            source,
        })?;
    let expected = count_index.checked_mul(width).ok_or_else(|| {
        directory_fault(
            entry.ordinal,
            DirectoryFault::CountWidthOverflow {
                kind: entry.kind,
                count: u32::from(entry.lane.count),
                width,
            },
        )
    })?;
    let actual_index =
        usize::try_from(entry.lane.length).map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionByteLength {
                ordinal: entry.ordinal,
            },
            actual: u32::from(entry.lane.length),
            source,
        })?;
    if expected != actual_index {
        return Err(directory_fault(
            entry.ordinal,
            DirectoryFault::ByteLength {
                kind: entry.kind,
                expected,
                actual: u32::from(entry.lane.length),
            },
        ));
    }
    Ok(())
}

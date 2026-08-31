use nudox_ir_vocab::{EntityId, TypeId};

use crate::{
    view::{
        DirectoryFault, FragmentError, FragmentView, WireField,
        directory::{DirectoryState, ParsedDirectoryEntry, directory_fault},
    },
    wire::{
        ByteLength, DIRECTORY_ENTRY_LAYOUT, ENTITY_BYTES, FragmentLayout, HEADER_LAYOUT,
        SectionCount, SectionKind, SectionRequirement, TYPE_NODE_BYTES, decode_type_node,
        entity_fault, read_u16, read_u32, type_node_fault,
    },
};

impl<'fragment> FragmentView<'fragment> {
    pub fn validate(envelope: &'fragment [u8]) -> Result<Self, FragmentError> {
        let layout = validate_layout(envelope)?;
        let entity_lane = &envelope[layout.entities.range()];
        for (ordinal, record) in
            (0..u32::from(layout.entities.count)).zip(entity_lane.chunks_exact(ENTITY_BYTES))
        {
            let target = TypeId::new(read_u32(record, 0));
            if let Some(fault) = entity_fault(target, layout.type_nodes.count) {
                return Err(FragmentError::Entity {
                    ordinal: EntityId::new(ordinal),
                    fault,
                });
            }
        }

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
        Ok(Self {
            envelope,
            entities: entity_lane,
            type_nodes: type_lane,
        })
    }
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
    Ok(FragmentLayout {
        entities,
        type_nodes,
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

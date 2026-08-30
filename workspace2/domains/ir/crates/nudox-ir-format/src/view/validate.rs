use nudox_ir_vocab::{EntityId, TypeId};

use crate::{
    view::{DirectoryFault, FragmentError, FragmentView, WireField},
    wire::{
        ByteLength, ByteOffset, DIRECTORY_ENTRY_BYTES, ENTITY_BYTES, FragmentLayout, HEADER_BYTES,
        ItemCount, LaneLayout, SectionCount, SectionKind, SectionRequirement, TYPE_NODE_BYTES,
        decode_type_node, entity_fault, read_u16, read_u32, type_node_fault,
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
    if envelope.len() < HEADER_BYTES {
        return Err(FragmentError::TruncatedHeader {
            required: HEADER_BYTES,
            actual: envelope.len(),
        });
    }
    let actual_magic = [envelope[0], envelope[1], envelope[2], envelope[3]];
    if actual_magic != crate::FRAGMENT_MAGIC {
        return Err(FragmentError::Magic {
            actual: actual_magic,
        });
    }
    let schema = read_u16(envelope, 4);
    if schema != crate::FRAGMENT_SCHEMA {
        return Err(FragmentError::Schema { actual: schema });
    }
    let section_count = SectionCount::from(read_u16(envelope, 6));
    let declared_wire_length = ByteLength::from(read_u32(envelope, 8));
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
        .checked_mul(DIRECTORY_ENTRY_BYTES)
        .ok_or(FragmentError::Extent {
            required: usize::MAX,
            actual: envelope.len(),
        })?;
    let directory_end = HEADER_BYTES
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

    let mut previous = None;
    let mut expected_offset = directory_end;
    let mut entities = None;
    let mut type_nodes = None;
    for ordinal in 0..u16::from(section_count) {
        let entry = HEADER_BYTES + usize::from(ordinal) * DIRECTORY_ENTRY_BYTES;
        let raw_kind = read_u16(envelope, entry);
        let raw_requirement = read_u16(envelope, entry + 2);
        let count = ItemCount::from(read_u32(envelope, entry + 4));
        let start = ByteOffset::from(read_u32(envelope, entry + 8));
        let byte_len = ByteLength::from(read_u32(envelope, entry + 12));
        if let Some(previous_kind) = previous
            && raw_kind <= previous_kind
        {
            return Err(FragmentError::Directory {
                ordinal,
                fault: DirectoryFault::Order {
                    previous: previous_kind,
                    actual: raw_kind,
                },
            });
        }
        previous = Some(raw_kind);
        let requirement = SectionRequirement::try_from(raw_requirement).map_err(|actual| {
            FragmentError::Directory {
                ordinal,
                fault: DirectoryFault::Flags {
                    kind: raw_kind,
                    actual,
                },
            }
        })?;
        let start_index = usize::try_from(start).map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionOffset { ordinal },
            actual: u32::from(start),
            source,
        })?;
        if start_index != expected_offset {
            return Err(FragmentError::Directory {
                ordinal,
                fault: DirectoryFault::Offset {
                    kind: raw_kind,
                    expected: expected_offset,
                    actual: u32::from(start),
                },
            });
        }
        let byte_len_index =
            usize::try_from(byte_len).map_err(|source| FragmentError::WireWidth {
                field: WireField::SectionByteLength { ordinal },
                actual: u32::from(byte_len),
                source,
            })?;
        let end_index =
            start_index
                .checked_add(byte_len_index)
                .ok_or(FragmentError::Directory {
                    ordinal,
                    fault: DirectoryFault::RangeOverflow {
                        kind: raw_kind,
                        start: u32::from(start),
                        length: u32::from(byte_len),
                    },
                })?;
        if end_index > envelope.len() {
            return Err(FragmentError::Extent {
                required: end_index,
                actual: envelope.len(),
            });
        }
        let lane = LaneLayout {
            count,
            start,
            length: byte_len,
            start_index,
            end_index,
        };
        match SectionKind::try_from(raw_kind) {
            Ok(SectionKind::EntityTypes) => {
                validate_known_length(ordinal, raw_kind, count, ENTITY_BYTES, byte_len)?;
                require_known(ordinal, raw_kind, requirement)?;
                entities = Some(lane);
            }
            Ok(SectionKind::TypeNodes) => {
                validate_known_length(ordinal, raw_kind, count, TYPE_NODE_BYTES, byte_len)?;
                require_known(ordinal, raw_kind, requirement)?;
                type_nodes = Some(lane);
            }
            Err(kind) if requirement == SectionRequirement::Required => {
                return Err(FragmentError::Directory {
                    ordinal,
                    fault: DirectoryFault::RequiredUnknown { kind },
                });
            }
            Err(_) => {}
        }
        expected_offset = end_index;
    }
    if expected_offset != envelope.len() {
        return Err(FragmentError::Extent {
            required: expected_offset,
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

fn require_known(
    ordinal: u16,
    kind: u16,
    requirement: SectionRequirement,
) -> Result<(), FragmentError> {
    if requirement != SectionRequirement::Required {
        return Err(FragmentError::Directory {
            ordinal,
            fault: DirectoryFault::Flags {
                kind,
                actual: u16::from(requirement),
            },
        });
    }
    Ok(())
}

fn validate_known_length(
    ordinal: u16,
    kind: u16,
    count: ItemCount,
    width: usize,
    actual: ByteLength,
) -> Result<(), FragmentError> {
    let count_index = usize::try_from(count).map_err(|source| FragmentError::WireWidth {
        field: WireField::SectionItemCount { ordinal },
        actual: u32::from(count),
        source,
    })?;
    let expected = count_index
        .checked_mul(width)
        .ok_or(FragmentError::Directory {
            ordinal,
            fault: DirectoryFault::CountWidthOverflow {
                kind,
                count: u32::from(count),
                width,
            },
        })?;
    let actual_index = usize::try_from(actual).map_err(|source| FragmentError::WireWidth {
        field: WireField::SectionByteLength { ordinal },
        actual: u32::from(actual),
        source,
    })?;
    if expected != actual_index {
        return Err(FragmentError::Directory {
            ordinal,
            fault: DirectoryFault::ByteLength {
                kind,
                expected,
                actual: u32::from(actual),
            },
        });
    }
    Ok(())
}

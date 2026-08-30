use nudox_ir_vocab::{EntityId, TypeId};

use crate::{
    TypeNode, TypeNodeFault,
    wire::{
        DIRECTORY_ENTRY_BYTES, ENTITY_BYTES, ENTITY_SECTION, FragmentLayout, HEADER_BYTES,
        LaneLayout, REQUIRED_SECTION, TYPE_NODE_BYTES, TYPE_NODE_SECTION, decode_type_node,
        decode_validated_type_node, read_u16, read_u32, type_node_fault,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KnownSection {
    EntityIds,
    TypeNodes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectoryFault {
    Order {
        previous: u16,
        actual: u16,
    },
    Flags {
        kind: u16,
        actual: u16,
    },
    RequiredUnknown {
        kind: u16,
    },
    Offset {
        kind: u16,
        expected: usize,
        actual: u32,
    },
    ByteLength {
        kind: u16,
        expected: usize,
        actual: u32,
    },
    CountWidthOverflow {
        kind: u16,
        count: u32,
        width: usize,
    },
    RangeOverflow {
        kind: u16,
        start: u32,
        length: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FragmentError {
    TruncatedHeader {
        required: usize,
        actual: usize,
    },
    Magic {
        actual: [u8; 4],
    },
    Schema {
        actual: u16,
    },
    Geometry {
        expected: usize,
        actual: usize,
    },
    DirectoryBytesOverflow {
        section_count: u16,
    },
    Directory {
        ordinal: u16,
        fault: DirectoryFault,
    },
    MissingSection {
        section: KnownSection,
    },
    TypeNode {
        ordinal: TypeId,
        fault: TypeNodeFault,
    },
}

pub struct FragmentView<'fragment> {
    envelope: &'fragment [u8],
    entities: &'fragment [u8],
    type_nodes: &'fragment [u8],
}

impl<'fragment> FragmentView<'fragment> {
    pub fn validate(envelope: &'fragment [u8]) -> Result<Self, FragmentError> {
        let layout = validate_layout(envelope)?;
        let type_lane = &envelope[layout.type_nodes.range()];
        for (ordinal, record) in type_lane.chunks_exact(TYPE_NODE_BYTES).enumerate() {
            let node = match decode_type_node(record) {
                Ok(node) => node,
                Err(fault) => {
                    return Err(FragmentError::TypeNode {
                        ordinal: TypeId::new(ordinal as u32),
                        fault,
                    });
                }
            };
            if let Some(fault) = type_node_fault(node, layout.type_nodes.count) {
                return Err(FragmentError::TypeNode {
                    ordinal: TypeId::new(ordinal as u32),
                    fault,
                });
            }
        }
        Ok(Self {
            envelope,
            entities: &envelope[layout.entities.range()],
            type_nodes: type_lane,
        })
    }

    pub fn entity_ids(&self) -> EntityCursor<'fragment> {
        EntityCursor {
            remaining: self.entities,
        }
    }

    pub fn type_nodes(&self) -> TypeNodeCursor<'fragment> {
        TypeNodeCursor {
            remaining: self.type_nodes,
        }
    }
}

impl AsRef<[u8]> for FragmentView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.envelope
    }
}

pub struct EntityCursor<'fragment> {
    remaining: &'fragment [u8],
}

impl Iterator for EntityCursor<'_> {
    type Item = EntityId;

    fn next(&mut self) -> Option<Self::Item> {
        let (record, remaining) = self.remaining.split_first_chunk::<ENTITY_BYTES>()?;
        self.remaining = remaining;
        Some(EntityId::new(u32::from_le_bytes(*record)))
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
    let section_count = read_u16(envelope, 6);
    let total_len = read_u32(envelope, 8) as usize;
    if total_len != envelope.len() {
        return Err(FragmentError::Geometry {
            expected: total_len,
            actual: envelope.len(),
        });
    }
    let Some(directory_bytes) = usize::from(section_count).checked_mul(DIRECTORY_ENTRY_BYTES)
    else {
        return Err(FragmentError::DirectoryBytesOverflow { section_count });
    };
    let Some(directory_end) = HEADER_BYTES.checked_add(directory_bytes) else {
        return Err(FragmentError::DirectoryBytesOverflow { section_count });
    };
    if directory_end > envelope.len() {
        return Err(FragmentError::Geometry {
            expected: directory_end,
            actual: envelope.len(),
        });
    }

    let mut previous = None;
    let mut expected_offset = directory_end;
    let mut entities = None;
    let mut type_nodes = None;
    for ordinal in 0..section_count {
        let entry = HEADER_BYTES + usize::from(ordinal) * DIRECTORY_ENTRY_BYTES;
        let kind = read_u16(envelope, entry);
        let flags = read_u16(envelope, entry + 2);
        let count = read_u32(envelope, entry + 4);
        let start = read_u32(envelope, entry + 8);
        let byte_len = read_u32(envelope, entry + 12);
        if let Some(previous_kind) = previous
            && kind <= previous_kind
        {
            return Err(FragmentError::Directory {
                ordinal,
                fault: DirectoryFault::Order {
                    previous: previous_kind,
                    actual: kind,
                },
            });
        }
        previous = Some(kind);
        if flags & !REQUIRED_SECTION != 0 {
            return Err(FragmentError::Directory {
                ordinal,
                fault: DirectoryFault::Flags {
                    kind,
                    actual: flags,
                },
            });
        }
        if start as usize != expected_offset {
            return Err(FragmentError::Directory {
                ordinal,
                fault: DirectoryFault::Offset {
                    kind,
                    expected: expected_offset,
                    actual: start,
                },
            });
        }
        let Some(end) = (start as usize).checked_add(byte_len as usize) else {
            return Err(FragmentError::Directory {
                ordinal,
                fault: DirectoryFault::RangeOverflow {
                    kind,
                    start,
                    length: byte_len,
                },
            });
        };
        if end > envelope.len() {
            return Err(FragmentError::Geometry {
                expected: end,
                actual: envelope.len(),
            });
        }
        let lane = LaneLayout {
            count,
            start: expected_offset,
            end,
        };
        match kind {
            ENTITY_SECTION => {
                validate_known_length(ordinal, kind, count, ENTITY_BYTES, byte_len)?;
                if flags != REQUIRED_SECTION {
                    return Err(FragmentError::Directory {
                        ordinal,
                        fault: DirectoryFault::Flags {
                            kind,
                            actual: flags,
                        },
                    });
                }
                entities = Some(lane);
            }
            TYPE_NODE_SECTION => {
                validate_known_length(ordinal, kind, count, TYPE_NODE_BYTES, byte_len)?;
                if flags != REQUIRED_SECTION {
                    return Err(FragmentError::Directory {
                        ordinal,
                        fault: DirectoryFault::Flags {
                            kind,
                            actual: flags,
                        },
                    });
                }
                type_nodes = Some(lane);
            }
            _ if flags == REQUIRED_SECTION => {
                return Err(FragmentError::Directory {
                    ordinal,
                    fault: DirectoryFault::RequiredUnknown { kind },
                });
            }
            _ => {}
        }
        expected_offset = end;
    }
    if expected_offset != envelope.len() {
        return Err(FragmentError::Geometry {
            expected: expected_offset,
            actual: envelope.len(),
        });
    }
    let entities = entities.ok_or(FragmentError::MissingSection {
        section: KnownSection::EntityIds,
    })?;
    let type_nodes = type_nodes.ok_or(FragmentError::MissingSection {
        section: KnownSection::TypeNodes,
    })?;
    Ok(FragmentLayout {
        entities,
        type_nodes,
        output_len: envelope.len(),
    })
}

fn validate_known_length(
    ordinal: u16,
    kind: u16,
    count: u32,
    width: usize,
    actual: u32,
) -> Result<(), FragmentError> {
    let Some(expected) = (count as usize).checked_mul(width) else {
        return Err(FragmentError::Directory {
            ordinal,
            fault: DirectoryFault::CountWidthOverflow { kind, count, width },
        });
    };
    if expected != actual as usize {
        return Err(FragmentError::Directory {
            ordinal,
            fault: DirectoryFault::ByteLength {
                kind,
                expected,
                actual,
            },
        });
    }
    Ok(())
}

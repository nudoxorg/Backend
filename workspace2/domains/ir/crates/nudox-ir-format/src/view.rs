use core::num::TryFromIntError;

use nudox_ir_vocab::{EntityId, TypeId};
use thiserror::Error;

use crate::{
    EntityType, TypeNode, TypeNodeFault,
    wire::{
        ByteLength, ByteOffset, DIRECTORY_ENTRY_BYTES, ENTITY_BYTES, FragmentLayout, HEADER_BYTES,
        ItemCount, LaneLayout, SectionRequirement, TYPE_NODE_BYTES, decode_type_node,
        decode_validated_type_node, entity_fault, read_u16, read_u32, type_node_fault,
    },
};

pub use crate::wire::SectionKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WireField {
    DeclaredLength,
    SectionItemCount { ordinal: u16 },
    SectionOffset { ordinal: u16 },
    SectionByteLength { ordinal: u16 },
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DirectoryFault {
    #[error("section kind {actual} does not follow {previous}")]
    Order { previous: u16, actual: u16 },
    #[error("section {kind} has invalid flags {actual}")]
    Flags { kind: u16, actual: u16 },
    #[error("unknown section {kind} is marked required")]
    RequiredUnknown { kind: u16 },
    #[error("section {kind} starts at {actual}, not canonical offset {expected}")]
    Offset {
        kind: u16,
        expected: usize,
        actual: u32,
    },
    #[error("section {kind} has {actual} bytes, not {expected}")]
    ByteLength {
        kind: u16,
        expected: usize,
        actual: u32,
    },
    #[error("section {kind} count {count} times width {width} overflows")]
    CountWidthOverflow { kind: u16, count: u32, width: usize },
    #[error("section {kind} range {start}+{length} overflows")]
    RangeOverflow { kind: u16, start: u32, length: u32 },
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FragmentError {
    #[error("fragment header needs {required} bytes but only {actual} are present")]
    TruncatedHeader { required: usize, actual: usize },
    #[error("fragment magic {actual:?} is unknown")]
    Magic { actual: [u8; 4] },
    #[error("fragment schema {actual} is unknown")]
    Schema { actual: u16 },
    #[error("fragment declares {declared} bytes but received {actual}")]
    DeclaredLength { declared: usize, actual: usize },
    #[error("fragment requires extent {required} but received {actual} bytes")]
    Extent { required: usize, actual: usize },
    #[error("wire field {field:?} value {actual} does not fit this platform")]
    WireWidth {
        field: WireField,
        actual: u32,
        #[source]
        source: TryFromIntError,
    },
    #[error("directory entry {ordinal} is invalid: {fault}")]
    Directory {
        ordinal: u16,
        #[source]
        fault: DirectoryFault,
    },
    #[error("required section {section:?} is missing")]
    MissingSection { section: SectionKind },
    #[error("entity {ordinal:?} is invalid: {fault}")]
    Entity {
        ordinal: EntityId,
        #[source]
        fault: crate::EntityFault,
    },
    #[error("type node {ordinal:?} is invalid: {fault}")]
    TypeNode {
        ordinal: TypeId,
        #[source]
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
        let entity_lane = &envelope[layout.entities.range()];
        for (ordinal, record) in
            (0..layout.entities.count.get()).zip(entity_lane.chunks_exact(ENTITY_BYTES))
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
            (0..layout.type_nodes.count.get()).zip(type_lane.chunks_exact(TYPE_NODE_BYTES))
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

impl AsRef<[u8]> for FragmentView<'_> {
    fn as_ref(&self) -> &[u8] {
        self.envelope
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
    let section_count = crate::wire::SectionCount::from(read_u16(envelope, 6));
    let declared_wire_length = ByteLength::from(read_u32(envelope, 8));
    let declared_length =
        declared_wire_length
            .as_usize()
            .map_err(|source| FragmentError::WireWidth {
                field: WireField::DeclaredLength,
                actual: declared_wire_length.get(),
                source,
            })?;
    if declared_length != envelope.len() {
        return Err(FragmentError::DeclaredLength {
            declared: declared_length,
            actual: envelope.len(),
        });
    }
    let directory_bytes = section_count
        .as_usize()
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
    for ordinal in 0..section_count.get() {
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
        let start_index = start
            .as_usize()
            .map_err(|source| FragmentError::WireWidth {
                field: WireField::SectionOffset { ordinal },
                actual: start.get(),
                source,
            })?;
        if start_index != expected_offset {
            return Err(FragmentError::Directory {
                ordinal,
                fault: DirectoryFault::Offset {
                    kind: raw_kind,
                    expected: expected_offset,
                    actual: start.get(),
                },
            });
        }
        let byte_len_index = byte_len
            .as_usize()
            .map_err(|source| FragmentError::WireWidth {
                field: WireField::SectionByteLength { ordinal },
                actual: byte_len.get(),
                source,
            })?;
        let end_index =
            start_index
                .checked_add(byte_len_index)
                .ok_or(FragmentError::Directory {
                    ordinal,
                    fault: DirectoryFault::RangeOverflow {
                        kind: raw_kind,
                        start: start.get(),
                        length: byte_len.get(),
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
                actual: requirement.code(),
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
    let count_index = count
        .as_usize()
        .map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionItemCount { ordinal },
            actual: count.get(),
            source,
        })?;
    let expected = count_index
        .checked_mul(width)
        .ok_or(FragmentError::Directory {
            ordinal,
            fault: DirectoryFault::CountWidthOverflow {
                kind,
                count: count.get(),
                width,
            },
        })?;
    let actual_index = actual
        .as_usize()
        .map_err(|source| FragmentError::WireWidth {
            field: WireField::SectionByteLength { ordinal },
            actual: actual.get(),
            source,
        })?;
    if expected != actual_index {
        return Err(FragmentError::Directory {
            ordinal,
            fault: DirectoryFault::ByteLength {
                kind,
                expected,
                actual: actual.get(),
            },
        });
    }
    Ok(())
}

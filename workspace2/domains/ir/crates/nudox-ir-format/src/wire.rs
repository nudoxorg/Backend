use core::{num::TryFromIntError, ops::Range};

use nudox_ir_vocab::TypeId;

use crate::{EntityFault, PrimitiveType, TypeNode, TypeNodeFault};

pub(crate) const HEADER_BYTES: usize = 12;
pub(crate) const DIRECTORY_ENTRY_BYTES: usize = 16;
pub(crate) const ENTITY_BYTES: usize = 4;
pub(crate) const TYPE_NODE_BYTES: usize = 8;
pub(crate) const WRITTEN_SECTION_COUNT: SectionCount = SectionCount { wire: 2 };

const PRIMITIVE_TAG: u8 = 0;
const REFERENCE_TAG: u8 = 1;

#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SectionKind {
    EntityTypes = 1,
    TypeNodes = 2,
}

impl From<SectionKind> for u16 {
    fn from(kind: SectionKind) -> Self {
        kind as Self
    }
}

impl TryFrom<u16> for SectionKind {
    type Error = u16;

    fn try_from(actual: u16) -> Result<Self, Self::Error> {
        match actual {
            1 => Ok(Self::EntityTypes),
            2 => Ok(Self::TypeNodes),
            actual => Err(actual),
        }
    }
}

#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SectionRequirement {
    Optional = 0,
    Required = 1,
}

impl From<SectionRequirement> for u16 {
    fn from(requirement: SectionRequirement) -> Self {
        requirement as Self
    }
}

impl TryFrom<u16> for SectionRequirement {
    type Error = u16;

    fn try_from(actual: u16) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Optional),
            1 => Ok(Self::Required),
            actual => Err(actual),
        }
    }
}

#[repr(transparent)]
#[derive(Clone, Copy)]
pub(crate) struct SectionCount {
    wire: u16,
}

impl From<u16> for SectionCount {
    fn from(value: u16) -> Self {
        Self { wire: value }
    }
}

impl From<SectionCount> for usize {
    fn from(count: SectionCount) -> Self {
        usize::from(count.wire)
    }
}

impl From<SectionCount> for u16 {
    fn from(count: SectionCount) -> Self {
        count.wire
    }
}

macro_rules! transparent_wire_u32 {
    ($($name:ident),+ $(,)?) => {
        $(
            #[repr(transparent)]
            #[derive(Clone, Copy)]
            pub(crate) struct $name {
                wire: u32,
            }

            impl From<u32> for $name {
                fn from(value: u32) -> Self {
                    Self { wire: value }
                }
            }

            impl TryFrom<usize> for $name {
                type Error = TryFromIntError;

                fn try_from(value: usize) -> Result<Self, Self::Error> {
                    u32::try_from(value).map(|wire| Self { wire })
                }
            }

            impl TryFrom<$name> for usize {
                type Error = TryFromIntError;

                fn try_from(value: $name) -> Result<Self, Self::Error> {
                    usize::try_from(value.wire)
                }
            }

            impl From<$name> for u32 {
                fn from(value: $name) -> Self {
                    value.wire
                }
            }
        )+
    };
}

transparent_wire_u32!(ItemCount, ByteOffset, ByteLength);

#[derive(Clone, Copy)]
pub(crate) struct LaneLayout {
    pub(crate) count: ItemCount,
    pub(crate) start: ByteOffset,
    pub(crate) length: ByteLength,
    pub(crate) start_index: usize,
    pub(crate) end_index: usize,
}

impl LaneLayout {
    pub(crate) const fn range(self) -> Range<usize> {
        self.start_index..self.end_index
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FragmentLayout {
    pub(crate) entities: LaneLayout,
    pub(crate) type_nodes: LaneLayout,
    pub(crate) output_len: usize,
    pub(crate) output_wire_len: ByteLength,
}

pub(crate) const fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

pub(crate) const fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

pub(crate) fn write_u16(output: &mut [u8], offset: usize, value: u16) {
    output[offset..offset + size_of::<u16>()].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn write_u32(output: &mut [u8], offset: usize, value: u32) {
    output[offset..offset + size_of::<u32>()].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn entity_fault(target: TypeId, node_count: ItemCount) -> Option<EntityFault> {
    let node_count = u32::from(node_count);
    if target.raw < node_count {
        None
    } else {
        Some(EntityFault { target, node_count })
    }
}

pub(crate) fn type_node_fault(node: TypeNode, node_count: ItemCount) -> Option<TypeNodeFault> {
    let node_count = u32::from(node_count);
    match node {
        TypeNode::Primitive(_) => None,
        TypeNode::Reference(target) if target.raw < node_count => None,
        TypeNode::Reference(target) => Some(TypeNodeFault::Edge { target, node_count }),
    }
}

pub(crate) fn write_type_node(output: &mut [u8], node: TypeNode) {
    output.fill(0);
    match node {
        TypeNode::Primitive(primitive) => {
            output[0] = PRIMITIVE_TAG;
            write_u32(output, 4, u32::from(primitive));
        }
        TypeNode::Reference(target) => {
            output[0] = REFERENCE_TAG;
            write_u32(output, 4, target.raw);
        }
    }
}

pub(crate) fn decode_type_node(record: &[u8]) -> Result<TypeNode, TypeNodeFault> {
    let reserved = [record[1], record[2], record[3]];
    if reserved != [0; 3] {
        return Err(TypeNodeFault::Reserved { actual: reserved });
    }
    let operand = read_u32(record, 4);
    match record[0] {
        PRIMITIVE_TAG => PrimitiveType::try_from(operand).map(TypeNode::Primitive),
        REFERENCE_TAG => Ok(TypeNode::Reference(TypeId::new(operand))),
        actual => Err(TypeNodeFault::Tag { actual }),
    }
}

pub(crate) fn decode_validated_type_node(record: &[u8]) -> TypeNode {
    let operand = read_u32(record, 4);
    match (record[0], operand) {
        (PRIMITIVE_TAG, 0) => TypeNode::Primitive(PrimitiveType::Bool),
        (PRIMITIVE_TAG, _) => TypeNode::Primitive(PrimitiveType::I32),
        _ => TypeNode::Reference(TypeId::new(operand)),
    }
}

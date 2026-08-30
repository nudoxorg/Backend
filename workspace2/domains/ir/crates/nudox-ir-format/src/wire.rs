use core::ops::Range;

use nudox_ir_vocab::TypeId;

use crate::{PrimitiveType, TypeNode, TypeNodeFault};

pub(crate) const HEADER_BYTES: usize = 12;
pub(crate) const DIRECTORY_ENTRY_BYTES: usize = 16;
pub(crate) const ENTITY_BYTES: usize = 4;
pub(crate) const TYPE_NODE_BYTES: usize = 8;
pub(crate) const REQUIRED_SECTION: u16 = 1;
pub(crate) const ENTITY_SECTION: u16 = 1;
pub(crate) const TYPE_NODE_SECTION: u16 = 2;
pub(crate) const WRITTEN_SECTION_COUNT: u16 = 2;

const PRIMITIVE_TAG: u8 = 0;
const REFERENCE_TAG: u8 = 1;

#[derive(Clone, Copy)]
pub(crate) struct LaneLayout {
    pub(crate) count: u32,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

impl LaneLayout {
    pub(crate) const fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

#[derive(Clone, Copy)]
pub(crate) struct FragmentLayout {
    pub(crate) entities: LaneLayout,
    pub(crate) type_nodes: LaneLayout,
    pub(crate) output_len: usize,
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

pub(crate) const fn type_node_fault(node: TypeNode, node_count: u32) -> Option<TypeNodeFault> {
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
            write_u32(output, 4, primitive.code());
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
        PRIMITIVE_TAG => PrimitiveType::decode(operand).map(TypeNode::Primitive),
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

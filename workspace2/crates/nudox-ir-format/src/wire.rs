use core::{num::TryFromIntError, ops::Range};

use nudox_compile_vocab::{Language, NativeTool, Stage};
use nudox_id::{
    CompileRecipeDomain, ContentId, HASH_BYTES, SourceFactDomain, ToolchainDomain,
};
use nudox_ir_vocab::{AtomId, TypeId};

use crate::{
    AtomFault, EntityFault, EntityKind, EntityNameFault, EntityRecord, EntityRecordFault,
    PrimitiveType, RecipeFact, RecipeFactFault, SourceIdentity, SourceIdentityFault, TypeNode,
    TypeNodeFault,
};

pub(crate) struct HeaderLayout {
    pub(crate) magic: usize,
    pub(crate) schema: usize,
    pub(crate) section_count: usize,
    pub(crate) declared_length: usize,
    pub(crate) encoded_len: usize,
}

pub(crate) const HEADER_LAYOUT: HeaderLayout = HeaderLayout {
    magic: 0,
    schema: size_of::<[u8; 4]>(),
    section_count: size_of::<[u8; 4]>() + size_of::<u16>(),
    declared_length: size_of::<[u8; 4]>() + size_of::<u16>() + size_of::<u16>(),
    encoded_len: size_of::<[u8; 4]>() + size_of::<u16>() + size_of::<u16>() + size_of::<u32>(),
};

pub(crate) struct DirectoryEntryLayout {
    pub(crate) kind: usize,
    pub(crate) requirement: usize,
    pub(crate) item_count: usize,
    pub(crate) byte_offset: usize,
    pub(crate) byte_length: usize,
    pub(crate) encoded_len: usize,
}

pub(crate) const DIRECTORY_ENTRY_LAYOUT: DirectoryEntryLayout = DirectoryEntryLayout {
    kind: 0,
    requirement: size_of::<u16>(),
    item_count: size_of::<u16>() + size_of::<u16>(),
    byte_offset: size_of::<u16>() + size_of::<u16>() + size_of::<u32>(),
    byte_length: size_of::<u16>() + size_of::<u16>() + size_of::<u32>() + size_of::<u32>(),
    encoded_len: size_of::<u16>()
        + size_of::<u16>()
        + size_of::<u32>()
        + size_of::<u32>()
        + size_of::<u32>(),
};

pub(crate) struct TypeNodeLayout {
    pub(crate) tag: usize,
    pub(crate) reserved: usize,
    pub(crate) operand: usize,
    pub(crate) encoded_len: usize,
}

pub(crate) const TYPE_NODE_LAYOUT: TypeNodeLayout = TypeNodeLayout {
    tag: 0,
    reserved: size_of::<u8>(),
    operand: size_of::<u8>() + size_of::<[u8; 3]>(),
    encoded_len: size_of::<u8>() + size_of::<[u8; 3]>() + size_of::<u32>(),
};

pub(crate) const ENTITY_BYTES: usize = size_of::<u32>() + size_of::<u32>() + size_of::<u16>() * 2;
pub(crate) const TYPE_NODE_BYTES: usize = TYPE_NODE_LAYOUT.encoded_len;
pub(crate) const ATOM_RECORD_BYTES: usize = size_of::<u32>() * 2;
pub(crate) const SOURCE_IDENTITY_BYTES: usize = size_of::<u32>() + HASH_BYTES;
pub(crate) const RECIPE_FACT_BYTES: usize = size_of::<u8>() * 4 + HASH_BYTES * 2;
pub(crate) const WRITTEN_SECTION_COUNT: SectionCount = SectionCount { wire: 6 };

const PRIMITIVE_TAG: u8 = 0;
const REFERENCE_TAG: u8 = 1;

macro_rules! wire_enum_u16 {
    ($visibility:vis enum $name:ident { $($variant:ident = $wire:literal),+ $(,)? }) => {
        #[repr(u16)]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        $visibility enum $name {
            $($variant = $wire),+
        }

        impl From<$name> for u16 {
            fn from(value: $name) -> Self {
                match value {
                    $($name::$variant => $wire),+
                }
            }
        }

        impl TryFrom<u16> for $name {
            type Error = u16;

            fn try_from(actual: u16) -> Result<Self, Self::Error> {
                match actual {
                    $($wire => Ok($name::$variant)),+,
                    actual => Err(actual),
                }
            }
        }
    };
}

wire_enum_u16! {
    pub enum SectionKind {
        EntityTypes = 1,
        TypeNodes = 2,
        AtomRecords = 3,
        AtomBytes = 4,
        SourceIdentity = 5,
        RecipeFact = 6,
    }
}

wire_enum_u16! {
    pub(crate) enum SectionRequirement {
        Optional = 0,
        Required = 1,
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
    pub(crate) atoms: LaneLayout,
    pub(crate) atom_bytes: LaneLayout,
    pub(crate) source_identity: LaneLayout,
    pub(crate) recipe_fact: LaneLayout,
    pub(crate) source: SourceIdentity,
    pub(crate) recipe: RecipeFact,
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

pub(crate) fn entity_name_fault(target: AtomId, atom_count: ItemCount) -> Option<EntityNameFault> {
    let atom_count = u32::from(atom_count);
    if target.raw < atom_count {
        None
    } else {
        Some(EntityNameFault { target, atom_count })
    }
}

pub(crate) fn write_entity(output: &mut [u8], entity: EntityRecord) {
    write_u32(output, 0, entity.semantic_type.raw);
    write_u32(output, size_of::<u32>(), entity.name.raw);
    write_u16(output, size_of::<u32>() * 2, u16::from(entity.kind));
    write_u16(output, size_of::<u32>() * 2 + size_of::<u16>(), 0);
}

pub(crate) fn decode_entity(record: &[u8]) -> Result<EntityRecord, EntityRecordFault> {
    let kind_offset = size_of::<u32>() * 2;
    let reserved = read_u16(record, kind_offset + size_of::<u16>());
    if reserved != 0 {
        return Err(EntityRecordFault::Reserved { actual: reserved });
    }
    let kind = EntityKind::try_from(read_u16(record, kind_offset))
        .map_err(|actual| EntityRecordFault::Kind { actual })?;
    Ok(EntityRecord {
        semantic_type: TypeId::new(read_u32(record, 0)),
        name: AtomId::new(read_u32(record, size_of::<u32>())),
        kind,
    })
}

pub(crate) fn decode_validated_entity(record: &[u8]) -> EntityRecord {
    let kind_offset = size_of::<u32>() * 2;
    let kind = match read_u16(record, kind_offset) {
        0 => EntityKind::Function,
        1 => EntityKind::Constant,
        _ => EntityKind::Record,
    };
    EntityRecord {
        semantic_type: TypeId::new(read_u32(record, 0)),
        name: AtomId::new(read_u32(record, size_of::<u32>())),
        kind,
    }
}

pub(crate) fn write_atom_record(output: &mut [u8], start: u32, length: u32) {
    write_u32(output, 0, start);
    write_u32(output, size_of::<u32>(), length);
}

pub(crate) fn atom_fault(
    ordinal: AtomId,
    record: &[u8],
    byte_count: ItemCount,
) -> Option<AtomFault> {
    let start = read_u32(record, 0);
    let length = read_u32(record, size_of::<u32>());
    if length == 0 {
        return Some(AtomFault::Empty { ordinal });
    }
    let byte_count = u32::from(byte_count);
    match start.checked_add(length) {
        Some(end) if end <= byte_count => None,
        _ => Some(AtomFault::Range {
            ordinal,
            start,
            length,
            byte_count,
        }),
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
            output[TYPE_NODE_LAYOUT.tag] = PRIMITIVE_TAG;
            write_u32(output, TYPE_NODE_LAYOUT.operand, u32::from(primitive));
        }
        TypeNode::Reference(target) => {
            output[TYPE_NODE_LAYOUT.tag] = REFERENCE_TAG;
            write_u32(output, TYPE_NODE_LAYOUT.operand, target.raw);
        }
    }
}

pub(crate) fn write_source_identity(output: &mut [u8], source: SourceIdentity) {
    write_u32(output, 0, source.byte_len);
    output[size_of::<u32>()..SOURCE_IDENTITY_BYTES].copy_from_slice(source.identity.as_ref());
}

pub(crate) fn decode_source_identity(record: &[u8]) -> Result<SourceIdentity, SourceIdentityFault> {
    let mut raw = [0; HASH_BYTES];
    raw.copy_from_slice(&record[size_of::<u32>()..SOURCE_IDENTITY_BYTES]);
    let identity =
        ContentId::<SourceFactDomain>::try_from(raw).map_err(SourceIdentityFault::Authority)?;
    Ok(SourceIdentity {
        identity,
        byte_len: read_u32(record, 0),
    })
}

pub(crate) fn write_recipe_fact(output: &mut [u8], recipe: RecipeFact) {
    output[0] = u8::from(recipe.language);
    output[1] = u8::from(recipe.stage);
    output[2] = u8::from(recipe.tool);
    output[3] = 0;
    output[4..4 + HASH_BYTES].copy_from_slice(recipe.identity.as_ref());
    output[4 + HASH_BYTES..RECIPE_FACT_BYTES].copy_from_slice(recipe.toolchain.as_ref());
}

pub(crate) fn decode_recipe_fact(record: &[u8]) -> Result<RecipeFact, RecipeFactFault> {
    if record[3] != 0 {
        return Err(RecipeFactFault::Reserved { actual: record[3] });
    }
    let language = Language::try_from(record[0]).map_err(|actual| RecipeFactFault::Language { actual })?;
    let stage = Stage::try_from(record[1]).map_err(|actual| RecipeFactFault::Stage { actual })?;
    let tool = NativeTool::try_from(record[2]).map_err(|actual| RecipeFactFault::Tool { actual })?;
    let mut recipe_raw = [0; HASH_BYTES];
    recipe_raw.copy_from_slice(&record[4..4 + HASH_BYTES]);
    let identity = ContentId::<CompileRecipeDomain>::try_from(recipe_raw)
        .map_err(RecipeFactFault::Identity)?;
    let mut toolchain_raw = [0; HASH_BYTES];
    toolchain_raw.copy_from_slice(&record[4 + HASH_BYTES..RECIPE_FACT_BYTES]);
    let toolchain = ContentId::<ToolchainDomain>::try_from(toolchain_raw)
        .map_err(RecipeFactFault::Toolchain)?;
    Ok(RecipeFact {
        identity,
        language,
        stage,
        tool,
        toolchain,
    })
}

pub(crate) fn decode_type_node(record: &[u8]) -> Result<TypeNode, TypeNodeFault> {
    let reserved = [
        record[TYPE_NODE_LAYOUT.reserved],
        record[TYPE_NODE_LAYOUT.reserved + 1],
        record[TYPE_NODE_LAYOUT.reserved + 2],
    ];
    if reserved != [0; 3] {
        return Err(TypeNodeFault::Reserved { actual: reserved });
    }
    let operand = read_u32(record, TYPE_NODE_LAYOUT.operand);
    match record[TYPE_NODE_LAYOUT.tag] {
        PRIMITIVE_TAG => PrimitiveType::try_from(operand).map(TypeNode::Primitive),
        REFERENCE_TAG => Ok(TypeNode::Reference(TypeId::new(operand))),
        actual => Err(TypeNodeFault::Tag { actual }),
    }
}

pub(crate) fn decode_validated_type_node(record: &[u8]) -> TypeNode {
    let operand = read_u32(record, TYPE_NODE_LAYOUT.operand);
    match (record[TYPE_NODE_LAYOUT.tag], operand) {
        (PRIMITIVE_TAG, 0) => TypeNode::Primitive(PrimitiveType::Bool),
        (PRIMITIVE_TAG, 1) => TypeNode::Primitive(PrimitiveType::I32),
        (PRIMITIVE_TAG, _) => TypeNode::Primitive(PrimitiveType::String),
        _ => TypeNode::Reference(TypeId::new(operand)),
    }
}

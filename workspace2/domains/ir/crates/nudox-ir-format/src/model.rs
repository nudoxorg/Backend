use nudox_ir_vocab::{EntityId, TypeId};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("entity type target {target:?} is outside node count {node_count}")]
pub struct EntityFault {
    pub target: TypeId,
    pub node_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityRecord {
    pub semantic_type: TypeId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityType {
    pub entity: EntityId,
    pub semantic_type: TypeId,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveType {
    Bool = 0,
    I32 = 1,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeNode {
    Primitive(PrimitiveType),
    Reference(TypeId),
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TypeNodeFault {
    #[error("reserved bytes are nonzero: {actual:?}")]
    Reserved { actual: [u8; 3] },
    #[error("type node tag {actual} is unknown")]
    Tag { actual: u8 },
    #[error("primitive code {actual} is unknown")]
    Primitive { actual: u32 },
    #[error("type edge {target:?} is outside node count {node_count}")]
    Edge { target: TypeId, node_count: u32 },
}

impl From<PrimitiveType> for u32 {
    fn from(primitive: PrimitiveType) -> Self {
        primitive as Self
    }
}

impl TryFrom<u32> for PrimitiveType {
    type Error = TypeNodeFault;

    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Bool),
            1 => Ok(Self::I32),
            actual => Err(TypeNodeFault::Primitive { actual }),
        }
    }
}

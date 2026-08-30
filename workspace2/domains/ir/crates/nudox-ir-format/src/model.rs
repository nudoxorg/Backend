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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveType {
    Bool,
    I32,
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

impl PrimitiveType {
    pub(crate) const fn code(self) -> u32 {
        match self {
            Self::Bool => 0,
            Self::I32 => 1,
        }
    }

    pub(crate) const fn decode(actual: u32) -> Result<Self, TypeNodeFault> {
        match actual {
            0 => Ok(Self::Bool),
            1 => Ok(Self::I32),
            actual => Err(TypeNodeFault::Primitive { actual }),
        }
    }
}

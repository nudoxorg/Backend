use nudox_compile_vocab::CompileRecipeFact;
use nudox_id::{ContentId, SourceFactDomain};
use nudox_ir_vocab::{AtomId, EntityId, TypeId};
use thiserror::Error;

/// Immutable source identity carried by every production semantic fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceIdentity {
    /// Typed central source-content identity of the exact native adapter input.
    pub identity: ContentId<SourceFactDomain>,
    /// Exact source byte count bound to `identity` in the canonical fragment.
    pub byte_len: u32,
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum SourceIdentityFault {
    #[error("source identity authority is invalid")]
    Authority(#[source] nudox_id::ContentIdDecodeError),
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum RecipeFactFault {
    #[error("recipe reserved byte is nonzero: {actual}")]
    Reserved { actual: u8 },
    #[error("recipe language tag {actual} is unknown")]
    Language { actual: u8 },
    #[error("recipe stage tag {actual} is unknown")]
    Stage { actual: u8 },
    #[error("recipe tool tag {actual} is unknown")]
    Tool { actual: u8 },
    #[error("recipe identity authority is invalid")]
    Identity(#[source] nudox_id::ContentIdDecodeError),
    #[error("recipe toolchain authority is invalid")]
    Toolchain(#[source] nudox_id::ContentIdDecodeError),
    #[error("recipe identity does not bind decoded source and recipe facts")]
    IdentityRelation {
        expected: ContentId<nudox_id::CompileRecipeDomain>,
        observed: ContentId<nudox_id::CompileRecipeDomain>,
    },
}

/// Typed compilation recipe facts persisted alongside the source fact in every fragment.
pub type RecipeFact = CompileRecipeFact;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("entity type target {target:?} is outside node count {node_count}")]
pub struct EntityFault {
    pub target: TypeId,
    pub node_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("entity name atom {target:?} is outside atom count {atom_count}")]
pub struct EntityNameFault {
    pub target: AtomId,
    pub atom_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EntityRecordFault {
    #[error(transparent)]
    Type(#[from] EntityFault),
    #[error(transparent)]
    Name(#[from] EntityNameFault),
    #[error("entity kind tag {actual} is unknown")]
    Kind { actual: u16 },
    #[error("entity reserved bits are nonzero: {actual}")]
    Reserved { actual: u16 },
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum AtomFault {
    #[error("atom {ordinal:?} range {start}+{length} is outside {byte_count} atom bytes")]
    Range {
        ordinal: AtomId,
        start: u32,
        length: u32,
        byte_count: u32,
    },
    #[error("atom {ordinal:?} is empty")]
    Empty { ordinal: AtomId },
}

/// Closed declaration shape retained in the semantic entity lane.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityKind {
    Function = 0,
    Constant = 1,
    Record = 2,
}

impl From<EntityKind> for u16 {
    fn from(value: EntityKind) -> Self {
        match value {
            EntityKind::Function => 0,
            EntityKind::Constant => 1,
            EntityKind::Record => 2,
        }
    }
}

impl TryFrom<u16> for EntityKind {
    type Error = u16;

    fn try_from(actual: u16) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Function),
            1 => Ok(Self::Constant),
            2 => Ok(Self::Record),
            actual => Err(actual),
        }
    }
}

/// One borrowed semantic atom copied once into the fragment atom pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtomInput<'source> {
    /// Exact UTF-8-or-binary atom bytes retained by a source declaration.
    pub bytes: &'source [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityRecord {
    pub semantic_type: TypeId,
    pub name: AtomId,
    pub kind: EntityKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityType {
    pub entity: EntityId,
    pub semantic_type: TypeId,
    pub name: AtomId,
    pub kind: EntityKind,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrimitiveType {
    Bool = 0,
    I32 = 1,
    String = 2,
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
        match primitive {
            PrimitiveType::Bool => 0,
            PrimitiveType::I32 => 1,
            PrimitiveType::String => 2,
        }
    }
}

impl TryFrom<u32> for PrimitiveType {
    type Error = TypeNodeFault;

    fn try_from(actual: u32) -> Result<Self, Self::Error> {
        match actual {
            0 => Ok(Self::Bool),
            1 => Ok(Self::I32),
            2 => Ok(Self::String),
            actual => Err(TypeNodeFault::Primitive { actual }),
        }
    }
}

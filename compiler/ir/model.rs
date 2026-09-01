//! Defines model behavior for `compiler-ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the model invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::{AtomId, EntityId, TypeId};
use compiler_vocabulary::CompileRecipeFact;
use heart_identity::{ContentId, SourceFactDomain};
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
    Authority(#[source] heart_identity::ContentIdDecodeError),
}

#[derive(Debug, Eq, Error, PartialEq)]
pub enum RecipeFactFault {
    #[error("recipe language profile {actual:?} is unknown")]
    Profile { actual: [u8; 2] },
    #[error("recipe stage tag {actual} is unknown")]
    Stage { actual: u8 },
    #[error("recipe tool tag {actual} is unknown")]
    Tool { actual: u8 },
    #[error("recipe identity authority is invalid")]
    Identity(#[source] heart_identity::ContentIdDecodeError),
    #[error("recipe toolchain authority is invalid")]
    Toolchain(#[source] heart_identity::ContentIdDecodeError),
    #[error("recipe identity does not bind decoded source and recipe facts")]
    IdentityRelation {
        expected: ContentId<heart_identity::CompileRecipeDomain>,
        observed: ContentId<heart_identity::CompileRecipeDomain>,
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
///
/// The discriminant set is exactly the declaration-kind rows named by the
/// compiler parity matrix; codes 0..=2 predate the full set and never move.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityKind {
    Function = 0,
    Constant = 1,
    Record = 2,
    Module = 3,
    Field = 4,
    Alias = 5,
    Trait = 6,
    Implementation = 7,
    Enum = 8,
    Variant = 9,
    Static = 10,
    Reexport = 11,
    Parameter = 12,
}

impl EntityKind {
    /// Canonical declaration-kind order used by registry reports and tests.
    pub const ALL: [Self; 13] = [
        Self::Function,
        Self::Constant,
        Self::Record,
        Self::Module,
        Self::Field,
        Self::Alias,
        Self::Trait,
        Self::Implementation,
        Self::Enum,
        Self::Variant,
        Self::Static,
        Self::Reexport,
        Self::Parameter,
    ];
}

impl From<EntityKind> for u16 {
    fn from(value: EntityKind) -> Self {
        match value {
            EntityKind::Function => 0,
            EntityKind::Constant => 1,
            EntityKind::Record => 2,
            EntityKind::Module => 3,
            EntityKind::Field => 4,
            EntityKind::Alias => 5,
            EntityKind::Trait => 6,
            EntityKind::Implementation => 7,
            EntityKind::Enum => 8,
            EntityKind::Variant => 9,
            EntityKind::Static => 10,
            EntityKind::Reexport => 11,
            EntityKind::Parameter => 12,
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
            3 => Ok(Self::Module),
            4 => Ok(Self::Field),
            5 => Ok(Self::Alias),
            6 => Ok(Self::Trait),
            7 => Ok(Self::Implementation),
            8 => Ok(Self::Enum),
            9 => Ok(Self::Variant),
            10 => Ok(Self::Static),
            11 => Ok(Self::Reexport),
            12 => Ok(Self::Parameter),
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

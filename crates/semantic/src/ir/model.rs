//! Defines model behavior for `backend-semantic::ir`, whose purpose is to encode, validate, map, and borrow canonical compiler IR fragments.
//! This module owns the model invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use crate::ir::{AtomId, EntityId, TypeId};
use crate::vocabulary::CompileRecipeFact;
use backend_version::{ContentId, SourceFactDomain};
use thiserror::Error;

/// Immutable source identity carried by every production semantic fragment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceIdentity {
    /// Typed central source-content identity of the exact native adapter input.
    pub identity: ContentId<SourceFactDomain>,
    /// Exact source byte count bound to `identity` in the canonical fragment.
    pub byte_len: u32,
}

impl SourceIdentity {
    /// Captures exact bytes with the shared source-fact content identity domain.
    #[must_use]
    pub fn from_bytes(source: &[u8]) -> Option<Self> {
        Some(Self {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len()).ok()?,
        })
    }
}

#[derive(Debug, Eq, Error, PartialEq)]
/// Failure to decode the typed identity of the source bytes retained by a fragment.
pub enum SourceIdentityFault {
    /// The encoded identifier is not a valid source-fact content ID.
    #[error("source identity authority is invalid")]
    Authority(#[source] backend_version::ContentIdDecodeError),
}

#[derive(Debug, Eq, Error, PartialEq)]
/// Failure to decode or bind the compilation recipe persisted with a fragment.
pub enum RecipeFactFault {
    /// The two-byte language profile tag is not part of the closed profile registry.
    #[error("recipe language profile {actual:?} is unknown")]
    Profile {
        /// Unrecognized bytes read from the recipe record.
        actual: [u8; 2],
    },
    /// The recipe's stage tag is not recognized by this version of the registry.
    #[error("recipe stage tag {actual} is unknown")]
    Stage {
        /// Unrecognized stage discriminant from the wire record.
        actual: u8,
    },
    /// The recipe's tool tag is not recognized by this version of the registry.
    #[error("recipe tool tag {actual} is unknown")]
    Tool {
        /// Unrecognized tool discriminant from the wire record.
        actual: u8,
    },
    /// The encoded recipe identity is not a valid compile-recipe content ID.
    #[error("recipe identity authority is invalid")]
    Identity(#[source] backend_version::ContentIdDecodeError),
    /// The encoded toolchain identity is not a valid content ID.
    #[error("recipe toolchain authority is invalid")]
    Toolchain(#[source] backend_version::ContentIdDecodeError),
    /// The observed identity does not commit to the decoded recipe and source facts.
    #[error("recipe identity does not bind decoded source and recipe facts")]
    IdentityRelation {
        /// Identity recomputed from the decoded facts.
        expected: ContentId<backend_version::CompileRecipeDomain>,
        /// Identity carried by the record.
        observed: ContentId<backend_version::CompileRecipeDomain>,
    },
}

/// Typed compilation recipe facts persisted alongside the source fact in every fragment.
pub type RecipeFact = CompileRecipeFact;

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// An entity row points at a semantic type ordinal outside the fragment's type-node lane.
#[error("entity type target {target:?} is outside node count {node_count}")]
pub struct EntityFault {
    /// Type-node ordinal referenced by the entity row.
    pub target: TypeId,
    /// Number of type nodes available in the fragment.
    pub node_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// An entity row points at an atom ordinal outside the fragment's atom lane.
#[error("entity name atom {target:?} is outside atom count {atom_count}")]
pub struct EntityNameFault {
    /// Atom ordinal referenced as the entity name.
    pub target: AtomId,
    /// Number of atom records available in the fragment.
    pub atom_count: u32,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// An entity wire record contains an invalid type, name, kind, or reserved value.
pub enum EntityRecordFault {
    /// The entity's semantic-type ordinal is outside the type-node lane.
    #[error(transparent)]
    Type(#[from] EntityFault),
    /// The entity's name ordinal is outside the atom lane.
    #[error(transparent)]
    Name(#[from] EntityNameFault),
    /// The entity kind discriminant is not in the closed kind registry.
    #[error("entity kind tag {actual} is unknown")]
    Kind {
        /// Unrecognized kind code read from the entity record.
        actual: u16,
    },
    /// A reserved entity-record bit was set, so the record is noncanonical.
    #[error("entity reserved bits are nonzero: {actual}")]
    Reserved {
        /// Reserved-bit value read from the entity record.
        actual: u16,
    },
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// An atom record has an invalid byte range or refers to an empty name.
pub enum AtomFault {
    /// The atom's byte interval is out of bounds in the shared atom byte pool.
    #[error("atom {ordinal:?} range {start}+{length} is outside {byte_count} atom bytes")]
    Range {
        /// Atom-record ordinal whose range failed validation.
        ordinal: AtomId,
        /// Start offset in the byte pool, measured in bytes.
        start: u32,
        /// Length of the interval, measured in bytes.
        length: u32,
        /// Total byte length of the atom pool.
        byte_count: u32,
    },
    /// The atom record resolves to a zero-length slice, which is not a valid atom.
    #[error("atom {ordinal:?} is empty")]
    Empty {
        /// Atom-record ordinal of the empty slice.
        ordinal: AtomId,
    },
}

/// Closed declaration shape retained in the semantic entity lane.
///
/// The frozen discriminant registry lives in `backend-semantic`
/// (`ir_vocabulary`; it is shared by declaration-identity keys and occurrence
/// facts); this crate re-exports it unchanged.
pub use crate::ir_vocabulary::{EntityKind, EntityKindCodeError};

/// One borrowed semantic atom copied once into the fragment atom pool.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Borrowed atom bytes used as input while preparing a fragment.
pub struct AtomInput<'source> {
    /// Exact UTF-8-or-binary atom bytes retained by a source declaration.
    pub bytes: &'source [u8],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Validated entity-lane values before they are written to the canonical fragment.
pub struct EntityRecord {
    /// Ordinal in the type-node lane assigned as this entity's type.
    pub semantic_type: TypeId,
    /// Ordinal in the atom lane used as the entity name.
    pub name: AtomId,
    /// Closed declaration-shape code stored in the entity row.
    pub kind: EntityKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Entity identity and declaration facts paired with its type-lane reference.
pub struct EntityType {
    /// Ordinal of the entity row in the fragment.
    pub entity: EntityId,
    /// Type-node ordinal assigned to this entity.
    pub semantic_type: TypeId,
    /// Name atom ordinal from the fragment atom lane.
    pub name: AtomId,
    /// Closed declaration-shape code from the entity row.
    pub kind: EntityKind,
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Primitive type codes accepted by the fragment's closed type-node grammar.
pub enum PrimitiveType {
    /// Boolean scalar.
    Bool = 0,
    /// Signed 32-bit integer scalar.
    I32 = 1,
    /// String scalar.
    String = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// One node in the fragment's type graph.
pub enum TypeNode {
    /// A primitive leaf with no outgoing edge.
    Primitive(PrimitiveType),
    /// A reference to another ordinal in the fragment's type-node lane.
    Reference(TypeId),
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
/// A type-node record contains an invalid tag, primitive code, reserved byte, or edge.
pub enum TypeNodeFault {
    /// The record's reserved bytes are nonzero and therefore noncanonical.
    #[error("reserved bytes are nonzero: {actual:?}")]
    Reserved {
        /// Reserved bytes read from the node record.
        actual: [u8; 3],
    },
    /// The type-node tag is not part of the wire grammar.
    #[error("type node tag {actual} is unknown")]
    Tag {
        /// Unrecognized node tag read from the record.
        actual: u8,
    },
    /// The node names a primitive code not present in [`PrimitiveType`].
    #[error("primitive code {actual} is unknown")]
    Primitive {
        /// Unrecognized primitive code read from the record.
        actual: u32,
    },
    /// A reference node points past the available type-node lane.
    #[error("type edge {target:?} is outside node count {node_count}")]
    Edge {
        /// Type-node ordinal referenced by the edge.
        target: TypeId,
        /// Number of nodes available in the fragment.
        node_count: u32,
    },
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

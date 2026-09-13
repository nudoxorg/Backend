//! Public pooled-extension model and admission faults.

use thiserror::Error;

use crate::{
    TypeParameterInference, TypeParameterPrimaryRequirement, TypeParameterRequirements, Variance,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionTypeParameterBound<'bytes> {
    Type(u32),
    Lifetime(&'bytes [u8]),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionTypeParameterBoundRange {
    pub start: u32,
    pub length: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionTypeParameterKind {
    Type { inference: TypeParameterInference },
    ConstValue { value_type: u32 },
    Lifetime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionTypeParameter<'bytes> {
    pub name: &'bytes [u8],
    pub bounds: ExtensionTypeParameterBoundRange,
    pub default: Option<u32>,
    pub variance: Variance,
    pub kind: ExtensionTypeParameterKind,
    pub requirements: TypeParameterRequirements,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionTypeParameterRange {
    pub start: u32,
    pub length: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionRefList<'bytes> {
    pub elements: &'bytes [u32],
}

/// Closed identity of one homogeneous extension reference-list lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionPoolListLane {
    Atoms,
    Types,
    Entities,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionPoolsLane<'bytes> {
    pub type_parameters: &'bytes [ExtensionTypeParameter<'bytes>],
    pub type_parameter_bounds: &'bytes [ExtensionTypeParameterBound<'bytes>],
    pub type_parameter_lists: &'bytes [ExtensionTypeParameterRange],
    pub atom_lists: &'bytes [ExtensionRefList<'bytes>],
    pub type_lists: &'bytes [ExtensionRefList<'bytes>],
    pub entity_lists: &'bytes [ExtensionRefList<'bytes>],
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ExtensionPoolFault {
    #[error("type parameter {ordinal} has an empty name")]
    EmptyName { ordinal: u32 },
    #[error("type parameter {ordinal} {field:?} references type fact {raw} outside {limit}")]
    TypeReference {
        ordinal: u32,
        field: TypeParameterField,
        raw: u32,
        limit: u32,
    },
    #[error("type-parameter bound {bound} references type fact {raw} outside {limit}")]
    BoundTypeReference { bound: u32, raw: u32, limit: u32 },
    #[error(
        "type parameter {ordinal} has bound range start {start} length {length} outside {bound_count}"
    )]
    TypeParameterBounds {
        ordinal: u32,
        start: u32,
        length: u32,
        bound_count: u32,
    },
    #[error("type-parameter lifetime bound {bound} is empty")]
    EmptyLifetime { bound: u32 },
    #[error("type-parameter bound-list position {position} is outside length {length}")]
    TypeParameterBoundPosition { position: u32, length: u32 },
    #[error(
        "type parameter {ordinal} has illegal primary requirement {primary:?} with constructor={constructor} allows_ref_like={allows_ref_like}"
    )]
    TypeParameterRequirements {
        ordinal: u32,
        primary: TypeParameterPrimaryRequirement,
        constructor: bool,
        allows_ref_like: bool,
    },
    #[error("type parameter {ordinal} has an unknown {field:?} tag {actual}")]
    TypeParameterTag {
        ordinal: u32,
        field: TypeParameterTagField,
        actual: u8,
    },
    #[error(
        "type-parameter list {list} range start {start} length {length} exceeds {element_count} pooled elements"
    )]
    TypeParameterRange {
        list: u32,
        start: u32,
        length: u32,
        element_count: u32,
    },
    #[error("type-parameter list {list} is outside {count} exact ranges")]
    TypeParameterList { list: u32, count: u32 },
    #[error("type-parameter element {ordinal} is outside {count} pooled elements")]
    TypeParameterElement { ordinal: u32, count: u32 },
    #[error("type-parameter list member {position} is outside its length {length}")]
    TypeParameterPosition { position: u32, length: u32 },
    #[error("legacy type-parameter start {start} is outside {element_count} pooled elements")]
    LegacyTypeParameterStart { start: u32, element_count: u32 },
    #[error("{lane:?} list {list} element {position} references {raw} outside {limit}")]
    Reference {
        lane: ExtensionPoolListLane,
        list: u32,
        position: u32,
        raw: u32,
        limit: u32,
    },
    #[error("pooled-lane payload is truncated before {needed} bytes")]
    Truncated { needed: usize },
    #[error("pooled-lane payload carries trailing bytes after its declared lanes")]
    TrailingBytes,
    #[error("pooled-lane payload carries an unknown presence tag {actual}")]
    Presence { actual: u8 },
    #[error("pooled-lane cursor arithmetic overflowed at byte {at}")]
    StructuralOverflow { at: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeParameterField {
    LegacyConstraint,
    Default,
    ConstValueType,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeParameterTagField {
    Variance,
    Kind,
    PrimaryRequirement,
    RequirementFlags,
    Bound,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeParameterListBounds {
    ExactRanges { count: u32 },
    LegacyStarts { element_count: u32 },
}

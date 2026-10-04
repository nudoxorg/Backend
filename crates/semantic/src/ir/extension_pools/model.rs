//! Public pooled-extension model and admission faults.

use thiserror::Error;

use crate::ir::{
    TypeParameterInference, TypeParameterPrimaryRequirement, TypeParameterRequirements, Variance,
};

/// One pooled generic bound with a shared type or borrowed lifetime reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionTypeParameterBound<'bytes> {
    /// Type-fact coordinate forming one trait or constraint bound.
    Type(u32),
    /// Nonempty lifetime spelling borrowed from the encoded payload.
    Lifetime(&'bytes [u8]),
}

/// Contiguous half-open range of bound rows in the pooled bound lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionTypeParameterBoundRange {
    /// Zero-based first bound row in the lane.
    pub start: u32,
    /// Number of consecutive rows owned by this range.
    pub length: u32,
}

/// Closed kind-specific payload for one generic parameter record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionTypeParameterKind {
    /// Type parameter with its inference behavior.
    Type {
        /// Inference behavior permitted for this type parameter.
        inference: TypeParameterInference,
    },
    /// Constant parameter whose value has the referenced type fact.
    ConstValue {
        /// Type-fact coordinate of the constant's value type.
        value_type: u32,
    },
    /// Lifetime parameter with no type-fact payload.
    Lifetime,
}

/// Borrowed type-parameter record decoded from a pooled extension lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionTypeParameter<'bytes> {
    /// Nonempty parameter identifier bytes in the carrying payload.
    pub name: &'bytes [u8],
    /// Exact bounds run in the shared extension bound pool.
    pub bounds: ExtensionTypeParameterBoundRange,
    /// Optional default type-fact coordinate; `None` means no default.
    pub default: Option<u32>,
    /// Declared covariance, contravariance, or invariance.
    pub variance: Variance,
    /// Type, constant-value, or lifetime parameter payload.
    pub kind: ExtensionTypeParameterKind,
    /// Additional language-defined constraints on this parameter.
    pub requirements: TypeParameterRequirements,
}

/// Contiguous half-open range in the type-parameter element lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionTypeParameterRange {
    /// Zero-based first parameter or free-predicate row.
    pub start: u32,
    /// Number of consecutive rows in this list.
    pub length: u32,
}

/// One free generic predicate whose subject is not a declared parameter.
///
/// The subject is a type-fact coordinate and the ordered bounds reuse the
/// shared [`ExtensionTypeParameterBound`] lane.  Free predicates are a
/// Rust-only lane: only [`crate::ir::RustFacts`] carries a handle to them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionFreePredicate {
    /// Type-fact coordinate of the predicate subject.
    pub subject: u32,
    /// Ordered bound run in the shared `type_parameter_bounds` lane.
    pub bounds: ExtensionTypeParameterBoundRange,
}

/// Borrowed raw ordinals for one atom, type, or entity list.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionRefList<'bytes> {
    /// Raw ordinals interpreted in the homogeneous lane that owns this list.
    pub elements: &'bytes [u32],
}

/// Closed identity of one homogeneous extension reference-list lane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtensionPoolListLane {
    /// List elements address the shared atom table.
    Atoms,
    /// List elements address the shared type-fact table.
    Types,
    /// List elements address the shared entity table.
    Entities,
}

/// Borrowed views into one validated set of language-extension pools.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExtensionPoolsLane<'bytes> {
    /// Generic parameter records, ordered by their type-parameter ordinal.
    pub type_parameters: &'bytes [ExtensionTypeParameter<'bytes>],
    /// Bound records referenced by each parameter's bound range.
    pub type_parameter_bounds: &'bytes [ExtensionTypeParameterBound<'bytes>],
    /// Parameter lists referencing `type_parameters` by `(start, length)`.
    pub type_parameter_lists: &'bytes [ExtensionTypeParameterRange],
    /// Rust-only free-predicate row table: `(subject, ordered bounds)`.
    pub free_predicates: &'bytes [ExtensionFreePredicate],
    /// Exact `(start, length)` runs over [`Self::free_predicates`].
    pub free_predicate_lists: &'bytes [ExtensionTypeParameterRange],
    /// Atom-reference lists stored in the extension payload.
    pub atom_lists: &'bytes [ExtensionRefList<'bytes>],
    /// Type-reference lists stored in the extension payload.
    pub type_lists: &'bytes [ExtensionRefList<'bytes>],
    /// Entity-reference lists stored in the extension payload.
    pub entity_lists: &'bytes [ExtensionRefList<'bytes>],
}

/// Structural or reference fault found while admitting or reopening pooled data.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ExtensionPoolFault {
    /// A type-parameter record requires a nonempty identifier.
    #[error("type parameter {ordinal} has an empty name")]
    EmptyName {
        /// Type-parameter ordinal whose borrowed name is empty.
        ordinal: u32,
    },
    /// A type coordinate in a parameter record exceeds the carrying type lane.
    #[error("type parameter {ordinal} {field:?} references type fact {raw} outside {limit}")]
    TypeReference {
        /// Type-parameter ordinal containing the bad reference.
        ordinal: u32,
        /// Field in that parameter record which contains the reference.
        field: TypeParameterField,
        /// Raw type-fact ordinal encoded by the field.
        raw: u32,
        /// Number of type-fact rows available in the carrying fragment.
        limit: u32,
    },
    /// A type-kind bound references a coordinate outside the type lane.
    #[error("type-parameter bound {bound} references type fact {raw} outside {limit}")]
    BoundTypeReference {
        /// Bound-row ordinal containing the bad type reference.
        bound: u32,
        /// Raw type-fact ordinal encoded by that bound.
        raw: u32,
        /// Number of type-fact rows available in the carrying fragment.
        limit: u32,
    },
    /// A parameter's bound run extends outside the pooled bound rows.
    #[error(
        "type parameter {ordinal} has bound range start {start} length {length} outside {bound_count}"
    )]
    TypeParameterBounds {
        /// Type-parameter ordinal whose bound range is invalid.
        ordinal: u32,
        /// First bound row requested by the encoded range.
        start: u32,
        /// Number of bound rows requested.
        length: u32,
        /// Number of bound rows available in the shared pool.
        bound_count: u32,
    },
    /// Lifetime bounds must retain at least one source byte.
    #[error("type-parameter lifetime bound {bound} is empty")]
    EmptyLifetime {
        /// Bound-row ordinal with an empty lifetime spelling.
        bound: u32,
    },
    /// A requested bound-list element position is outside that list.
    #[error("type-parameter bound-list position {position} is outside length {length}")]
    TypeParameterBoundPosition {
        /// Requested zero-based index within a bound list.
        position: u32,
        /// Number of bounds in that list.
        length: u32,
    },
    /// A free-predicate subject references a coordinate outside the type lane.
    #[error("free predicate {predicate} references subject type fact {raw} outside {limit}")]
    FreePredicateSubject {
        /// Free-predicate row containing the bad subject coordinate.
        predicate: u32,
        /// Raw type-fact ordinal used for the subject.
        raw: u32,
        /// Number of type-fact rows available in the carrying fragment.
        limit: u32,
    },
    /// A free predicate's bound run extends outside the pooled bound rows.
    #[error(
        "free predicate {predicate} has bound range start {start} length {length} outside {bound_count}"
    )]
    FreePredicateBounds {
        /// Free-predicate row whose ordered bound range is invalid.
        predicate: u32,
        /// First requested bound row.
        start: u32,
        /// Number of requested bound rows.
        length: u32,
        /// Number of bound rows available in the shared pool.
        bound_count: u32,
    },
    /// A free-predicate list range extends outside its row pool.
    #[error(
        "free-predicate list {list} range start {start} length {length} exceeds {predicate_count} rows"
    )]
    FreePredicateList {
        /// Free-predicate list ordinal with the invalid range.
        list: u32,
        /// First requested free-predicate row.
        start: u32,
        /// Number of requested free-predicate rows.
        length: u32,
        /// Number of free-predicate rows available in the pool.
        predicate_count: u32,
    },
    /// A parameter combines generic requirements that the language rejects.
    #[error(
        "type parameter {ordinal} has illegal primary requirement {primary:?} with constructor={constructor} allows_ref_like={allows_ref_like}"
    )]
    TypeParameterRequirements {
        /// Parameter ordinal carrying the invalid requirement combination.
        ordinal: u32,
        /// Primary requirement that conflicts with the remaining flags.
        primary: TypeParameterPrimaryRequirement,
        /// Whether the parameter also requires a public parameterless constructor.
        constructor: bool,
        /// Whether ref-like types are admitted by the requirement set.
        allows_ref_like: bool,
    },
    /// A type-parameter field contains a discriminant outside the defined tags.
    #[error("type parameter {ordinal} has an unknown {field:?} tag {actual}")]
    TypeParameterTag {
        /// Parameter ordinal with the unknown encoded tag.
        ordinal: u32,
        /// Type-parameter field whose tag was decoded.
        field: TypeParameterTagField,
        /// Raw tag byte found in the payload.
        actual: u8,
    },
    /// An exact type-parameter list range exceeds its element pool.
    #[error(
        "type-parameter list {list} range start {start} length {length} exceeds {element_count} pooled elements"
    )]
    TypeParameterRange {
        /// Parameter-list ordinal with the invalid element range.
        list: u32,
        /// First requested type-parameter element.
        start: u32,
        /// Number of elements requested.
        length: u32,
        /// Number of pooled type-parameter elements available.
        element_count: u32,
    },
    /// A requested exact-range directory entry is absent.
    #[error("type-parameter list {list} is outside {count} exact ranges")]
    TypeParameterList {
        /// Requested list ordinal.
        list: u32,
        /// Number of exact ranges present in the lane.
        count: u32,
    },
    /// A requested type-parameter element is outside the element pool.
    #[error("type-parameter element {ordinal} is outside {count} pooled elements")]
    TypeParameterElement {
        /// Requested zero-based element ordinal.
        ordinal: u32,
        /// Number of pooled type-parameter rows present.
        count: u32,
    },
    /// A requested member position is outside the selected parameter list.
    #[error("type-parameter list member {position} is outside its length {length}")]
    TypeParameterPosition {
        /// Requested zero-based position within the list.
        position: u32,
        /// Number of elements in the selected list.
        length: u32,
    },
    /// A legacy start-only list begins outside its type-parameter pool.
    #[error("legacy type-parameter start {start} is outside {element_count} pooled elements")]
    LegacyTypeParameterStart {
        /// Start ordinal from the legacy start-only lane.
        start: u32,
        /// Number of pooled elements available for that lane.
        element_count: u32,
    },
    /// A reference list contains an ordinal outside its owning common lane.
    #[error("{lane:?} list {list} element {position} references {raw} outside {limit}")]
    Reference {
        /// Homogeneous pool addressed by the invalid list.
        lane: ExtensionPoolListLane,
        /// List ordinal containing the invalid element.
        list: u32,
        /// Zero-based position within that list.
        position: u32,
        /// Raw reference ordinal encoded in the element.
        raw: u32,
        /// Number of rows available in the addressed common lane.
        limit: u32,
    },
    /// Payload bytes end before the current field can be read.
    #[error("pooled-lane payload is truncated before {needed} bytes")]
    Truncated {
        /// Smallest payload length needed to complete the current field.
        needed: usize,
    },
    /// The payload contains bytes after all declared lanes have been consumed.
    #[error("pooled-lane payload carries trailing bytes after its declared lanes")]
    TrailingBytes,
    /// A presence byte does not match an encoded optional-field tag.
    #[error("pooled-lane payload carries an unknown presence tag {actual}")]
    Presence {
        /// Unknown presence-tag byte encountered in the payload.
        actual: u8,
    },
    /// Checked cursor arithmetic could not represent the next payload offset.
    #[error("pooled-lane cursor arithmetic overflowed at byte {at}")]
    StructuralOverflow {
        /// Payload byte offset where checked cursor arithmetic overflowed.
        at: usize,
    },
}

/// Parameter field that contains a type-fact reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeParameterField {
    /// Legacy constraint type in an older parameter record.
    LegacyConstraint,
    /// Optional default type coordinate.
    Default,
    /// Type coordinate describing a constant parameter's value.
    ConstValueType,
}

/// Encoded type-parameter field whose discriminant was not recognized.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeParameterTagField {
    /// Variance discriminator.
    Variance,
    /// Type, constant, or lifetime parameter-kind discriminator.
    Kind,
    /// Primary generic-requirement discriminator.
    PrimaryRequirement,
    /// Bitfield carrying secondary requirement flags.
    RequirementFlags,
    /// Bound-kind discriminator.
    Bound,
}

/// Directory format used to resolve type-parameter lists in a payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeParameterListBounds {
    /// Each list stores its exact starting ordinal and length.
    ExactRanges {
        /// Number of list entries in the directory.
        count: u32,
    },
    /// Legacy directory stores starts and derives ends from the next start.
    LegacyStarts {
        /// Number of type-parameter elements used to bound the final list.
        element_count: u32,
    },
}

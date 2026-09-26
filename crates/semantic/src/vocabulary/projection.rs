//! Shared portable projection admission and grammar vocabulary.

/// Exact foreign-key grammar rejection projected by Go, TypeScript, or
/// Python. The shared shape is closed and has no coordinate bag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionForeignKeyFault {
    /// The canonical path was empty.
    EmptyPath,
    /// The canonical path contained a backslash.
    BackslashInPath,
}

/// Exact rejected portion of one package lineage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionLineagePart {
    /// The ecosystem component.
    Ecosystem,
    /// The package-name component.
    Package,
    /// An explicitly identified non-canonical component segment.
    Invalid { segment: u8 },
}

/// Exact package-lineage grammar rejection projected by Go, TypeScript, or
/// Python.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionPackageLineageFault {
    /// The ecosystem component was empty.
    EmptyEcosystem,
    /// The package-name component was empty.
    EmptyPackage,
    /// The ecosystem contained the closed render separator.
    SeparatorInEcosystem,
    /// The package name contained the closed render separator.
    SeparatorInPackage,
    /// The named component contained a path separator.
    Backslash { part: ProjectionLineagePart },
}

/// Closed constructor tag used by the portable admission-fault snapshot.
///
/// This deliberately mirrors the IR constructor grammar without importing
/// the IR crate into the transport vocabulary crate.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionConstructorTag {
    /// Function product.
    Function,
    /// Generic product.
    Generic,
    /// Tuple product.
    Tuple,
    /// Array product.
    Array,
    /// Union product.
    Union,
    /// Intersection product.
    Intersection,
    /// Language-owned product.
    Product,
}

/// Exact portable constructor-admission fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionConstructorFault {
    /// The observed constructor tag was outside the closed grammar.
    Tag { actual: u32 },
    /// A reserved constructor payload was nonzero.
    ReservedPayload {
        tag: ProjectionConstructorTag,
        payload0: u32,
        payload1: u32,
    },
    /// Constructor payload arity overflowed its source width.
    ArityOverflow {
        tag: ProjectionConstructorTag,
        payload0: u32,
        payload1: u32,
    },
    /// Constructor arity disagreed with its admitted children.
    Arity {
        tag: ProjectionConstructorTag,
        expected: u32,
        actual: u32,
    },
}

/// Closed product-child role used by the portable admission snapshot.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionChildRole {
    /// Function parameter.
    FunctionParameter,
    /// Function result.
    FunctionResult,
    /// Generic argument.
    GenericArgument,
    /// Tuple element.
    TupleElement,
    /// Array element.
    ArrayElement,
    /// Union member.
    UnionMember,
    /// Intersection member.
    IntersectionMember,
    /// Language-owned product member.
    ProductMember,
}

/// Closed semantic type tag used by the portable admission snapshot.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionSemanticTypeTag {
    /// Receiver/self type.
    SelfType,
    /// Primitive type.
    Primitive,
    /// Tuple type.
    Tuple,
    /// Slice type.
    Slice,
    /// Fixed array type.
    Array,
    /// Union type.
    Union,
    /// Intersection type.
    Intersection,
    /// Never/bottom type.
    Never,
    /// Genuine top type.
    Any,
    /// Unknown type.
    Unknown,
    /// Nominal type.
    Nominal,
    /// Generic application.
    Apply,
    /// Generic type variable.
    TypeVar,
    /// Wildcard type.
    Wildcard,
    /// Callable type.
    FunctionPointer,
    /// Annotated type.
    Annotated,
    /// TypeScript conditional type.
    Conditional,
    /// TypeScript mapped type.
    Mapped,
    /// TypeScript template-literal type.
    TemplateLiteral,
    /// Anonymous structural record.
    AnonymousRecord,
    /// Rust `impl Trait`.
    ImplTrait,
    /// Rust `dyn Trait`.
    DynTrait,
    /// Inferred type.
    Inferred,
    /// Qualified path.
    QualifiedPath,
    /// Go map type.
    Map,
    /// Go channel type.
    Channel,
    /// Sequence array.
    ArraySequence,
    /// Rectangular array.
    ArrayRectangular,
    /// Fixed array with numeric extent.
    ArrayFixed,
    /// Array with source expression extent.
    ArrayConstExpression,
    /// Incomplete/dependent array.
    ArrayIncomplete,
    /// C-family qualified type.
    CQualified,
}

/// Closed type-record cell used by the portable admission snapshot.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionTypeCell {
    /// First payload cell.
    Payload0,
    /// Second payload cell.
    Payload1,
    /// First text cell.
    Text,
    /// Second text cell.
    Text2,
    /// Nominal target cell.
    Nominal,
}

/// Exact portable semantic-type admission fault.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionSemanticTypeFault {
    /// Observed type tag was outside the closed lattice.
    Tag { actual: u8 },
    /// A reserved cell carried a value.
    ReservedCell {
        tag: ProjectionSemanticTypeTag,
        cell: ProjectionTypeCell,
        actual: u32,
    },
    /// A required cell was absent.
    MissingCell {
        tag: ProjectionSemanticTypeTag,
        cell: ProjectionTypeCell,
    },
    /// Unknown-reason cell was invalid.
    Reason { actual: u32 },
    /// Primitive-shape cell was invalid.
    PrimitiveShape { actual: u32 },
    /// C-family qualifier cell was invalid.
    CvQualifiers { actual: u32 },
    /// Width cell was invalid.
    Width { actual: u32 },
    /// Child count violated the tag law.
    ChildCount {
        tag: ProjectionSemanticTypeTag,
        min: u32,
        max: u32,
        actual: u32,
    },
    /// Child carried a forbidden name.
    ChildNameForbidden {
        tag: ProjectionSemanticTypeTag,
        position: u32,
    },
    /// Child omitted a required name.
    ChildNameRequired {
        tag: ProjectionSemanticTypeTag,
        position: u32,
    },
    /// Child carried forbidden flag bits.
    ChildFlagsForbidden {
        tag: ProjectionSemanticTypeTag,
        position: u32,
        actual: u8,
    },
    /// Function row's variadic parameter marker was invalid.
    VariadicParameter { position: u32, actual: u8 },
    /// Child carried forbidden text.
    ChildTextForbidden {
        tag: ProjectionSemanticTypeTag,
        position: u32,
    },
}

/// Closed lane of the pooled type-child arena.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionTypeChildLane {
    /// Declared type rows.
    Declared,
    /// Anonymous type rows.
    Anonymous,
    /// Checker-computed type rows.
    Computed,
}

/// Closed lane name for a pooled reference target.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionFactLane {
    /// Declared type rows.
    TypeRows,
    /// Reserved anonymous type rows.
    ReservedTypeRows,
    /// Computed-row owners.
    ComputedOwners,
    /// Extension rows.
    Extensions,
    /// Replacement type-parameter ranges.
    ReplacementTypeParameterRange,
    /// Explicit type-parameter ranges.
    TypeParameterRanges,
    /// Captured type-parameter ranges.
    CapturedTypeParameterRange,
    /// Entity member provenance.
    EntityMembers,
    /// Entity parentage provenance.
    EntityParentage,
    /// Entity parent rows.
    EntityParents,
    /// Entity source spans.
    EntitySourceSpans,
    /// Type-parameter rows.
    TypeParameters,
    /// Type lists.
    TypeLists,
    /// Entity lists.
    EntityLists,
    /// Atom lists.
    AtomLists,
}

/// Exact portable parentage snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionParentageState {
    /// The authority did not provide a parentage fact.
    Unavailable,
    /// The entity is a proved root.
    Root,
    /// The entity is owned by this emitted parent ordinal.
    Bound { parent: u32 },
    /// The authority owner had no emitted row.
    UnrepresentedAuthorityOwner { identity: [u8; 16] },
}

/// Exact portable source span snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProjectionSpan {
    /// Inclusive source start.
    pub start: u32,
    /// Exclusive source end.
    pub end: u32,
}

/// Closed, value-only mirror of the driver's fact-admission rejection.
///
/// Native `usize` operands widen to `u64`, preserving their exact value while
/// keeping this vocabulary independent of the driver crate. No coordinate is
/// represented by a sentinel or an optional bag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionAdmissionFault {
    /// An emitted declaration name was empty.
    EmptyName,
    /// The bounded fact lane was full.
    Capacity,
    /// The fact's product-child lane was full.
    ChildCapacity,
    /// The flat product-child arena was exhausted.
    ProductChildPoolCapacity {
        used: u64,
        requested: u64,
        capacity: u64,
    },
    /// Constructor payload/children disagreed.
    Constructor { cause: ProjectionConstructorFault },
    /// Product-child role disagreed with the constructor lane.
    ChildRole {
        position: u64,
        expected: ProjectionChildRole,
        actual: ProjectionChildRole,
    },
    /// Product-child target was outside the pushed prefix.
    ChildTarget {
        position: u64,
        target: u32,
        fact_count: u64,
    },
    /// Type record violated the closed lattice.
    TypeRecord { cause: ProjectionSemanticTypeFault },
    /// One type-record child violated the closed child law.
    TypeChild {
        position: u64,
        cause: ProjectionSemanticTypeFault,
    },
    /// Type child target was outside the pushed prefix.
    TypeChildTarget {
        position: u64,
        target: u32,
        fact_count: u64,
    },
    /// The pending type-child lane was full.
    TypeChildCapacity,
    /// The flat type-child arena was exhausted.
    TypeChildPoolCapacity {
        lane: ProjectionTypeChildLane,
        used: u64,
        requested: u64,
        capacity: u64,
    },
    /// Anonymous type-row lane was full.
    TypeRowCapacity,
    /// Computed type-row lane was full.
    ComputedRowCapacity,
    /// Occurrence owner was outside the pushed prefix.
    OccurrenceOwner { owner: u32, fact_count: u64 },
    /// Occurrence lane was full.
    OccurrenceCapacity,
    /// Documentation owner was outside the pushed prefix.
    DocOwner { owner: u32, fact_count: u64 },
    /// Documentation lane was full.
    DocCapacity,
    /// Extension-atom lane was full.
    ExtensionAtomCapacity,
    /// Type-parameter lane was full.
    TypeParameterCapacity,
    /// Type/lifetime bound lane was full.
    TypeParameterBoundCapacity { requested: u64, available: u64 },
    /// Reference-list lane was full.
    RefListCapacity,
    /// Reference-list element count was too large.
    RefListElements,
    /// A pooled reference named an invalid target.
    RefTarget {
        lane: ProjectionFactLane,
        raw: u32,
        fact_count: u64,
    },
    /// Source span escaped the entered source lease.
    SourceSpan {
        entity: u32,
        start: u32,
        end: u32,
        source_len: u32,
    },
    /// Two authority passes supplied incompatible source spans.
    ConflictingSourceSpan {
        entity: u32,
        existing: ProjectionSpan,
        requested: ProjectionSpan,
    },
    /// Two authority passes supplied incompatible parentage claims.
    ConflictingParentage {
        entity: u32,
        existing: ProjectionParentageState,
        requested: ProjectionParentageState,
    },
}

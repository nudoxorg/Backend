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
    Invalid {
        /// Zero-based path component in the package lineage whose spelling is non-canonical.
        segment: u8,
    },
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
    Backslash {
        /// Package-lineage component in which the forbidden path separator occurred.
        part: ProjectionLineagePart,
    },
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
    Tag {
        /// Raw constructor tag value outside the closed IR constructor grammar.
        actual: u32,
    },
    /// A reserved constructor payload was nonzero.
    ReservedPayload {
        /// Closed constructor whose payload cells are being validated.
        tag: ProjectionConstructorTag,
        /// First constructor payload cell; its meaning or reserved status depends on the closed tag.
        payload0: u32,
        /// Second constructor payload cell; function result count occupies it and other tags may reserve it.
        payload1: u32,
    },
    /// Constructor payload arity overflowed its source width.
    ArityOverflow {
        /// Function constructor whose parameter and result counts overflowed the closed arity width.
        tag: ProjectionConstructorTag,
        /// Function parameter count before checked addition with the result count.
        payload0: u32,
        /// Function result count before checked addition with the parameter count.
        payload1: u32,
    },
    /// Constructor arity disagreed with its admitted children.
    Arity {
        /// Closed constructor whose child arity is being checked.
        tag: ProjectionConstructorTag,
        /// Number of child facts required by the constructor tag and payload.
        expected: u32,
        /// Number of admitted child facts supplied for the constructor.
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
    /// TypeScript's unevaluated `keyof` operator.
    KeyOf,
    /// TypeScript's unevaluated indexed-access operator.
    IndexedAccess,
    /// TypeScript's exact entity-targeted `typeof` query.
    TypeOf,
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
    Tag {
        /// Raw semantic-type tag outside the closed admitted type lattice.
        actual: u8,
    },
    /// A reserved cell carried a value.
    ReservedCell {
        /// Semantic-type form whose reserved payload cell must be zero.
        tag: ProjectionSemanticTypeTag,
        /// Specific payload or text cell that must remain empty for this type form.
        cell: ProjectionTypeCell,
        /// Nonzero cell value; a present-but-forbidden text cell is represented by one.
        actual: u32,
    },
    /// A required cell was absent.
    MissingCell {
        /// Semantic-type form that requires the absent cell.
        tag: ProjectionSemanticTypeTag,
        /// Required payload or text cell that was absent.
        cell: ProjectionTypeCell,
    },
    /// Unknown-reason cell was invalid.
    Reason {
        /// Raw unknown-type reason code that did not map to the closed `TypeReason` set.
        actual: u32,
    },
    /// Primitive-shape cell was invalid.
    PrimitiveShape {
        /// Raw primitive-shape code that did not map to the closed `PrimitiveShape` set.
        actual: u32,
    },
    /// C-family qualifier cell was invalid.
    CvQualifiers {
        /// Complete C-family qualifier bit cell, retained even for unknown bits or an empty wrapper.
        actual: u32,
    },
    /// Width cell was invalid.
    Width {
        /// Complete primitive-width cell bits retained from the type record.
        actual: u32,
    },
    /// Child count violated the tag law.
    ChildCount {
        /// Semantic-type form whose child-count law is being checked.
        tag: ProjectionSemanticTypeTag,
        /// Minimum number of child rows admitted for this type form.
        min: u32,
        /// Maximum number of child rows admitted for this type form.
        max: u32,
        /// Number of child rows actually supplied.
        actual: u32,
    },
    /// Child carried a forbidden name.
    ChildNameForbidden {
        /// Semantic-type form that determines whether this child may carry the value.
        tag: ProjectionSemanticTypeTag,
        /// Zero-based child position within the type record.
        position: u32,
    },
    /// Child omitted a required name.
    ChildNameRequired {
        /// Semantic-type form that determines whether this child may carry the value.
        tag: ProjectionSemanticTypeTag,
        /// Zero-based child position within the type record.
        position: u32,
    },
    /// Child carried forbidden flag bits.
    ChildFlagsForbidden {
        /// Semantic-type form that determines the allowed child flags.
        tag: ProjectionSemanticTypeTag,
        /// Zero-based child position within the type record.
        position: u32,
        /// Raw flag byte carried by that child.
        actual: u8,
    },
    /// Function row's variadic parameter marker was invalid.
    VariadicParameter {
        /// Zero-based child position marked as the variadic parameter.
        position: u32,
        /// Raw marker byte; the function grammar admits only its defined marker values.
        actual: u8,
    },
    /// Child carried forbidden text.
    ChildTextForbidden {
        /// Semantic-type form that determines whether this child may carry the value.
        tag: ProjectionSemanticTypeTag,
        /// Zero-based child position within the type record.
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
    Bound {
        /// Zero-based emitted fact ordinal that owns the entity; absence and root status use separate variants.
        parent: u32,
    },
    /// The authority owner had no emitted row.
    UnrepresentedAuthorityOwner {
        /// Exact 16-byte authority identity of the claimed parent for which no emitted row exists.
        identity: [u8; 16],
    },
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
        /// Number of product-child entries already retained.
        used: u64,
        /// Additional product-child entries required by the pending fact.
        requested: u64,
        /// Maximum product-child entries the flat arena can hold.
        capacity: u64,
    },
    /// Constructor payload/children disagreed.
    Constructor {
        /// Exact constructor grammar fault, including its tag and child-count operands.
        cause: ProjectionConstructorFault,
    },
    /// Product-child role disagreed with the constructor lane.
    ChildRole {
        /// Zero-based product-child position within the constructor.
        position: u64,
        /// Child role required at this position by the constructor grammar.
        expected: ProjectionChildRole,
        /// Child role supplied for this position.
        actual: ProjectionChildRole,
    },
    /// Product-child target was outside the pushed prefix.
    ChildTarget {
        /// Zero-based product-child position containing the target.
        position: u64,
        /// Zero-based fact ordinal named by this product child.
        target: u32,
        /// Number of facts already pushed; valid target ordinals are smaller.
        fact_count: u64,
    },
    /// Type record violated the closed lattice.
    TypeRecord {
        /// Exact semantic-type record fault and the failed cell or tag.
        cause: ProjectionSemanticTypeFault,
    },
    /// One type-record child violated the closed child law.
    TypeChild {
        /// Zero-based semantic-type child position that violated its child law.
        position: u64,
        /// Exact semantic-type fault for this child.
        cause: ProjectionSemanticTypeFault,
    },
    /// Type child target was outside the pushed prefix.
    TypeChildTarget {
        /// Zero-based semantic-type child position containing the target.
        position: u64,
        /// Zero-based fact ordinal named by this type child.
        target: u32,
        /// Number of facts already pushed; valid target ordinals are smaller.
        fact_count: u64,
    },
    /// The pending type-child lane was full.
    TypeChildCapacity,
    /// The flat type-child arena was exhausted.
    TypeChildPoolCapacity {
        /// Closed type-child arena lane whose bound was reached.
        lane: ProjectionTypeChildLane,
        /// Number of child entries already retained in this lane.
        used: u64,
        /// Additional child entries required by the pending row.
        requested: u64,
        /// Maximum child entries this lane can hold.
        capacity: u64,
    },
    /// Anonymous type-row lane was full.
    TypeRowCapacity,
    /// Computed type-row lane was full.
    ComputedRowCapacity,
    /// Native type recursion exceeded the projector's explicit depth bound.
    TypeProjectionDepthLimit {
        /// Depth observed at the rejected type node.
        depth: u64,
        /// Maximum admitted projection depth.
        maximum: u64,
    },
    /// Native type interning graph revisited a node already on the active path.
    TypeProjectionCycle {
        /// Exact interned type handle that closes the cycle.
        type_id: u32,
    },
    /// Native checker exposed a recursive back-reference that cannot be represented as a local row.
    TypeProjectionRecursiveReference {
        /// De Bruijn-style number of enclosing recursive scopes to reference.
        distance: u32,
    },
    /// A non-associative native type constructor exceeds the row child bound.
    TypeProjectionWidth {
        /// Exact number of children required by the source constructor.
        actual: u64,
        /// Maximum children admitted for one row.
        maximum: u64,
    },
    /// Occurrence owner was outside the pushed prefix.
    OccurrenceOwner {
        /// Zero-based emitted fact ordinal claimed as the occurrence owner.
        owner: u32,
        /// Number of facts already pushed; valid owner ordinals are smaller.
        fact_count: u64,
    },
    /// Occurrence lane was full.
    OccurrenceCapacity,
    /// Documentation owner was outside the pushed prefix.
    DocOwner {
        /// Zero-based emitted fact ordinal claimed as the documentation owner.
        owner: u32,
        /// Number of facts already pushed; valid owner ordinals are smaller.
        fact_count: u64,
    },
    /// Documentation lane was full.
    DocCapacity,
    /// Extension-atom lane was full.
    ExtensionAtomCapacity,
    /// Type-parameter lane was full.
    TypeParameterCapacity,
    /// Type/lifetime bound lane was full.
    TypeParameterBoundCapacity {
        /// Cumulative bound-plane endpoint after the append; the host's `usize::MAX` records checked-add overflow.
        requested: u64,
        /// Total planned capacity of the ordered type/lifetime bound lane.
        available: u64,
    },
    /// Reference-list lane was full.
    RefListCapacity,
    /// Reference-list element count was too large.
    RefListElements,
    /// A pooled reference named an invalid target.
    RefTarget {
        /// Closed relation lane in which the pooled target was read.
        lane: ProjectionFactLane,
        /// Raw zero-based fact ordinal stored as the relation target.
        raw: u32,
        /// Number of facts already pushed; valid target ordinals are smaller.
        fact_count: u64,
    },
    /// Source span escaped the entered source lease.
    SourceSpan {
        /// Zero-based emitted entity row whose span escaped the entered source.
        entity: u32,
        /// Inclusive source-byte offset of the reported entity span.
        start: u32,
        /// Exclusive source-byte offset of the reported entity span.
        end: u32,
        /// Byte length of the entered source lease.
        source_len: u32,
    },
    /// Two authority passes supplied incompatible source spans.
    ConflictingSourceSpan {
        /// Zero-based emitted entity row reported by both authority passes.
        entity: u32,
        /// Half-open source-byte span retained from the first pass.
        existing: ProjectionSpan,
        /// Half-open source-byte span reported by the later pass.
        requested: ProjectionSpan,
    },
    /// Two authority passes supplied incompatible parentage claims.
    ConflictingParentage {
        /// Zero-based emitted entity row whose owner claims disagree.
        entity: u32,
        /// Parentage state retained from the first authority pass.
        existing: ProjectionParentageState,
        /// Parentage state reported by the later authority pass.
        requested: ProjectionParentageState,
    },
    /// Conflicting complete direct-declaration inventories; operands name the first difference.
    ConflictingMemberInventory {
        /// Owner whose complete inventories disagree.
        entity: u32,
        /// Length of the retained canonical inventory.
        existing_count: u64,
        /// Length of the rejected canonical inventory.
        requested_count: u64,
        /// First differing canonical ordinal position.
        first_difference: u64,
        /// Retained member at that position, absent at the end of the run.
        existing_member: Option<u32>,
        /// Rejected member at that position, absent at the end of the run.
        requested_member: Option<u32>,
    },
}

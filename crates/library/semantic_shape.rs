//! Bounded, root-pinned transport contract for compiler-owned declaration shapes.
//!
//! The contract deliberately contains no compiler image coordinates. A shape
//! producer resolves all `TypeId`s while borrowing the exact admitted image and
//! returns only owned, request-local values and stable declaration identities.

use crate::{
    ProductAdmissionError, SemanticDeclarationIdentity, SemanticVersionRecord, SourceAtomText,
    SymbolAddress, ViewRevision, ViewStateRoot,
};
use backend_semantic::{
    ir::{
        BuiltinType, CSharpNullability, CSharpReferenceKind, ChannelDirection, Confidence,
        FunctionVariadicForm, ItemKind, PythonParameterKind, RustOwnership, TupleElementKind,
        TypeTag, UnknownReason,
    },
    vocabulary::LanguageProfile,
};
use backend_version::ObjectKey;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Maximum declarations in one semantic-shape request.
pub const MAX_SEMANTIC_SHAPE_BATCH: usize = 32;
/// Maximum type-expression and declaration-member nodes in one response.
pub const MAX_SEMANTIC_SHAPE_NODES: usize = 4096;
/// Maximum conservative wire-size estimate for one response.
pub const MAX_SEMANTIC_SHAPE_BYTES: usize = 256 * 1024;
/// Conservative product and projection byte charge for one callable carrier
/// declaration identity.
pub const SEMANTIC_SHAPE_CARRIER_IDENTITY_BYTES: usize = 192;
/// Maximum structural depth admitted in one type expression.
///
/// The wire vocabulary represents recursive edges through nested tagged JSON
/// objects. A nested callable edge adds up to five JSON containers, so this
/// limit keeps even the densest product shape below serde_json's built-in
/// nesting limit of 128 before the typed admission walk runs.
pub const MAX_SEMANTIC_SHAPE_DEPTH: usize = 22;

/// Product admission failures for the semantic-shape boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticShapeError {
    /// The request does not contain between one and the hard maximum of symbols.
    BatchBound,
    /// One symbol appears more than once.
    DuplicateSymbol,
    /// A caller budget is zero or exceeds the server-owned hard maximum.
    BudgetBound,
    /// The caller did not name a selected compiler generation.
    UnselectedSource,
    /// The selected compiler generation did not cover its complete scope.
    IncompleteSource,
    /// The selected-source record failed shared product admission.
    InvalidSource,
    /// A response basis differs from the exact request basis.
    WrongBasis,
    /// A response does not preserve the exact selected source record.
    WrongSource,
    /// A response does not contain the exact requested symbol sequence.
    WrongSymbols,
    /// A positive shape claim has no exact selected-image provenance.
    MissingOrigin,
    /// The same result contains an invalid image extent or source mismatch.
    InvalidOrigin,
    /// The response exceeds one of the request or hard output budgets.
    OutputBound,
    /// A type edge or recursive value is malformed.
    InvalidShape,
    /// Text exceeded the shared atom byte bound.
    Text(ProductAdmissionError),
}

impl fmt::Display for SemanticShapeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::BatchBound => "semantic-shape request exceeds its declaration bound",
            Self::DuplicateSymbol => "semantic-shape request contains a duplicate symbol",
            Self::BudgetBound => "semantic-shape request budget is outside its fixed bound",
            Self::UnselectedSource => "semantic-shape request source is not selected",
            Self::IncompleteSource => "semantic-shape request source is incomplete",
            Self::InvalidSource => "semantic-shape request source is malformed",
            Self::WrongBasis => "semantic-shape response has another view root",
            Self::WrongSource => "semantic-shape response has another selected semantic source",
            Self::WrongSymbols => "semantic-shape response differs from its requested symbols",
            Self::MissingOrigin => "semantic-shape response omitted selected-image provenance",
            Self::InvalidOrigin => "semantic-shape image provenance is malformed",
            Self::OutputBound => "semantic-shape response exceeds its fixed output bound",
            Self::InvalidShape => "semantic-shape response contains an invalid type graph",
            Self::Text(_) => "semantic-shape text exceeds its shared byte bound",
        })
    }
}

impl std::error::Error for SemanticShapeError {}

impl From<ProductAdmissionError> for SemanticShapeError {
    fn from(error: ProductAdmissionError) -> Self {
        Self::Text(error)
    }
}

/// Caller limits that can only tighten the fixed server limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticShapeBudget {
    max_nodes: u16,
    max_bytes: u32,
}

impl SemanticShapeBudget {
    /// Admits caller limits below the fixed server maxima.
    pub fn new(max_nodes: u16, max_bytes: u32) -> Result<Self, SemanticShapeError> {
        if max_nodes == 0
            || usize::from(max_nodes) > MAX_SEMANTIC_SHAPE_NODES
            || max_bytes == 0
            || usize::try_from(max_bytes).unwrap_or(usize::MAX) > MAX_SEMANTIC_SHAPE_BYTES
        {
            return Err(SemanticShapeError::BudgetBound);
        }
        Ok(Self {
            max_nodes,
            max_bytes,
        })
    }

    /// Largest node count this caller accepts.
    #[must_use]
    pub const fn max_nodes(self) -> u16 {
        self.max_nodes
    }

    /// Largest conservative encoded-size estimate this caller accepts.
    #[must_use]
    pub const fn max_bytes(self) -> u32 {
        self.max_bytes
    }
}

/// Exact selected-source read pinned to one visible view revision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticShapeRequest {
    basis: ViewRevision,
    source: SemanticVersionRecord,
    symbols: Box<[SymbolAddress]>,
    budget: SemanticShapeBudget,
}

impl SemanticShapeRequest {
    /// Admits one selected semantic generation and an ordered batch of symbols.
    pub fn new(
        basis: ViewStateRoot,
        source: SemanticVersionRecord,
        symbols: impl Into<Box<[SymbolAddress]>>,
        budget: SemanticShapeBudget,
    ) -> Result<Self, SemanticShapeError> {
        let symbols = symbols.into();
        if symbols.is_empty() || symbols.len() > MAX_SEMANTIC_SHAPE_BATCH {
            return Err(SemanticShapeError::BatchBound);
        }
        source
            .admit()
            .map_err(|_| SemanticShapeError::InvalidSource)?;
        admit_minimum_budget(&source, symbols.len(), budget)?;
        if !source.selected {
            return Err(SemanticShapeError::UnselectedSource);
        }
        if !source.complete {
            return Err(SemanticShapeError::IncompleteSource);
        }
        let mut seen = std::collections::BTreeSet::new();
        if symbols
            .iter()
            .any(|symbol| !seen.insert(symbol.claimed_bytes()))
        {
            return Err(SemanticShapeError::DuplicateSymbol);
        }
        Ok(Self {
            basis: basis.into(),
            source,
            symbols,
            budget,
        })
    }

    /// Rebuilds a request after strict wire identity admission.
    pub(crate) fn from_admitted_parts(
        basis: ViewRevision,
        source: SemanticVersionRecord,
        symbols: Box<[SymbolAddress]>,
        budget: SemanticShapeBudget,
    ) -> Result<Self, SemanticShapeError> {
        let mut seen = std::collections::BTreeSet::new();
        if symbols.is_empty() || symbols.len() > MAX_SEMANTIC_SHAPE_BATCH {
            return Err(SemanticShapeError::BatchBound);
        }
        source
            .admit()
            .map_err(|_| SemanticShapeError::InvalidSource)?;
        admit_minimum_budget(&source, symbols.len(), budget)?;
        if !source.selected {
            return Err(SemanticShapeError::UnselectedSource);
        }
        if !source.complete {
            return Err(SemanticShapeError::IncompleteSource);
        }
        if symbols
            .iter()
            .any(|symbol| !seen.insert(symbol.claimed_bytes()))
        {
            return Err(SemanticShapeError::DuplicateSymbol);
        }
        Ok(Self {
            basis,
            source,
            symbols,
            budget,
        })
    }

    /// Exact requested view revision.
    #[must_use]
    pub const fn basis(&self) -> ViewRevision {
        self.basis
    }

    /// Exact selected compiler source expected by the caller.
    #[must_use]
    pub const fn source(&self) -> &SemanticVersionRecord {
        &self.source
    }

    /// Ordered declaration selectors.
    #[must_use]
    pub fn symbols(&self) -> &[SymbolAddress] {
        &self.symbols
    }

    /// Tight caller output limits.
    #[must_use]
    pub const fn budget(&self) -> SemanticShapeBudget {
        self.budget
    }
}

/// Immutable authority for the exact image that supplied one result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticShapeImageOrigin {
    /// Exact typed semantic-image artifact identity and byte length.
    pub image: crate::interface::SemanticImageAuthority,
    /// Exact compiler profile admitted for this selected image.
    pub profile: LanguageProfile,
}

/// Selected semantic generation fields that the service rechecks against its durable relation.
///
/// This witness includes the full immutable compiler-generation identity and the current
/// source-input freshness observation. `selected` and `complete` are required before construction
/// and are committed as true. History publication status is intentionally omitted: it is an
/// asynchronous derived sidecar, not compiler-selection authority, and a shape read must neither
/// echo it as provenance nor trigger history work to validate it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticShapeSelection {
    package: crate::PackageReference,
    coordinate: crate::PackageCoordinate,
    profile: crate::SemanticLanguageProfile,
    generation: crate::SemanticGenerationId,
    generation_root: [u8; 32],
    dependency_set: [u8; 32],
    manifest: [u8; 32],
    artifacts: u32,
    semantic_bytes: u32,
    /// Source-input freshness recomputed against the current semantic authority.
    freshness: crate::SemanticVersionFreshness,
}

impl SemanticShapeSelection {
    /// Captures only the exact selected, complete compiler-generation authority.
    pub fn from_selected(source: &SemanticVersionRecord) -> Result<Self, SemanticShapeError> {
        source
            .admit()
            .map_err(|_| SemanticShapeError::InvalidSource)?;
        if !source.selected {
            return Err(SemanticShapeError::UnselectedSource);
        }
        if !source.complete {
            return Err(SemanticShapeError::IncompleteSource);
        }
        Ok(Self {
            package: source.package.clone(),
            coordinate: source.coordinate.clone(),
            profile: source.profile,
            generation: source.generation,
            generation_root: source.generation_root,
            dependency_set: source.dependency_set,
            manifest: source.manifest,
            artifacts: source.artifacts,
            semantic_bytes: source.semantic_bytes,
            freshness: source.freshness,
        })
    }

    /// Product package whose selected compiler generation owns this shape.
    #[must_use]
    pub const fn package(&self) -> &crate::PackageReference {
        &self.package
    }

    /// Exact compiler package coordinate selected for this read.
    #[must_use]
    pub const fn coordinate(&self) -> &crate::PackageCoordinate {
        &self.coordinate
    }

    /// Closed compiler language profile selected for this read.
    #[must_use]
    pub const fn profile(&self) -> crate::SemanticLanguageProfile {
        self.profile
    }

    /// Exact immutable compiler binding identity.
    #[must_use]
    pub const fn generation(&self) -> crate::SemanticGenerationId {
        self.generation
    }

    /// Verified compiler generation root.
    #[must_use]
    pub const fn generation_root(&self) -> &[u8; 32] {
        &self.generation_root
    }

    /// Verified dependency set commitment.
    #[must_use]
    pub const fn dependency_set(&self) -> &[u8; 32] {
        &self.dependency_set
    }

    /// Semantic manifest commitment.
    #[must_use]
    pub const fn manifest(&self) -> &[u8; 32] {
        &self.manifest
    }

    /// Number of image artifacts in the selected semantic manifest.
    #[must_use]
    pub const fn artifacts(&self) -> u32 {
        self.artifacts
    }

    /// Total encoded semantic-image extent in the selected manifest.
    #[must_use]
    pub const fn semantic_bytes(&self) -> u32 {
        self.semantic_bytes
    }

    /// Exact source-input freshness admitted for this selected generation.
    #[must_use]
    pub const fn freshness(&self) -> crate::SemanticVersionFreshness {
        self.freshness
    }
}

/// Provenance stamp for one selected semantic-shape response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticShapeSourceOrigin {
    /// Selected version identity rechecked against the durable selection relation.
    pub source: SemanticShapeSelection,
    /// Workspace snapshot root the owner observed while reading that selection.
    /// The owner checks the selected relation and source freshness again after
    /// projection; this is an optimistic before/after check, not an atomic
    /// cross-index proof or a request-side expected-root comparison.
    pub selection_root: [u8; 32],
    /// Exact image containing a known declaration or shape fact. `None`
    /// denotes closure-level unavailability without falsely attributing it
    /// to the first image in a multi-image generation.
    pub image: Option<SemanticShapeImageOrigin>,
}

/// A semantic type state that never treats absent facts as complete negatives.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticTypeFact {
    /// A concrete or computed type represented by the admitted type vocabulary.
    Known(SemanticTypeExpr),
    /// The compiler explicitly recorded an unknown type and its cause.
    Unknown {
        /// Closed semantic IR unknown-reason discriminant.
        reason: UnknownReason,
        /// Exact optional authority spelling.
        spelling: Option<SourceAtomText>,
    },
    /// The type exists in the compiler IR but this DTO version does not model its tag.
    Unsupported {
        /// Closed compiler IR type-tag discriminant.
        tag: TypeTag,
    },
    /// The source image did not provide a type row or a projection limit was reached.
    Unavailable(SemanticTypeUnavailable),
}

/// Why one nested type edge is unavailable without making a negative claim.
///
/// This vocabulary intentionally excludes selection-level states such as
/// `NoSelectedImage` and `NotInView`; those cannot truthfully appear inside an
/// admitted type graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticTypeUnavailable {
    /// The declaration has no semantic type coordinate in the image.
    MissingTypeCoordinate,
    /// An exact referenced entity or atom is missing from the validated image.
    MissingImageFact,
    /// The caller's node budget stopped projection.
    NodeBudget,
    /// The caller's byte budget stopped projection.
    ByteBudget,
    /// The caller's depth budget stopped projection.
    DepthBudget,
    /// A structural type cycle was encountered outside a nominal declaration edge.
    StructuralCycle,
    /// An atom required by this DTO is not UTF-8 text.
    InvalidText,
    /// A language extension row required to preserve shape detail is missing.
    MissingLanguageFact,
}

/// Why a semantic fact is unavailable without making a negative claim.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticShapeUnavailable {
    /// An exact referenced entity or atom is missing from the validated image.
    MissingImageFact,
    /// The selected semantic publication did not cover its declared scope completely.
    PartialPublication,
    /// The selected target has no published semantic image.
    NoSelectedImage,
    /// The caller's node budget stopped projection.
    NodeBudget,
    /// The caller's byte budget stopped projection.
    ByteBudget,
    /// The caller's depth budget stopped projection.
    DepthBudget,
    /// A structural type cycle was encountered outside a nominal declaration edge.
    StructuralCycle,
    /// An atom required by this DTO is not UTF-8 text.
    InvalidText,
    /// The owner observed the selector absent from the requested exact view root.
    /// The current root certificate grammar has positive row and root witnesses,
    /// but no sparse non-membership proof; the batch commitment therefore
    /// attests this owner observation rather than independently proving absence.
    NotInView,
    /// A language extension row required to preserve shape detail is missing.
    MissingLanguageFact,
}

/// Owned subset of the compiler type algebra used by callable and member shapes.
///
/// Nominals are terminal references. A recursive source type such as
/// `Node { next: Option<Box<Node>> }` therefore terminates at a declaration
/// identity rather than expanding the nominal declaration recursively.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticTypeExpr {
    /// Cross-language compiler builtin discriminant.
    Builtin(BuiltinType),
    /// Literal type with a closed payload specific to its category.
    Literal(SemanticLiteral),
    /// Local nominal declaration; `symbol` is present only when the declaration
    /// is also represented in the exact visible view.
    Nominal {
        /// Exact compiler declaration family and variant.
        declaration: SemanticDeclarationIdentity,
        /// Exact view-scoped navigation key, when available.
        symbol: Option<SymbolAddress>,
    },
    /// Exact external-endpoint commitment and its authority display spelling.
    External {
        /// Coordinate-independent external endpoint identity.
        identity: [u8; 32],
        /// Source-authoritative display, when valid bounded text.
        display: Option<SourceAtomText>,
    },
    /// Generic/type parameter name.
    Parameter(SourceAtomText),
    /// Type constructor application.
    Applied {
        /// Constructor type.
        constructor: Box<SemanticTypeFact>,
        /// Ordered type arguments.
        arguments: Box<[SemanticTypeFact]>,
    },
    /// Ordered tuple element list.
    Tuple(Box<[SemanticTypeElement]>),
    /// Ordered structural object member list.
    Object(Box<[SemanticObjectMember]>),
    /// Nested callable type.
    Function(Box<SemanticCallableShape>),
    /// Shared or mutable reference, with optional lifetime spelling.
    Reference {
        /// Referent type.
        target: Box<SemanticTypeFact>,
        /// True when the compiler recorded mutable access.
        mutable: bool,
        /// Optional lifetime spelling.
        lifetime: Option<SourceAtomText>,
    },
    /// Raw or language-neutral pointer.
    Pointer {
        /// Pointee type.
        target: Box<SemanticTypeFact>,
        /// True when the compiler recorded mutable access.
        mutable: bool,
    },
    /// Sequence/slice type.
    Slice(Box<SemanticTypeFact>),
    /// Array with the compiler's closed extent category and optional value.
    Array {
        /// Element type.
        element: Box<SemanticTypeFact>,
        /// Closed compiler array shape; impossible extent combinations cannot be constructed.
        shape: SemanticArrayShape,
    },
    /// Optional type.
    Optional(Box<SemanticTypeFact>),
    /// Ordered union alternatives.
    Union(Box<[SemanticTypeFact]>),
    /// Ordered intersection operands.
    Intersection(Box<[SemanticTypeFact]>),
    /// Go map type.
    Map {
        /// Key type.
        key: Box<SemanticTypeFact>,
        /// Value type.
        value: Box<SemanticTypeFact>,
    },
    /// Go channel type.
    Channel {
        /// Channel direction discriminant.
        direction: ChannelDirection,
        /// Element type.
        element: Box<SemanticTypeFact>,
    },
    /// Any compiler tag not yet represented above.
    Unsupported {
        /// Closed compiler IR type-tag discriminant.
        tag: TypeTag,
    },
}

/// Closed literal type category and its exact source-authoritative payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticLiteral {
    /// String literal contents without added quotes.
    String(SourceAtomText),
    /// Exact numeric spelling, retaining separators and precision.
    Number(SourceAtomText),
    /// Exact arbitrary-precision integer spelling.
    BigInt(SourceAtomText),
    /// Boolean literal value.
    Boolean(bool),
    /// Null literal.
    Null,
    /// Undefined literal.
    Undefined,
}

/// Closed compiler array shape with payloads coupled to the relevant variant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticArrayShape {
    /// Growable or unsized sequence.
    Sequence,
    /// Rectangular array with a positive rank.
    Rectangular { rank: std::num::NonZeroU16 },
    /// Fixed array with exact element count.
    FixedValue { length: u64 },
    /// Constant-expression extent retained as source text.
    ConstExpression(SourceAtomText),
    /// Incomplete native array with no known extent.
    Incomplete,
}

/// One ordered function parameter or result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticTypeElement {
    /// Optional compiler-owned element label.
    pub label: Option<SourceAtomText>,
    /// Required, optional, or rest tuple role.
    pub kind: TupleElementKind,
    /// Typed element fact.
    pub ty: SemanticTypeFact,
}

/// Exact declaration identities bound to one callable's ordered IR carriers.
///
/// `Unavailable` means the selected image did not provide an admitted owner to
/// carrier relation (including legacy image schemas). `Captured` is a complete
/// relation for this callable; its arrays contain stable family-and-variant
/// identities resolved from the same selected image and follow the
/// corresponding admitted IR tuple order. Empty arrays therefore mean the
/// selected image proved that the callable has no carriers in that role, and
/// are distinct from `Unavailable`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticCallableCarrierBindings {
    /// No complete owner-to-carrier relation is available in this image.
    Unavailable,
    /// Complete role-local carrier identities in compiler IR tuple order.
    Captured {
        /// Declaration identities for the callable's ordered parameters.
        parameters: Box<[SemanticDeclarationIdentity]>,
        /// Declaration identities for the callable's ordered results.
        results: Box<[SemanticDeclarationIdentity]>,
    },
}

/// One ordered structural object member.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticPropertyKey {
    /// Statically named property.
    Named(SourceAtomText),
    /// Private name with its exact source spelling.
    Private(SourceAtomText),
    /// Numeric property key with its exact source spelling.
    Numeric(SourceAtomText),
    /// Computed key whose type remains explicit.
    Computed(SemanticTypeFact),
}

/// One structural object member with its legal payload tied to its kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticObjectMember {
    /// Named or computed property.
    Property {
        key: SemanticPropertyKey,
        ty: SemanticTypeFact,
        optional: bool,
        readonly: bool,
    },
    /// Named or computed method signature.
    Method {
        key: SemanticPropertyKey,
        signature: SemanticTypeFact,
        optional: bool,
    },
    /// Index signature with a named parameter and independent key/value types.
    Index {
        parameter: SourceAtomText,
        key: SemanticTypeFact,
        value: SemanticTypeFact,
        readonly: bool,
    },
    /// Call signature.
    Call(SemanticTypeFact),
    /// Construct signature.
    Construct(SemanticTypeFact),
}

/// Function shape with source-ordered parameters and independent results.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticCallableShape {
    /// Parameters in compiler order.
    pub parameters: Box<[SemanticTypeElement]>,
    /// Results in compiler order.
    pub results: Box<[SemanticTypeElement]>,
    /// Owner-specific declaration identities for the admitted IR carriers.
    pub carrier_bindings: SemanticCallableCarrierBindings,
    /// Optional ABI spelling.
    pub abi: Option<SourceAtomText>,
    /// Compiler variadic-form discriminant.
    pub variadic: FunctionVariadicForm,
    /// Compiler-recorded unsafe flag.
    pub unsafe_: bool,
}

/// Member row retained on a record, object, enum, or module shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticShapeMember {
    /// Exact compiler declaration family and variant.
    pub identity: SemanticDeclarationIdentity,
    /// Exact compiler-owned member name.
    pub name: SourceAtomText,
    /// Closed member-kind discriminant.
    pub kind: ItemKind,
    /// Member type fact; absence remains explicit.
    pub ty: SemanticTypeFact,
    /// Extension facts attached to this exact member entity.
    pub language: SemanticShapeLanguageFacts,
}

/// Shape of one callable, aggregate declaration, or directly typed declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticDeclarationShape {
    /// Ordered callable contract.
    Callable(SemanticCallableShape),
    /// Ordered record/object members.
    Aggregate(Box<[SemanticShapeMember]>),
    /// Type alias, field, property, or other directly typed declaration.
    Typed(SemanticTypeFact),
}

/// Language-specific compiler facts needed to avoid erasing shape modifiers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticShapeLanguageFacts {
    /// No extension row was captured for this entity in the selected profile.
    Unavailable { profile: LanguageProfile },
    /// Only language-neutral compiler facts are represented.
    CommonOnly { profile: LanguageProfile },
    /// A bounded subset of profile-specific facts was captured.
    Partial {
        profile: LanguageProfile,
        facts: SemanticShapeLanguageFact,
    },
}

/// One closed profile-specific subset retained by this DTO version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticShapeLanguageFact {
    /// Rust ownership on the exact declaration.
    RustOwnership(RustOwnership),
    /// Go signature-level variadic status.
    GoVariadic(bool),
    /// Python parameter convention and dynamic confidence.
    PythonParameter {
        kind: PythonParameterKind,
        confidence: Confidence,
    },
    /// C# nullable/reference convention and callable effects.
    CSharp {
        nullability: CSharpNullability,
        reference_kind: CSharpReferenceKind,
        is_async: bool,
        is_iterator: bool,
        is_extension: bool,
    },
    /// TypeScript declaration and observed checker types.
    TypeScript {
        declared: Option<Box<SemanticTypeFact>>,
        observed: Option<Box<SemanticTypeFact>>,
    },
    /// Java throws types and annotations.
    Java {
        throws: Box<[SemanticTypeFact]>,
        annotations: Box<[SourceAtomText]>,
    },
    /// Clang qualifiers and measured layout.
    Clang {
        is_const: bool,
        is_volatile: bool,
        is_restrict: bool,
        size_bits: Option<u32>,
        align_bits: Option<u32>,
    },
}

/// Complete shape fact for one exact requested declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SemanticShapeFact {
    /// Shape projection succeeded from the selected typed image.
    Available {
        /// Structured declaration shape.
        shape: SemanticDeclarationShape,
        /// Language-specific type facts retained for this row.
        language: SemanticShapeLanguageFacts,
    },
    /// The root entity's compiler type is explicitly unknown.
    Unknown {
        /// Closed semantic IR unknown-reason discriminant.
        reason: UnknownReason,
        /// Exact optional authority spelling.
        spelling: Option<SourceAtomText>,
    },
    /// The selected compiler IR row uses a shape feature this DTO does not model.
    Unsupported {
        /// Closed compiler IR type-tag discriminant.
        tag: TypeTag,
    },
    /// No positive or negative shape claim can be made.
    Unavailable(SemanticShapeUnavailable),
}

/// One declaration result, with the image authority kept beside its shape.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticShapeEntry {
    /// Exact symbol selected from the request's visible root.
    pub symbol: SymbolAddress,
    /// Exact compiler declaration identity, when an image row was found.
    pub identity: Option<SemanticDeclarationIdentity>,
    /// Exact selected source and image origin, when an image row was found.
    pub origin: Option<SemanticShapeSourceOrigin>,
    /// Positive, unknown, unsupported, or unavailable shape state.
    pub fact: SemanticShapeFact,
}

/// Root-pinned batch of compiler-owned declaration shapes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticShapeBatch {
    /// Exact view revision requested by the caller.
    pub basis: ViewRevision,
    /// Results in the exact request order.
    pub entries: Box<[SemanticShapeEntry]>,
}

/// Bounded product walk over a semantic-shape batch.
///
/// The summary uses the same traversal for recursive node accounting, the
/// conservative product byte estimate, and nominal navigation selectors.
/// Wire admission still performs its own walk over the untrusted wire DTO
/// before it constructs this product value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticShapeAdmissionSummary {
    nodes: usize,
    bytes: usize,
    nominal_symbols: Box<[SymbolAddress]>,
}

impl SemanticShapeAdmissionSummary {
    /// Number of product nodes visited, including each entry envelope.
    #[must_use]
    pub const fn nodes(&self) -> usize {
        self.nodes
    }

    /// Conservative encoded-size estimate for the complete batch.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    /// Nominal declaration selectors found anywhere in the product graph.
    #[must_use]
    pub fn nominal_symbols(&self) -> &[SymbolAddress] {
        &self.nominal_symbols
    }
}

impl SemanticShapeBatch {
    /// Walks this owned product batch once under the fixed shape budget.
    ///
    /// The returned nominal selectors include only terminal declaration
    /// references. They are not resolved or authorized until the caller checks
    /// each one against the exact view root.
    pub fn admission_summary(&self) -> Result<SemanticShapeAdmissionSummary, SemanticShapeError> {
        if self.entries.is_empty() || self.entries.len() > MAX_SEMANTIC_SHAPE_BATCH {
            return Err(SemanticShapeError::BatchBound);
        }
        self.walk_admission_summary(None)
    }

    /// Admits result order, source proof, recursive shape budget, and text extents.
    pub fn admit_against(&self, request: &SemanticShapeRequest) -> Result<(), SemanticShapeError> {
        self.admit_with_summary(request).map(|_| ())
    }

    /// Admits the batch and returns the product walk needed by downstream
    /// certificate construction, avoiding a second traversal of the DTO.
    pub fn admit_with_summary(
        &self,
        request: &SemanticShapeRequest,
    ) -> Result<SemanticShapeAdmissionSummary, SemanticShapeError> {
        if self.basis != request.basis {
            return Err(SemanticShapeError::WrongBasis);
        }
        if self.entries.len() != request.symbols.len()
            || self
                .entries
                .iter()
                .zip(request.symbols.iter())
                .any(|(entry, requested)| *requested != entry.symbol)
        {
            return Err(SemanticShapeError::WrongSymbols);
        }

        let expected_source = SemanticShapeSelection::from_selected(&request.source)?;
        let expected_profile = request
            .source
            .profile
            .profile()
            .map_err(|_| SemanticShapeError::InvalidOrigin)?;
        for entry in &self.entries {
            match &entry.origin {
                Some(origin) => {
                    if origin.source != expected_source {
                        return Err(SemanticShapeError::InvalidOrigin);
                    }
                    if matches!(
                        &entry.fact,
                        SemanticShapeFact::Unavailable(SemanticShapeUnavailable::NotInView)
                    ) {
                        return Err(SemanticShapeError::InvalidOrigin);
                    }
                    match (&origin.image, &entry.fact, entry.identity) {
                        (Some(image), _, Some(_))
                            if image.image.byte_len > 0
                                && image.image.byte_len <= origin.source.semantic_bytes
                                && image.profile == expected_profile => {}
                        (None, SemanticShapeFact::Unavailable(_), None) => {}
                        _ => return Err(SemanticShapeError::InvalidOrigin),
                    }
                }
                None => {
                    if entry.identity.is_some()
                        || !matches!(
                            &entry.fact,
                            SemanticShapeFact::Unavailable(SemanticShapeUnavailable::NotInView)
                        )
                    {
                        return Err(SemanticShapeError::MissingOrigin);
                    }
                }
            }
        }
        let summary = self.walk_admission_summary(Some(expected_profile))?;
        let allowed_nodes = usize::from(request.budget.max_nodes).min(MAX_SEMANTIC_SHAPE_NODES);
        let allowed_bytes = usize::try_from(request.budget.max_bytes)
            .unwrap_or(usize::MAX)
            .min(MAX_SEMANTIC_SHAPE_BYTES);
        if summary.nodes > allowed_nodes || summary.bytes > allowed_bytes {
            return Err(SemanticShapeError::OutputBound);
        }
        Ok(summary)
    }

    fn walk_admission_summary(
        &self,
        expected_profile: Option<LanguageProfile>,
    ) -> Result<SemanticShapeAdmissionSummary, SemanticShapeError> {
        let mut walker = SemanticShapeWalker {
            expected_profile,
            ..SemanticShapeWalker::default()
        };
        for entry in &self.entries {
            walker.node(256)?;
            if let Some(origin) = &entry.origin {
                let source_bytes = serde_json::to_vec(&origin.source)
                    .map_err(|_| SemanticShapeError::InvalidOrigin)?
                    .len();
                walker.bytes(source_bytes.saturating_add(768))?;
            }
            walker.shape_fact(&entry.fact, 0)?;
        }
        Ok(SemanticShapeAdmissionSummary {
            nodes: walker.nodes,
            bytes: walker.bytes,
            nominal_symbols: walker.nominal_symbols.into_boxed_slice(),
        })
    }
}

#[derive(Default)]
struct SemanticShapeWalker {
    expected_profile: Option<LanguageProfile>,
    nodes: usize,
    bytes: usize,
    nominal_symbols: Vec<SymbolAddress>,
    seen_symbols: std::collections::BTreeSet<[u8; 32]>,
}

impl SemanticShapeWalker {
    fn node(&mut self, bytes: usize) -> Result<(), SemanticShapeError> {
        self.nodes = self
            .nodes
            .checked_add(1)
            .ok_or(SemanticShapeError::OutputBound)?;
        if self.nodes > MAX_SEMANTIC_SHAPE_NODES {
            return Err(SemanticShapeError::OutputBound);
        }
        self.bytes(bytes)
    }

    fn bytes(&mut self, bytes: usize) -> Result<(), SemanticShapeError> {
        self.bytes = self.bytes.saturating_add(bytes);
        if self.bytes > MAX_SEMANTIC_SHAPE_BYTES {
            return Err(SemanticShapeError::OutputBound);
        }
        Ok(())
    }

    fn text(&mut self, value: &SourceAtomText) -> Result<(), SemanticShapeError> {
        let text = value.as_str();
        if text.len() > crate::MAX_PRODUCT_TEXT_BYTES {
            return Err(SemanticShapeError::Text(ProductAdmissionError::TextBound));
        }
        self.bytes(text.len().saturating_mul(6).saturating_add(8))
    }

    fn shape_fact(
        &mut self,
        fact: &SemanticShapeFact,
        depth: usize,
    ) -> Result<(), SemanticShapeError> {
        if depth > MAX_SEMANTIC_SHAPE_DEPTH {
            return Err(SemanticShapeError::OutputBound);
        }
        self.node(96)?;
        match fact {
            SemanticShapeFact::Available { shape, language } => {
                self.language(language, 0)?;
                match shape {
                    SemanticDeclarationShape::Callable(callable) => self.callable(callable, 0)?,
                    SemanticDeclarationShape::Aggregate(members) => {
                        for member in members.iter() {
                            self.node(160)?;
                            self.text(&member.name)?;
                            self.language(&member.language, 0)?;
                            self.fact(&member.ty, 0)?;
                        }
                    }
                    SemanticDeclarationShape::Typed(ty) => self.fact(ty, 0)?,
                }
            }
            SemanticShapeFact::Unsupported { tag } if !is_supported_shape_tag(*tag) => {}
            SemanticShapeFact::Unsupported { .. } => {
                return Err(SemanticShapeError::InvalidShape);
            }
            SemanticShapeFact::Unknown { spelling, .. } => {
                if let Some(spelling) = spelling {
                    self.text(spelling)?;
                }
            }
            SemanticShapeFact::Unavailable(_) => {}
        }
        Ok(())
    }

    fn language(
        &mut self,
        language: &SemanticShapeLanguageFacts,
        depth: usize,
    ) -> Result<(), SemanticShapeError> {
        self.node(64)?;
        match language {
            SemanticShapeLanguageFacts::Unavailable { profile }
            | SemanticShapeLanguageFacts::CommonOnly { profile } => {
                if self
                    .expected_profile
                    .is_some_and(|expected| expected != *profile)
                {
                    return Err(SemanticShapeError::InvalidShape);
                }
            }
            SemanticShapeLanguageFacts::Partial { profile, facts } => {
                if self
                    .expected_profile
                    .is_some_and(|expected| expected != *profile)
                    || !language_fact_matches_profile(profile, facts)
                {
                    return Err(SemanticShapeError::InvalidShape);
                }
                match facts {
                    SemanticShapeLanguageFact::TypeScript { declared, observed } => {
                        for fact in declared.iter().chain(observed.iter()) {
                            self.fact(fact, depth)?;
                        }
                    }
                    SemanticShapeLanguageFact::Java {
                        throws,
                        annotations,
                    } => {
                        for fact in throws.iter() {
                            self.fact(fact, depth)?;
                        }
                        for annotation in annotations.iter() {
                            self.node(0)?;
                            self.text(annotation)?;
                        }
                    }
                    SemanticShapeLanguageFact::RustOwnership(_)
                    | SemanticShapeLanguageFact::GoVariadic(_)
                    | SemanticShapeLanguageFact::PythonParameter { .. }
                    | SemanticShapeLanguageFact::CSharp { .. }
                    | SemanticShapeLanguageFact::Clang { .. } => {}
                }
            }
        }
        Ok(())
    }

    fn callable(
        &mut self,
        callable: &SemanticCallableShape,
        depth: usize,
    ) -> Result<(), SemanticShapeError> {
        if let SemanticCallableCarrierBindings::Captured {
            parameters,
            results,
        } = &callable.carrier_bindings
        {
            if parameters.len() != callable.parameters.len()
                || results.len() != callable.results.len()
            {
                return Err(SemanticShapeError::InvalidShape);
            }
            let bindings = parameters
                .len()
                .checked_add(results.len())
                .ok_or(SemanticShapeError::OutputBound)?;
            if bindings > MAX_SEMANTIC_SHAPE_NODES.saturating_sub(self.nodes) {
                return Err(SemanticShapeError::OutputBound);
            }
            for _ in parameters.iter().chain(results.iter()) {
                self.node(SEMANTIC_SHAPE_CARRIER_IDENTITY_BYTES)?;
            }
        }
        for element in callable.parameters.iter().chain(callable.results.iter()) {
            self.node(48)?;
            if let Some(label) = &element.label {
                self.text(label)?;
            }
            self.fact(&element.ty, depth)?;
        }
        if let Some(abi) = &callable.abi {
            self.text(abi)?;
        }
        Ok(())
    }

    fn fact(&mut self, fact: &SemanticTypeFact, depth: usize) -> Result<(), SemanticShapeError> {
        if depth > MAX_SEMANTIC_SHAPE_DEPTH {
            return Err(SemanticShapeError::OutputBound);
        }
        self.node(96)?;
        match fact {
            SemanticTypeFact::Known(expression) => self.expression(expression, depth)?,
            SemanticTypeFact::Unknown { spelling, .. } => {
                if let Some(spelling) = spelling {
                    self.text(spelling)?;
                }
            }
            SemanticTypeFact::Unsupported { tag } if !is_supported_shape_tag(*tag) => {}
            SemanticTypeFact::Unsupported { .. } => {
                return Err(SemanticShapeError::InvalidShape);
            }
            SemanticTypeFact::Unavailable(_) => {}
        }
        Ok(())
    }

    fn element(
        &mut self,
        element: &SemanticTypeElement,
        depth: usize,
    ) -> Result<(), SemanticShapeError> {
        self.node(48)?;
        if let Some(label) = &element.label {
            self.text(label)?;
        }
        self.fact(&element.ty, depth)
    }

    fn expression(
        &mut self,
        expression: &SemanticTypeExpr,
        depth: usize,
    ) -> Result<(), SemanticShapeError> {
        if depth > MAX_SEMANTIC_SHAPE_DEPTH {
            return Err(SemanticShapeError::OutputBound);
        }
        self.node(64)?;
        match expression {
            SemanticTypeExpr::Builtin(_) => {}
            SemanticTypeExpr::Unsupported { tag } if !is_supported_shape_tag(*tag) => {}
            SemanticTypeExpr::Unsupported { .. } => {
                return Err(SemanticShapeError::InvalidShape);
            }
            SemanticTypeExpr::Literal(literal) => match literal {
                SemanticLiteral::String(value)
                | SemanticLiteral::Number(value)
                | SemanticLiteral::BigInt(value) => self.text(value)?,
                SemanticLiteral::Boolean(_)
                | SemanticLiteral::Null
                | SemanticLiteral::Undefined => {}
            },
            SemanticTypeExpr::Nominal { symbol, .. } => {
                self.bytes(320)?;
                if let Some(symbol) = symbol {
                    if self.seen_symbols.insert(symbol.claimed_bytes()) {
                        self.nominal_symbols.push(*symbol);
                    }
                }
            }
            SemanticTypeExpr::External { display, .. } => {
                self.bytes(160)?;
                if let Some(display) = display {
                    self.text(display)?;
                }
            }
            SemanticTypeExpr::Parameter(name) => self.text(name)?,
            SemanticTypeExpr::Applied {
                constructor,
                arguments,
            } => {
                self.fact(constructor, depth + 1)?;
                for argument in arguments.iter() {
                    self.fact(argument, depth + 1)?;
                }
            }
            SemanticTypeExpr::Tuple(elements) => {
                for element in elements.iter() {
                    self.element(element, depth + 1)?;
                }
            }
            SemanticTypeExpr::Object(members) => {
                for member in members.iter() {
                    self.object_member(member, depth + 1)?;
                }
            }
            SemanticTypeExpr::Function(callable) => self.callable(callable, depth + 1)?,
            SemanticTypeExpr::Reference {
                target, lifetime, ..
            } => {
                self.fact(target, depth + 1)?;
                if let Some(lifetime) = lifetime {
                    self.text(lifetime)?;
                }
            }
            SemanticTypeExpr::Pointer { target, .. }
            | SemanticTypeExpr::Slice(target)
            | SemanticTypeExpr::Optional(target) => self.fact(target, depth + 1)?,
            SemanticTypeExpr::Array { element, shape } => {
                self.fact(element, depth + 1)?;
                if let SemanticArrayShape::ConstExpression(expression) = shape {
                    self.text(expression)?;
                }
            }
            SemanticTypeExpr::Union(items) | SemanticTypeExpr::Intersection(items) => {
                for item in items.iter() {
                    self.fact(item, depth + 1)?;
                }
            }
            SemanticTypeExpr::Map { key, value } => {
                self.fact(key, depth + 1)?;
                self.fact(value, depth + 1)?;
            }
            SemanticTypeExpr::Channel { element, .. } => self.fact(element, depth + 1)?,
        }
        Ok(())
    }

    fn object_member(
        &mut self,
        member: &SemanticObjectMember,
        depth: usize,
    ) -> Result<(), SemanticShapeError> {
        self.node(96)?;
        match member {
            SemanticObjectMember::Property { key, ty, .. } => {
                self.property_key(key, depth)?;
                self.fact(ty, depth)?;
            }
            SemanticObjectMember::Method { key, signature, .. } => {
                self.property_key(key, depth)?;
                self.fact(signature, depth)?;
            }
            SemanticObjectMember::Index {
                parameter,
                key,
                value,
                ..
            } => {
                self.text(parameter)?;
                self.fact(key, depth)?;
                self.fact(value, depth)?;
            }
            SemanticObjectMember::Call(signature) | SemanticObjectMember::Construct(signature) => {
                self.fact(signature, depth)?
            }
        }
        Ok(())
    }

    fn property_key(
        &mut self,
        key: &SemanticPropertyKey,
        depth: usize,
    ) -> Result<(), SemanticShapeError> {
        match key {
            SemanticPropertyKey::Named(value)
            | SemanticPropertyKey::Private(value)
            | SemanticPropertyKey::Numeric(value) => self.text(value),
            SemanticPropertyKey::Computed(fact) => self.fact(fact, depth),
        }
    }
}

fn is_supported_shape_tag(tag: TypeTag) -> bool {
    matches!(
        tag,
        TypeTag::Builtin
            | TypeTag::Literal
            | TypeTag::Nominal
            | TypeTag::External
            | TypeTag::Parameter
            | TypeTag::Applied
            | TypeTag::Tuple
            | TypeTag::Object
            | TypeTag::Function
            | TypeTag::Reference
            | TypeTag::Pointer
            | TypeTag::Slice
            | TypeTag::Array
            | TypeTag::Optional
            | TypeTag::Union
            | TypeTag::Intersection
            | TypeTag::Unknown
            | TypeTag::Map
            | TypeTag::Channel
    )
}

fn language_fact_matches_profile(
    profile: &LanguageProfile,
    facts: &SemanticShapeLanguageFact,
) -> bool {
    matches!(
        (profile, facts),
        (
            LanguageProfile::Rust(_),
            SemanticShapeLanguageFact::RustOwnership(_)
        ) | (
            LanguageProfile::Go(_),
            SemanticShapeLanguageFact::GoVariadic(_)
        ) | (
            LanguageProfile::Python(_),
            SemanticShapeLanguageFact::PythonParameter { .. }
        ) | (
            LanguageProfile::CSharp(_),
            SemanticShapeLanguageFact::CSharp { .. }
        ) | (
            LanguageProfile::TypeScript(_),
            SemanticShapeLanguageFact::TypeScript { .. }
        ) | (
            LanguageProfile::Java(_),
            SemanticShapeLanguageFact::Java { .. }
        ) | (
            LanguageProfile::C(_) | LanguageProfile::Cxx(_),
            SemanticShapeLanguageFact::Clang { .. }
        )
    )
}

fn admit_minimum_budget(
    source: &SemanticVersionRecord,
    symbols: usize,
    budget: SemanticShapeBudget,
) -> Result<(), SemanticShapeError> {
    let source_bytes = serde_json::to_vec(source)
        .map_err(|_| SemanticShapeError::InvalidOrigin)?
        .len();
    let min_nodes = symbols.saturating_mul(2);
    let min_bytes = symbols.saturating_mul(source_bytes.saturating_add(768 + 256 + 96));
    if usize::from(budget.max_nodes) < min_nodes
        || usize::try_from(budget.max_bytes).unwrap_or(usize::MAX) < min_bytes
    {
        return Err(SemanticShapeError::BudgetBound);
    }
    Ok(())
}

/// Canonical preimage for the selected source/image witness carried by each result.
/// The preimage commits selected compiler-generation fields, current source-input
/// freshness, the durable selection root, and the optional image identity.
/// History publication status is a separate derived sidecar observation: this
/// contract neither echoes it as authority nor triggers history work to validate it.
pub fn semantic_shape_source_preimage(origin: &SemanticShapeSourceOrigin) -> Vec<u8> {
    fn append(bytes: &mut Vec<u8>, value: &[u8]) {
        bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
        bytes.extend_from_slice(value);
    }

    let mut bytes = b"SEMANTIC-SHAPE-SOURCE\0v3".to_vec();
    append(&mut bytes, origin.source.package.as_str().as_bytes());
    append(&mut bytes, origin.source.coordinate.as_str().as_bytes());
    append(&mut bytes, &origin.source.profile.to_bytes());
    append(&mut bytes, &origin.source.generation.to_bytes());
    append(&mut bytes, &origin.source.generation_root);
    append(&mut bytes, &origin.source.dependency_set);
    append(&mut bytes, &origin.source.manifest);
    bytes.extend_from_slice(&origin.source.artifacts.to_be_bytes());
    bytes.extend_from_slice(&origin.source.semantic_bytes.to_be_bytes());
    bytes.extend_from_slice(&[1, 1]);
    match origin.source.freshness {
        crate::SemanticVersionFreshness::Current { input_digest } => {
            bytes.push(0);
            append(&mut bytes, &input_digest);
        }
        crate::SemanticVersionFreshness::Historical {
            selected_input,
            latest_input,
        } => {
            bytes.push(1);
            append(&mut bytes, &selected_input);
            append(&mut bytes, &latest_input);
        }
        crate::SemanticVersionFreshness::Unverified => bytes.push(2),
    }
    append(&mut bytes, &origin.selection_root);
    if let Some(image) = origin.image {
        bytes.push(1);
        append(&mut bytes, image.image.identity.as_ref());
        bytes.extend_from_slice(&image.image.byte_len.to_be_bytes());
        append(
            &mut bytes,
            &crate::SemanticLanguageProfile::new(image.profile).to_bytes(),
        );
    } else {
        bytes.push(0);
    }
    bytes
}

/// Typed commitment to the exact selected source/image witness.
pub fn semantic_shape_source_key(
    origin: &SemanticShapeSourceOrigin,
) -> ObjectKey<crate::SemanticShapeSourceSchema> {
    ObjectKey::from_value(&semantic_shape_source_preimage(origin))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interface::SemanticImageAuthority;
    use backend_semantic::vocabulary::{JavaRelease, LanguageProfile, RustEdition};
    use backend_version::{ArtifactId, IrSemanticImageDomain, IrSemanticImageEncoding};

    fn source() -> SemanticVersionRecord {
        let coordinate = crate::PackageCoordinate::parse("pkg:cargo/shape-fixture@0.1.0")
            .expect("fixture coordinate");
        SemanticVersionRecord {
            package: crate::PackageReference::Purl(coordinate.clone()),
            coordinate,
            profile: crate::SemanticLanguageProfile::new(LanguageProfile::Rust(
                RustEdition::Rust2021,
            )),
            generation: crate::SemanticGenerationId::new([1; 32]),
            generation_root: [2; 32],
            dependency_set: [3; 32],
            manifest: [4; 32],
            artifacts: 1,
            semantic_bytes: 4096,
            complete: true,
            selected: true,
            freshness: Default::default(),
            history_status: Default::default(),
        }
    }

    fn budget() -> SemanticShapeBudget {
        SemanticShapeBudget::new(
            MAX_SEMANTIC_SHAPE_NODES as u16,
            MAX_SEMANTIC_SHAPE_BYTES as u32,
        )
        .expect("hard maxima are admitted caller limits")
    }

    fn request(names: &[&str]) -> SemanticShapeRequest {
        SemanticShapeRequest::new(
            crate::view_state_root(&[]),
            source(),
            names
                .iter()
                .map(|name| SymbolAddress::selected(crate::symbol_key(name)))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            budget(),
        )
        .expect("fixture request")
    }

    fn entry(request: &SemanticShapeRequest, index: usize) -> SemanticShapeEntry {
        let source = request.source().clone();
        SemanticShapeEntry {
            symbol: request.symbols()[index],
            identity: Some(SemanticDeclarationIdentity {
                family: [5; 16],
                variant: [6; 16],
            }),
            origin: Some(SemanticShapeSourceOrigin {
                source: SemanticShapeSelection::from_selected(&source).expect("selected source"),
                selection_root: [7; 32],
                image: Some(
                    SemanticShapeImageOrigin {
                        image:
                            SemanticImageAuthority {
                                identity: ArtifactId::<
                                    IrSemanticImageEncoding,
                                    IrSemanticImageDomain,
                                >::from_encoded_bytes(
                                    b"shape-image"
                                ),
                                byte_len: 11,
                            },
                        profile: LanguageProfile::Rust(RustEdition::Rust2021),
                    },
                ),
            }),
            fact: SemanticShapeFact::Available {
                shape: SemanticDeclarationShape::Typed(SemanticTypeFact::Known(
                    SemanticTypeExpr::Builtin(backend_semantic::ir::BuiltinType::Never),
                )),
                language: SemanticShapeLanguageFacts::CommonOnly {
                    profile: LanguageProfile::Rust(RustEdition::Rust2021),
                },
            },
        }
    }

    #[test]
    fn response_admission_preserves_exact_root_source_and_selector_order() {
        let request = request(&["shape::first", "shape::second"]);
        let batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![entry(&request, 0), entry(&request, 1)].into_boxed_slice(),
        };
        assert_eq!(batch.admit_against(&request), Ok(()));

        let mut wrong_root = batch.clone();
        wrong_root.basis =
            crate::view_state_root(&[("other".to_owned(), "root".to_owned())]).into();
        assert_eq!(
            wrong_root.admit_against(&request),
            Err(SemanticShapeError::WrongBasis)
        );

        let mut wrong_order = batch.clone();
        wrong_order.entries.as_mut().swap(0, 1);
        assert_eq!(
            wrong_order.admit_against(&request),
            Err(SemanticShapeError::WrongSymbols)
        );

        let mut wrong_address_kind = batch.clone();
        wrong_address_kind.entries[0].symbol =
            SymbolAddress::canonical(crate::symbol_key("shape::first"));
        assert_eq!(
            wrong_address_kind.admit_against(&request),
            Err(SemanticShapeError::WrongSymbols)
        );

        let mut wrong_source = batch.clone();
        wrong_source.entries[0]
            .origin
            .as_mut()
            .expect("source witness")
            .source
            .generation = crate::SemanticGenerationId::new([8; 32]);
        assert_eq!(
            wrong_source.admit_against(&request),
            Err(SemanticShapeError::InvalidOrigin)
        );
    }

    #[test]
    fn callable_bindings_preserve_unavailable_empty_and_complete_states() {
        let identity = SemanticDeclarationIdentity {
            family: [21; 16],
            variant: [22; 16],
        };
        let mut walker = SemanticShapeWalker::default();
        let captured_empty = SemanticCallableShape {
            parameters: Box::new([]),
            results: Box::new([]),
            carrier_bindings: SemanticCallableCarrierBindings::Captured {
                parameters: Box::new([]),
                results: Box::new([]),
            },
            abi: None,
            variadic: FunctionVariadicForm::None,
            unsafe_: false,
        };
        walker
            .callable(&captured_empty, 0)
            .expect("captured empty is a complete no-carrier relation");
        assert_eq!(walker.nodes, 0);

        let unavailable = SemanticCallableShape {
            carrier_bindings: SemanticCallableCarrierBindings::Unavailable,
            ..captured_empty.clone()
        };
        walker
            .callable(&unavailable, 0)
            .expect("unavailable remains a valid explicit state");
        assert_eq!(walker.nodes, 0);

        let mismatched = SemanticCallableShape {
            carrier_bindings: SemanticCallableCarrierBindings::Captured {
                parameters: vec![identity].into_boxed_slice(),
                results: Box::new([]),
            },
            ..captured_empty
        };
        assert_eq!(
            walker.callable(&mismatched, 0),
            Err(SemanticShapeError::InvalidShape),
            "captured identity arrays must align exactly with IR tuple cells"
        );
    }

    #[test]
    fn callable_binding_identities_share_the_shape_node_and_byte_budgets() {
        let identity = SemanticDeclarationIdentity {
            family: [31; 16],
            variant: [32; 16],
        };
        let element = || SemanticTypeElement {
            label: None,
            kind: TupleElementKind::Required,
            ty: SemanticTypeFact::Unavailable(SemanticTypeUnavailable::MissingImageFact),
        };
        let base = SemanticCallableShape {
            parameters: vec![element()].into_boxed_slice(),
            results: vec![element()].into_boxed_slice(),
            carrier_bindings: SemanticCallableCarrierBindings::Unavailable,
            abi: None,
            variadic: FunctionVariadicForm::None,
            unsafe_: false,
        };
        let mut without_bindings = SemanticShapeWalker::default();
        without_bindings
            .callable(&base, 0)
            .expect("unavailable relation still admits structural callable facts");

        let complete = SemanticCallableShape {
            carrier_bindings: SemanticCallableCarrierBindings::Captured {
                parameters: vec![identity].into_boxed_slice(),
                results: vec![identity].into_boxed_slice(),
            },
            ..base
        };
        let mut with_bindings = SemanticShapeWalker::default();
        with_bindings
            .callable(&complete, 0)
            .expect("complete carrier identities share callable admission budgets");
        assert_eq!(with_bindings.nodes - without_bindings.nodes, 2);
        assert_eq!(with_bindings.bytes - without_bindings.bytes, 384);

        let mut at_limit = SemanticShapeWalker {
            nodes: MAX_SEMANTIC_SHAPE_NODES,
            ..SemanticShapeWalker::default()
        };
        assert_eq!(
            at_limit.callable(&complete, 0),
            Err(SemanticShapeError::OutputBound),
            "binding nodes are rejected before the callable's tuple walk"
        );
    }

    #[test]
    fn shared_product_walk_counts_nested_facts_and_collects_nominal_selectors_once() {
        let request = request(&["shape::walk"]);
        let nominal = SemanticTypeFact::Known(SemanticTypeExpr::Nominal {
            declaration: SemanticDeclarationIdentity {
                family: [11; 16],
                variant: [12; 16],
            },
            symbol: Some(SymbolAddress::selected(crate::symbol_key("shape::Node"))),
        });
        let batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![SemanticShapeEntry {
                symbol: request.symbols()[0],
                identity: Some(SemanticDeclarationIdentity {
                    family: [13; 16],
                    variant: [14; 16],
                }),
                origin: entry(&request, 0).origin,
                fact: SemanticShapeFact::Available {
                    shape: SemanticDeclarationShape::Typed(SemanticTypeFact::Known(
                        SemanticTypeExpr::Applied {
                            constructor: Box::new(nominal.clone()),
                            arguments: vec![nominal].into_boxed_slice(),
                        },
                    )),
                    language: SemanticShapeLanguageFacts::CommonOnly {
                        profile: LanguageProfile::Rust(RustEdition::Rust2021),
                    },
                },
            }]
            .into_boxed_slice(),
        };

        let summary = batch
            .admit_with_summary(&request)
            .expect("product walk and request admission");
        assert!(summary.nodes() > 5);
        assert!(summary.bytes() > 256);
        assert_eq!(
            summary.nominal_symbols(),
            &[SymbolAddress::selected(crate::symbol_key("shape::Node"))]
        );
    }

    #[test]
    fn source_witness_commits_freshness_but_excludes_history_sidecar() {
        let original = source();
        let original_selection =
            SemanticShapeSelection::from_selected(&original).expect("selected source");

        let mut changed_freshness = original.clone();
        changed_freshness.freshness = crate::SemanticVersionFreshness::Current {
            input_digest: [9; 32],
        };
        let changed_freshness_selection = SemanticShapeSelection::from_selected(&changed_freshness)
            .expect("selected source with a different freshness observation");
        assert_ne!(original_selection, changed_freshness_selection);

        let mut changed_history = original;
        changed_history.history_status = crate::SemanticHistoryPublicationStatus::Refused {
            selection_id: [8; 32],
            reason: "derived history unavailable".to_owned(),
        };
        let changed_history_selection = SemanticShapeSelection::from_selected(&changed_history)
            .expect("selected source with a changed history sidecar");
        assert_eq!(original_selection, changed_history_selection);

        let original_request = request(&["shape::sidecar"]);
        let batch = SemanticShapeBatch {
            basis: original_request.basis(),
            entries: vec![entry(&original_request, 0)].into_boxed_slice(),
        };
        let changed_history_request = SemanticShapeRequest::new(
            crate::view_state_root(&[]),
            changed_history,
            original_request.symbols.clone(),
            original_request.budget,
        )
        .expect("history sidecar does not change selected shape authority");
        assert_eq!(batch.admit_against(&changed_history_request), Ok(()));
    }

    #[test]
    fn response_admission_rejects_a_different_source_freshness() {
        let request = request(&["shape::freshness"]);
        let mut entry = entry(&request, 0);
        entry
            .origin
            .as_mut()
            .expect("source witness")
            .source
            .freshness = crate::SemanticVersionFreshness::Current {
            input_digest: [9; 32],
        };
        let batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![entry].into_boxed_slice(),
        };
        assert_eq!(
            batch.admit_against(&request),
            Err(SemanticShapeError::InvalidOrigin)
        );
    }

    #[test]
    fn response_admission_rejects_language_facts_for_another_profile() {
        let request = request(&["shape::language"]);
        let mut entry = entry(&request, 0);
        let SemanticShapeFact::Available { shape, .. } = entry.fact else {
            panic!("fixture entry is available");
        };
        entry.fact = SemanticShapeFact::Available {
            shape,
            language: SemanticShapeLanguageFacts::Partial {
                profile: LanguageProfile::Java(JavaRelease::Java17),
                facts: SemanticShapeLanguageFact::Java {
                    throws: Box::new([]),
                    annotations: Box::new([]),
                },
            },
        };
        let batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![entry].into_boxed_slice(),
        };
        assert_eq!(
            batch.admit_against(&request),
            Err(SemanticShapeError::InvalidShape)
        );
    }

    #[test]
    fn positive_and_selection_level_unavailable_facts_need_distinct_provenance() {
        let request = request(&["shape::selected"]);
        let valid_entry = entry(&request, 0);
        let mut detached = valid_entry.clone();
        detached.origin.as_mut().expect("origin").image = None;
        let detached_batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![detached].into_boxed_slice(),
        };
        assert_eq!(
            detached_batch.admit_against(&request),
            Err(SemanticShapeError::InvalidOrigin)
        );

        let mut unavailable = valid_entry;
        unavailable.identity = None;
        unavailable.origin.as_mut().expect("origin").image = None;
        unavailable.fact =
            SemanticShapeFact::Unavailable(SemanticShapeUnavailable::NoSelectedImage);
        let unavailable_batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![unavailable].into_boxed_slice(),
        };
        assert_eq!(unavailable_batch.admit_against(&request), Ok(()));

        let absent = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![SemanticShapeEntry {
                symbol: request.symbols()[0],
                identity: None,
                origin: None,
                fact: SemanticShapeFact::Unavailable(SemanticShapeUnavailable::NotInView),
            }]
            .into_boxed_slice(),
        };
        assert_eq!(absent.admit_against(&request), Ok(()));
    }

    #[test]
    fn response_walk_rejects_a_type_graph_deeper_than_the_shared_limit() {
        let request = request(&["shape::deep"]);
        let mut ty = SemanticTypeFact::Known(SemanticTypeExpr::Builtin(
            backend_semantic::ir::BuiltinType::Never,
        ));
        for _ in 0..=MAX_SEMANTIC_SHAPE_DEPTH {
            ty = SemanticTypeFact::Known(SemanticTypeExpr::Optional(Box::new(ty)));
        }
        let mut deep = entry(&request, 0);
        deep.fact = SemanticShapeFact::Available {
            shape: SemanticDeclarationShape::Typed(ty),
            language: SemanticShapeLanguageFacts::CommonOnly {
                profile: LanguageProfile::Rust(RustEdition::Rust2021),
            },
        };
        let batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![deep].into_boxed_slice(),
        };
        assert_eq!(
            batch.admit_against(&request),
            Err(SemanticShapeError::OutputBound)
        );
    }

    #[test]
    fn response_walk_counts_language_annotation_nodes() {
        let request = request(&["shape::annotations"]);
        let mut entry = entry(&request, 0);
        entry.fact = SemanticShapeFact::Available {
            shape: SemanticDeclarationShape::Typed(SemanticTypeFact::Unavailable(
                SemanticTypeUnavailable::MissingImageFact,
            )),
            language: SemanticShapeLanguageFacts::Partial {
                profile: LanguageProfile::Java(JavaRelease::Java17),
                facts: SemanticShapeLanguageFact::Java {
                    throws: Vec::<SemanticTypeFact>::new().into_boxed_slice(),
                    annotations: vec![
                        SourceAtomText::new("A").expect("exact annotation text");
                        MAX_SEMANTIC_SHAPE_NODES
                    ]
                    .into_boxed_slice(),
                },
            },
        };
        let batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![entry].into_boxed_slice(),
        };
        assert_eq!(
            batch.admit_against(&request),
            Err(SemanticShapeError::OutputBound)
        );
    }

    #[test]
    fn product_walk_preserves_whitespace_in_exact_compiler_text() {
        let request = request(&["shape::text"]);
        let mut entry = entry(&request, 0);
        entry.fact = SemanticShapeFact::Available {
            shape: SemanticDeclarationShape::Typed(SemanticTypeFact::Known(
                SemanticTypeExpr::Parameter(
                    SourceAtomText::new(" T ").expect("exact compiler parameter spelling"),
                ),
            )),
            language: SemanticShapeLanguageFacts::CommonOnly {
                profile: LanguageProfile::Rust(RustEdition::Rust2021),
            },
        };
        let batch = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![entry].into_boxed_slice(),
        };
        assert_eq!(batch.admission_summary().map(|_| ()), Ok(()));
    }

    #[test]
    fn public_product_summary_checks_the_batch_bound_before_traversal() {
        let request = request(&["shape::bounded"]);
        let entry = entry(&request, 0);
        let empty = SemanticShapeBatch {
            basis: request.basis(),
            entries: Box::new([]),
        };
        assert_eq!(
            empty.admission_summary(),
            Err(SemanticShapeError::BatchBound)
        );

        let oversized = SemanticShapeBatch {
            basis: request.basis(),
            entries: vec![entry; MAX_SEMANTIC_SHAPE_BATCH + 1].into_boxed_slice(),
        };
        assert_eq!(
            oversized.admission_summary(),
            Err(SemanticShapeError::BatchBound)
        );
    }
}

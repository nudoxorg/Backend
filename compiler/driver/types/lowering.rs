//! Exact facts rejected while entering the bounded compiler emission lane.
//!
//! The fault and its snapshot are value types on purpose: the shared compile
//! terminal outlives every collector arena, so the rejection crosses the
//! driver boundary by copy (ordinal, name length, and the full typed cause),
//! never by borrow. Name bytes stay inspectable at the collector boundary
//! that produced them.

use compiler_ir::{EntityId, ProductChildRole, ProductConstructorFault, SemanticTypeFault};
use compiler_languages_clang::{
    DeclarationId as ClangDeclarationId, SourceSpan as ClangSourceSpan, SymbolIdentity,
    TypeId as ClangTypeId, TypeKind as ClangTypeKind, TypeQualifiers as ClangTypeQualifiers,
};
use compiler_languages_csharp::{ImageError, TypeRef as CSharpTypeRef};

/// One closed authority-backed containment state for an emitted entity.
///
/// The staging lane may receive the same proven relationship through several
/// passes, but contradictory authority claims must remain a typed failure
/// rather than silently replacing an earlier relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParentageState {
    /// The authority did not expose containment for this entity.
    Unavailable,
    /// The authority proved the entity has no local parent.
    Root,
    /// The authority proved one already-emitted local parent.
    Bound {
        /// Durable ordinal of the local parent declaration.
        parent: EntityId,
    },
    /// The authority proved an owner whose declaration has no emitted row.
    UnrepresentedAuthorityOwner {
        /// Opaque native identity retained without fabricating a local parent.
        identity: [u8; 16],
    },
}

/// One authority-proven half-open span in the entered primary source.
///
/// This is a compact provenance fact rather than a durable wire coordinate:
/// the owned and compact projections bind their request-local source file
/// only after the authority span has been admitted. Its fields are public so
/// an exact conflict diagnostic carries both observed facts without getters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceSpanFact {
    /// Inclusive starting byte offset in the entered primary source.
    pub start: u32,
    /// Exclusive ending byte offset in the entered primary source.
    pub end: u32,
}

/// Closed pooled type-child arena whose measured request capacity was
/// exhausted. Per-row cardinality remains a separate `TypeChildCapacity`
/// fault, so callers can distinguish protocol shape from resource demand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TypeChildLane {
    Declared,
    Anonymous,
    Computed,
}

impl SourceSpanFact {
    /// Constructs only a well-ordered half-open source span.
    #[must_use]
    pub const fn new(start: u32, end: u32) -> Option<Self> {
        if start <= end {
            Some(Self { start, end })
        } else {
            None
        }
    }
}

/// Exact cause for rejecting one emitted fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FactFault {
    /// The emitted declaration name is empty.
    EmptyName,
    /// The bounded fact lane is full.
    Capacity,
    /// The fact's product child lane is full.
    ChildCapacity,
    /// The request-level flat product-child arena is exhausted.
    ProductChildPoolCapacity {
        used: usize,
        requested: usize,
        capacity: usize,
    },
    /// The constructor payload disagrees with its child count.
    Constructor(ProductConstructorFault),
    /// A child role disagrees with the constructor's closed role lane.
    ChildRole {
        /// Ordered position of the offending child.
        position: usize,
        /// Role demanded by the constructor's closed lane.
        expected: ProductChildRole,
        /// Role the fact actually carried.
        actual: ProductChildRole,
    },
    /// A child targets a fact outside the already-pushed set.
    ChildTarget {
        /// Ordered position of the offending child.
        position: usize,
        /// Target ordinal the fact named.
        target: u32,
        /// Ordinals admitted before this fact.
        fact_count: usize,
    },
    /// The declared type record violates the closed lattice.
    TypeRecord(SemanticTypeFault),
    /// One type-record child violates its tag's closed child law.
    TypeChild {
        /// Ordered position of the offending child.
        position: usize,
        /// Exact closed-lattice fault.
        fault: SemanticTypeFault,
    },
    /// A type-record child targets a fact outside the already-pushed set.
    TypeChildTarget {
        /// Ordered position of the offending child.
        position: usize,
        /// Target ordinal the child named.
        target: u32,
        /// Ordinals admitted before this fact.
        fact_count: usize,
    },
    /// The type-record child lane is full.
    TypeChildCapacity,
    /// One request-level flat type-child arena is exhausted.
    TypeChildPoolCapacity {
        lane: TypeChildLane,
        used: usize,
        requested: usize,
        capacity: usize,
    },
    /// The anonymous type-row pool is full.
    TypeRowCapacity,
    /// The computed type-row lane is full.
    ComputedRowCapacity,
    /// An occurrence names an owner outside the pushed prefix.
    OccurrenceOwner {
        /// Owner ordinal the occurrence named.
        owner: u32,
        /// Ordinals admitted before this fact.
        fact_count: usize,
    },
    /// The occurrence lane is full.
    OccurrenceCapacity,
    /// A doc fragment names an owner outside the pushed prefix.
    DocOwner {
        /// Owner ordinal the fragment named.
        owner: u32,
        /// Ordinals admitted before this fact.
        fact_count: usize,
    },
    /// The documentation lane is full.
    DocCapacity,
    /// The extension-atom lane is full.
    ExtensionAtomCapacity,
    /// The type-parameter lane is full.
    TypeParameterCapacity,
    /// The ordered type/lifetime bound lane is full.
    TypeParameterBoundCapacity { requested: usize, available: usize },
    /// A pooled reference lane is full.
    RefListCapacity,
    /// A pooled reference list has too many elements.
    RefListElements,
    /// A pooled reference targets a fact outside the pushed prefix.
    RefTarget {
        /// Closed lane name the reference targeted.
        lane: &'static str,
        /// Raw ordinal the reference named.
        raw: u32,
        /// Ordinals admitted in that lane.
        fact_count: usize,
    },
    /// An authority span escaped the entered primary-source lease.  A
    /// foreign-file coordinate requires its own typed provenance instead of
    /// being silently treated as a primary-source offset.
    SourceSpan {
        /// Entity whose provenance row was being attached.
        entity: u32,
        /// Observed half-open source range.
        start: u32,
        end: u32,
        /// Exact entered primary source length.
        source_len: u32,
    },
    /// Two authority passes supplied incompatible primary-source spans for
    /// one emitted entity. Repeating the same source fact is idempotent.
    ConflictingSourceSpan {
        /// Entity whose source fact was already established.
        entity: EntityId,
        /// Previously retained authority span.
        existing: SourceSpanFact,
        /// New incompatible authority span.
        requested: SourceSpanFact,
    },
    /// Two authority passes made incompatible containment claims for one
    /// emitted entity. Repeating an identical proved state is idempotent.
    ConflictingParentage {
        /// Entity whose staging relation was already established.
        entity: EntityId,
        /// Previously retained closed parentage state.
        existing: ParentageState,
        /// New incompatible authority claim.
        requested: ParentageState,
    },
}

/// Exact rejection of one emitted fact, retained by value at the shared
/// compile terminal. The fact ordinal, the rejected name's exact byte
/// length, and the full typed cause survive every boundary; the name bytes
/// themselves stay at the collector that borrowed them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FactRejection {
    /// Zero-based ordinal the fact would have occupied.
    pub fact: usize,
    /// Exact byte length of the rejected declaration name.
    pub name_len: usize,
    /// Full typed rejection cause with every operand.
    pub cause: FactFault,
}

/// Exact C# projection failure retained across the compile terminal.
#[derive(Debug)]
pub enum CSharpProjectionFault {
    /// The validated authority image rejected one row while it was lent.
    Image(ImageError),
    /// The recursive authority type graph exceeded its projection budget.
    Depth,
    /// A declaration name span did not name the bound source bytes.
    NameSpan { start: u32, end: u32 },
    /// A reference preceded its owning declaration.
    OwnerOrder {
        owner_start: u32,
        reference_start: u32,
    },
    /// A foreign occurrence key could not be built from its spelling.
    Foreign,
    /// The bounded attribute lane exceeded its element capacity.
    AttributeCapacity { spellings: usize },
    /// A projection index exceeded its representable width.
    IndexCapacity,
    /// One rectangular-array authority row repeated non-identical element
    /// coordinates.  Rank is a repetition of one element type, never a
    /// lossy selection of the first child.
    HeterogeneousArrayRank {
        /// The first repeated element coordinate.
        first: CSharpTypeRef,
        /// The distinct coordinate observed later in the same rank row.
        observed: CSharpTypeRef,
    },
    /// Canonical fact admission rejected the projected fact.
    Fact(FactFault),
}

/// Exact Clang projection failure retained across the compile terminal.
///
/// These are authority coordinates, not compact emission coordinates: the
/// driver reports precisely what libclang supplied rather than restating a
/// rejected native fact as an unsupported language declaration.
#[derive(Debug)]
pub enum ClangProjectionFault {
    /// An authority source span escaped the entered source lease.
    Span { span: ClangSourceSpan },
    /// A declaration requiring a name had no nonempty authority name span.
    Nameless { declaration: ClangDeclarationId },
    /// An anonymous authority type had no representable owning declaration.
    Anchor,
    /// An authority coordinate could not fit the bounded projection index.
    IndexCapacity,
    /// An override named a foreign native identity with no exact public key.
    ForeignOverride { identity: SymbolIdentity },
    /// A reference named a foreign native identity with no exact public key.
    ///
    /// Its source spelling is presentation, not identity: overloaded native
    /// declarations may share it. Until the authority schema carries the
    /// complete native key, lowering stops with the supplied identity rather
    /// than manufacturing a foreign path from use-site text.
    ForeignReference { identity: SymbolIdentity },
    /// Direct `const`/`volatile`/`restrict` facts named a native shape on
    /// which those qualifiers are not semantically legal. The full authority
    /// row operands remain inspectable; no qualifier is silently relocated.
    IllegalQualifierTarget {
        /// Qualified native type row.
        type_id: ClangTypeId,
        /// Native type form directly qualified.
        kind: ClangTypeKind,
        /// Exact native qualifier set.
        qualifiers: ClangTypeQualifiers,
    },
    /// A C++ member pointer named a non-class owner type. A missing or
    /// malformed owner is not a root and must not degrade to an ordinary
    /// pointer.
    IllegalMemberPointerOwner {
        /// Native member-pointer row.
        pointer: ClangTypeId,
        /// Native owner row supplied by libclang.
        owner: ClangTypeId,
        /// Owner's direct native type kind.
        kind: ClangTypeKind,
        /// Native declaration identity attached to the owner row, if any.
        declaration: Option<SymbolIdentity>,
    },
}

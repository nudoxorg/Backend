use super::ids::{LinkOccurrenceId, TreeEntityId};
use super::language_facts::SemanticImageAuthority;
use super::packed_types::{
    CallableElementRole, ConcreteType, TupleElementKind, TypeParameterRequirements,
};
use super::relations::{ExternalTarget, LinkTarget, SourceSpan};
use crate::ir::{
    AtomId, AuthorityFactFault, CapacityError, DeclarationIdentity, DenseId, EntityId,
    ImageProvenanceClaim, PreimageOverflow, SourceIdentity, TextId, TypeId, VariantFingerprint,
};
use crate::vocabulary::{CompileRecipeFact, Language, LanguageProfile};
use core::fmt;

/// Range assigned to one borrowed tree in the condensed entity arena.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityRange {
    pub(super) start: EntityId,
    pub(super) len: u32,
}

impl EntityRange {
    /// Maps a tree-local ID to its final condensed ID.
    #[must_use]
    pub fn get(self, local: TreeEntityId) -> Option<EntityId> {
        (local.raw < self.len).then(|| EntityId::new(self.start.raw + local.raw))
    }
    #[must_use]
    pub const fn start(self) -> EntityId {
        self.start
    }
    #[must_use]
    pub const fn len(self) -> u32 {
        self.len
    }
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Coordinate class used in structural validation errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticSpace {
    Atom,
    Text,
    Type,
    Entity,
    External,
    Link,
    LinkOccurrence,
    TypeList,
    EntityList,
    AtomList,
    Docs,
    TupleElements,
    ObjectMembers,
    TemplateParts,
    TypeParameterBounds,
    TypeParameters,
    FreePredicates,
}

/// Closed semantic fault in a language-owned extension row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LanguageExtensionViolation {
    /// A TypeScript computed ID did not point to a computed type state.
    ComputedType,
    /// A Clang layout alignment claimed an impossible zero-bit alignment.
    ZeroLayoutAlignment,
    /// A transaction-local sparse binding escaped the exact typed fact pool
    /// measured for its one language.  Both coordinates are retained so an
    /// internal projection defect cannot be disguised as an absent fact.
    MissingPoolFact { fact: usize, count: usize },
}

/// Failure while condensing or validating frontend IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildError {
    Capacity(CapacityError),
    InvalidTreeEntity {
        raw: u32,
        count: u32,
    },
    Dangling {
        space: SemanticSpace,
        raw: u32,
    },
    /// A recursive compound type reached a row still under projection.  A
    /// recursive nominal is representable as its terminal nominal row; a
    /// compound cycle needs a dedicated recursive handle and must never
    /// recurse on the process stack while that handle is absent.
    RecursiveType {
        raw: u32,
    },
    /// A callable element used a modifier illegal for its semantic role or
    /// variadic form. Results are always required; a typed variadic tail is
    /// exactly one final parameter.
    CallableElement {
        role: CallableElementRole,
        position: usize,
        kind: TupleElementKind,
    },
    /// A callable claimed a typed variadic tail but had no final `Rest`
    /// parameter element to own it.
    MissingTypedVariadicParameter {
        parameter_count: usize,
    },
    /// A qualified type path had no named segments after its self/trait base.
    EmptyQualifiedPath,
    /// A publicly constructible generic requirement set combined mutually
    /// exclusive C# primary constraints with `new()`.
    TypeParameterRequirements {
        list: u32,
        position: u32,
        requirements: TypeParameterRequirements,
    },
    /// A direct C-family qualifier wrapper was constructed with no qualifier.
    /// Empty qualification has no source-semantic node and must be omitted.
    EmptyCxxQualification,
    /// A direct C-family qualifier wrapper named a structural target on
    /// which its exact native qualifiers are illegal. The wrapper is never
    /// silently moved to a pointee or referent.
    IllegalCQualifierTarget {
        target: TypeId,
        qualifiers: crate::ir::CvQualifiers,
    },
    /// A C++ member pointer's first operand was not a record/class nominal
    /// (or an application of one). Such a pair cannot be rendered truthfully
    /// as `Member Owner::*`.
    IllegalCxxMemberPointerOwner {
        owner: TypeId,
    },
    /// A durable documentation fact was not valid UTF-8, so it cannot enter
    /// the owned text arena without loss.  Callers must retain it in the
    /// compact fragment or surface this exact terminal; silently dropping it
    /// would split render truth from durable truth.
    InvalidDocumentationUtf8 {
        bytes: usize,
    },
    /// A relative occurrence span escaped its authority-captured owner span.
    InvalidOccurrenceSpan {
        owner: EntityId,
        start: u32,
        end: u32,
    },
    /// Parentage formed a cycle, so no stable qualified ownership key exists.
    ParentCycle {
        entity: EntityId,
    },
    /// One source declaration could not form its validated package/path/kind
    /// identity key.  The entity coordinate and original vocabulary fault are
    /// retained instead of being reclassified as a dangling reference.
    DeclarationKey {
        entity: EntityId,
        cause: crate::ir::DeclarationKeyFault,
    },
    /// The central scoped declaration-key writer rejected its exact framed
    /// preimage.  This preserves profile/parentage/collision-width causes.
    ScopedDeclarationPreimage {
        entity: EntityId,
        cause: crate::ir::PreimageOverflow,
    },
    /// A foreign-key preimage could not be measured or written. The owner
    /// and compact foreign-path digest retain the exact failing endpoint.
    ForeignKeyPreimage {
        owner: EntityId,
        target: VariantFingerprint,
        cause: crate::ir::PreimageOverflow,
    },
    DuplicateDeclarationIdentity {
        identity: DeclarationIdentity,
    },
    TreeVersionCount {
        versions: usize,
        items: usize,
    },
    /// The cold semantic-authority lanes no longer matched the owned entity
    /// row count. This is an internal admission invariant, retained as an
    /// exact terminal instead of truncating or padding authority truth.
    AuthorityRowCount {
        entities: usize,
        authority_rows: usize,
    },
    /// The cold occurrence-authority lane no longer matched observed graph
    /// occurrence rows.
    OccurrenceAuthorityRowCount {
        occurrences: usize,
        authority_rows: usize,
    },
    /// One authority row contradicted its corresponding owned entity facts.
    AuthorityFacts {
        entity: EntityId,
        cause: AuthorityFactFault,
    },
    /// One authority row contradicted its corresponding graph occurrence.
    OccurrenceAuthorityFacts {
        occurrence: LinkOccurrenceId,
        cause: AuthorityFactFault,
    },
    /// An image provenance scope could not form the same validated
    /// package/path declaration key required by every entity family.
    ImageProvenanceLineage {
        cause: crate::ir::PackageLineageFault,
    },
    /// An image provenance scope could not form the same validated
    /// package/path declaration key required by every entity family.
    ImageProvenanceScope {
        cause: crate::ir::DeclarationKeyFault,
    },
    /// The validated image scope could not allocate or write its canonical
    /// declaration-key claim.
    ImageProvenanceScopePreimage {
        cause: PreimageOverflow,
    },
    /// The image header's recipe was not derived from its exact source and
    /// advertised recipe facts.
    ImageProvenanceRecipe {
        source: SourceIdentity,
        recipe: CompileRecipeFact,
    },
    /// A second image provenance claim differed from the already-bound
    /// header. Equal claims are idempotent; neither source nor recipe truth
    /// is silently overwritten.
    ImageProvenanceRebind {
        existing: ImageProvenanceClaim,
        requested: ImageProvenanceClaim,
    },
    LanguageExtension {
        language: Language,
        entity: EntityId,
        violation: LanguageExtensionViolation,
    },
    LanguageProfileMismatch {
        authority: SemanticImageAuthority,
        extension: Language,
    },
    LanguageProfileRebind {
        existing: SemanticImageAuthority,
        requested: LanguageProfile,
    },
}

impl From<CapacityError> for BuildError {
    fn from(value: CapacityError) -> Self {
        Self::Capacity(value)
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Capacity(error) => error.fmt(formatter),
            Self::InvalidTreeEntity { raw, count } => {
                write!(formatter, "tree entity {raw} is outside tree size {count}")
            }
            Self::Dangling { space, raw } => {
                write!(formatter, "{space:?} coordinate {raw} is dangling")
            }
            Self::RecursiveType { raw } => {
                write!(
                    formatter,
                    "compound type coordinate {raw} is recursively projected"
                )
            }
            Self::CallableElement {
                role,
                position,
                kind,
            } => write!(
                formatter,
                "callable {role:?} element at position {position} has illegal {kind:?} form"
            ),
            Self::MissingTypedVariadicParameter { parameter_count } => write!(
                formatter,
                "typed variadic callable has {parameter_count} parameters but no final rest parameter"
            ),
            Self::TypeParameterRequirements {
                list,
                position,
                requirements,
            } => write!(
                formatter,
                "type-parameter list {list} position {position} has inconsistent requirements {requirements:?}"
            ),
            Self::EmptyQualifiedPath => formatter.write_str("qualified type path has no segments"),
            Self::EmptyCxxQualification => {
                formatter.write_str("C-family qualifier wrapper is empty")
            }
            Self::IllegalCQualifierTarget { target, qualifiers } => write!(
                formatter,
                "C-family qualifiers {qualifiers:?} are illegal on type {}",
                target.raw
            ),
            Self::IllegalCxxMemberPointerOwner { owner } => write!(
                formatter,
                "C++ member pointer owner type {} is not a record nominal",
                owner.raw
            ),
            Self::InvalidDocumentationUtf8 { bytes } => {
                write!(
                    formatter,
                    "documentation fact has {bytes} invalid UTF-8 bytes"
                )
            }
            Self::InvalidOccurrenceSpan { owner, start, end } => {
                write!(
                    formatter,
                    "occurrence span {start}..{end} escapes entity {}",
                    owner.raw
                )
            }
            Self::ParentCycle { entity } => {
                write!(
                    formatter,
                    "entity {} participates in a parent cycle",
                    entity.raw
                )
            }
            Self::DeclarationKey { entity, cause } => {
                write!(
                    formatter,
                    "entity {} has an invalid declaration key: {cause:?}",
                    entity.raw
                )
            }
            Self::ScopedDeclarationPreimage { entity, cause } => write!(
                formatter,
                "entity {} has an invalid scoped declaration preimage: {cause:?}",
                entity.raw
            ),
            Self::ForeignKeyPreimage {
                owner,
                target,
                cause,
            } => write!(
                formatter,
                "entity {} has an invalid foreign key preimage for {:02x?}: {cause:?}",
                owner.raw,
                target.as_bytes()
            ),
            Self::DuplicateDeclarationIdentity { identity } => {
                write!(
                    formatter,
                    "declaration family {:02x?} variant {:02x?} is duplicated",
                    identity.family.as_bytes(),
                    identity.variant.as_bytes()
                )
            }
            Self::TreeVersionCount { versions, items } => {
                write!(
                    formatter,
                    "borrowed tree has {versions} versions for {items} items"
                )
            }
            Self::AuthorityRowCount {
                entities,
                authority_rows,
            } => write!(
                formatter,
                "semantic authority has {authority_rows} rows for {entities} entities"
            ),
            Self::OccurrenceAuthorityRowCount {
                occurrences,
                authority_rows,
            } => write!(
                formatter,
                "occurrence authority has {authority_rows} rows for {occurrences} occurrences"
            ),
            Self::AuthorityFacts { entity, cause } => write!(
                formatter,
                "semantic authority for entity {} is inconsistent: {cause:?}",
                entity.raw
            ),
            Self::OccurrenceAuthorityFacts { occurrence, cause } => write!(
                formatter,
                "semantic authority for occurrence {} is inconsistent: {cause:?}",
                occurrence.raw
            ),
            Self::ImageProvenanceScope { cause } => {
                write!(
                    formatter,
                    "semantic image provenance scope is invalid: {cause:?}"
                )
            }
            Self::ImageProvenanceLineage { cause } => write!(
                formatter,
                "semantic image provenance package lineage is invalid: {cause:?}"
            ),
            Self::ImageProvenanceScopePreimage { cause } => write!(
                formatter,
                "semantic image provenance scope preimage is invalid: {cause:?}"
            ),
            Self::ImageProvenanceRecipe { source, recipe } => write!(
                formatter,
                "semantic image recipe {recipe:?} does not bind source {source:?}"
            ),
            Self::ImageProvenanceRebind {
                existing,
                requested,
            } => write!(
                formatter,
                "semantic image provenance {requested:?} cannot replace {existing:?}"
            ),
            Self::LanguageExtension {
                language,
                entity,
                violation,
            } => {
                write!(
                    formatter,
                    "{language:?} extension for entity {} violates {violation:?}",
                    entity.raw
                )
            }
            Self::LanguageProfileMismatch {
                authority,
                extension,
            } => write!(
                formatter,
                "{extension:?} extension conflicts with image authority {authority:?}"
            ),
            Self::LanguageProfileRebind {
                existing,
                requested,
            } => write!(
                formatter,
                "language profile {requested:?} cannot replace {existing:?}"
            ),
        }
    }
}

impl core::error::Error for BuildError {}

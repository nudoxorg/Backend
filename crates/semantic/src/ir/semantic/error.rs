use super::ids::{LinkOccurrenceId, TreeEntityId};
use super::language_facts::SemanticImageAuthority;
use super::packed_types::{
    CallableElementRole, TupleElementKind, TypeParameterRequirements,
};
use crate::ir::{
    AuthorityFactFault, CapacityError, DeclarationIdentity, EntityId,
    ImageProvenanceClaim, PreimageOverflow, ProductChildRole, SignatureCarrierBindingRole,
    SourceIdentity, TypeId, VariantFingerprint,
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
    /// First global entity coordinate assigned to this tree.
    #[must_use]
    pub const fn start(self) -> EntityId {
        self.start
    }
    /// Number of consecutive entity coordinates reserved for this tree.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.len
    }
    /// Whether this tree was admitted with no entity rows.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Coordinate class used in structural validation errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticSpace {
    /// Coordinates in the byte-string atom arena.
    Atom,
    /// Coordinates in the UTF-8 text arena.
    Text,
    /// Coordinates in the typed semantic type arena.
    Type,
    /// Coordinates in the entity arena.
    Entity,
    /// Coordinates in the cross-fragment external-target pool.
    External,
    /// Coordinates in the deduplicated graph relation pool.
    Link,
    /// Coordinates in the graph use-site occurrence pool.
    LinkOccurrence,
    /// Coordinates in the interned type-list pool.
    TypeList,
    /// Coordinates in the interned entity-list pool.
    EntityList,
    /// Coordinates in the interned atom-list pool.
    AtomList,
    /// Coordinates in the interned documentation-fragment pool.
    Docs,
    /// Coordinates in the tuple-element list pool.
    TupleElements,
    /// Coordinates in the object-member list pool.
    ObjectMembers,
    /// Coordinates in the template-literal part list pool.
    TemplateParts,
    /// Coordinates in the type-parameter-bound list pool.
    TypeParameterBounds,
    /// Coordinates in the type-parameter list pool.
    TypeParameters,
    /// Coordinates in the free-predicate list pool.
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
    MissingPoolFact {
        /// Zero-based fact row requested from the typed pool.
        fact: usize,
        /// Number of rows reserved in that pool for this transaction.
        count: usize,
    },
}

/// Failure while condensing or validating frontend IR.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuildError {
    /// A builder operation exceeded a premeasured storage capacity.
    Capacity(CapacityError),
    /// A tree-local entity ID is outside the borrowed tree's entity rows.
    InvalidTreeEntity {
        /// Raw tree-local coordinate supplied by the input.
        raw: u32,
        /// Number of entity rows in the borrowed tree.
        count: u32,
    },
    /// An ID refers past the end of the indicated semantic pool.
    Dangling {
        /// ID namespace in which validation found the bad coordinate.
        space: SemanticSpace,
        /// Raw pool-local coordinate supplied by the input.
        raw: u32,
    },
    /// A recursive compound type reached a row still under projection.  A
    /// recursive nominal is representable as its terminal nominal row; a
    /// compound cycle needs a dedicated recursive handle and must never
    /// recurse on the process stack while that handle is absent.
    RecursiveType {
        /// Raw type coordinate whose compound projection encountered itself.
        raw: u32,
    },
    /// A callable element used a modifier illegal for its semantic role or
    /// variadic form. Results are always required; a typed variadic tail is
    /// exactly one final parameter.
    CallableElement {
        /// Whether the invalid element belongs to parameters or results.
        role: CallableElementRole,
        /// Zero-based position within that ordered callable lane.
        position: usize,
        /// Tuple-element modifier rejected for this slot.
        kind: TupleElementKind,
    },
    /// A callable claimed a typed variadic tail but had no final `Rest`
    /// parameter element to own it.
    MissingTypedVariadicParameter {
        /// Number of parameter elements present when the required final rest element was absent.
        parameter_count: usize,
    },
    /// A qualified type path had no named segments after its self/trait base.
    EmptyQualifiedPath,
    /// A publicly constructible generic requirement set combined mutually
    /// exclusive C# primary constraints with `new()`.
    TypeParameterRequirements {
        /// Coordinate of the interned type-parameter list.
        list: u32,
        /// Zero-based parameter position with contradictory constraints.
        position: u32,
        /// Constraint set that could not be admitted.
        requirements: TypeParameterRequirements,
    },
    /// A direct C-family qualifier wrapper was constructed with no qualifier.
    /// Empty qualification has no source-semantic node and must be omitted.
    EmptyCxxQualification,
    /// A direct C-family qualifier wrapper named a structural target on
    /// which its exact native qualifiers are illegal. The wrapper is never
    /// silently moved to a pointee or referent.
    IllegalCQualifierTarget {
        /// Type coordinate directly wrapped by the C-family qualifier node.
        target: TypeId,
        /// Native qualifier bits rejected for that target shape.
        qualifiers: crate::ir::CvQualifiers,
    },
    /// A C++ member pointer's first operand was not a record/class nominal
    /// (or an application of one). Such a pair cannot be rendered truthfully
    /// as `Member Owner::*`.
    IllegalCxxMemberPointerOwner {
        /// Type coordinate used as the member-pointer owner operand.
        owner: TypeId,
    },
    /// A durable documentation fact was not valid UTF-8, so it cannot enter
    /// the owned text arena without loss.  Callers must retain it in the
    /// compact fragment or surface this exact terminal; silently dropping it
    /// would split render truth from durable truth.
    InvalidDocumentationUtf8 {
        /// Number of bytes in the rejected documentation fragment.
        bytes: usize,
    },
    /// An occurrence span overflowed while rebasing or escaped its captured owner span.
    InvalidOccurrenceSpan {
        /// Entity whose captured source range bounds this occurrence.
        owner: EntityId,
        /// Start offset: relative if rebasing overflowed, absolute if containment failed.
        start: u32,
        /// Exclusive end offset: relative if rebasing overflowed, absolute if containment failed.
        end: u32,
    },
    /// A complete signature-carrier role plane did not align with the exact
    /// committed entity row count.
    SignatureCarrierRoleCount {
        /// Entity-row count the captured role plane must match.
        expected: usize,
        /// Number of role values supplied by the caller.
        observed: usize,
    },
    /// Signature-carrier role evidence was attached to a non-parameter row.
    SignatureCarrierRoleKind {
        /// Entity assigned an invalid signature-carrier role.
        entity: EntityId,
        /// Actual declaration kind at that entity coordinate.
        kind: crate::ir::ItemKind,
    },
    /// A function-signature product edge was attached to a non-function owner.
    SignatureCarrierRoleOwnerKind {
        /// Entity used as the owner of a function-signature edge.
        owner: EntityId,
        /// Actual declaration kind of the proposed owner.
        kind: crate::ir::ItemKind,
    },
    /// A complete signature-carrier role plane was already captured.
    SignatureCarrierRoleAlreadyCaptured,
    /// One signature-binding capture attempted to replace an existing role or
    /// binding capture.
    SignatureCarrierBindingsAlreadyCaptured,
    /// The binding owner inputs did not name every Function row exactly once
    /// in entity order.
    SignatureCarrierBindingOwnerSet {
        /// Position in the entity-ordered owner input where comparison failed.
        row: usize,
        /// Function entity required at this position, if the owner lane still had one.
        expected: Option<EntityId>,
        /// Function entity supplied at this position, if the input still had one.
        observed: Option<EntityId>,
    },
    /// A captured function owner lacks a concrete ordered function tuple.
    SignatureCarrierBindingSignature {
        /// Function entity whose semantic type is not a concrete ordered signature tuple.
        owner: EntityId,
    },
    /// Captured role-local counts do not match the owner's concrete tuple.
    SignatureCarrierBindingCounts {
        /// Function entity whose captured lane counts disagree with its tuple.
        owner: EntityId,
        /// Number of parameter carriers captured for this owner.
        parameters: u32,
        /// Number of result carriers captured for this owner.
        results: u32,
    },
    /// One function product edge disagrees with its ordered tuple-cell role.
    SignatureCarrierBindingEdgeRole {
        /// Function entity that owns the mismatched product edge.
        owner: EntityId,
        /// Zero-based tuple-cell position of the edge.
        position: u32,
        /// Role assigned to this position by the function tuple.
        expected: ProductChildRole,
        /// Role recorded by the product edge.
        observed: ProductChildRole,
    },
    /// The flattened target pool has trailing or missing rows after all
    /// captured owner ranges were consumed.
    SignatureCarrierBindingTargetCount {
        /// Number of carrier targets implied by all captured owner ranges.
        expected: usize,
        /// Number of target coordinates in the flattened capture lane.
        observed: usize,
    },
    /// A signature carrier target is not a parameter declaration.
    SignatureCarrierBindingTargetKind {
        /// Function entity whose tuple cell points at this carrier.
        owner: EntityId,
        /// Entity selected to carry the parameter or result cell.
        carrier: EntityId,
        /// Actual declaration kind at the selected carrier entity.
        kind: crate::ir::ItemKind,
    },
    /// The carrier's semantic type does not match its exact tuple cell.
    SignatureCarrierBindingType {
        /// Function entity whose tuple cell establishes the required type.
        owner: EntityId,
        /// Entity selected to carry the tuple cell.
        carrier: EntityId,
        /// Parameter/result lane containing the mismatched cell.
        role: SignatureCarrierBindingRole,
        /// Zero-based position within that ordered lane.
        position: u32,
    },
    /// The product edge and function type child at this slot name different
    /// fact rows.
    SignatureCarrierBindingEdgeMismatch {
        /// Function entity that owns the mismatched product edge.
        owner: EntityId,
        /// Zero-based function tuple position of the edge.
        position: u32,
        /// Type coordinate recorded by the product edge.
        product_target: u32,
        /// Type coordinate recorded by the function type tuple.
        type_target: u32,
    },
    /// Parentage formed a cycle, so no stable qualified ownership key exists.
    ParentCycle {
        /// Entity whose parent chain participates in the cycle.
        entity: EntityId,
    },
    /// One source declaration could not form its validated package/path/kind
    /// identity key.  The entity coordinate and original vocabulary fault are
    /// retained instead of being reclassified as a dangling reference.
    DeclarationKey {
        /// Entity for which no valid package/path/kind key could be formed.
        entity: EntityId,
        /// Precise vocabulary validation failure for its declaration facts.
        cause: crate::ir::DeclarationKeyFault,
    },
    /// The central scoped declaration-key writer rejected its exact framed
    /// preimage.  This preserves profile/parentage/collision-width causes.
    ScopedDeclarationPreimage {
        /// Entity whose scoped key bytes could not be constructed.
        entity: EntityId,
        /// Profile, lineage, or size fault returned by the canonical key writer.
        cause: PreimageOverflow,
    },
    /// A foreign-key preimage could not be measured or written. The owner
    /// and compact foreign-path digest retain the exact failing endpoint.
    ForeignKeyPreimage {
        /// Entity that owns the foreign endpoint.
        owner: EntityId,
        /// Compact fingerprint of the foreign declaration key being written.
        target: VariantFingerprint,
        /// Exact measure or write failure from the foreign-key encoder.
        cause: PreimageOverflow,
    },
    /// The same family and variant pair was assigned to more than one tree item.
    DuplicateDeclarationIdentity {
        /// Repeated declaration-instance identity.
        identity: DeclarationIdentity,
    },
    /// A borrowed tree supplied a version row count different from its item count.
    TreeVersionCount {
        /// Number of declaration-version rows supplied.
        versions: usize,
        /// Number of tree items awaiting version rows.
        items: usize,
    },
    /// The cold semantic-authority lanes no longer matched the owned entity
    /// row count. This is an internal admission invariant, retained as an
    /// exact terminal instead of truncating or padding authority truth.
    AuthorityRowCount {
        /// Number of committed entity rows in the image.
        entities: usize,
        /// Number of supplied entity-authority fact rows.
        authority_rows: usize,
    },
    /// The cold occurrence-authority lane no longer matched observed graph
    /// occurrence rows.
    OccurrenceAuthorityRowCount {
        /// Number of graph occurrence rows in the image.
        occurrences: usize,
        /// Number of supplied occurrence-authority fact rows.
        authority_rows: usize,
    },
    /// One authority row contradicted its corresponding owned entity facts.
    AuthorityFacts {
        /// Entity whose admitted row disagrees with its authority facts.
        entity: EntityId,
        /// Specific consistency fault found in the authority row.
        cause: AuthorityFactFault,
    },
    /// One authority row contradicted its corresponding graph occurrence.
    OccurrenceAuthorityFacts {
        /// Occurrence whose admitted row disagrees with its authority facts.
        occurrence: LinkOccurrenceId,
        /// Specific consistency fault found in the occurrence authority row.
        cause: AuthorityFactFault,
    },
    /// An image provenance scope could not form the same validated
    /// package/path declaration key required by every entity family.
    ImageProvenanceLineage {
        /// Package-lineage validation fault that prevented scoping the image.
        cause: crate::ir::PackageLineageFault,
    },
    /// An image provenance scope could not form the same validated
    /// package/path declaration key required by every entity family.
    ImageProvenanceScope {
        /// Declaration-key validation fault for the image-level scope.
        cause: crate::ir::DeclarationKeyFault,
    },
    /// The validated image scope could not allocate or write its canonical
    /// declaration-key claim.
    ImageProvenanceScopePreimage {
        /// Allocation or encoding fault returned for the canonical scope key.
        cause: PreimageOverflow,
    },
    /// The image header's recipe was not derived from its exact source and
    /// advertised recipe facts.
    ImageProvenanceRecipe {
        /// Source identity the image provenance claim is expected to bind.
        source: SourceIdentity,
        /// Recipe evidence that did not match the source-derived recipe.
        recipe: CompileRecipeFact,
    },
    /// A second image provenance claim differed from the already-bound
    /// header. Equal claims are idempotent; neither source nor recipe truth
    /// is silently overwritten.
    ImageProvenanceRebind {
        /// Provenance claim already attached to the image header.
        existing: ImageProvenanceClaim,
        /// Different claim requested by the later builder operation.
        requested: ImageProvenanceClaim,
    },
    /// A language-specific fact row failed structural admission.
    LanguageExtension {
        /// Language that owns the extension facts.
        language: Language,
        /// Entity row carrying the invalid extension.
        entity: EntityId,
        /// Closed validation fault returned by that language's fact validator.
        violation: LanguageExtensionViolation,
    },
    /// Extension facts claim a different language than the image authority.
    LanguageProfileMismatch {
        /// Image-wide language authority captured by the builder.
        authority: SemanticImageAuthority,
        /// Language whose extension row requested another authority.
        extension: Language,
    },
    /// A builder attempted to replace the image's previously captured profile.
    LanguageProfileRebind {
        /// Language profile already fixed for the current image.
        existing: SemanticImageAuthority,
        /// Replacement language profile requested by the caller.
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
            Self::SignatureCarrierRoleCount { expected, observed } => write!(
                formatter,
                "signature-carrier role plane has {observed} rows, expected {expected}"
            ),
            Self::SignatureCarrierRoleKind { entity, kind } => write!(
                formatter,
                "signature-carrier role for entity {} is attached to non-parameter kind {kind:?}",
                entity.raw
            ),
            Self::SignatureCarrierRoleOwnerKind { owner, kind } => write!(
                formatter,
                "function-signature role edge is attached to non-function owner {} of kind {kind:?}",
                owner.raw
            ),
            Self::SignatureCarrierRoleAlreadyCaptured => {
                formatter.write_str("signature-carrier role plane was already captured")
            }
            Self::SignatureCarrierBindingsAlreadyCaptured => {
                formatter.write_str("signature-carrier bindings were already captured")
            }
            Self::SignatureCarrierBindingOwnerSet {
                row,
                expected,
                observed,
            } => write!(
                formatter,
                "signature-carrier owner row {row} is {observed:?}, expected {expected:?}"
            ),
            Self::SignatureCarrierBindingSignature { owner } => write!(
                formatter,
                "function {} has no concrete function tuple for captured bindings",
                owner.raw
            ),
            Self::SignatureCarrierBindingCounts {
                owner,
                parameters,
                results,
            } => write!(
                formatter,
                "function {} binding counts ({parameters} parameters, {results} results) do not match its tuple",
                owner.raw
            ),
            Self::SignatureCarrierBindingEdgeRole {
                owner,
                position,
                expected,
                observed,
            } => write!(
                formatter,
                "function {} signature edge {position} is {observed:?}, expected {expected:?}",
                owner.raw
            ),
            Self::SignatureCarrierBindingTargetCount { expected, observed } => write!(
                formatter,
                "signature-carrier target pool has {observed} rows, expected {expected}"
            ),
            Self::SignatureCarrierBindingTargetKind {
                owner,
                carrier,
                kind,
            } => write!(
                formatter,
                "function {} signature carrier {} has non-parameter kind {kind:?}",
                owner.raw, carrier.raw
            ),
            Self::SignatureCarrierBindingType {
                owner,
                carrier,
                role,
                position,
            } => write!(
                formatter,
                "function {} {role:?} slot {position} carrier {} has a different semantic type from its tuple cell",
                owner.raw, carrier.raw
            ),
            Self::SignatureCarrierBindingEdgeMismatch {
                owner,
                position,
                product_target,
                type_target,
            } => write!(
                formatter,
                "function {} signature slot {position} product target {product_target} differs from type child {type_target}",
                owner.raw
            ),
        }
    }
}

impl core::error::Error for BuildError {}

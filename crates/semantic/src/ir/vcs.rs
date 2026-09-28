//! Zero-copy version control over the canonical IR itself.
//!
//! There is no lowered VCS model, archive payload, or raise step here. A
//! snapshot is an identity plus `&Ir`; entity and link deltas merge the IR's
//! existing stable-order indices and borrow every result from the two images.

use core::{cmp::Ordering, fmt, iter::FusedIterator, iter::Peekable};

use crate::ir::{
    ArrayShape, CSharpFacts, ClangFacts, ConcreteType, CorePayloadHash, DeclarationFamilyId,
    DeclarationIdentity, DeclarationLinkTarget, DocFragment, EntityId, ExternalTarget,
    ExternalTargetIdentity, FactAvailability, FreePredicate, GoFacts, ImageProvenance, Ir,
    ItemIdIter, ItemView, JavaFacts, Link, LinkId, LinkKind, LinkTarget, LiteralType, ObjectMember,
    PropertyKey, PythonFacts, QualifiedSegments, RustFacts, SemanticEntity, SemanticReader,
    TemplatePart, TupleElement, TypeExpr, TypeParameter, TypeParameterBound, TypeParameterKind,
    TypeQuery, TypeScriptFacts, VariantFingerprint, WildcardBound,
};

/// Content identity of one complete immutable IR generation.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GenerationId([u8; 32]);

impl GenerationId {
    #[must_use]
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One VCS generation borrowing the same IR used by renderers and graph queries.
#[derive(Clone, Copy)]
pub struct Snapshot<'ir> {
    pub generation: GenerationId,
    pub ir: &'ir Ir,
}

impl fmt::Debug for Snapshot<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("generation", &self.generation)
            .field("entities", &self.ir.items().len())
            .finish_non_exhaustive()
    }
}

/// One immutable generation exposed through the canonical semantic-reader
/// contract.
///
/// Unlike [`Snapshot`], this view also accepts a validated, borrowed
/// [`crate::ir::SemanticImageView`]. Durable publications can therefore be
/// compared without rebuilding an owned [`Ir`] or introducing a second VCS
/// representation.
pub struct SemanticSnapshot<'reader, Reader: SemanticReader + ?Sized> {
    pub generation: GenerationId,
    pub reader: &'reader Reader,
}

impl<Reader: SemanticReader + ?Sized> Clone for SemanticSnapshot<'_, Reader> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Reader: SemanticReader + ?Sized> Copy for SemanticSnapshot<'_, Reader> {}

impl<Reader: SemanticReader + ?Sized> fmt::Debug for SemanticSnapshot<'_, Reader> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticSnapshot")
            .field("generation", &self.generation)
            .field("entities", &self.reader.canonical_entities().len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delta<T> {
    Unchanged,
    Changed { before: T, after: T },
}

/// Whether the reader has a complete value for one semantic facet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetCoverage {
    Complete,
    Partial,
    Unavailable,
}

/// Result of comparing one facet's semantic values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetComparison {
    Unchanged,
    Changed,
    /// The facet is present or claimed, but this diff cannot compare it exactly.
    NeedsComparison,
    /// Neither generation captured this facet.
    Unavailable,
}

/// Value comparison paired with an explicit before/after coverage witness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FacetChange {
    pub coverage: Delta<FacetCoverage>,
    pub comparison: FacetComparison,
}

impl FacetChange {
    /// Whether a consumer must treat this facet as changed or unresolved.
    #[must_use]
    pub const fn requires_change(self) -> bool {
        !matches!(self.coverage, Delta::Unchanged)
            || matches!(
                self.comparison,
                FacetComparison::Changed | FacetComparison::NeedsComparison
            )
    }
}

/// Coordinate-independent facet results for one retained declaration.
///
/// Occurrence-site rows are deliberately outside entity deltas and must be
/// diffed through the occurrence relation by callers that need site-level
/// changes. Link deltas describe stable graph relations; they do not imply an
/// occurrence-site delta.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntityFacetChanges {
    pub declaration_shape: FacetChange,
    pub documentation: FacetChange,
    pub visibility: FacetChange,
    pub attributes: FacetChange,
    pub source: FacetChange,
    pub language_extension: FacetChange,
}

impl EntityFacetChanges {
    #[must_use]
    pub const fn requires_change(self) -> bool {
        self.declaration_shape.requires_change()
            || self.documentation.requires_change()
            || self.visibility.requires_change()
            || self.attributes.requires_change()
            || self.source.requires_change()
            || self.language_extension.requires_change()
    }
}

#[derive(Clone, Copy)]
pub enum EntityChange<'before, 'after> {
    Introduced {
        identity: DeclarationIdentity,
        after: ItemView<'after>,
    },
    Deleted {
        identity: DeclarationIdentity,
        before: ItemView<'before>,
    },
    Retained {
        family: DeclarationFamilyId,
        before: ItemView<'before>,
        after: ItemView<'after>,
        variant: Delta<VariantFingerprint>,
        core_payload: Delta<CorePayloadHash>,
        parent: Delta<Option<DeclarationIdentity>>,
        facets: EntityFacetChanges,
    },
}

/// One stable declaration delta borrowed from any complete semantic reader.
///
/// Exact composite identities are merged directly. A changed overload
/// fingerprint is consequently represented as one deletion and one
/// introduction; callers comparing package aggregates may conservatively
/// re-pair singleton families without guessing inside ambiguous overload
/// groups.
/// Stable handle to one declaration in a borrowed semantic reader.
///
/// The handle keeps change records small while preserving allocation-free
/// access to the complete declaration row. Its private reader field prevents
/// callers from constructing an identity/id pair that was not admitted by the
/// reader.
pub struct SemanticEntityRef<'reader, Reader: SemanticReader + ?Sized> {
    pub id: EntityId,
    pub identity: DeclarationIdentity,
    reader: &'reader Reader,
}

impl<Reader: SemanticReader + ?Sized> SemanticEntityRef<'_, Reader> {
    fn new(reader: &Reader, entity: SemanticEntity) -> SemanticEntityRef<'_, Reader> {
        SemanticEntityRef {
            id: entity.id,
            identity: entity.version.identity(),
            reader,
        }
    }

    /// Resolves the complete row from the reader that admitted this handle.
    #[must_use]
    pub fn entity(self) -> SemanticEntity {
        self.reader
            .entity(self.id)
            .expect("canonical semantic entity remains addressable")
    }
}

impl<Reader: SemanticReader + ?Sized> Clone for SemanticEntityRef<'_, Reader> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Reader: SemanticReader + ?Sized> Copy for SemanticEntityRef<'_, Reader> {}

impl<Reader: SemanticReader + ?Sized> fmt::Debug for SemanticEntityRef<'_, Reader> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticEntityRef")
            .field("id", &self.id)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

pub enum SemanticEntityChange<
    'before,
    'after,
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
> {
    Introduced {
        identity: DeclarationIdentity,
        after: SemanticEntityRef<'after, After>,
    },
    Deleted {
        identity: DeclarationIdentity,
        before: SemanticEntityRef<'before, Before>,
    },
    Retained {
        identity: DeclarationIdentity,
        before: SemanticEntityRef<'before, Before>,
        after: SemanticEntityRef<'after, After>,
        core_payload: Delta<CorePayloadHash>,
        parent: Delta<Option<DeclarationIdentity>>,
        facets: EntityFacetChanges,
    },
}

impl<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized> fmt::Debug
    for SemanticEntityChange<'_, '_, Before, After>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Introduced { identity, after } => formatter
                .debug_struct("Introduced")
                .field("identity", identity)
                .field("after", after)
                .finish(),
            Self::Deleted { identity, before } => formatter
                .debug_struct("Deleted")
                .field("identity", identity)
                .field("before", before)
                .finish(),
            Self::Retained {
                identity,
                before,
                after,
                core_payload,
                parent,
                facets,
            } => formatter
                .debug_struct("Retained")
                .field("identity", identity)
                .field("before", before)
                .field("after", after)
                .field("core_payload", core_payload)
                .field("parent", parent)
                .field("facets", facets)
                .finish(),
        }
    }
}

/// Allocation-free merge over two canonical reader entity streams.
pub struct SemanticEntityChanges<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> {
    /// Image-level source, recipe, and package-scope provenance delta, once
    /// per comparison rather than repeated on every retained entity.
    pub provenance: FacetChange,
    before: &'before Before,
    after: &'after After,
    left: Peekable<Before::CanonicalEntities<'before>>,
    right: Peekable<After::CanonicalEntities<'after>>,
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> SemanticEntityChanges<'before, 'after, Before, After>
{
    #[must_use]
    pub fn new(
        before: SemanticSnapshot<'before, Before>,
        after: SemanticSnapshot<'after, After>,
    ) -> Self {
        Self {
            provenance: compare_image_provenance(before.reader, after.reader),
            before: before.reader,
            after: after.reader,
            left: before.reader.canonical_entities().peekable(),
            right: after.reader.canonical_entities().peekable(),
        }
    }
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> Iterator for SemanticEntityChanges<'before, 'after, Before, After>
{
    type Item = SemanticEntityChange<'before, 'after, Before, After>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let ordering = match (self.left.peek(), self.right.peek()) {
                (Some(left), Some(right)) => left.version.identity().cmp(&right.version.identity()),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return None,
            };
            match ordering {
                Ordering::Less => {
                    let before = self.left.next()?;
                    return Some(SemanticEntityChange::Deleted {
                        identity: before.version.identity(),
                        before: SemanticEntityRef::new(self.before, before),
                    });
                }
                Ordering::Greater => {
                    let after = self.right.next()?;
                    return Some(SemanticEntityChange::Introduced {
                        identity: after.version.identity(),
                        after: SemanticEntityRef::new(self.after, after),
                    });
                }
                Ordering::Equal => {
                    let before = self.left.next()?;
                    let after = self.right.next()?;
                    let identity = before.version.identity();
                    let core_payload =
                        delta(before.version.core_payload, after.version.core_payload);
                    let parent = delta(
                        semantic_parent(self.before, before),
                        semantic_parent(self.after, after),
                    );
                    let facets = compare_entity_facets(self.before, before, self.after, after);
                    if matches!(core_payload, Delta::Unchanged)
                        && matches!(parent, Delta::Unchanged)
                        && !facets.requires_change()
                    {
                        continue;
                    }
                    return Some(SemanticEntityChange::Retained {
                        identity,
                        before: SemanticEntityRef::new(self.before, before),
                        after: SemanticEntityRef::new(self.after, after),
                        core_payload,
                        parent,
                        facets,
                    });
                }
            }
        }
    }
}

fn facet_change(
    before_coverage: FacetCoverage,
    after_coverage: FacetCoverage,
    comparison: Option<bool>,
) -> FacetChange {
    let coverage = delta(before_coverage, after_coverage);
    let comparison = if matches!(coverage, Delta::Changed { .. }) {
        FacetComparison::Changed
    } else {
        match (before_coverage, comparison) {
            (FacetCoverage::Unavailable, _) => FacetComparison::Unavailable,
            (FacetCoverage::Partial, _) => FacetComparison::NeedsComparison,
            (FacetCoverage::Complete, Some(true)) => FacetComparison::Unchanged,
            (FacetCoverage::Complete, Some(false)) => FacetComparison::Changed,
            (FacetCoverage::Complete, None) => FacetComparison::NeedsComparison,
        }
    };
    FacetChange {
        coverage,
        comparison,
    }
}

fn availability_coverage(value: FactAvailability) -> FacetCoverage {
    match value {
        FactAvailability::Captured => FacetCoverage::Complete,
        FactAvailability::Unavailable => FacetCoverage::Unavailable,
    }
}

fn source_coverage(entity: SemanticEntity) -> FacetCoverage {
    match (entity.authority.source, entity.authority.source_file) {
        (FactAvailability::Captured, FactAvailability::Captured) => FacetCoverage::Complete,
        (FactAvailability::Unavailable, FactAvailability::Unavailable) => {
            FacetCoverage::Unavailable
        }
        _ => FacetCoverage::Partial,
    }
}

fn provenance_coverage(provenance: ImageProvenance) -> FacetCoverage {
    match provenance {
        ImageProvenance::Captured { .. } => FacetCoverage::Complete,
        ImageProvenance::Unavailable => FacetCoverage::Unavailable,
    }
}

fn compare_entity_facets<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: SemanticEntity,
    after_reader: &After,
    after: SemanticEntity,
) -> EntityFacetChanges {
    let declaration_shape = facet_change(
        FacetCoverage::Complete,
        FacetCoverage::Complete,
        match (
            before_reader.atom(before.name),
            after_reader.atom(after.name),
        ) {
            (Some(left), Some(right)) => Some(before.kind == after.kind && left == right),
            _ => None,
        },
    );

    let documentation = facet_change(
        availability_coverage(before.authority.documentation),
        availability_coverage(after.authority.documentation),
        if before.authority.documentation == FactAvailability::Captured
            && after.authority.documentation == FactAvailability::Captured
        {
            semantic_docs_equal(before_reader, before.docs, after_reader, after.docs)
        } else {
            None
        },
    );
    let visibility = facet_change(
        availability_coverage(before.authority.visibility),
        availability_coverage(after.authority.visibility),
        if before.authority.visibility == FactAvailability::Captured
            && after.authority.visibility == FactAvailability::Captured
        {
            Some(before.visibility == after.visibility)
        } else {
            None
        },
    );
    let attributes = facet_change(
        availability_coverage(before.authority.attributes),
        availability_coverage(after.authority.attributes),
        if before.authority.attributes == FactAvailability::Captured
            && after.authority.attributes == FactAvailability::Captured
        {
            semantic_atoms_equal(
                before_reader,
                before.attributes,
                after_reader,
                after.attributes,
            )
        } else {
            None
        },
    );
    let source = facet_change(
        source_coverage(before),
        source_coverage(after),
        if source_coverage(before) == FacetCoverage::Complete
            && source_coverage(after) == FacetCoverage::Complete
        {
            semantic_source_equal(before_reader, before.source, after_reader, after.source)
        } else {
            None
        },
    );

    let language_extension = facet_change(
        availability_coverage(before.authority.language_extension),
        availability_coverage(after.authority.language_extension),
        if before.authority.language_extension == FactAvailability::Captured
            && after.authority.language_extension == FactAvailability::Captured
        {
            semantic_language_extension_equal(before_reader, before, after_reader, after)
        } else {
            None
        },
    );

    EntityFacetChanges {
        declaration_shape,
        documentation,
        visibility,
        attributes,
        source,
        language_extension,
    }
}

fn compare_image_provenance<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    after_reader: &After,
) -> FacetChange {
    let before = before_reader.image_facts().provenance;
    let after = after_reader.image_facts().provenance;
    facet_change(
        provenance_coverage(before),
        provenance_coverage(after),
        match (before, after) {
            (ImageProvenance::Captured { .. }, ImageProvenance::Captured { .. }) => {
                semantic_provenance_equal(before_reader, before, after_reader, after)
            }
            _ => None,
        },
    )
}

fn semantic_language_extension_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: SemanticEntity,
    after_reader: &After,
    after: SemanticEntity,
) -> Option<bool> {
    // Valid owned IR and validated full images enforce at most one named
    // extension family per declaration, and require one whenever this claim
    // is Captured. Still inspect every family here: the reader contract is
    // intentionally value based, so an inconsistent/custom reader cannot
    // hide a mismatch in a later family behind an early return.
    let mut present = false;
    let mut equal = true;
    let mut comparable = true;

    macro_rules! compare_family {
        ($before:ident, $after:ident, $compare:ident) => {
            match (
                before_reader.$before(before.id),
                after_reader.$after(after.id),
            ) {
                (Some(left), Some(right)) => {
                    present = true;
                    match $compare(before_reader, left, after_reader, right) {
                        Some(family_equal) => equal &= family_equal,
                        None => comparable = false,
                    }
                }
                (Some(_), None) | (None, Some(_)) => return Some(false),
                (None, None) => {}
            }
        };
    }

    compare_family!(
        typescript_extension,
        typescript_extension,
        semantic_typescript_facts_equal
    );
    compare_family!(
        csharp_extension,
        csharp_extension,
        semantic_csharp_facts_equal
    );
    compare_family!(go_extension, go_extension, semantic_go_facts_equal);
    compare_family!(rust_extension, rust_extension, semantic_rust_facts_equal);
    compare_family!(
        python_extension,
        python_extension,
        semantic_python_facts_equal
    );
    compare_family!(java_extension, java_extension, semantic_java_facts_equal);
    compare_family!(clang_extension, clang_extension, semantic_clang_facts_equal);

    if !present {
        // A captured plane with no row on either side represents the same
        // empty semantic value. Valid images reject that authority mismatch,
        // but treating the pair as equal here avoids manufacturing churn if
        // a reader provides captured coverage with no per-entity extension.
        Some(true)
    } else if !equal {
        Some(false)
    } else if comparable {
        Some(true)
    } else {
        None
    }
}

fn semantic_typescript_facts_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: TypeScriptFacts,
    after_reader: &After,
    after: TypeScriptFacts,
) -> Option<bool> {
    all_equal([
        semantic_type_parameter_lists_equal(
            before_reader,
            before.type_parameters,
            after_reader,
            after.type_parameters,
            0,
        ),
        semantic_optional_type_equal(
            before_reader,
            before.declared,
            after_reader,
            after.declared,
            0,
        ),
        semantic_optional_type_equal(
            before_reader,
            before.observed,
            after_reader,
            after.observed,
            0,
        ),
    ])
}

fn semantic_csharp_facts_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: CSharpFacts,
    after_reader: &After,
    after: CSharpFacts,
) -> Option<bool> {
    all_equal([
        Some(
            before.nullability == after.nullability
                && before.reference_kind == after.reference_kind
                && before.effects == after.effects
                && before.partial == after.partial,
        ),
        semantic_type_parameter_lists_equal(
            before_reader,
            before.constraints,
            after_reader,
            after.constraints,
            0,
        ),
        semantic_atoms_equal(
            before_reader,
            before.attributes,
            after_reader,
            after.attributes,
        ),
        semantic_source_equal(
            before_reader,
            before.xml_provenance,
            after_reader,
            after.xml_provenance,
        ),
    ])
}

fn semantic_go_facts_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: GoFacts,
    after_reader: &After,
    after: GoFacts,
) -> Option<bool> {
    all_equal([
        Some(
            before.signature.variadic == after.signature.variadic
                && before.constant_group == after.constant_group
                && before.constant_flags == after.constant_flags,
        ),
        semantic_type_lists_equal(
            before_reader,
            before.signature.parameters,
            after_reader,
            after.signature.parameters,
            0,
        ),
        semantic_type_lists_equal(
            before_reader,
            before.signature.results,
            after_reader,
            after.signature.results,
            0,
        ),
        semantic_type_parameter_lists_equal(
            before_reader,
            before.type_parameters,
            after_reader,
            after.type_parameters,
            0,
        ),
        semantic_entity_lists_equal(before_reader, before.fields, after_reader, after.fields),
        semantic_entity_lists_equal(
            before_reader,
            before.method_set,
            after_reader,
            after.method_set,
        ),
        semantic_atoms_equal(
            before_reader,
            before.build_constraints,
            after_reader,
            after.build_constraints,
        ),
        semantic_atoms_equal(
            before_reader,
            before.constant_value,
            after_reader,
            after.constant_value,
        ),
    ])
}

fn semantic_rust_facts_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: RustFacts,
    after_reader: &After,
    after: RustFacts,
) -> Option<bool> {
    all_equal([
        Some(before.ownership == after.ownership),
        semantic_atoms_equal(
            before_reader,
            before.lifetimes,
            after_reader,
            after.lifetimes,
        ),
        semantic_type_parameter_lists_equal(
            before_reader,
            before.where_clauses,
            after_reader,
            after.where_clauses,
            0,
        ),
        semantic_atoms_equal(before_reader, before.macros, after_reader, after.macros),
        semantic_atoms_equal(
            before_reader,
            before.const_defaults,
            after_reader,
            after.const_defaults,
        ),
        semantic_free_predicate_lists_equal(
            before_reader,
            before.free_predicates,
            after_reader,
            after.free_predicates,
            0,
        ),
    ])
}

fn semantic_python_facts_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: PythonFacts,
    after_reader: &After,
    after: PythonFacts,
) -> Option<bool> {
    all_equal([
        Some(
            before.parameter_kind == after.parameter_kind
                && before.dynamic_confidence == after.dynamic_confidence,
        ),
        semantic_atoms_equal(
            before_reader,
            before.decorators,
            after_reader,
            after.decorators,
        ),
    ])
}

fn semantic_java_facts_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: JavaFacts,
    after_reader: &After,
    after: JavaFacts,
) -> Option<bool> {
    all_equal([
        semantic_type_lists_equal(before_reader, before.throws, after_reader, after.throws, 0),
        semantic_atoms_equal(
            before_reader,
            before.annotations,
            after_reader,
            after.annotations,
        ),
        semantic_entity_lists_equal(
            before_reader,
            before.overloads,
            after_reader,
            after.overloads,
        ),
        semantic_entity_lists_equal(
            before_reader,
            before.record_components,
            after_reader,
            after.record_components,
        ),
    ])
}

fn semantic_clang_facts_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: ClangFacts,
    after_reader: &After,
    after: ClangFacts,
) -> Option<bool> {
    all_equal([
        Some(
            before.qualifiers == after.qualifiers
                && before.storage == after.storage
                && before.layout == after.layout,
        ),
        semantic_type_parameter_lists_equal(
            before_reader,
            before.templates,
            after_reader,
            after.templates,
            0,
        ),
        semantic_atoms_equal(before_reader, before.includes, after_reader, after.includes),
    ])
}

fn all_equal<const N: usize>(comparisons: [Option<bool>; N]) -> Option<bool> {
    let mut unresolved = false;
    for comparison in comparisons {
        match comparison {
            Some(true) => {}
            Some(false) => return Some(false),
            None => unresolved = true,
        }
    }
    if unresolved { None } else { Some(true) }
}

fn semantic_sequence_equal<Before, After, Compare>(
    before: Before,
    after: After,
    mut compare: Compare,
) -> Option<bool>
where
    Before: ExactSizeIterator,
    After: ExactSizeIterator,
    Compare: FnMut(Before::Item, After::Item) -> Option<bool>,
{
    if before.len() != after.len() {
        return Some(false);
    }
    let mut unresolved = false;
    for (left, right) in before.zip(after) {
        match compare(left, right) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => unresolved = true,
        }
    }
    if unresolved { None } else { Some(true) }
}

fn semantic_entity_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: EntityId,
    after_reader: &After,
    after: EntityId,
) -> Option<bool> {
    Some(
        before_reader.entity(before)?.version.identity()
            == after_reader.entity(after)?.version.identity(),
    )
}

fn semantic_atom_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::AtomId,
    after_reader: &After,
    after: crate::ir::AtomId,
) -> Option<bool> {
    Some(before_reader.atom(before)? == after_reader.atom(after)?)
}

fn semantic_optional_type_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: Option<crate::ir::TypeId>,
    after_reader: &After,
    after: Option<crate::ir::TypeId>,
    depth: u8,
) -> Option<bool> {
    match (before, after) {
        (None, None) => Some(true),
        (Some(left), Some(right)) => {
            semantic_type_equal(before_reader, left, after_reader, right, depth)
        }
        _ => Some(false),
    }
}

// Type graphs are validated, but still bound recursive stack use. A deeper
// extension type remains explicitly unresolved rather than risking overflow.
const MAX_EXTENSION_TYPE_DEPTH: u8 = 96;

fn semantic_type_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::TypeId,
    after_reader: &After,
    after: crate::ir::TypeId,
    depth: u8,
) -> Option<bool> {
    if depth >= MAX_EXTENSION_TYPE_DEPTH {
        return None;
    }
    let child_depth = depth.saturating_add(1);
    match (before_reader.ty(before)?, after_reader.ty(after)?) {
        (TypeExpr::Concrete(left), TypeExpr::Concrete(right)) => {
            semantic_concrete_equal(before_reader, left, after_reader, right, child_depth)
        }
        (TypeExpr::Computed(left), TypeExpr::Computed(right)) => {
            semantic_computed_equal(before_reader, left, after_reader, right, child_depth)
        }
        (TypeExpr::Unknown(left), TypeExpr::Unknown(right)) => all_equal([
            Some(left.reason == right.reason),
            semantic_optional_atoms_equal(
                before_reader,
                left.spelling,
                after_reader,
                right.spelling,
            ),
        ]),
        _ => Some(false),
    }
}

fn semantic_concrete_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: ConcreteType,
    after_reader: &After,
    after: ConcreteType,
    depth: u8,
) -> Option<bool> {
    use ConcreteType as C;
    match (before, after) {
        (C::Builtin(left), C::Builtin(right)) => Some(left == right),
        (C::Literal(left), C::Literal(right)) => {
            semantic_literal_equal(before_reader, left, after_reader, right)
        }
        (C::Nominal(left), C::Nominal(right)) => {
            semantic_entity_equal(before_reader, left, after_reader, right)
        }
        (C::External(left), C::External(right)) => {
            semantic_external_targets_equal(before_reader, left, after_reader, right)
        }
        (C::Parameter(left), C::Parameter(right)) => {
            semantic_atom_equal(before_reader, left, after_reader, right)
        }
        (
            C::Applied {
                constructor: left_constructor,
                arguments: left_arguments,
            },
            C::Applied {
                constructor: right_constructor,
                arguments: right_arguments,
            },
        ) => all_equal([
            semantic_type_equal(
                before_reader,
                left_constructor,
                after_reader,
                right_constructor,
                depth,
            ),
            semantic_type_lists_equal(
                before_reader,
                left_arguments,
                after_reader,
                right_arguments,
                depth,
            ),
        ]),
        (C::Tuple(left), C::Tuple(right)) => {
            semantic_tuple_lists_equal(before_reader, left, after_reader, right, depth)
        }
        (C::Object(left), C::Object(right)) => {
            semantic_object_lists_equal(before_reader, left, after_reader, right, depth)
        }
        (
            C::Function {
                parameters: left_parameters,
                results: left_results,
                abi: left_abi,
                variadic: left_variadic,
                unsafe_: left_unsafe,
            },
            C::Function {
                parameters: right_parameters,
                results: right_results,
                abi: right_abi,
                variadic: right_variadic,
                unsafe_: right_unsafe,
            },
        ) => all_equal([
            Some(left_variadic == right_variadic && left_unsafe == right_unsafe),
            semantic_optional_atoms_equal(before_reader, left_abi, after_reader, right_abi),
            semantic_tuple_lists_equal(
                before_reader,
                left_parameters,
                after_reader,
                right_parameters,
                depth,
            ),
            semantic_tuple_lists_equal(
                before_reader,
                left_results,
                after_reader,
                right_results,
                depth,
            ),
        ]),
        (
            C::Reference {
                target: left_target,
                mutability: left_mutability,
                lifetime: left_lifetime,
            },
            C::Reference {
                target: right_target,
                mutability: right_mutability,
                lifetime: right_lifetime,
            },
        ) => all_equal([
            Some(left_mutability == right_mutability),
            semantic_optional_atoms_equal(
                before_reader,
                left_lifetime,
                after_reader,
                right_lifetime,
            ),
            semantic_type_equal(
                before_reader,
                left_target,
                after_reader,
                right_target,
                depth,
            ),
        ]),
        (
            C::CxxReference {
                target: left_target,
                category: left_category,
            },
            C::CxxReference {
                target: right_target,
                category: right_category,
            },
        ) => all_equal([
            Some(left_category == right_category),
            semantic_type_equal(
                before_reader,
                left_target,
                after_reader,
                right_target,
                depth,
            ),
        ]),
        (C::CPointer { target: left }, C::CPointer { target: right })
        | (C::CBlockPointer { target: left }, C::CBlockPointer { target: right })
        | (C::Slice(left), C::Slice(right))
        | (C::Optional(left), C::Optional(right)) => {
            semantic_type_equal(before_reader, left, after_reader, right, depth)
        }
        (
            C::CxxMemberPointer {
                owner: left_owner,
                member: left_member,
            },
            C::CxxMemberPointer {
                owner: right_owner,
                member: right_member,
            },
        ) => all_equal([
            semantic_type_equal(before_reader, left_owner, after_reader, right_owner, depth),
            semantic_type_equal(
                before_reader,
                left_member,
                after_reader,
                right_member,
                depth,
            ),
        ]),
        (
            C::CQualified {
                target: left_target,
                qualifiers: left_qualifiers,
            },
            C::CQualified {
                target: right_target,
                qualifiers: right_qualifiers,
            },
        ) => all_equal([
            Some(left_qualifiers == right_qualifiers),
            semantic_type_equal(
                before_reader,
                left_target,
                after_reader,
                right_target,
                depth,
            ),
        ]),
        (
            C::NativeCharacter {
                role: left_role,
                width: left_width,
            },
            C::NativeCharacter {
                role: right_role,
                width: right_width,
            },
        ) => Some(left_role == right_role && left_width == right_width),
        (
            C::Pointer {
                target: left_target,
                mutability: left_mutability,
            },
            C::Pointer {
                target: right_target,
                mutability: right_mutability,
            },
        ) => all_equal([
            Some(left_mutability == right_mutability),
            semantic_type_equal(
                before_reader,
                left_target,
                after_reader,
                right_target,
                depth,
            ),
        ]),
        (
            C::Array {
                element: left_element,
                shape: left_shape,
            },
            C::Array {
                element: right_element,
                shape: right_shape,
            },
        ) => all_equal([
            semantic_type_equal(
                before_reader,
                left_element,
                after_reader,
                right_element,
                depth,
            ),
            semantic_array_shape_equal(before_reader, left_shape, after_reader, right_shape),
        ]),
        (C::Union(left), C::Union(right))
        | (C::Intersection(left), C::Intersection(right))
        | (C::ImplTrait(left), C::ImplTrait(right))
        | (C::DynTrait(left), C::DynTrait(right)) => {
            semantic_type_lists_equal(before_reader, left, after_reader, right, depth)
        }
        (C::Wildcard(left), C::Wildcard(right)) => {
            semantic_wildcard_equal(before_reader, left, after_reader, right, depth)
        }
        (
            C::Annotated {
                kind: left_kind,
                target: left_target,
            },
            C::Annotated {
                kind: right_kind,
                target: right_target,
            },
        ) => all_equal([
            Some(left_kind == right_kind),
            semantic_type_equal(
                before_reader,
                left_target,
                after_reader,
                right_target,
                depth,
            ),
        ]),
        (C::Inferred(left), C::Inferred(right)) => {
            semantic_optional_atoms_equal(before_reader, left, after_reader, right)
        }
        (
            C::QualifiedPath {
                self_type: left_self,
                trait_type: left_trait,
                segments: left_segments,
                spelling: left_spelling,
            },
            C::QualifiedPath {
                self_type: right_self,
                trait_type: right_trait,
                segments: right_segments,
                spelling: right_spelling,
            },
        ) => all_equal([
            semantic_type_equal(before_reader, left_self, after_reader, right_self, depth),
            semantic_optional_type_equal(
                before_reader,
                left_trait,
                after_reader,
                right_trait,
                depth,
            ),
            semantic_qualified_segments_equal(
                before_reader,
                left_segments,
                after_reader,
                right_segments,
            ),
            semantic_atom_equal(before_reader, left_spelling, after_reader, right_spelling),
        ]),
        (
            C::Map {
                key: left_key,
                value: left_value,
            },
            C::Map {
                key: right_key,
                value: right_value,
            },
        ) => all_equal([
            semantic_type_equal(before_reader, left_key, after_reader, right_key, depth),
            semantic_type_equal(before_reader, left_value, after_reader, right_value, depth),
        ]),
        (
            C::Channel {
                direction: left_direction,
                element: left_element,
            },
            C::Channel {
                direction: right_direction,
                element: right_element,
            },
        ) => all_equal([
            Some(left_direction == right_direction),
            semantic_type_equal(
                before_reader,
                left_element,
                after_reader,
                right_element,
                depth,
            ),
        ]),
        _ => Some(false),
    }
}

fn semantic_computed_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::ComputedType,
    after_reader: &After,
    after: crate::ir::ComputedType,
    depth: u8,
) -> Option<bool> {
    use crate::ir::ComputedType as C;
    match (before, after) {
        (C::KeyOf(left), C::KeyOf(right)) | (C::Awaited(left), C::Awaited(right)) => {
            semantic_type_equal(before_reader, left, after_reader, right, depth)
        }
        (C::TypeOf(left), C::TypeOf(right)) => {
            semantic_type_query_equal(before_reader, left, after_reader, right)
        }
        (
            C::IndexedAccess {
                object: left_object,
                index: left_index,
            },
            C::IndexedAccess {
                object: right_object,
                index: right_index,
            },
        ) => all_equal([
            semantic_type_equal(
                before_reader,
                left_object,
                after_reader,
                right_object,
                depth,
            ),
            semantic_type_equal(before_reader, left_index, after_reader, right_index, depth),
        ]),
        (
            C::Conditional {
                check: left_check,
                extends: left_extends,
                then_type: left_then,
                else_type: left_else,
                distributive: left_distributive,
            },
            C::Conditional {
                check: right_check,
                extends: right_extends,
                then_type: right_then,
                else_type: right_else,
                distributive: right_distributive,
            },
        ) => all_equal([
            Some(left_distributive == right_distributive),
            semantic_type_equal(before_reader, left_check, after_reader, right_check, depth),
            semantic_type_equal(
                before_reader,
                left_extends,
                after_reader,
                right_extends,
                depth,
            ),
            semantic_type_equal(before_reader, left_then, after_reader, right_then, depth),
            semantic_type_equal(before_reader, left_else, after_reader, right_else, depth),
        ]),
        (
            C::Mapped {
                parameter: left_parameter,
                constraint: left_constraint,
                name_as: left_name_as,
                value: left_value,
                readonly: left_readonly,
                optional: left_optional,
            },
            C::Mapped {
                parameter: right_parameter,
                constraint: right_constraint,
                name_as: right_name_as,
                value: right_value,
                readonly: right_readonly,
                optional: right_optional,
            },
        ) => all_equal([
            Some(left_readonly == right_readonly && left_optional == right_optional),
            semantic_atom_equal(before_reader, left_parameter, after_reader, right_parameter),
            semantic_type_equal(
                before_reader,
                left_constraint,
                after_reader,
                right_constraint,
                depth,
            ),
            semantic_optional_type_equal(
                before_reader,
                left_name_as,
                after_reader,
                right_name_as,
                depth,
            ),
            semantic_type_equal(before_reader, left_value, after_reader, right_value, depth),
        ]),
        (
            C::Infer {
                parameter: left_parameter,
                constraint: left_constraint,
            },
            C::Infer {
                parameter: right_parameter,
                constraint: right_constraint,
            },
        ) => all_equal([
            semantic_atom_equal(before_reader, left_parameter, after_reader, right_parameter),
            semantic_optional_type_equal(
                before_reader,
                left_constraint,
                after_reader,
                right_constraint,
                depth,
            ),
        ]),
        (C::TemplateLiteral(left), C::TemplateLiteral(right)) => {
            semantic_template_lists_equal(before_reader, left, after_reader, right, depth)
        }
        (
            C::Import {
                specifier: left_specifier,
                qualifier: left_qualifier,
                arguments: left_arguments,
            },
            C::Import {
                specifier: right_specifier,
                qualifier: right_qualifier,
                arguments: right_arguments,
            },
        ) => all_equal([
            semantic_atom_equal(before_reader, left_specifier, after_reader, right_specifier),
            semantic_atoms_equal(before_reader, left_qualifier, after_reader, right_qualifier),
            semantic_type_lists_equal(
                before_reader,
                left_arguments,
                after_reader,
                right_arguments,
                depth,
            ),
        ]),
        (C::This, C::This) => Some(true),
        _ => Some(false),
    }
}

fn semantic_literal_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: LiteralType,
    after_reader: &After,
    after: LiteralType,
) -> Option<bool> {
    use LiteralType as L;
    match (before, after) {
        (L::String(left), L::String(right))
        | (L::Number(left), L::Number(right))
        | (L::BigInt(left), L::BigInt(right)) => {
            semantic_atom_equal(before_reader, left, after_reader, right)
        }
        (L::Boolean(left), L::Boolean(right)) => Some(left == right),
        (L::Null, L::Null) | (L::Undefined, L::Undefined) => Some(true),
        _ => Some(false),
    }
}

fn semantic_array_shape_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: ArrayShape,
    after_reader: &After,
    after: ArrayShape,
) -> Option<bool> {
    use ArrayShape as S;
    match (before, after) {
        (S::Sequence, S::Sequence) | (S::Incomplete, S::Incomplete) => Some(true),
        (S::Rectangular { rank: left }, S::Rectangular { rank: right }) => Some(left == right),
        (S::FixedValue { length: left }, S::FixedValue { length: right }) => Some(left == right),
        (S::ConstExpression(left), S::ConstExpression(right)) => {
            semantic_atom_equal(before_reader, left, after_reader, right)
        }
        _ => Some(false),
    }
}

fn semantic_wildcard_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: WildcardBound,
    after_reader: &After,
    after: WildcardBound,
    depth: u8,
) -> Option<bool> {
    match (before, after) {
        (WildcardBound::Unbounded, WildcardBound::Unbounded) => Some(true),
        (WildcardBound::Extends(left), WildcardBound::Extends(right))
        | (WildcardBound::Super(left), WildcardBound::Super(right)) => {
            semantic_type_equal(before_reader, left, after_reader, right, depth)
        }
        _ => Some(false),
    }
}

fn semantic_qualified_segments_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: QualifiedSegments,
    after_reader: &After,
    after: QualifiedSegments,
) -> Option<bool> {
    match (before, after) {
        (QualifiedSegments::Unavailable, QualifiedSegments::Unavailable) => Some(true),
        (QualifiedSegments::Captured(left), QualifiedSegments::Captured(right)) => {
            semantic_atoms_equal(before_reader, left, after_reader, right)
        }
        _ => Some(false),
    }
}

fn semantic_type_query_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: TypeQuery,
    after_reader: &After,
    after: TypeQuery,
) -> Option<bool> {
    match (before, after) {
        (TypeQuery::Entity(left), TypeQuery::Entity(right)) => {
            semantic_entity_equal(before_reader, left, after_reader, right)
        }
        (TypeQuery::Path(left), TypeQuery::Path(right)) => {
            semantic_atoms_equal(before_reader, left, after_reader, right)
        }
        (TypeQuery::External(left), TypeQuery::External(right)) => {
            semantic_external_targets_equal(before_reader, left, after_reader, right)
        }
        _ => Some(false),
    }
}

fn semantic_type_lists_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::TypeListId,
    after_reader: &After,
    after: crate::ir::TypeListId,
    depth: u8,
) -> Option<bool> {
    semantic_sequence_equal(
        before_reader.types(before)?,
        after_reader.types(after)?,
        |left, right| semantic_type_equal(before_reader, left, after_reader, right, depth),
    )
}

fn semantic_entity_lists_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::EntityListId,
    after_reader: &After,
    after: crate::ir::EntityListId,
) -> Option<bool> {
    semantic_sequence_equal(
        before_reader.entity_list(before)?,
        after_reader.entity_list(after)?,
        |left, right| semantic_entity_equal(before_reader, left, after_reader, right),
    )
}

fn semantic_tuple_lists_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::TupleElementListId,
    after_reader: &After,
    after: crate::ir::TupleElementListId,
    depth: u8,
) -> Option<bool> {
    semantic_sequence_equal(
        before_reader.tuple_elements(before)?,
        after_reader.tuple_elements(after)?,
        |left, right| semantic_tuple_element_equal(before_reader, left, after_reader, right, depth),
    )
}

fn semantic_tuple_element_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: TupleElement,
    after_reader: &After,
    after: TupleElement,
    depth: u8,
) -> Option<bool> {
    all_equal([
        Some(before.kind == after.kind),
        semantic_optional_atoms_equal(before_reader, before.label, after_reader, after.label),
        semantic_type_equal(before_reader, before.ty, after_reader, after.ty, depth),
    ])
}

fn semantic_object_lists_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::ObjectMemberListId,
    after_reader: &After,
    after: crate::ir::ObjectMemberListId,
    depth: u8,
) -> Option<bool> {
    semantic_sequence_equal(
        before_reader.object_members(before)?,
        after_reader.object_members(after)?,
        |left, right| semantic_object_member_equal(before_reader, left, after_reader, right, depth),
    )
}

fn semantic_object_member_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: ObjectMember,
    after_reader: &After,
    after: ObjectMember,
    depth: u8,
) -> Option<bool> {
    use ObjectMember as M;
    match (before, after) {
        (
            M::Property {
                key: left_key,
                ty: left_ty,
                optional: left_optional,
                readonly: left_readonly,
            },
            M::Property {
                key: right_key,
                ty: right_ty,
                optional: right_optional,
                readonly: right_readonly,
            },
        ) => all_equal([
            Some(left_optional == right_optional && left_readonly == right_readonly),
            semantic_property_key_equal(before_reader, left_key, after_reader, right_key, depth),
            semantic_type_equal(before_reader, left_ty, after_reader, right_ty, depth),
        ]),
        (
            M::Method {
                key: left_key,
                signature: left_signature,
                optional: left_optional,
            },
            M::Method {
                key: right_key,
                signature: right_signature,
                optional: right_optional,
            },
        ) => all_equal([
            Some(left_optional == right_optional),
            semantic_property_key_equal(before_reader, left_key, after_reader, right_key, depth),
            semantic_type_equal(
                before_reader,
                left_signature,
                after_reader,
                right_signature,
                depth,
            ),
        ]),
        (
            M::Index {
                parameter: left_parameter,
                key: left_key,
                value: left_value,
                readonly: left_readonly,
            },
            M::Index {
                parameter: right_parameter,
                key: right_key,
                value: right_value,
                readonly: right_readonly,
            },
        ) => all_equal([
            Some(left_readonly == right_readonly),
            semantic_atom_equal(before_reader, left_parameter, after_reader, right_parameter),
            semantic_type_equal(before_reader, left_key, after_reader, right_key, depth),
            semantic_type_equal(before_reader, left_value, after_reader, right_value, depth),
        ]),
        (M::Call(left), M::Call(right)) | (M::Construct(left), M::Construct(right)) => {
            semantic_type_equal(before_reader, left, after_reader, right, depth)
        }
        _ => Some(false),
    }
}

fn semantic_property_key_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: PropertyKey,
    after_reader: &After,
    after: PropertyKey,
    depth: u8,
) -> Option<bool> {
    match (before, after) {
        (PropertyKey::Named(left), PropertyKey::Named(right))
        | (PropertyKey::Private(left), PropertyKey::Private(right))
        | (PropertyKey::Numeric(left), PropertyKey::Numeric(right)) => {
            semantic_atom_equal(before_reader, left, after_reader, right)
        }
        (PropertyKey::Computed(left), PropertyKey::Computed(right)) => {
            semantic_type_equal(before_reader, left, after_reader, right, depth)
        }
        _ => Some(false),
    }
}

fn semantic_template_lists_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: crate::ir::TemplatePartListId,
    after_reader: &After,
    after: crate::ir::TemplatePartListId,
    depth: u8,
) -> Option<bool> {
    semantic_sequence_equal(
        before_reader.template_parts(before)?,
        after_reader.template_parts(after)?,
        |left, right| semantic_template_part_equal(before_reader, left, after_reader, right, depth),
    )
}

fn semantic_template_part_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: TemplatePart,
    after_reader: &After,
    after: TemplatePart,
    depth: u8,
) -> Option<bool> {
    match (before, after) {
        (TemplatePart::Bytes(left), TemplatePart::Bytes(right)) => {
            semantic_atom_equal(before_reader, left, after_reader, right)
        }
        (TemplatePart::Placeholder(left), TemplatePart::Placeholder(right)) => {
            semantic_type_equal(before_reader, left, after_reader, right, depth)
        }
        _ => Some(false),
    }
}

fn semantic_type_parameter_lists_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: crate::ir::TypeParameterListId,
    after_reader: &After,
    after: crate::ir::TypeParameterListId,
    depth: u8,
) -> Option<bool> {
    semantic_sequence_equal(
        before_reader.type_parameters(before)?,
        after_reader.type_parameters(after)?,
        |left, right| {
            semantic_type_parameter_equal(before_reader, left, after_reader, right, depth)
        },
    )
}

fn semantic_type_parameter_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: TypeParameter,
    after_reader: &After,
    after: TypeParameter,
    depth: u8,
) -> Option<bool> {
    all_equal([
        semantic_atom_equal(before_reader, before.name, after_reader, after.name),
        semantic_type_parameter_bounds_equal(
            before_reader,
            before.bounds,
            after_reader,
            after.bounds,
            depth,
        ),
        semantic_optional_type_equal(
            before_reader,
            before.default,
            after_reader,
            after.default,
            depth,
        ),
        Some(before.variance == after.variance && before.requirements == after.requirements),
        semantic_type_parameter_kind_equal(
            before_reader,
            before.kind,
            after_reader,
            after.kind,
            depth,
        ),
    ])
}

fn semantic_type_parameter_kind_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: TypeParameterKind,
    after_reader: &After,
    after: TypeParameterKind,
    depth: u8,
) -> Option<bool> {
    match (before, after) {
        (
            TypeParameterKind::Type { inference: left },
            TypeParameterKind::Type { inference: right },
        ) => Some(left == right),
        (TypeParameterKind::Lifetime, TypeParameterKind::Lifetime) => Some(true),
        (
            TypeParameterKind::ConstValue { value_type: left },
            TypeParameterKind::ConstValue { value_type: right },
        ) => semantic_type_equal(before_reader, left, after_reader, right, depth),
        _ => Some(false),
    }
}

fn semantic_type_parameter_bounds_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: crate::ir::TypeParameterBoundListId,
    after_reader: &After,
    after: crate::ir::TypeParameterBoundListId,
    depth: u8,
) -> Option<bool> {
    semantic_sequence_equal(
        before_reader.type_parameter_bounds(before)?,
        after_reader.type_parameter_bounds(after)?,
        |left, right| {
            semantic_type_parameter_bound_equal(before_reader, left, after_reader, right, depth)
        },
    )
}

fn semantic_type_parameter_bound_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: TypeParameterBound,
    after_reader: &After,
    after: TypeParameterBound,
    depth: u8,
) -> Option<bool> {
    match (before, after) {
        (TypeParameterBound::Type(left), TypeParameterBound::Type(right)) => {
            semantic_type_equal(before_reader, left, after_reader, right, depth)
        }
        (TypeParameterBound::Lifetime(left), TypeParameterBound::Lifetime(right)) => {
            semantic_atom_equal(before_reader, left, after_reader, right)
        }
        _ => Some(false),
    }
}

fn semantic_free_predicate_lists_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: crate::ir::FreePredicateListId,
    after_reader: &After,
    after: crate::ir::FreePredicateListId,
    depth: u8,
) -> Option<bool> {
    semantic_sequence_equal(
        before_reader.free_predicates(before)?,
        after_reader.free_predicates(after)?,
        |left, right| {
            semantic_free_predicate_equal(before_reader, left, after_reader, right, depth)
        },
    )
}

fn semantic_free_predicate_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: FreePredicate,
    after_reader: &After,
    after: FreePredicate,
    depth: u8,
) -> Option<bool> {
    all_equal([
        semantic_type_equal(
            before_reader,
            before.subject,
            after_reader,
            after.subject,
            depth,
        ),
        semantic_type_parameter_bounds_equal(
            before_reader,
            before.bounds,
            after_reader,
            after.bounds,
            depth,
        ),
    ])
}

fn semantic_docs_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::DocId,
    after_reader: &After,
    after: crate::ir::DocId,
) -> Option<bool> {
    let before_docs = before_reader.docs(before)?;
    let after_docs = after_reader.docs(after)?;
    if before_docs.len() != after_docs.len() {
        return Some(false);
    }
    let mut unresolved = false;
    for (left, right) in before_docs.zip(after_docs) {
        match semantic_doc_equal(before_reader, left, after_reader, right) {
            Some(true) => {}
            Some(false) => return Some(false),
            None => unresolved = true,
        }
    }
    if unresolved { None } else { Some(true) }
}

fn semantic_doc_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: DocFragment,
    after_reader: &After,
    after: DocFragment,
) -> Option<bool> {
    match (before, after) {
        (DocFragment::Text(left), DocFragment::Text(right))
        | (DocFragment::Code(left), DocFragment::Code(right)) => {
            Some(before_reader.text(left)? == after_reader.text(right)?)
        }
        (
            DocFragment::Link {
                label: left_label,
                target: left_target,
            },
            DocFragment::Link {
                label: right_label,
                target: right_target,
            },
        ) => {
            let labels_equal = before_reader.text(left_label)? == after_reader.text(right_label)?;
            if !labels_equal {
                return Some(false);
            }
            semantic_link_targets_equal(before_reader, left_target, after_reader, right_target)
        }
        (DocFragment::SoftBreak, DocFragment::SoftBreak)
        | (DocFragment::HardBreak, DocFragment::HardBreak) => Some(true),
        _ => Some(false),
    }
}

fn semantic_link_targets_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: LinkTarget,
    after_reader: &After,
    after: LinkTarget,
) -> Option<bool> {
    match (before, after) {
        (LinkTarget::Local(left), LinkTarget::Local(right)) => Some(
            before_reader.entity(left)?.version.identity()
                == after_reader.entity(right)?.version.identity(),
        ),
        (LinkTarget::External(left), LinkTarget::External(right)) => {
            semantic_external_targets_equal(before_reader, left, after_reader, right)
        }
        (LinkTarget::Local(_), LinkTarget::External(_))
        | (LinkTarget::External(_), LinkTarget::Local(_)) => Some(false),
    }
}

fn semantic_external_targets_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: crate::ir::ExternalId,
    after_reader: &After,
    after: crate::ir::ExternalId,
) -> Option<bool> {
    match (
        before_reader.external(before)?,
        after_reader.external(after)?,
    ) {
        (ExternalTarget::Stable { target: left }, ExternalTarget::Stable { target: right }) => {
            Some(left == right)
        }
        (ExternalTarget::Foreign(_), ExternalTarget::Foreign(_)) => Some(
            ExternalTargetIdentity::capture(before_reader, before).ok()?
                == ExternalTargetIdentity::capture(after_reader, after).ok()?,
        ),
        (ExternalTarget::FragmentEntity { .. }, ExternalTarget::FragmentEntity { .. }) => None,
        _ => Some(false),
    }
}

fn semantic_atoms_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: crate::ir::AtomListId,
    after_reader: &After,
    after: crate::ir::AtomListId,
) -> Option<bool> {
    let before_atoms = before_reader.atom_list(before)?;
    let after_atoms = after_reader.atom_list(after)?;
    if before_atoms.len() != after_atoms.len() {
        return Some(false);
    }
    let mut unresolved = false;
    for (left, right) in before_atoms.zip(after_atoms) {
        match (before_reader.atom(left), after_reader.atom(right)) {
            (Some(left), Some(right)) if left == right => {}
            (Some(_), Some(_)) => return Some(false),
            _ => unresolved = true,
        }
    }
    if unresolved { None } else { Some(true) }
}

fn semantic_source_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: Option<crate::ir::SourceSpan>,
    after_reader: &After,
    after: Option<crate::ir::SourceSpan>,
) -> Option<bool> {
    match (before, after) {
        (None, None) => Some(true),
        (Some(left), Some(right)) => {
            let left_path = before_reader.atom(left.file())?;
            let right_path = after_reader.atom(right.file())?;
            Some(
                left.start() == right.start()
                    && left.end() == right.end()
                    && left_path == right_path,
            )
        }
        (None, Some(_)) | (Some(_), None) => Some(false),
    }
}

fn semantic_provenance_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before_reader: &Before,
    before: ImageProvenance,
    after_reader: &After,
    after: ImageProvenance,
) -> Option<bool> {
    match (before, after) {
        (
            ImageProvenance::Captured {
                source: left_source,
                recipe: left_recipe,
                claim: left_claim,
                scope: left_scope,
            },
            ImageProvenance::Captured {
                source: right_source,
                recipe: right_recipe,
                claim: right_claim,
                scope: right_scope,
            },
        ) => {
            if left_source != right_source
                || left_recipe != right_recipe
                || left_claim != right_claim
            {
                return Some(false);
            }
            let atoms_equal = semantic_optional_atoms_equal(
                before_reader,
                Some(left_scope.ecosystem),
                after_reader,
                Some(right_scope.ecosystem),
            )? && semantic_optional_atoms_equal(
                before_reader,
                Some(left_scope.package),
                after_reader,
                Some(right_scope.package),
            )? && semantic_optional_atoms_equal(
                before_reader,
                Some(left_scope.path),
                after_reader,
                Some(right_scope.path),
            )? && semantic_optional_atoms_equal(
                before_reader,
                left_scope.coordinate,
                after_reader,
                right_scope.coordinate,
            )?;
            Some(atoms_equal)
        }
        (ImageProvenance::Unavailable, ImageProvenance::Unavailable) => None,
        _ => Some(false),
    }
}

fn semantic_optional_atoms_equal<
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
>(
    before_reader: &Before,
    before: Option<crate::ir::AtomId>,
    after_reader: &After,
    after: Option<crate::ir::AtomId>,
) -> Option<bool> {
    match (before, after) {
        (None, None) => Some(true),
        (Some(_), None) | (None, Some(_)) => Some(false),
        (Some(left), Some(right)) => Some(before_reader.atom(left)? == after_reader.atom(right)?),
    }
}

fn semantic_parent<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: SemanticEntity,
) -> Option<DeclarationIdentity> {
    entity
        .parent
        .and_then(|parent| reader.entity(parent))
        .map(|parent| parent.version.identity())
}

/// One canonical graph relation borrowed from any complete semantic reader.
pub struct SemanticStableLink<'reader, Reader: SemanticReader + ?Sized> {
    pub id: LinkId,
    pub key: StableLinkKey,
    pub evidence: Link,
    reader: &'reader Reader,
}

impl<Reader: SemanticReader + ?Sized> Clone for SemanticStableLink<'_, Reader> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Reader: SemanticReader + ?Sized> Copy for SemanticStableLink<'_, Reader> {}

impl<Reader: SemanticReader + ?Sized> fmt::Debug for SemanticStableLink<'_, Reader> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticStableLink")
            .field("id", &self.id)
            .field("key", &self.key)
            .field("evidence", &self.evidence)
            .finish()
    }
}

/// Allocation-free canonical graph cursor over a complete semantic reader.
pub struct SemanticStableLinks<'reader, Reader: SemanticReader + ?Sized + 'reader> {
    reader: &'reader Reader,
    links: Reader::CanonicalLinks<'reader>,
}

impl<'reader, Reader: SemanticReader + ?Sized> SemanticStableLinks<'reader, Reader> {
    #[must_use]
    pub fn new(snapshot: SemanticSnapshot<'reader, Reader>) -> Self {
        Self {
            reader: snapshot.reader,
            links: snapshot.reader.canonical_links(),
        }
    }
}

impl<'reader, Reader: SemanticReader + ?Sized> Iterator for SemanticStableLinks<'reader, Reader> {
    type Item = SemanticStableLink<'reader, Reader>;

    fn next(&mut self) -> Option<Self::Item> {
        self.links.find_map(|(id, evidence)| {
            semantic_stable_link_key(self.reader, evidence).map(|key| SemanticStableLink {
                id,
                key,
                evidence,
                reader: self.reader,
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.links.size_hint()
    }
}

impl<Reader: SemanticReader + ?Sized> FusedIterator for SemanticStableLinks<'_, Reader> {}

/// One stable graph relation delta borrowed directly from two complete
/// semantic readers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticLinkChangeKind {
    Added,
    Removed,
    EvidenceChanged,
}

pub struct SemanticLinkChange<
    'before,
    'after,
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
> {
    pub key: StableLinkKey,
    pub kind: SemanticLinkChangeKind,
    pub before: Option<SemanticStableLink<'before, Before>>,
    pub after: Option<SemanticStableLink<'after, After>>,
}

impl<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized> fmt::Debug
    for SemanticLinkChange<'_, '_, Before, After>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticLinkChange")
            .field("key", &self.key)
            .field("kind", &self.kind)
            .field("before", &self.before)
            .field("after", &self.after)
            .finish()
    }
}

/// Allocation-free merge over two canonical semantic-reader graph streams.
pub struct SemanticLinkChanges<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> {
    left: Peekable<SemanticStableLinks<'before, Before>>,
    right: Peekable<SemanticStableLinks<'after, After>>,
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> SemanticLinkChanges<'before, 'after, Before, After>
{
    #[must_use]
    pub fn new(
        before: SemanticSnapshot<'before, Before>,
        after: SemanticSnapshot<'after, After>,
    ) -> Self {
        Self {
            left: SemanticStableLinks::new(before).peekable(),
            right: SemanticStableLinks::new(after).peekable(),
        }
    }
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> Iterator for SemanticLinkChanges<'before, 'after, Before, After>
{
    type Item = SemanticLinkChange<'before, 'after, Before, After>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let ordering = match (self.left.peek(), self.right.peek()) {
                (Some(left), Some(right)) => left.key.cmp(&right.key),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return None,
            };
            match ordering {
                Ordering::Less => {
                    let before = self.left.next()?;
                    return Some(SemanticLinkChange {
                        key: before.key,
                        kind: SemanticLinkChangeKind::Removed,
                        before: Some(before),
                        after: None,
                    });
                }
                Ordering::Greater => {
                    let after = self.right.next()?;
                    return Some(SemanticLinkChange {
                        key: after.key,
                        kind: SemanticLinkChangeKind::Added,
                        before: None,
                        after: Some(after),
                    });
                }
                Ordering::Equal => {
                    let before = self.left.next()?;
                    let after = self.right.next()?;
                    if semantic_link_evidence_equal(before, after) {
                        continue;
                    }
                    return Some(SemanticLinkChange {
                        key: before.key,
                        kind: SemanticLinkChangeKind::EvidenceChanged,
                        before: Some(before),
                        after: Some(after),
                    });
                }
            }
        }
    }
}

fn semantic_stable_link_key<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    link: Link,
) -> Option<StableLinkKey> {
    let from = reader.entity(link.from)?.version.identity();
    let target = match link.target {
        LinkTarget::Local(entity) => {
            DeclarationLinkTarget::Local(reader.entity(entity)?.version.identity())
        }
        LinkTarget::External(external) => match reader.external(external)? {
            ExternalTarget::Stable { target } => DeclarationLinkTarget::Stable(target),
            ExternalTarget::Foreign(target) => DeclarationLinkTarget::Foreign(target.identity),
            ExternalTarget::FragmentEntity { target, .. } => {
                DeclarationLinkTarget::FragmentEntity(target)
            }
        },
    };
    Some(StableLinkKey {
        from,
        target,
        kind: link.kind,
    })
}

fn semantic_link_evidence_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before: SemanticStableLink<'_, Before>,
    after: SemanticStableLink<'_, After>,
) -> bool {
    before.evidence.confidence == after.evidence.confidence
        && match (before.evidence.source, after.evidence.source) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.start() == right.start()
                    && left.end() == right.end()
                    && before.reader.atom(left.file()) == after.reader.atom(right.file())
            }
            (None, Some(_)) | (Some(_), None) => false,
        }
}

/// Direct entity and graph diff over any pair of complete semantic readers.
pub struct SemanticDiff<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> {
    pub entities: SemanticEntityChanges<'before, 'after, Before, After>,
    pub links: SemanticLinkChanges<'before, 'after, Before, After>,
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> SemanticDiff<'before, 'after, Before, After>
{
    #[must_use]
    pub fn between(
        before: SemanticSnapshot<'before, Before>,
        after: SemanticSnapshot<'after, After>,
    ) -> Self {
        Self {
            entities: SemanticEntityChanges::new(before, after),
            links: SemanticLinkChanges::new(before, after),
        }
    }
}

impl fmt::Debug for EntityChange<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Introduced { identity, after } => formatter
                .debug_struct("Introduced")
                .field("identity", identity)
                .field("after", &after.id())
                .finish(),
            Self::Deleted { identity, before } => formatter
                .debug_struct("Deleted")
                .field("identity", identity)
                .field("before", &before.id())
                .finish(),
            Self::Retained {
                family,
                before,
                after,
                variant,
                core_payload,
                parent,
                facets,
            } => formatter
                .debug_struct("Retained")
                .field("family", family)
                .field("before", &before.id())
                .field("after", &after.id())
                .field("variant", variant)
                .field("core_payload", core_payload)
                .field("parent", parent)
                .field("facets", facets)
                .finish(),
        }
    }
}

/// Merge iterator over the two canonical stable-identity entity indices.
pub struct EntityChanges<'before, 'after> {
    /// Image-level source, recipe, and package-scope provenance delta, once
    /// per comparison rather than repeated on every retained entity.
    pub provenance: FacetChange,
    before: Snapshot<'before>,
    after: Snapshot<'after>,
    left: Peekable<ItemIdIter<'before>>,
    right: Peekable<ItemIdIter<'after>>,
}

impl<'before, 'after> EntityChanges<'before, 'after> {
    #[must_use]
    pub fn new(before: Snapshot<'before>, after: Snapshot<'after>) -> Self {
        Self {
            provenance: compare_image_provenance(before.ir, after.ir),
            before,
            after,
            left: before.ir.canonical_items().peekable(),
            right: after.ir.canonical_items().peekable(),
        }
    }
}

impl<'before, 'after> Iterator for EntityChanges<'before, 'after> {
    type Item = EntityChange<'before, 'after>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let ordering = match (self.left.peek(), self.right.peek()) {
                (Some(left), Some(right)) => left.version().family.cmp(&right.version().family),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return None,
            };
            match ordering {
                Ordering::Less => {
                    let before = self.left.next()?;
                    return Some(EntityChange::Deleted {
                        identity: before.version().identity(),
                        before,
                    });
                }
                Ordering::Greater => {
                    let after = self.right.next()?;
                    return Some(EntityChange::Introduced {
                        identity: after.version().identity(),
                        after,
                    });
                }
                Ordering::Equal => {
                    let family = self.left.peek()?.version().family;
                    let singleton = self.before.ir.family_items(family).len() == 1
                        && self.after.ir.family_items(family).len() == 1;
                    if singleton {
                        let before = self.left.next()?;
                        let after = self.right.next()?;
                        if let Some(change) =
                            retained_change(self.before.ir, self.after.ir, before, after)
                        {
                            return Some(change);
                        }
                        continue;
                    }
                    let identities = self
                        .left
                        .peek()?
                        .version()
                        .identity()
                        .cmp(&self.right.peek()?.version().identity());
                    if identities != Ordering::Equal {
                        if identities == Ordering::Less {
                            let before = self.left.next()?;
                            return Some(EntityChange::Deleted {
                                identity: before.version().identity(),
                                before,
                            });
                        }
                        let after = self.right.next()?;
                        return Some(EntityChange::Introduced {
                            identity: after.version().identity(),
                            after,
                        });
                    }
                    let before = self.left.next()?;
                    let after = self.right.next()?;
                    if let Some(change) =
                        retained_change(self.before.ir, self.after.ir, before, after)
                    {
                        return Some(change);
                    }
                    continue;
                }
            }
        }
    }
}

fn retained_change<'before, 'after>(
    before_ir: &Ir,
    after_ir: &Ir,
    before: ItemView<'before>,
    after: ItemView<'after>,
) -> Option<EntityChange<'before, 'after>> {
    let variant = delta(before.version().variant, after.version().variant);
    let core_payload = delta(before.version().core_payload, after.version().core_payload);
    let parent = delta(
        stable_parent(before_ir, before),
        stable_parent(after_ir, after),
    );
    let facets = compare_entity_facets(
        before_ir,
        before_ir.entity(before.id())?,
        after_ir,
        after_ir.entity(after.id())?,
    );
    if matches!(variant, Delta::Unchanged)
        && matches!(core_payload, Delta::Unchanged)
        && matches!(parent, Delta::Unchanged)
        && !facets.requires_change()
    {
        return None;
    }
    Some(EntityChange::Retained {
        family: before.version().family,
        before,
        after,
        variant,
        core_payload,
        parent,
        facets,
    })
}

fn delta<T: Eq>(before: T, after: T) -> Delta<T> {
    if before == after {
        Delta::Unchanged
    } else {
        Delta::Changed { before, after }
    }
}

fn stable_parent(ir: &Ir, item: ItemView<'_>) -> Option<DeclarationIdentity> {
    item.parent()
        .and_then(|parent| ir.version(parent))
        .map(|version| version.identity())
}

/// Canonical identity of a directed graph link.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StableLinkKey {
    pub from: DeclarationIdentity,
    pub target: DeclarationLinkTarget,
    pub kind: LinkKind,
}

/// One canonical link carrying a reconstructed register-sized evidence row.
#[derive(Clone, Copy)]
pub struct StableLink<'ir> {
    pub id: LinkId,
    pub key: StableLinkKey,
    pub evidence: Link,
    ir: &'ir Ir,
}

impl fmt::Debug for StableLink<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StableLink")
            .field("id", &self.id)
            .field("key", &self.key)
            .field("evidence", &self.evidence)
            .finish()
    }
}

/// Canonical stable-order link iterator.
pub struct StableLinks<'ir> {
    ir: &'ir Ir,
    ids: &'ir [LinkId],
}

impl<'ir> Iterator for StableLinks<'ir> {
    type Item = StableLink<'ir>;
    fn next(&mut self) -> Option<Self::Item> {
        let (id, rest) = self.ids.split_first()?;
        self.ids = rest;
        let evidence = self.ir.link(*id)?;
        Some(StableLink {
            id: *id,
            key: stable_link_key(self.ir, evidence)?,
            evidence,
            ir: self.ir,
        })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.ids.len(), Some(self.ids.len()))
    }
}
impl ExactSizeIterator for StableLinks<'_> {}
impl core::iter::FusedIterator for StableLinks<'_> {}

impl Ir {
    /// Iterates graph links in the canonical stable order used by IR-VCS.
    #[must_use]
    pub fn stable_links(&self) -> StableLinks<'_> {
        StableLinks {
            ir: self,
            ids: self.canonical_link_ids(),
        }
    }
}

/// Link lifecycle/evidence change classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkChangeKind {
    Added,
    Removed,
    EvidenceChanged,
}

/// One allocation-free stable link delta.
#[derive(Clone, Copy, Debug)]
pub struct LinkChange<'before, 'after> {
    pub key: StableLinkKey,
    pub kind: LinkChangeKind,
    pub before: Option<StableLink<'before>>,
    pub after: Option<StableLink<'after>>,
}

/// Merge iterator over two canonical stable-link indices.
pub struct LinkChanges<'before, 'after> {
    left: Peekable<StableLinks<'before>>,
    right: Peekable<StableLinks<'after>>,
}

impl<'before, 'after> LinkChanges<'before, 'after> {
    #[must_use]
    pub fn new(before: Snapshot<'before>, after: Snapshot<'after>) -> Self {
        Self {
            left: before.ir.stable_links().peekable(),
            right: after.ir.stable_links().peekable(),
        }
    }
}

impl<'before, 'after> Iterator for LinkChanges<'before, 'after> {
    type Item = LinkChange<'before, 'after>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let ordering = match (self.left.peek(), self.right.peek()) {
                (Some(left), Some(right)) => left.key.cmp(&right.key),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return None,
            };
            match ordering {
                Ordering::Less => {
                    let before = self.left.next()?;
                    return Some(LinkChange {
                        key: before.key,
                        kind: LinkChangeKind::Removed,
                        before: Some(before),
                        after: None,
                    });
                }
                Ordering::Greater => {
                    let after = self.right.next()?;
                    return Some(LinkChange {
                        key: after.key,
                        kind: LinkChangeKind::Added,
                        before: None,
                        after: Some(after),
                    });
                }
                Ordering::Equal => {
                    let before = self.left.next()?;
                    let after = self.right.next()?;
                    if link_evidence_equal(before, after) {
                        continue;
                    }
                    return Some(LinkChange {
                        key: before.key,
                        kind: LinkChangeKind::EvidenceChanged,
                        before: Some(before),
                        after: Some(after),
                    });
                }
            }
        }
    }
}

fn stable_link_key(ir: &Ir, link: Link) -> Option<StableLinkKey> {
    let from = ir.version(link.from)?.identity();
    let target = match link.target {
        LinkTarget::Local(entity) => DeclarationLinkTarget::Local(ir.version(entity)?.identity()),
        LinkTarget::External(external) => match ir.external(external)? {
            ExternalTarget::Stable { target } => DeclarationLinkTarget::Stable(*target),
            ExternalTarget::Foreign(target) => DeclarationLinkTarget::Foreign(target.identity),
            ExternalTarget::FragmentEntity { target, .. } => {
                DeclarationLinkTarget::FragmentEntity(*target)
            }
        },
    };
    Some(StableLinkKey {
        from,
        target,
        kind: link.kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
        FactAvailability, IrBuilder, ItemKind, SemanticReader, TreeItemInput, VariantFingerprint,
        Visibility,
    };

    #[test]
    fn captured_extension_coverage_without_a_row_is_semantically_empty() {
        let mut builder = IrBuilder::new();
        let versions = [EntityVersion {
            family: DeclarationFamilyId::from_raw([1; 16]),
            variant: VariantFingerprint::from_raw([1; 16]),
            core_payload: CorePayloadHash::from_raw([1; 16]),
        }];
        let items = [TreeItemInput {
            name: b"plain",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                visibility: FactAvailability::Captured,
                ..EntityAuthorityFacts::default()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }];
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .expect("plain test declaration is valid");
        let ir = builder.finish().expect("plain test IR is valid");
        let mut entity = ir
            .canonical_entities()
            .next()
            .expect("test declaration is retained");

        // Valid IrBuilder/full-image paths require exactly one extension row
        // when this claim is Captured. Exercise the comparer defensively with
        // the captured row view and no sparse family row on either side.
        entity.authority.language_extension = FactAvailability::Captured;
        let comparison = semantic_language_extension_equal(&ir, entity, &ir, entity);
        assert_eq!(comparison, Some(true));
        assert_eq!(
            facet_change(FacetCoverage::Complete, FacetCoverage::Complete, comparison).comparison,
            FacetComparison::Unchanged
        );
    }
}

fn link_evidence_equal(before: StableLink<'_>, after: StableLink<'_>) -> bool {
    before.evidence.confidence == after.evidence.confidence
        && match (before.evidence.source, after.evidence.source) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.start() == right.start()
                    && left.end() == right.end()
                    && before.ir.atom(left.file()) == after.ir.atom(right.file())
            }
            (None, Some(_)) | (Some(_), None) => false,
        }
}

/// Convenience pair of direct, zero-copy entity and link diff iterators.
pub struct Diff<'before, 'after> {
    pub entities: EntityChanges<'before, 'after>,
    pub links: LinkChanges<'before, 'after>,
}

impl<'before, 'after> Diff<'before, 'after> {
    #[must_use]
    pub fn between(before: Snapshot<'before>, after: Snapshot<'after>) -> Self {
        Self {
            entities: EntityChanges::new(before, after),
            links: LinkChanges::new(before, after),
        }
    }
}

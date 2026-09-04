//! Defines lower behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//!
//! Occurrence admission is deliberately deferred to the integration phase and
//! remains owned by compiler/ir/semantic_facts.rs. Direct authorities emit
//! declaration facts into this one canonical lane; unsupported authorities
//! return typed terminals instead of inspecting source text here.
use compiler_ir::DocumentationLane;
use compiler_ir::{
    AnnotationKind, AtomId, BuiltinType, ChannelDirection, ComputedType, ConcreteType, CvQualifiers,
    CxxReferenceCategory, DeclarationKey, Disambiguator, DocInput,
    EntityVersion, ExternalTarget, Ir,
    IrBuilder, ItemKind, LanguageExtensionInput, ListSpan, LiteralType, NominalRef, PayloadHash,
    PrimitiveShape, ProductChildRole, ProductChildren, ProductId, ProductListId, ProductRef,
    ObjectMember, PropertyKey,
    SemanticAtom, SemanticProduct, SemanticProductChild, SemanticProductConstructor,
    SemanticTypeChild, SemanticTypeFault, SemanticTypeRecord, SemanticTypeTag, StableEntityId,
    TemplatePart, TreeItemInput, TreeLinkInput, TreeLinkTarget, TupleElement, TupleElementKind,
    NativeCharacterRole, QualifiedSegments, TypeChildTarget, TypeId, TypeWidth, Visibility,
    WildcardBound,
};
use compiler_ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataResourceBudget, DataScratch,
    DocFactInput, DocFragmentInput, DocLinkTarget, EntityKind, EntityRecord, ExtensionPoolsLane,
    ExtensionRefList, ExtensionSectionInput, ExtensionSectionPlane, ExtensionTypeParameter,
    ExtensionTypeParameterRange,
    Occurrence, OccurrenceInput, OccurrenceLane, PrepareError, PreparedFragment, RecipeFact,
    SourceIdentity, TypeFactInput, TypeFactLane, TypeNode, WriteError,
    canonicalize_data_with_budget,
};
use core::mem::size_of;
use core::num::NonZeroU16;

pub(crate) mod clang;
pub(crate) mod csharp;
pub(crate) mod go;
pub(crate) mod java;
pub(crate) mod python;
pub(crate) mod rust;
pub(crate) mod typescript;

/// Dense bound of the multi-declaration semantic emission lane.
///
/// One LowerIr fragment admits at most this many provable declaration facts;
/// a source with more declarations is a typed lane rejection, never a
/// truncated emission. The bound also fixes every canonicalization scratch,
/// output, and resource reservation below.
/// 16,384 slots × 4-byte `u32` coordinate = 64 KiB; measured high-water 6,882 facts. Roll back to 8,192 if every target package stays below 4,096 facts.
pub(super) const MAX_EMISSION_FACTS: usize = 16384;
/// Dense bound of one fact's ordered product children.
/// 64 slots × 8-byte child = 512 bytes per fact; measured target high-water 244 pooled children. Roll back to 32 if it stays below 16 per fact.
pub(super) const MAX_FACT_CHILDREN: usize = 64;
/// Dense bound of one fact's ordered type-record children.
/// 64 slots × 8-byte child = 512 bytes per fact; measured target high-water 23,876 pooled computed children. Roll back to 32 if every target stays below 16 per row.
pub(super) const MAX_TYPE_CHILDREN: usize = 64;
/// Dense bound of the occurrence lane committed beside the declarations.
pub(super) const MAX_EMISSION_OCCURRENCES: usize = 8192;
/// Dense bound of the documentation lane; measured maximum is 13,529 fragments (`StringUtils.java`), so 16,384 is next.
pub(super) const MAX_EMISSION_DOC_FRAGMENTS: usize = 16384;
/// Dense bound of extension atoms admitted beside declaration names.
pub(super) const MAX_EXTENSION_ATOMS: usize = 2048;
/// Dense bound of pooled type parameters.
pub(super) const MAX_TYPE_PARAMETERS: usize = 512;
/// Dense bound of ordered type/lifetime bounds across one request.
/// Each bound has written source evidence, so the request geometry scales
/// with entered bytes rather than allocating a language-wide maximum.
pub(super) const MAX_TYPE_PARAMETER_BOUNDS: usize = MAX_TYPE_PARAMETERS * MAX_REF_LIST_ELEMENTS;
/// Dense bound of pooled reference lists per lane kind.
pub(super) const MAX_REF_LISTS: usize = 512;
/// Dense bound of one pooled reference list.
/// The measured corpus maximum is 46 (`ToStringBuilder.append`).
/// 64 is the next dense bound, preserving the old geometry for lists up to 16.
pub(super) const MAX_REF_LIST_ELEMENTS: usize = 64;
/// Total atom budget: one name per fact plus every extension atom.
pub(super) const MAX_EMISSION_ATOMS: usize = MAX_EMISSION_FACTS + MAX_EXTENSION_ATOMS;
/// Dense bound of anonymous type rows interned beside the fact rows.
/// 8,192 slots × 4-byte `u32` owner = 32 KiB; measured target high-water 0 rows. Roll back to 2,048 if it stays below 1,024.
pub(super) const MAX_ANONYMOUS_TYPE_ROWS: usize = 8192;
/// Dense bound of checker-computed type rows in the schema-2 segment.
/// 32,768 slots × 4-byte `u32` owner = 128 KiB; measured target high-water 23,037 rows. Roll back to 16,384 if every target stays below 8,192.
pub(super) const MAX_COMPUTED_TYPE_ROWS: usize = 32768;
/// Total type-row budget: one record per fact plus the anonymous pool.
pub(super) const MAX_TYPE_ROWS: usize =
    MAX_EMISSION_FACTS + MAX_ANONYMOUS_TYPE_ROWS + MAX_COMPUTED_TYPE_ROWS;
const _: () = assert!(MAX_EMISSION_FACTS <= u32::MAX as usize);
const _: () = assert!(MAX_TYPE_ROWS <= u32::MAX as usize);
/// Tagged staging coordinate for anonymous type rows.
///
/// These are *not* array offsets.  Keeping the lanes in disjoint tagged
/// segments prevents an entered ten-byte request from reserving every slot
/// below a protocol-maximum anonymous/computed row.  The compact writer is
/// the only place where these transaction-local coordinates become dense wire
/// `TypeId`s.
const ANONYMOUS_ROW_BASE: u32 = 0x4000_0000;
/// Tagged staging coordinate for checker-observed type rows.
pub(super) const COMPUTED_ROW_BASE: u32 = 0x8000_0000;
/// A literal template segment is not a staged type coordinate. This value is
/// private to the transaction-local collector and becomes the typed
/// `TypeChildTarget::Text` at the durable projection boundary.
const STAGED_TEXT_CHILD: u32 = u32::MAX;
/// Sentinel marking an absent row in an extension plane's row table.
const SECTION_NONE: u32 = u32::MAX;

/// Per-request reservation selected before an authority starts work. The
/// protocol constants remain hard ceilings; this plan alone determines the
/// collector's allocation geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResourcePlan {
    facts: usize,
    occurrences: usize,
    docs: usize,
    extension_atoms: usize,
    type_parameters: usize,
    type_parameter_bounds: usize,
    ref_lists: usize,
    anonymous_rows: usize,
    computed_rows: usize,
}

impl ResourcePlan {
    /// Complete geometry used only by explicit capacity falsifiers.
    pub(crate) const fn protocol_maximum() -> Self {
        Self {
            facts: MAX_EMISSION_FACTS,
            occurrences: MAX_EMISSION_OCCURRENCES,
            docs: MAX_EMISSION_DOC_FRAGMENTS,
            extension_atoms: MAX_EXTENSION_ATOMS,
            type_parameters: MAX_TYPE_PARAMETERS,
            type_parameter_bounds: MAX_TYPE_PARAMETER_BOUNDS,
            ref_lists: MAX_REF_LISTS,
            anonymous_rows: MAX_ANONYMOUS_TYPE_ROWS,
            computed_rows: MAX_COMPUTED_TYPE_ROWS,
        }
    }

    /// Conservative measured demand derived from entered bytes and the one
    /// selected language. Excess authority output is an exact capacity fault,
    /// never an eager max-of-seven-languages allocation.
    pub(crate) fn for_source(
        profile: compiler_vocabulary::LanguageProfile,
        source_bytes: usize,
    ) -> Self {
        let multiplier = match profile {
            compiler_vocabulary::LanguageProfile::TypeScript(_)
            | compiler_vocabulary::LanguageProfile::Python(_) => 4,
            compiler_vocabulary::LanguageProfile::C(_)
            | compiler_vocabulary::LanguageProfile::Cxx(_) => 3,
            compiler_vocabulary::LanguageProfile::Rust(_)
            | compiler_vocabulary::LanguageProfile::Go(_)
            | compiler_vocabulary::LanguageProfile::Java(_)
            | compiler_vocabulary::LanguageProfile::CSharp(_) => 2,
        };
        let units = source_bytes.saturating_add(1);
        let bounded = |value, maximum| value.clamp(8, maximum);
        let facts = bounded(units / 2 + 8, MAX_EMISSION_FACTS);
        Self {
            facts,
            // A reference occurrence owns at least one source byte.  This
            // preserves the measured C# 1,253-occurrence demand without
            // reserving eight thousand rows for a ten-byte source.
            occurrences: bounded(units, MAX_EMISSION_OCCURRENCES),
            docs: bounded(units.saturating_add(8), MAX_EMISSION_DOC_FRAGMENTS),
            extension_atoms: bounded(units / 2 + 8, MAX_EXTENSION_ATOMS),
            type_parameters: bounded(units / 2 + 8, MAX_TYPE_PARAMETERS),
            type_parameter_bounds: bounded(units, MAX_TYPE_PARAMETER_BOUNDS),
            ref_lists: bounded(facts, MAX_REF_LISTS),
            anonymous_rows: bounded(
                units.saturating_mul(multiplier).saturating_add(8),
                MAX_ANONYMOUS_TYPE_ROWS,
            ),
            computed_rows: bounded(
                units
                    .saturating_mul(multiplier.saturating_mul(2))
                    .saturating_add(8),
                MAX_COMPUTED_TYPE_ROWS,
            ),
        }
    }
}

/// The declared-type record lane row default: an honest unknown with its
/// reason cell, never a claimed shape the source did not spell.
pub(super) const fn opaque_record() -> SemanticTypeRecord<'static> {
    SemanticTypeRecord::leaf(SemanticTypeTag::Unknown)
}

/// One borrowed type-record child held before admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FactTypeChild<'source> {
    /// The nested type row this position targets: an already-pushed fact
    /// ordinal.
    pub(super) target: u32,
    /// Member or label spelling, where the record tag demands one.
    pub(super) name: Option<&'source [u8]>,
    /// Anonymous-record member flags.
    pub(super) flags: u8,
}

/// One ordered product child: a semantic role plus the fact ordinal whose
/// product it targets. Targets are backward references into the fact set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FactChild {
    role: ProductChildRole,
    target: u32,
}

/// One provable declaration fact of the multi-declaration emission lane.
///
/// The fact carries every canonical wire fact: the entity kind, the exact
/// name atom bytes, the declared-type fact, and the recursive type product
/// expressed through the canonical constructor plus its ordered role-bearing
/// children.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SemanticFact<'source> {
    kind: EntityKind,
    name: &'source [u8],
    type_record: SemanticTypeRecord<'source>,
    type_children: [FactTypeChild<'source>; MAX_TYPE_CHILDREN],
    type_child_count: u8,
    type_truncated: bool,
    constructor: SemanticProductConstructor,
    children: [FactChild; MAX_FACT_CHILDREN],
    child_count: u8,
    truncated: bool,
    extension: Option<EmissionExtension>,
    visibility: Visibility,
    /// Explicit producer-bound visibility-plane capture. An `Unknown`
    /// visibility value can be authority truth, so value inspection cannot
    /// establish this bit after admission.
    visibility_captured: bool,
}

/// One per-language extension fact committed beside a declaration.
///
/// The pooled list and atom coordinates are provisional until admission
/// fixes the final fragment lane layout; `admit` rewrites exactly the
/// provisional atom coordinates before encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EmissionExtension {
    /// TypeScript facts.
    TypeScript(compiler_ir::TypeScriptFacts),
    /// C# facts.
    CSharp(compiler_ir::CSharpFacts),
    /// Go facts.
    Go(compiler_ir::GoFacts),
    /// Rust facts.
    Rust(compiler_ir::RustFacts),
    /// Python facts.
    Python(compiler_ir::PythonFacts),
    /// Java facts.
    Java(compiler_ir::JavaFacts),
    /// Clang facts.
    Clang(compiler_ir::ClangFacts),
}

impl<'source> SemanticFact<'source> {
    /// Creates the zero-child fact for one provable declaration with an
    /// honestly unknown declared type.
    pub(super) const fn new(
        kind: EntityKind,
        name: &'source [u8],
        constructor: SemanticProductConstructor,
    ) -> Self {
        Self {
            kind,
            name,
            type_record: opaque_record(),
            type_children: [FactTypeChild {
                target: 0,
                name: None,
                flags: 0,
            }; MAX_TYPE_CHILDREN],
            type_child_count: 0,
            type_truncated: false,
            constructor,
            children: [FactChild {
                role: ProductChildRole::ProductMember,
                target: 0,
            }; MAX_FACT_CHILDREN],
            child_count: 0,
            truncated: false,
            extension: None,
            visibility: Visibility::Unknown,
            visibility_captured: false,
        }
    }

    /// Commits the declared-type record and its borrowed children.
    #[must_use]
    pub(super) const fn typed(mut self, record: SemanticTypeRecord<'source>) -> Self {
        self.type_record = record;
        self
    }

    /// Commits the source authority's written declaration visibility.
    #[must_use]
    pub(super) const fn with_visibility(mut self, visibility: Visibility) -> Self {
        self.visibility = visibility;
        self.visibility_captured = true;
        self
    }

    /// Appends one ordered type-record child targeting an already-pushed
    /// fact ordinal. Overflowing the bounded child lane sets the typed
    /// truncation flag so admission rejects the whole fact.
    #[must_use]
    pub(super) fn type_child(
        mut self,
        target: u32,
        name: Option<&'source [u8]>,
        flags: u8,
    ) -> Self {
        let ordinal = usize::from(self.type_child_count);
        if ordinal < MAX_TYPE_CHILDREN {
            self.type_children[ordinal] = FactTypeChild {
                target,
                name,
                flags,
            };
            self.type_child_count += 1;
        } else {
            self.type_truncated = true;
        }
        self
    }

    /// Commits one language extension fact for this declaration.
    #[must_use]
    pub(super) const fn with_extension(mut self, extension: EmissionExtension) -> Self {
        self.extension = Some(extension);
        self
    }

    /// Appends one literal text child (template-literal parts).
    #[must_use]
    pub(super) fn type_text_child(mut self, text: &'source [u8]) -> Self {
        let ordinal = usize::from(self.type_child_count);
        if ordinal < MAX_TYPE_CHILDREN {
            self.type_children[ordinal] = FactTypeChild {
                target: STAGED_TEXT_CHILD,
                name: Some(text),
                flags: 0,
            };
            self.type_child_count += 1;
        } else {
            self.type_truncated = true;
        }
        self
    }

    /// Appends one ordered product child. Overflowing the bounded child lane
    /// sets the fact's typed truncation flag so admission rejects the whole
    /// fact instead of silently dropping a proven child.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the recursive child input ships as the lane's tested contract; structural collectors prove zero-child products until native frontends lend member arenas"
        )
    )]
    #[expect(
        clippy::indexing_slicing,
        reason = "the child ordinal is admitted below MAX_FACT_CHILDREN before the single fixed-capacity slot write"
    )]
    pub(super) fn child(mut self, role: ProductChildRole, target: u32) -> Self {
        let ordinal = usize::from(self.child_count);
        if ordinal < MAX_FACT_CHILDREN {
            self.children[ordinal] = FactChild { role, target };
            self.child_count += 1;
        } else {
            self.truncated = true;
        }
        self
    }
}

use crate::types::{FactFault, FactRejection, ParentageState};

/// Exact rejection of one fact at admission, retaining the offending ordinal,
/// its exact name bytes, and the typed cause. The shared terminal projects
/// this borrow into the value-typed [`FactRejection`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RejectedFact<'source> {
    pub(super) fact: usize,
    pub(super) name: &'source [u8],
    pub(super) cause: FactFault,
}

impl RejectedFact<'_> {
    /// Value snapshot that outlives the collector arena.
    pub(super) const fn rejection(&self) -> FactRejection {
        FactRejection {
            fact: self.fact,
            name_len: self.name.len(),
            cause: self.cause,
        }
    }
}

/// Caller-owned bounded SoA lanes for the ordered emission set. Only the
/// admitted prefix is read by [`admit`]; slots past `len` are never observed.
pub(super) struct FactSet<'source> {
    plan: ResourcePlan,
    primary_source_len: Option<u32>,
    len: usize,
    total_children: usize,
    kinds: Box<[EntityKind]>,
    names: Box<[&'source [u8]]>,
    type_records: Box<[SemanticTypeRecord<'source>]>,
    type_child_targets: Box<[u32]>,
    type_child_names: Box<[Option<&'source [u8]>]>,
    type_child_flags: Box<[u8]>,
    type_child_counts: Box<[u8]>,
    type_child_starts: Box<[u32]>,
    total_type_children: usize,
    constructors: Box<[SemanticProductConstructor]>,
    child_roles: Box<[ProductChildRole]>,
    child_targets: Box<[u32]>,
    child_counts: Box<[u8]>,
    child_starts: Box<[u32]>,
    extensions: Box<[Option<EmissionExtension>]>,
    key_digests: Box<[PayloadHash]>,
    visibility: Box<[Visibility]>,
    visibility_captured: Box<[bool]>,
    documentation_captured: Box<[bool]>,
    parentage: Box<[StagedParentage]>,
    source_spans: Box<[Option<StagedSourceSpan>]>,
    occurrence_owners: Box<[u32]>,
    occurrences: Box<[Occurrence<'source>]>,
    occurrence_len: usize,
    doc_facts: Box<[DocFactInput<'source>]>,
    doc_len: usize,
    extension_atoms: Box<[&'source [u8]]>,
    extension_atom_len: usize,
    type_parameters: Box<[ExtensionTypeParameter<'source>]>,
    type_parameter_len: usize,
    type_parameter_bounds: Box<[compiler_ir::ExtensionTypeParameterBound<'source>]>,
    type_parameter_bound_len: usize,
    type_parameter_ranges: Box<[Option<StagedTypeParameterRange>]>,
    atom_lists: Box<[[u32; MAX_REF_LIST_ELEMENTS]]>,
    atom_list_lengths: Box<[u8]>,
    atom_list_len: usize,
    type_lists: Box<[[u32; MAX_REF_LIST_ELEMENTS]]>,
    type_list_lengths: Box<[u8]>,
    type_list_len: usize,
    entity_lists: Box<[[u32; MAX_REF_LIST_ELEMENTS]]>,
    entity_list_lengths: Box<[u8]>,
    entity_list_len: usize,
    anonymous_records: Box<[SemanticTypeRecord<'source>]>,
    anonymous_owners: Box<[u32]>,
    anonymous_child_starts: Box<[u32]>,
    anonymous_child_counts: Box<[u8]>,
    anonymous_child_targets: Box<[u32]>,
    anonymous_child_names: Box<[Option<&'source [u8]>]>,
    anonymous_child_flags: Box<[u8]>,
    anonymous_rows: usize,
    anonymous_children_total: usize,
    anonymous_child_pending: u32,
    computed_records: Box<[SemanticTypeRecord<'source>]>,
    computed_owners: Box<[u32]>,
    computed_child_starts: Box<[u32]>,
    computed_child_counts: Box<[u8]>,
    computed_child_targets: Box<[u32]>,
    computed_child_names: Box<[Option<&'source [u8]>]>,
    computed_child_flags: Box<[u8]>,
    computed_rows: usize,
    computed_children_total: usize,
    computed_child_pending: u32,
}

/// A source range captured by an authority before it reaches the owned tree.
/// It is intentionally distinct from wire/type coordinates: only `build_ir`
/// binds its request-local file atom and constructs `compiler_ir::SourceSpan`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StagedSourceSpan {
    start: u32,
    end: u32,
}

/// An explicit transaction-local type-parameter range.  The public semantic
/// ID is dense only after admission; a bare staging start is ambiguous when
/// an empty declaration precedes the first generic declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StagedTypeParameterRange {
    start: u32,
    length: u32,
}

/// Staging uses the public closed transition state directly so a fact
/// rejection can retain both conflicting authority claims without erasure.
type StagedParentage = ParentageState;

/// Extracts the one local parent representable in the owned/compact views.
/// Root, unavailable, and unrepresented native ownership deliberately never
/// fabricate an entity coordinate.
const fn local_parent(parentage: StagedParentage) -> Option<compiler_ir::EntityId> {
    match parentage {
        StagedParentage::Bound { parent } => Some(parent),
        StagedParentage::Unavailable
        | StagedParentage::Root
        | StagedParentage::UnrepresentedAuthorityOwner { .. } => None,
    }
}

impl StagedSourceSpan {
    pub(super) const fn new(start: u32, end: u32) -> Option<Self> {
        (start <= end).then_some(Self { start, end })
    }
}

// The boxed lanes keep this caller-owned collector below the 64 KiB stack
// budget; each box is allocated exactly once by FactSet::new and lives with
// its FactSet, rather than being grown during admission.
const _: () = assert!(size_of::<FactSet<'static>>() <= 64 * 1024);

impl<'source> FactSet<'source> {
    fn staged_type_slot(&self, row: u32) -> Option<usize> {
        if row < ANONYMOUS_ROW_BASE {
            return (row as usize < self.len).then_some(row as usize);
        }
        if row >= COMPUTED_ROW_BASE {
            let computed = row - COMPUTED_ROW_BASE;
            return (computed as usize < self.computed_rows)
                .then_some(self.len + self.anonymous_rows + computed as usize);
        }
        let anonymous = row - ANONYMOUS_ROW_BASE;
        (anonymous as usize < self.anonymous_rows).then_some(self.len + anonymous as usize)
    }

    fn is_anonymous_type_row(&self, row: u32) -> bool {
        row >= ANONYMOUS_ROW_BASE
            && row < COMPUTED_ROW_BASE
            && (row - ANONYMOUS_ROW_BASE) as usize < self.anonymous_rows
    }

    fn is_computed_type_row(&self, row: u32) -> bool {
        row >= COMPUTED_ROW_BASE && (row - COMPUTED_ROW_BASE) as usize < self.computed_rows
    }

    /// Conservative active compound-projection bound for this transaction.
    ///
    /// A scratch lane is a depth-first stack, not a collection of unrelated
    /// row maxima: a parent prefix can remain live while one of its compound
    /// children is being completed.  Planning only the largest individual
    /// row therefore under-allocates nested tuples/objects.  This walk
    /// derives a safe maximum live path per typed scratch lane while retaining
    /// independent per-family allocations. Cache/order can make the observed
    /// peak smaller; this bound must never be smaller than that peak.
    fn projected_type_demand(&self) -> ProjectionDemand {
        let row_count = self.len + self.anonymous_rows + self.computed_rows;
        let mut state = vec![0_u8; row_count].into_boxed_slice();
        let mut cached = vec![ProjectionDemand::default(); row_count].into_boxed_slice();
        let mut demand = ProjectionDemand::default();
        for ordinal in 0..self.len {
            demand = demand.maximum(self.projected_type_demand_from(
                ordinal as u32,
                &mut state,
                &mut cached,
            ));
        }
        for ordinal in 0..self.anonymous_rows {
            demand = demand.maximum(self.projected_type_demand_from(
                ANONYMOUS_ROW_BASE + ordinal as u32,
                &mut state,
                &mut cached,
            ));
        }
        for ordinal in 0..self.computed_rows {
            demand = demand.maximum(self.projected_type_demand_from(
                COMPUTED_ROW_BASE + ordinal as u32,
                &mut state,
                &mut cached,
            ));
        }
        demand
    }

    fn projected_type_demand_from(
        &self,
        row: u32,
        state: &mut [u8],
        cached: &mut [ProjectionDemand],
    ) -> ProjectionDemand {
        let Some(index) = self.staged_type_slot(row) else {
            return ProjectionDemand::default();
        };
        match state[index] {
            2 => return cached[index],
            // The owned projector later rejects this as `RecursiveType`.
            // Treating the back-edge as no additional scratch keeps planning
            // finite without turning a semantic fault into an allocation
            // policy.
            1 => return ProjectionDemand::default(),
            _ => {}
        }
        state[index] = 1;
        let record = self.type_record_for_demand(row);
        let child_count = self.type_child_count_for_demand(row);
        let mut deepest_child = ProjectionDemand::default();
        for position in 0..child_count {
            let Some((target, _, _)) = self.staged_type_child(row, position) else {
                continue;
            };
            if target != STAGED_TEXT_CHILD {
                deepest_child = deepest_child.maximum(self.projected_type_demand_from(
                    target,
                    state,
                    cached,
                ));
            }
        }
        let demand = ProjectionDemand::for_row(record.tag, child_count).with_child(deepest_child);
        state[index] = 2;
        cached[index] = demand;
        demand
    }

    fn type_record_for_demand(&self, row: u32) -> SemanticTypeRecord<'source> {
        debug_assert!(self.staged_type_slot(row).is_some());
        if row < ANONYMOUS_ROW_BASE {
            self.type_records[row as usize]
        } else if self.is_anonymous_type_row(row) {
            self.anonymous_records[(row - ANONYMOUS_ROW_BASE) as usize]
        } else {
            self.computed_records[(row - COMPUTED_ROW_BASE) as usize]
        }
    }

    fn type_child_count_for_demand(&self, row: u32) -> usize {
        debug_assert!(self.staged_type_slot(row).is_some());
        if row < ANONYMOUS_ROW_BASE {
            usize::from(self.type_child_counts[row as usize])
        } else if self.is_anonymous_type_row(row) {
            usize::from(self.anonymous_child_counts[(row - ANONYMOUS_ROW_BASE) as usize])
        } else {
            usize::from(self.computed_child_counts[(row - COMPUTED_ROW_BASE) as usize])
        }
    }
    /// Full protocol plan for direct lowerer boundary tests.
    pub(super) fn new() -> Self {
        Self::with_plan(ResourcePlan::protocol_maximum())
    }

    /// Production collector allocation under one entered request plan.
    pub(super) fn with_plan(plan: ResourcePlan) -> Self {
        Self::with_source_plan(plan, None)
    }

    /// Production collector allocation bound to the one entered primary
    /// source lease.  Unit-level lowerer probes may intentionally omit a
    /// source authority; production cannot.
    pub(super) fn with_primary_source(plan: ResourcePlan, source_len: u32) -> Self {
        Self::with_source_plan(plan, Some(source_len))
    }

    fn with_source_plan(plan: ResourcePlan, primary_source_len: Option<u32>) -> Self {
        let empty_name: &[u8] = &[];
        Self {
            plan,
            primary_source_len,
            len: 0,
            total_children: 0,
            kinds: vec![EntityKind::Function; plan.facts].into_boxed_slice(),
            names: vec![empty_name; plan.facts].into_boxed_slice(),
            type_records: vec![opaque_record(); plan.facts].into_boxed_slice(),
            type_child_targets: vec![0; plan.facts * MAX_TYPE_CHILDREN].into_boxed_slice(),
            type_child_names: vec![None; plan.facts * MAX_TYPE_CHILDREN].into_boxed_slice(),
            type_child_flags: vec![0; plan.facts * MAX_TYPE_CHILDREN].into_boxed_slice(),
            type_child_counts: vec![0; plan.facts].into_boxed_slice(),
            type_child_starts: vec![0; plan.facts].into_boxed_slice(),
            total_type_children: 0,
            constructors: vec![SemanticProductConstructor::PRODUCT; plan.facts]
                .into_boxed_slice(),
            child_roles: vec![
                ProductChildRole::ProductMember;
                plan.facts * MAX_FACT_CHILDREN
            ]
            .into_boxed_slice(),
            child_targets: vec![0; plan.facts * MAX_FACT_CHILDREN].into_boxed_slice(),
            child_counts: vec![0; plan.facts].into_boxed_slice(),
            child_starts: vec![0; plan.facts].into_boxed_slice(),
            extensions: vec![None; plan.facts].into_boxed_slice(),
            key_digests: vec![PayloadHash::from_raw([0; 16]); plan.facts].into_boxed_slice(),
            visibility: vec![Visibility::Unknown; plan.facts].into_boxed_slice(),
            visibility_captured: vec![false; plan.facts].into_boxed_slice(),
            documentation_captured: vec![false; plan.facts].into_boxed_slice(),
            parentage: vec![StagedParentage::Unavailable; plan.facts].into_boxed_slice(),
            source_spans: vec![None; plan.facts].into_boxed_slice(),
            occurrence_owners: vec![0; plan.occurrences].into_boxed_slice(),
            occurrences: vec![
                Occurrence {
                    target: compiler_ir::OccurrenceTarget::Foreign(compiler_ir::ForeignKey {
                        origin: compiler_ir::ForeignOrigin::Universe { ecosystem: "" },
                        path: "",
                        display: "",
                        kind: None,
                    }),
                    kind: compiler_ir::ReferenceKind::FunctionCall,
                    confidence: compiler_ir::OccurrenceConfidence::Syntactic,
                    span: compiler_ir::RelSpan { start: 0, end: 0 },
                };
                plan.occurrences
            ]
            .into_boxed_slice(),
            occurrence_len: 0,
            doc_facts: vec![
                DocFactInput {
                    owner: compiler_ir::EntityId::new(0),
                    fragment: DocFragmentInput::SoftBreak,
                };
                plan.docs
            ]
            .into_boxed_slice(),
            doc_len: 0,
            extension_atoms: vec![empty_name; plan.extension_atoms].into_boxed_slice(),
            extension_atom_len: 0,
            type_parameters: vec![
                ExtensionTypeParameter {
                    name: &[],
                    bounds: compiler_ir::ExtensionTypeParameterBoundRange {
                        start: 0,
                        length: 0,
                    },
                    default: None,
                    variance: compiler_ir::Variance::Invariant,
                    kind: compiler_ir::ExtensionTypeParameterKind::Type {
                        inference: compiler_ir::TypeParameterInference::Ordinary,
                    },
                    requirements: compiler_ir::TypeParameterRequirements::none(),
                };
                plan.type_parameters
            ]
            .into_boxed_slice(),
            type_parameter_len: 0,
            type_parameter_bounds: vec![
                compiler_ir::ExtensionTypeParameterBound::Type(0);
                plan.type_parameter_bounds
            ]
            .into_boxed_slice(),
            type_parameter_bound_len: 0,
            type_parameter_ranges: vec![None; plan.facts].into_boxed_slice(),
            atom_lists: vec![[0; MAX_REF_LIST_ELEMENTS]; plan.ref_lists].into_boxed_slice(),
            atom_list_lengths: vec![0; plan.ref_lists].into_boxed_slice(),
            atom_list_len: 0,
            type_lists: vec![[0; MAX_REF_LIST_ELEMENTS]; plan.ref_lists].into_boxed_slice(),
            type_list_lengths: vec![0; plan.ref_lists].into_boxed_slice(),
            type_list_len: 0,
            entity_lists: vec![[0; MAX_REF_LIST_ELEMENTS]; plan.ref_lists].into_boxed_slice(),
            entity_list_lengths: vec![0; plan.ref_lists].into_boxed_slice(),
            entity_list_len: 0,
            anonymous_records: vec![opaque_record(); plan.anonymous_rows].into_boxed_slice(),
            anonymous_owners: vec![0; plan.anonymous_rows].into_boxed_slice(),
            anonymous_child_starts: vec![0; plan.anonymous_rows].into_boxed_slice(),
            anonymous_child_counts: vec![0; plan.anonymous_rows].into_boxed_slice(),
            anonymous_child_targets: vec![0; plan.anonymous_rows * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            anonymous_child_names: vec![None; plan.anonymous_rows * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            anonymous_child_flags: vec![0; plan.anonymous_rows * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            anonymous_rows: 0,
            anonymous_children_total: 0,
            anonymous_child_pending: 0,
            computed_records: vec![opaque_record(); plan.computed_rows].into_boxed_slice(),
            computed_owners: vec![0; plan.computed_rows].into_boxed_slice(),
            computed_child_starts: vec![0; plan.computed_rows].into_boxed_slice(),
            computed_child_counts: vec![0; plan.computed_rows].into_boxed_slice(),
            computed_child_targets: vec![0; plan.computed_rows * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            computed_child_names: vec![None; plan.computed_rows * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            computed_child_flags: vec![0; plan.computed_rows * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            computed_rows: 0,
            computed_children_total: 0,
            computed_child_pending: 0,
        }
    }

    /// Number of admitted facts.
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// Makes authority availability explicit beside optional owned-IR fields.
    /// A `None` inside `Ir` is never promoted to proof that the source lacked
    /// that fact: this sidecar names which authority planes were unavailable.
    pub(super) fn rich_capture(&self) -> crate::types::RichIrCapture {
        let capture = |captured| {
            if captured {
                crate::types::RichCapture::Captured
            } else {
                crate::types::RichCapture::Unavailable
            }
        };
        let entities = (0..self.len)
            .map(|ordinal| {
                let source = self.source_spans[ordinal].is_some();
                let attributes = self.extensions[ordinal]
                    .as_ref()
                    .and_then(extension_item_attributes)
                    .is_some();
                crate::types::RichEntityCapture {
                    parentage: match self.parentage[ordinal] {
                        StagedParentage::Unavailable => {
                            crate::types::RichParentageCapture::Unavailable
                        }
                        StagedParentage::Root => crate::types::RichParentageCapture::Root,
                        StagedParentage::Bound { .. } => crate::types::RichParentageCapture::Bound,
                        StagedParentage::UnrepresentedAuthorityOwner { identity } => {
                            crate::types::RichParentageCapture::UnrepresentedAuthorityOwner {
                                identity,
                            }
                        }
                    },
                    source: if source {
                        crate::types::RichCapture::Captured
                    } else {
                        crate::types::RichCapture::Unavailable
                    },
                    source_file: if source {
                        crate::types::RichCapture::Captured
                    } else {
                        crate::types::RichCapture::Unavailable
                    },
                    members: if matches!(
                        self.parentage[ordinal],
                        StagedParentage::Root | StagedParentage::Bound { .. }
                    ) {
                        crate::types::RichCapture::Captured
                    } else {
                        crate::types::RichCapture::Unavailable
                    },
                    // Every admitted semantic fact has a closed type record;
                    // an explicit `Unknown` record is truth, not absence.
                    semantic_type: crate::types::RichCapture::Captured,
                    documentation: capture(self.documentation_captured[ordinal]),
                    visibility: capture(self.visibility_captured[ordinal]),
                    attributes: if attributes {
                        crate::types::RichCapture::Captured
                    } else {
                        crate::types::RichCapture::Unavailable
                    },
                    extension: if self.extensions[ordinal].is_some() {
                        crate::types::RichCapture::Captured
                    } else {
                        crate::types::RichCapture::Unavailable
                    },
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let occurrences = self.occurrence_owners[..self.occurrence_len]
            .iter()
            .map(|owner| {
                // Span rows may attach after references in a two-pass native
                // authority. Capture derives from final staging truth, not
                // the push-time order of those passes.
                if self.source_spans[*owner as usize].is_some() {
                    crate::types::RichCapture::Captured
                } else {
                    crate::types::RichCapture::Unavailable
                }
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        crate::types::RichIrCapture {
            entities,
            occurrences,
        }
    }

    /// Interns one anonymous type row owned by an already-pushed fact: the
    /// home of every compound type (applications, arrays, pointers,
    /// builtins) that is not itself a declaration. Children of the new row
    /// are appended with [`FactSet::anonymous_type_child`] before the next
    /// row is interned, keeping the pooled lane topologically backward.
    /// The returned coordinate is the row's type-lane ordinal; fact rows
    /// occupy `0..len`, anonymous rows follow.
    pub(super) fn intern_anonymous_type_row(
        &mut self,
        owner: u32,
        record: SemanticTypeRecord<'source>,
    ) -> Result<u32, FactFault> {
        if owner >= self.len as u32 {
            return Err(FactFault::RefTarget {
                lane: "type_rows",
                raw: owner,
                fact_count: self.len,
            });
        }
        if self.anonymous_rows == self.plan.anonymous_rows {
            return Err(FactFault::TypeRowCapacity);
        }
        let child_count = self.anonymous_child_pending;
        record
            .validate(child_count)
            .map_err(FactFault::TypeRecord)?;
        self.validate_pending_anonymous_children(record, child_count)?;
        let row = ANONYMOUS_ROW_BASE + self.anonymous_rows as u32;
        let index = self.anonymous_rows;
        self.anonymous_records[index] = record;
        self.anonymous_owners[index] = owner;
        // The row's own children were appended before this intern (the go-lane
        // order): they occupy the trailing `child_count` slots, so the recorded
        // start is the range begin, not the post-append total.
        self.anonymous_child_starts[index] =
            (self.anonymous_children_total - child_count as usize) as u32;
        self.anonymous_child_counts[index] = child_count as u8;
        self.anonymous_rows += 1;
        self.anonymous_child_pending = 0;
        Ok(row)
    }

    /// Interns an anonymous row for a fact reserved immediately after the
    /// current fact prefix. The owner is admitted before that fact exists;
    /// the caller must push it next.
    pub(super) fn intern_reserved_anchor_type_row(
        &mut self,
        reserved_owner: u32,
        record: SemanticTypeRecord<'source>,
    ) -> Result<u32, FactFault> {
        debug_assert_eq!(reserved_owner, self.len as u32);
        if reserved_owner != self.len as u32 {
            return Err(FactFault::RefTarget {
                lane: "reserved_type_rows",
                raw: reserved_owner,
                fact_count: self.len,
            });
        }
        if self.anonymous_rows == self.plan.anonymous_rows {
            return Err(FactFault::TypeRowCapacity);
        }
        let child_count = self.anonymous_child_pending;
        record
            .validate(child_count)
            .map_err(FactFault::TypeRecord)?;
        self.validate_pending_anonymous_children(record, child_count)?;
        let row = ANONYMOUS_ROW_BASE + self.anonymous_rows as u32;
        let index = self.anonymous_rows;
        self.anonymous_records[index] = record;
        self.anonymous_owners[index] = reserved_owner;
        self.anonymous_child_starts[index] =
            (self.anonymous_children_total - child_count as usize) as u32;
        self.anonymous_child_counts[index] = child_count as u8;
        self.anonymous_rows += 1;
        self.anonymous_child_pending = 0;
        Ok(row)
    }

    /// Appends one ordered child to the anonymous row currently being built.
    /// The target must name an already-interned anonymous row or an
    /// already-pushed fact; the lane rejects forward coordinates.
    pub(super) fn anonymous_type_child(
        &mut self,
        target: u32,
        name: Option<&'source [u8]>,
        flags: u8,
    ) -> Result<(), FactFault> {
        let valid = self.is_anonymous_type_row(target) || target < self.len as u32;
        if !valid {
            return Err(FactFault::TypeChildTarget {
                position: self.anonymous_child_pending as usize,
                target,
                fact_count: self.len,
            });
        }
        if self.anonymous_child_pending == MAX_TYPE_CHILDREN as u32
            || self.anonymous_children_total == self.anonymous_child_targets.len()
        {
            return Err(FactFault::TypeChildCapacity);
        }
        let pooled = self.anonymous_children_total;
        self.anonymous_child_targets[pooled] = target;
        self.anonymous_child_names[pooled] = name;
        self.anonymous_child_flags[pooled] = flags;
        self.anonymous_children_total += 1;
        self.anonymous_child_pending += 1;
        Ok(())
    }

    /// Validates the pending anonymous child range as one closed row before
    /// committing it. This is where function result boundaries and a
    /// variadic final parameter are proven for anonymous/root callable types.
    fn validate_pending_anonymous_children(
        &self,
        record: SemanticTypeRecord<'source>,
        child_count: u32,
    ) -> Result<(), FactFault> {
        let child_start = self.anonymous_children_total - child_count as usize;
        for position in 0..child_count as usize {
            let pooled = child_start + position;
            record
                .validate_child_in_row(
                    position as u32,
                    child_count,
                    &SemanticTypeChild {
                        target: TypeChildTarget::Type(compiler_ir::TypeRef::Local(
                            compiler_ir::TypeId::new(self.anonymous_child_targets[pooled]),
                        )),
                        name: self.anonymous_child_names[pooled],
                        flags: self.anonymous_child_flags[pooled],
                    },
                )
                .map_err(|fault| FactFault::TypeChild { position, fault })?;
        }
        Ok(())
    }

    /// Interns one checker-computed row in the schema-2-only lane.
    pub(super) fn intern_computed_type_row(
        &mut self,
        owner: u32,
        record: SemanticTypeRecord<'source>,
    ) -> Result<u32, FactFault> {
        if owner >= self.len as u32 {
            return Err(FactFault::RefTarget {
                lane: "computed_owners",
                raw: owner,
                fact_count: self.len,
            });
        }
        if self.computed_rows == self.plan.computed_rows {
            return Err(FactFault::ComputedRowCapacity);
        }
        let child_count = self.computed_child_pending;
        record
            .validate(child_count)
            .map_err(FactFault::TypeRecord)?;
        let child_start = self.computed_children_total - child_count as usize;
        for position in 0..child_count as usize {
            let pooled = child_start + position;
            let target = if self.computed_child_targets[pooled] == STAGED_TEXT_CHILD {
                TypeChildTarget::Text
            } else {
                TypeChildTarget::Type(compiler_ir::TypeRef::Local(compiler_ir::TypeId::new(
                    self.computed_child_targets[pooled],
                )))
            };
            record
                .validate_child_in_row(
                    position as u32,
                    child_count,
                    &SemanticTypeChild {
                        target,
                        name: self.computed_child_names[pooled],
                        flags: self.computed_child_flags[pooled],
                    },
                )
                .map_err(|fault| FactFault::TypeChild { position, fault })?;
        }
        let index = self.computed_rows;
        self.computed_records[index] = record;
        self.computed_owners[index] = owner;
        self.computed_child_starts[index] =
            (self.computed_children_total - child_count as usize) as u32;
        self.computed_child_counts[index] = child_count as u8;
        self.computed_rows += 1;
        self.computed_child_pending = 0;
        Ok(COMPUTED_ROW_BASE + index as u32)
    }

    /// Appends a child to the computed row currently being built. Computed
    /// targets are either declared rows or strictly earlier computed rows.
    pub(super) fn computed_type_child(
        &mut self,
        target: u32,
        name: Option<&'source [u8]>,
        flags: u8,
    ) -> Result<(), FactFault> {
        let valid = target == STAGED_TEXT_CHILD
            || self.is_computed_type_row(target)
            || self.is_anonymous_type_row(target)
            || target < self.len as u32;
        if !valid {
            return Err(FactFault::TypeChildTarget {
                position: self.computed_child_pending as usize,
                target,
                fact_count: self.len,
            });
        }
        if self.computed_child_pending == MAX_TYPE_CHILDREN as u32
            || self.computed_children_total == self.computed_child_targets.len()
        {
            return Err(FactFault::TypeChildCapacity);
        }
        let pooled = self.computed_children_total;
        self.computed_child_targets[pooled] = target;
        self.computed_child_names[pooled] = name;
        self.computed_child_flags[pooled] = flags;
        self.computed_children_total += 1;
        self.computed_child_pending += 1;
        Ok(())
    }

    /// Appends one literal template segment to the computed lane. It is kept
    /// distinct from a type coordinate all the way to `TypeChildTarget::Text`.
    pub(super) fn computed_type_text_child(
        &mut self,
        text: &'source [u8],
    ) -> Result<(), FactFault> {
        self.computed_type_child(STAGED_TEXT_CHILD, Some(text), 0)
    }

    /// Attaches one language extension to an already-pushed fact whose
    /// extension references members that only exist after the two-pass
    /// declaration order (record components, method sets).
    pub(super) fn attach_extension(
        &mut self,
        ordinal: usize,
        extension: EmissionExtension,
    ) -> Result<(), FactFault> {
        if ordinal >= self.len {
            return Err(FactFault::RefTarget {
                lane: "extensions",
                raw: ordinal as u32,
                fact_count: self.len,
            });
        }
        // A replacement after another declaration appended parameters cannot
        // infer its list end from the ambient cursor.  Producers that replace
        // a generic extension must pass their immutable close-time range via
        // `attach_extension_with_type_parameters`.
        if self.type_parameter_ranges[ordinal].is_some()
            && extension_type_parameter_start(&extension).is_some()
        {
            return Err(FactFault::RefTarget {
                lane: "replacement_type_parameter_range",
                raw: ordinal as u32,
                fact_count: self.type_parameter_len,
            });
        }
        if self.type_parameter_ranges[ordinal].is_none() {
            self.type_parameter_ranges[ordinal] = self.capture_type_parameter_range(&extension)?;
        }
        self.extensions[ordinal] = Some(extension);
        Ok(())
    }

    /// Captures an explicit list range at the producer's close point.  This
    /// is the only valid operation for a deferred extension replacement:
    /// attachment time is not a list boundary.
    pub(super) fn type_parameter_range(
        &self,
        start: u32,
    ) -> Result<StagedTypeParameterRange, FactFault> {
        let start = start as usize;
        if start > self.type_parameter_len {
            return Err(FactFault::RefTarget {
                lane: "type_parameter_ranges",
                raw: start as u32,
                fact_count: self.type_parameter_len,
            });
        }
        Ok(StagedTypeParameterRange {
            start: start as u32,
            length: (self.type_parameter_len - start) as u32,
        })
    }

    /// Retrieves the immutable close-time list handle of an admitted generic
    /// declaration.  Deferred passes use this rather than reading the shared
    /// pool cursor, which may now belong to an unrelated declaration.
    pub(super) fn captured_type_parameter_range(
        &self,
        ordinal: usize,
    ) -> Result<StagedTypeParameterRange, FactFault> {
        self.type_parameter_ranges
            .get(ordinal)
            .copied()
            .flatten()
            .ok_or(FactFault::RefTarget {
                lane: "captured_type_parameter_range",
                raw: ordinal as u32,
                fact_count: self.len,
            })
    }

    /// Replaces an extension and binds the already-closed parameter range
    /// supplied by its producer, never the ambient pool cursor.
    pub(super) fn attach_extension_with_type_parameters(
        &mut self,
        ordinal: usize,
        extension: EmissionExtension,
        range: StagedTypeParameterRange,
    ) -> Result<(), FactFault> {
        if ordinal >= self.len {
            return Err(FactFault::RefTarget {
                lane: "extensions",
                raw: ordinal as u32,
                fact_count: self.len,
            });
        }
        let range_end = range.start.checked_add(range.length);
        if extension_type_parameter_start(&extension).map(|start| start.raw) != Some(range.start)
            || !matches!(range_end, Some(end) if end as usize <= self.type_parameter_len)
            || matches!(self.type_parameter_ranges[ordinal], Some(existing) if existing != range)
        {
            return Err(FactFault::RefTarget {
                lane: "type_parameter_ranges",
                raw: range.start,
                fact_count: self.type_parameter_len,
            });
        }
        self.type_parameter_ranges[ordinal] = Some(range);
        self.extensions[ordinal] = Some(extension);
        Ok(())
    }

    fn capture_type_parameter_range(
        &self,
        extension: &EmissionExtension,
    ) -> Result<Option<StagedTypeParameterRange>, FactFault> {
        let Some(start) = extension_type_parameter_start(extension) else {
            return Ok(None);
        };
        let start = start.raw as usize;
        if start > self.type_parameter_len {
            return Err(FactFault::RefTarget {
                lane: "type_parameter_ranges",
                raw: start as u32,
                fact_count: self.type_parameter_len,
            });
        }
        Ok(Some(StagedTypeParameterRange {
            start: start as u32,
            length: (self.type_parameter_len - start) as u32,
        }))
    }

    /// Performs the one legal parentage transition for an admitted entity.
    /// Identical repeated authority observations are idempotent; any other
    /// second claim retains both states as an exact fact fault.
    fn transition_parentage(
        &mut self,
        entity: u32,
        requested: StagedParentage,
    ) -> Result<(), FactFault> {
        let ordinal = entity as usize;
        let existing = self.parentage[ordinal];
        if existing == StagedParentage::Unavailable {
            self.parentage[ordinal] = requested;
            return Ok(());
        }
        if existing == requested {
            return Ok(());
        }
        Err(FactFault::ConflictingParentage {
            entity: compiler_ir::EntityId::new(entity),
            existing,
            requested,
        })
    }

    /// Binds a child declaration to an already admitted parent.  Parentage
    /// stays in the transaction staging lane, so the compact and owned views
    /// derive their relation from one authority fact rather than a renderer
    /// side channel.
    pub(super) fn attach_parent(&mut self, child: u32, parent: u32) -> Result<(), FactFault> {
        if child as usize >= self.len || parent as usize >= self.len || child == parent {
            return Err(FactFault::RefTarget {
                lane: "entity_parents",
                raw: if child as usize >= self.len { child } else { parent },
                fact_count: self.len,
            });
        }
        self.transition_parentage(
            child,
            StagedParentage::Bound {
                parent: compiler_ir::EntityId::new(parent),
            },
        )
    }

    /// Marks that an authority considered parentage for this row even when it
    /// proved the row is a root.  This is distinct from source-span capture.
    pub(super) fn mark_parentage_root(&mut self, entity: u32) -> Result<(), FactFault> {
        if entity as usize >= self.len {
            return Err(FactFault::RefTarget {
                lane: "entity_parentage",
                raw: entity,
                fact_count: self.len,
            });
        }
        self.transition_parentage(entity, StagedParentage::Root)
    }

    /// Retains an authoritative native owner which has no emitted row.  It
    /// cannot be represented as a root or fabricated as a local parent.
    pub(super) fn mark_unrepresented_parent(
        &mut self,
        entity: u32,
        authority_identity: [u8; 16],
    ) -> Result<(), FactFault> {
        if entity as usize >= self.len {
            return Err(FactFault::RefTarget {
                lane: "entity_parentage",
                raw: entity,
                fact_count: self.len,
            });
        }
        self.transition_parentage(
            entity,
            StagedParentage::UnrepresentedAuthorityOwner {
                identity: authority_identity,
            },
        )
    }

    /// Binds one authority-captured declaration span without giving a raw
    /// image coordinate the ability to masquerade as a type or entity ID.
    pub(super) fn attach_source_span(
        &mut self,
        entity: u32,
        span: StagedSourceSpan,
    ) -> Result<(), FactFault> {
        if entity as usize >= self.len {
            return Err(FactFault::RefTarget {
                lane: "entity_source_spans",
                raw: entity,
                fact_count: self.len,
            });
        }
        if let Some(source_len) = self.primary_source_len
            && span.end > source_len
        {
            return Err(FactFault::SourceSpan {
                entity,
                start: span.start,
                end: span.end,
                source_len,
            });
        }
        let slot = &mut self.source_spans[entity as usize];
        *slot = Some(span);
        Ok(())
    }

    /// First pooled position of one fact's type-record children.
    pub(super) fn type_children_base(&self, ordinal: usize) -> usize {
        self.type_child_counts[..ordinal]
            .iter()
            .map(|count| usize::from(*count))
            .sum()
    }

    /// One pooled type-record child of a fact row by absolute position.
    #[expect(
        clippy::type_complexity,
        reason = "the borrowed child triple is the pooled lane's own representation"
    )]
    pub(super) fn type_child_flat(&self, pooled: usize) -> (u32, Option<&'source [u8]>, u8) {
        (
            self.type_child_targets[pooled],
            self.type_child_names[pooled],
            self.type_child_flags[pooled],
        )
    }

    /// Returns one staged type row without exposing its storage plane.  The
    /// declared, anonymous, and observed-computed planes deliberately share
    /// this one projection boundary: consumers cannot accidentally treat a
    /// computed coordinate as an anonymous coordinate.
    fn staged_type_record(
        &self,
        row: u32,
    ) -> Result<SemanticTypeRecord<'source>, compiler_ir::BuildError> {
        if row < ANONYMOUS_ROW_BASE {
            return self.type_records.get(row as usize).copied().ok_or(
                compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Type,
                    raw: row,
                },
            );
        }
        if self.is_anonymous_type_row(row) {
            return Ok(self.anonymous_records[(row - ANONYMOUS_ROW_BASE) as usize]);
        }
        if self.is_computed_type_row(row) {
            return Ok(self.computed_records[(row - COMPUTED_ROW_BASE) as usize]);
        }
        Err(compiler_ir::BuildError::Dangling { space: compiler_ir::SemanticSpace::Type, raw: row })
    }

    /// Returns one staged child together with its exact text/tag payload.
    fn staged_type_child(
        &self,
        row: u32,
        position: usize,
    ) -> Option<(u32, Option<&'source [u8]>, u8)> {
        if row < ANONYMOUS_ROW_BASE {
            let index = row as usize;
            return (position < usize::from(*self.type_child_counts.get(index)?))
                .then(|| self.type_child_flat(self.type_children_base(index) + position));
        }
        if self.is_anonymous_type_row(row) {
            let anonymous = (row - ANONYMOUS_ROW_BASE) as usize;
            return (position < usize::from(self.anonymous_child_counts[anonymous])).then(|| {
                let start = self.anonymous_child_starts[anonymous] as usize;
                (
                    self.anonymous_child_targets[start + position],
                    self.anonymous_child_names[start + position],
                    self.anonymous_child_flags[start + position],
                )
            });
        }
        if self.is_computed_type_row(row) {
            let computed = (row - COMPUTED_ROW_BASE) as usize;
            return (position < usize::from(self.computed_child_counts[computed])).then(|| {
                let start = self.computed_child_starts[computed] as usize;
                (
                    self.computed_child_targets[start + position],
                    self.computed_child_names[start + position],
                    self.computed_child_flags[start + position],
                )
            });
        }
        None
    }

    fn staged_type_child_count(&self, row: u32) -> Option<usize> {
        if row < ANONYMOUS_ROW_BASE {
            return self.type_child_counts.get(row as usize).copied().map(usize::from);
        }
        if self.is_anonymous_type_row(row) {
            return Some(usize::from(
                self.anonymous_child_counts[(row - ANONYMOUS_ROW_BASE) as usize],
            ));
        }
        self.is_computed_type_row(row).then(|| {
            usize::from(self.computed_child_counts[(row - COMPUTED_ROW_BASE) as usize])
        })
    }

    /// Interns one extension atom spelling, returning its provisional
    /// coordinate. Identical spellings share one atom so committed bytes
    /// stay canonical.
    pub(super) fn intern_atom(&mut self, bytes: &'source [u8]) -> Result<u32, FactFault> {
        if let Some(index) = self.extension_atoms[..self.extension_atom_len]
            .iter()
            .position(|known| *known == bytes)
        {
            return Ok(index as u32);
        }
        if self.extension_atom_len == self.plan.extension_atoms {
            return Err(FactFault::ExtensionAtomCapacity);
        }
        self.extension_atoms[self.extension_atom_len] = bytes;
        self.extension_atom_len += 1;
        Ok((self.extension_atom_len - 1) as u32)
    }

    /// Interns one pooled atom list of provisional atom coordinates.
    pub(super) fn intern_atom_list(
        &mut self,
        atoms: &[u32],
    ) -> Result<compiler_ir::AtomListId, FactFault> {
        self.intern_ref_list("atom_lists", atoms)
            .map(compiler_ir::AtomListId::new)
    }

    /// Interns one pooled type list of fact ordinals.
    pub(super) fn intern_type_list(
        &mut self,
        types: &[u32],
    ) -> Result<compiler_ir::TypeListId, FactFault> {
        self.intern_ref_list("type_lists", types)
            .map(compiler_ir::TypeListId::new)
    }

    /// Interns one pooled entity list of fact ordinals.
    pub(super) fn intern_entity_list(
        &mut self,
        entities: &[u32],
    ) -> Result<compiler_ir::EntityListId, FactFault> {
        self.intern_ref_list("entity_lists", entities)
            .map(compiler_ir::EntityListId::new)
    }

    fn intern_ref_list(&mut self, lane: &'static str, elements: &[u32]) -> Result<u32, FactFault> {
        if elements.len() > MAX_REF_LIST_ELEMENTS {
            return Err(FactFault::RefListElements);
        }
        for raw in elements {
            let limit = match lane {
                "atom_lists" => self.extension_atom_len,
                _ => self.len,
            };
            if *raw >= limit as u32 {
                return Err(FactFault::RefTarget {
                    lane,
                    raw: *raw,
                    fact_count: limit,
                });
            }
        }
        let (count, matches) = match lane {
            "atom_lists" => {
                let count = self.atom_list_len;
                let matches = (0..count).find(|index| {
                    usize::from(self.atom_list_lengths[*index]) == elements.len()
                        && self.atom_lists[*index][..elements.len()] == *elements
                });
                (count, matches)
            }
            "type_lists" => {
                let count = self.type_list_len;
                let matches = (0..count).find(|index| {
                    usize::from(self.type_list_lengths[*index]) == elements.len()
                        && self.type_lists[*index][..elements.len()] == *elements
                });
                (count, matches)
            }
            _ => {
                let count = self.entity_list_len;
                let matches = (0..count).find(|index| {
                    usize::from(self.entity_list_lengths[*index]) == elements.len()
                        && self.entity_lists[*index][..elements.len()] == *elements
                });
                (count, matches)
            }
        };
        if let Some(index) = matches {
            return Ok(index as u32);
        }
        if count == self.plan.ref_lists {
            return Err(FactFault::RefListCapacity);
        }
        let mut row = [0; MAX_REF_LIST_ELEMENTS];
        row[..elements.len()].copy_from_slice(elements);
        match lane {
            "atom_lists" => {
                self.atom_lists[count] = row;
                self.atom_list_lengths[count] = elements.len() as u8;
                self.atom_list_len = count + 1;
            }
            "type_lists" => {
                self.type_lists[count] = row;
                self.type_list_lengths[count] = elements.len() as u8;
                self.type_list_len = count + 1;
            }
            _ => {
                self.entity_lists[count] = row;
                self.entity_list_lengths[count] = elements.len() as u8;
                self.entity_list_len = count + 1;
            }
        }
        Ok(count as u32)
    }

    /// Appends one pooled type parameter, returning its coordinate.
    pub(super) fn push_type_parameter(
        &mut self,
        name: &'source [u8],
        constraint: Option<u32>,
        default: Option<u32>,
    ) -> Result<u32, FactFault> {
        let bound = constraint.map(compiler_ir::ExtensionTypeParameterBound::Type);
        self.push_type_parameter_with_bounds(
            name,
            bound.as_slice(),
            default,
            compiler_ir::ExtensionTypeParameterKind::Type {
                inference: compiler_ir::TypeParameterInference::Ordinary,
            },
            compiler_ir::Variance::Invariant,
            compiler_ir::TypeParameterRequirements::none(),
        )
    }

    /// Appends one generic parameter together with its exact written bound
    /// sequence. The copied prefix is owned by this admission transaction;
    /// no producer can retain a temporary vector or infer an end later.
    pub(super) fn push_type_parameter_with_bounds(
        &mut self,
        name: &'source [u8],
        bounds: &[compiler_ir::ExtensionTypeParameterBound<'source>],
        default: Option<u32>,
        kind: compiler_ir::ExtensionTypeParameterKind,
        variance: compiler_ir::Variance,
        requirements: compiler_ir::TypeParameterRequirements,
    ) -> Result<u32, FactFault> {
        for raw in bounds.iter().filter_map(|bound| match bound {
            compiler_ir::ExtensionTypeParameterBound::Type(raw) => Some(*raw),
            compiler_ir::ExtensionTypeParameterBound::Lifetime(_) => None,
        }).chain(default).chain(match kind {
            compiler_ir::ExtensionTypeParameterKind::Type { .. }
            | compiler_ir::ExtensionTypeParameterKind::Lifetime => None,
            compiler_ir::ExtensionTypeParameterKind::ConstValue { value_type } => Some(value_type),
        }) {
            if raw >= self.len as u32
                && !self.is_anonymous_type_row(raw)
                && !self.is_computed_type_row(raw)
            {
                return Err(FactFault::RefTarget {
                    lane: "type_parameters",
                    raw,
                    fact_count: self.len,
                });
            }
        }
        if self.type_parameter_len == self.plan.type_parameters {
            return Err(FactFault::TypeParameterCapacity);
        }
        let start = self.type_parameter_bound_len;
        let end = start
            .checked_add(bounds.len())
            .ok_or(FactFault::TypeParameterBoundCapacity {
                requested: usize::MAX,
                available: self.plan.type_parameter_bounds,
            })?;
        if end > self.plan.type_parameter_bounds {
            return Err(FactFault::TypeParameterBoundCapacity {
                requested: end,
                available: self.plan.type_parameter_bounds,
            });
        }
        self.type_parameter_bounds[start..end].copy_from_slice(bounds);
        self.type_parameters[self.type_parameter_len] = ExtensionTypeParameter {
            name,
            bounds: compiler_ir::ExtensionTypeParameterBoundRange {
                start: u32::try_from(start).map_err(|_| FactFault::TypeParameterCapacity)?,
                length: u32::try_from(bounds.len()).map_err(|_| FactFault::TypeParameterCapacity)?,
            },
            default,
            variance,
            kind,
            requirements,
        };
        self.type_parameter_len += 1;
        self.type_parameter_bound_len = end;
        Ok((self.type_parameter_len - 1) as u32)
    }

    /// Appends one occurrence fact owned by an already-pushed fact ordinal.
    pub(super) fn push_occurrence(
        &mut self,
        owner: u32,
        occurrence: Occurrence<'source>,
    ) -> Result<(), FactFault> {
        if owner >= self.len as u32 {
            return Err(FactFault::OccurrenceOwner {
                owner,
                fact_count: self.len,
            });
        }
        if self.occurrence_len == self.plan.occurrences {
            return Err(FactFault::OccurrenceCapacity);
        }
        self.occurrence_owners[self.occurrence_len] = owner;
        self.occurrences[self.occurrence_len] = occurrence;
        self.occurrence_len += 1;
        Ok(())
    }

    /// Appends one documentation fragment owned by an already-pushed fact
    /// ordinal.
    pub(super) fn push_doc(
        &mut self,
        owner: u32,
        fragment: DocFragmentInput<'source>,
    ) -> Result<(), FactFault> {
        if owner >= self.len as u32 {
            return Err(FactFault::DocOwner {
                owner,
                fact_count: self.len,
            });
        }
        if self.doc_len == self.plan.docs {
            return Err(FactFault::DocCapacity);
        }
        self.documentation_captured[owner as usize] = true;
        self.doc_facts[self.doc_len] = DocFactInput {
            owner: compiler_ir::EntityId::new(owner),
            fragment,
        };
        self.doc_len += 1;
        Ok(())
    }

    /// Marks an authority-owned documentation plane for one emitted source
    /// declaration, including the semantically real empty-documentation
    /// case. Synthetic carrier rows must never call this marker.
    pub(super) fn mark_documentation_captured(&mut self, owner: u32) -> Result<(), FactFault> {
        if owner >= self.len as u32 {
            return Err(FactFault::DocOwner {
                owner,
                fact_count: self.len,
            });
        }
        self.documentation_captured[owner as usize] = true;
        Ok(())
    }

    /// Returns one authority-admitted declaration kind by its validated lane ordinal.
    #[cfg(test)]
    pub(super) fn kind_at(&self, ordinal: usize) -> Option<EntityKind> {
        if ordinal >= self.len {
            None
        } else {
            self.kinds.get(ordinal).copied()
        }
    }

    /// Materializes the rich compatibility view directly from this exact
    /// admitted lane. The compact fragment and this view therefore cannot
    /// diverge on declaration names, kinds, or primitive facts.
    ///
    /// Facts not supplied by this lane remain explicit `Unknown` visibility
    /// or absent fields; this view never manufactures visibility, members,
    /// documentation, source spans, or language extensions.
    pub(super) fn build_ir(
        &self,
        profile: compiler_vocabulary::LanguageProfile,
        source: compiler_ir::SourceIdentity,
        declaration_scope: crate::types::DeclarationScope<'source>,
    ) -> Result<Ir, compiler_ir::BuildError> {
        let fact_count = self.len;
        let mut builder = IrBuilder::new();
        builder.set_language_profile(profile)?;

        let empty_version = EntityVersion {
            stable: StableEntityId::from_raw([0; 16]),
            payload: PayloadHash::from_raw([0; 16]),
        };
        // Declaration identity includes its stable ownership path, not a
        // transient parent ordinal.  Documentation, spans, visibility, and
        // extension payloads deliberately remain version payload: editing
        // them changes the entity's contents, never its logical identity.
        let mut scoped_keys = vec![None; fact_count].into_boxed_slice();
        let mut visiting = vec![false; fact_count].into_boxed_slice();
        let mut versions = vec![empty_version; fact_count].into_boxed_slice();
        for (ordinal, version) in versions.iter_mut().take(fact_count).enumerate() {
            let key = scoped_fact_key(self, ordinal, &mut scoped_keys, &mut visiting)?;
            *version = fact_version(
                declaration_scope,
                profile,
                key,
                self.kinds[ordinal],
                self.names[ordinal],
                self.key_digests[ordinal],
            )?;
        }
        let mut tree = builder.reserve_tree(&versions[..fact_count])?;
        // Staging coordinates are segmented tags, not physical offsets.  The
        // projection stores exactly the rows admitted by this transaction.
        let staged_type_end = fact_count + self.anonymous_rows + self.computed_rows;
        let mut type_ids = vec![None; staged_type_end].into_boxed_slice();
        // 0 = unseen, 1 = recursively visiting, 2 = fully interned.  A
        // boolean can only distinguish cache hit from miss and recurses
        // forever on hostile compound type cycles.
        let mut type_seen = vec![0_u8; staged_type_end].into_boxed_slice();
        // Compound projections borrow this one measured transaction buffer;
        // they never allocate temporary vectors or invent placeholder IDs.
        let mut projection_scratch = ProjectionScratch::new(self.projected_type_demand());
        let mut semantic_types = vec![None; fact_count].into_boxed_slice();
        for (ordinal, semantic_type) in semantic_types.iter_mut().take(fact_count).enumerate() {
            *semantic_type = Some(live_type(
                &mut tree,
                self,
                ordinal as u32,
                &mut type_ids,
                &mut type_seen,
                &mut projection_scratch,
            )?);
        }
        // Documentation facts are allowed to arrive interleaved by owner.
        // Count/prefix/scatter once so each tree item borrows one contiguous
        // range without the former O(facts × docs) rescan or an accidental
        // assumption that documentation was grouped at collection time.
        let mut doc_counts = vec![0_usize; fact_count];
        for fact in self.doc_facts[..self.doc_len].iter() {
            let owner = fact.owner.raw as usize;
            let Some(count) = doc_counts.get_mut(owner) else {
                return Err(compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Entity,
                    raw: fact.owner.raw,
                });
            };
            *count += 1;
        }
        let mut doc_ranges = vec![(0usize, 0usize); fact_count].into_boxed_slice();
        let mut cursor = 0_usize;
        for (ordinal, count) in doc_counts.iter().copied().enumerate() {
            doc_ranges[ordinal] = (cursor, count);
            cursor += count;
        }
        let mut doc_cursors = doc_ranges.iter().map(|range| range.0).collect::<Vec<_>>();
        let mut docs = vec![DocInput::SoftBreak; self.doc_len];
        for fact in self.doc_facts[..self.doc_len].iter() {
            let owner = fact.owner.raw as usize;
            let slot = doc_cursors[owner];
            docs[slot] = doc_input(&mut tree, fact.fragment)?;
            doc_cursors[owner] += 1;
        }
        // Every sparse language plane is materialized from the same staged
        // rows as the durable fragment.  There is intentionally no
        // TypeScript-only rewrite path: a compact fact can never disappear
        // merely because a caller asks for the owned IR view.
        let mut rewritten_typescript = vec![empty_typescript_facts(); fact_count];
        let mut rewritten_csharp = vec![empty_csharp_facts(); fact_count];
        let mut rewritten_go = vec![empty_go_facts(); fact_count];
        let mut rewritten_rust = vec![empty_rust_facts(); fact_count];
        let mut rewritten_python = vec![empty_python_facts(); fact_count];
        let mut rewritten_java = vec![empty_java_facts(); fact_count];
        let mut rewritten_clang = vec![empty_clang_facts(); fact_count];
        for (ordinal, extension) in self.extensions[..fact_count].iter().enumerate() {
            match extension {
                Some(EmissionExtension::TypeScript(value)) => {
                    rewritten_typescript[ordinal] = compiler_ir::TypeScriptFacts {
                        type_parameters: live_type_parameters(
                            &mut tree,
                            self,
                            value.type_parameters,
                            self.type_parameter_ranges[ordinal].ok_or(
                                compiler_ir::BuildError::Dangling {
                                    space: compiler_ir::SemanticSpace::TypeParameters,
                                    raw: value.type_parameters.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        declared: value
                            .declared
                            .map(|id| {
                                live_type(
                                    &mut tree,
                                    self,
                                    id.raw,
                                    &mut type_ids,
                                    &mut type_seen,
                                    &mut projection_scratch,
                                )
                            })
                            .transpose()?,
                        observed: value
                            .observed
                            .map(|id| {
                                live_type(
                                    &mut tree,
                                    self,
                                    id.raw,
                                    &mut type_ids,
                                    &mut type_seen,
                                    &mut projection_scratch,
                                )
                            })
                            .transpose()?,
                    };
                }
                Some(EmissionExtension::CSharp(value)) => {
                    rewritten_csharp[ordinal] = compiler_ir::CSharpFacts {
                        constraints: live_type_parameters(
                            &mut tree,
                            self,
                            value.constraints,
                            self.type_parameter_ranges[ordinal].ok_or(
                                compiler_ir::BuildError::Dangling {
                                    space: compiler_ir::SemanticSpace::TypeParameters,
                                    raw: value.constraints.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        attributes: live_atom_list(&mut tree, self, value.attributes)?,
                        xml_provenance: value
                            .xml_provenance
                            .map(|span| live_extension_span(&mut tree, self, span))
                            .transpose()?,
                        ..*value
                    };
                }
                Some(EmissionExtension::Go(value)) => {
                    rewritten_go[ordinal] = compiler_ir::GoFacts {
                        signature: compiler_ir::GoSignature {
                            parameters: live_type_list(
                                &mut tree,
                                self,
                                value.signature.parameters,
                                &mut type_ids,
                                &mut type_seen,
                                &mut projection_scratch,
                            )?,
                            results: live_type_list(
                                &mut tree,
                                self,
                                value.signature.results,
                                &mut type_ids,
                                &mut type_seen,
                                &mut projection_scratch,
                            )?,
                            ..value.signature
                        },
                        type_parameters: live_type_parameters(
                            &mut tree,
                            self,
                            value.type_parameters,
                            self.type_parameter_ranges[ordinal].ok_or(
                                compiler_ir::BuildError::Dangling {
                                    space: compiler_ir::SemanticSpace::TypeParameters,
                                    raw: value.type_parameters.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        fields: live_entity_list(&mut tree, self, value.fields)?,
                        method_set: live_entity_list(&mut tree, self, value.method_set)?,
                        build_constraints: live_atom_list(
                            &mut tree,
                            self,
                            value.build_constraints,
                        )?,
                        constant_value: live_atom_list(
                            &mut tree,
                            self,
                            value.constant_value,
                        )?,
                        ..*value
                    };
                }
                Some(EmissionExtension::Rust(value)) => {
                    rewritten_rust[ordinal] = compiler_ir::RustFacts {
                        lifetimes: live_atom_list(&mut tree, self, value.lifetimes)?,
                        where_clauses: live_type_parameters(
                            &mut tree,
                            self,
                            value.where_clauses,
                            self.type_parameter_ranges[ordinal].ok_or(
                                compiler_ir::BuildError::Dangling {
                                    space: compiler_ir::SemanticSpace::TypeParameters,
                                    raw: value.where_clauses.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        macros: live_atom_list(&mut tree, self, value.macros)?,
                        ..*value
                    };
                }
                Some(EmissionExtension::Python(value)) => {
                    rewritten_python[ordinal] = compiler_ir::PythonFacts {
                        decorators: live_atom_list(&mut tree, self, value.decorators)?,
                        ..*value
                    };
                }
                Some(EmissionExtension::Java(value)) => {
                    rewritten_java[ordinal] = compiler_ir::JavaFacts {
                        throws: live_type_list(
                            &mut tree,
                            self,
                            value.throws,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        annotations: live_atom_list(&mut tree, self, value.annotations)?,
                        overloads: live_entity_list(&mut tree, self, value.overloads)?,
                        record_components: live_entity_list(
                            &mut tree,
                            self,
                            value.record_components,
                        )?,
                    };
                }
                Some(EmissionExtension::Clang(value)) => {
                    rewritten_clang[ordinal] = compiler_ir::ClangFacts {
                        templates: live_type_parameters(
                            &mut tree,
                            self,
                            value.templates,
                            self.type_parameter_ranges[ordinal].ok_or(
                                compiler_ir::BuildError::Dangling {
                                    space: compiler_ir::SemanticSpace::TypeParameters,
                                    raw: value.templates.raw,
                                },
                            )?,
                            &mut type_ids,
                            &mut type_seen,
                            &mut projection_scratch,
                        )?,
                        includes: live_atom_list(&mut tree, self, value.includes)?,
                        ..*value
                    };
                }
                None => {}
            }
        }
        // Parent edges arrive as authority facts.  Prefix/scatter them once
        // into a compact child pool, preserving declaration order and making
        // the `parent` and `members` projections mutual inverses.
        let mut member_counts = vec![0_usize; fact_count];
        for parent in self.parentage[..fact_count]
            .iter()
            .copied()
            .filter_map(local_parent)
        {
            let Some(count) = member_counts.get_mut(parent.raw as usize) else {
                return Err(compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Entity,
                    raw: parent.raw,
                });
            };
            *count += 1;
        }
        let mut member_ranges = vec![(0_usize, 0_usize); fact_count];
        let mut member_total = 0_usize;
        for (ordinal, count) in member_counts.iter().copied().enumerate() {
            member_ranges[ordinal] = (member_total, count);
            member_total += count;
        }
        let mut member_cursors = member_ranges.iter().map(|range| range.0).collect::<Vec<_>>();
        let mut members = vec![compiler_ir::TreeEntityId::new(0); member_total];
        for (child, parentage) in self.parentage[..fact_count].iter().copied().enumerate() {
            let Some(parent) = local_parent(parentage) else {
                continue;
            };
            let slot = member_cursors[parent.raw as usize];
            members[slot] = compiler_ir::TreeEntityId::new(child as u32);
            member_cursors[parent.raw as usize] += 1;
        }
        let source_file = self.source_spans[..fact_count]
            .iter()
            .any(Option::is_some)
            // This atom is the entered source's typed content authority, not
            // a made-up filename.  Adapters with a distinct file authority
            // must stage it explicitly rather than relabel it as primary.
            .then(|| tree.intern_atom(source.identity.as_ref()))
            .transpose()?;
        let mut item_attributes = vec![Vec::<&'source [u8]>::new(); fact_count];
        for (ordinal, extension) in self.extensions[..fact_count].iter().enumerate() {
            let Some(list) = extension.as_ref().and_then(extension_item_attributes) else {
                continue;
            };
            let list = list.raw as usize;
            if self.atom_list_len == 0 && list == 0 {
                continue;
            }
            let length = self.atom_list_lengths.get(list).copied().ok_or(
                compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::AtomList,
                    raw: list as u32,
                },
            )?;
            if list >= self.atom_list_len {
                return Err(compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::AtomList,
                    raw: list as u32,
                });
            }
            let target = &mut item_attributes[ordinal];
            target.reserve(usize::from(length));
            for provisional in &self.atom_lists[list][..usize::from(length)] {
                target.push(*self.extension_atoms.get(*provisional as usize).ok_or(
                    compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Atom,
                        raw: *provisional,
                    },
                )?);
            }
        }
        let empty_item = TreeItemInput {
            name: b"",
            kind: ItemKind::Function,
            visibility: Visibility::Unknown,
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        };
        let mut items = vec![empty_item; fact_count].into_boxed_slice();
        for (ordinal, item) in items.iter_mut().take(fact_count).enumerate() {
            let extension = match self.extensions[ordinal].as_ref() {
                Some(EmissionExtension::TypeScript(_)) => Some(LanguageExtensionInput::TypeScript(
                    &rewritten_typescript[ordinal],
                )),
                Some(EmissionExtension::CSharp(_)) => {
                    Some(LanguageExtensionInput::CSharp(&rewritten_csharp[ordinal]))
                }
                Some(EmissionExtension::Go(_)) => {
                    Some(LanguageExtensionInput::Go(&rewritten_go[ordinal]))
                }
                Some(EmissionExtension::Rust(_)) => {
                    Some(LanguageExtensionInput::Rust(&rewritten_rust[ordinal]))
                }
                Some(EmissionExtension::Python(_)) => {
                    Some(LanguageExtensionInput::Python(&rewritten_python[ordinal]))
                }
                Some(EmissionExtension::Java(_)) => {
                    Some(LanguageExtensionInput::Java(&rewritten_java[ordinal]))
                }
                Some(EmissionExtension::Clang(_)) => {
                    Some(LanguageExtensionInput::Clang(&rewritten_clang[ordinal]))
                }
                None => None,
            };
            *item = TreeItemInput {
                name: self.names[ordinal],
                kind: item_kind(self.kinds[ordinal]),
                visibility: self.visibility[ordinal],
                parent: local_parent(self.parentage[ordinal])
                    .map(|parent| compiler_ir::TreeEntityId::new(parent.raw)),
                semantic_type: semantic_types[ordinal],
                members: &members[member_ranges[ordinal].0
                    ..member_ranges[ordinal].0 + member_ranges[ordinal].1],
                docs: &docs[doc_ranges[ordinal].0..doc_ranges[ordinal].0 + doc_ranges[ordinal].1],
                attributes: &item_attributes[ordinal],
                source: self.source_spans[ordinal].and_then(|span| {
                    source_file.and_then(|file| compiler_ir::SourceSpan::new(file, span.start, span.end))
                }),
                extension,
            };
        }
        let mut links = Vec::with_capacity(self.occurrence_len);
        for index in 0..self.occurrence_len {
            let owner = self.occurrence_owners[index];
            let occurrence = self.occurrences[index];
            links.push(TreeLinkInput {
                from: compiler_ir::TreeEntityId::new(owner),
                target: external_from_occurrence(&mut tree, occurrence.target)?,
                kind: occurrence_link_kind(occurrence.kind),
                confidence: occurrence_link_confidence(occurrence.confidence),
                source: occurrence_source_span(self, source_file, owner, occurrence.span)?,
            });
        }
        tree.commit(&items[..fact_count], &links)?;
        builder.finish()
    }

    /// Admits one fact after proving its name, its constructor payload against
    /// its child count, every child role against the constructor's closed role
    /// lane, and every child target against the already-pushed prefix.
    ///
    /// A rejected fact leaves every lane byte-for-byte unchanged and retains
    /// the offending ordinal, exact name bytes, and typed cause.
    #[expect(
        clippy::indexing_slicing,
        reason = "the fact ordinal is admitted below MAX_EMISSION_FACTS and the pooled child range is admitted to the fixed lane length before any lane write"
    )]
    pub(super) fn push(
        &mut self,
        fact: SemanticFact<'source>,
    ) -> Result<usize, RejectedFact<'source>> {
        let fact_ordinal = self.len;
        let rejected = |cause| RejectedFact {
            fact: fact_ordinal,
            name: fact.name,
            cause,
        };
        if fact.name.is_empty() {
            return Err(rejected(FactFault::EmptyName));
        }
        if fact_ordinal == self.plan.facts {
            return Err(rejected(FactFault::Capacity));
        }
        if fact.truncated {
            return Err(rejected(FactFault::ChildCapacity));
        }
        if fact.type_truncated {
            return Err(rejected(FactFault::TypeChildCapacity));
        }
        let type_child_count = u32::from(fact.type_child_count);
        fact.type_record
            .validate(type_child_count)
            .map_err(|fault| rejected(FactFault::TypeRecord(fault)))?;
        for (position, child) in fact
            .type_children
            .iter()
            .take(usize::from(fact.type_child_count))
            .enumerate()
        {
            let target = if child.target == u32::MAX {
                TypeChildTarget::Text
            } else {
                TypeChildTarget::Type(compiler_ir::TypeRef::Local(compiler_ir::TypeId::new(
                    child.target,
                )))
            };
            let wire_child = SemanticTypeChild {
                target,
                name: child.name,
                flags: child.flags,
            };
            fact.type_record
                .validate_child_in_row(position as u32, type_child_count, &wire_child)
                .map_err(|fault| rejected(FactFault::TypeChild { position, fault }))?;
            if child.target != u32::MAX {
                let valid = self.is_anonymous_type_row(child.target)
                    || child.target < fact_ordinal as u32;
                if !valid {
                    return Err(rejected(FactFault::TypeChildTarget {
                        position,
                        target: child.target,
                        fact_count: fact_ordinal,
                    }));
                }
            }
        }
        if let Some(NominalRef::Local(target)) = fact.type_record.nominal {
            if target.raw > fact_ordinal as u32 {
                return Err(rejected(FactFault::TypeRecord(
                    SemanticTypeFault::ReservedCell {
                        tag: fact.type_record.tag,
                        cell: compiler_ir::TypeCell::Nominal,
                        actual: target.raw,
                    },
                )));
            }
        }
        let child_count = u32::from(fact.child_count);
        fact.constructor
            .validate(child_count)
            .map_err(|fault| rejected(FactFault::Constructor(fault)))?;
        for (position, child) in fact.children.iter().enumerate().take(child_count as usize) {
            #[expect(
                clippy::as_conversions,
                reason = "positions are bounded by MAX_FACT_CHILDREN and always fit the u32 role coordinate"
            )]
            let expected = fact.constructor.expected_role(position as u32);
            if child.role != expected {
                return Err(rejected(FactFault::ChildRole {
                    position,
                    expected,
                    actual: child.role,
                }));
            }
            #[expect(
                clippy::as_conversions,
                reason = "u32 child targets widen totally to the native fact-count width on every supported target"
            )]
            let target = child.target as usize;
            if target >= fact_ordinal {
                return Err(rejected(FactFault::ChildTarget {
                    position,
                    target: child.target,
                    fact_count: fact_ordinal,
                }));
            }
        }

        // Every fallible admission rule must close before the first staging
        // write. In particular, a generic extension's pool start is a
        // producer-provided coordinate, not a best-effort attachment after
        // kind/type rows and pooled cursors have moved.
        let type_parameter_range = match fact.extension.as_ref() {
            Some(extension) => self
                .capture_type_parameter_range(extension)
                .map_err(rejected)?,
            None => None,
        };

        self.kinds[fact_ordinal] = fact.kind;
        self.names[fact_ordinal] = fact.name;
        self.type_records[fact_ordinal] = fact.type_record;
        let type_pooled_start = self.total_type_children;
        for (offset, child) in fact
            .type_children
            .iter()
            .copied()
            .take(usize::from(fact.type_child_count))
            .enumerate()
        {
            let pooled = type_pooled_start + offset;
            self.type_child_targets[pooled] = child.target;
            self.type_child_names[pooled] = child.name;
            self.type_child_flags[pooled] = child.flags;
        }
        self.total_type_children = type_pooled_start + usize::from(fact.type_child_count);
        self.type_child_starts[fact_ordinal] = type_pooled_start as u32;
        self.type_child_counts[fact_ordinal] = fact.type_child_count;
        self.constructors[fact_ordinal] = fact.constructor;
        self.child_counts[fact_ordinal] = fact.child_count;
        self.type_parameter_ranges[fact_ordinal] = type_parameter_range;
        self.extensions[fact_ordinal] = fact.extension;
        self.visibility[fact_ordinal] = fact.visibility;
        self.visibility_captured[fact_ordinal] = fact.visibility_captured;
        self.key_digests[fact_ordinal] = fact_key_digest(&fact);
        let pooled_start = self.total_children;
        for (offset, child) in fact
            .children
            .iter()
            .copied()
            .take(child_count as usize)
            .enumerate()
        {
            self.child_roles[pooled_start + offset] = child.role;
            self.child_targets[pooled_start + offset] = child.target;
        }
        self.total_children = pooled_start + child_count as usize;
        self.child_starts[fact_ordinal] = pooled_start as u32;
        self.len = fact_ordinal + 1;
        Ok(fact_ordinal)
    }
}

/// Maps one closed primitive record onto the compact builtin lattice; every
/// other record honestly stays off the compact node lane.
fn builtin_type(record: SemanticTypeRecord<'_>) -> Option<BuiltinType> {
    if record.tag != SemanticTypeTag::Primitive {
        return None;
    }
    match PrimitiveShape::try_from(record.payload0) {
        Ok(PrimitiveShape::Bool) if record.payload1 == 0 => Some(BuiltinType::Bool),
        Ok(PrimitiveShape::LegacyChar) if record.payload1 == 0 => Some(BuiltinType::LegacyChar),
        Ok(PrimitiveShape::Integer) => match (record.payload1 >> 1, record.payload1 & 1) {
            (8, 0) => Some(BuiltinType::U8),
            (8, 1) => Some(BuiltinType::I8),
            (16, 0) => Some(BuiltinType::U16),
            (16, 1) => Some(BuiltinType::I16),
            (32, 0) => Some(BuiltinType::U32),
            (32, 1) => Some(BuiltinType::I32),
            (64, 0) => Some(BuiltinType::U64),
            (64, 1) => Some(BuiltinType::I64),
            (128, 0) => Some(BuiltinType::U128),
            (128, 1) => Some(BuiltinType::I128),
            _ => None,
        },
        Ok(PrimitiveShape::Float) => match record.payload1 {
            16 => Some(BuiltinType::F16),
            32 => Some(BuiltinType::F32),
            64 => Some(BuiltinType::F64),
            _ => None,
        },
        Ok(PrimitiveShape::Str) if record.payload1 == 0 => Some(BuiltinType::String),
        _ => None,
    }
}

/// Stable lattice code for the one unknown reason rendered as an external.
fn doc_input<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    fragment: DocFragmentInput<'source>,
) -> Result<DocInput<'source>, compiler_ir::BuildError> {
    let text = |bytes: &'source [u8]| {
        core::str::from_utf8(bytes).map_err(|_| compiler_ir::BuildError::InvalidDocumentationUtf8 {
            bytes: bytes.len(),
        })
    };
    match fragment {
        DocFragmentInput::Text(bytes) => Ok(DocInput::Text(text(bytes)?)),
        DocFragmentInput::Code(bytes) => Ok(DocInput::Code(text(bytes)?)),
        DocFragmentInput::Link { label, target } => Ok(DocInput::Link {
            label: text(label)?,
            target: match target {
                DocLinkTarget::Local(id) => {
                    TreeLinkTarget::Local(compiler_ir::TreeEntityId::new(id.raw))
                }
                DocLinkTarget::Foreign { ecosystem, path } => {
                    let mut identity = Vec::with_capacity(
                        b"compiler.ir.documentation.foreign.v1\0".len()
                            + ecosystem.len()
                            + path.len()
                            + 1,
                    );
                    identity.extend_from_slice(b"compiler.ir.documentation.foreign.v1\0");
                    identity.extend_from_slice(ecosystem);
                    identity.push(0);
                    identity.extend_from_slice(path);
                    let package = tree.intern_atom(ecosystem)?;
                    let path_id = tree.intern_atom(path)?;
                    let display_id = tree.intern_atom(path)?;
                    TreeLinkTarget::External(tree.intern_external(ExternalTarget {
                        stable: StableEntityId::from_canonical_bytes(&identity),
                        package: Some(package),
                        path: path_id,
                        display: display_id,
                        kind: None,
                    })?)
                }
            },
        }),
        DocFragmentInput::SoftBreak => Ok(DocInput::SoftBreak),
        DocFragmentInput::HardBreak => Ok(DocInput::HardBreak),
    }
}

fn extension_type_parameter_start(
    extension: &EmissionExtension,
) -> Option<compiler_ir::TypeParameterListId> {
    match extension {
        EmissionExtension::TypeScript(facts) => Some(facts.type_parameters),
        EmissionExtension::CSharp(facts) => Some(facts.constraints),
        EmissionExtension::Go(facts) => Some(facts.type_parameters),
        EmissionExtension::Rust(facts) => Some(facts.where_clauses),
        EmissionExtension::Clang(facts) => Some(facts.templates),
        EmissionExtension::Python(_) | EmissionExtension::Java(_) => None,
    }
}

/// Attributes that belong to the generic tree item as well as their language
/// extension row.  The bytes are staged once and borrowed into both views;
/// this is not a renderer-side reconstruction.
fn extension_item_attributes(extension: &EmissionExtension) -> Option<compiler_ir::AtomListId> {
    match extension {
        EmissionExtension::CSharp(facts) => Some(facts.attributes),
        EmissionExtension::Python(facts) => Some(facts.decorators),
        EmissionExtension::Java(facts) => Some(facts.annotations),
        EmissionExtension::TypeScript(_)
        | EmissionExtension::Go(_)
        | EmissionExtension::Rust(_)
        | EmissionExtension::Clang(_) => None,
    }
}

fn live_type_parameters<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    start: compiler_ir::TypeParameterListId,
    range: StagedTypeParameterRange,
    ids: &mut [Option<TypeId>],
    seen: &mut [u8],
    scratch: &mut ProjectionScratch,
) -> Result<compiler_ir::TypeParameterListId, compiler_ir::BuildError> {
    let start = start.raw as usize;
    if range.start != start as u32 {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::TypeParameters,
            raw: start as u32,
        });
    }
    if range.length == 0 {
        return tree.intern_type_parameters(&[]);
    }
    let end = start.checked_add(range.length as usize).ok_or(compiler_ir::BuildError::Dangling {
        space: compiler_ir::SemanticSpace::TypeParameters,
        raw: start as u32,
    })?;
    let parameters = facts.type_parameters.get(start..end).ok_or(
        compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::TypeParameters,
            raw: start as u32,
        },
    )?;
    let mut materialized = Vec::with_capacity(parameters.len());
    for parameter in parameters {
        let bound_end = parameter
            .bounds
            .start
            .checked_add(parameter.bounds.length)
            .ok_or(compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::TypeParameterBounds,
                raw: parameter.bounds.start,
            })? as usize;
        let bounds = facts
            .type_parameter_bounds
            .get(parameter.bounds.start as usize..bound_end)
            .ok_or(compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::TypeParameterBounds,
                raw: parameter.bounds.start,
            })?;
        let mut live_bounds = Vec::with_capacity(bounds.len());
        for bound in bounds {
            live_bounds.push(match bound {
                compiler_ir::ExtensionTypeParameterBound::Type(row) => {
                    compiler_ir::TypeParameterBound::Type(live_type(
                        tree, facts, *row, ids, seen, scratch,
                    )?)
                }
                compiler_ir::ExtensionTypeParameterBound::Lifetime(name) => {
                    compiler_ir::TypeParameterBound::Lifetime(tree.intern_atom(name)?)
                }
            });
        }
        let kind = match parameter.kind {
            compiler_ir::ExtensionTypeParameterKind::Type { inference } => {
                compiler_ir::TypeParameterKind::Type { inference }
            }
            compiler_ir::ExtensionTypeParameterKind::ConstValue { value_type } => {
                compiler_ir::TypeParameterKind::ConstValue {
                    value_type: live_type(tree, facts, value_type, ids, seen, scratch)?,
                }
            }
            compiler_ir::ExtensionTypeParameterKind::Lifetime => {
                compiler_ir::TypeParameterKind::Lifetime
            }
        };
        materialized.push(compiler_ir::TypeParameter {
            name: tree.intern_atom(parameter.name)?,
            bounds: tree.intern_type_parameter_bounds(&live_bounds)?,
            default: parameter
                .default
                .map(|row| live_type(tree, facts, row, ids, seen, scratch))
                .transpose()?,
            variance: parameter.variance,
            kind,
            requirements: parameter.requirements,
        });
    }
    tree.intern_type_parameters(&materialized)
}

fn live_atom_list<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    id: compiler_ir::AtomListId,
) -> Result<compiler_ir::AtomListId, compiler_ir::BuildError> {
    let index = id.raw as usize;
    if facts.atom_list_len == 0 && index == 0 {
        return tree.intern_attributes(&[]);
    }
    let length = facts.atom_list_lengths.get(index).copied().ok_or(
        compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::AtomList,
            raw: id.raw,
        },
    )?;
    if index >= facts.atom_list_len {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::AtomList,
            raw: id.raw,
        });
    }
    let mut atoms = Vec::with_capacity(usize::from(length));
    for provisional in &facts.atom_lists[index][..usize::from(length)] {
        let bytes = facts
            .extension_atoms
            .get(*provisional as usize)
            .copied()
            .ok_or(compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Atom,
                raw: *provisional,
            })?;
        atoms.push(tree.intern_atom(bytes)?);
    }
    tree.intern_attributes(&atoms)
}

fn live_type_list<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    id: compiler_ir::TypeListId,
    ids: &mut [Option<TypeId>],
    seen: &mut [u8],
    scratch: &mut ProjectionScratch,
) -> Result<compiler_ir::TypeListId, compiler_ir::BuildError> {
    let index = id.raw as usize;
    if facts.type_list_len == 0 && index == 0 {
        return tree.intern_types(&[]);
    }
    if index >= facts.type_list_len {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::TypeList,
            raw: id.raw,
        });
    }
    let length = usize::from(facts.type_list_lengths[index]);
    let mut types = Vec::with_capacity(length);
    for row in &facts.type_lists[index][..length] {
        types.push(
            live_type(tree, facts, *row, ids, seen, scratch)?,
        );
    }
    tree.intern_types(&types)
}

fn live_entity_list(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'_>,
    id: compiler_ir::EntityListId,
) -> Result<compiler_ir::EntityListId, compiler_ir::BuildError> {
    let index = id.raw as usize;
    if facts.entity_list_len == 0 && index == 0 {
        return tree.intern_members(&[]);
    }
    if index >= facts.entity_list_len {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::EntityList,
            raw: id.raw,
        });
    }
    let length = usize::from(facts.entity_list_lengths[index]);
    let mut entities = Vec::with_capacity(length);
    for raw in &facts.entity_lists[index][..length] {
        let local = compiler_ir::TreeEntityId::new(*raw);
        entities.push(tree.entities().get(local).ok_or(
            compiler_ir::BuildError::InvalidTreeEntity {
                raw: *raw,
                count: facts.len as u32,
            },
        )?);
    }
    tree.intern_members(&entities)
}

fn live_extension_span<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    span: compiler_ir::SourceSpan,
) -> Result<compiler_ir::SourceSpan, compiler_ir::BuildError> {
    let file = facts
        .extension_atoms
        .get(span.file().raw as usize)
        .copied()
        .ok_or(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Atom,
            raw: span.file().raw,
        })?;
    compiler_ir::SourceSpan::new(tree.intern_atom(file)?, span.start(), span.end()).ok_or(
        compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Atom,
            raw: span.file().raw,
        },
    )
}

const fn occurrence_link_kind(kind: compiler_ir::ReferenceKind) -> compiler_ir::LinkKind {
    match kind {
        compiler_ir::ReferenceKind::FunctionCall => compiler_ir::LinkKind::Calls,
        compiler_ir::ReferenceKind::MethodCall => compiler_ir::LinkKind::MethodCall,
        compiler_ir::ReferenceKind::TypeReference => compiler_ir::LinkKind::TypeReference,
        compiler_ir::ReferenceKind::VariableUse => compiler_ir::LinkKind::Reads,
        compiler_ir::ReferenceKind::MacroInvocation => compiler_ir::LinkKind::Calls,
        compiler_ir::ReferenceKind::FieldAccess => compiler_ir::LinkKind::Reads,
        compiler_ir::ReferenceKind::Import => compiler_ir::LinkKind::Imports,
        compiler_ir::ReferenceKind::Overrides => compiler_ir::LinkKind::Overrides,
    }
}

const fn occurrence_link_confidence(
    confidence: compiler_ir::OccurrenceConfidence,
) -> compiler_ir::Confidence {
    match confidence {
        compiler_ir::OccurrenceConfidence::Syntactic => compiler_ir::Confidence::Syntactic,
        compiler_ir::OccurrenceConfidence::Suffix => compiler_ir::Confidence::Heuristic,
        compiler_ir::OccurrenceConfidence::Index => compiler_ir::Confidence::Indexed,
        compiler_ir::OccurrenceConfidence::Import => compiler_ir::Confidence::Imported,
        compiler_ir::OccurrenceConfidence::Oracle => compiler_ir::Confidence::Compiler,
    }
}

/// Lifts an owner-relative staged occurrence range into the primary entered
/// source coordinate space.  Uncaptured owners remain explicitly source-less;
/// captured owners must satisfy the exact containment law.
fn occurrence_source_span(
    facts: &FactSet<'_>,
    file: Option<AtomId>,
    owner: u32,
    relative: compiler_ir::RelSpan,
) -> Result<Option<compiler_ir::SourceSpan>, compiler_ir::BuildError> {
    let Some(owner_span) = facts.source_spans.get(owner as usize).copied().flatten() else {
        return Ok(None);
    };
    let Some(start) = owner_span.start.checked_add(relative.start) else {
        return Err(compiler_ir::BuildError::InvalidOccurrenceSpan {
            owner: compiler_ir::EntityId::new(owner),
            start: relative.start,
            end: relative.end,
        });
    };
    let Some(end) = owner_span.start.checked_add(relative.end) else {
        return Err(compiler_ir::BuildError::InvalidOccurrenceSpan {
            owner: compiler_ir::EntityId::new(owner),
            start: relative.start,
            end: relative.end,
        });
    };
    if start > end || end > owner_span.end {
        return Err(compiler_ir::BuildError::InvalidOccurrenceSpan {
            owner: compiler_ir::EntityId::new(owner),
            start,
            end,
        });
    }
    Ok(file.and_then(|file| compiler_ir::SourceSpan::new(file, start, end)))
}

fn external_from_occurrence<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    target: compiler_ir::OccurrenceTarget<'source>,
) -> Result<TreeLinkTarget, compiler_ir::BuildError> {
    match target {
        compiler_ir::OccurrenceTarget::Local(local) => {
            Ok(TreeLinkTarget::Local(compiler_ir::TreeEntityId::new(local.raw)))
        }
        compiler_ir::OccurrenceTarget::Stable(stable) => {
            let mut identity = [0_u8; 64];
            identity[..32].copy_from_slice(stable.fragment.as_ref());
            identity[32..].copy_from_slice(stable.entity.as_ref());
            let path = tree.intern_atom(b"stable")?;
            Ok(TreeLinkTarget::External(tree.intern_external(ExternalTarget {
                stable: StableEntityId::from_canonical_bytes(&identity),
                package: None,
                path,
                display: path,
                kind: None,
            })?))
        }
        compiler_ir::OccurrenceTarget::Foreign(foreign) => {
            // The vocabulary owns the domain separation, origin cells, and
            // kind discriminator.  Rebuilding a near-copy here once omitted
            // `ForeignKey::kind`, causing same-path references to collapse.
            let mut identity = vec![0_u8; foreign.key_preimage_len()];
            let identity = foreign.key_id(&mut identity).map_err(|_| {
                // The scratch is sized from `key_preimage_len`, so this is
                // unreachable unless the vocabulary itself violates its
                // stated measured-length contract.
                compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::External,
                    raw: 0,
                }
            })?;
            let package = match foreign.origin {
                compiler_ir::ForeignOrigin::Package(lineage) => {
                    Some(tree.intern_atom(lineage.name.as_bytes())?)
                }
                compiler_ir::ForeignOrigin::Namespace {
                    ecosystem,
                    namespace,
                } => {
                    Some(tree.intern_atom(namespace.as_bytes())?)
                }
                compiler_ir::ForeignOrigin::Universe { ecosystem } => {
                    Some(tree.intern_atom(ecosystem.as_bytes())?)
                }
            };
            let path = tree.intern_atom(foreign.path.as_bytes())?;
            let display = tree.intern_atom(foreign.display.as_bytes())?;
            let mut stable = [0_u8; 16];
            stable.copy_from_slice(&identity.as_ref()[..16]);
            Ok(TreeLinkTarget::External(tree.intern_external(ExternalTarget {
                stable: StableEntityId::from_raw(stable),
                package,
                path,
                display,
                kind: foreign.kind.map(item_kind),
            })?))
        }
    }
}

/// Conservative per-family compound staging bound over live depth-first
/// projection paths.
#[derive(Clone, Copy, Debug, Default)]
struct ProjectionDemand {
    type_ids: usize,
    tuple_elements: usize,
    template_parts: usize,
    object_members: usize,
}

impl ProjectionDemand {
    fn for_row(tag: SemanticTypeTag, child_count: usize) -> Self {
        let mut demand = Self::default();
        match tag {
            SemanticTypeTag::Tuple | SemanticTypeTag::FunctionPointer => {
                demand.tuple_elements = child_count;
            }
            SemanticTypeTag::TemplateLiteral => {
                demand.template_parts = child_count;
            }
            SemanticTypeTag::AnonymousRecord => {
                demand.object_members = child_count;
            }
            SemanticTypeTag::Apply => {
                demand.type_ids = child_count.saturating_sub(1);
            }
            SemanticTypeTag::Union
            | SemanticTypeTag::Intersection
            | SemanticTypeTag::ImplTrait
            | SemanticTypeTag::DynTrait => {
                demand.type_ids = child_count;
            }
            _ => {}
        }
        demand
    }

    fn maximum(self, other: Self) -> Self {
        Self {
            type_ids: self.type_ids.max(other.type_ids),
            tuple_elements: self.tuple_elements.max(other.tuple_elements),
            template_parts: self.template_parts.max(other.template_parts),
            object_members: self.object_members.max(other.object_members),
        }
    }

    fn with_child(self, child: Self) -> Self {
        Self {
            type_ids: self.type_ids.saturating_add(child.type_ids),
            tuple_elements: self.tuple_elements.saturating_add(child.tuple_elements),
            template_parts: self.template_parts.saturating_add(child.template_parts),
            object_members: self.object_members.saturating_add(child.object_members),
        }
    }
}

/// Request-owned, bounded staging for compound values projected into the
/// tree interner.
///
/// The interner copies a completed slice immediately. Reusing these typed
/// lanes avoids placeholder IDs and one heap allocation per tuple, template,
/// object, or callable while retaining the exact element types demanded by
/// each semantic constructor. Each lane has its own measured maximum, never
/// a protocol-wide maximum or a duplicate reservation for another family.
struct ProjectionScratch {
    type_capacity: usize,
    tuple_capacity: usize,
    template_capacity: usize,
    object_capacity: usize,
    type_children: Vec<TypeId>,
    tuple_elements: Vec<TupleElement>,
    template_parts: Vec<TemplatePart>,
    object_members: Vec<ObjectMember>,
}

impl ProjectionScratch {
    fn new(demand: ProjectionDemand) -> Self {
        Self {
            type_capacity: demand.type_ids,
            tuple_capacity: demand.tuple_elements,
            template_capacity: demand.template_parts,
            object_capacity: demand.object_members,
            type_children: Vec::with_capacity(demand.type_ids),
            tuple_elements: Vec::with_capacity(demand.tuple_elements),
            template_parts: Vec::with_capacity(demand.template_parts),
            object_members: Vec::with_capacity(demand.object_members),
        }
    }

    fn begin_type_children(
        &self,
        count: usize,
        row: u32,
    ) -> Result<usize, compiler_ir::BuildError> {
        self.begin(self.type_children.len(), count, self.type_capacity, row)
    }

    fn begin_tuple_elements(
        &self,
        count: usize,
        row: u32,
    ) -> Result<usize, compiler_ir::BuildError> {
        self.begin(self.tuple_elements.len(), count, self.tuple_capacity, row)
    }

    fn begin_template_parts(
        &self,
        count: usize,
        row: u32,
    ) -> Result<usize, compiler_ir::BuildError> {
        self.begin(self.template_parts.len(), count, self.template_capacity, row)
    }

    fn begin_object_members(
        &self,
        count: usize,
        row: u32,
    ) -> Result<usize, compiler_ir::BuildError> {
        self.begin(self.object_members.len(), count, self.object_capacity, row)
    }

    fn begin(
        &self,
        used: usize,
        additional: usize,
        capacity: usize,
        row: u32,
    ) -> Result<usize, compiler_ir::BuildError> {
        let required = used.checked_add(additional).ok_or(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Type,
            raw: row,
        })?;
        if required > capacity {
            return Err(compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Type,
                raw: row,
            });
        }
        Ok(used)
    }
}

/// Resolves one already-projected structural child without ever constructing
/// a placeholder `TypeId`. Text children are meaningful only to template
/// literals and cannot masquerade as type coordinates in another form.
fn resolved_type_child(
    facts: &FactSet<'_>,
    row: u32,
    position: usize,
    ids: &[Option<TypeId>],
    seen: &[u8],
) -> Result<TypeId, compiler_ir::BuildError> {
    let (target, _, _) = facts.staged_type_child(row, position).ok_or(
        compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Type,
            raw: row,
        },
    )?;
    if target == STAGED_TEXT_CHILD {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Type,
            raw: row,
        });
    }
    let index = facts.staged_type_slot(target).ok_or(compiler_ir::BuildError::Dangling {
        space: compiler_ir::SemanticSpace::Type,
        raw: target,
    })?;
    if seen.get(index).copied() != Some(2) {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Type,
            raw: target,
        });
    }
    ids.get(index)
        .copied()
        .flatten()
        .ok_or(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Type,
            raw: target,
        })
}

fn live_type<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    row: u32,
    ids: &mut [Option<TypeId>],
    seen: &mut [u8],
    scratch: &mut ProjectionScratch,
) -> Result<TypeId, compiler_ir::BuildError> {
    let index = facts.staged_type_slot(row).ok_or(compiler_ir::BuildError::Dangling {
        space: compiler_ir::SemanticSpace::Type,
        raw: row,
    })?;
    debug_assert!(index < ids.len());
    match seen[index] {
        2 => {
            return ids[index].ok_or(compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Type,
                raw: row,
            });
        }
        1 => return Err(compiler_ir::BuildError::RecursiveType { raw: row }),
        _ => {}
    }
    let record = facts.staged_type_record(row)?;
    // An explicit source `Unknown` is semantic truth (unannotated,
    // dynamically typed, unresolved, truncated, ...), not absence.  The
    // owned item type therefore never disappears merely because it is the
    // declaration's top-level row.
    seen[index] = 1;
    let child = |position: usize| facts.staged_type_child(row, position);
    let child_count = facts.staged_type_child_count(row).ok_or(
        compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Type,
            raw: row,
        },
    )?;
    for position in 0..child_count {
        if child(position).is_some_and(|item| item.0 == STAGED_TEXT_CHILD) {
            continue;
        }
        live_type(
            tree,
            facts,
            child(position)
                .map(|item| item.0)
                .ok_or(compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Type,
                    raw: row,
                })?,
            ids,
            seen,
            scratch,
        )?;
    }
    let child_type = |position| resolved_type_child(facts, row, position, ids, seen);
    let ty = match record.tag {
        SemanticTypeTag::Primitive => match (record.payload0, record.payload1, record.text) {
            (value, 0, Some(bytes))
                if value == u32::from(PrimitiveShape::Builtin)
                    && ((bytes.starts_with(b"\"") && bytes.ends_with(b"\""))
                        || (bytes.starts_with(b"'") && bytes.ends_with(b"'"))) =>
            {
                let value = bytes
                    .strip_prefix(b"\"")
                    .and_then(|value| value.strip_suffix(b"\""))
                    .or_else(|| {
                        bytes
                            .strip_prefix(b"'")
                            .and_then(|value| value.strip_suffix(b"'"))
                    })
                    .unwrap_or(bytes);
                let atom = tree.intern_atom(value)?;
                tree.intern_concrete(ConcreteType::Literal(LiteralType::String(atom)))?
                    .erase()
            }
            (value, 1, Some(bytes))
                if value == u32::from(PrimitiveShape::Builtin)
                    && bytes.iter().all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'+' | b'_')) =>
            {
                let atom = tree.intern_atom(bytes)?;
                tree.intern_concrete(ConcreteType::Literal(LiteralType::Number(atom)))?
                    .erase()
            }
            (value, 2, Some(bytes))
                if value == u32::from(PrimitiveShape::Builtin) && bytes.ends_with(b"n") =>
            {
                let atom = tree.intern_atom(bytes)?;
                tree.intern_concrete(ConcreteType::Literal(LiteralType::BigInt(atom)))?
                    .erase()
            }
            (value, 3, Some(bytes))
                if value == u32::from(PrimitiveShape::Builtin)
                    && matches!(bytes, b"true" | b"false") => tree
                .intern_concrete(ConcreteType::Literal(LiteralType::Boolean(
                    bytes == b"true",
                )))?
                .erase(),
            (_, _, _) => match PrimitiveShape::try_from(record.payload0) {
                Ok(PrimitiveShape::Bool) => tree
                    .intern_concrete(ConcreteType::Builtin(BuiltinType::Bool))?
                    .erase(),
                Ok(PrimitiveShape::LegacyChar) => tree
                    .intern_concrete(ConcreteType::Builtin(BuiltinType::LegacyChar))?
                    .erase(),
                Ok(
                    PrimitiveShape::UnicodeScalar
                    | PrimitiveShape::Utf16CodeUnit
                    | PrimitiveShape::Utf32CodeUnit
                    | PrimitiveShape::CPlainSignedChar
                    | PrimitiveShape::CPlainUnsignedChar
                    | PrimitiveShape::CSignedChar
                    | PrimitiveShape::CUnsignedChar
                    | PrimitiveShape::CWideChar
                    | PrimitiveShape::CWideSignedChar
                    | PrimitiveShape::CWideUnsignedChar,
                ) => {
                    let width = match TypeWidth::try_from_cell(record.payload1) {
                        Ok(TypeWidth::Fixed(width)) => NonZeroU16::new(width),
                        Ok(TypeWidth::Arch) | Err(_) => None,
                    }
                    .ok_or(compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    })?;
                    let role = match PrimitiveShape::try_from(record.payload0) {
                        Ok(PrimitiveShape::UnicodeScalar) => NativeCharacterRole::UnicodeScalar,
                        Ok(PrimitiveShape::Utf16CodeUnit) => NativeCharacterRole::Utf16CodeUnit,
                        Ok(PrimitiveShape::Utf32CodeUnit) => NativeCharacterRole::Utf32CodeUnit,
                        Ok(PrimitiveShape::CPlainSignedChar) => NativeCharacterRole::CPlainSigned,
                        Ok(PrimitiveShape::CPlainUnsignedChar) => NativeCharacterRole::CPlainUnsigned,
                        Ok(PrimitiveShape::CSignedChar) => NativeCharacterRole::CSigned,
                        Ok(PrimitiveShape::CUnsignedChar) => NativeCharacterRole::CUnsigned,
                        Ok(PrimitiveShape::CWideChar) => NativeCharacterRole::CWideSignednessUnavailable,
                        Ok(PrimitiveShape::CWideSignedChar) => NativeCharacterRole::CWideSigned,
                        Ok(PrimitiveShape::CWideUnsignedChar) => NativeCharacterRole::CWideUnsigned,
                        _ => unreachable!("character shape matched above"),
                    };
                    tree.intern_concrete(ConcreteType::NativeCharacter { role, width })?
                        .erase()
                }
                Ok(PrimitiveShape::Str) => tree
                    .intern_concrete(ConcreteType::Builtin(BuiltinType::String))?
                    .erase(),
                Ok(PrimitiveShape::Integer) => match (record.payload1 >> 1, record.payload1 & 1) {
                    (TypeWidth::ARCH_FLAG, 1) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::NativeSignedInteger))?
                        .erase(),
                    (TypeWidth::ARCH_FLAG, 0) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::NativeUnsignedInteger))?
                        .erase(),
                    (8, 0) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::U8))?
                        .erase(),
                    (8, 1) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::I8))?
                        .erase(),
                    (16, 0) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::U16))?
                        .erase(),
                    (16, 1) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::I16))?
                        .erase(),
                    (32, 0) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::U32))?
                        .erase(),
                    (32, 1) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::I32))?
                        .erase(),
                    (64, 0) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::U64))?
                        .erase(),
                    (64, 1) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::I64))?
                        .erase(),
                    (128, 0) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::U128))?
                        .erase(),
                    (128, 1) => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::I128))?
                        .erase(),
                    _ => tree
                        .intern_unknown(compiler_ir::UnknownType::new(
                            compiler_ir::UnknownReason::NoIrRepresentation,
                        ))?
                        .erase(),
                },
                Ok(PrimitiveShape::Builtin) => match record.text.and_then(builtin_from_spelling) {
                    Some(builtin) => tree.intern_concrete(ConcreteType::Builtin(builtin))?.erase(),
                    None => {
                        let spelling = record.text.map(|bytes| tree.intern_atom(bytes)).transpose()?;
                        tree.intern_unknown(compiler_ir::UnknownType {
                            reason: compiler_ir::UnknownReason::NoIrRepresentation,
                            spelling,
                        })?.erase()
                    }
                },
                Ok(PrimitiveShape::Float) => match record.payload1 {
                    16 => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::F16))?
                        .erase(),
                    32 => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::F32))?
                        .erase(),
                    64 => tree
                        .intern_concrete(ConcreteType::Builtin(BuiltinType::F64))?
                        .erase(),
                    _ => tree
                        .intern_unknown(compiler_ir::UnknownType::new(
                            compiler_ir::UnknownReason::NoIrRepresentation,
                        ))?
                        .erase(),
                },
                Ok(PrimitiveShape::Reference) => {
                    let lifetime = record
                        .text
                        .map(|bytes| tree.intern_atom(bytes))
                        .transpose()?;
                    tree.intern_concrete(ConcreteType::Reference {
                        target: child_type(0)?,
                        mutability: if record.payload1 == SemanticTypeRecord::INTEGER_SIGNED_FLAG {
                            compiler_ir::Mutability::Mutable
                        } else {
                            compiler_ir::Mutability::Immutable
                        },
                        lifetime,
                    })?
                    .erase()
                }
                Ok(PrimitiveShape::CPointer) => tree
                    .intern_concrete(ConcreteType::CPointer {
                        target: child_type(0)?,
                    })?
                    .erase(),
                Ok(PrimitiveShape::CBlockPointer) => tree
                    .intern_concrete(ConcreteType::CBlockPointer {
                        target: child_type(0)?,
                    })?
                    .erase(),
                Ok(PrimitiveShape::CxxLvalueReference | PrimitiveShape::CxxRvalueReference) => tree
                    .intern_concrete(ConcreteType::CxxReference {
                        target: child_type(0)?,
                        category: if record.payload0 == u32::from(PrimitiveShape::CxxLvalueReference) {
                            CxxReferenceCategory::Lvalue
                        } else {
                            CxxReferenceCategory::Rvalue
                        },
                    })?
                    .erase(),
                Ok(PrimitiveShape::CxxMemberPointer) => tree
                    .intern_concrete(ConcreteType::CxxMemberPointer {
                        owner: child_type(0)?,
                        member: child_type(1)?,
                    })?
                    .erase(),
                Ok(PrimitiveShape::ArbitraryInteger) => tree
                    .intern_concrete(ConcreteType::Builtin(BuiltinType::ArbitraryInteger))?
                    .erase(),
                Ok(PrimitiveShape::NativeSignedInteger) => tree
                    .intern_concrete(ConcreteType::Builtin(BuiltinType::NativeSignedInteger))?
                    .erase(),
                Ok(PrimitiveShape::NativeUnsignedInteger) => tree
                    .intern_concrete(ConcreteType::Builtin(BuiltinType::NativeUnsignedInteger))?
                    .erase(),
                Ok(PrimitiveShape::PointerAddressInteger) => tree
                    .intern_concrete(ConcreteType::Builtin(BuiltinType::PointerAddressInteger))?
                    .erase(),
                Ok(PrimitiveShape::MutPointer | PrimitiveShape::ConstPointer) => tree
                    .intern_concrete(ConcreteType::Pointer {
                        target: child_type(0)?,
                        mutability: if record.payload0 == PrimitiveShape::MutPointer as u32 {
                            compiler_ir::Mutability::Mutable
                        } else {
                            compiler_ir::Mutability::Immutable
                        },
                    })?
                    .erase(),
                _ => tree
                    .intern_unknown(compiler_ir::UnknownType::new(
                        compiler_ir::UnknownReason::NoIrRepresentation,
                    ))?
                    .erase(),
            },
        },
        SemanticTypeTag::Never => tree
            .intern_concrete(ConcreteType::Builtin(BuiltinType::Never))?
            .erase(),
        SemanticTypeTag::Any => tree
            .intern_concrete(ConcreteType::Builtin(BuiltinType::Any))?
            .erase(),
        SemanticTypeTag::Conditional if child_count == 4 => tree
            .intern_computed(ComputedType::Conditional {
                check: child_type(0)?,
                extends: child_type(1)?,
                then_type: child_type(2)?,
                else_type: child_type(3)?,
                distributive: record.payload1 != 0,
            })?
            .erase(),
        SemanticTypeTag::Mapped if matches!(child_count, 2 | 3) => {
            let parameter = tree.intern_atom(record.text.unwrap_or(b"K"))?;
            let modifier = |value: u32| match value {
                0 => compiler_ir::MappedModifier::Preserve,
                1 => compiler_ir::MappedModifier::Add,
                2 => compiler_ir::MappedModifier::Remove,
                _ => compiler_ir::MappedModifier::Preserve,
            };
            tree.intern_computed(ComputedType::Mapped {
                parameter,
                constraint: child_type(0)?,
                name_as: (child_count == 3).then(|| child_type(1)).transpose()?,
                value: child_type(child_count - 1)?,
                readonly: modifier(record.payload0),
                optional: modifier(record.payload1),
            })?
            .erase()
        }
        SemanticTypeTag::TemplateLiteral => {
            let start = scratch.begin_template_parts(child_count, row)?;
            for position in 0..child_count {
                let (target, text, _) = child(position).ok_or(
                    compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    },
                )?;
                let part = if target == STAGED_TEXT_CHILD {
                    TemplatePart::Bytes(tree.intern_atom(text.unwrap_or_default())?)
                } else {
                    TemplatePart::Placeholder(child_type(position)?)
                };
                scratch.template_parts.push(part);
            }
            let parts = tree.intern_template_parts(&scratch.template_parts[start..])?;
            scratch.template_parts.truncate(start);
            tree.intern_computed(ComputedType::TemplateLiteral(parts))?
                .erase()
        }
        SemanticTypeTag::SelfType if record.text == Some(b"this") => tree
            .intern_computed(ComputedType::This)?
            .erase(),
        SemanticTypeTag::SelfType | SemanticTypeTag::TypeVar => {
            let spelling = match record.text {
                Some(spelling) => spelling,
                None => b"Self",
            };
            let atom = tree.intern_atom(spelling)?;
            tree.intern_concrete(ConcreteType::Parameter(atom))?.erase()
        }
        SemanticTypeTag::Nominal => match record.nominal {
            Some(NominalRef::Local(id)) => tree
                .intern_concrete(ConcreteType::Nominal(id))?
                .erase(),
            Some(NominalRef::External(external)) => {
                let spelling = record.text.ok_or(compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Atom,
                    raw: external.ordinal,
                })?;
                let path = tree.intern_atom(spelling)?;
                let mut key = [0_u8; 36];
                key[..32].copy_from_slice(external.fragment.as_ref());
                key[32..].copy_from_slice(&external.ordinal.to_le_bytes());
                let target = tree.intern_external(ExternalTarget {
                    stable: StableEntityId::from_canonical_bytes(&key),
                    package: None,
                    path,
                    display: path,
                    kind: None,
                })?;
                tree.intern_concrete(ConcreteType::External(target))?.erase()
            }
            None => tree
                .intern_unknown(compiler_ir::UnknownType::new(
                    compiler_ir::UnknownReason::NoIrRepresentation,
                ))?
                .erase(),
        },
        SemanticTypeTag::Tuple => {
            if child_count == 0 {
                tree.intern_concrete(ConcreteType::Builtin(BuiltinType::Unit))?
                    .erase()
            } else {
                let start = scratch.begin_tuple_elements(child_count, row)?;
                for position in 0..child_count {
                    let (_, label, flags) = child(position).ok_or(
                        compiler_ir::BuildError::Dangling {
                            space: compiler_ir::SemanticSpace::Type,
                            raw: row,
                        },
                    )?;
                    scratch.tuple_elements.push(TupleElement {
                        label: label.map(|bytes| tree.intern_atom(bytes)).transpose()?,
                        ty: child_type(position)?,
                        kind: if flags & SemanticTypeChild::FLAG_REST != 0 {
                            TupleElementKind::Rest
                        } else if flags & SemanticTypeChild::FLAG_OPTIONAL != 0 {
                            TupleElementKind::Optional
                        } else {
                            TupleElementKind::Required
                        },
                    });
                }
                let list = tree.intern_tuple_elements(&scratch.tuple_elements[start..])?;
                scratch.tuple_elements.truncate(start);
                tree.intern_concrete(ConcreteType::Tuple(list))?.erase()
            }
        }
        SemanticTypeTag::Apply if child_count > 0 => {
            let start = scratch.begin_type_children(child_count - 1, row)?;
            for position in 1..child_count {
                scratch.type_children.push(child_type(position)?);
            }
            let arguments = tree.intern_types(&scratch.type_children[start..])?;
            scratch.type_children.truncate(start);
            tree.intern_concrete(ConcreteType::Applied {
                constructor: child_type(0)?,
                arguments,
            })?
            .erase()
        }
        SemanticTypeTag::Slice if child_count == 1 => tree
            .intern_concrete(ConcreteType::Slice(child_type(0)?))?
            .erase(),
        SemanticTypeTag::ArraySequence if child_count == 1 => {
            tree.intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: compiler_ir::ArrayShape::Sequence,
            })?
            .erase()
        }
        SemanticTypeTag::ArrayRectangular if child_count == 1 => {
            let rank = u16::try_from(record.payload0)
                .ok()
                .and_then(NonZeroU16::new)
                .ok_or(compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Type,
                    raw: row,
                })?;
            tree.intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: compiler_ir::ArrayShape::Rectangular { rank },
            })?
            .erase()
        }
        SemanticTypeTag::ArrayFixed if child_count == 1 => {
            tree.intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: compiler_ir::ArrayShape::FixedValue {
                    length: u64::from(record.payload0) | (u64::from(record.payload1) << 32),
                },
            })?
            .erase()
        }
        SemanticTypeTag::ArrayConstExpression if child_count == 1 => {
            let expression = record.text.ok_or(compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Atom,
                raw: row,
            })?;
            tree.intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: compiler_ir::ArrayShape::ConstExpression(tree.intern_atom(expression)?),
            })?
            .erase()
        }
        SemanticTypeTag::ArrayIncomplete if child_count == 1 => {
            tree.intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: compiler_ir::ArrayShape::Incomplete,
            })?
            .erase()
        }
        SemanticTypeTag::CQualified if child_count == 1 => tree
            .intern_concrete(ConcreteType::CQualified {
                target: child_type(0)?,
                qualifiers: CvQualifiers::try_from(record.payload0).map_err(|_| {
                    compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    }
                })?,
            })?
            .erase(),
        SemanticTypeTag::Wildcard => {
            let wildcard = match (record.payload0, child_count) {
                (0, 0) => WildcardBound::Unbounded,
                (1, 1) => WildcardBound::Extends(child_type(0)?),
                (2, 1) => WildcardBound::Super(child_type(0)?),
                _ => {
                    return Err(compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    });
                }
            };
            tree.intern_concrete(ConcreteType::Wildcard(wildcard))?.erase()
        }
        SemanticTypeTag::Annotated if child_count == 1 => {
            let kind = AnnotationKind::try_from(record.payload0).map_err(|_| {
                compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Type,
                    raw: row,
                }
            })?;
            tree.intern_concrete(ConcreteType::Annotated {
                kind,
                target: child_type(0)?,
            })?
            .erase()
        }
        SemanticTypeTag::Inferred => tree
            .intern_concrete(ConcreteType::Inferred(
                record.text.map(|bytes| tree.intern_atom(bytes)).transpose()?,
            ))?
            .erase(),
        SemanticTypeTag::QualifiedPath if child_count >= 1 => {
            let spelling = record.text.ok_or(compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Atom,
                raw: row,
            })?;
            // Legacy rows retain only source spelling. Their source authority
            // did not capture parsed components, so traversal remains typed
            // unavailable rather than fabricating one whole-spelling segment.
            let spelling = tree.intern_atom(spelling)?;
            tree.intern_concrete(ConcreteType::QualifiedPath {
                self_type: child_type(0)?,
                trait_type: (child_count == 2).then(|| child_type(1)).transpose()?,
                segments: QualifiedSegments::Unavailable,
                spelling,
            })?
            .erase()
        }
        SemanticTypeTag::Map if child_count == 2 => tree
            .intern_concrete(ConcreteType::Map {
                key: child_type(0)?,
                value: child_type(1)?,
            })?
            .erase(),
        SemanticTypeTag::Channel if child_count == 1 => {
            let direction = ChannelDirection::try_from(record.payload0).map_err(|_| {
                compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Type,
                    raw: row,
                }
            })?;
            tree.intern_concrete(ConcreteType::Channel {
                direction,
                element: child_type(0)?,
            })?
            .erase()
        }
        SemanticTypeTag::Union => {
            let start = scratch.begin_type_children(child_count, row)?;
            for position in 0..child_count {
                scratch.type_children.push(child_type(position)?);
            }
            let list = tree.intern_types(&scratch.type_children[start..])?;
            scratch.type_children.truncate(start);
            tree.intern_concrete(ConcreteType::Union(list))?.erase()
        }
        SemanticTypeTag::Intersection => {
            let start = scratch.begin_type_children(child_count, row)?;
            for position in 0..child_count {
                scratch.type_children.push(child_type(position)?);
            }
            let list = tree.intern_types(&scratch.type_children[start..])?;
            scratch.type_children.truncate(start);
            tree.intern_concrete(ConcreteType::Intersection(list))?
                .erase()
        }
        SemanticTypeTag::ImplTrait => {
            let start = scratch.begin_type_children(child_count, row)?;
            for position in 0..child_count {
                scratch.type_children.push(child_type(position)?);
            }
            let list = tree.intern_types(&scratch.type_children[start..])?;
            scratch.type_children.truncate(start);
            tree.intern_concrete(ConcreteType::ImplTrait(list))?.erase()
        }
        SemanticTypeTag::DynTrait => {
            let start = scratch.begin_type_children(child_count, row)?;
            for position in 0..child_count {
                scratch.type_children.push(child_type(position)?);
            }
            let list = tree.intern_types(&scratch.type_children[start..])?;
            scratch.type_children.truncate(start);
            tree.intern_concrete(ConcreteType::DynTrait(list))?.erase()
        }
        SemanticTypeTag::AnonymousRecord => {
            let start = scratch.begin_object_members(child_count, row)?;
            for position in 0..child_count {
                let (_, name, flags) = child(position).ok_or(
                    compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    },
                )?;
                let name = name.ok_or(compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Atom,
                    raw: position as u32,
                })?;
                scratch.object_members.push(ObjectMember::Property {
                    key: PropertyKey::Named(tree.intern_atom(name)?),
                    ty: child_type(position)?,
                    optional: flags & SemanticTypeChild::FLAG_OPTIONAL != 0,
                    readonly: flags & SemanticTypeChild::FLAG_READONLY != 0,
                });
            }
            let members = tree.intern_object_members(&scratch.object_members[start..])?;
            scratch.object_members.truncate(start);
            tree.intern_concrete(ConcreteType::Object(members))?.erase()
        }
        SemanticTypeTag::FunctionPointer => {
            let result_count = usize::try_from(record.function_result_count().ok_or(
                compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Type,
                    raw: row,
                },
            )?)
            .map_err(|_| compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Type,
                raw: row,
            })?;
            let parameter_count = child_count.checked_sub(result_count).ok_or(
                compiler_ir::BuildError::Dangling {
                    space: compiler_ir::SemanticSpace::Type,
                    raw: row,
                },
            )?;
            let parameter_start = scratch.begin_tuple_elements(parameter_count, row)?;
            for position in 0..parameter_count {
                let (target, child_name, flags) =
                    child(position).ok_or(compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    })?;
                let target = target as usize;
                let name = child_name.or_else(|| (target < facts.len).then(|| facts.names[target]));
                scratch.tuple_elements.push(TupleElement {
                    label: name.map(|name| tree.intern_atom(name)).transpose()?,
                    ty: child_type(position)?,
                    kind: if flags & SemanticTypeChild::FLAG_REST != 0 {
                        TupleElementKind::Rest
                    } else if flags & SemanticTypeChild::FLAG_OPTIONAL != 0 {
                        TupleElementKind::Optional
                    } else {
                        TupleElementKind::Required
                    },
                });
            }
            let parameters = tree.intern_tuple_elements(&scratch.tuple_elements[parameter_start..])?;
            scratch.tuple_elements.truncate(parameter_start);
            let result_start = scratch.begin_tuple_elements(result_count, row)?;
            for result in 0..result_count {
                let position = parameter_count + result;
                let (target, child_name, flags) =
                    child(position).ok_or(compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    })?;
                let target = target as usize;
                let name = child_name.or_else(|| (target < facts.len).then(|| facts.names[target]));
                scratch.tuple_elements.push(TupleElement {
                    label: name.map(|name| tree.intern_atom(name)).transpose()?,
                    ty: child_type(position)?,
                    kind: if flags & SemanticTypeChild::FLAG_REST != 0 {
                        TupleElementKind::Rest
                    } else if flags & SemanticTypeChild::FLAG_OPTIONAL != 0 {
                        TupleElementKind::Optional
                    } else {
                        TupleElementKind::Required
                    },
                });
            }
            let results = tree.intern_tuple_elements(&scratch.tuple_elements[result_start..])?;
            scratch.tuple_elements.truncate(result_start);
            tree.intern_concrete(ConcreteType::Function {
                parameters,
                results,
                abi: record.text.map(|abi| tree.intern_atom(abi)).transpose()?,
                variadic: record.function_variadic_form().ok_or(
                    compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    },
                )?,
                unsafe_: record.payload0 & SemanticTypeRecord::FUNCTION_UNSAFE_FLAG != 0,
            })?
            .erase()
        }
        SemanticTypeTag::Unknown => {
            let reason = match compiler_ir::TypeReason::try_from(record.payload0) {
                Ok(compiler_ir::TypeReason::Unannotated) => compiler_ir::UnknownReason::Unannotated,
                Ok(compiler_ir::TypeReason::DynamicallyTyped) => compiler_ir::UnknownReason::DynamicallyTyped,
                Ok(compiler_ir::TypeReason::UnresolvedLocalName) => compiler_ir::UnknownReason::UnresolvedLocalName,
                Ok(compiler_ir::TypeReason::UnresolvedExternal) => compiler_ir::UnknownReason::UnresolvedExternal,
                Ok(compiler_ir::TypeReason::TruncatedAtDepthLimit) => compiler_ir::UnknownReason::TruncatedAtDepthLimit,
                Ok(compiler_ir::TypeReason::OracleGap) => compiler_ir::UnknownReason::OracleGap,
                Ok(compiler_ir::TypeReason::NoIrRepresentation) | Err(_) => compiler_ir::UnknownReason::NoIrRepresentation,
            };
            let spelling = record.text.map(|bytes| tree.intern_atom(bytes)).transpose()?;
            tree.intern_unknown(compiler_ir::UnknownType { reason, spelling })?.erase()
        }
        // A closed row that has not gained a richer owned-IR variant remains
        // explicit semantic truth. Preserve any tag-owned spelling rather
        // than collapsing it into a renderer's generic `?unsupported`.
        _ => tree
            .intern_unknown(compiler_ir::UnknownType {
                reason: compiler_ir::UnknownReason::NoIrRepresentation,
                spelling: record.text.map(|bytes| tree.intern_atom(bytes)).transpose()?,
            })?
            .erase(),
    };
    ids[index] = Some(ty);
    seen[index] = 2;
    Ok(ty)
}

/// Maps only the spelling-bearing builtin rows in the common type lattice to
/// the closed cross-language builtin vocabulary.  Width-bearing primitives
/// remain payload-driven above; an unrecognized spelling stays an explicit
/// `NoIrRepresentation` unknown with its atom, never a false `String`.
const fn builtin_from_spelling(spelling: &[u8]) -> Option<BuiltinType> {
    match spelling {
        b"void" | b"System.Void" | b"java.lang.Void" => Some(BuiltinType::Void),
        b"bigint" => Some(BuiltinType::BigInt),
        b"symbol" => Some(BuiltinType::Symbol),
        b"unique symbol" => Some(BuiltinType::UniqueSymbol),
        b"null" => Some(BuiltinType::Null),
        b"undefined" => Some(BuiltinType::Undefined),
        b"None" | b"NoneType" => Some(BuiltinType::None_),
        b"bytes" | b"byte[]" => Some(BuiltinType::Bytes),
        b"list" | b"List" => Some(BuiltinType::List),
        b"dict" | b"Dict" => Some(BuiltinType::Dict),
        b"set" | b"Set" => Some(BuiltinType::Set),
        b"frozenset" | b"FrozenSet" => Some(BuiltinType::FrozenSet),
        b"complex" => Some(BuiltinType::Complex),
        b"decimal" | b"Decimal" => Some(BuiltinType::Decimal),
        b"object" | b"Object" | b"java.lang.Object" => Some(BuiltinType::Object),
        b"string" | b"str" | b"String" => Some(BuiltinType::String),
        b"boolean" | b"bool" | b"Boolean" => Some(BuiltinType::Bool),
        b"number" => Some(BuiltinType::Number),
        b"unknown" => Some(BuiltinType::Unknown),
        b"int" => Some(BuiltinType::ArbitraryInteger),
        b"isize" | b"nint" => Some(BuiltinType::NativeSignedInteger),
        b"usize" | b"uint" => Some(BuiltinType::NativeUnsignedInteger),
        b"uintptr" | b"nuint" => Some(BuiltinType::PointerAddressInteger),
        _ => None,
    }
}

/// Digests the producer-authored declaration skeleton of one fact.
///
/// This deliberately uses an unbounded preimage before the fixed-width
/// content digest.  Coordinates are deliberately *not* part of this key:
/// inserting an unrelated earlier fact must not remint every later stable
/// declaration.  Referenced declarations contribute through their own
/// stable keys at the qualified-scope layer; a source coordinate never
/// crosses this boundary. Length-prefix every byte field so adjacent cells
/// cannot alias one another.
fn fact_key_digest(fact: &SemanticFact<'_>) -> PayloadHash {
    fn bytes(out: &mut Vec<u8>, value: &[u8]) {
        out.extend_from_slice(&(value.len() as u64).to_le_bytes());
        out.extend_from_slice(value);
    }
    fn optional_bytes(out: &mut Vec<u8>, value: Option<&[u8]>) {
        match value {
            Some(value) => {
                out.push(1);
                bytes(out, value);
            }
            None => out.push(0),
        }
    }
    fn record(out: &mut Vec<u8>, value: SemanticTypeRecord<'_>) {
        out.push(u8::from(value.tag));
        out.extend_from_slice(&value.payload0.to_le_bytes());
        out.extend_from_slice(&value.payload1.to_le_bytes());
        optional_bytes(out, value.text);
        optional_bytes(out, value.text2);
        match value.nominal {
            None => out.push(0),
            Some(NominalRef::Local(target)) => {
                out.push(1);
                // A local nominal is resolved through its declaration key by
                // the enclosing projection. Its staging ordinal is never a
                // stable identity input.
                let _ = target;
            }
            Some(NominalRef::External(target)) => {
                out.push(2);
                out.extend_from_slice(target.fragment.as_ref());
                out.extend_from_slice(&target.ordinal.to_le_bytes());
            }
        }
    }

    let mut preimage = Vec::with_capacity(
        64 + fact.name.len()
            + fact.type_children[..usize::from(fact.type_child_count)]
                .iter()
                .map(|child| child.name.map_or(0, <[u8]>::len))
                .sum::<usize>(),
    );
    preimage.push(fact.kind as u8);
    bytes(&mut preimage, fact.name);
    preimage.push(u32::from(fact.constructor.tag) as u8);
    preimage.extend_from_slice(&fact.constructor.payload0.to_le_bytes());
    preimage.extend_from_slice(&fact.constructor.payload1.to_le_bytes());
    record(&mut preimage, fact.type_record);
    preimage.push(fact.type_child_count);
    for child in fact.type_children.iter().take(usize::from(fact.type_child_count)) {
        optional_bytes(&mut preimage, child.name);
        preimage.push(child.flags);
    }
    preimage.push(fact.child_count);
    for child in fact.children.iter().take(usize::from(fact.child_count)) {
        preimage.push(u8::from(child.role));
    }
    PayloadHash::from_canonical_bytes(&preimage)
}

/// Extends a structural declaration key with its recursively stable parent
/// key.  This distinguishes `a::value` from `b::value` without baking an
/// allocation-order ordinal into either identity.
fn scoped_fact_key(
    facts: &FactSet<'_>,
    ordinal: usize,
    cache: &mut [Option<PayloadHash>],
    visiting: &mut [bool],
) -> Result<PayloadHash, compiler_ir::BuildError> {
    let Some(cached) = cache.get(ordinal) else {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Entity,
            raw: ordinal as u32,
        });
    };
    if let Some(key) = *cached {
        return Ok(key);
    }
    if visiting.get(ordinal).copied().unwrap_or(false) {
        return Err(compiler_ir::BuildError::ParentCycle {
            entity: compiler_ir::EntityId::new(ordinal as u32),
        });
    }
    visiting[ordinal] = true;
    let parentage = facts.parentage.get(ordinal).copied();
    let parent = parentage.and_then(local_parent);
    let parent_key = match (parent, parentage) {
        (Some(parent), _) => scoped_fact_key(facts, parent.raw as usize, cache, visiting)?,
        (None, Some(StagedParentage::UnrepresentedAuthorityOwner { identity })) => {
            let mut preimage = [0_u8; 48];
            preimage[..32].copy_from_slice(b"compiler.unrepresented-parent.v1");
            preimage[32..].copy_from_slice(&identity);
            PayloadHash::from_canonical_bytes(&preimage)
        }
        (None, Some(StagedParentage::Root | StagedParentage::Unavailable)) | (None, None) => {
            PayloadHash::from_raw([0; 16])
        }
        // `Bound` is written atomically with a concrete parent row.
        (None, Some(StagedParentage::Bound { .. })) => {
            return Err(compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Entity,
                raw: ordinal as u32,
            });
        }
    };
    let structural = *facts.key_digests.get(ordinal).ok_or(
        compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Entity,
            raw: ordinal as u32,
        },
    )?;
    let mut preimage = Vec::with_capacity(64 + MAX_FACT_CHILDREN * 16 + MAX_TYPE_CHILDREN * 16);
    preimage.extend_from_slice(b"compiler.declaration-skeleton.v2");
    preimage.push(u8::from(parent.is_some()));
    preimage.extend_from_slice(parent_key.as_ref());
    preimage.extend_from_slice(structural.as_ref());
    // Child declaration references contribute their own qualified semantic
    // keys, never staging positions. `foo(A)` and `foo(B)` therefore differ
    // while inserting an unrelated declaration leaves both intact.
    let child_start = facts.child_starts[ordinal] as usize;
    let child_count = usize::from(facts.child_counts[ordinal]);
    for child in facts.child_targets[child_start..child_start + child_count].iter() {
        preimage.extend_from_slice(
            scoped_fact_key(facts, *child as usize, cache, visiting)?.as_ref(),
        );
    }
    let type_child_start = facts.type_child_starts[ordinal] as usize;
    let type_child_count = usize::from(facts.type_child_counts[ordinal]);
    for ((target, name), flags) in facts.type_child_targets
        [type_child_start..type_child_start + type_child_count]
        .iter()
        .zip(&facts.type_child_names[type_child_start..type_child_start + type_child_count])
        .zip(&facts.type_child_flags[type_child_start..type_child_start + type_child_count])
    {
        if *target == STAGED_TEXT_CHILD {
            preimage.extend_from_slice(b"compiler.template-text-child.v1");
            preimage.push(*flags);
            match name {
                Some(bytes) => {
                    preimage.push(1);
                    preimage.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
                    preimage.extend_from_slice(bytes);
                }
                None => preimage.push(0),
            }
        } else {
            append_type_target_key(&mut preimage, facts, ordinal, *target, cache, visiting)?;
        }
    }
    if let Some(NominalRef::Local(target)) = facts.type_records[ordinal].nominal {
        append_type_target_key(&mut preimage, facts, ordinal, target.raw, cache, visiting)?;
    }
    let key = PayloadHash::from_canonical_bytes(&preimage);
    visiting[ordinal] = false;
    cache[ordinal] = Some(key);
    Ok(key)
}

/// Extends a declaration skeleton with one semantic type target. Named
/// declaration rows recurse through their stable key; transaction-local
/// anonymous/computed rows contribute an intrinsic coordinate-free shape.
fn append_type_target_key(
    preimage: &mut Vec<u8>,
    facts: &FactSet<'_>,
    owner: usize,
    target: u32,
    cache: &mut [Option<PayloadHash>],
    visiting: &mut [bool],
) -> Result<(), compiler_ir::BuildError> {
    if target < facts.len as u32 && target != owner as u32 {
        preimage.push(0);
        preimage.extend_from_slice(scoped_fact_key(facts, target as usize, cache, visiting)?.as_ref());
    } else if target == owner as u32 {
        preimage.extend_from_slice(b"self-type");
    } else {
        preimage.push(1);
        preimage.extend_from_slice(
            staged_type_shape_key(facts, target, owner, cache, visiting)?.as_ref(),
        );
    }
    Ok(())
}

/// Coordinate-free shape key for a non-declaration staged type row. Named
/// declaration targets are resolved by `append_type_target_key`; this covers
/// anonymous/computed rows without leaking their segmented coordinates into
/// a declaration or overload identity.
fn staged_type_shape_key(
    facts: &FactSet<'_>,
    row: u32,
    owner: usize,
    cache: &mut [Option<PayloadHash>],
    fact_visiting: &mut [bool],
) -> Result<PayloadHash, compiler_ir::BuildError> {
    staged_type_shape_key_inner(facts, row, owner, cache, fact_visiting, &mut Vec::new())
}

fn staged_type_shape_key_inner(
    facts: &FactSet<'_>,
    row: u32,
    owner: usize,
    cache: &mut [Option<PayloadHash>],
    fact_visiting: &mut [bool],
    type_visiting: &mut Vec<u32>,
) -> Result<PayloadHash, compiler_ir::BuildError> {
    // Recursive source types are semantically real.  A cycle marker is
    // coordinate-free and keeps a self edge distinct from a missing child.
    if type_visiting.contains(&row) {
        return Ok(PayloadHash::from_canonical_bytes(b"compiler.type-cycle.v1"));
    }
    type_visiting.push(row);
    let (record, targets, names, flags, row_owner) = if row < facts.len as u32 {
        let ordinal = row as usize;
        let start = facts.type_child_starts[ordinal] as usize;
        let count = usize::from(facts.type_child_counts[ordinal]);
        (
            facts.type_records[ordinal],
            &facts.type_child_targets[start..start + count],
            &facts.type_child_names[start..start + count],
            &facts.type_child_flags[start..start + count],
            ordinal,
        )
    } else if facts.is_anonymous_type_row(row) {
        let ordinal = (row - ANONYMOUS_ROW_BASE) as usize;
        let start = facts.anonymous_child_starts[ordinal] as usize;
        let count = usize::from(facts.anonymous_child_counts[ordinal]);
        let row_owner = usize::try_from(facts.anonymous_owners[ordinal]).map_err(|_| {
            compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Entity,
                raw: facts.anonymous_owners[ordinal],
            }
        })?;
        (
            facts.anonymous_records[ordinal],
            &facts.anonymous_child_targets[start..start + count],
            &facts.anonymous_child_names[start..start + count],
            &facts.anonymous_child_flags[start..start + count],
            row_owner,
        )
    } else if facts.is_computed_type_row(row) {
        let ordinal = (row - COMPUTED_ROW_BASE) as usize;
        let start = facts.computed_child_starts[ordinal] as usize;
        let count = usize::from(facts.computed_child_counts[ordinal]);
        let row_owner = usize::try_from(facts.computed_owners[ordinal]).map_err(|_| {
            compiler_ir::BuildError::Dangling {
                space: compiler_ir::SemanticSpace::Entity,
                raw: facts.computed_owners[ordinal],
            }
        })?;
        (
            facts.computed_records[ordinal],
            &facts.computed_child_targets[start..start + count],
            &facts.computed_child_names[start..start + count],
            &facts.computed_child_flags[start..start + count],
            row_owner,
        )
    } else {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Type,
            raw: row,
        });
    };
    if row_owner >= facts.len {
        return Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Entity,
            raw: u32::try_from(row_owner).unwrap_or(u32::MAX),
        });
    }
    let mut preimage = Vec::with_capacity(48);
    preimage.extend_from_slice(b"compiler.staged-type-shape.v1");
    // A staged compound belongs to the declaration that admitted it. Its
    // physical row coordinate is deliberately excluded, but its owner's
    // coordinate-free scoped key must remain part of the shape whenever a
    // different declaration reaches it. The same-owner marker avoids a
    // recursive key lookup while preserving that self relationship.
    preimage.extend_from_slice(b"compiler.staged-type-owner.v1");
    if row_owner == owner {
        preimage.extend_from_slice(b"self-owner");
    } else {
        preimage.extend_from_slice(
            scoped_fact_key(facts, row_owner, cache, fact_visiting)?.as_ref(),
        );
    }
    preimage.push(u8::from(record.tag));
    preimage.extend_from_slice(&record.payload0.to_le_bytes());
    preimage.extend_from_slice(&record.payload1.to_le_bytes());
    for text in [record.text, record.text2] {
        match text {
            Some(text) => {
                preimage.push(1);
                preimage.extend_from_slice(&(text.len() as u64).to_le_bytes());
                preimage.extend_from_slice(text);
            }
            None => preimage.push(0),
        }
    }
    match record.nominal {
        Some(NominalRef::Local(target)) if target.raw == row_owner as u32 => {
            preimage.extend_from_slice(b"owner-nominal");
        }
        Some(NominalRef::Local(target)) if target.raw < facts.len as u32 => {
            preimage.extend_from_slice(
                scoped_fact_key(facts, target.raw as usize, cache, fact_visiting)?.as_ref(),
            );
        }
        Some(NominalRef::Local(target)) => preimage.extend_from_slice(
            staged_type_shape_key_inner(
                facts,
                target.raw,
                row_owner,
                cache,
                fact_visiting,
                type_visiting,
            )?
            .as_ref(),
        ),
        Some(NominalRef::External(target)) => {
            preimage.extend_from_slice(target.fragment.as_ref());
            preimage.extend_from_slice(&target.ordinal.to_le_bytes());
        }
        None => preimage.push(0),
    }
    preimage.extend_from_slice(&(targets.len() as u64).to_le_bytes());
    for ((target, name), flags) in targets.iter().zip(names).zip(flags) {
        match name {
            Some(name) => {
                preimage.push(1);
                preimage.extend_from_slice(&(name.len() as u64).to_le_bytes());
                preimage.extend_from_slice(name);
            }
            None => preimage.push(0),
        }
        preimage.push(*flags);
        if *target == STAGED_TEXT_CHILD {
            // Exact bytes and flags were framed above. This sentinel is never
            // a type row and therefore never enters recursive shape lookup.
            preimage.extend_from_slice(b"template-text");
        } else if *target == row_owner as u32 {
            preimage.extend_from_slice(b"owner-type");
        } else if *target < facts.len as u32 {
            preimage.extend_from_slice(
                scoped_fact_key(facts, *target as usize, cache, fact_visiting)?.as_ref(),
            );
        } else {
            preimage.extend_from_slice(
                staged_type_shape_key_inner(
                    facts,
                    *target,
                    row_owner,
                    cache,
                    fact_visiting,
                    type_visiting,
                )?
                .as_ref(),
            );
        }
    }
    let key = PayloadHash::from_canonical_bytes(&preimage);
    let _ = type_visiting.pop();
    Ok(key)
}

const fn item_kind(kind: EntityKind) -> ItemKind {
    match kind {
        EntityKind::Function => ItemKind::Function,
        EntityKind::Constant => ItemKind::Constant,
        EntityKind::Record => ItemKind::Record,
        EntityKind::Module => ItemKind::Module,
        EntityKind::Field => ItemKind::Field,
        EntityKind::Alias => ItemKind::TypeAlias,
        EntityKind::Trait => ItemKind::Trait,
        EntityKind::Implementation => ItemKind::Implementation,
        EntityKind::Enum => ItemKind::Enum,
        EntityKind::Variant => ItemKind::Variant,
        EntityKind::Static => ItemKind::Static,
        EntityKind::Reexport => ItemKind::Reexport,
        EntityKind::Parameter => ItemKind::Parameter,
    }
}

/// Mints a declaration key from explicit package/file provenance plus an
/// order-free producer skeleton. The caller passes the recursively stable
/// parent key as the skeleton prefix, so nested same-name declarations do
/// not collide. Source content, staged ordinals, and spans never enter this
/// key; they belong to the payload/version planes.
fn fact_version(
    scope: crate::types::DeclarationScope<'_>,
    profile: compiler_vocabulary::LanguageProfile,
    parent_key: PayloadHash,
    kind: EntityKind,
    name: &[u8],
    declaration_skeleton: PayloadHash,
) -> Result<EntityVersion, compiler_ir::BuildError> {
    let key = DeclarationKey::new(scope.lineage(), scope.path(), kind, name).map_err(|_| {
        compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Entity,
            raw: 0,
        }
    })?;
    let profile_bytes: [u8; 2] = profile.into();
    let mut skeleton = [0_u8; 34];
    skeleton[..2].copy_from_slice(&profile_bytes);
    skeleton[2..18].copy_from_slice(parent_key.as_ref());
    skeleton[18..].copy_from_slice(declaration_skeleton.as_ref());
    let mut preimage = vec![0_u8; key.preimage_len(Disambiguator::Skeleton(&skeleton))];
    let stable = key
        .stable_id(Disambiguator::Skeleton(&skeleton), &mut preimage)
        .map_err(|_| compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Entity,
            raw: 0,
        })?;
    let mut stable_bytes = [0_u8; 16];
    stable_bytes.copy_from_slice(&stable.as_ref()[..16]);
    // The complete (coordinate-free) skeleton is the version payload basis.
    // Visibility, docs, provenance, and language extensions are projected
    // separately below; this prevents the old name-only payload collision.
    Ok(EntityVersion {
        stable: StableEntityId::from_raw(stable_bytes),
        payload: declaration_skeleton,
    })
}

/// Exact canonicalization, preparation, or write failure of the emission
/// lane. Every fact-level invariant is already proven by [`FactSet::push`],
/// so the remaining terminals carry exact typed causes without fact operands.
#[derive(Debug)]
pub(super) enum AdmissionFault {
    Canonical(CanonicalDataError),
    Prepare(PrepareError),
    Write(WriteError),
    /// One extension fact named a provisional atom outside the admitted
    /// extension-atom lane.
    ExtensionAtom {
        row: usize,
        provisional: u32,
        atom_count: usize,
    },
    /// A generic extension was captured without one exact staged list range,
    /// or that range escaped the admitted type-parameter element prefix.
    ExtensionTypeParameters {
        row: usize,
        start: u32,
        length: u32,
        element_count: usize,
    },
}

/// Exact per-admission canonicalization work reservation, calculated from the
/// transaction-local geometry rather than a global worst-case lane.
fn emission_budget(facts: usize, children: usize) -> DataResourceBudget {
    let products = facts as u64;
    let children = children as u64;
    let hash = 2_u64.saturating_mul(products).saturating_mul(products);
    let sort = products.saturating_mul(products).saturating_add(
        products
            .saturating_mul(products.saturating_mul(products).saturating_add(2 * products)),
    );
    let probe = products.saturating_mul(products);
    let child_visits = 2_u64
        .saturating_mul(children)
        .saturating_mul(products)
        .saturating_add(children.saturating_mul(sort.saturating_add(probe)));
    let lane = 4_u64
        .saturating_mul(products)
        .saturating_add(children);
    DataResourceBudget {
        max_refinement_rounds: u32::try_from(facts).unwrap_or(u32::MAX),
        max_hash_evaluations: hash,
        max_sort_comparisons: sort,
        max_intern_probes: probe,
        max_work: hash
            .saturating_add(sort)
            .saturating_add(probe)
            .saturating_add(child_visits)
            .saturating_add(lane),
    }
}

/// Closed set of committed primitive type nodes.
const PRIMITIVE_NODE_CAPACITY: usize = 3;
/// One optional opaque-type sentinel plus one node per distinct primitive.
const MAX_TYPE_NODES: usize = 1 + PRIMITIVE_NODE_CAPACITY;

/// Leaf product constructor of every named value declaration: a bare named
/// product with no proven children.
pub(super) const LEAF_PRODUCT: SemanticProductConstructor = SemanticProductConstructor::generic(0);

/// Empty TypeScript pool row used before first write.
const fn empty_typescript_facts() -> compiler_ir::TypeScriptFacts {
    compiler_ir::TypeScriptFacts {
        type_parameters: compiler_ir::TypeParameterListId::new(0),
        declared: None,
        observed: None,
    }
}

/// Empty C# pool row used before first write.
const fn empty_csharp_facts() -> compiler_ir::CSharpFacts {
    compiler_ir::CSharpFacts {
        nullability: compiler_ir::CSharpNullability::Oblivious,
        reference_kind: compiler_ir::CSharpReferenceKind::Value,
        constraints: compiler_ir::TypeParameterListId::new(0),
        effects: compiler_ir::CSharpMemberEffects {
            is_async: false,
            is_iterator: false,
            is_extension: false,
        },
        attributes: compiler_ir::AtomListId::new(0),
        partial: compiler_ir::CSharpPartialRole::None,
        xml_provenance: None,
    }
}

/// Empty Go pool row used before first write.
const fn empty_go_facts() -> compiler_ir::GoFacts {
    compiler_ir::GoFacts {
        signature: compiler_ir::GoSignature {
            parameters: compiler_ir::TypeListId::new(0),
            results: compiler_ir::TypeListId::new(0),
            variadic: false,
        },
        type_parameters: compiler_ir::TypeParameterListId::new(0),
        fields: compiler_ir::EntityListId::new(0),
        method_set: compiler_ir::EntityListId::new(0),
        build_constraints: compiler_ir::AtomListId::new(0),
        constant_value: compiler_ir::AtomListId::new(0),
        constant_group: 0,
        constant_flags: 0,
    }
}

/// Empty Rust pool row used before first write.
const fn empty_rust_facts() -> compiler_ir::RustFacts {
    compiler_ir::RustFacts {
        ownership: compiler_ir::RustOwnership::Value,
        lifetimes: compiler_ir::AtomListId::new(0),
        where_clauses: compiler_ir::TypeParameterListId::new(0),
        macros: compiler_ir::AtomListId::new(0),
    }
}

/// Empty Python pool row used before first write.
const fn empty_python_facts() -> compiler_ir::PythonFacts {
    compiler_ir::PythonFacts {
        decorators: compiler_ir::AtomListId::new(0),
        parameter_kind: compiler_ir::PythonParameterKind::PositionalOrKeyword,
        dynamic_confidence: compiler_ir::Confidence::Syntactic,
    }
}

/// Empty Java pool row used before first write.
const fn empty_java_facts() -> compiler_ir::JavaFacts {
    compiler_ir::JavaFacts {
        throws: compiler_ir::TypeListId::new(0),
        annotations: compiler_ir::AtomListId::new(0),
        overloads: compiler_ir::EntityListId::new(0),
        record_components: compiler_ir::EntityListId::new(0),
    }
}

/// Empty Clang pool row used before first write.
const fn empty_clang_facts() -> compiler_ir::ClangFacts {
    compiler_ir::ClangFacts {
        qualifiers: compiler_ir::ClangQualifiers {
            is_const: false,
            is_volatile: false,
            is_restrict: false,
        },
        storage: compiler_ir::ClangStorageClass::None,
        layout: compiler_ir::ClangLayout {
            size_bits: None,
            align_bits: None,
        },
        templates: compiler_ir::TypeParameterListId::new(0),
        includes: compiler_ir::AtomListId::new(0),
    }
}

/// Admits one collector-emitted fact into the lane.
///
/// Collectors construct facts whose identifiers are parsed source words,
/// whose constructors are closed constants matched to their constructed
/// child counts, and whose targets are already-pushed ordinals; the single
/// reachable rejection class is the bounded lane capacity. A source beyond
/// the lane's compact-recipe capacity keeps the exact closed terminal, and
/// every rejection retains its full typed cause by value.
pub(super) fn push_fact<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<usize, FactRejection> {
    match facts.push(fact) {
        Ok(ordinal) => Ok(ordinal),
        Err(rejected) => Err(rejected.rejection()),
    }
}

/// Admits the ordered fact set into one prepared fragment.
///
/// An empty set writes the exact current-schema fragment without a semantic-data
/// section. A populated set canonicalizes the facts' products, commits the
/// entities, type nodes, name atoms, and the optional semantic section, and
/// writes everything into the caller-owned output.
#[expect(
    clippy::indexing_slicing,
    reason = "every lane position is bounded by MAX_EMISSION_FACTS, MAX_FACT_CHILDREN, and the closed primitive set; the exact node prefix is admitted before the single trusted subslice projection"
)]
pub(super) fn admit<'source, 'output>(
    facts: &FactSet<'source>,
    source: SourceIdentity,
    recipe: RecipeFact,
    profile: compiler_vocabulary::LanguageProfile,
    output: &'output mut [u8],
) -> Result<&'output [u8], AdmissionFault> {
    if facts.len == 0 {
        let prepared = PreparedFragment::prepare(source, recipe, &[], &[], &[]);
        return write_prepared(prepared, output);
    }
    let fact_count = facts.len;
    let extension_atom_count = facts.extension_atom_len;

    // Type-node lane: the opaque sentinel (when any fact needs it) followed by
    // one node per distinct proven primitive in first-use order.
    // The compact index has only Bool/I32/String primitive opcodes.  Keep
    // every other exact primitive in the type-fact lane and use the explicit
    // opaque compact sentinel rather than falsely publishing (say) `u64` as
    // `String`.
    let any_opaque = (0..fact_count)
        .any(|ordinal| compact_primitive_node(facts.type_records[ordinal]).is_none());
    let mut nodes = [TypeNode::Reference(TypeId::new(0)); MAX_TYPE_NODES];
    let mut node_count = 0;
    let mut primitive_nodes = [0_usize; PRIMITIVE_NODE_CAPACITY];
    if any_opaque {
        // Documented opaque-type sentinel: the node references itself, which
        // no proven-primitive node ever does.
        nodes[0] = TypeNode::Reference(TypeId::new(0));
        node_count = 1;
    }
    let mut fact_type_nodes = vec![0_usize; fact_count].into_boxed_slice();
    for (ordinal, record) in facts.type_records[..fact_count].iter().enumerate() {
        fact_type_nodes[ordinal] = match compact_primitive_node(*record) {
            None => 0,
            Some((code, node)) => {
                if primitive_nodes[code] == 0 {
                    nodes[node_count] = node;
                    node_count += 1;
                    primitive_nodes[code] = node_count;
                }
                primitive_nodes[code] - 1
            }
        };
    }
    let node_prefix = &nodes[..node_count];

    let atom_count = fact_count + extension_atom_count;
    let mut entities = vec![
        EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Function,
        };
        fact_count
    ]
    .into_boxed_slice();
    let mut atoms = vec![AtomInput { bytes: b"" }; atom_count].into_boxed_slice();
    for ordinal in 0..fact_count {
        #[expect(
            clippy::as_conversions,
            reason = "node positions, fact ordinals, and pooled cursors are bounded by the fixed emission lane and widen totally to u32 coordinates"
        )]
        let (type_node, name_atom) = (fact_type_nodes[ordinal] as u32, ordinal as u32);
        entities[ordinal] = EntityRecord {
            semantic_type: TypeId::new(type_node),
            name: AtomId::new(name_atom),
            kind: facts.kinds[ordinal],
        };
        atoms[ordinal] = AtomInput {
            bytes: facts.names[ordinal],
        };
    }
    for (index, bytes) in facts.extension_atoms[..extension_atom_count]
        .iter()
        .enumerate()
    {
        atoms[fact_count + index] = AtomInput { bytes };
    }

    let mut fact_products = vec![
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        };
        fact_count
    ]
    .into_boxed_slice();
    let mut fact_lists =
        vec![ListSpan::<ProductChildren>::new(0, 0); fact_count].into_boxed_slice();
    let mut fact_children = vec![
        SemanticProductChild {
            target: ProductRef::Local(ProductId::new(0)),
            role: ProductChildRole::ProductMember,
        };
        facts.total_children
    ]
    .into_boxed_slice();
    let mut pooled_cursor = 0_usize;
    for ordinal in 0..fact_count {
        let child_count = usize::from(facts.child_counts[ordinal]);
        #[expect(
            clippy::as_conversions,
            reason = "fact ordinals and pooled cursors are bounded by the fixed emission lane and widen totally to u32 coordinates"
        )]
        let (fact_coordinate, pooled_start, length) =
            (ordinal as u32, pooled_cursor as u32, child_count as u32);
        fact_products[ordinal] = SemanticProduct {
            head: AtomId::new(fact_coordinate),
            children: ProductListId::new(fact_coordinate),
        };
        fact_lists[ordinal] = ListSpan::new(pooled_start, length);
        for offset in 0..child_count {
            let pooled = pooled_cursor + offset;
            fact_children[pooled] = SemanticProductChild {
                target: ProductRef::Local(ProductId::new(facts.child_targets[pooled])),
                role: facts.child_roles[pooled],
            };
        }
        pooled_cursor += child_count;
    }

    let mut semantic_atoms =
        vec![SemanticAtom { bytes: b"" }; fact_count].into_boxed_slice();
    for (ordinal, name) in facts.names[..fact_count].iter().enumerate() {
        semantic_atoms[ordinal] = SemanticAtom { bytes: name };
    }
    let mut scratch_atom_order = vec![AtomId::new(0); fact_count].into_boxed_slice();
    let mut scratch_atom_map = vec![0_u32; fact_count].into_boxed_slice();
    let mut scratch_product_order = vec![ProductId::new(0); fact_count].into_boxed_slice();
    let mut scratch_product_map = vec![0_u32; fact_count].into_boxed_slice();
    let mut scratch_colors = vec![0_u32; fact_count].into_boxed_slice();
    let mut scratch_next_colors = vec![0_u32; fact_count].into_boxed_slice();
    let mut scratch_hashes = vec![0_u64; fact_count].into_boxed_slice();
    let mut scratch_next_hashes = vec![0_u64; fact_count].into_boxed_slice();
    let mut scratch_representatives =
        vec![ProductId::new(0); fact_count].into_boxed_slice();
    let mut scratch_intern_slots = vec![0_u64; fact_count].into_boxed_slice();
    let mut data_scratch = DataScratch {
        atom_order: &mut scratch_atom_order[..],
        atom_to_canonical: &mut scratch_atom_map[..],
        product_order: &mut scratch_product_order[..],
        product_to_canonical: &mut scratch_product_map[..],
        colors: &mut scratch_colors[..],
        next_colors: &mut scratch_next_colors[..],
        hashes: &mut scratch_hashes[..],
        next_hashes: &mut scratch_next_hashes[..],
        product_representatives: &mut scratch_representatives[..],
        intern_slots: &mut scratch_intern_slots[..],
    };
    let mut output_atoms = vec![SemanticAtom { bytes: b"" }; fact_count].into_boxed_slice();
    let mut output_products = vec![
        SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        };
        fact_count
    ]
    .into_boxed_slice();
    let mut output_constructors =
        vec![SemanticProductConstructor::PRODUCT; fact_count].into_boxed_slice();
    let mut output_lists =
        vec![ListSpan::<ProductChildren>::new(0, 0); fact_count].into_boxed_slice();
    let mut output_children = vec![
        SemanticProductChild {
            target: ProductRef::Local(ProductId::new(0)),
            role: ProductChildRole::ProductMember,
        };
        facts.total_children
    ]
    .into_boxed_slice();
    let mut data_output = DataOutput {
        atoms: &mut output_atoms[..],
        products: &mut output_products[..],
        constructors: &mut output_constructors[..],
        lists: &mut output_lists[..],
        children: &mut output_children[..],
    };
    let semantic = canonicalize_data_with_budget(
        DataFacts {
            atoms: &semantic_atoms[..fact_count],
            products: &fact_products[..fact_count],
            constructors: &facts.constructors[..fact_count],
            lists: &fact_lists[..fact_count],
            children: &fact_children[..pooled_cursor],
        },
        &mut data_scratch,
        &mut data_output,
        emission_budget(fact_count, pooled_cursor),
    )
    .map_err(AdmissionFault::Canonical)?;

    let anonymous_rows = facts.anonymous_rows;
    let declared_count = anonymous_rows + fact_count;
    let mut type_facts = vec![
        TypeFactInput {
            owner: compiler_ir::EntityId::new(0),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Unknown),
        };
        declared_count
    ]
    .into_boxed_slice();
    let mut type_children = vec![
        SemanticTypeChild {
            target: TypeChildTarget::Text,
            name: None,
            flags: 0,
        };
        facts.total_type_children
            + facts.anonymous_children_total
            + facts.computed_children_total
    ]
    .into_boxed_slice();
    // Lane order: anonymous rows first (topological by construction), then
    // fact rows — every fact-row child and anonymous child points backward.
    let remap = |target: u32| -> u32 {
        if target >= COMPUTED_ROW_BASE {
            declared_count as u32 + target - COMPUTED_ROW_BASE
        } else if target >= ANONYMOUS_ROW_BASE {
            target - ANONYMOUS_ROW_BASE
        } else {
            target + anonymous_rows as u32
        }
    };
    let mut type_parameters = vec![
        ExtensionTypeParameter {
            name: &[],
            bounds: compiler_ir::ExtensionTypeParameterBoundRange {
                start: 0,
                length: 0,
            },
            default: None,
            variance: compiler_ir::Variance::Invariant,
            kind: compiler_ir::ExtensionTypeParameterKind::Type {
                inference: compiler_ir::TypeParameterInference::Ordinary,
            },
            requirements: compiler_ir::TypeParameterRequirements::none(),
        };
        facts.type_parameter_len
    ]
    .into_boxed_slice();
    type_parameters[..facts.type_parameter_len]
        .copy_from_slice(&facts.type_parameters[..facts.type_parameter_len]);
    let mut type_parameter_bounds = vec![
        compiler_ir::ExtensionTypeParameterBound::Type(0);
        facts.type_parameter_bound_len
    ]
    .into_boxed_slice();
    for (index, bound) in facts.type_parameter_bounds[..facts.type_parameter_bound_len]
        .iter()
        .enumerate()
    {
        type_parameter_bounds[index] = match bound {
            compiler_ir::ExtensionTypeParameterBound::Type(raw) => {
                compiler_ir::ExtensionTypeParameterBound::Type(remap(*raw))
            }
            compiler_ir::ExtensionTypeParameterBound::Lifetime(name) => {
                compiler_ir::ExtensionTypeParameterBound::Lifetime(name)
            }
        };
    }
    for parameter in type_parameters[..facts.type_parameter_len].iter_mut() {
        parameter.default = parameter.default.map(remap);
        if let compiler_ir::ExtensionTypeParameterKind::ConstValue { value_type } = parameter.kind {
            parameter.kind = compiler_ir::ExtensionTypeParameterKind::ConstValue {
                value_type: remap(value_type),
            };
        }
    }
    let mut type_pooled_cursor = 0_usize;
    for (index, record) in facts.anonymous_records[..anonymous_rows].iter().enumerate() {
        let child_count = usize::from(facts.anonymous_child_counts[index]);
        let record = SemanticTypeRecord {
            children: ListSpan::new(type_pooled_cursor as u32, child_count as u32),
            ..*record
        };
        let base = facts.anonymous_child_starts[index] as usize;
        for offset in 0..child_count {
            let pooled = type_pooled_cursor + offset;
            let raw = facts.anonymous_child_targets[base + offset];
            type_children[pooled] = SemanticTypeChild {
                target: if raw == STAGED_TEXT_CHILD {
                    TypeChildTarget::Text
                } else {
                    TypeChildTarget::Type(compiler_ir::TypeRef::Local(
                        compiler_ir::TypeId::new(remap(raw)),
                    ))
                },
                name: facts.anonymous_child_names[base + offset],
                flags: facts.anonymous_child_flags[base + offset],
            };
        }
        type_pooled_cursor += child_count;
        type_facts[index] = TypeFactInput {
            owner: compiler_ir::EntityId::new(facts.anonymous_owners[index]),
            record,
        };
    }
    for ordinal in 0..fact_count {
        let child_count = usize::from(facts.type_child_counts[ordinal]);
        let record = SemanticTypeRecord {
            children: ListSpan::new(type_pooled_cursor as u32, child_count as u32),
            ..facts.type_records[ordinal]
        };
        for offset in 0..child_count {
            let pooled = type_pooled_cursor + offset;
            let source = facts.type_child_flat(facts.type_children_base(ordinal) + offset);
            type_children[pooled] = SemanticTypeChild {
                target: if source.0 == STAGED_TEXT_CHILD {
                    TypeChildTarget::Text
                } else {
                    TypeChildTarget::Type(compiler_ir::TypeRef::Local(
                        compiler_ir::TypeId::new(remap(source.0)),
                    ))
                },
                name: source.1,
                flags: source.2,
            };
        }
        type_pooled_cursor += child_count;
        type_facts[anonymous_rows + ordinal] = TypeFactInput {
            owner: compiler_ir::EntityId::new(ordinal as u32),
            record,
        };
    }
    let computed_rows = facts.computed_rows;
    let mut computed_facts = vec![
        TypeFactInput {
            owner: compiler_ir::EntityId::new(0),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Unknown),
        };
        facts.computed_rows
    ]
    .into_boxed_slice();
    for (index, source_record) in facts.computed_records[..computed_rows].iter().enumerate() {
        let child_count = usize::from(facts.computed_child_counts[index]);
        let record = SemanticTypeRecord {
            children: ListSpan::new(type_pooled_cursor as u32, child_count as u32),
            ..*source_record
        };
        let base = facts.computed_child_starts[index] as usize;
        for offset in 0..child_count {
            let pooled = type_pooled_cursor + offset;
            let raw = facts.computed_child_targets[base + offset];
            type_children[pooled] = SemanticTypeChild {
                target: if raw == STAGED_TEXT_CHILD {
                    TypeChildTarget::Text
                } else {
                    TypeChildTarget::Type(compiler_ir::TypeRef::Local(
                        compiler_ir::TypeId::new(remap(raw)),
                    ))
                },
                name: facts.computed_child_names[base + offset],
                flags: facts.computed_child_flags[base + offset],
            };
        }
        type_pooled_cursor += child_count;
        computed_facts[index] = TypeFactInput {
            owner: compiler_ir::EntityId::new(facts.computed_owners[index]),
            record,
        };
    }
    let type_fact_lane = TypeFactLane {
        inputs: &type_facts[..declared_count],
        computed: &computed_facts[..computed_rows],
        children: &type_children[..type_pooled_cursor],
    };

    // Compact type facts place anonymous rows first, declared rows second,
    // and observed-computed rows last.  Every extension type coordinate is
    // rewritten through this one mapping before it reaches the durable
    // schema; raw staging coordinates never leak across this boundary.
    let remap_staged_type = remap;

    // Schema-4+ type-parameter list table: generic extensions name dense
    // `(start, length)` rows rather than staging element starts.  We retain a
    // distinct row even for every explicit empty declaration, so an empty
    // generic before the first nonempty declaration cannot alias it.
    let mut durable_type_parameter_ranges = vec![
        ExtensionTypeParameterRange {
            start: 0,
            length: 0,
        };
        fact_count
    ]
    .into_boxed_slice();
    let mut durable_type_parameter_ids =
        vec![None; fact_count].into_boxed_slice();
    let mut durable_type_parameter_range_len = 0_usize;
    for (ordinal, extension) in facts.extensions[..fact_count].iter().enumerate() {
        let Some(start) = extension.as_ref().and_then(extension_type_parameter_start) else {
            continue;
        };
        let Some(range) = facts.type_parameter_ranges[ordinal] else {
            return Err(AdmissionFault::ExtensionTypeParameters {
                row: ordinal,
                start: start.raw,
                length: 0,
                element_count: facts.type_parameter_len,
            });
        };
        let Some(end) = range.start.checked_add(range.length) else {
            return Err(AdmissionFault::ExtensionTypeParameters {
                row: ordinal,
                start: range.start,
                length: range.length,
                element_count: facts.type_parameter_len,
            });
        };
        if range.start != start.raw
            || usize::try_from(end).map_or(true, |end| end > facts.type_parameter_len)
        {
            return Err(AdmissionFault::ExtensionTypeParameters {
                row: ordinal,
                start: range.start,
                length: range.length,
                element_count: facts.type_parameter_len,
            });
        }
        durable_type_parameter_ranges[durable_type_parameter_range_len] =
            ExtensionTypeParameterRange {
                start: range.start,
                length: range.length,
            };
        let list = u32::try_from(durable_type_parameter_range_len).map_err(|_| {
            AdmissionFault::ExtensionTypeParameters {
                row: ordinal,
                start: range.start,
                length: range.length,
                element_count: facts.type_parameter_len,
            }
        })?;
        durable_type_parameter_ids[ordinal] = Some(compiler_ir::TypeParameterListId::new(list));
        durable_type_parameter_range_len += 1;
    }

    // Language-extension section: dense per-plane fact pools plus their row
    // tables, with provisional atom coordinates rewritten to final lane
    // positions. Pooled lists keep provisional atom coordinates until the
    // pools lane rewrites them below.
    let mut typescript_pool = vec![empty_typescript_facts(); fact_count].into_boxed_slice();
    let mut csharp_pool = vec![empty_csharp_facts(); fact_count].into_boxed_slice();
    let mut go_pool = vec![empty_go_facts(); fact_count].into_boxed_slice();
    let mut rust_pool = vec![empty_rust_facts(); fact_count].into_boxed_slice();
    let mut python_pool = vec![empty_python_facts(); fact_count].into_boxed_slice();
    let mut java_pool = vec![empty_java_facts(); fact_count].into_boxed_slice();
    let mut clang_pool = vec![empty_clang_facts(); fact_count].into_boxed_slice();
    let mut typescript_rows = vec![SECTION_NONE; fact_count].into_boxed_slice();
    let mut csharp_rows = vec![SECTION_NONE; fact_count].into_boxed_slice();
    let mut go_rows = vec![SECTION_NONE; fact_count].into_boxed_slice();
    let mut rust_rows = vec![SECTION_NONE; fact_count].into_boxed_slice();
    let mut python_rows = vec![SECTION_NONE; fact_count].into_boxed_slice();
    let mut java_rows = vec![SECTION_NONE; fact_count].into_boxed_slice();
    let mut clang_rows = vec![SECTION_NONE; fact_count].into_boxed_slice();
    let mut typescript_len = 0_usize;
    let mut csharp_len = 0_usize;
    let mut go_len = 0_usize;
    let mut rust_len = 0_usize;
    let mut python_len = 0_usize;
    let mut java_len = 0_usize;
    let mut clang_len = 0_usize;
    let any_extension = facts.extensions[..fact_count].iter().any(Option::is_some);
    for (ordinal, extension) in facts.extensions[..fact_count].iter().enumerate() {
        match extension {
            Some(EmissionExtension::TypeScript(value)) => {
                let mut rewritten = *value;
                rewritten.type_parameters = durable_type_parameter_ids[ordinal].ok_or(
                    AdmissionFault::ExtensionTypeParameters {
                        row: ordinal,
                        start: value.type_parameters.raw,
                        length: 0,
                        element_count: facts.type_parameter_len,
                    },
                )?;
                rewritten.declared = rewritten
                    .declared
                    .map(|id| TypeId::new(remap_staged_type(id.raw)));
                rewritten.observed = rewritten
                    .observed
                    .map(|id| TypeId::new(remap_staged_type(id.raw)));
                typescript_pool[typescript_len] = rewritten;
                typescript_rows[ordinal] = typescript_len as u32;
                typescript_len += 1;
            }
            Some(EmissionExtension::CSharp(value)) => {
                let mut rewritten = *value;
                rewritten.constraints = durable_type_parameter_ids[ordinal].ok_or(
                    AdmissionFault::ExtensionTypeParameters {
                        row: ordinal,
                        start: value.constraints.raw,
                        length: 0,
                        element_count: facts.type_parameter_len,
                    },
                )?;
                if let Some(span) = rewritten.xml_provenance {
                    let provisional = span.file().raw;
                    if provisional as usize >= extension_atom_count {
                        return Err(AdmissionFault::ExtensionAtom {
                            row: ordinal,
                            provisional,
                            atom_count: extension_atom_count,
                        });
                    }
                    let file = AtomId::new((fact_count + provisional as usize) as u32);
                    rewritten.xml_provenance =
                        compiler_ir::SourceSpan::new(file, span.start(), span.end());
                }
                csharp_pool[csharp_len] = rewritten;
                csharp_rows[ordinal] = csharp_len as u32;
                csharp_len += 1;
            }
            Some(EmissionExtension::Go(value)) => {
                let mut rewritten = *value;
                rewritten.type_parameters = durable_type_parameter_ids[ordinal].ok_or(
                    AdmissionFault::ExtensionTypeParameters {
                        row: ordinal,
                        start: value.type_parameters.raw,
                        length: 0,
                        element_count: facts.type_parameter_len,
                    },
                )?;
                go_pool[go_len] = rewritten;
                go_rows[ordinal] = go_len as u32;
                go_len += 1;
            }
            Some(EmissionExtension::Rust(value)) => {
                let mut rewritten = *value;
                rewritten.where_clauses = durable_type_parameter_ids[ordinal].ok_or(
                    AdmissionFault::ExtensionTypeParameters {
                        row: ordinal,
                        start: value.where_clauses.raw,
                        length: 0,
                        element_count: facts.type_parameter_len,
                    },
                )?;
                rust_pool[rust_len] = rewritten;
                rust_rows[ordinal] = rust_len as u32;
                rust_len += 1;
            }
            Some(EmissionExtension::Python(value)) => {
                python_pool[python_len] = *value;
                python_rows[ordinal] = python_len as u32;
                python_len += 1;
            }
            Some(EmissionExtension::Java(value)) => {
                java_pool[java_len] = *value;
                java_rows[ordinal] = java_len as u32;
                java_len += 1;
            }
            Some(EmissionExtension::Clang(value)) => {
                let mut rewritten = *value;
                rewritten.templates = durable_type_parameter_ids[ordinal].ok_or(
                    AdmissionFault::ExtensionTypeParameters {
                        row: ordinal,
                        start: value.templates.raw,
                        length: 0,
                        element_count: facts.type_parameter_len,
                    },
                )?;
                clang_pool[clang_len] = rewritten;
                clang_rows[ordinal] = clang_len as u32;
                clang_len += 1;
            }
            None => {}
        }
    }

    // Occurrence lane: every admitted reference fact, owner-relative, in
    // admission order.
    let mut occurrence_inputs = vec![
        OccurrenceInput {
            owner: compiler_ir::EntityId::new(0),
            occurrence: Occurrence {
                target: compiler_ir::OccurrenceTarget::Local(compiler_ir::EntityId::new(0)),
                kind: compiler_ir::ReferenceKind::FunctionCall,
                confidence: compiler_ir::OccurrenceConfidence::Syntactic,
                span: compiler_ir::RelSpan { start: 0, end: 0 },
            },
        };
        facts.occurrence_len
    ]
    .into_boxed_slice();
    for (index, owner) in facts.occurrence_owners[..facts.occurrence_len]
        .iter()
        .enumerate()
    {
        occurrence_inputs[index] = OccurrenceInput {
            owner: compiler_ir::EntityId::new(*owner),
            occurrence: facts.occurrences[index],
        };
    }
    let occurrence_lane = OccurrenceLane {
        inputs: &occurrence_inputs[..facts.occurrence_len],
    };

    // Documentation lane: every admitted doc fragment in admission order.
    let documentation_lane = DocumentationLane {
        inputs: &facts.doc_facts[..facts.doc_len],
    };

    // Extension pooled lanes: provisional atom coordinates become final atom
    // lane positions; type and entity coordinates were already final.
    let mut atom_list_elements = vec![[0; MAX_REF_LIST_ELEMENTS]; facts.atom_list_len].into_boxed_slice();
    for (index, length) in facts.atom_list_lengths[..facts.atom_list_len]
        .iter()
        .enumerate()
    {
        for (offset, provisional) in facts.atom_lists[index][..usize::from(*length)]
            .iter()
            .enumerate()
        {
            if *provisional as usize >= extension_atom_count {
                return Err(AdmissionFault::ExtensionAtom {
                    row: index,
                    provisional: *provisional,
                    atom_count: extension_atom_count,
                });
            }
            atom_list_elements[index][offset] = (fact_count + *provisional as usize) as u32;
        }
    }
    let mut pooled_atom_lists = vec![ExtensionRefList { elements: &[] }; facts.atom_list_len];
    for (index, length) in facts.atom_list_lengths[..facts.atom_list_len]
        .iter()
        .enumerate()
    {
        pooled_atom_lists[index] = ExtensionRefList {
            elements: &atom_list_elements[index][..usize::from(*length)],
        };
    }
    let mut type_list_elements = vec![[0; MAX_REF_LIST_ELEMENTS]; facts.type_list_len].into_boxed_slice();
    let mut pooled_type_lists = vec![ExtensionRefList { elements: &[] }; facts.type_list_len];
    for (index, length) in facts.type_list_lengths[..facts.type_list_len]
        .iter()
        .enumerate()
    {
        for (offset, raw) in facts.type_lists[index][..usize::from(*length)]
            .iter()
            .enumerate()
        {
            type_list_elements[index][offset] = remap_staged_type(*raw);
        }
        pooled_type_lists[index] = ExtensionRefList {
            elements: &type_list_elements[index][..usize::from(*length)],
        };
    }
    let mut pooled_entity_lists = vec![ExtensionRefList { elements: &[] }; facts.entity_list_len];
    for (index, length) in facts.entity_list_lengths[..facts.entity_list_len]
        .iter()
        .enumerate()
    {
        pooled_entity_lists[index] = ExtensionRefList {
            elements: &facts.entity_lists[index][..usize::from(*length)],
        };
    }
    let extension_pools = ExtensionPoolsLane {
        type_parameters: &type_parameters[..facts.type_parameter_len],
        type_parameter_bounds: &type_parameter_bounds[..facts.type_parameter_bound_len],
        type_parameter_lists:
            &durable_type_parameter_ranges[..durable_type_parameter_range_len],
        atom_lists: &pooled_atom_lists[..facts.atom_list_len],
        type_lists: &pooled_type_lists[..facts.type_list_len],
        entity_lists: &pooled_entity_lists[..facts.entity_list_len],
    };

    let extension_section = (any_extension).then(|| ExtensionSectionInput {
        authority: compiler_ir::SemanticImageAuthority::Language(profile),
        typescript: ExtensionSectionPlane {
            facts: &typescript_pool[..typescript_len],
            row_ordinals: &typescript_rows[..fact_count],
        },
        csharp: ExtensionSectionPlane {
            facts: &csharp_pool[..csharp_len],
            row_ordinals: &csharp_rows[..fact_count],
        },
        go: ExtensionSectionPlane {
            facts: &go_pool[..go_len],
            row_ordinals: &go_rows[..fact_count],
        },
        rust: ExtensionSectionPlane {
            facts: &rust_pool[..rust_len],
            row_ordinals: &rust_rows[..fact_count],
        },
        python: ExtensionSectionPlane {
            facts: &python_pool[..python_len],
            row_ordinals: &python_rows[..fact_count],
        },
        java: ExtensionSectionPlane {
            facts: &java_pool[..java_len],
            row_ordinals: &java_rows[..fact_count],
        },
        clang: ExtensionSectionPlane {
            facts: &clang_pool[..clang_len],
            row_ordinals: &clang_rows[..fact_count],
        },
    });

    let prepared = PreparedFragment::prepare_with_semantics(
        source,
        recipe,
        &entities[..fact_count],
        node_prefix,
        &atoms[..atom_count],
        compiler_ir::FragmentSemantics {
            data: Some(&semantic),
            occurrences: (facts.occurrence_len > 0).then_some(&occurrence_lane),
            type_facts: Some(&type_fact_lane),
            docs: (facts.doc_len > 0).then_some(&documentation_lane),
            extensions: extension_section.as_ref(),
            pools: any_extension.then_some(&extension_pools),
        },
    );
    write_prepared(prepared, output)
}

/// Exact subset representable by the compact primitive-node opcode table.
/// The richer type-fact lane carries every [`BuiltinType`] without a lossy
/// coercion; callers that need widths inspect that lane instead of this index.
fn compact_primitive_node(
    record: SemanticTypeRecord<'_>,
) -> Option<(usize, TypeNode)> {
    match builtin_type(record)? {
        BuiltinType::Bool => Some((0, TypeNode::Primitive(compiler_ir::PrimitiveType::Bool))),
        BuiltinType::I32 => Some((1, TypeNode::Primitive(compiler_ir::PrimitiveType::I32))),
        BuiltinType::String => Some((2, TypeNode::Primitive(compiler_ir::PrimitiveType::String))),
        _ => None,
    }
}

fn write_prepared<'output>(
    prepared: Result<PreparedFragment<'_>, PrepareError>,
    output: &'output mut [u8],
) -> Result<&'output [u8], AdmissionFault> {
    let prepared = prepared.map_err(AdmissionFault::Prepare)?;
    prepared.write_into(output).map_err(AdmissionFault::Write)
}

#[cfg(test)]
mod tests;

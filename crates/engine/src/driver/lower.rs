//! Defines lower behavior for the `backend-engine` driver, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//!
//! Occurrence admission is deliberately deferred to the integration phase and
//! remains owned by compiler/ir/semantic_facts.rs. Direct authorities emit
//! declaration facts into this one canonical lane; unsupported authorities
//! return typed terminals instead of inspecting source text here.
use backend_semantic::ir::DocumentationLane;
use backend_semantic::ir::{
    AnnotationKind, AtomId, BuiltinType, ChannelDirection, ComputedType, ConcreteType,
    CorePayloadHash, CvQualifiers, CxxReferenceCategory, DocInput, EntityAuthorityFacts,
    ExternalDeclarationIdentity, ExternalTarget, FactAvailability, ForeignDeclarationId,
    ForeignExternalTarget, ForeignTargetOrigin, Ir, IrBuilder, ItemKind, LanguageExtensionInput,
    ListSpan, LiteralType, NativeCharacterRole, NominalRef, ObjectMember, OccurrenceAuthorityFacts,
    ParentageAuthority, PrimitiveShape, ProductChildRole, ProductChildren, ProductId,
    ProductListId, ProductRef, PropertyKey, QualifiedSegments, SemanticAtom, SemanticProduct,
    SemanticProductChild, SemanticProductConstructor, SemanticTypeChild, SemanticTypeFault,
    SemanticTypeRecord, SemanticTypeTag, TemplatePart, TreeItemInput, TreeLinkInput,
    TreeLinkTarget, TupleElement, TupleElementKind, TypeChildTarget, TypeId, TypeWidth,
    UnrepresentedAuthorityOwner, VariantAvailability, VariantFingerprint, Visibility,
    WildcardBound,
};
use backend_semantic::ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataResourceBudget, DataScratch,
    DocFactInput, DocFragmentInput, DocLinkTarget, EntityKind, EntityRecord, ExtensionPoolsLane,
    ExtensionRefList, ExtensionSectionInput, ExtensionSectionPlane, ExtensionTypeParameter,
    ExtensionTypeParameterRange, Occurrence, OccurrenceInput, OccurrenceLane, PrepareError,
    PreparedFragment, RecipeFact, SourceIdentity, TypeFactInput, TypeFactLane, TypeNode,
    WriteError, canonicalize_data_with_budget,
};
use backend_semantic::vocabulary::ProjectionFactLane;
use core::mem::size_of;
use core::num::NonZeroU16;

mod admission;
mod build_ir;
pub(crate) mod clang;
pub(crate) mod csharp;
pub(crate) mod go;
mod identity;
pub(crate) mod java;
mod provenance;
pub(crate) mod python;
pub(crate) mod rust;
pub(crate) mod typescript;

pub(crate) use admission::{portable_admission, portable_count};
pub(super) use provenance::StagedSourceSpan;
use provenance::{MemberSetCapture, Provenance, local_parent};

/// Dense bound of the multi-declaration semantic emission lane.
///
/// One LowerIr fragment admits at most this many provable declaration facts;
/// a source with more declarations is a typed lane rejection, never a
/// truncated emission. The bound also fixes every canonicalization scratch,
/// output, and resource reservation below.
/// 32,768 slots × 4-byte `u32` coordinate = 128 KiB; measured high-water 6,882 facts, and the clang authority mirror measures 32,768 declaration-lane rows for fmt's deferred-parameter traversal, so the coupled protocol geometry moves with it. Roll back to 16,384 if every target package stays below 8,192 facts.
pub(super) const MAX_EMISSION_FACTS: usize = 32768;
/// Dense bound of one fact's ordered product children.
///
/// A signature or product may name this many children. The count is a `u8`,
/// so 255 is the hard per-row maximum. The pooled lane stays at
/// [`PRODUCT_CHILD_POOL_STRIDE`] on average so `protocol_maximum` does not
/// reserve a 255-wide slot for every fact.
pub(super) const MAX_FACT_CHILDREN: usize = 255;
/// Average product-child slots reserved per fact in the pooled lane.
const PRODUCT_CHILD_POOL_STRIDE: usize = 64;
const _: () = assert!(MAX_FACT_CHILDREN <= u8::MAX as usize);
/// Dense bound of one fact's ordered type-record children.
///
/// A compound row may name this many children. The count is a `u8`, so 255
/// is the hard per-row maximum. The pooled lanes stay at
/// [`TYPE_CHILD_POOL_STRIDE`] on average: raising this constant must not
/// multiply `protocol_maximum` by 255.
pub(super) const MAX_TYPE_CHILDREN: usize = 255;
/// Average child slots reserved per type row in the pooled lanes.
///
/// One row may still use [`MAX_TYPE_CHILDREN`]. The pool product stays at
/// this stride so a maximal fact set does not allocate a 255-wide slot for
/// every declaration.
const TYPE_CHILD_POOL_STRIDE: usize = 64;
/// Total pooled product-child ceiling across one request. Per-row legality is
/// still governed by [`MAX_FACT_CHILDREN`]; production allocation uses the
/// request's measured aggregate demand rather than this Cartesian maximum.
const MAX_EMISSION_CHILDREN: usize = MAX_EMISSION_FACTS * PRODUCT_CHILD_POOL_STRIDE;
/// Total pooled declared-type-child ceiling across one request.
const MAX_EMISSION_TYPE_CHILDREN: usize = MAX_EMISSION_FACTS * TYPE_CHILD_POOL_STRIDE;
/// The uncommitted portion of either fixed type-child lane can never exceed
/// one record's bounded child capacity, so its cursor has a total compact
/// representation independent of the platform's native word width.
const MAX_PENDING_TYPE_CHILDREN: u8 = MAX_TYPE_CHILDREN as u8;
const _: () = assert!(MAX_TYPE_CHILDREN <= u8::MAX as usize);
/// Dense bound of the occurrence lane committed beside the declarations.
///
/// This is the protocol ceiling for the *pooled* occurrence lane, which
/// [`ResourcePlan::for_source`] budgets from entered bytes; it exists so a
/// genuine measured overrun is a typed `OccurrenceCapacity` fault rather than
/// an eager maximum allocation. The Clang authority records four references
/// per declaration (`MAX_CLANG_REFERENCES`), so a translation unit past the
/// declaration ceiling was still `OccurrenceCapacity` while its source bytes
/// had room. The ceiling matches that four-times ratio.
pub(super) const MAX_EMISSION_OCCURRENCES: usize = 4 * MAX_EMISSION_FACTS;
/// Dense bound of the documentation lane; measured maximum is 13,529 fragments (`StringUtils.java`), so 16,384 is next.
pub(super) const MAX_EMISSION_DOC_FRAGMENTS: usize = 16384;
/// Dense bound of extension atoms admitted beside declaration names.
pub(super) const MAX_EXTENSION_ATOMS: usize = 2048;
/// Dense bound of pooled type parameters.
///
/// [`ResourcePlan::for_source`] derives the actual reservation from entered
/// bytes; this protocol ceiling only bounds a genuine measured overrun. Raised
/// from 512 because real generic-heavy multi-package fragments exceed 512
/// pooled parameters.
pub(super) const MAX_TYPE_PARAMETERS: usize = 4096;
/// Dense bound of ordered type/lifetime bounds across one request.
/// Each bound has written source evidence, so the request geometry scales
/// with entered bytes rather than allocating a language-wide maximum.
pub(super) const MAX_TYPE_PARAMETER_BOUNDS: usize = MAX_TYPE_PARAMETERS * 255;
/// Dense bound of pooled Rust free generic predicates across one request.
/// Each predicate carries written source evidence, so real usage scales with
/// entered bytes like type parameters do.
pub(super) const MAX_FREE_PREDICATES: usize = 4096;
/// Dense bound of pooled reference lists per lane kind.
///
/// The request reservation is `min(facts, MAX_REF_LISTS)`, so this ceiling
/// only bites for a source whose measured list count reaches it. Raised from
/// 512 to keep real multi-package fragment import/override lists from becoming
/// a hard `RefListCapacity` wall.
pub(super) const MAX_REF_LISTS: usize = 4096;
/// Dense bound of one pooled reference list.
///
/// The measured Go method-set maximum was 220, which fit in a `u8` length.
/// C translation units such as Lua and json-c emit a wider include or
/// reference list at the first fact, so the ceiling is the element count a
/// flat pool can address, not a fixed row width. A list beyond this is still
/// a typed `RefListElements` rejection, never a truncated emission.
pub(super) const MAX_REF_LIST_ELEMENTS: usize = 4096;
/// Total atom budget: one name per fact plus every extension atom.
pub(super) const MAX_EMISSION_ATOMS: usize = MAX_EMISSION_FACTS + MAX_EXTENSION_ATOMS;
/// Dense bound of anonymous type rows interned beside the fact rows.
/// 8,192 slots × 4-byte `u32` owner = 32 KiB; measured target high-water 0 rows. Roll back to 2,048 if it stays below 1,024.
pub(super) const MAX_ANONYMOUS_TYPE_ROWS: usize = 8192;
/// Dense bound of checker-computed type rows in the schema-2 segment.
/// 32,768 slots × 4-byte `u32` owner = 128 KiB; measured target high-water 23,037 rows. Roll back to 16,384 if every target stays below 8,192.
pub(super) const MAX_COMPUTED_TYPE_ROWS: usize = 32768;
/// Aggregate anonymous/computed child ceilings. These remain protocol limits,
/// not eager allocation instructions.
const MAX_ANONYMOUS_TYPE_CHILDREN: usize = MAX_ANONYMOUS_TYPE_ROWS * TYPE_CHILD_POOL_STRIDE;
const MAX_COMPUTED_TYPE_CHILDREN: usize = MAX_COMPUTED_TYPE_ROWS * TYPE_CHILD_POOL_STRIDE;
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
    product_children: usize,
    declared_type_children: usize,
    occurrences: usize,
    docs: usize,
    extension_atoms: usize,
    type_parameters: usize,
    type_parameter_bounds: usize,
    free_predicates: usize,
    ref_lists: usize,
    anonymous_rows: usize,
    anonymous_type_children: usize,
    computed_rows: usize,
    computed_type_children: usize,
}

impl ResourcePlan {
    /// Complete geometry used only by explicit capacity falsifiers.
    pub(crate) const fn protocol_maximum() -> Self {
        Self {
            facts: MAX_EMISSION_FACTS,
            product_children: MAX_EMISSION_CHILDREN,
            declared_type_children: MAX_EMISSION_TYPE_CHILDREN,
            occurrences: MAX_EMISSION_OCCURRENCES,
            docs: MAX_EMISSION_DOC_FRAGMENTS,
            extension_atoms: MAX_EXTENSION_ATOMS,
            type_parameters: MAX_TYPE_PARAMETERS,
            type_parameter_bounds: MAX_TYPE_PARAMETER_BOUNDS,
            free_predicates: MAX_FREE_PREDICATES,
            ref_lists: MAX_REF_LISTS,
            anonymous_rows: MAX_ANONYMOUS_TYPE_ROWS,
            anonymous_type_children: MAX_ANONYMOUS_TYPE_CHILDREN,
            computed_rows: MAX_COMPUTED_TYPE_ROWS,
            computed_type_children: MAX_COMPUTED_TYPE_CHILDREN,
        }
    }

    /// Conservative measured demand derived from entered bytes and the one
    /// selected language. Excess authority output is an exact capacity fault,
    /// never an eager max-of-seven-languages allocation.
    pub(crate) fn for_source(
        profile: backend_semantic::vocabulary::LanguageProfile,
        source_bytes: usize,
    ) -> Self {
        let multiplier = match profile {
            backend_semantic::vocabulary::LanguageProfile::TypeScript(_)
            | backend_semantic::vocabulary::LanguageProfile::Python(_) => 4,
            backend_semantic::vocabulary::LanguageProfile::C(_)
            | backend_semantic::vocabulary::LanguageProfile::Cxx(_) => 3,
            backend_semantic::vocabulary::LanguageProfile::Rust(_)
            | backend_semantic::vocabulary::LanguageProfile::Go(_)
            | backend_semantic::vocabulary::LanguageProfile::Java(_)
            | backend_semantic::vocabulary::LanguageProfile::CSharp(_) => 2,
        };
        let units = source_bytes.saturating_add(1);
        let bounded = |value: usize, maximum: usize| value.clamp(8, maximum);
        let bounded_children =
            |value: usize, maximum: usize| value.clamp(TYPE_CHILD_POOL_STRIDE, maximum);
        let facts = bounded(units / 2 + 8, MAX_EMISSION_FACTS);
        Self {
            facts,
            product_children: bounded_children(units.saturating_mul(4), MAX_EMISSION_CHILDREN),
            declared_type_children: bounded_children(
                units.saturating_mul(8),
                MAX_EMISSION_TYPE_CHILDREN,
            ),
            // A reference occurrence owns at least one source byte.  This
            // preserves the measured C# 1,253-occurrence demand without
            // reserving eight thousand rows for a ten-byte source.
            occurrences: bounded(units, MAX_EMISSION_OCCURRENCES),
            docs: bounded(units.saturating_add(8), MAX_EMISSION_DOC_FRAGMENTS),
            extension_atoms: bounded(units / 2 + 8, MAX_EXTENSION_ATOMS),
            type_parameters: bounded(units / 2 + 8, MAX_TYPE_PARAMETERS),
            type_parameter_bounds: bounded(units, MAX_TYPE_PARAMETER_BOUNDS),
            free_predicates: bounded(units / 2 + 8, MAX_FREE_PREDICATES),
            ref_lists: bounded(facts, MAX_REF_LISTS),
            anonymous_rows: bounded(
                units.saturating_mul(multiplier).saturating_add(8),
                MAX_ANONYMOUS_TYPE_ROWS,
            ),
            anonymous_type_children: bounded_children(
                units.saturating_mul(8),
                MAX_ANONYMOUS_TYPE_CHILDREN,
            ),
            computed_rows: bounded(
                units
                    .saturating_mul(multiplier.saturating_mul(2))
                    .saturating_add(8),
                MAX_COMPUTED_TYPE_ROWS,
            ),
            computed_type_children: bounded_children(
                units.saturating_mul(16),
                MAX_COMPUTED_TYPE_CHILDREN,
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
    /// Whether this declaration is a static class member. A static and an
    /// instance member can share kind, name, owner, and structure yet remain
    /// two distinct source declarations, so this bit is an identity
    /// discriminator, never a payload cell.
    static_member: bool,
    /// An authority-proven 16-byte distinction for two declarations whose
    /// structural identity is otherwise byte-identical — for example two C++
    /// class template specializations whose non-type arguments are erased from
    /// the type graph. It enters declaration identity only when present, so
    /// every other declaration's frames remain byte-identical.
    identity_discriminator: Option<[u8; 16]>,
}

/// One per-language extension fact committed beside a declaration.
///
/// The pooled list and atom coordinates are provisional until admission
/// fixes the final fragment lane layout; `admit` rewrites exactly the
/// provisional atom coordinates before encoding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EmissionExtension {
    /// TypeScript facts.
    TypeScript(backend_semantic::ir::TypeScriptFacts),
    /// C# facts.
    CSharp(backend_semantic::ir::CSharpFacts),
    /// Go facts.
    Go(backend_semantic::ir::GoFacts),
    /// Rust facts.
    Rust(backend_semantic::ir::RustFacts),
    /// Python facts.
    Python(backend_semantic::ir::PythonFacts),
    /// Java facts.
    Java(backend_semantic::ir::JavaFacts),
    /// Clang facts.
    Clang(backend_semantic::ir::ClangFacts),
}

/// Transaction-local coordinate into exactly one rewritten language pool.
///
/// The closed tag mirrors the ingress sum type while the finalized `Ir`
/// still owns seven named sparse planes.  This avoids allocating seven
/// fact-count-wide temporary arrays for a source that can have only one
/// language authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RewrittenExtension {
    TypeScript(usize),
    CSharp(usize),
    Go(usize),
    Rust(usize),
    Python(usize),
    Java(usize),
    Clang(usize),
}

/// Exact temporary-pool demand measured from the already-admitted facts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ExtensionDemand {
    typescript: usize,
    csharp: usize,
    go: usize,
    rust: usize,
    python: usize,
    java: usize,
    clang: usize,
}

impl ExtensionDemand {
    fn measure(rows: &[Option<EmissionExtension>]) -> Self {
        let mut demand = Self::default();
        for extension in rows.iter().flatten() {
            match extension {
                EmissionExtension::TypeScript(_) => demand.typescript += 1,
                EmissionExtension::CSharp(_) => demand.csharp += 1,
                EmissionExtension::Go(_) => demand.go += 1,
                EmissionExtension::Rust(_) => demand.rust += 1,
                EmissionExtension::Python(_) => demand.python += 1,
                EmissionExtension::Java(_) => demand.java += 1,
                EmissionExtension::Clang(_) => demand.clang += 1,
            }
        }
        demand
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "FactSet fact geometry is statically bounded below u32::MAX"
)]
fn rewritten_extension_fact<Fact>(
    facts: &[Fact],
    fact: usize,
    language: backend_semantic::ir::Language,
    entity: usize,
) -> Result<&Fact, backend_semantic::ir::BuildError> {
    facts
        .get(fact)
        .ok_or(backend_semantic::ir::BuildError::LanguageExtension {
            language,
            entity: backend_semantic::ir::EntityId::new(entity as u32),
            violation: backend_semantic::ir::LanguageExtensionViolation::MissingPoolFact {
                fact,
                count: facts.len(),
            },
        })
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "every extension pool is bounded by FactSet's u32-sized fact geometry"
)]
fn next_extension_ordinal<Fact>(facts: &[Fact]) -> u32 {
    facts.len() as u32
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
            static_member: false,
            identity_discriminator: None,
        }
    }

    /// Marks this declaration as a static class member so its identity cannot
    /// collide with an identically named instance member of the same owner.
    #[must_use]
    pub(super) const fn static_member(mut self) -> Self {
        self.static_member = true;
        self
    }

    /// Commits an authority-proven identity discriminator for this declaration.
    /// It is framed into the declaration variant exactly when the source
    /// authority proves two declarations cannot be separated structurally.
    #[must_use]
    pub(super) const fn with_identity_discriminator(mut self, discriminator: [u8; 16]) -> Self {
        self.identity_discriminator = Some(discriminator);
        self
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

use crate::driver::types::{FactFault, FactRejection, ParentageState, TypeChildLane};

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
/// Flat pool of reference-list rows. Each row is a span into `elements`,
/// so a list wider than 255 does not reserve a fixed-width scratch row.
struct PooledRefLists {
    elements: Vec<u32>,
    starts: Box<[u32]>,
    lengths: Box<[u32]>,
    len: usize,
}

impl PooledRefLists {
    fn reserve(lists: usize) -> Self {
        Self {
            elements: Vec::new(),
            starts: vec![0; lists].into_boxed_slice(),
            lengths: vec![0; lists].into_boxed_slice(),
            len: 0,
        }
    }

    fn row(&self, index: usize) -> Option<&[u32]> {
        if index >= self.len {
            return None;
        }
        let start = usize::try_from(self.starts[index]).ok()?;
        let length = usize::try_from(self.lengths[index]).ok()?;
        let end = start.checked_add(length)?;
        self.elements.get(start..end)
    }

    fn push(&mut self, elements: &[u32]) -> Result<(), ()> {
        let start = u32::try_from(self.elements.len()).map_err(|_| ())?;
        let length = u32::try_from(elements.len()).map_err(|_| ())?;
        if self.len >= self.starts.len() {
            return Err(());
        }
        self.elements.extend_from_slice(elements);
        self.starts[self.len] = start;
        self.lengths[self.len] = length;
        self.len += 1;
        Ok(())
    }
}

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
    key_digests: Box<[CorePayloadHash]>,
    visibility: Box<[Visibility]>,
    visibility_captured: Box<[bool]>,
    /// Static-class-member discriminator carried into declaration identity.
    static_members: Box<[bool]>,
    /// Authority-proven identity discriminators carried into the declaration
    /// variant exactly when the declaring frontend supplies one.
    identity_discriminators: Box<[Option<[u8; 16]>]>,
    documentation_captured: Box<[bool]>,
    provenance: Provenance,
    occurrence_owners: Box<[u32]>,
    occurrences: Box<[Occurrence<'source>]>,
    occurrence_len: usize,
    doc_facts: Box<[DocFactInput<'source>]>,
    doc_len: usize,
    extension_atoms: Box<[&'source [u8]]>,
    extension_atom_len: usize,
    type_parameters: Box<[ExtensionTypeParameter<'source>]>,
    type_parameter_len: usize,
    type_parameter_bounds: Box<[backend_semantic::ir::ExtensionTypeParameterBound<'source>]>,
    type_parameter_bound_len: usize,
    type_parameter_ranges: Box<[Option<StagedTypeParameterRange>]>,
    impl_traits: Box<[Option<StagedImplTrait<'source>>]>,
    free_predicates: Box<[backend_semantic::ir::ExtensionFreePredicate]>,
    free_predicate_len: usize,
    free_predicate_ranges: Box<[Option<StagedFreePredicateRange>]>,
    atom_lists: PooledRefLists,
    atom_list_len: usize,
    type_lists: PooledRefLists,
    type_list_len: usize,
    entity_lists: PooledRefLists,
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
    anonymous_child_pending: u8,
    computed_records: Box<[SemanticTypeRecord<'source>]>,
    computed_owners: Box<[u32]>,
    computed_child_starts: Box<[u32]>,
    computed_child_counts: Box<[u8]>,
    computed_child_targets: Box<[u32]>,
    computed_child_names: Box<[Option<&'source [u8]>]>,
    computed_child_flags: Box<[u8]>,
    computed_rows: usize,
    computed_children_total: usize,
    computed_child_pending: u8,
}

/// An explicit transaction-local type-parameter range.  The public semantic
/// ID is dense only after admission; a bare staging start is ambiguous when
/// an empty declaration precedes the first generic declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StagedTypeParameterRange {
    start: u32,
    length: u32,
}

/// One Rust implementation's trait reference for identity framing. The
/// coordinate never enters a hash directly: a local trait frames through its
/// already-minted coordinate-free locator plus its exact written spelling
/// (which carries the trait's generic arguments, the only content separating
/// several legal impls of one local trait for one self type), a foreign trait
/// through its exact written spelling, and an inherent implementation through
/// its distinct tag.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum StagedImplTrait<'source> {
    /// An inherent implementation with no trait reference.
    Inherent,
    /// A lane-local trait by fact ordinal and exact written spelling.
    Local {
        target: u32,
        spelling: &'source [u8],
    },
    /// A trait outside this fragment by exact written spelling.
    Foreign(&'source [u8]),
}

/// An explicit transaction-local free-predicate range. Like its type-parameter
/// counterpart, the public list ID stays dense only after admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StagedFreePredicateRange {
    start: u32,
    length: u32,
}

/// One fixed child lane whose current trailing run has not yet been attached
/// to a committed type row.  The backing slot arrays stay reusable after an
/// abort; only their observable prefix/cursors are rolled back.
#[derive(Clone, Copy)]
enum PendingTypeLane {
    Anonymous,
    Computed,
}

/// The three pooled reference-list lanes exposed by `FactSet`.  Keeping this
/// staging selector closed prevents a caller from manufacturing a fact-lane
/// spelling that the admission snapshot cannot describe exactly.
#[derive(Clone, Copy)]
enum ReferenceListLane {
    Atoms,
    Types,
    Entities,
}

// The boxed lanes keep this caller-owned collector below the 64 KiB stack
// budget; each box is allocated exactly once by FactSet::new and lives with
// its FactSet, rather than being grown during admission.
const _: () = assert!(size_of::<FactSet<'static>>() <= 64 * 1024);

impl<'source> FactSet<'source> {
    /// Abandons one uncommitted trailing child run while leaving its fixed
    /// storage available for the next row.  A row interning failure and a
    /// child append failure share this one transition so no stale child can
    /// become the prefix of a later otherwise-valid row.
    fn abandon_pending_type_run(&mut self, lane: PendingTypeLane) {
        let (total, pending) = match lane {
            PendingTypeLane::Anonymous => (
                &mut self.anonymous_children_total,
                &mut self.anonymous_child_pending,
            ),
            PendingTypeLane::Computed => (
                &mut self.computed_children_total,
                &mut self.computed_child_pending,
            ),
        };
        let pending_count = usize::from(*pending);
        debug_assert!(pending_count <= *total);
        *total -= pending_count;
        *pending = 0;
    }

    /// Preserves the precise fault that aborted a pending row after returning
    /// its child run to the reusable fixed lane.
    fn reject_pending_type_run<T>(
        &mut self,
        lane: PendingTypeLane,
        fault: FactFault,
    ) -> Result<T, FactFault> {
        self.abandon_pending_type_run(lane);
        Err(fault)
    }

    fn staged_type_slot(&self, row: u32) -> Option<usize> {
        if row < ANONYMOUS_ROW_BASE {
            return ((row as usize) < self.len).then_some(row as usize);
        }
        if row >= COMPUTED_ROW_BASE {
            let computed = row - COMPUTED_ROW_BASE;
            return ((computed as usize) < self.computed_rows)
                .then_some(self.len + self.anonymous_rows + computed as usize);
        }
        let anonymous = row - ANONYMOUS_ROW_BASE;
        ((anonymous as usize) < self.anonymous_rows).then_some(self.len + anonymous as usize)
    }

    fn is_anonymous_type_row(&self, row: u32) -> bool {
        row >= ANONYMOUS_ROW_BASE
            && row < COMPUTED_ROW_BASE
            && ((row - ANONYMOUS_ROW_BASE) as usize) < self.anonymous_rows
    }

    fn is_computed_type_row(&self, row: u32) -> bool {
        row >= COMPUTED_ROW_BASE && ((row - COMPUTED_ROW_BASE) as usize) < self.computed_rows
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
                deepest_child =
                    deepest_child.maximum(self.projected_type_demand_from(target, state, cached));
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
            type_child_targets: vec![0; plan.declared_type_children].into_boxed_slice(),
            type_child_names: vec![None; plan.declared_type_children].into_boxed_slice(),
            type_child_flags: vec![0; plan.declared_type_children].into_boxed_slice(),
            type_child_counts: vec![0; plan.facts].into_boxed_slice(),
            type_child_starts: vec![0; plan.facts].into_boxed_slice(),
            total_type_children: 0,
            constructors: vec![SemanticProductConstructor::PRODUCT; plan.facts].into_boxed_slice(),
            child_roles: vec![ProductChildRole::ProductMember; plan.product_children]
                .into_boxed_slice(),
            child_targets: vec![0; plan.product_children].into_boxed_slice(),
            child_counts: vec![0; plan.facts].into_boxed_slice(),
            child_starts: vec![0; plan.facts].into_boxed_slice(),
            extensions: vec![None; plan.facts].into_boxed_slice(),
            key_digests: vec![CorePayloadHash::from_raw([0; 16]); plan.facts].into_boxed_slice(),
            visibility: vec![Visibility::Unknown; plan.facts].into_boxed_slice(),
            visibility_captured: vec![false; plan.facts].into_boxed_slice(),
            static_members: vec![false; plan.facts].into_boxed_slice(),
            identity_discriminators: vec![None; plan.facts].into_boxed_slice(),
            documentation_captured: vec![false; plan.facts].into_boxed_slice(),
            provenance: Provenance::new(plan.facts),
            occurrence_owners: vec![0; plan.occurrences].into_boxed_slice(),
            occurrences: vec![
                Occurrence {
                    target: backend_semantic::ir::OccurrenceTarget::Foreign(
                        backend_semantic::ir::ForeignKey {
                            origin: backend_semantic::ir::ForeignOrigin::Universe { ecosystem: "" },
                            path: "",
                            display: "",
                            kind: None,
                        }
                    ),
                    kind: backend_semantic::ir::ReferenceKind::FunctionCall,
                    confidence: backend_semantic::ir::OccurrenceConfidence::Syntactic,
                    span: backend_semantic::ir::RelSpan { start: 0, end: 0 },
                };
                plan.occurrences
            ]
            .into_boxed_slice(),
            occurrence_len: 0,
            doc_facts: vec![
                DocFactInput {
                    owner: backend_semantic::ir::EntityId::new(0),
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
                    bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
                        start: 0,
                        length: 0,
                    },
                    default: None,
                    variance: backend_semantic::ir::Variance::Invariant,
                    kind: backend_semantic::ir::ExtensionTypeParameterKind::Type {
                        inference: backend_semantic::ir::TypeParameterInference::Ordinary,
                    },
                    requirements: backend_semantic::ir::TypeParameterRequirements::none(),
                };
                plan.type_parameters
            ]
            .into_boxed_slice(),
            type_parameter_len: 0,
            type_parameter_bounds: vec![
                backend_semantic::ir::ExtensionTypeParameterBound::Type(0);
                plan.type_parameter_bounds
            ]
            .into_boxed_slice(),
            type_parameter_bound_len: 0,
            type_parameter_ranges: vec![None; plan.facts].into_boxed_slice(),
            impl_traits: vec![None; plan.facts].into_boxed_slice(),
            free_predicates: vec![
                backend_semantic::ir::ExtensionFreePredicate {
                    subject: 0,
                    bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
                        start: 0,
                        length: 0,
                    },
                };
                plan.free_predicates
            ]
            .into_boxed_slice(),
            free_predicate_len: 0,
            free_predicate_ranges: vec![None; plan.facts].into_boxed_slice(),
            atom_lists: PooledRefLists::reserve(plan.ref_lists),
            atom_list_len: 0,
            type_lists: PooledRefLists::reserve(plan.ref_lists),
            type_list_len: 0,
            entity_lists: PooledRefLists::reserve(plan.ref_lists),
            entity_list_len: 0,
            anonymous_records: vec![opaque_record(); plan.anonymous_rows].into_boxed_slice(),
            anonymous_owners: vec![0; plan.anonymous_rows].into_boxed_slice(),
            anonymous_child_starts: vec![0; plan.anonymous_rows].into_boxed_slice(),
            anonymous_child_counts: vec![0; plan.anonymous_rows].into_boxed_slice(),
            anonymous_child_targets: vec![0; plan.anonymous_type_children].into_boxed_slice(),
            anonymous_child_names: vec![None; plan.anonymous_type_children].into_boxed_slice(),
            anonymous_child_flags: vec![0; plan.anonymous_type_children].into_boxed_slice(),
            anonymous_rows: 0,
            anonymous_children_total: 0,
            anonymous_child_pending: 0,
            computed_records: vec![opaque_record(); plan.computed_rows].into_boxed_slice(),
            computed_owners: vec![0; plan.computed_rows].into_boxed_slice(),
            computed_child_starts: vec![0; plan.computed_rows].into_boxed_slice(),
            computed_child_counts: vec![0; plan.computed_rows].into_boxed_slice(),
            computed_child_targets: vec![0; plan.computed_type_children].into_boxed_slice(),
            computed_child_names: vec![None; plan.computed_type_children].into_boxed_slice(),
            computed_child_flags: vec![0; plan.computed_type_children].into_boxed_slice(),
            computed_rows: 0,
            computed_children_total: 0,
            computed_child_pending: 0,
        }
    }

    /// Number of admitted facts.
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// Projects one admitted staging row's authority truth directly into the
    /// owned IR tree input.  This is part of the same build transaction as
    /// the semantic row itself; no driver-owned capture sidecar survives it.
    fn entity_authority(
        &self,
        ordinal: usize,
        versions: &[backend_semantic::ir::EntityVersion],
    ) -> Result<EntityAuthorityFacts, backend_semantic::ir::BuildError> {
        let unavailable = FactAvailability::Unavailable;
        let captured = FactAvailability::Captured;
        let source_present = self
            .provenance
            .source_spans()
            .get(ordinal)
            .copied()
            .flatten()
            .is_some();
        let parentage = match *self.provenance.parentage().get(ordinal).ok_or(
            backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::Entity,
                raw: u32::try_from(ordinal).unwrap_or(u32::MAX),
            },
        )? {
            ParentageState::Unavailable => ParentageAuthority::Unavailable,
            ParentageState::Root => ParentageAuthority::Root,
            ParentageState::Bound { parent } => ParentageAuthority::Bound(
                versions
                    .get(parent.index())
                    .copied()
                    .ok_or(backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Entity,
                        raw: parent.raw,
                    })?
                    .identity(),
            ),
            ParentageState::UnrepresentedAuthorityOwner { identity } => {
                ParentageAuthority::UnrepresentedAuthorityOwner(UnrepresentedAuthorityOwner::new(
                    identity,
                ))
            }
        };
        let attributes = self
            .extensions
            .get(ordinal)
            .and_then(Option::as_ref)
            .and_then(extension_item_attributes)
            .is_some();
        Ok(EntityAuthorityFacts {
            parentage,
            source: if source_present {
                captured
            } else {
                unavailable
            },
            source_file: if source_present {
                captured
            } else {
                unavailable
            },
            members: if self.provenance.member_sets().get(ordinal)
                == Some(&MemberSetCapture::Captured)
            {
                captured
            } else {
                unavailable
            },
            // Each admitted staging fact has one closed semantic record; an
            // explicit `Unknown` record remains captured semantic truth.
            semantic_type: captured,
            documentation: if self.documentation_captured.get(ordinal) == Some(&true) {
                captured
            } else {
                unavailable
            },
            visibility: if self.visibility_captured.get(ordinal) == Some(&true) {
                captured
            } else {
                unavailable
            },
            attributes: if attributes { captured } else { unavailable },
            language_extension: if self.extensions.get(ordinal).is_some_and(Option::is_some) {
                captured
            } else {
                unavailable
            },
        })
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
            return self.reject_pending_type_run(
                PendingTypeLane::Anonymous,
                FactFault::RefTarget {
                    lane: backend_semantic::vocabulary::ProjectionFactLane::TypeRows,
                    raw: owner,
                    fact_count: self.len,
                },
            );
        }
        if self.anonymous_rows == self.plan.anonymous_rows {
            return self
                .reject_pending_type_run(PendingTypeLane::Anonymous, FactFault::TypeRowCapacity);
        }
        let child_count = u32::from(self.anonymous_child_pending);
        if let Err(fault) = record.validate(child_count).map_err(FactFault::TypeRecord) {
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
        if let Err(fault) = self.validate_pending_anonymous_children(record, child_count) {
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
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
        if reserved_owner != self.len as u32 {
            return self.reject_pending_type_run(
                PendingTypeLane::Anonymous,
                FactFault::RefTarget {
                    lane: backend_semantic::vocabulary::ProjectionFactLane::ReservedTypeRows,
                    raw: reserved_owner,
                    fact_count: self.len,
                },
            );
        }
        debug_assert_eq!(reserved_owner, self.len as u32);
        if self.anonymous_rows == self.plan.anonymous_rows {
            return self
                .reject_pending_type_run(PendingTypeLane::Anonymous, FactFault::TypeRowCapacity);
        }
        let child_count = u32::from(self.anonymous_child_pending);
        if let Err(fault) = record.validate(child_count).map_err(FactFault::TypeRecord) {
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
        if let Err(fault) = self.validate_pending_anonymous_children(record, child_count) {
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
        }
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
            return self.reject_pending_type_run(
                PendingTypeLane::Anonymous,
                FactFault::TypeChildTarget {
                    position: usize::from(self.anonymous_child_pending),
                    target,
                    fact_count: self.len,
                },
            );
        }
        if self.anonymous_child_pending == MAX_PENDING_TYPE_CHILDREN {
            return self
                .reject_pending_type_run(PendingTypeLane::Anonymous, FactFault::TypeChildCapacity);
        }
        if self.anonymous_children_total == self.anonymous_child_targets.len() {
            let fault = FactFault::TypeChildPoolCapacity {
                lane: TypeChildLane::Anonymous,
                used: self.anonymous_children_total,
                requested: 1,
                capacity: self.anonymous_child_targets.len(),
            };
            return self.reject_pending_type_run(PendingTypeLane::Anonymous, fault);
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
                        target: TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                            backend_semantic::ir::TypeId::new(self.anonymous_child_targets[pooled]),
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
            return self.reject_pending_type_run(
                PendingTypeLane::Computed,
                FactFault::RefTarget {
                    lane: backend_semantic::vocabulary::ProjectionFactLane::ComputedOwners,
                    raw: owner,
                    fact_count: self.len,
                },
            );
        }
        if self.computed_rows == self.plan.computed_rows {
            return self.reject_pending_type_run(
                PendingTypeLane::Computed,
                FactFault::ComputedRowCapacity,
            );
        }
        let child_count = u32::from(self.computed_child_pending);
        if let Err(fault) = record.validate(child_count).map_err(FactFault::TypeRecord) {
            return self.reject_pending_type_run(PendingTypeLane::Computed, fault);
        }
        let child_start = self.computed_children_total - child_count as usize;
        for position in 0..child_count as usize {
            let pooled = child_start + position;
            let target = if self.computed_child_targets[pooled] == STAGED_TEXT_CHILD {
                TypeChildTarget::Text
            } else {
                TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                    backend_semantic::ir::TypeId::new(self.computed_child_targets[pooled]),
                ))
            };
            if let Err(fault) = record
                .validate_child_in_row(
                    position as u32,
                    child_count,
                    &SemanticTypeChild {
                        target,
                        name: self.computed_child_names[pooled],
                        flags: self.computed_child_flags[pooled],
                    },
                )
                .map_err(|fault| FactFault::TypeChild { position, fault })
            {
                return self.reject_pending_type_run(PendingTypeLane::Computed, fault);
            }
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
            return self.reject_pending_type_run(
                PendingTypeLane::Computed,
                FactFault::TypeChildTarget {
                    position: usize::from(self.computed_child_pending),
                    target,
                    fact_count: self.len,
                },
            );
        }
        if self.computed_child_pending == MAX_PENDING_TYPE_CHILDREN {
            return self
                .reject_pending_type_run(PendingTypeLane::Computed, FactFault::TypeChildCapacity);
        }
        if self.computed_children_total == self.computed_child_targets.len() {
            let fault = FactFault::TypeChildPoolCapacity {
                lane: TypeChildLane::Computed,
                used: self.computed_children_total,
                requested: 1,
                capacity: self.computed_child_targets.len(),
            };
            return self.reject_pending_type_run(PendingTypeLane::Computed, fault);
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
                lane: backend_semantic::vocabulary::ProjectionFactLane::Extensions,
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
                lane:
                    backend_semantic::vocabulary::ProjectionFactLane::ReplacementTypeParameterRange,
                raw: ordinal as u32,
                fact_count: self.type_parameter_len,
            });
        }
        if self.type_parameter_ranges[ordinal].is_none() {
            self.type_parameter_ranges[ordinal] = self.capture_type_parameter_range(&extension)?;
        }
        if self.free_predicate_ranges[ordinal].is_none() {
            self.free_predicate_ranges[ordinal] = self.capture_free_predicate_range(&extension)?;
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
                lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameterRanges,
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
                lane: backend_semantic::vocabulary::ProjectionFactLane::CapturedTypeParameterRange,
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
                lane: backend_semantic::vocabulary::ProjectionFactLane::Extensions,
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
                lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameterRanges,
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
                lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameterRanges,
                raw: start as u32,
                fact_count: self.type_parameter_len,
            });
        }
        Ok(Some(StagedTypeParameterRange {
            start: start as u32,
            length: (self.type_parameter_len - start) as u32,
        }))
    }

    /// Captures one Rust free-predicate range ambiently at push time. Only
    /// Rust extensions carry a free list; every other lane keeps its absent
    /// state.
    fn capture_free_predicate_range(
        &self,
        extension: &EmissionExtension,
    ) -> Result<Option<StagedFreePredicateRange>, FactFault> {
        let Some(start) = extension_free_predicate_start(extension) else {
            return Ok(None);
        };
        let start = start.raw as usize;
        if start > self.free_predicate_len {
            return Err(FactFault::RefTarget {
                lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameterRanges,
                raw: start as u32,
                fact_count: self.free_predicate_len,
            });
        }
        Ok(Some(StagedFreePredicateRange {
            start: start as u32,
            length: (self.free_predicate_len - start) as u32,
        }))
    }

    /// Binds a child declaration to an already admitted parent.  Parentage
    /// stays in the transaction staging lane, so the compact and owned views
    /// derive their relation from one authority fact rather than a renderer
    /// side channel.
    pub(super) fn attach_parent(&mut self, child: u32, parent: u32) -> Result<(), FactFault> {
        self.provenance.attach_parent(self.len, child, parent)
    }

    /// Records one Rust implementation's trait reference for identity
    /// framing. Only `Implementation` rows carry a trait; every other row
    /// keeps its absent state.
    pub(super) fn set_impl_trait(
        &mut self,
        ordinal: u32,
        trait_ref: StagedImplTrait<'source>,
    ) -> Result<(), FactFault> {
        let slot = self
            .impl_traits
            .get_mut(ordinal as usize)
            .ok_or(FactFault::RefTarget {
                lane: backend_semantic::vocabulary::ProjectionFactLane::Extensions,
                raw: ordinal,
                fact_count: self.len,
            })?;
        *slot = Some(trait_ref);
        Ok(())
    }

    /// Marks that an authority considered parentage for this row even when it
    /// proved the row is a root.  This is distinct from source-span capture.
    pub(super) fn mark_parentage_root(&mut self, entity: u32) -> Result<(), FactFault> {
        self.provenance.mark_parentage_root(self.len, entity)
    }

    /// Retains an authoritative native owner which has no emitted row.  It
    /// cannot be represented as a root or fabricated as a local parent.
    pub(super) fn mark_unrepresented_parent(
        &mut self,
        entity: u32,
        authority_identity: [u8; 16],
    ) -> Result<(), FactFault> {
        self.provenance
            .mark_unrepresented_parent(self.len, entity, authority_identity)
    }

    /// Binds one authority-captured declaration span without giving a raw
    /// image coordinate the ability to masquerade as a type or entity ID.
    pub(super) fn attach_source_span(
        &mut self,
        entity: u32,
        span: StagedSourceSpan,
    ) -> Result<(), FactFault> {
        self.provenance
            .attach_source_span(self.len, self.primary_source_len, entity, span)
    }

    /// Marks an authority-complete local member-set observation for one
    /// emitted declaration, including a semantically real empty set.
    pub(super) fn mark_members_captured(&mut self, entity: u32) -> Result<(), FactFault> {
        self.provenance.mark_members_captured(self.len, entity)
    }

    /// First pooled position of one fact's type-record children.
    ///
    /// Admission records this prefix coordinate with the row. Recursive
    /// projection therefore performs one indexed lookup instead of summing
    /// every preceding row's child count for each visit.
    pub(super) fn type_children_base(&self, ordinal: usize) -> usize {
        self.type_child_starts[ordinal] as usize
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
    ) -> Result<SemanticTypeRecord<'source>, backend_semantic::ir::BuildError> {
        if row < ANONYMOUS_ROW_BASE {
            return self.type_records.get(row as usize).copied().ok_or(
                backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
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
        Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Type,
            raw: row,
        })
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
            return self
                .type_child_counts
                .get(row as usize)
                .copied()
                .map(usize::from);
        }
        if self.is_anonymous_type_row(row) {
            return Some(usize::from(
                self.anonymous_child_counts[(row - ANONYMOUS_ROW_BASE) as usize],
            ));
        }
        self.is_computed_type_row(row)
            .then(|| usize::from(self.computed_child_counts[(row - COMPUTED_ROW_BASE) as usize]))
    }

    /// Reports whether the committed row at `row` carries exactly the staged
    /// record cells of `record`. The pooled child span is staging geometry and
    /// compares through the targets instead.
    pub(super) fn staged_record_matches(
        &self,
        row: u32,
        record: &SemanticTypeRecord<'source>,
    ) -> bool {
        self.staged_type_slot(row)
            .is_some_and(|_| self.type_record_for_demand(row) == *record)
    }

    /// Reports whether two committed row coordinates carry structurally
    /// identical type frames: the record cells, the ordered staged children
    /// (names and flags), and every recursive target. Anonymous and computed
    /// staging mints fresh coordinates for genuinely identical graphs, so
    /// the comparison is structural, never ordinal-exact, below the roots.
    pub(super) fn staged_rows_structurally_equal(&self, left: u32, right: u32, depth: u8) -> bool {
        if left == right {
            return true;
        }
        if depth == 0 {
            return false;
        }
        if self.staged_type_slot(left).is_none() || self.staged_type_slot(right).is_none() {
            return left == right;
        }
        if !self.staged_record_matches(left, &self.type_record_for_demand(right)) {
            return false;
        }
        let Some(left_count) = self.staged_type_child_count(left) else {
            return left == right;
        };
        let Some(right_count) = self.staged_type_child_count(right) else {
            return left == right;
        };
        if left_count != right_count {
            return false;
        }
        for position in 0..left_count {
            let (
                Some((left_target, left_name, left_flags)),
                Some((right_target, right_name, right_flags)),
            ) = (
                self.staged_type_child(left, position),
                self.staged_type_child(right, position),
            )
            else {
                return false;
            };
            if left_name != right_name || left_flags != right_flags {
                return false;
            }
            if !self.staged_rows_structurally_equal(left_target, right_target, depth - 1) {
                return false;
            }
        }
        true
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
    ) -> Result<backend_semantic::ir::AtomListId, FactFault> {
        self.intern_ref_list(ReferenceListLane::Atoms, atoms)
            .map(backend_semantic::ir::AtomListId::new)
    }

    /// Interns one pooled type list of fact ordinals.
    pub(super) fn intern_type_list(
        &mut self,
        types: &[u32],
    ) -> Result<backend_semantic::ir::TypeListId, FactFault> {
        self.intern_ref_list(ReferenceListLane::Types, types)
            .map(backend_semantic::ir::TypeListId::new)
    }

    /// Interns one pooled entity list of fact ordinals.
    pub(super) fn intern_entity_list(
        &mut self,
        entities: &[u32],
    ) -> Result<backend_semantic::ir::EntityListId, FactFault> {
        self.intern_ref_list(ReferenceListLane::Entities, entities)
            .map(backend_semantic::ir::EntityListId::new)
    }

    fn intern_ref_list(
        &mut self,
        lane: ReferenceListLane,
        elements: &[u32],
    ) -> Result<u32, FactFault> {
        if elements.len() > MAX_REF_LIST_ELEMENTS {
            return Err(FactFault::RefListElements);
        }
        for raw in elements {
            let limit = match lane {
                ReferenceListLane::Atoms => self.extension_atom_len,
                ReferenceListLane::Types | ReferenceListLane::Entities => self.len,
            };
            if *raw >= limit as u32 {
                return Err(FactFault::RefTarget {
                    lane: match lane {
                        ReferenceListLane::Atoms => ProjectionFactLane::AtomLists,
                        ReferenceListLane::Types => ProjectionFactLane::TypeLists,
                        ReferenceListLane::Entities => ProjectionFactLane::EntityLists,
                    },
                    raw: *raw,
                    fact_count: limit,
                });
            }
        }
        let (count, matches) = match lane {
            ReferenceListLane::Atoms => {
                let count = self.atom_list_len;
                let matches = (0..count).find(|index| self.atom_lists.row(*index) == Some(elements));
                (count, matches)
            }
            ReferenceListLane::Types => {
                let count = self.type_list_len;
                let matches = (0..count).find(|index| self.type_lists.row(*index) == Some(elements));
                (count, matches)
            }
            ReferenceListLane::Entities => {
                let count = self.entity_list_len;
                let matches =
                    (0..count).find(|index| self.entity_lists.row(*index) == Some(elements));
                (count, matches)
            }
        };
        if let Some(index) = matches {
            return Ok(index as u32);
        }
        if count == self.plan.ref_lists {
            return Err(FactFault::RefListCapacity);
        }
        let pool = match lane {
            ReferenceListLane::Atoms => &mut self.atom_lists,
            ReferenceListLane::Types => &mut self.type_lists,
            ReferenceListLane::Entities => &mut self.entity_lists,
        };
        pool.push(elements).map_err(|_| FactFault::RefListElements)?;
        match lane {
            ReferenceListLane::Atoms => self.atom_list_len = count + 1,
            ReferenceListLane::Types => self.type_list_len = count + 1,
            ReferenceListLane::Entities => self.entity_list_len = count + 1,
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
        let bound = constraint.map(backend_semantic::ir::ExtensionTypeParameterBound::Type);
        self.push_type_parameter_with_bounds(
            name,
            bound.as_slice(),
            default,
            backend_semantic::ir::ExtensionTypeParameterKind::Type {
                inference: backend_semantic::ir::TypeParameterInference::Ordinary,
            },
            backend_semantic::ir::Variance::Invariant,
            backend_semantic::ir::TypeParameterRequirements::none(),
        )
    }

    /// Appends one generic parameter together with its exact written bound
    /// sequence. The copied prefix is owned by this admission transaction;
    /// no producer can retain a temporary vector or infer an end later.
    pub(super) fn push_type_parameter_with_bounds(
        &mut self,
        name: &'source [u8],
        bounds: &[backend_semantic::ir::ExtensionTypeParameterBound<'source>],
        default: Option<u32>,
        kind: backend_semantic::ir::ExtensionTypeParameterKind,
        variance: backend_semantic::ir::Variance,
        requirements: backend_semantic::ir::TypeParameterRequirements,
    ) -> Result<u32, FactFault> {
        for raw in bounds
            .iter()
            .filter_map(|bound| match bound {
                backend_semantic::ir::ExtensionTypeParameterBound::Type(raw) => Some(*raw),
                backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(_) => None,
            })
            .chain(default)
            .chain(match kind {
                backend_semantic::ir::ExtensionTypeParameterKind::Type { .. }
                | backend_semantic::ir::ExtensionTypeParameterKind::Lifetime => None,
                backend_semantic::ir::ExtensionTypeParameterKind::ConstValue { value_type } => {
                    Some(value_type)
                }
            })
        {
            if raw >= self.len as u32
                && !self.is_anonymous_type_row(raw)
                && !self.is_computed_type_row(raw)
            {
                return Err(FactFault::RefTarget {
                    lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameters,
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
            bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
                start: u32::try_from(start).map_err(|_| FactFault::TypeParameterCapacity)?,
                length: u32::try_from(bounds.len())
                    .map_err(|_| FactFault::TypeParameterCapacity)?,
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

    /// Appends one Rust free predicate with its exact ordered bound run. The
    /// subject names an already-hosted staged type row and the bounds reuse
    /// the shared type-parameter bound lane, so admission rewrites both
    /// through its one staged-type remapping.
    pub(super) fn push_free_predicate(
        &mut self,
        subject: u32,
        bounds: &[backend_semantic::ir::ExtensionTypeParameterBound<'source>],
    ) -> Result<u32, FactFault> {
        if subject >= self.len as u32
            && !self.is_anonymous_type_row(subject)
            && !self.is_computed_type_row(subject)
        {
            return Err(FactFault::RefTarget {
                lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameters,
                raw: subject,
                fact_count: self.len,
            });
        }
        for raw in bounds.iter().filter_map(|bound| match bound {
            backend_semantic::ir::ExtensionTypeParameterBound::Type(raw) => Some(*raw),
            backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(_) => None,
        }) {
            if raw >= self.len as u32
                && !self.is_anonymous_type_row(raw)
                && !self.is_computed_type_row(raw)
            {
                return Err(FactFault::RefTarget {
                    lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameters,
                    raw,
                    fact_count: self.len,
                });
            }
        }
        if self.free_predicate_len == self.plan.free_predicates {
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
        self.free_predicates[self.free_predicate_len] =
            backend_semantic::ir::ExtensionFreePredicate {
                subject,
                bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
                    start: u32::try_from(start).map_err(|_| FactFault::TypeParameterCapacity)?,
                    length: u32::try_from(bounds.len())
                        .map_err(|_| FactFault::TypeParameterCapacity)?,
                },
            };
        self.free_predicate_len += 1;
        self.type_parameter_bound_len = end;
        Ok((self.free_predicate_len - 1) as u32)
    }

    /// Captures an explicit free-predicate list range at the producer's close
    /// point. Attachment time is not a list boundary.
    pub(super) fn free_predicate_range(
        &self,
        start: u32,
    ) -> Result<StagedFreePredicateRange, FactFault> {
        let start = start as usize;
        if start > self.free_predicate_len {
            return Err(FactFault::RefTarget {
                lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameterRanges,
                raw: start as u32,
                fact_count: self.free_predicate_len,
            });
        }
        Ok(StagedFreePredicateRange {
            start: start as u32,
            length: (self.free_predicate_len - start) as u32,
        })
    }

    /// Retrieves the immutable close-time free-predicate handle of an
    /// admitted generic declaration.
    pub(super) fn captured_free_predicate_range(
        &self,
        ordinal: usize,
    ) -> Result<StagedFreePredicateRange, FactFault> {
        self.free_predicate_ranges
            .get(ordinal)
            .copied()
            .flatten()
            .ok_or(FactFault::RefTarget {
                lane: backend_semantic::vocabulary::ProjectionFactLane::CapturedTypeParameterRange,
                raw: ordinal as u32,
                fact_count: self.len,
            })
    }

    /// Binds an already-closed free-predicate range to one fact's extension
    /// replacement, validating the staged start against the replacement row.
    pub(super) fn set_free_predicate_range(
        &mut self,
        ordinal: usize,
        extension: &EmissionExtension,
        range: StagedFreePredicateRange,
    ) -> Result<(), FactFault> {
        if ordinal >= self.len {
            return Err(FactFault::RefTarget {
                lane: backend_semantic::vocabulary::ProjectionFactLane::Extensions,
                raw: ordinal as u32,
                fact_count: self.len,
            });
        }
        let Some(start) = extension_free_predicate_start(extension) else {
            return Err(FactFault::RefTarget {
                lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameterRanges,
                raw: range.start,
                fact_count: self.free_predicate_len,
            });
        };
        let range_end = range.start.checked_add(range.length);
        if start.raw != range.start
            || !matches!(range_end, Some(end) if end as usize <= self.free_predicate_len)
            || matches!(self.free_predicate_ranges[ordinal], Some(existing) if existing != range)
        {
            return Err(FactFault::RefTarget {
                lane: backend_semantic::vocabulary::ProjectionFactLane::TypeParameterRanges,
                raw: range.start,
                fact_count: self.free_predicate_len,
            });
        }
        self.free_predicate_ranges[ordinal] = Some(range);
        Ok(())
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
            owner: backend_semantic::ir::EntityId::new(owner),
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
                TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                    backend_semantic::ir::TypeId::new(child.target),
                ))
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
                let valid =
                    self.is_anonymous_type_row(child.target) || child.target < fact_ordinal as u32;
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
                        cell: backend_semantic::ir::TypeCell::Nominal,
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
        let free_predicate_range = match fact.extension.as_ref() {
            Some(extension) => self
                .capture_free_predicate_range(extension)
                .map_err(rejected)?,
            None => None,
        };
        let type_pooled_start = self.total_type_children;
        let requested_type_children = usize::from(fact.type_child_count);
        let type_pooled_end = type_pooled_start
            .checked_add(requested_type_children)
            .filter(|end| *end <= self.type_child_targets.len())
            .ok_or_else(|| {
                rejected(FactFault::TypeChildPoolCapacity {
                    lane: TypeChildLane::Declared,
                    used: type_pooled_start,
                    requested: requested_type_children,
                    capacity: self.type_child_targets.len(),
                })
            })?;
        let pooled_start = self.total_children;
        let requested_product_children = child_count as usize;
        let pooled_end = pooled_start
            .checked_add(requested_product_children)
            .filter(|end| *end <= self.child_targets.len())
            .ok_or_else(|| {
                rejected(FactFault::ProductChildPoolCapacity {
                    used: pooled_start,
                    requested: requested_product_children,
                    capacity: self.child_targets.len(),
                })
            })?;

        self.kinds[fact_ordinal] = fact.kind;
        self.names[fact_ordinal] = fact.name;
        self.type_records[fact_ordinal] = fact.type_record;
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
        self.total_type_children = type_pooled_end;
        self.type_child_starts[fact_ordinal] = type_pooled_start as u32;
        self.type_child_counts[fact_ordinal] = fact.type_child_count;
        self.constructors[fact_ordinal] = fact.constructor;
        self.child_counts[fact_ordinal] = fact.child_count;
        self.type_parameter_ranges[fact_ordinal] = type_parameter_range;
        self.free_predicate_ranges[fact_ordinal] = free_predicate_range;
        self.extensions[fact_ordinal] = fact.extension;
        self.visibility[fact_ordinal] = fact.visibility;
        self.visibility_captured[fact_ordinal] = fact.visibility_captured;
        self.static_members[fact_ordinal] = fact.static_member;
        self.identity_discriminators[fact_ordinal] = fact.identity_discriminator;
        self.key_digests[fact_ordinal] = identity::fact_payload_basis(&fact);
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
        self.total_children = pooled_end;
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
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    fragment: DocFragmentInput<'source>,
) -> Result<DocInput<'source>, backend_semantic::ir::BuildError> {
    let text = |bytes: &'source [u8]| {
        core::str::from_utf8(bytes).map_err(|_| {
            backend_semantic::ir::BuildError::InvalidDocumentationUtf8 { bytes: bytes.len() }
        })
    };
    match fragment {
        DocFragmentInput::Text(bytes) => Ok(DocInput::Text(text(bytes)?)),
        DocFragmentInput::Code(bytes) => Ok(DocInput::Code(text(bytes)?)),
        DocFragmentInput::Link { label, target } => Ok(DocInput::Link {
            label: text(label)?,
            target: match target {
                DocLinkTarget::Local(id) => {
                    TreeLinkTarget::Local(backend_semantic::ir::TreeEntityId::new(id.raw))
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
                    let ecosystem = tree.intern_atom(ecosystem)?;
                    let path_id = tree.intern_atom(path)?;
                    let display_id = tree.intern_atom(path)?;
                    TreeLinkTarget::External(tree.intern_external(ExternalTarget::Foreign(
                        ForeignExternalTarget {
                            identity: ExternalDeclarationIdentity {
                                foreign: ForeignDeclarationId::from_canonical_bytes(&identity),
                                variant: VariantAvailability::Unavailable,
                            },
                            origin: ForeignTargetOrigin::Unspecified { ecosystem },
                            path: path_id,
                            display: display_id,
                            kind: None,
                        },
                    ))?)
                }
            },
        }),
        DocFragmentInput::SoftBreak => Ok(DocInput::SoftBreak),
        DocFragmentInput::HardBreak => Ok(DocInput::HardBreak),
    }
}

fn extension_type_parameter_start(
    extension: &EmissionExtension,
) -> Option<backend_semantic::ir::TypeParameterListId> {
    match extension {
        EmissionExtension::TypeScript(facts) => Some(facts.type_parameters),
        EmissionExtension::CSharp(facts) => Some(facts.constraints),
        EmissionExtension::Go(facts) => Some(facts.type_parameters),
        EmissionExtension::Rust(facts) => Some(facts.where_clauses),
        EmissionExtension::Clang(facts) => Some(facts.templates),
        EmissionExtension::Python(_) | EmissionExtension::Java(_) => None,
    }
}

/// Staging start of one Rust free-predicate list. Only Rust carries free
/// predicates; every other lane keeps its absent state.
fn extension_free_predicate_start(
    extension: &EmissionExtension,
) -> Option<backend_semantic::ir::FreePredicateListId> {
    match extension {
        EmissionExtension::Rust(facts) => Some(facts.free_predicates),
        _ => None,
    }
}

/// Attributes that belong to the generic tree item as well as their language
/// extension row.  The bytes are staged once and borrowed into both views;
/// this is not a renderer-side reconstruction.
fn extension_item_attributes(
    extension: &EmissionExtension,
) -> Option<backend_semantic::ir::AtomListId> {
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
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    start: backend_semantic::ir::TypeParameterListId,
    range: StagedTypeParameterRange,
    ids: &mut [Option<TypeId>],
    seen: &mut [u8],
    scratch: &mut ProjectionScratch,
) -> Result<backend_semantic::ir::TypeParameterListId, backend_semantic::ir::BuildError> {
    let start = start.raw as usize;
    if range.start != start as u32 {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::TypeParameters,
            raw: start as u32,
        });
    }
    if range.length == 0 {
        return tree.intern_type_parameters(&[]);
    }
    let end = start.checked_add(range.length as usize).ok_or(
        backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::TypeParameters,
            raw: start as u32,
        },
    )?;
    let parameters = facts.type_parameters.get(start..end).ok_or(
        backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::TypeParameters,
            raw: start as u32,
        },
    )?;
    let mut materialized = Vec::with_capacity(parameters.len());
    for parameter in parameters {
        let bound_end = parameter
            .bounds
            .start
            .checked_add(parameter.bounds.length)
            .ok_or(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::TypeParameterBounds,
                raw: parameter.bounds.start,
            })? as usize;
        let bounds = facts
            .type_parameter_bounds
            .get(parameter.bounds.start as usize..bound_end)
            .ok_or(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::TypeParameterBounds,
                raw: parameter.bounds.start,
            })?;
        let mut live_bounds = Vec::with_capacity(bounds.len());
        for bound in bounds {
            live_bounds.push(match bound {
                backend_semantic::ir::ExtensionTypeParameterBound::Type(row) => {
                    backend_semantic::ir::TypeParameterBound::Type(live_type(
                        tree, facts, *row, ids, seen, scratch,
                    )?)
                }
                backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(name) => {
                    backend_semantic::ir::TypeParameterBound::Lifetime(tree.intern_atom(name)?)
                }
            });
        }
        let kind = match parameter.kind {
            backend_semantic::ir::ExtensionTypeParameterKind::Type { inference } => {
                backend_semantic::ir::TypeParameterKind::Type { inference }
            }
            backend_semantic::ir::ExtensionTypeParameterKind::ConstValue { value_type } => {
                backend_semantic::ir::TypeParameterKind::ConstValue {
                    value_type: live_type(tree, facts, value_type, ids, seen, scratch)?,
                }
            }
            backend_semantic::ir::ExtensionTypeParameterKind::Lifetime => {
                backend_semantic::ir::TypeParameterKind::Lifetime
            }
        };
        materialized.push(backend_semantic::ir::TypeParameter {
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
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    id: backend_semantic::ir::AtomListId,
) -> Result<backend_semantic::ir::AtomListId, backend_semantic::ir::BuildError> {
    let index = id.raw as usize;
    if facts.atom_list_len == 0 && index == 0 {
        return tree.intern_attributes(&[]);
    }
    let Some(row) = facts.atom_lists.row(index) else {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::AtomList,
            raw: id.raw,
        });
    };
    let mut atoms = Vec::with_capacity(row.len());
    for provisional in row {
        let bytes = facts
            .extension_atoms
            .get(*provisional as usize)
            .copied()
            .ok_or(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::Atom,
                raw: *provisional,
            })?;
        atoms.push(tree.intern_atom(bytes)?);
    }
    tree.intern_attributes(&atoms)
}

/// Materializes one Rust free-predicate list into the carrying builder. The
/// staging handle names the declaration's pooled run start and the captured
/// range bounds its length; subjects and type bounds project through the one
/// staged-type mapping while lifetime bounds keep their exact spelling.
fn live_free_predicates<'source>(
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    start: backend_semantic::ir::FreePredicateListId,
    range: StagedFreePredicateRange,
    ids: &mut [Option<TypeId>],
    seen: &mut [u8],
    scratch: &mut ProjectionScratch,
) -> Result<backend_semantic::ir::FreePredicateListId, backend_semantic::ir::BuildError> {
    let start = start.raw as usize;
    if range.start != start as u32 {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::FreePredicates,
            raw: start as u32,
        });
    }
    if range.length == 0 {
        return tree.intern_free_predicates(&[]);
    }
    let end = start.checked_add(range.length as usize).ok_or(
        backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::FreePredicates,
            raw: start as u32,
        },
    )?;
    let predicates = facts.free_predicates.get(start..end).ok_or(
        backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::FreePredicates,
            raw: start as u32,
        },
    )?;
    let mut materialized = Vec::with_capacity(predicates.len());
    for predicate in predicates {
        let bound_end = predicate
            .bounds
            .start
            .checked_add(predicate.bounds.length)
            .ok_or(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::TypeParameterBounds,
                raw: predicate.bounds.start,
            })? as usize;
        let bounds = facts
            .type_parameter_bounds
            .get(predicate.bounds.start as usize..bound_end)
            .ok_or(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::TypeParameterBounds,
                raw: predicate.bounds.start,
            })?;
        let mut live_bounds = Vec::with_capacity(bounds.len());
        for bound in bounds {
            live_bounds.push(match bound {
                backend_semantic::ir::ExtensionTypeParameterBound::Type(row) => {
                    backend_semantic::ir::TypeParameterBound::Type(live_type(
                        tree, facts, *row, ids, seen, scratch,
                    )?)
                }
                backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(name) => {
                    backend_semantic::ir::TypeParameterBound::Lifetime(tree.intern_atom(name)?)
                }
            });
        }
        materialized.push(backend_semantic::ir::FreePredicate {
            subject: live_type(tree, facts, predicate.subject, ids, seen, scratch)?,
            bounds: tree.intern_type_parameter_bounds(&live_bounds)?,
        });
    }
    tree.intern_free_predicates(&materialized)
}

fn live_type_list<'source>(
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    id: backend_semantic::ir::TypeListId,
    ids: &mut [Option<TypeId>],
    seen: &mut [u8],
    scratch: &mut ProjectionScratch,
) -> Result<backend_semantic::ir::TypeListId, backend_semantic::ir::BuildError> {
    let index = id.raw as usize;
    if facts.type_list_len == 0 && index == 0 {
        return tree.intern_types(&[]);
    }
    if index >= facts.type_list_len {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::TypeList,
            raw: id.raw,
        });
    }
    let Some(rows) = facts.type_lists.row(index) else {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::TypeList,
            raw: id.raw,
        });
    };
    let mut types = Vec::with_capacity(rows.len());
    for row in rows {
        types.push(live_type(tree, facts, *row, ids, seen, scratch)?);
    }
    tree.intern_types(&types)
}

fn live_entity_list(
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'_>,
    id: backend_semantic::ir::EntityListId,
) -> Result<backend_semantic::ir::EntityListId, backend_semantic::ir::BuildError> {
    let index = id.raw as usize;
    if facts.entity_list_len == 0 && index == 0 {
        return tree.intern_members(&[]);
    }
    if index >= facts.entity_list_len {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::EntityList,
            raw: id.raw,
        });
    }
    let Some(rows) = facts.entity_lists.row(index) else {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::EntityList,
            raw: id.raw,
        });
    };
    let mut entities = Vec::with_capacity(rows.len());
    for raw in rows {
        let local = backend_semantic::ir::TreeEntityId::new(*raw);
        entities.push(tree.entities().get(local).ok_or(
            backend_semantic::ir::BuildError::InvalidTreeEntity {
                raw: *raw,
                count: facts.len as u32,
            },
        )?);
    }
    tree.intern_members(&entities)
}

fn live_extension_span<'source>(
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    span: backend_semantic::ir::SourceSpan,
) -> Result<backend_semantic::ir::SourceSpan, backend_semantic::ir::BuildError> {
    let file = facts
        .extension_atoms
        .get(span.file().raw as usize)
        .copied()
        .ok_or(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Atom,
            raw: span.file().raw,
        })?;
    backend_semantic::ir::SourceSpan::new(tree.intern_atom(file)?, span.start(), span.end()).ok_or(
        backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Atom,
            raw: span.file().raw,
        },
    )
}

const fn occurrence_link_kind(
    kind: backend_semantic::ir::ReferenceKind,
) -> backend_semantic::ir::LinkKind {
    match kind {
        backend_semantic::ir::ReferenceKind::FunctionCall => backend_semantic::ir::LinkKind::Calls,
        backend_semantic::ir::ReferenceKind::MethodCall => {
            backend_semantic::ir::LinkKind::MethodCall
        }
        backend_semantic::ir::ReferenceKind::TypeReference => {
            backend_semantic::ir::LinkKind::TypeReference
        }
        backend_semantic::ir::ReferenceKind::VariableUse => backend_semantic::ir::LinkKind::Reads,
        backend_semantic::ir::ReferenceKind::MacroInvocation => {
            backend_semantic::ir::LinkKind::Calls
        }
        backend_semantic::ir::ReferenceKind::FieldAccess => backend_semantic::ir::LinkKind::Reads,
        backend_semantic::ir::ReferenceKind::Import => backend_semantic::ir::LinkKind::Imports,
        backend_semantic::ir::ReferenceKind::Overrides => backend_semantic::ir::LinkKind::Overrides,
    }
}

const fn occurrence_link_confidence(
    confidence: backend_semantic::ir::OccurrenceConfidence,
) -> backend_semantic::ir::Confidence {
    match confidence {
        backend_semantic::ir::OccurrenceConfidence::Syntactic => {
            backend_semantic::ir::Confidence::Syntactic
        }
        backend_semantic::ir::OccurrenceConfidence::Suffix => {
            backend_semantic::ir::Confidence::Heuristic
        }
        backend_semantic::ir::OccurrenceConfidence::Index => {
            backend_semantic::ir::Confidence::Indexed
        }
        backend_semantic::ir::OccurrenceConfidence::Import => {
            backend_semantic::ir::Confidence::Imported
        }
        backend_semantic::ir::OccurrenceConfidence::Oracle => {
            backend_semantic::ir::Confidence::Compiler
        }
    }
}

/// Lifts an owner-relative staged occurrence range into the primary entered
/// source coordinate space.  Uncaptured owners remain explicitly source-less;
/// captured owners must satisfy the exact containment law.
fn occurrence_source_span(
    facts: &FactSet<'_>,
    file: Option<AtomId>,
    owner: u32,
    relative: backend_semantic::ir::RelSpan,
) -> Result<Option<backend_semantic::ir::SourceSpan>, backend_semantic::ir::BuildError> {
    let Some(owner_span) = facts
        .provenance
        .source_spans()
        .get(owner as usize)
        .copied()
        .flatten()
    else {
        return Ok(None);
    };
    let Some(start) = owner_span.start.checked_add(relative.start) else {
        return Err(backend_semantic::ir::BuildError::InvalidOccurrenceSpan {
            owner: backend_semantic::ir::EntityId::new(owner),
            start: relative.start,
            end: relative.end,
        });
    };
    let Some(end) = owner_span.start.checked_add(relative.end) else {
        return Err(backend_semantic::ir::BuildError::InvalidOccurrenceSpan {
            owner: backend_semantic::ir::EntityId::new(owner),
            start: relative.start,
            end: relative.end,
        });
    };
    if start > end || end > owner_span.end {
        return Err(backend_semantic::ir::BuildError::InvalidOccurrenceSpan {
            owner: backend_semantic::ir::EntityId::new(owner),
            start,
            end,
        });
    }
    Ok(file.and_then(|file| backend_semantic::ir::SourceSpan::new(file, start, end)))
}

fn external_from_occurrence<'source>(
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    owner: backend_semantic::ir::EntityId,
    target: backend_semantic::ir::OccurrenceTarget<'source>,
) -> Result<TreeLinkTarget, backend_semantic::ir::BuildError> {
    match target {
        backend_semantic::ir::OccurrenceTarget::Local(local) => Ok(TreeLinkTarget::Local(
            backend_semantic::ir::TreeEntityId::new(local.raw),
        )),
        backend_semantic::ir::OccurrenceTarget::Stable(stable) => Ok(TreeLinkTarget::External(
            tree.intern_external(ExternalTarget::Stable { target: stable })?,
        )),
        backend_semantic::ir::OccurrenceTarget::Foreign(foreign) => {
            // The vocabulary owns the domain separation, origin cells, and
            // kind discriminator.  Rebuilding a near-copy here once omitted
            // `ForeignKey::kind`, causing same-path references to collapse.
            let target = VariantFingerprint::from_canonical_bytes(foreign.path.as_bytes());
            let length = foreign.key_preimage_len().map_err(|cause| {
                backend_semantic::ir::BuildError::ForeignKeyPreimage {
                    owner,
                    target,
                    cause,
                }
            })?;
            let mut identity = vec![0_u8; length];
            let identity = foreign.key_id(&mut identity).map_err(|cause| {
                backend_semantic::ir::BuildError::ForeignKeyPreimage {
                    owner,
                    target,
                    cause,
                }
            })?;
            let origin = match foreign.origin {
                backend_semantic::ir::ForeignOrigin::Package(lineage) => {
                    ForeignTargetOrigin::Package {
                        ecosystem: tree.intern_atom(lineage.ecosystem.as_bytes())?,
                        package: tree.intern_atom(lineage.name.as_bytes())?,
                    }
                }
                backend_semantic::ir::ForeignOrigin::Namespace {
                    ecosystem,
                    namespace,
                } => ForeignTargetOrigin::Namespace {
                    ecosystem: tree.intern_atom(ecosystem.as_bytes())?,
                    namespace: tree.intern_atom(namespace.as_bytes())?,
                },
                backend_semantic::ir::ForeignOrigin::Universe { ecosystem } => {
                    ForeignTargetOrigin::Universe {
                        ecosystem: tree.intern_atom(ecosystem.as_bytes())?,
                    }
                }
            };
            let path = tree.intern_atom(foreign.path.as_bytes())?;
            let display = tree.intern_atom(foreign.display.as_bytes())?;
            Ok(TreeLinkTarget::External(tree.intern_external(
                ExternalTarget::Foreign(ForeignExternalTarget {
                    identity: ExternalDeclarationIdentity {
                        foreign: ForeignDeclarationId::from_content_id(identity),
                        variant: VariantAvailability::Unavailable,
                    },
                    origin,
                    path,
                    display,
                    kind: foreign.kind,
                }),
            )?))
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
    ) -> Result<usize, backend_semantic::ir::BuildError> {
        self.begin(self.type_children.len(), count, self.type_capacity, row)
    }

    fn begin_tuple_elements(
        &self,
        count: usize,
        row: u32,
    ) -> Result<usize, backend_semantic::ir::BuildError> {
        self.begin(self.tuple_elements.len(), count, self.tuple_capacity, row)
    }

    fn begin_template_parts(
        &self,
        count: usize,
        row: u32,
    ) -> Result<usize, backend_semantic::ir::BuildError> {
        self.begin(
            self.template_parts.len(),
            count,
            self.template_capacity,
            row,
        )
    }

    fn begin_object_members(
        &self,
        count: usize,
        row: u32,
    ) -> Result<usize, backend_semantic::ir::BuildError> {
        self.begin(self.object_members.len(), count, self.object_capacity, row)
    }

    fn begin(
        &self,
        used: usize,
        additional: usize,
        capacity: usize,
        row: u32,
    ) -> Result<usize, backend_semantic::ir::BuildError> {
        let required =
            used.checked_add(additional)
                .ok_or(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
                    raw: row,
                })?;
        if required > capacity {
            return Err(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::Type,
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
) -> Result<TypeId, backend_semantic::ir::BuildError> {
    let (target, _, _) = facts.staged_type_child(row, position).ok_or(
        backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Type,
            raw: row,
        },
    )?;
    if target == STAGED_TEXT_CHILD {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Type,
            raw: row,
        });
    }
    let index =
        facts
            .staged_type_slot(target)
            .ok_or(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::Type,
                raw: target,
            })?;
    if seen.get(index).copied() != Some(2) {
        return Err(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Type,
            raw: target,
        });
    }
    ids.get(index)
        .copied()
        .flatten()
        .ok_or(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Type,
            raw: target,
        })
}

fn live_type<'source>(
    tree: &mut backend_semantic::ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    row: u32,
    ids: &mut [Option<TypeId>],
    seen: &mut [u8],
    scratch: &mut ProjectionScratch,
) -> Result<TypeId, backend_semantic::ir::BuildError> {
    let index = facts
        .staged_type_slot(row)
        .ok_or(backend_semantic::ir::BuildError::Dangling {
            space: backend_semantic::ir::SemanticSpace::Type,
            raw: row,
        })?;
    debug_assert!(index < ids.len());
    match seen[index] {
        2 => {
            return ids[index].ok_or(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::Type,
                raw: row,
            });
        }
        1 => return Err(backend_semantic::ir::BuildError::RecursiveType { raw: row }),
        _ => {}
    }
    let record = facts.staged_type_record(row)?;
    // An explicit source `Unknown` is semantic truth (unannotated,
    // dynamically typed, unresolved, truncated, ...), not absence.  The
    // owned item type therefore never disappears merely because it is the
    // declaration's top-level row.
    seen[index] = 1;
    let child = |position: usize| facts.staged_type_child(row, position);
    let child_count =
        facts
            .staged_type_child_count(row)
            .ok_or(backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::Type,
                raw: row,
            })?;
    for position in 0..child_count {
        if child(position).is_some_and(|item| item.0 == STAGED_TEXT_CHILD) {
            continue;
        }
        live_type(
            tree,
            facts,
            child(position).map(|item| item.0).ok_or(
                backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
                    raw: row,
                },
            )?,
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
                    && bytes.iter().all(|byte| {
                        byte.is_ascii_digit() || matches!(byte, b'.' | b'-' | b'+' | b'_')
                    }) =>
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
                    && matches!(bytes, b"true" | b"false") =>
            {
                tree.intern_concrete(ConcreteType::Literal(LiteralType::Boolean(
                    bytes == b"true",
                )))?
                .erase()
            }
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
                    .ok_or(backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Type,
                        raw: row,
                    })?;
                    let role = match PrimitiveShape::try_from(record.payload0) {
                        Ok(PrimitiveShape::UnicodeScalar) => NativeCharacterRole::UnicodeScalar,
                        Ok(PrimitiveShape::Utf16CodeUnit) => NativeCharacterRole::Utf16CodeUnit,
                        Ok(PrimitiveShape::Utf32CodeUnit) => NativeCharacterRole::Utf32CodeUnit,
                        Ok(PrimitiveShape::CPlainSignedChar) => NativeCharacterRole::CPlainSigned,
                        Ok(PrimitiveShape::CPlainUnsignedChar) => {
                            NativeCharacterRole::CPlainUnsigned
                        }
                        Ok(PrimitiveShape::CSignedChar) => NativeCharacterRole::CSigned,
                        Ok(PrimitiveShape::CUnsignedChar) => NativeCharacterRole::CUnsigned,
                        Ok(PrimitiveShape::CWideChar) => {
                            NativeCharacterRole::CWideSignednessUnavailable
                        }
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
                        .intern_unknown(backend_semantic::ir::UnknownType::new(
                            backend_semantic::ir::UnknownReason::NoIrRepresentation,
                        ))?
                        .erase(),
                },
                Ok(PrimitiveShape::Builtin) => match record
                    .text
                    .and_then(|spelling| builtin_from_spelling(spelling, record.payload1))
                {
                    Some(builtin) => tree
                        .intern_concrete(ConcreteType::Builtin(builtin))?
                        .erase(),
                    None => {
                        let spelling = record
                            .text
                            .map(|bytes| tree.intern_atom(bytes))
                            .transpose()?;
                        tree.intern_unknown(backend_semantic::ir::UnknownType {
                            reason: backend_semantic::ir::UnknownReason::NoIrRepresentation,
                            spelling,
                        })?
                        .erase()
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
                        .intern_unknown(backend_semantic::ir::UnknownType::new(
                            backend_semantic::ir::UnknownReason::NoIrRepresentation,
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
                            backend_semantic::ir::Mutability::Mutable
                        } else {
                            backend_semantic::ir::Mutability::Immutable
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
                        category: if record.payload0
                            == u32::from(PrimitiveShape::CxxLvalueReference)
                        {
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
                            backend_semantic::ir::Mutability::Mutable
                        } else {
                            backend_semantic::ir::Mutability::Immutable
                        },
                    })?
                    .erase(),
                _ => tree
                    .intern_unknown(backend_semantic::ir::UnknownType::new(
                        backend_semantic::ir::UnknownReason::NoIrRepresentation,
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
            // The staged cell carries the lattice's frozen discriminant
            // (`Add = 0`, `Remove = 1`, `Absent = 2`), not the owned IR's.
            let modifier = |value: u32| match backend_semantic::ir::LatticeMappedModifier::try_from(
                value,
            ) {
                Ok(backend_semantic::ir::LatticeMappedModifier::Add) => {
                    backend_semantic::ir::MappedModifier::Add
                }
                Ok(backend_semantic::ir::LatticeMappedModifier::Remove) => {
                    backend_semantic::ir::MappedModifier::Remove
                }
                Ok(backend_semantic::ir::LatticeMappedModifier::Absent) | Err(_) => {
                    backend_semantic::ir::MappedModifier::Preserve
                }
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
                let (target, text, _) =
                    child(position).ok_or(backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Type,
                        raw: row,
                    })?;
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
        SemanticTypeTag::SelfType if record.text == Some(b"this") => {
            tree.intern_computed(ComputedType::This)?.erase()
        }
        SemanticTypeTag::SelfType | SemanticTypeTag::TypeVar => {
            let spelling = match record.text {
                Some(spelling) => spelling,
                None => b"Self",
            };
            let atom = tree.intern_atom(spelling)?;
            tree.intern_concrete(ConcreteType::Parameter(atom))?.erase()
        }
        SemanticTypeTag::Nominal => match record.nominal {
            Some(NominalRef::Local(id)) => tree.intern_concrete(ConcreteType::Nominal(id))?.erase(),
            Some(NominalRef::External(external)) => {
                let spelling = record
                    .text
                    .ok_or(backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Atom,
                        raw: external.ordinal,
                    })?;
                let path = tree.intern_atom(spelling)?;
                let target = tree.intern_external(ExternalTarget::FragmentEntity {
                    target: external,
                    display: path,
                })?;
                tree.intern_concrete(ConcreteType::External(target))?
                    .erase()
            }
            Some(NominalRef::Stable(stable)) => {
                let target = tree.intern_external(ExternalTarget::Stable { target: stable })?;
                tree.intern_concrete(ConcreteType::External(target))?
                    .erase()
            }
            None => tree
                .intern_unknown(backend_semantic::ir::UnknownType::new(
                    backend_semantic::ir::UnknownReason::NoIrRepresentation,
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
                    let (_, label, flags) =
                        child(position).ok_or(backend_semantic::ir::BuildError::Dangling {
                            space: backend_semantic::ir::SemanticSpace::Type,
                            raw: row,
                        })?;
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
        SemanticTypeTag::ArraySequence if child_count == 1 => tree
            .intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: backend_semantic::ir::ArrayShape::Sequence,
            })?
            .erase(),
        SemanticTypeTag::ArrayRectangular if child_count == 1 => {
            let rank = u16::try_from(record.payload0)
                .ok()
                .and_then(NonZeroU16::new)
                .ok_or(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
                    raw: row,
                })?;
            tree.intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: backend_semantic::ir::ArrayShape::Rectangular { rank },
            })?
            .erase()
        }
        SemanticTypeTag::ArrayFixed if child_count == 1 => tree
            .intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: backend_semantic::ir::ArrayShape::FixedValue {
                    length: u64::from(record.payload0) | (u64::from(record.payload1) << 32),
                },
            })?
            .erase(),
        SemanticTypeTag::ArrayConstExpression if child_count == 1 => {
            let expression = record
                .text
                .ok_or(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Atom,
                    raw: row,
                })?;
            let expression = tree.intern_atom(expression)?;
            tree.intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: backend_semantic::ir::ArrayShape::ConstExpression(expression),
            })?
            .erase()
        }
        SemanticTypeTag::ArrayIncomplete if child_count == 1 => tree
            .intern_concrete(ConcreteType::Array {
                element: child_type(0)?,
                shape: backend_semantic::ir::ArrayShape::Incomplete,
            })?
            .erase(),
        SemanticTypeTag::CQualified if child_count == 1 => tree
            .intern_concrete(ConcreteType::CQualified {
                target: child_type(0)?,
                qualifiers: CvQualifiers::try_from(record.payload0).map_err(|_| {
                    backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Type,
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
                    return Err(backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Type,
                        raw: row,
                    });
                }
            };
            tree.intern_concrete(ConcreteType::Wildcard(wildcard))?
                .erase()
        }
        SemanticTypeTag::Annotated if child_count == 1 => {
            let kind = AnnotationKind::try_from(record.payload0).map_err(|_| {
                backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
                    raw: row,
                }
            })?;
            tree.intern_concrete(ConcreteType::Annotated {
                kind,
                target: child_type(0)?,
            })?
            .erase()
        }
        SemanticTypeTag::Inferred => {
            let spelling = record
                .text
                .map(|bytes| tree.intern_atom(bytes))
                .transpose()?;
            tree.intern_concrete(ConcreteType::Inferred(spelling))?
                .erase()
        }
        SemanticTypeTag::QualifiedPath if child_count >= 1 => {
            let spelling = record
                .text
                .ok_or(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Atom,
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
                backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
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
                let (_, name, flags) =
                    child(position).ok_or(backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Type,
                        raw: row,
                    })?;
                let name = name.ok_or(backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Atom,
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
                backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
                    raw: row,
                },
            )?)
            .map_err(|_| backend_semantic::ir::BuildError::Dangling {
                space: backend_semantic::ir::SemanticSpace::Type,
                raw: row,
            })?;
            let parameter_count = child_count.checked_sub(result_count).ok_or(
                backend_semantic::ir::BuildError::Dangling {
                    space: backend_semantic::ir::SemanticSpace::Type,
                    raw: row,
                },
            )?;
            let parameter_start = scratch.begin_tuple_elements(parameter_count, row)?;
            for position in 0..parameter_count {
                let (target, child_name, flags) =
                    child(position).ok_or(backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Type,
                        raw: row,
                    })?;
                let target = target as usize;
                // A parameter label is only ever the explicit child name a
                // producer attached, or the target row's own name when that
                // row is a real `Parameter` carrier fact (a declared
                // executable's parameter binding). For an anonymous function
                // type the target is a type row whose fact name is a type
                // spelling (TypeScript) or an unrelated identity; inheriting
                // it would render `rgb: fn(number: f64, ...)` for a written
                // `rgb: (red: number, ...) => this`.
                let name = child_name.or_else(|| {
                    (target < facts.len && facts.kinds[target] == EntityKind::Parameter)
                        .then(|| facts.names[target])
                });
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
            let parameters =
                tree.intern_tuple_elements(&scratch.tuple_elements[parameter_start..])?;
            scratch.tuple_elements.truncate(parameter_start);
            let result_start = scratch.begin_tuple_elements(result_count, row)?;
            for result in 0..result_count {
                let position = parameter_count + result;
                let (_, child_name, flags) =
                    child(position).ok_or(backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Type,
                        raw: row,
                    })?;
                // A result label is only ever the explicit child name a
                // producer attached (a Go named result). Unlike a parameter,
                // a result slot's target row is not a source-named binding:
                // its fact name is the owning function's name (Rust,
                // Python), a type spelling (TypeScript), or a synthesized
                // positional identity (Go `_1`). Inheriting it would render
                // `-> (apply: u8)` for a plain `-> u8`.
                scratch.tuple_elements.push(TupleElement {
                    label: child_name.map(|name| tree.intern_atom(name)).transpose()?,
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
            let abi = record.text.map(|abi| tree.intern_atom(abi)).transpose()?;
            tree.intern_concrete(ConcreteType::Function {
                parameters,
                results,
                abi,
                variadic: record.function_variadic_form().ok_or(
                    backend_semantic::ir::BuildError::Dangling {
                        space: backend_semantic::ir::SemanticSpace::Type,
                        raw: row,
                    },
                )?,
                unsafe_: record.payload0 & SemanticTypeRecord::FUNCTION_UNSAFE_FLAG != 0,
            })?
            .erase()
        }
        SemanticTypeTag::Unknown => {
            let reason = match backend_semantic::ir::TypeReason::try_from(record.payload0) {
                Ok(backend_semantic::ir::TypeReason::Unannotated) => {
                    backend_semantic::ir::UnknownReason::Unannotated
                }
                Ok(backend_semantic::ir::TypeReason::DynamicallyTyped) => {
                    backend_semantic::ir::UnknownReason::DynamicallyTyped
                }
                Ok(backend_semantic::ir::TypeReason::UnresolvedLocalName) => {
                    backend_semantic::ir::UnknownReason::UnresolvedLocalName
                }
                Ok(backend_semantic::ir::TypeReason::UnresolvedExternal) => {
                    backend_semantic::ir::UnknownReason::UnresolvedExternal
                }
                Ok(backend_semantic::ir::TypeReason::TruncatedAtDepthLimit) => {
                    backend_semantic::ir::UnknownReason::TruncatedAtDepthLimit
                }
                Ok(backend_semantic::ir::TypeReason::OracleGap) => {
                    backend_semantic::ir::UnknownReason::OracleGap
                }
                Ok(backend_semantic::ir::TypeReason::NoIrRepresentation) | Err(_) => {
                    backend_semantic::ir::UnknownReason::NoIrRepresentation
                }
            };
            let spelling = record
                .text
                .map(|bytes| tree.intern_atom(bytes))
                .transpose()?;
            tree.intern_unknown(backend_semantic::ir::UnknownType { reason, spelling })?
                .erase()
        }
        // A closed row that has not gained a richer owned-IR variant remains
        // explicit semantic truth. Preserve any tag-owned spelling rather
        // than collapsing it into a renderer's generic `?unsupported`.
        _ => {
            let spelling = record
                .text
                .map(|bytes| tree.intern_atom(bytes))
                .transpose()?;
            tree.intern_unknown(backend_semantic::ir::UnknownType {
                reason: backend_semantic::ir::UnknownReason::NoIrRepresentation,
                spelling,
            })?
            .erase()
        }
    };
    ids[index] = Some(ty);
    seen[index] = 2;
    Ok(ty)
}

/// Maps only the spelling-bearing builtin rows in the common type lattice to
/// the closed cross-language builtin vocabulary.  Width-bearing primitives
/// remain payload-driven above; an unrecognized spelling stays an explicit
/// `NoIrRepresentation` unknown with its atom, never a false `String`.
///
/// `width` is the measured bit width carried in the row's free payload cell.
/// It is authoritative only for the canonical C integer ranks `long` and
/// `long long` (and their unsigned forms), whose exact C rank is not
/// recoverable from a width cell and whose spelling is therefore the identity
/// cell.  Using the measured width keeps the result correct on every ABI
/// instead of hardcoding LP64.  Every other spelling ignores `width`.
const fn builtin_from_spelling(spelling: &[u8], width: u32) -> Option<BuiltinType> {
    if let Some(builtin) = c_integer_from_spelling(spelling, width) {
        return Some(builtin);
    }
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
        b"string" | b"str" | b"String" | b"java.lang.String" => Some(BuiltinType::String),
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

/// Maps the canonical C rank spellings `long`/`long long` and their unsigned
/// forms, together with the measured width, to the closed width-bearing
/// builtin vocabulary.  This is deliberately narrower than "every C integer
/// spelling": `int` is already a live spelling for Python's arbitrary-precision
/// integer, so only the ranks that need spelling identity are claimed here.
const fn c_integer_from_spelling(spelling: &[u8], width: u32) -> Option<BuiltinType> {
    let signed = match spelling {
        b"long" | b"long long" => true,
        b"unsigned long" | b"unsigned long long" => false,
        _ => return None,
    };
    match (width, signed) {
        (8, false) => Some(BuiltinType::U8),
        (8, true) => Some(BuiltinType::I8),
        (16, false) => Some(BuiltinType::U16),
        (16, true) => Some(BuiltinType::I16),
        (32, false) => Some(BuiltinType::U32),
        (32, true) => Some(BuiltinType::I32),
        (64, false) => Some(BuiltinType::U64),
        (64, true) => Some(BuiltinType::I64),
        (128, false) => Some(BuiltinType::U128),
        (128, true) => Some(BuiltinType::I128),
        _ => None,
    }
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
        products.saturating_mul(
            products
                .saturating_mul(products)
                .saturating_add(2 * products),
        ),
    );
    let probe = products.saturating_mul(products);
    let child_visits = 2_u64
        .saturating_mul(children)
        .saturating_mul(products)
        .saturating_add(children.saturating_mul(sort.saturating_add(probe)));
    let lane = 4_u64.saturating_mul(products).saturating_add(children);
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
    profile: backend_semantic::vocabulary::LanguageProfile,
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

    let mut semantic_atoms = vec![SemanticAtom { bytes: b"" }; fact_count].into_boxed_slice();
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
    let mut scratch_representatives = vec![ProductId::new(0); fact_count].into_boxed_slice();
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
            owner: backend_semantic::ir::EntityId::new(0),
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
            bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
                start: 0,
                length: 0,
            },
            default: None,
            variance: backend_semantic::ir::Variance::Invariant,
            kind: backend_semantic::ir::ExtensionTypeParameterKind::Type {
                inference: backend_semantic::ir::TypeParameterInference::Ordinary,
            },
            requirements: backend_semantic::ir::TypeParameterRequirements::none(),
        };
        facts.type_parameter_len
    ]
    .into_boxed_slice();
    type_parameters[..facts.type_parameter_len]
        .copy_from_slice(&facts.type_parameters[..facts.type_parameter_len]);
    let mut type_parameter_bounds = vec![
        backend_semantic::ir::ExtensionTypeParameterBound::Type(0);
        facts.type_parameter_bound_len
    ]
    .into_boxed_slice();
    for (index, bound) in facts.type_parameter_bounds[..facts.type_parameter_bound_len]
        .iter()
        .enumerate()
    {
        type_parameter_bounds[index] = match bound {
            backend_semantic::ir::ExtensionTypeParameterBound::Type(raw) => {
                backend_semantic::ir::ExtensionTypeParameterBound::Type(remap(*raw))
            }
            backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(name) => {
                backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(name)
            }
        };
    }
    for parameter in type_parameters[..facts.type_parameter_len].iter_mut() {
        parameter.default = parameter.default.map(remap);
        if let backend_semantic::ir::ExtensionTypeParameterKind::ConstValue { value_type } =
            parameter.kind
        {
            parameter.kind = backend_semantic::ir::ExtensionTypeParameterKind::ConstValue {
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
                    TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                        backend_semantic::ir::TypeId::new(remap(raw)),
                    ))
                },
                name: facts.anonymous_child_names[base + offset],
                flags: facts.anonymous_child_flags[base + offset],
            };
        }
        type_pooled_cursor += child_count;
        type_facts[index] = TypeFactInput {
            owner: backend_semantic::ir::EntityId::new(facts.anonymous_owners[index]),
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
                    TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                        backend_semantic::ir::TypeId::new(remap(source.0)),
                    ))
                },
                name: source.1,
                flags: source.2,
            };
        }
        type_pooled_cursor += child_count;
        type_facts[anonymous_rows + ordinal] = TypeFactInput {
            owner: backend_semantic::ir::EntityId::new(ordinal as u32),
            record,
        };
    }
    let computed_rows = facts.computed_rows;
    let mut computed_facts = vec![
        TypeFactInput {
            owner: backend_semantic::ir::EntityId::new(0),
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
                    TypeChildTarget::Type(backend_semantic::ir::TypeRef::Local(
                        backend_semantic::ir::TypeId::new(remap(raw)),
                    ))
                },
                name: facts.computed_child_names[base + offset],
                flags: facts.computed_child_flags[base + offset],
            };
        }
        type_pooled_cursor += child_count;
        computed_facts[index] = TypeFactInput {
            owner: backend_semantic::ir::EntityId::new(facts.computed_owners[index]),
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
    let mut durable_type_parameter_ids = vec![None; fact_count].into_boxed_slice();
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
        durable_type_parameter_ids[ordinal] =
            Some(backend_semantic::ir::TypeParameterListId::new(list));
        durable_type_parameter_range_len += 1;
    }

    // Rust-only free-predicate rows and their per-declaration list table.
    // Staging subjects are rewritten through the one staged-type remapping;
    // bound runs reuse the shared remapped bound lane, so their ranges carry
    // over unchanged. Like type-parameter lists, every Rust extension keeps
    // its own list row, even when empty.
    let mut durable_free_predicates = vec![
        backend_semantic::ir::ExtensionFreePredicate {
            subject: 0,
            bounds: backend_semantic::ir::ExtensionTypeParameterBoundRange {
                start: 0,
                length: 0
            },
        };
        facts.free_predicate_len
    ]
    .into_boxed_slice();
    for (index, predicate) in facts.free_predicates[..facts.free_predicate_len]
        .iter()
        .enumerate()
    {
        durable_free_predicates[index] = backend_semantic::ir::ExtensionFreePredicate {
            subject: remap_staged_type(predicate.subject),
            bounds: predicate.bounds,
        };
    }
    let mut durable_free_predicate_ranges = vec![
        ExtensionTypeParameterRange {
            start: 0,
            length: 0,
        };
        fact_count
    ]
    .into_boxed_slice();
    let mut durable_free_predicate_ids = vec![None; fact_count].into_boxed_slice();
    let mut durable_free_predicate_range_len = 0_usize;
    for (ordinal, extension) in facts.extensions[..fact_count].iter().enumerate() {
        let Some(start) = extension.as_ref().and_then(extension_free_predicate_start) else {
            continue;
        };
        let Some(range) = facts.free_predicate_ranges[ordinal] else {
            return Err(AdmissionFault::ExtensionTypeParameters {
                row: ordinal,
                start: start.raw,
                length: 0,
                element_count: facts.free_predicate_len,
            });
        };
        let Some(end) = range.start.checked_add(range.length) else {
            return Err(AdmissionFault::ExtensionTypeParameters {
                row: ordinal,
                start: range.start,
                length: range.length,
                element_count: facts.free_predicate_len,
            });
        };
        if range.start != start.raw
            || usize::try_from(end).map_or(true, |end| end > facts.free_predicate_len)
        {
            return Err(AdmissionFault::ExtensionTypeParameters {
                row: ordinal,
                start: range.start,
                length: range.length,
                element_count: facts.free_predicate_len,
            });
        }
        durable_free_predicate_ranges[durable_free_predicate_range_len] =
            ExtensionTypeParameterRange {
                start: range.start,
                length: range.length,
            };
        let list = u32::try_from(durable_free_predicate_range_len).map_err(|_| {
            AdmissionFault::ExtensionTypeParameters {
                row: ordinal,
                start: range.start,
                length: range.length,
                element_count: facts.free_predicate_len,
            }
        })?;
        durable_free_predicate_ids[ordinal] =
            Some(backend_semantic::ir::FreePredicateListId::new(list));
        durable_free_predicate_range_len += 1;
    }

    // Language-extension section: dense per-plane fact pools plus their row
    // tables, with provisional atom coordinates rewritten to final lane
    // positions. Pooled lists keep provisional atom coordinates until the
    // pools lane rewrites them below.
    let extension_demand = ExtensionDemand::measure(&facts.extensions[..fact_count]);
    let mut typescript_pool = Vec::with_capacity(extension_demand.typescript);
    let mut csharp_pool = Vec::with_capacity(extension_demand.csharp);
    let mut go_pool = Vec::with_capacity(extension_demand.go);
    let mut rust_pool = Vec::with_capacity(extension_demand.rust);
    let mut python_pool = Vec::with_capacity(extension_demand.python);
    let mut java_pool = Vec::with_capacity(extension_demand.java);
    let mut clang_pool = Vec::with_capacity(extension_demand.clang);
    // One source image has one closed language authority. The selected plane
    // owns this aligned lane; the other six expose the same logical row count
    // over an empty universal-absence slice.
    let mut extension_rows = vec![SECTION_NONE; fact_count].into_boxed_slice();
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
                extension_rows[ordinal] = next_extension_ordinal(&typescript_pool);
                typescript_pool.push(rewritten);
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
                        backend_semantic::ir::SourceSpan::new(file, span.start(), span.end());
                }
                extension_rows[ordinal] = next_extension_ordinal(&csharp_pool);
                csharp_pool.push(rewritten);
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
                extension_rows[ordinal] = next_extension_ordinal(&go_pool);
                go_pool.push(rewritten);
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
                rewritten.free_predicates = durable_free_predicate_ids[ordinal].ok_or(
                    AdmissionFault::ExtensionTypeParameters {
                        row: ordinal,
                        start: value.free_predicates.raw,
                        length: 0,
                        element_count: facts.free_predicate_len,
                    },
                )?;
                extension_rows[ordinal] = next_extension_ordinal(&rust_pool);
                rust_pool.push(rewritten);
            }
            Some(EmissionExtension::Python(value)) => {
                extension_rows[ordinal] = next_extension_ordinal(&python_pool);
                python_pool.push(*value);
            }
            Some(EmissionExtension::Java(value)) => {
                extension_rows[ordinal] = next_extension_ordinal(&java_pool);
                java_pool.push(*value);
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
                extension_rows[ordinal] = next_extension_ordinal(&clang_pool);
                clang_pool.push(rewritten);
            }
            None => {}
        }
    }

    // Occurrence lane: every admitted reference fact, owner-relative, in
    // admission order.
    let mut occurrence_inputs = vec![
        OccurrenceInput {
            owner: backend_semantic::ir::EntityId::new(0),
            occurrence: Occurrence {
                target: backend_semantic::ir::OccurrenceTarget::Local(
                    backend_semantic::ir::EntityId::new(0)
                ),
                kind: backend_semantic::ir::ReferenceKind::FunctionCall,
                confidence: backend_semantic::ir::OccurrenceConfidence::Syntactic,
                span: backend_semantic::ir::RelSpan { start: 0, end: 0 },
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
            owner: backend_semantic::ir::EntityId::new(*owner),
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
    let mut atom_list_elements = Vec::with_capacity(facts.atom_list_len);
    for index in 0..facts.atom_list_len {
        let Some(row) = facts.atom_lists.row(index) else {
            return Err(AdmissionFault::ExtensionAtom {
                row: index,
                provisional: 0,
                atom_count: extension_atom_count,
            });
        };
        let mut mapped = Vec::with_capacity(row.len());
        for provisional in row {
            if *provisional as usize >= extension_atom_count {
                return Err(AdmissionFault::ExtensionAtom {
                    row: index,
                    provisional: *provisional,
                    atom_count: extension_atom_count,
                });
            }
            mapped.push((fact_count + *provisional as usize) as u32);
        }
        atom_list_elements.push(mapped);
    }
    let pooled_atom_lists = atom_list_elements
        .iter()
        .map(|row| ExtensionRefList { elements: row })
        .collect::<Vec<_>>();
    let mut type_list_elements = Vec::with_capacity(facts.type_list_len);
    for index in 0..facts.type_list_len {
        let Some(row) = facts.type_lists.row(index) else {
            return Err(AdmissionFault::ExtensionAtom {
                row: index,
                provisional: 0,
                atom_count: 0,
            });
        };
        type_list_elements.push(row.iter().copied().map(remap_staged_type).collect::<Vec<_>>());
    }
    let pooled_type_lists = type_list_elements
        .iter()
        .map(|row| ExtensionRefList { elements: row })
        .collect::<Vec<_>>();
    let mut pooled_entity_lists = Vec::with_capacity(facts.entity_list_len);
    for index in 0..facts.entity_list_len {
        let Some(row) = facts.entity_lists.row(index) else {
            return Err(AdmissionFault::ExtensionAtom {
                row: index,
                provisional: 0,
                atom_count: 0,
            });
        };
        pooled_entity_lists.push(ExtensionRefList { elements: row });
    }
    const EMPTY_FREE_PREDICATE_LISTS: [ExtensionTypeParameterRange; 1] =
        [ExtensionTypeParameterRange {
            start: 0,
            length: 0,
        }];
    let (durable_free_predicates, durable_free_predicate_lists): (
        &[backend_semantic::ir::ExtensionFreePredicate],
        &[ExtensionTypeParameterRange],
    ) = if durable_free_predicate_range_len == 0 {
        (&[], &EMPTY_FREE_PREDICATE_LISTS)
    } else {
        (
            &durable_free_predicates[..facts.free_predicate_len],
            &durable_free_predicate_ranges[..durable_free_predicate_range_len],
        )
    };
    let extension_pools = ExtensionPoolsLane {
        type_parameters: &type_parameters[..facts.type_parameter_len],
        type_parameter_bounds: &type_parameter_bounds[..facts.type_parameter_bound_len],
        type_parameter_lists: &durable_type_parameter_ranges[..durable_type_parameter_range_len],
        free_predicates: durable_free_predicates,
        free_predicate_lists: durable_free_predicate_lists,
        atom_lists: &pooled_atom_lists[..facts.atom_list_len],
        type_lists: &pooled_type_lists[..facts.type_list_len],
        entity_lists: &pooled_entity_lists[..facts.entity_list_len],
    };

    let extension_section = (any_extension).then(|| ExtensionSectionInput {
        authority: backend_semantic::ir::SemanticImageAuthority::Language(profile),
        typescript: ExtensionSectionPlane {
            entity_rows: fact_count,
            facts: &typescript_pool,
            row_ordinals: if typescript_pool.is_empty() {
                &[]
            } else {
                &extension_rows
            },
        },
        csharp: ExtensionSectionPlane {
            entity_rows: fact_count,
            facts: &csharp_pool,
            row_ordinals: if csharp_pool.is_empty() {
                &[]
            } else {
                &extension_rows
            },
        },
        go: ExtensionSectionPlane {
            entity_rows: fact_count,
            facts: &go_pool,
            row_ordinals: if go_pool.is_empty() {
                &[]
            } else {
                &extension_rows
            },
        },
        rust: ExtensionSectionPlane {
            entity_rows: fact_count,
            facts: &rust_pool,
            row_ordinals: if rust_pool.is_empty() {
                &[]
            } else {
                &extension_rows
            },
        },
        python: ExtensionSectionPlane {
            entity_rows: fact_count,
            facts: &python_pool,
            row_ordinals: if python_pool.is_empty() {
                &[]
            } else {
                &extension_rows
            },
        },
        java: ExtensionSectionPlane {
            entity_rows: fact_count,
            facts: &java_pool,
            row_ordinals: if java_pool.is_empty() {
                &[]
            } else {
                &extension_rows
            },
        },
        clang: ExtensionSectionPlane {
            entity_rows: fact_count,
            facts: &clang_pool,
            row_ordinals: if clang_pool.is_empty() {
                &[]
            } else {
                &extension_rows
            },
        },
    });

    let prepared = PreparedFragment::prepare_with_semantics(
        source,
        recipe,
        &entities[..fact_count],
        node_prefix,
        &atoms[..atom_count],
        backend_semantic::ir::FragmentSemantics {
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
fn compact_primitive_node(record: SemanticTypeRecord<'_>) -> Option<(usize, TypeNode)> {
    match builtin_type(record)? {
        BuiltinType::Bool => Some((
            0,
            TypeNode::Primitive(backend_semantic::ir::PrimitiveType::Bool),
        )),
        BuiltinType::I32 => Some((
            1,
            TypeNode::Primitive(backend_semantic::ir::PrimitiveType::I32),
        )),
        BuiltinType::String => Some((
            2,
            TypeNode::Primitive(backend_semantic::ir::PrimitiveType::String),
        )),
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

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
    AtomId, BuiltinType, ConcreteType, DocInput, EntityVersion, ExternalTarget, Ir, IrBuilder,
    ItemKind, LanguageExtensionInput, ListSpan, NominalRef, PayloadHash, PrimitiveShape,
    ProductChildRole, ProductChildren, ProductId, ProductListId, ProductRef, SemanticAtom,
    SemanticProduct, SemanticProductChild, SemanticProductConstructor, SemanticTypeChild,
    SemanticTypeFault, SemanticTypeRecord, SemanticTypeTag, StableEntityId, TreeItemInput,
    TreeLinkTarget, TupleElement, TupleElementKind, TypeChildTarget, TypeId, TypeWidth, Visibility,
};
use compiler_ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataResourceBudget, DataScratch,
    DocFactInput, DocFragmentInput, DocLinkTarget, EntityKind, EntityRecord, ExtensionPoolsLane,
    ExtensionRefList, ExtensionSectionInput, ExtensionSectionPlane, ExtensionTypeParameter,
    Occurrence, OccurrenceInput, OccurrenceLane, PrepareError, PreparedFragment, RecipeFact,
    SourceIdentity, TypeFactInput, TypeFactLane, TypeNode, WriteError,
    canonicalize_data_with_budget,
};
use core::mem::size_of;

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
pub(super) const MAX_EMISSION_FACTS: usize = 1024;
/// Dense bound of one fact's ordered product children.
pub(super) const MAX_FACT_CHILDREN: usize = 8;
/// Dense bound of one fact's ordered type-record children.
pub(super) const MAX_TYPE_CHILDREN: usize = 8;
/// Dense bound of the occurrence lane committed beside the declarations.
pub(super) const MAX_EMISSION_OCCURRENCES: usize = 1024;
/// Dense bound of the documentation lane committed beside the declarations.
pub(super) const MAX_EMISSION_DOC_FRAGMENTS: usize = 4096;
/// Dense bound of extension atoms admitted beside declaration names.
pub(super) const MAX_EXTENSION_ATOMS: usize = 2048;
/// Dense bound of pooled type parameters.
pub(super) const MAX_TYPE_PARAMETERS: usize = 512;
/// Dense bound of pooled reference lists per lane kind.
pub(super) const MAX_REF_LISTS: usize = 512;
/// Dense bound of one pooled reference list.
pub(super) const MAX_REF_LIST_ELEMENTS: usize = 16;
/// Total atom budget: one name per fact plus every extension atom.
pub(super) const MAX_EMISSION_ATOMS: usize = MAX_EMISSION_FACTS + MAX_EXTENSION_ATOMS;
/// Dense bound of anonymous type rows interned beside the fact rows.
pub(super) const MAX_ANONYMOUS_TYPE_ROWS: usize = 2048;
/// Dense bound of checker-computed type rows in the schema-2 segment.
pub(super) const MAX_COMPUTED_TYPE_ROWS: usize = 1024;
/// Total type-row budget: one record per fact plus the anonymous pool.
pub(super) const MAX_TYPE_ROWS: usize =
    MAX_EMISSION_FACTS + MAX_ANONYMOUS_TYPE_ROWS + MAX_COMPUTED_TYPE_ROWS;
/// First pool-local ordinal of an anonymous type row.
const ANONYMOUS_ROW_BASE: u32 = MAX_EMISSION_FACTS as u32;
/// First pool-local ordinal of a computed type row.
pub(super) const COMPUTED_ROW_BASE: u32 = ANONYMOUS_ROW_BASE + MAX_ANONYMOUS_TYPE_ROWS as u32;
/// Sentinel marking an absent row in an extension plane's row table.
const SECTION_NONE: u32 = u32::MAX;

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
                target: u32::MAX,
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

use crate::types::{FactFault, FactRejection};

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
    len: usize,
    total_children: usize,
    kinds: [EntityKind; MAX_EMISSION_FACTS],
    names: Box<[&'source [u8]; MAX_EMISSION_FACTS]>,
    type_records: Box<[SemanticTypeRecord<'source>; MAX_EMISSION_FACTS]>,
    type_child_targets: Box<[u32; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]>,
    type_child_names: Box<[Option<&'source [u8]>; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]>,
    type_child_flags: Box<[u8; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]>,
    type_child_counts: [u8; MAX_EMISSION_FACTS],
    total_type_children: usize,
    constructors: [SemanticProductConstructor; MAX_EMISSION_FACTS],
    child_roles: Box<[ProductChildRole; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN]>,
    child_targets: Box<[u32; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN]>,
    child_counts: [u8; MAX_EMISSION_FACTS],
    extensions: Box<[Option<EmissionExtension>; MAX_EMISSION_FACTS]>,
    parents: Box<[Option<u32>; MAX_EMISSION_FACTS]>,
    identity_lists: Box<[[u8; 16]; MAX_EMISSION_FACTS]>,
    key_digests: [u64; MAX_EMISSION_FACTS],
    visibility: [Visibility; MAX_EMISSION_FACTS],
    occurrence_owners: Box<[u32; MAX_EMISSION_OCCURRENCES]>,
    occurrences: Box<[Occurrence<'source>; MAX_EMISSION_OCCURRENCES]>,
    occurrence_len: usize,
    doc_facts: Box<[DocFactInput<'source>; MAX_EMISSION_DOC_FRAGMENTS]>,
    doc_len: usize,
    extension_atoms: Box<[&'source [u8]; MAX_EXTENSION_ATOMS]>,
    extension_atom_len: usize,
    type_parameters: Box<[ExtensionTypeParameter<'source>; MAX_TYPE_PARAMETERS]>,
    type_parameter_len: usize,
    atom_lists: Box<[[u32; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS]>,
    atom_list_lengths: [u8; MAX_REF_LISTS],
    atom_list_len: usize,
    type_lists: Box<[[u32; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS]>,
    type_list_lengths: [u8; MAX_REF_LISTS],
    type_list_len: usize,
    entity_lists: Box<[[u32; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS]>,
    entity_list_lengths: [u8; MAX_REF_LISTS],
    entity_list_len: usize,
    anonymous_records: Box<[SemanticTypeRecord<'source>; MAX_ANONYMOUS_TYPE_ROWS]>,
    anonymous_owners: Box<[u32; MAX_ANONYMOUS_TYPE_ROWS]>,
    anonymous_child_starts: Box<[u32; MAX_ANONYMOUS_TYPE_ROWS]>,
    anonymous_child_counts: Box<[u8; MAX_ANONYMOUS_TYPE_ROWS]>,
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

// The boxed lanes keep this caller-owned collector below the 64 KiB stack
// budget; each box is allocated exactly once by FactSet::new and lives with
// its FactSet, rather than being grown during admission.
const _: () = assert!(size_of::<FactSet<'static>>() <= 64 * 1024);

impl<'source> FactSet<'source> {
    pub(super) fn new() -> Self {
        Self {
            len: 0,
            total_children: 0,
            kinds: [EntityKind::Function; MAX_EMISSION_FACTS],
            names: Box::new([&[]; MAX_EMISSION_FACTS]),
            type_records: Box::new([opaque_record(); MAX_EMISSION_FACTS]),
            type_child_targets: Box::new([0; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]),
            type_child_names: Box::new([None; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]),
            type_child_flags: Box::new([0; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]),
            type_child_counts: [0; MAX_EMISSION_FACTS],
            total_type_children: 0,
            constructors: [SemanticProductConstructor::PRODUCT; MAX_EMISSION_FACTS],
            child_roles: Box::new(
                [ProductChildRole::ProductMember; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
            ),
            child_targets: Box::new([0; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN]),
            child_counts: [0; MAX_EMISSION_FACTS],
            extensions: Box::new([None; MAX_EMISSION_FACTS]),
            parents: Box::new([None; MAX_EMISSION_FACTS]),
            identity_lists: Box::new([[0; 16]; MAX_EMISSION_FACTS]),
            key_digests: [0; MAX_EMISSION_FACTS],
            visibility: [Visibility::Unknown; MAX_EMISSION_FACTS],
            occurrence_owners: Box::new([0; MAX_EMISSION_OCCURRENCES]),
            occurrences: Box::new(
                [Occurrence {
                    target: compiler_ir::OccurrenceTarget::Foreign(compiler_ir::ForeignKey {
                        origin: compiler_ir::ForeignOrigin::Universe { ecosystem: "" },
                        path: "",
                        display: "",
                        kind: None,
                    }),
                    kind: compiler_ir::ReferenceKind::FunctionCall,
                    confidence: compiler_ir::OccurrenceConfidence::Syntactic,
                    span: compiler_ir::RelSpan { start: 0, end: 0 },
                }; MAX_EMISSION_OCCURRENCES],
            ),
            occurrence_len: 0,
            doc_facts: Box::new(
                [DocFactInput {
                    owner: compiler_ir::EntityId::new(0),
                    fragment: DocFragmentInput::SoftBreak,
                }; MAX_EMISSION_DOC_FRAGMENTS],
            ),
            doc_len: 0,
            extension_atoms: Box::new([&[]; MAX_EXTENSION_ATOMS]),
            extension_atom_len: 0,
            type_parameters: Box::new(
                [ExtensionTypeParameter {
                    name: &[],
                    constraint: None,
                    default: None,
                }; MAX_TYPE_PARAMETERS],
            ),
            type_parameter_len: 0,
            atom_lists: Box::new([[0; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS]),
            atom_list_lengths: [0; MAX_REF_LISTS],
            atom_list_len: 0,
            type_lists: Box::new([[0; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS]),
            type_list_lengths: [0; MAX_REF_LISTS],
            type_list_len: 0,
            entity_lists: Box::new([[0; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS]),
            entity_list_lengths: [0; MAX_REF_LISTS],
            entity_list_len: 0,
            anonymous_records: Box::new([opaque_record(); MAX_ANONYMOUS_TYPE_ROWS]),
            anonymous_owners: Box::new([0; MAX_ANONYMOUS_TYPE_ROWS]),
            anonymous_child_starts: Box::new([0; MAX_ANONYMOUS_TYPE_ROWS]),
            anonymous_child_counts: Box::new([0; MAX_ANONYMOUS_TYPE_ROWS]),
            anonymous_child_targets: vec![0; MAX_ANONYMOUS_TYPE_ROWS * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            anonymous_child_names: vec![None; MAX_ANONYMOUS_TYPE_ROWS * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            anonymous_child_flags: vec![0; MAX_ANONYMOUS_TYPE_ROWS * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            anonymous_rows: 0,
            anonymous_children_total: 0,
            anonymous_child_pending: 0,
            computed_records: vec![opaque_record(); MAX_COMPUTED_TYPE_ROWS].into_boxed_slice(),
            computed_owners: vec![0; MAX_COMPUTED_TYPE_ROWS].into_boxed_slice(),
            computed_child_starts: vec![0; MAX_COMPUTED_TYPE_ROWS].into_boxed_slice(),
            computed_child_counts: vec![0; MAX_COMPUTED_TYPE_ROWS].into_boxed_slice(),
            computed_child_targets: vec![0; MAX_COMPUTED_TYPE_ROWS * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            computed_child_names: vec![None; MAX_COMPUTED_TYPE_ROWS * MAX_TYPE_CHILDREN]
                .into_boxed_slice(),
            computed_child_flags: vec![0; MAX_COMPUTED_TYPE_ROWS * MAX_TYPE_CHILDREN]
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

    /// Rank of one admitted fact among earlier facts with an identical
    /// identity key: zero for the first, one for the next, and so on.
    pub(super) fn key_rank(&self, ordinal: usize) -> u32 {
        let digest = self.key_digests[ordinal];
        self.key_digests[..ordinal]
            .iter()
            .filter(|known| **known == digest)
            .count() as u32
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
        if self.anonymous_rows == MAX_ANONYMOUS_TYPE_ROWS {
            return Err(FactFault::TypeRowCapacity);
        }
        let child_count = self.anonymous_child_pending;
        record
            .validate(child_count)
            .map_err(FactFault::TypeRecord)?;
        let row = ANONYMOUS_ROW_BASE + self.anonymous_rows as u32;
        let index = self.anonymous_rows;
        self.anonymous_records[index] = record;
        self.anonymous_owners[index] = owner;
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
        if self.anonymous_rows == MAX_ANONYMOUS_TYPE_ROWS {
            return Err(FactFault::TypeRowCapacity);
        }
        let child_count = self.anonymous_child_pending;
        record
            .validate(child_count)
            .map_err(FactFault::TypeRecord)?;
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
        let anonymous = target >= ANONYMOUS_ROW_BASE;
        let valid = if anonymous {
            target - ANONYMOUS_ROW_BASE < self.anonymous_rows as u32
        } else {
            target < self.len as u32
        };
        if !valid {
            return Err(FactFault::TypeChildTarget {
                position: self.anonymous_child_pending as usize,
                target,
                fact_count: self.len,
            });
        }
        if self.anonymous_child_pending == MAX_TYPE_CHILDREN as u32
            || self.anonymous_children_total == MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN
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
        if self.computed_rows == MAX_COMPUTED_TYPE_ROWS {
            return Err(FactFault::ComputedRowCapacity);
        }
        let child_count = self.computed_child_pending;
        record
            .validate(child_count)
            .map_err(FactFault::TypeRecord)?;
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
        let valid = if target >= COMPUTED_ROW_BASE {
            target - COMPUTED_ROW_BASE < self.computed_rows as u32
        } else if target >= ANONYMOUS_ROW_BASE {
            target - ANONYMOUS_ROW_BASE < self.anonymous_rows as u32
        } else {
            target < self.len as u32
        };
        if !valid {
            return Err(FactFault::TypeChildTarget {
                position: self.computed_child_pending as usize,
                target,
                fact_count: self.len,
            });
        }
        if self.computed_child_pending == MAX_TYPE_CHILDREN as u32
            || self.computed_children_total == MAX_COMPUTED_TYPE_ROWS * MAX_TYPE_CHILDREN
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
        self.extensions[ordinal] = Some(extension);
        Ok(())
    }

    /// Records the authority owner of one admitted declaration. Missing owners
    /// remain absent rather than being inferred from spelling or position.
    pub(super) fn set_parent(&mut self, ordinal: u32, parent: Option<u32>) {
        if let Some(slot) = self.parents.get_mut(ordinal as usize) {
            *slot = parent;
        }
    }

    /// Records one foreign override identity in the fact's dense Clang pool
    /// slot. All-zero is the pool's landed empty sentinel.
    pub(super) fn set_identity_list(&mut self, ordinal: u32, identity: Option<[u8; 16]>) {
        if let Some(slot) = self.identity_lists.get_mut(ordinal as usize) {
            *slot = identity.unwrap_or([0; 16]);
        }
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
        if self.extension_atom_len == MAX_EXTENSION_ATOMS {
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
        if count == MAX_REF_LISTS {
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
        for raw in constraint.iter().chain(default.iter()) {
            let fact = *raw < self.len as u32;
            let anonymous = *raw >= ANONYMOUS_ROW_BASE
                && *raw - ANONYMOUS_ROW_BASE < self.anonymous_rows as u32;
            if !fact && !anonymous {
                return Err(FactFault::RefTarget {
                    lane: "type_parameters",
                    raw: *raw,
                    fact_count: self.len,
                });
            }
        }
        if self.type_parameter_len == MAX_TYPE_PARAMETERS {
            return Err(FactFault::TypeParameterCapacity);
        }
        self.type_parameters[self.type_parameter_len] = ExtensionTypeParameter {
            name,
            constraint,
            default,
        };
        self.type_parameter_len += 1;
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
        if self.occurrence_len == MAX_EMISSION_OCCURRENCES {
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
        if self.doc_len == MAX_EMISSION_DOC_FRAGMENTS {
            return Err(FactFault::DocCapacity);
        }
        self.doc_facts[self.doc_len] = DocFactInput {
            owner: compiler_ir::EntityId::new(owner),
            fragment,
        };
        self.doc_len += 1;
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
    ) -> Result<Ir, compiler_ir::BuildError> {
        let fact_count = self.len;
        let mut builder = IrBuilder::new();
        builder.set_language_profile(profile)?;

        let empty_version = EntityVersion {
            stable: StableEntityId::from_raw([0; 16]),
            payload: PayloadHash::from_raw([0; 16]),
        };
        let mut versions = Box::new([empty_version; MAX_EMISSION_FACTS]);
        for (ordinal, version) in versions.iter_mut().take(fact_count).enumerate() {
            *version = fact_version(
                source,
                self.key_digests[ordinal],
                self.names[ordinal],
                self.key_rank(ordinal),
            );
        }
        let mut tree = builder.reserve_tree(&versions[..fact_count])?;
        let mut type_ids = [TypeId::new(0); MAX_TYPE_ROWS];
        let mut type_seen = [false; MAX_TYPE_ROWS];
        let mut semantic_types = Box::new([None; MAX_EMISSION_FACTS]);
        for (ordinal, semantic_type) in semantic_types.iter_mut().take(fact_count).enumerate() {
            *semantic_type = live_type(
                &mut tree,
                self,
                ordinal as u32,
                &mut type_ids,
                &mut type_seen,
                true,
            )?;
        }
        let mut docs = Box::new([DocInput::SoftBreak; MAX_EMISSION_DOC_FRAGMENTS]);
        let mut doc_ranges = Box::new([(0usize, 0usize); MAX_EMISSION_FACTS]);
        for ordinal in 0..fact_count {
            let start = match self.doc_facts[..self.doc_len]
                .iter()
                .position(|fact| fact.owner.raw as usize == ordinal)
            {
                Some(start) => start,
                None => self.doc_len,
            };
            let mut count = 0;
            for fact in self.doc_facts[..self.doc_len]
                .iter()
                .filter(|fact| fact.owner.raw as usize == ordinal)
            {
                if let Some(input) = doc_input(&mut tree, fact.fragment)?
                    && let Some(slot) = docs.get_mut(start + count)
                {
                    *slot = input;
                    count += 1;
                }
            }
            doc_ranges[ordinal] = (start, count);
        }
        // Rewrite FactSet-local TypeScript parameter starts into live list ids
        // before the tree commits its extension plane.
        let mut rewritten_typescript = Box::new([empty_typescript_facts(); MAX_EMISSION_FACTS]);
        for (ordinal, extension) in self.extensions[..fact_count].iter().enumerate() {
            let Some(EmissionExtension::TypeScript(value)) = extension else {
                continue;
            };
            let start = value.type_parameters.raw as usize;
            let end = self.extensions[..fact_count]
                .iter()
                .filter_map(|extension| match extension {
                    Some(EmissionExtension::TypeScript(value)) => {
                        let candidate = value.type_parameters.raw as usize;
                        (candidate > start).then_some(candidate)
                    }
                    _ => None,
                })
                .min()
                .unwrap_or(self.type_parameter_len);
            let parameters =
                self.type_parameters
                    .get(start..end)
                    .ok_or(compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::TypeParameters,
                        raw: value.type_parameters.raw,
                    })?;
            let mut live_parameters = [compiler_ir::TypeParameter {
                name: AtomId::new(0),
                constraint: None,
                default: None,
                variance: compiler_ir::Variance::Invariant,
                is_const: false,
            }; MAX_TYPE_PARAMETERS];
            for (index, parameter) in parameters.iter().enumerate() {
                live_parameters[index] = compiler_ir::TypeParameter {
                    name: tree.intern_atom(parameter.name)?,
                    constraint: parameter
                        .constraint
                        .map(|row| {
                            live_type(&mut tree, self, row, &mut type_ids, &mut type_seen, false)
                        })
                        .transpose()?
                        .flatten(),
                    default: parameter
                        .default
                        .map(|row| {
                            live_type(&mut tree, self, row, &mut type_ids, &mut type_seen, false)
                        })
                        .transpose()?
                        .flatten(),
                    variance: compiler_ir::Variance::Invariant,
                    is_const: false,
                };
            }
            let computed = value.computed.and_then(|id| {
                let row = id.erase().raw as usize;
                self.computed_records.get(row).and_then(|record| {
                    match (record.tag, PrimitiveShape::try_from(record.payload0)) {
                        (SemanticTypeTag::Primitive, Ok(PrimitiveShape::Str)) => Some(
                            tree.intern_computed(compiler_ir::ComputedType::KeyOf(TypeId::new(0))),
                        ),
                        (SemanticTypeTag::SelfType, _) => {
                            Some(tree.intern_computed(compiler_ir::ComputedType::This))
                        }
                        // Every other computed row carries the checker's
                        // derivation for its owning declaration; the live cell
                        // projects that relation truthfully as typeof owner.
                        _ => {
                            let owner = *self.computed_owners.get(row)?;
                            Some(tree.intern_computed(compiler_ir::ComputedType::TypeOf(
                                compiler_ir::TypeQuery::Entity(compiler_ir::EntityId::new(owner)),
                            )))
                        }
                    }
                })
            });
            let computed = computed.transpose()?;
            let declared = value
                .declared
                .map(|id| {
                    live_type(
                        &mut tree,
                        self,
                        id.raw,
                        &mut type_ids,
                        &mut type_seen,
                        false,
                    )
                })
                .transpose()?
                .flatten();
            rewritten_typescript[ordinal] = compiler_ir::TypeScriptFacts {
                type_parameters: tree
                    .intern_type_parameters(&live_parameters[..parameters.len()])?,
                declared,
                computed,
                ..*value
            };
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
        let mut items = Box::new([empty_item; MAX_EMISSION_FACTS]);
        let mut parents = Box::new([None; MAX_EMISSION_FACTS]);
        let mut member_ids: Box<[compiler_ir::TreeEntityId]> =
            vec![compiler_ir::TreeEntityId::new(0); MAX_EMISSION_FACTS * MAX_EMISSION_FACTS]
                .into_boxed_slice();
        let mut member_counts = Box::new([0_usize; MAX_EMISSION_FACTS]);
        for ordinal in 0..fact_count {
            parents[ordinal] = self.parents[ordinal].map(compiler_ir::TreeEntityId::new);
            let is_container = matches!(
                self.kinds[ordinal],
                EntityKind::Record | EntityKind::Enum | EntityKind::Module
            );
            if !is_container {
                continue;
            }
            for member in 0..fact_count {
                if self.parents[member] == Some(ordinal as u32) {
                    let base = ordinal * MAX_EMISSION_FACTS;
                    member_ids[base + member_counts[ordinal]] =
                        compiler_ir::TreeEntityId::new(member as u32);
                    member_counts[ordinal] += 1;
                }
            }
        }
        for (ordinal, item) in items.iter_mut().take(fact_count).enumerate() {
            let base = ordinal * MAX_EMISSION_FACTS;
            let extension = match self.extensions[ordinal].as_ref() {
                Some(EmissionExtension::TypeScript(_)) => Some(LanguageExtensionInput::TypeScript(
                    &rewritten_typescript[ordinal],
                )),
                // Other lanes retain provisional atom coordinates until fragment admission.
                Some(_) | None => None,
            };
            *item = TreeItemInput {
                name: self.names[ordinal],
                kind: item_kind(self.kinds[ordinal]),
                visibility: self.visibility[ordinal],
                parent: parents[ordinal],
                semantic_type: semantic_types[ordinal],
                members: &member_ids[base..base + member_counts[ordinal]],
                docs: &docs[doc_ranges[ordinal].0..doc_ranges[ordinal].0 + doc_ranges[ordinal].1],
                attributes: &[],
                source: None,
                extension,
            };
        }
        tree.commit(&items[..fact_count], &[])?;
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
        if fact_ordinal == MAX_EMISSION_FACTS {
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
                .validate_child(position as u32, &wire_child)
                .map_err(|fault| rejected(FactFault::TypeChild { position, fault }))?;
            if child.target != u32::MAX {
                let anonymous = child.target >= ANONYMOUS_ROW_BASE;
                let valid = if anonymous {
                    child.target - ANONYMOUS_ROW_BASE < self.anonymous_rows as u32
                } else {
                    child.target < fact_ordinal as u32
                };
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
                reason = "positions are bounded by MAX_FACT_CHILDREN (8) and always fit the u32 role coordinate"
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
        self.type_child_counts[fact_ordinal] = fact.type_child_count;
        self.constructors[fact_ordinal] = fact.constructor;
        self.child_counts[fact_ordinal] = fact.child_count;
        self.extensions[fact_ordinal] = fact.extension;
        self.visibility[fact_ordinal] = fact.visibility;
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
        Ok(PrimitiveShape::Char) if record.payload1 == 0 => Some(BuiltinType::Char),
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
const UNRESOLVED_EXTERNAL_REASON: u32 = {
    #[expect(
        clippy::as_conversions,
        reason = "TypeReason is a repr(u32) lattice with frozen wire discriminants"
    )]
    {
        compiler_ir::TypeReason::UnresolvedExternal as u32
    }
};

fn doc_input<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    fragment: DocFragmentInput<'source>,
) -> Result<Option<DocInput<'source>>, compiler_ir::BuildError> {
    match fragment {
        DocFragmentInput::Text(bytes) => Ok(core::str::from_utf8(bytes).ok().map(DocInput::Text)),
        DocFragmentInput::Code(bytes) => Ok(core::str::from_utf8(bytes).ok().map(DocInput::Code)),
        DocFragmentInput::Link { label, target } => Ok(Some(DocInput::Link {
            label: match core::str::from_utf8(label) {
                Ok(value) => value,
                Err(_) => return Ok(None),
            },
            target: match target {
                DocLinkTarget::Local(id) => {
                    TreeLinkTarget::Local(compiler_ir::TreeEntityId::new(id.raw))
                }
                DocLinkTarget::Foreign { path, .. } => {
                    let path_id = tree.intern_atom(path)?;
                    let display_id = tree.intern_atom(path)?;
                    TreeLinkTarget::External(tree.intern_external(ExternalTarget {
                        stable: StableEntityId::from_canonical_bytes(path),
                        package: None,
                        path: path_id,
                        display: display_id,
                        kind: None,
                    })?)
                }
            },
        })),
        DocFragmentInput::SoftBreak => Ok(Some(DocInput::SoftBreak)),
        DocFragmentInput::HardBreak => Ok(Some(DocInput::HardBreak)),
    }
}

fn live_type<'source>(
    tree: &mut compiler_ir::TreeBuilder<'_, '_>,
    facts: &FactSet<'source>,
    row: u32,
    ids: &mut [TypeId; MAX_TYPE_ROWS],
    seen: &mut [bool; MAX_TYPE_ROWS],
    top_level: bool,
) -> Result<Option<TypeId>, compiler_ir::BuildError> {
    let index = row as usize;
    if seen[index] {
        return Ok(Some(ids[index]));
    }
    let record = if index < facts.len {
        facts.type_records[index]
    } else {
        facts.anonymous_records[index - ANONYMOUS_ROW_BASE as usize]
    };
    let top_level_excluded = index < facts.len && record.tag == SemanticTypeTag::Unknown;
    if top_level_excluded && top_level {
        return Ok(None);
    }
    let child = |position: usize| -> Option<(u32, Option<&'source [u8]>, u8)> {
        if index < facts.len {
            let start = facts.type_children_base(index);
            (position < facts.type_child_counts[index] as usize)
                .then(|| facts.type_child_flat(start + position))
        } else {
            let anonymous = index - ANONYMOUS_ROW_BASE as usize;
            (position < facts.anonymous_child_counts[anonymous] as usize).then(|| {
                let start = facts.anonymous_child_starts[anonymous] as usize;
                (
                    facts.anonymous_child_targets[start + position],
                    facts.anonymous_child_names[start + position],
                    facts.anonymous_child_flags[start + position],
                )
            })
        }
    };
    let mut children = [TypeId::new(0); MAX_TYPE_CHILDREN];
    let child_count = if index < facts.len {
        facts.type_child_counts[index] as usize
    } else {
        facts.anonymous_child_counts[index - ANONYMOUS_ROW_BASE as usize] as usize
    };
    for position in 0..child_count {
        children[position] = live_type(
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
            false,
        )?
        .ok_or(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Type,
            raw: row,
        })?;
    }
    let ty = match record.tag {
        SemanticTypeTag::Primitive => match PrimitiveShape::try_from(record.payload0) {
            Ok(PrimitiveShape::Bool) => tree
                .intern_concrete(ConcreteType::Builtin(BuiltinType::Bool))?
                .erase(),
            Ok(PrimitiveShape::Char) => tree
                .intern_concrete(ConcreteType::Builtin(BuiltinType::Char))?
                .erase(),
            Ok(PrimitiveShape::Str) => tree
                .intern_concrete(ConcreteType::Builtin(BuiltinType::String))?
                .erase(),
            Ok(PrimitiveShape::Integer) => match (record.payload1 >> 1, record.payload1 & 1) {
                (TypeWidth::ARCH_FLAG, 1) => tree
                    .intern_concrete(ConcreteType::Builtin(BuiltinType::Int))?
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
                    .intern_unknown(compiler_ir::UnknownType::Unsupported)?
                    .erase(),
            },
            Ok(PrimitiveShape::Builtin) if record.text == Some(b"None") => tree
                .intern_concrete(ConcreteType::Builtin(BuiltinType::None_))?
                .erase(),
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
                    .intern_unknown(compiler_ir::UnknownType::Unsupported)?
                    .erase(),
            },
            Ok(PrimitiveShape::Reference) => {
                let lifetime = record
                    .text
                    .map(|bytes| tree.intern_atom(bytes))
                    .transpose()?;
                tree.intern_concrete(ConcreteType::Reference {
                    target: children[0],
                    mutability: if record.payload1 == SemanticTypeRecord::INTEGER_SIGNED_FLAG {
                        compiler_ir::Mutability::Mutable
                    } else {
                        compiler_ir::Mutability::Immutable
                    },
                    lifetime,
                })?
                .erase()
            }
            Ok(PrimitiveShape::MutPointer | PrimitiveShape::ConstPointer) => tree
                .intern_concrete(ConcreteType::Pointer {
                    target: children[0],
                    mutability: if record.payload0 == PrimitiveShape::MutPointer as u32 {
                        compiler_ir::Mutability::Mutable
                    } else {
                        compiler_ir::Mutability::Immutable
                    },
                })?
                .erase(),
            _ => tree
                .intern_unknown(compiler_ir::UnknownType::Unsupported)?
                .erase(),
        },
        SemanticTypeTag::Never => tree
            .intern_concrete(ConcreteType::Builtin(BuiltinType::Never))?
            .erase(),
        SemanticTypeTag::SelfType | SemanticTypeTag::TypeVar => {
            let spelling = match record.text {
                Some(spelling) => spelling,
                None => b"Self",
            };
            let atom = tree.intern_atom(spelling)?;
            tree.intern_concrete(ConcreteType::Parameter(atom))?.erase()
        }
        SemanticTypeTag::Nominal => tree
            .intern_concrete(ConcreteType::Nominal(match record.nominal {
                Some(NominalRef::Local(id)) => id,
                _ => {
                    return Ok(Some(
                        tree.intern_unknown(compiler_ir::UnknownType::Unsupported)?
                            .erase(),
                    ));
                }
            }))?
            .erase(),
        SemanticTypeTag::Tuple => {
            if child_count == 0 {
                tree.intern_concrete(ConcreteType::Builtin(BuiltinType::Unit))?
                    .erase()
            } else {
                let mut elements = [TupleElement {
                    label: None,
                    ty: children[0],
                    kind: TupleElementKind::Required,
                }; MAX_TYPE_CHILDREN];
                for position in 0..child_count {
                    elements[position].ty = children[position];
                }
                let list = tree.intern_tuple_elements(&elements[..child_count])?;
                tree.intern_concrete(ConcreteType::Tuple(list))?.erase()
            }
        }
        SemanticTypeTag::Apply if child_count > 0 => {
            let arguments = tree.intern_types(&children[1..child_count])?;
            tree.intern_concrete(ConcreteType::Applied {
                constructor: children[0],
                arguments,
            })?
            .erase()
        }
        SemanticTypeTag::Slice if child_count == 1 => tree
            .intern_concrete(ConcreteType::Slice(children[0]))?
            .erase(),
        SemanticTypeTag::Array if child_count == 1 => {
            let length = record
                .text
                .map(|bytes| tree.intern_atom(bytes))
                .transpose()?;
            tree.intern_concrete(ConcreteType::Array {
                element: children[0],
                length,
            })?
            .erase()
        }
        SemanticTypeTag::Union => {
            let list = tree.intern_types(&children[..child_count])?;
            tree.intern_concrete(ConcreteType::Union(list))?.erase()
        }
        SemanticTypeTag::Intersection | SemanticTypeTag::DynTrait | SemanticTypeTag::ImplTrait => {
            let list = tree.intern_types(&children[..child_count])?;
            tree.intern_concrete(ConcreteType::Intersection(list))?
                .erase()
        }
        SemanticTypeTag::FunctionPointer => {
            let has_result =
                child_count > 0 && record.payload1 & SemanticTypeRecord::RESULT_FLAG != 0;
            let parameter_count = child_count - usize::from(has_result);
            let mut elements = [TupleElement {
                label: None,
                ty: children[0],
                kind: TupleElementKind::Required,
            }; MAX_TYPE_CHILDREN];
            for position in 0..parameter_count {
                let (target, child_name, _) =
                    child(position).ok_or(compiler_ir::BuildError::Dangling {
                        space: compiler_ir::SemanticSpace::Type,
                        raw: row,
                    })?;
                let target = target as usize;
                let name = child_name.or_else(|| (target < facts.len).then(|| facts.names[target]));
                elements[position] = TupleElement {
                    label: name.map(|name| tree.intern_atom(name)).transpose()?,
                    ty: children[position],
                    kind: TupleElementKind::Required,
                };
            }
            let parameters = tree.intern_tuple_elements(&elements[..parameter_count])?;
            tree.intern_concrete(ConcreteType::Function {
                parameters,
                result: has_result.then_some(children[child_count - 1]),
                abi: None,
                variadic: false,
                unsafe_: false,
            })?
            .erase()
        }
        SemanticTypeTag::Unknown if record.payload0 == UNRESOLVED_EXTERNAL_REASON => {
            let spelling = match record.text {
                Some(spelling) => spelling,
                None => b"unresolved",
            };
            let path = tree.intern_atom(spelling)?;
            let external = tree.intern_external(ExternalTarget {
                stable: StableEntityId::from_canonical_bytes(spelling),
                package: None,
                path,
                display: path,
                kind: None,
            })?;
            tree.intern_concrete(ConcreteType::External(external))?
                .erase()
        }
        SemanticTypeTag::Unknown => tree
            .intern_unknown(compiler_ir::UnknownType::Unsupported)?
            .erase(),
        _ => tree
            .intern_unknown(compiler_ir::UnknownType::Unsupported)?
            .erase(),
    };
    ids[index] = ty;
    seen[index] = true;
    Ok(Some(ty))
}

/// Digests the identity-bearing key of one fact: kind, name, constructor
/// payload, declared-type record cells, and every ordered child coordinate.
fn fact_key_digest(fact: &SemanticFact<'_>) -> u64 {
    let mut preimage = [0u8; 64];
    preimage[0] = fact.kind as u8;
    preimage[1] = u32::from(fact.constructor.tag) as u8;
    preimage[2..6].copy_from_slice(&fact.constructor.payload0.to_le_bytes());
    preimage[6..10].copy_from_slice(&fact.constructor.payload1.to_le_bytes());
    let mut cursor = 10;
    preimage[cursor] = u8::from(fact.type_record.tag);
    cursor += 1;
    preimage[cursor..cursor + 4].copy_from_slice(&fact.type_record.payload0.to_le_bytes());
    cursor += 4;
    for child in fact.children.iter().take(usize::from(fact.child_count)) {
        if cursor + 8 <= preimage.len() {
            preimage[cursor] = u8::from(child.role);
            preimage[cursor + 4..cursor + 8].copy_from_slice(&child.target.to_le_bytes());
            cursor += 8;
        }
    }
    let name_digest = PayloadHash::from_canonical_bytes(fact.name);
    let stable = StableEntityId::from_canonical_bytes(&preimage[..cursor]);
    let mix = u64::from(stable.as_bytes()[0]) << 56
        | u64::from(name_digest.as_bytes()[0]) << 48
        | u64::from(stable.as_bytes()[1]) << 40
        | u64::from(name_digest.as_bytes()[1]) << 32
        | u64::from(stable.as_bytes()[2]) << 24
        | u64::from(name_digest.as_bytes()[2]) << 16
        | u64::from(stable.as_bytes()[3]) << 8
        | u64::from(name_digest.as_bytes()[3]);
    mix
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

/// Derives the stable declaration identity of one admitted fact from its
/// complete semantic key — source identity, kind, name, constructor payload,
/// declared-type record, and every ordered child coordinate — plus the rank
/// of earlier facts carrying an identical key. Genuine overloads differ in
/// constructor or type cells, so they keep distinct identities; adding an
/// unrelated declaration never moves another declaration's identity.
fn fact_version(
    source: compiler_ir::SourceIdentity,
    key_digest: u64,
    name: &[u8],
    rank: u32,
) -> EntityVersion {
    let mut identity_input = [0; 44];
    let (source_input, remainder) = identity_input.split_at_mut(32);
    let (key_input, rank_input) = remainder.split_at_mut(8);
    source_input.copy_from_slice(source.identity.as_ref());
    key_input.copy_from_slice(&key_digest.to_le_bytes());
    rank_input.copy_from_slice(&rank.to_le_bytes());
    EntityVersion {
        stable: StableEntityId::from_canonical_bytes(&identity_input),
        payload: PayloadHash::from_canonical_bytes(name),
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
}

/// Conservative canonicalization reservation derived exactly from the bounded
/// emission lane. With `P = 1,024` products and `C = P * 8 = 8,192` children:
/// `hash = 2*P² = 2,097,152`; `sort = P² + (P²+2P)*P = 1,076,887,552`;
/// `probe = P² = 1,048,576`; `child = 2*C*P + C*(sort+probe) =
/// 8,830,469,537,792`; and `lane = 4P+C = 12,288`. Thus `work = hash + sort
/// + probe + child + lane = 8,831,549,583,360`. All products fit in `u64`
/// (the largest is `C*(sort+probe)`, below 2^64), and these are the exact
/// worst-case formulas used by `compiler-ir::reserve_budget`: the maximal lane
/// is admitted and any measured overrun remains a typed `BudgetExceeded`.
#[expect(
    clippy::as_conversions,
    reason = "the widened lane bounds fit totally in u32/u64 reservation counters"
)]
const EMISSION_BUDGET: DataResourceBudget = DataResourceBudget {
    max_refinement_rounds: MAX_EMISSION_FACTS as u32,
    max_hash_evaluations: EMISSION_HASH_RESERVATION,
    max_sort_comparisons: EMISSION_SORT_RESERVATION,
    max_intern_probes: EMISSION_PROBE_RESERVATION,
    max_work: EMISSION_HASH_RESERVATION
        + EMISSION_SORT_RESERVATION
        + EMISSION_PROBE_RESERVATION
        + EMISSION_CHILD_VISIT_RESERVATION
        + EMISSION_LANE_VISIT_RESERVATION,
};

/// P = maximal products = maximal semantic atoms.
const EMISSION_PRODUCTS: u64 = MAX_EMISSION_FACTS as u64;
/// C = maximal pooled product children.
const EMISSION_CHILDREN: u64 = (MAX_EMISSION_FACTS * MAX_FACT_CHILDREN) as u64;
/// Every product is hashed twice per refinement round.
const EMISSION_HASH_RESERVATION: u64 = 2 * EMISSION_PRODUCTS * EMISSION_PRODUCTS;
/// Atom sort plus per-round product structural sorts.
const EMISSION_SORT_RESERVATION: u64 = EMISSION_PRODUCTS * EMISSION_PRODUCTS
    + (EMISSION_PRODUCTS * EMISSION_PRODUCTS + 2 * EMISSION_PRODUCTS) * EMISSION_PRODUCTS;
/// Open-addressing probes at the worst-case fill.
const EMISSION_PROBE_RESERVATION: u64 = EMISSION_PRODUCTS * EMISSION_PRODUCTS;
/// Refinement passes plus keyed comparisons visit the pooled child lane.
const EMISSION_CHILD_VISIT_RESERVATION: u64 = 2 * EMISSION_CHILDREN * EMISSION_PRODUCTS
    + EMISSION_CHILDREN
        * ((EMISSION_PRODUCTS * EMISSION_PRODUCTS + 2 * EMISSION_PRODUCTS) * EMISSION_PRODUCTS
            + EMISSION_PROBE_RESERVATION);
/// Each admitted lane row is visited once while measuring work.
const EMISSION_LANE_VISIT_RESERVATION: u64 = 4 * EMISSION_PRODUCTS + EMISSION_CHILDREN;

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
        computed: None,
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
/// An empty set writes the exact schema-1 fragment without a semantic-data
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
    let any_opaque =
        (0..fact_count).any(|ordinal| builtin_type(facts.type_records[ordinal]).is_none());
    let mut nodes = [TypeNode::Reference(TypeId::new(0)); MAX_TYPE_NODES];
    let mut node_count = 0;
    let mut primitive_nodes = [0_usize; PRIMITIVE_NODE_CAPACITY];
    if any_opaque {
        // Documented opaque-type sentinel: the node references itself, which
        // no proven-primitive node ever does.
        nodes[0] = TypeNode::Reference(TypeId::new(0));
        node_count = 1;
    }
    let mut fact_type_nodes = Box::new([0_usize; MAX_EMISSION_FACTS]);
    for (ordinal, record) in facts.type_records[..fact_count].iter().enumerate() {
        fact_type_nodes[ordinal] = match builtin_type(*record) {
            None => 0,
            Some(builtin) => {
                let (code, node) = match builtin {
                    BuiltinType::Bool => (0, TypeNode::Primitive(compiler_ir::PrimitiveType::Bool)),
                    BuiltinType::I32 => (1, TypeNode::Primitive(compiler_ir::PrimitiveType::I32)),
                    _ => (2, TypeNode::Primitive(compiler_ir::PrimitiveType::String)),
                };
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
    let mut entities = Box::new(
        [EntityRecord {
            semantic_type: TypeId::new(0),
            name: AtomId::new(0),
            kind: EntityKind::Function,
        }; MAX_EMISSION_FACTS],
    );
    let mut atoms = Box::new([AtomInput { bytes: b"" }; MAX_EMISSION_ATOMS]);
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

    let mut fact_products = Box::new(
        [SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        }; MAX_EMISSION_FACTS],
    );
    let mut fact_lists = Box::new([ListSpan::<ProductChildren>::new(0, 0); MAX_EMISSION_FACTS]);
    let mut fact_children = Box::new(
        [SemanticProductChild {
            target: ProductRef::Local(ProductId::new(0)),
            role: ProductChildRole::ProductMember,
        }; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
    );
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

    let mut semantic_atoms = Box::new([SemanticAtom { bytes: b"" }; MAX_EMISSION_FACTS]);
    for (ordinal, name) in facts.names[..fact_count].iter().enumerate() {
        semantic_atoms[ordinal] = SemanticAtom { bytes: name };
    }
    let mut scratch_atom_order = Box::new([AtomId::new(0); MAX_EMISSION_FACTS]);
    let mut scratch_atom_map = Box::new([0_u32; MAX_EMISSION_FACTS]);
    let mut scratch_product_order = Box::new([ProductId::new(0); MAX_EMISSION_FACTS]);
    let mut scratch_product_map = Box::new([0_u32; MAX_EMISSION_FACTS]);
    let mut scratch_colors = Box::new([0_u32; MAX_EMISSION_FACTS]);
    let mut scratch_next_colors = Box::new([0_u32; MAX_EMISSION_FACTS]);
    let mut scratch_hashes = Box::new([0_u64; MAX_EMISSION_FACTS]);
    let mut scratch_next_hashes = Box::new([0_u64; MAX_EMISSION_FACTS]);
    let mut scratch_representatives = Box::new([ProductId::new(0); MAX_EMISSION_FACTS]);
    let mut scratch_intern_slots = Box::new([0_u64; MAX_EMISSION_FACTS]);
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
    let mut output_atoms = Box::new([SemanticAtom { bytes: b"" }; MAX_EMISSION_FACTS]);
    let mut output_products = Box::new(
        [SemanticProduct {
            head: AtomId::new(0),
            children: ProductListId::new(0),
        }; MAX_EMISSION_FACTS],
    );
    let mut output_constructors =
        Box::new([SemanticProductConstructor::PRODUCT; MAX_EMISSION_FACTS]);
    let mut output_lists = Box::new([ListSpan::<ProductChildren>::new(0, 0); MAX_EMISSION_FACTS]);
    let mut output_children = Box::new(
        [SemanticProductChild {
            target: ProductRef::Local(ProductId::new(0)),
            role: ProductChildRole::ProductMember,
        }; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
    );
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
        EMISSION_BUDGET,
    )
    .map_err(AdmissionFault::Canonical)?;

    let mut type_facts = vec![
        TypeFactInput {
            owner: compiler_ir::EntityId::new(0),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Unknown),
        };
        MAX_TYPE_ROWS
    ]
    .into_boxed_slice();
    let mut type_children = vec![
        SemanticTypeChild {
            target: TypeChildTarget::Text,
            name: None,
            flags: 0,
        };
        MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN
            + MAX_ANONYMOUS_TYPE_ROWS * MAX_TYPE_CHILDREN
            + MAX_COMPUTED_TYPE_ROWS * MAX_TYPE_CHILDREN
    ]
    .into_boxed_slice();
    let anonymous_rows = facts.anonymous_rows;
    // Lane order: anonymous rows first (topological by construction), then
    // fact rows — every fact-row child and anonymous child points backward.
    let mut remap = |target: u32| -> u32 {
        if target >= ANONYMOUS_ROW_BASE {
            target - ANONYMOUS_ROW_BASE
        } else {
            target + anonymous_rows as u32
        }
    };
    let mut type_parameters = Box::new(
        [ExtensionTypeParameter {
            name: &[],
            constraint: None,
            default: None,
        }; MAX_TYPE_PARAMETERS],
    );
    type_parameters[..facts.type_parameter_len]
        .copy_from_slice(&facts.type_parameters[..facts.type_parameter_len]);
    for parameter in type_parameters[..facts.type_parameter_len].iter_mut() {
        parameter.constraint = parameter.constraint.map(&mut remap);
        parameter.default = parameter.default.map(&mut remap);
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
            type_children[pooled] = SemanticTypeChild {
                target: TypeChildTarget::Type(compiler_ir::TypeRef::Local(
                    compiler_ir::TypeId::new(remap(facts.anonymous_child_targets[base + offset])),
                )),
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
                target: TypeChildTarget::Type(compiler_ir::TypeRef::Local(
                    compiler_ir::TypeId::new(remap(source.0)),
                )),
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
        MAX_COMPUTED_TYPE_ROWS
    ]
    .into_boxed_slice();
    let declared_count = anonymous_rows + fact_count;
    for (index, source_record) in facts.computed_records[..computed_rows].iter().enumerate() {
        let child_count = usize::from(facts.computed_child_counts[index]);
        let record = SemanticTypeRecord {
            children: ListSpan::new(type_pooled_cursor as u32, child_count as u32),
            ..*source_record
        };
        let base = facts.computed_child_starts[index] as usize;
        for offset in 0..child_count {
            let pooled = type_pooled_cursor + offset;
            let target = facts.computed_child_targets[base + offset];
            let target = if target >= COMPUTED_ROW_BASE {
                declared_count as u32 + target - COMPUTED_ROW_BASE
            } else {
                remap(target)
            };
            type_children[pooled] = SemanticTypeChild {
                target: TypeChildTarget::Type(compiler_ir::TypeRef::Local(
                    compiler_ir::TypeId::new(target),
                )),
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

    // Language-extension section: dense per-plane fact pools plus their row
    // tables, with provisional atom coordinates rewritten to final lane
    // positions. Pooled lists keep provisional atom coordinates until the
    // pools lane rewrites them below.
    let mut typescript_pool = Box::new([empty_typescript_facts(); MAX_EMISSION_FACTS]);
    let mut csharp_pool = Box::new([empty_csharp_facts(); MAX_EMISSION_FACTS]);
    let mut go_pool = Box::new([empty_go_facts(); MAX_EMISSION_FACTS]);
    let mut rust_pool = Box::new([empty_rust_facts(); MAX_EMISSION_FACTS]);
    let mut python_pool = Box::new([empty_python_facts(); MAX_EMISSION_FACTS]);
    let mut java_pool = Box::new([empty_java_facts(); MAX_EMISSION_FACTS]);
    let mut clang_pool = Box::new([empty_clang_facts(); MAX_EMISSION_FACTS]);
    let mut identity_lists = Box::new([[0_u8; 16]; MAX_EMISSION_FACTS]);
    let mut typescript_rows = Box::new([SECTION_NONE; MAX_EMISSION_FACTS]);
    let mut csharp_rows = Box::new([SECTION_NONE; MAX_EMISSION_FACTS]);
    let mut go_rows = Box::new([SECTION_NONE; MAX_EMISSION_FACTS]);
    let mut rust_rows = Box::new([SECTION_NONE; MAX_EMISSION_FACTS]);
    let mut python_rows = Box::new([SECTION_NONE; MAX_EMISSION_FACTS]);
    let mut java_rows = Box::new([SECTION_NONE; MAX_EMISSION_FACTS]);
    let mut clang_rows = Box::new([SECTION_NONE; MAX_EMISSION_FACTS]);
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
                typescript_pool[typescript_len] = *value;
                typescript_rows[ordinal] = typescript_len as u32;
                typescript_len += 1;
            }
            Some(EmissionExtension::CSharp(value)) => {
                let mut rewritten = *value;
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
                go_pool[go_len] = *value;
                go_rows[ordinal] = go_len as u32;
                go_len += 1;
            }
            Some(EmissionExtension::Rust(value)) => {
                rust_pool[rust_len] = *value;
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
                clang_pool[clang_len] = *value;
                identity_lists[clang_len] = facts.identity_lists[ordinal];
                clang_rows[ordinal] = clang_len as u32;
                clang_len += 1;
            }
            None => {}
        }
    }

    // Occurrence lane: every admitted reference fact, owner-relative, in
    // admission order.
    let mut occurrence_inputs = Box::new(
        [OccurrenceInput {
            owner: compiler_ir::EntityId::new(0),
            occurrence: Occurrence {
                target: compiler_ir::OccurrenceTarget::Local(compiler_ir::EntityId::new(0)),
                kind: compiler_ir::ReferenceKind::FunctionCall,
                confidence: compiler_ir::OccurrenceConfidence::Syntactic,
                span: compiler_ir::RelSpan { start: 0, end: 0 },
            },
        }; MAX_EMISSION_OCCURRENCES],
    );
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
    let mut atom_list_elements = Box::new([[0; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS]);
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
    let mut pooled_atom_lists = [ExtensionRefList { elements: &[] }; MAX_REF_LISTS];
    for (index, length) in facts.atom_list_lengths[..facts.atom_list_len]
        .iter()
        .enumerate()
    {
        pooled_atom_lists[index] = ExtensionRefList {
            elements: &atom_list_elements[index][..usize::from(*length)],
        };
    }
    let mut pooled_type_lists = [ExtensionRefList { elements: &[] }; MAX_REF_LISTS];
    for (index, length) in facts.type_list_lengths[..facts.type_list_len]
        .iter()
        .enumerate()
    {
        pooled_type_lists[index] = ExtensionRefList {
            elements: &facts.type_lists[index][..usize::from(*length)],
        };
    }
    let mut pooled_entity_lists = [ExtensionRefList { elements: &[] }; MAX_REF_LISTS];
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
        atom_lists: &pooled_atom_lists[..facts.atom_list_len],
        type_lists: &pooled_type_lists[..facts.type_list_len],
        entity_lists: &pooled_entity_lists[..facts.entity_list_len],
        identity_lists: &identity_lists[..clang_len],
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

fn write_prepared<'output>(
    prepared: Result<PreparedFragment<'_>, PrepareError>,
    output: &'output mut [u8],
) -> Result<&'output [u8], AdmissionFault> {
    let prepared = prepared.map_err(AdmissionFault::Prepare)?;
    prepared.write_into(output).map_err(AdmissionFault::Write)
}

#[cfg(test)]
mod tests;

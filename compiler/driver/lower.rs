//! Defines lower behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//!
//! Occurrence admission is deliberately deferred to the integration phase and
//! remains owned by compiler/ir/semantic_facts.rs. Direct authorities emit
//! declaration facts into this one canonical lane; unsupported authorities
//! return typed terminals instead of inspecting source text here.
use crate::types::LoweringUnsupported;
use compiler_ir::DocumentationLane;
use compiler_ir::{
    AtomId, BuiltinType, ConcreteType, EntityVersion, Ir, IrBuilder, ItemKind, ListSpan,
    NominalRef, PayloadHash, PrimitiveShape, ProductChildRole, ProductChildren,
    ProductConstructorFault, ProductId, ProductListId, ProductRef, SemanticAtom, SemanticProduct,
    SemanticProductChild, SemanticProductConstructor, SemanticTypeChild, SemanticTypeFault,
    SemanticTypeRecord, SemanticTypeTag, StableEntityId, TreeItemInput, TypeChildTarget, TypeId,
    Visibility,
};
use compiler_ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataResourceBudget, DataScratch,
    DocFactInput, DocFragmentInput, EntityKind, EntityRecord, ExtensionPoolsLane, ExtensionRefList,
    ExtensionSectionInput, ExtensionSectionPlane, ExtensionTypeParameter, Occurrence,
    OccurrenceInput, OccurrenceLane, PrepareError, PreparedFragment, RecipeFact, SourceIdentity,
    TypeFactInput, TypeFactLane, TypeNode, WriteError, canonicalize_data_with_budget,
    encode_fragment_extension_section, fragment_extension_section_len,
};

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
pub(super) const MAX_EMISSION_FACTS: usize = 128;
/// Dense bound of one fact's ordered product children.
pub(super) const MAX_FACT_CHILDREN: usize = 8;
/// Dense bound of one fact's ordered type-record children.
pub(super) const MAX_TYPE_CHILDREN: usize = 8;
/// Dense bound of the occurrence lane committed beside the declarations.
pub(super) const MAX_EMISSION_OCCURRENCES: usize = 128;
/// Dense bound of the documentation lane committed beside the declarations.
pub(super) const MAX_EMISSION_DOC_FRAGMENTS: usize = 256;
/// Dense bound of extension atoms admitted beside declaration names.
pub(super) const MAX_EXTENSION_ATOMS: usize = 384;
/// Dense bound of pooled type parameters.
pub(super) const MAX_TYPE_PARAMETERS: usize = 128;
/// Dense bound of pooled reference lists per lane kind.
pub(super) const MAX_REF_LISTS: usize = 128;
/// Dense bound of one pooled reference list.
pub(super) const MAX_REF_LIST_ELEMENTS: usize = 16;
/// Total atom budget: one name per fact plus every extension atom.
pub(super) const MAX_EMISSION_ATOMS: usize = MAX_EMISSION_FACTS + MAX_EXTENSION_ATOMS;
/// Dense bound of anonymous type rows interned beside the fact rows.
pub(super) const MAX_ANONYMOUS_TYPE_ROWS: usize = 256;
/// Total type-row budget: one record per fact plus the anonymous pool.
pub(super) const MAX_TYPE_ROWS: usize = MAX_EMISSION_FACTS + MAX_ANONYMOUS_TYPE_ROWS;
/// First pool-local ordinal of an anonymous type row.
const ANONYMOUS_ROW_BASE: u32 = MAX_EMISSION_FACTS as u32;
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
        }
    }

    /// Commits the declared-type record and its borrowed children.
    #[must_use]
    pub(super) const fn typed(mut self, record: SemanticTypeRecord<'source>) -> Self {
        self.type_record = record;
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

/// Exact emission-lane rejection cause, retaining every operand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FactFault {
    /// The emitted declaration name is empty.
    EmptyName,
    /// The bounded fact lane already holds [`MAX_EMISSION_FACTS`] facts.
    Capacity,
    /// The fact's child lane overflowed [`MAX_FACT_CHILDREN`].
    ChildCapacity,
    /// The constructor payload disagrees with the fact's child count.
    Constructor(ProductConstructorFault),
    /// The child role at this position differs from the constructor's closed
    /// role lane.
    ChildRole {
        position: usize,
        expected: ProductChildRole,
        actual: ProductChildRole,
    },
    /// The child targets a fact ordinal outside the already-pushed set.
    ChildTarget {
        position: usize,
        target: u32,
        fact_count: usize,
    },
    /// The declared-type record violates the closed lattice.
    TypeRecord(SemanticTypeFault),
    /// One type-record child violates its tag's closed child law.
    TypeChild {
        position: usize,
        fault: SemanticTypeFault,
    },
    /// A type-record child targets a fact ordinal that is not strictly
    /// backward.
    TypeChildTarget {
        position: usize,
        target: u32,
        fact_count: usize,
    },
    /// The type-record child lane overflowed [`MAX_TYPE_CHILDREN`].
    TypeChildCapacity,
    /// The anonymous type-row pool overflowed [`MAX_ANONYMOUS_TYPE_ROWS`].
    TypeRowCapacity,
    /// An occurrence names an owner outside the pushed prefix.
    OccurrenceOwner { owner: u32, fact_count: usize },
    /// The bounded occurrence lane is full.
    OccurrenceCapacity,
    /// A doc fragment names an owner outside the pushed prefix.
    DocOwner { owner: u32, fact_count: usize },
    /// The bounded documentation lane is full.
    DocCapacity,
    /// The bounded extension-atom lane is full.
    ExtensionAtomCapacity,
    /// The bounded type-parameter lane is full.
    TypeParameterCapacity,
    /// A pooled reference lane is full.
    RefListCapacity,
    /// One pooled reference list overflows [`MAX_REF_LIST_ELEMENTS`].
    RefListElements,
    /// A pooled reference targets a fact outside the pushed prefix.
    RefTarget {
        lane: &'static str,
        raw: u32,
        fact_count: usize,
    },
}

/// Exact rejection of one fact at admission, retaining the offending ordinal,
/// its exact name bytes, and the typed cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RejectedFact<'source> {
    pub(super) fact: usize,
    pub(super) name: &'source [u8],
    pub(super) cause: FactFault,
}

/// Caller-owned bounded SoA lanes for the ordered emission set. Only the
/// admitted prefix is read by [`admit`]; slots past `len` are never observed.
pub(super) struct FactSet<'source> {
    len: usize,
    total_children: usize,
    kinds: [EntityKind; MAX_EMISSION_FACTS],
    names: [&'source [u8]; MAX_EMISSION_FACTS],
    type_records: [SemanticTypeRecord<'source>; MAX_EMISSION_FACTS],
    type_child_targets: [u32; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN],
    type_child_names: Box<[Option<&'source [u8]>; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]>,
    type_child_flags: [u8; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN],
    type_child_counts: [u8; MAX_EMISSION_FACTS],
    total_type_children: usize,
    constructors: [SemanticProductConstructor; MAX_EMISSION_FACTS],
    child_roles: [ProductChildRole; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
    child_targets: [u32; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
    child_counts: [u8; MAX_EMISSION_FACTS],
    extensions: [Option<EmissionExtension>; MAX_EMISSION_FACTS],
    key_digests: [u64; MAX_EMISSION_FACTS],
    occurrence_owners: Box<[u32; MAX_EMISSION_OCCURRENCES]>,
    occurrences: Box<[Occurrence<'source>; MAX_EMISSION_OCCURRENCES]>,
    occurrence_len: usize,
    doc_facts: Box<[DocFactInput<'source>; MAX_EMISSION_DOC_FRAGMENTS]>,
    doc_len: usize,
    extension_atoms: [&'source [u8]; MAX_EXTENSION_ATOMS],
    extension_atom_len: usize,
    type_parameters: [ExtensionTypeParameter<'source>; MAX_TYPE_PARAMETERS],
    type_parameter_len: usize,
    atom_lists: [[u32; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS],
    atom_list_lengths: [u8; MAX_REF_LISTS],
    atom_list_len: usize,
    type_lists: [[u32; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS],
    type_list_lengths: [u8; MAX_REF_LISTS],
    type_list_len: usize,
    entity_lists: [[u32; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS],
    entity_list_lengths: [u8; MAX_REF_LISTS],
    entity_list_len: usize,
    anonymous_records: Box<[SemanticTypeRecord<'source>; MAX_ANONYMOUS_TYPE_ROWS]>,
    anonymous_owners: [u32; MAX_ANONYMOUS_TYPE_ROWS],
    anonymous_child_starts: [u32; MAX_ANONYMOUS_TYPE_ROWS],
    anonymous_child_counts: [u8; MAX_ANONYMOUS_TYPE_ROWS],
    anonymous_child_targets: [u32; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN],
    anonymous_child_names: Box<[Option<&'source [u8]>; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]>,
    anonymous_child_flags: [u8; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN],
    anonymous_rows: usize,
    anonymous_children_total: usize,
    anonymous_child_pending: u32,
}

impl<'source> FactSet<'source> {
    pub(super) fn new() -> Self {
        Self {
            len: 0,
            total_children: 0,
            kinds: [EntityKind::Function; MAX_EMISSION_FACTS],
            names: [&[]; MAX_EMISSION_FACTS],
            type_records: [opaque_record(); MAX_EMISSION_FACTS],
            type_child_targets: [0; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN],
            type_child_names: Box::new([None; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]),
            type_child_flags: [0; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN],
            type_child_counts: [0; MAX_EMISSION_FACTS],
            total_type_children: 0,
            constructors: [SemanticProductConstructor::PRODUCT; MAX_EMISSION_FACTS],
            child_roles: [ProductChildRole::ProductMember; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
            child_targets: [0; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
            child_counts: [0; MAX_EMISSION_FACTS],
            extensions: [None; MAX_EMISSION_FACTS],
            key_digests: [0; MAX_EMISSION_FACTS],
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
            extension_atoms: [&[]; MAX_EXTENSION_ATOMS],
            extension_atom_len: 0,
            type_parameters: [ExtensionTypeParameter {
                name: &[],
                constraint: None,
                default: None,
            }; MAX_TYPE_PARAMETERS],
            type_parameter_len: 0,
            atom_lists: [[0; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS],
            atom_list_lengths: [0; MAX_REF_LISTS],
            atom_list_len: 0,
            type_lists: [[0; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS],
            type_list_lengths: [0; MAX_REF_LISTS],
            type_list_len: 0,
            entity_lists: [[0; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS],
            entity_list_lengths: [0; MAX_REF_LISTS],
            entity_list_len: 0,
            anonymous_records: Box::new([opaque_record(); MAX_ANONYMOUS_TYPE_ROWS]),
            anonymous_owners: [0; MAX_ANONYMOUS_TYPE_ROWS],
            anonymous_child_starts: [0; MAX_ANONYMOUS_TYPE_ROWS],
            anonymous_child_counts: [0; MAX_ANONYMOUS_TYPE_ROWS],
            anonymous_child_targets: [0; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN],
            anonymous_child_names: Box::new([None; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN]),
            anonymous_child_flags: [0; MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN],
            anonymous_rows: 0,
            anonymous_children_total: 0,
            anonymous_child_pending: 0,
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
            if *raw >= self.len as u32 {
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
        let mut versions = [empty_version; MAX_EMISSION_FACTS];
        for (ordinal, version) in versions.iter_mut().take(fact_count).enumerate() {
            *version = fact_version(
                source,
                self.key_digests[ordinal],
                self.names[ordinal],
                self.key_rank(ordinal),
            );
        }
        let mut tree = builder.reserve_tree(&versions[..fact_count])?;
        let mut semantic_types = [None; MAX_EMISSION_FACTS];
        for (ordinal, semantic_type) in semantic_types.iter_mut().take(fact_count).enumerate() {
            if let Some(builtin) = builtin_type(self.type_records[ordinal]) {
                *semantic_type = Some(
                    tree.intern_concrete(ConcreteType::Builtin(builtin))?
                        .erase(),
                );
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
        let mut items = [empty_item; MAX_EMISSION_FACTS];
        for (ordinal, item) in items.iter_mut().take(fact_count).enumerate() {
            *item = TreeItemInput {
                name: self.names[ordinal],
                kind: item_kind(self.kinds[ordinal]),
                visibility: Visibility::Unknown,
                parent: None,
                semantic_type: semantic_types[ordinal],
                members: &[],
                docs: &[],
                attributes: &[],
                source: None,
                extension: None,
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
        Ok(PrimitiveShape::Integer)
            if record.payload1 == (32u32 << 1) | SemanticTypeRecord::INTEGER_SIGNED_FLAG =>
        {
            Some(BuiltinType::I32)
        }
        Ok(PrimitiveShape::Str) if record.payload1 == 0 => Some(BuiltinType::String),
        _ => None,
    }
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
/// emission lane: at most `MAX_EMISSION_FACTS` products and atoms and at most
/// `MAX_EMISSION_FACTS * MAX_FACT_CHILDREN` pooled children. Each limit is the
/// worst-case reservation formula of compiler-ir `reserve_budget`, so the
/// maximal lane is always admitted and any measured overrun remains a typed
/// `BudgetExceeded` rather than a mid-run capacity surprise.
#[expect(
    clippy::as_conversions,
    reason = "the lane bounds 128 and 1024 widen totally to u32/u64 reservation counters"
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
/// the lane's compact-recipe capacity keeps the exact closed terminal.
pub(super) fn push_fact<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<usize, LoweringUnsupported> {
    match facts.push(fact) {
        Ok(ordinal) => Ok(ordinal),
        Err(_) => Err(LoweringUnsupported::NoSupportedDeclaration),
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
    let mut fact_type_nodes = [0_usize; MAX_EMISSION_FACTS];
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
    let mut entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }; MAX_EMISSION_FACTS];
    let mut atoms = [AtomInput { bytes: b"" }; MAX_EMISSION_ATOMS];
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

    let mut fact_products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }; MAX_EMISSION_FACTS];
    let mut fact_lists = [ListSpan::<ProductChildren>::new(0, 0); MAX_EMISSION_FACTS];
    let mut fact_children = [SemanticProductChild {
        target: ProductRef::Local(ProductId::new(0)),
        role: ProductChildRole::ProductMember,
    }; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN];
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

    let mut semantic_atoms = [SemanticAtom { bytes: b"" }; MAX_EMISSION_FACTS];
    for (ordinal, name) in facts.names[..fact_count].iter().enumerate() {
        semantic_atoms[ordinal] = SemanticAtom { bytes: name };
    }
    let mut scratch_atom_order = [AtomId::new(0); MAX_EMISSION_FACTS];
    let mut scratch_atom_map = [0_u32; MAX_EMISSION_FACTS];
    let mut scratch_product_order = [ProductId::new(0); MAX_EMISSION_FACTS];
    let mut scratch_product_map = [0_u32; MAX_EMISSION_FACTS];
    let mut scratch_colors = [0_u32; MAX_EMISSION_FACTS];
    let mut scratch_next_colors = [0_u32; MAX_EMISSION_FACTS];
    let mut scratch_hashes = [0_u64; MAX_EMISSION_FACTS];
    let mut scratch_next_hashes = [0_u64; MAX_EMISSION_FACTS];
    let mut scratch_representatives = [ProductId::new(0); MAX_EMISSION_FACTS];
    let mut scratch_intern_slots = [0_u64; MAX_EMISSION_FACTS];
    let mut data_scratch = DataScratch {
        atom_order: &mut scratch_atom_order,
        atom_to_canonical: &mut scratch_atom_map,
        product_order: &mut scratch_product_order,
        product_to_canonical: &mut scratch_product_map,
        colors: &mut scratch_colors,
        next_colors: &mut scratch_next_colors,
        hashes: &mut scratch_hashes,
        next_hashes: &mut scratch_next_hashes,
        product_representatives: &mut scratch_representatives,
        intern_slots: &mut scratch_intern_slots,
    };
    let mut output_atoms = [SemanticAtom { bytes: b"" }; MAX_EMISSION_FACTS];
    let mut output_products = [SemanticProduct {
        head: AtomId::new(0),
        children: ProductListId::new(0),
    }; MAX_EMISSION_FACTS];
    let mut output_constructors = [SemanticProductConstructor::PRODUCT; MAX_EMISSION_FACTS];
    let mut output_lists = [ListSpan::<ProductChildren>::new(0, 0); MAX_EMISSION_FACTS];
    let mut output_children = [SemanticProductChild {
        target: ProductRef::Local(ProductId::new(0)),
        role: ProductChildRole::ProductMember,
    }; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN];
    let mut data_output = DataOutput {
        atoms: &mut output_atoms,
        products: &mut output_products,
        constructors: &mut output_constructors,
        lists: &mut output_lists,
        children: &mut output_children,
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

    let mut type_facts = Box::new(
        [TypeFactInput {
            owner: compiler_ir::EntityId::new(0),
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Unknown),
        }; MAX_TYPE_ROWS],
    );
    let mut type_children = Box::new(
        [SemanticTypeChild {
            target: TypeChildTarget::Text,
            name: None,
            flags: 0,
        };
            MAX_EMISSION_FACTS * MAX_TYPE_CHILDREN + MAX_ANONYMOUS_TYPE_ROWS * MAX_TYPE_CHILDREN],
    );
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
    let type_row_count = anonymous_rows + fact_count;
    let type_fact_lane = TypeFactLane {
        inputs: &type_facts[..type_row_count],
        children: &type_children[..type_pooled_cursor],
    };

    // Language-extension section: dense per-plane fact pools plus their row
    // tables, with provisional atom coordinates rewritten to final lane
    // positions. Pooled lists keep provisional atom coordinates until the
    // pools lane rewrites them below.
    let mut typescript_pool = [empty_typescript_facts(); MAX_EMISSION_FACTS];
    let mut csharp_pool = [empty_csharp_facts(); MAX_EMISSION_FACTS];
    let mut go_pool = [empty_go_facts(); MAX_EMISSION_FACTS];
    let mut rust_pool = [empty_rust_facts(); MAX_EMISSION_FACTS];
    let mut python_pool = [empty_python_facts(); MAX_EMISSION_FACTS];
    let mut java_pool = [empty_java_facts(); MAX_EMISSION_FACTS];
    let mut clang_pool = [empty_clang_facts(); MAX_EMISSION_FACTS];
    let mut typescript_rows = [SECTION_NONE; MAX_EMISSION_FACTS];
    let mut csharp_rows = [SECTION_NONE; MAX_EMISSION_FACTS];
    let mut go_rows = [SECTION_NONE; MAX_EMISSION_FACTS];
    let mut rust_rows = [SECTION_NONE; MAX_EMISSION_FACTS];
    let mut python_rows = [SECTION_NONE; MAX_EMISSION_FACTS];
    let mut java_rows = [SECTION_NONE; MAX_EMISSION_FACTS];
    let mut clang_rows = [SECTION_NONE; MAX_EMISSION_FACTS];
    let mut typescript_len = 0_usize;
    let mut csharp_len = 0_usize;
    let mut go_len = 0_usize;
    let mut rust_len = 0_usize;
    let mut python_len = 0_usize;
    let mut java_len = 0_usize;
    let mut clang_len = 0_usize;
    let any_extension = facts.extensions[..fact_count].iter().any(Option::is_some);
    for (ordinal, extension) in facts.extensions[..fact_count].iter().enumerate() {
        let row = ordinal as u32;
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
    let mut atom_list_elements = [[0; MAX_REF_LIST_ELEMENTS]; MAX_REF_LISTS];
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
        type_parameters: &facts.type_parameters[..facts.type_parameter_len],
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

fn write_prepared<'output>(
    prepared: Result<PreparedFragment<'_>, PrepareError>,
    output: &'output mut [u8],
) -> Result<&'output [u8], AdmissionFault> {
    let prepared = prepared.map_err(AdmissionFault::Prepare)?;
    prepared.write_into(output).map_err(AdmissionFault::Write)
}

#[cfg(test)]
mod tests;

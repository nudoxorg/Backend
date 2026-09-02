//! Defines lower behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//!
//! Occurrence admission is deliberately deferred to the integration phase and
//! remains owned by `compiler/ir/semantic_facts.rs`. These interim scanner
//! collectors emit declaration facts and type facts only; real frontends will
//! supply occurrence facts when the scanner retirement gate is closed.
use compiler_ir::{
    AtomId, ListSpan, PrimitiveShape, ProductChildRole, ProductChildren, ProductConstructorFault,
    ProductId, ProductListId, ProductRef, SemanticAtom, SemanticProduct, SemanticProductChild,
    SemanticProductConstructor, SemanticTypeRecord, SemanticTypeTag, TypeId,
};
use compiler_ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataResourceBudget, DataScratch,
    EntityKind, EntityRecord, PrepareError, PreparedFragment, PrimitiveType, RecipeFact,
    SourceIdentity, TypeFactInput, TypeNode, WriteError, canonicalize_data_with_budget,
};
use compiler_vocabulary::Language;

use crate::types::LoweringUnsupported;

mod clang;
mod csharp;
mod go;
mod java;
mod python;
mod rust;
mod scanner;
pub(crate) mod typescript;

pub(crate) fn java_top_level_type_name(source: &[u8]) -> Option<&[u8]> {
    scanner::java_top_level_type_name(source)
}

/// One declaration projected into the current rich-IR compatibility path.
pub(super) struct Declaration<'source> {
    pub(super) name: &'source [u8],
    pub(super) kind: EntityKind,
    pub(super) semantic_type: Option<PrimitiveType>,
}

/// Projects the first fully proven declaration without weakening the richer
/// compact fact collector used by durable publication.
pub(super) fn declaration<'source>(
    language: Language,
    source: &'source [u8],
) -> Result<Declaration<'source>, LoweringUnsupported> {
    let mut facts = FactSet::new();
    let mut unsupported = UnsupportedLane::new();
    emit(language, source, &mut facts, &mut unsupported)?;
    let semantic_type = match facts.fact_types[0] {
        FactType::Primitive(primitive) => Some(primitive),
        FactType::Opaque => None,
    };
    Ok(Declaration {
        name: facts.names[0],
        kind: facts.kinds[0],
        semantic_type,
    })
}

/// Dense bound of the multi-declaration semantic emission lane.
///
/// One LowerIr fragment admits at most this many provable declaration facts;
/// a source with more declarations is a typed lane rejection, never a
/// truncated emission. The bound also fixes every canonicalization scratch,
/// output, and resource reservation below.
pub(super) const MAX_EMISSION_FACTS: usize = 128;
/// Dense bound of one fact's ordered product children.
pub(super) const MAX_FACT_CHILDREN: usize = 8;

/// Exact reason a recognized declaration form stayed outside this slice's
/// provable emission set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum UnsupportedReason {
    /// The declaration's value type is proven but outside the closed
    /// primitive recipe, so the compact fact would guess.
    ClosedValueType,
    /// The declaration form needs the real language frontend's binding
    /// authority before any compact fact would be honest.
    NeedsFrontend,
}

/// One recognized declaration that this slice provably cannot lower,
/// retaining its exact source name and the exact reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct UnsupportedDeclaration<'source> {
    pub(super) name: &'source [u8],
    pub(super) reason: UnsupportedReason,
}

/// Caller-owned bounded lane of unsupported declarations.
pub(super) struct UnsupportedLane<'source> {
    len: usize,
    overflowed: bool,
    names: [&'source [u8]; MAX_EMISSION_FACTS],
    reasons: [UnsupportedReason; MAX_EMISSION_FACTS],
}

impl<'source> UnsupportedLane<'source> {
    #[allow(
        clippy::indexing_slicing,
        reason = "the initializer literals fill every fixed lane element exactly; no dynamic index exists at construction"
    )]
    pub(super) const fn new() -> Self {
        Self {
            len: 0,
            overflowed: false,
            names: [&[]; MAX_EMISSION_FACTS],
            reasons: [UnsupportedReason::NeedsFrontend; MAX_EMISSION_FACTS],
        }
    }

    /// Records one unsupported declaration. A full lane keeps the first
    /// [`MAX_EMISSION_FACTS`] records; overflow is impossible before the fact
    /// lane rejects the same source.
    #[allow(
        clippy::indexing_slicing,
        reason = "the record ordinal is admitted below MAX_EMISSION_FACTS before the fixed slot writes"
    )]
    pub(super) fn record(&mut self, declaration: UnsupportedDeclaration<'source>) {
        let ordinal = self.len;
        if ordinal == MAX_EMISSION_FACTS {
            self.overflowed = true;
            return;
        }
        self.names[ordinal] = declaration.name;
        self.reasons[ordinal] = declaration.reason;
        self.len = ordinal + 1;
    }
}

/// Closed declared-type fact for one emission lane row.
///
/// `Primitive` commits the exact closed primitive spelled at the declaration.
/// `Opaque` commits the lane's documented sentinel for shape declarations
/// (records, modules, traits, functions with unannotated results) whose
/// declared type is outside the compact primitive recipe: the fragment's
/// semantic product carries their structure, and the sentinel never claims a
/// primitive the source did not spell.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FactType {
    Primitive(PrimitiveType),
    Opaque,
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
    fact_type: FactType,
    constructor: SemanticProductConstructor,
    children: [FactChild; MAX_FACT_CHILDREN],
    child_count: u8,
    truncated: bool,
}

impl<'source> SemanticFact<'source> {
    /// Creates the zero-child fact for one provable declaration.
    pub(super) const fn new(
        kind: EntityKind,
        name: &'source [u8],
        fact_type: FactType,
        constructor: SemanticProductConstructor,
    ) -> Self {
        Self {
            kind,
            name,
            fact_type,
            constructor,
            children: [FactChild {
                role: ProductChildRole::ProductMember,
                target: 0,
            }; MAX_FACT_CHILDREN],
            child_count: 0,
            truncated: false,
        }
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
    overflowed: bool,
    total_children: usize,
    kinds: [EntityKind; MAX_EMISSION_FACTS],
    names: [&'source [u8]; MAX_EMISSION_FACTS],
    fact_types: [FactType; MAX_EMISSION_FACTS],
    constructors: [SemanticProductConstructor; MAX_EMISSION_FACTS],
    child_roles: [ProductChildRole; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
    child_targets: [u32; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
    child_counts: [u8; MAX_EMISSION_FACTS],
}

impl<'source> FactSet<'source> {
    pub(super) const fn new() -> Self {
        Self {
            len: 0,
            overflowed: false,
            total_children: 0,
            kinds: [EntityKind::Function; MAX_EMISSION_FACTS],
            names: [&[]; MAX_EMISSION_FACTS],
            fact_types: [FactType::Opaque; MAX_EMISSION_FACTS],
            constructors: [SemanticProductConstructor::PRODUCT; MAX_EMISSION_FACTS],
            child_roles: [ProductChildRole::ProductMember; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
            child_targets: [0; MAX_EMISSION_FACTS * MAX_FACT_CHILDREN],
            child_counts: [0; MAX_EMISSION_FACTS],
        }
    }

    /// Number of admitted facts.
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    pub(super) const fn overflowed(&self) -> bool {
        self.overflowed
    }

    /// Borrows the first authority-admitted declaration name, when present.
    pub(super) fn first_name(&self) -> Option<&'source [u8]> {
        self.names
            .get(..self.len)
            .and_then(|names| names.first().copied())
    }

    /// Borrows the first authority-admitted declaration kind, when present.
    pub(super) fn first_kind(&self) -> Option<EntityKind> {
        if self.len == 0 {
            None
        } else {
            self.kinds.first().copied()
        }
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
        self.fact_types[fact_ordinal] = fact.fact_type;
        self.constructors[fact_ordinal] = fact.constructor;
        self.child_counts[fact_ordinal] = fact.child_count;
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

/// Exact canonicalization, preparation, or write failure of the emission
/// lane. Every fact-level invariant is already proven by [`FactSet::push`],
/// so the remaining terminals carry exact typed causes without fact operands.
#[derive(Debug)]
pub(super) enum AdmissionFault {
    Canonical(CanonicalDataError),
    Prepare(PrepareError),
    Write(WriteError),
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
        Err(_) => {
            facts.overflowed = true;
            Err(LoweringUnsupported::NoSupportedDeclaration)
        }
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
    output: &'output mut [u8],
) -> Result<&'output [u8], AdmissionFault> {
    if facts.len == 0 {
        let prepared = PreparedFragment::prepare(source, recipe, &[], &[], &[]);
        return write_prepared(prepared, output);
    }
    let fact_count = facts.len;

    // Type-node lane: the opaque sentinel (when any fact needs it) followed by
    // one node per distinct proven primitive in first-use order.
    let any_opaque = facts.fact_types[..fact_count].contains(&FactType::Opaque);
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
    for (ordinal, fact_type) in facts.fact_types[..fact_count].iter().enumerate() {
        fact_type_nodes[ordinal] = match *fact_type {
            FactType::Opaque => 0,
            FactType::Primitive(primitive) => {
                #[expect(
                    clippy::as_conversions,
                    reason = "primitive codes 0..=2 widen totally to the native sentinel-table width"
                )]
                let code = u32::from(primitive) as usize;
                if primitive_nodes[code] == 0 {
                    nodes[node_count] = TypeNode::Primitive(primitive);
                    node_count += 1;
                    primitive_nodes[code] = node_count;
                }
                primitive_nodes[code] - 1
            }
        };
    }
    let node_prefix = &nodes[..node_count];

    let mut entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Function,
    }; MAX_EMISSION_FACTS];
    let mut atoms = [AtomInput { bytes: b"" }; MAX_EMISSION_FACTS];
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

    let mut type_facts = [TypeFactInput {
        owner: compiler_ir::EntityId::new(0),
        record: SemanticTypeRecord::leaf(SemanticTypeTag::Unknown),
    }; MAX_EMISSION_FACTS];
    for (ordinal, type_fact) in type_facts.iter_mut().enumerate().take(fact_count) {
        let record = match facts.fact_types[ordinal] {
            FactType::Opaque => SemanticTypeRecord {
                tag: SemanticTypeTag::Unknown,
                payload0: 0,
                payload1: 0,
                text: None,
                text2: None,
                nominal: None,
                children: ListSpan::new(0, 0),
            },
            FactType::Primitive(primitive) => SemanticTypeRecord {
                tag: SemanticTypeTag::Primitive,
                payload0: match primitive {
                    PrimitiveType::Bool => u32::from(PrimitiveShape::Bool),
                    PrimitiveType::I32 => u32::from(PrimitiveShape::Integer),
                    PrimitiveType::String => u32::from(PrimitiveShape::Str),
                },
                payload1: if primitive == PrimitiveType::I32 {
                    32 << 1
                } else {
                    0
                },
                text: None,
                text2: None,
                nominal: None,
                children: ListSpan::new(0, 0),
            },
        };
        *type_fact = TypeFactInput {
            owner: compiler_ir::EntityId::new(ordinal as u32),
            record,
        };
    }
    let type_fact_lane = compiler_ir::TypeFactLane {
        inputs: &type_facts[..fact_count],
        children: &[],
    };

    let prepared = PreparedFragment::prepare_with_semantics(
        source,
        recipe,
        &entities[..fact_count],
        node_prefix,
        &atoms[..fact_count],
        compiler_ir::FragmentSemantics {
            data: Some(&semantic),
            occurrences: None,
            type_facts: Some(&type_fact_lane),
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

/// Lowers one source into the multi-declaration emission lane through the
/// per-language structural collectors.
///
/// Each collector emits every provable declaration fact and records every
/// recognized form it provably cannot lower. A source with no provable facts
/// keeps the exact closed no-declaration terminal.
pub(super) fn emit<'source>(
    language: Language,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) -> Result<(), LoweringUnsupported> {
    match language {
        Language::Rust => rust::collect(source, facts, unsupported)?,
        Language::Python => python::collect(source, facts, unsupported)?,
        Language::Clang => clang::collect(source, facts, unsupported)?,
        // TypeScript requires its exact closed source profile for grammar selection;
        // callers use `typescript::collect` directly rather than profile-erasing dispatch.
        Language::TypeScript => return Err(LoweringUnsupported::TypeScriptDeclarationForm),
        Language::CSharp => csharp::collect(source, facts, unsupported)?,
        Language::Go => go::collect(source, facts, unsupported)?,
        Language::Java => java::collect(source, facts, unsupported)?,
    }
    if facts.overflowed() || unsupported.overflowed {
        return Err(LoweringUnsupported::NoSupportedDeclaration);
    }
    if facts.len() == 0 {
        return Err(LoweringUnsupported::NoSupportedDeclaration);
    }
    Ok(())
}

#[cfg(test)]
mod tests;

//! Defines lower behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use compiler_ir::{
    AtomInput, CanonicalDataError, DataFacts, DataOutput, DataResourceBudget, DataScratch,
    EntityKind, EntityRecord, PrepareError, PreparedFragment, PrimitiveType, RecipeFact,
    SourceIdentity, TypeNode, WriteError, canonicalize_data_with_budget,
};
use compiler_ir::{
    AtomId, ListSpan, ProductChildRole, ProductChildren, ProductConstructorFault, ProductId,
    ProductListId, ProductRef, SemanticAtom, SemanticProduct, SemanticProductChild,
    SemanticProductConstructor, TypeId,
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
mod typescript;

pub(super) struct Declaration<'source> {
    pub(super) name: &'source [u8],
    pub(super) kind: EntityKind,
    pub(super) semantic_type: PrimitiveType,
}

pub(crate) fn java_top_level_type_name(source: &[u8]) -> Option<&[u8]> {
    scanner::java_top_level_type_name(source)
}

pub(super) fn declaration<'source>(
    language: Language,
    source: &'source [u8],
) -> Result<Declaration<'source>, LoweringUnsupported> {
    match language {
        Language::Rust => rust::declaration(source),
        Language::Python => python::declaration(source),
        Language::Clang => clang::declaration(source),
        Language::TypeScript => typescript::declaration(source),
        Language::CSharp => csharp::declaration(source),
        Language::Go => go::declaration(source),
        Language::Java => java::declaration(source),
    }
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
            reason = "the typed child input shape ships with the lane; per-language facts consume it in the next emission card"
        )
    )]
    #[allow(
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
    #[allow(
        clippy::indexing_slicing,
        reason = "the initializer literals fill every fixed lane element exactly; no dynamic index exists at construction"
    )]
    pub(super) const fn new() -> Self {
        Self {
            len: 0,
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
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "the admitted-count observation ships with the lane; its consumer grows with the per-language emission card"
        )
    )]
    pub(super) const fn len(&self) -> usize {
        self.len
    }

    /// Admits one fact after proving its name, its constructor payload against
    /// its child count, every child role against the constructor's closed role
    /// lane, and every child target against the already-pushed prefix.
    ///
    /// A rejected fact leaves every lane byte-for-byte unchanged and retains
    /// the offending ordinal, exact name bytes, and typed cause.
    #[allow(
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
            #[allow(
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
            #[allow(
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
#[allow(
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

/// Admits the ordered fact set into one prepared fragment.
///
/// An empty set writes the exact schema-1 fragment without a semantic-data
/// section. A populated set canonicalizes the facts' products, commits the
/// entities, type nodes, name atoms, and the optional semantic section, and
/// writes everything into the caller-owned output.
#[allow(
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
                #[allow(
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
        #[allow(
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
        #[allow(
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

    let prepared = PreparedFragment::prepare_with_data(
        source,
        recipe,
        &entities[..fact_count],
        node_prefix,
        &atoms[..fact_count],
        &semantic,
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

/// Lowers one source through the per-language declaration path into the
/// multi-declaration emission lane.
///
/// The single-declaration language paths remain the compat producers: each
/// emits at most one provable fact as a zero-child language-owned product,
/// and every unsupported form keeps its exact closed terminal.
pub(super) fn emit<'source>(
    language: Language,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), LoweringUnsupported> {
    let declaration = declaration(language, source)?;
    let fact = SemanticFact::new(
        declaration.kind,
        declaration.name,
        FactType::Primitive(declaration.semantic_type),
        SemanticProductConstructor::PRODUCT,
    );
    match facts.push(fact) {
        Ok(_) => Ok(()),
        // The compat path pushes exactly one parsed, nonempty identifier as a
        // zero-child product fact, so every rejection class is unreachable by
        // construction; the closed no-declaration terminal preserves the
        // public compile boundary without new surface.
        Err(_) => Err(LoweringUnsupported::NoSupportedDeclaration),
    }
}

#[cfg(test)]
mod tests;

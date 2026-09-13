//! Allocation-free semantic-data canonicalization.
//!
//! Local recursive products use a coinductive (bisimulation) quotient: two
//! products are equal when their atom bytes and ordered child signatures are
//! equal at the fixed point. Consequently, a one-node self-cycle and a
//! same-headed two-node cycle intentionally collapse, while differing atom
//! heads remain distinct. External product coordinates are never folded into
//! local product IDs.

#[allow(
    clippy::indexing_slicing,
    reason = "validate_facts and the scratch/output admission checks prove every lane position and span before the trusted indexing passes; repeating the bounds checks would duplicate the validator"
)]
mod accounting;
#[allow(
    clippy::indexing_slicing,
    reason = "scratch lanes are admitted to source-lane length before canonicalize_atoms indexes them"
)]
mod atoms;
mod error;
mod graph;
#[allow(
    clippy::indexing_slicing,
    reason = "intern slots, order lanes, and maps are admitted or derived from validated source ordinals before indexing"
)]
mod interning;
#[allow(
    clippy::indexing_slicing,
    reason = "validated product/list coordinates index source lanes and exact canonical prefixes index output lanes"
)]
mod output;
#[allow(
    clippy::indexing_slicing,
    reason = "refinement indexes scratch lanes admitted to source-lane length and validated product coordinates"
)]
mod refinement;
mod types;
#[allow(
    clippy::indexing_slicing,
    reason = "validation and output-layout passes index admitted scratch lanes and validated source coordinates"
)]
mod validation;

pub use error::{CanonicalDataError, DataCountLane, DataOutputLane, DataScratchLane};
pub use graph::CanonicalDataGraph;
pub use types::{
    DataCanonicalization, DataFacts, DataOutput, DataResource, DataResourceBudget, DataScratch,
};

use accounting::{Counters, enforce_budget, reserve_budget};
use atoms::{canonicalize_atoms, distinct_atom_count, write_atoms};
use error::{count, require_output, require_scratch};
use interning::intern_products;
use output::{max_output_child_count, output_extent, write_products};
use refinement::refine_products;
use validation::{validate_facts, validate_output_layout};

/// Prepare a graph with measured counters and an explicit resource budget.
pub fn canonicalize_data_with_budget<'output, 'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    scratch: &'output mut DataScratch<'output>,
    output: &'output mut DataOutput<'output, 'bytes>,
    budget: DataResourceBudget,
) -> Result<CanonicalDataGraph<'output, 'bytes>, CanonicalDataError> {
    let source_atom_count = count(DataCountLane::Atoms, facts.atoms.len())?;
    let source_product_count = count(DataCountLane::Products, facts.products.len())?;
    let source_constructor_count = count(DataCountLane::Constructors, facts.constructors.len())?;
    let source_list_count = count(DataCountLane::Lists, facts.lists.len())?;
    let source_child_count = count(DataCountLane::Children, facts.children.len())?;
    require_scratch(
        DataScratchLane::AtomOrder,
        facts.atoms.len(),
        scratch.atom_order.len(),
    )?;
    require_scratch(
        DataScratchLane::AtomMap,
        facts.atoms.len(),
        scratch.atom_to_canonical.len(),
    )?;
    require_scratch(
        DataScratchLane::ProductOrder,
        facts.products.len(),
        scratch.product_order.len(),
    )?;
    require_scratch(
        DataScratchLane::ProductMap,
        facts.products.len(),
        scratch.product_to_canonical.len(),
    )?;
    require_scratch(
        DataScratchLane::Colors,
        facts.products.len(),
        scratch.colors.len(),
    )?;
    require_scratch(
        DataScratchLane::NextColors,
        facts.products.len(),
        scratch.next_colors.len(),
    )?;
    require_scratch(
        DataScratchLane::Hashes,
        facts.products.len(),
        scratch.hashes.len(),
    )?;
    require_scratch(
        DataScratchLane::NextHashes,
        facts.products.len(),
        scratch.next_hashes.len(),
    )?;
    require_scratch(
        DataScratchLane::Representatives,
        facts.products.len(),
        scratch.product_representatives.len(),
    )?;
    if !facts.products.is_empty() {
        require_scratch(DataScratchLane::InternSlots, 1, scratch.intern_slots.len())?;
    }

    validate_facts(
        facts,
        source_atom_count,
        source_product_count,
        source_list_count,
    )?;

    // Admission is intentionally complete before the first scratch write.
    // The reservation is conservative for measured work and the output lanes
    // reserve source-cardinality upper bounds (with exact atom deduplication
    // and a span-sum upper bound for pooled children).  A rejected operation
    // therefore cannot leave an interner, order lane, or output prefix half
    // prepared for its caller.
    reserve_budget(facts, budget)?;
    let canonical_atom_count = distinct_atom_count(facts.atoms)?;
    let max_child_output = max_output_child_count(facts)?;
    require_output(
        DataOutputLane::Atoms,
        canonical_atom_count,
        output.atoms.len(),
    )?;
    require_output(
        DataOutputLane::Products,
        source_product_count,
        output.products.len(),
    )?;
    require_output(
        DataOutputLane::Constructors,
        source_product_count,
        output.constructors.len(),
    )?;
    require_output(
        DataOutputLane::Lists,
        source_product_count,
        output.lists.len(),
    )?;
    require_output(
        DataOutputLane::Children,
        max_child_output,
        output.children.len(),
    )?;
    if source_product_count > 0 && scratch.intern_slots.len() < facts.products.len() {
        return Err(CanonicalDataError::InternTableFull {
            product: crate::ir::ProductId::new(0),
            capacity: scratch.intern_slots.len(),
        });
    }

    let counters = Counters::new();
    let canonical_atom_count = canonicalize_atoms(facts.atoms, scratch, &counters)?;
    let refinement_rounds =
        refine_products(facts, scratch, &counters, &budget, source_product_count)?;
    enforce_budget(
        DataResource::SortComparisons,
        counters.sort_comparisons.get(),
        budget.max_sort_comparisons,
    )?;
    let canonical_product_count = intern_products(facts, scratch, &counters, &budget)?;
    let (canonical_list_count, canonical_child_count) = output_extent(
        facts,
        scratch.product_representatives,
        canonical_product_count,
    )?;
    require_output(
        DataOutputLane::Atoms,
        canonical_atom_count,
        output.atoms.len(),
    )?;
    require_output(
        DataOutputLane::Products,
        canonical_product_count,
        output.products.len(),
    )?;
    require_output(
        DataOutputLane::Constructors,
        canonical_product_count,
        output.constructors.len(),
    )?;
    require_output(
        DataOutputLane::Lists,
        canonical_list_count,
        output.lists.len(),
    )?;
    require_output(
        DataOutputLane::Children,
        canonical_child_count,
        output.children.len(),
    )?;
    let atom_count = usize::try_from(canonical_atom_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Atoms,
            actual: canonical_atom_count,
            source,
        }
    })?;
    let product_count = usize::try_from(canonical_product_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Products,
            actual: canonical_product_count,
            source,
        }
    })?;
    let list_count = usize::try_from(canonical_list_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Lists,
            actual: canonical_list_count,
            source,
        }
    })?;
    let child_count = usize::try_from(canonical_child_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Children,
            actual: canonical_child_count,
            source,
        }
    })?;
    validate_output_layout(
        facts,
        scratch,
        canonical_atom_count,
        canonical_product_count,
        canonical_list_count,
        canonical_child_count,
    )?;
    let work = counters.work(
        facts.atoms.len(),
        facts.products.len(),
        facts.constructors.len(),
        facts.lists.len(),
        facts.children.len(),
    )?;
    enforce_budget(DataResource::Work, work, budget.max_work)?;
    write_atoms(facts.atoms, scratch, output);
    write_products(facts, scratch, output, product_count);
    Ok(CanonicalDataGraph {
        atoms: &output.atoms[..atom_count],
        products: &output.products[..product_count],
        constructors: &output.constructors[..product_count],
        lists: &output.lists[..list_count],
        children: &output.children[..child_count],
        atom_map: &scratch.atom_to_canonical[..facts.atoms.len()],
        product_map: &scratch.product_to_canonical[..facts.products.len()],
        metrics: DataCanonicalization {
            source_atom_count,
            source_product_count,
            source_constructor_count,
            source_list_count,
            source_child_count,
            canonical_atom_count,
            canonical_product_count,
            canonical_constructor_count: canonical_product_count,
            canonical_list_count,
            canonical_child_count,
            refinement_rounds,
            sort_comparisons: counters.sort_comparisons.get(),
            hash_evaluations: counters.hash_evaluations.get(),
            intern_probes: counters.intern_probes.get(),
            work,
        },
    })
}

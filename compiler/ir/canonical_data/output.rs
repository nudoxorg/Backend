//! Defines output behavior for compiler-ir canonical data, whose purpose is to write canonical lanes exactly once.
//! This module owns the canonical output extent calculation and lane writes.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Canonical output extent calculation and lane writes.

use super::{
    error::{CanonicalDataError, DataCountLane, DataOutputLane},
    types::{DataFacts, DataOutput, DataScratch, lane_index},
};
use compiler_ir_vocabulary::{
    AtomId, ListSpan, ProductId, ProductListId, ProductRef, SemanticProduct, SemanticProductChild,
};

pub(super) fn output_extent<'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    representatives: &[ProductId],
    canonical_product_count: u32,
) -> Result<(u32, u32), CanonicalDataError> {
    let count = usize::try_from(canonical_product_count).map_err(|source| {
        CanonicalDataError::NativeCount {
            lane: DataCountLane::Products,
            actual: canonical_product_count,
            source,
        }
    })?;
    let mut child_count = 0_usize;
    for source in representatives.iter().copied().take(count) {
        let span = facts.lists[lane_index(facts.products[lane_index(source.raw)].children.raw)];
        child_count = child_count.checked_add(lane_index(span.length)).ok_or(
            CanonicalDataError::OutputLength {
                lane: DataOutputLane::Children,
                actual: usize::MAX,
            },
        )?;
    }
    let list_count = u32::try_from(count).map_err(|source| CanonicalDataError::Count {
        lane: DataCountLane::Lists,
        actual: count,
        source,
    })?;
    let child_count = u32::try_from(child_count).map_err(|source| CanonicalDataError::Count {
        lane: DataCountLane::Children,
        actual: child_count,
        source,
    })?;
    Ok((list_count, child_count))
}

/// Reserve enough pooled-child output for any possible set of canonical
/// representatives before the interner starts writing scratch.  A canonical
/// result can repeat overlapping source spans, so the source child count is
/// not a sufficient upper bound.  Summing every validated source span is
/// conservative and keeps the later exact extent calculation infallible.
pub(super) fn max_output_child_count(facts: DataFacts<'_, '_>) -> Result<u32, CanonicalDataError> {
    let mut child_count = 0_usize;
    for product in facts.products.iter().copied() {
        let span = facts.lists[lane_index(product.children.raw)];
        child_count = child_count.checked_add(lane_index(span.length)).ok_or(
            CanonicalDataError::OutputLength {
                lane: DataOutputLane::Children,
                actual: usize::MAX,
            },
        )?;
    }
    u32::try_from(child_count).map_err(|source| CanonicalDataError::Count {
        lane: DataCountLane::Children,
        actual: child_count,
        source,
    })
}

pub(super) fn write_products<'output, 'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    scratch: &DataScratch<'output>,
    output: &mut DataOutput<'output, 'bytes>,
    product_count: usize,
) {
    let mut child_cursor = 0_u32;
    let mut canonical = 0_u32;
    // The canonical coordinate is a `u32` product id, not a native loop
    // index, so `enumerate` would trade one scoped allow for a second
    // conversion on every row.
    #[allow(
        clippy::explicit_counter_loop,
        reason = "the canonical coordinate is a u32 dense id required by the pooled-list table"
    )]
    for source in scratch.product_representatives[..product_count]
        .iter()
        .copied()
    {
        let product = facts.products[lane_index(source.raw)];
        let atom = scratch.atom_to_canonical[lane_index(product.head.raw)];
        let span = facts.lists[lane_index(product.children.raw)];
        let start = lane_index(span.start);
        let length = lane_index(span.length);
        let end = start + length;
        let list_id = ProductListId::new(canonical);
        output.products[lane_index(canonical)] = SemanticProduct {
            head: AtomId::new(atom),
            children: list_id,
        };
        output.constructors[lane_index(canonical)] = facts.constructors[lane_index(source.raw)];
        output.lists[lane_index(canonical)] = ListSpan::new(child_cursor, span.length);
        for (offset, child) in facts.children[start..end].iter().copied().enumerate() {
            let target = match child.target {
                ProductRef::Local(target) => ProductRef::Local(ProductId::new(
                    scratch.product_to_canonical[lane_index(target.raw)],
                )),
                ProductRef::External(target) => ProductRef::External(target),
            };
            output.children[lane_index(child_cursor) + offset] = SemanticProductChild {
                target,
                role: child.role,
            };
        }
        // The preflight child bound is the checked sum over every validated
        // source span, and the canonical counter stays below the `u32`
        // product count converted by the caller, so neither addition can
        // overflow here.
        child_cursor += span.length;
        canonical += 1;
    }
}

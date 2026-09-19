//! Coinductive product refinement and structural key comparison.

use core::cmp::Ordering;

use super::{
    accounting::{Counters, enforce_budget, increment},
    error::{CanonicalDataError, DataCountLane},
    types::{DataFacts, DataResource, DataResourceBudget, DataScratch, lane_index},
};
use crate::ir::{
    ExternalProductRef, ListSpan, ProductChildren, ProductId, ProductRef, SemanticProduct,
    SemanticProductChild,
};

pub(super) fn refine_products<'output, 'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    scratch: &mut DataScratch<'output>,
    counters: &Counters,
    budget: &DataResourceBudget,
    product_count: u32,
) -> Result<u32, CanonicalDataError> {
    let product_len = facts.products.len();
    scratch.colors[..product_len].fill(0);
    let mut rounds = 0_u32;
    if product_len == 0 {
        return Ok(rounds);
    }
    loop {
        if rounds >= budget.max_refinement_rounds {
            return Err(CanonicalDataError::BudgetExceeded {
                resource: DataResource::RefinementRounds,
                observed: u64::from(rounds),
                limit: u64::from(budget.max_refinement_rounds),
            });
        }
        rounds = rounds
            .checked_add(1)
            .ok_or(CanonicalDataError::RefinementBound {
                rounds,
                product_count,
            })?;
        compute_hashes(
            facts,
            scratch.atom_to_canonical,
            scratch.colors,
            scratch.hashes,
            counters,
        )?;
        enforce_budget(
            DataResource::HashEvaluations,
            counters.hash_evaluations.get(),
            budget.max_hash_evaluations,
        )?;
        let mut sort_overflow = false;
        for (ordinal, destination) in scratch.product_order[..product_len].iter_mut().enumerate() {
            *destination = ProductId::new(u32::try_from(ordinal).map_err(|source| {
                CanonicalDataError::Count {
                    lane: DataCountLane::Products,
                    actual: ordinal,
                    source,
                }
            })?);
        }
        let order = &mut scratch.product_order[..product_len];
        let products = facts.products;
        let lists = facts.lists;
        let children = facts.children;
        let atom_map = &scratch.atom_to_canonical[..facts.atoms.len()];
        let colors = &scratch.colors[..product_len];
        let hashes = &scratch.hashes[..product_len];
        order.sort_unstable_by(|left, right| {
            increment(&counters.sort_comparisons);
            if counters.sort_comparisons.get() == u64::MAX {
                sort_overflow = true;
            }
            compare_product_key(
                *left,
                *right,
                products,
                facts.constructors,
                lists,
                children,
                atom_map,
                colors,
                counters,
            )
            .then_with(|| hashes[lane_index(left.raw)].cmp(&hashes[lane_index(right.raw)]))
            .then_with(|| left.raw.cmp(&right.raw))
        });
        if sort_overflow {
            return Err(CanonicalDataError::ResourceCounterOverflow {
                resource: DataResource::SortComparisons,
            });
        }
        enforce_budget(
            DataResource::SortComparisons,
            counters.sort_comparisons.get(),
            budget.max_sort_comparisons,
        )?;
        assign_colors(
            facts,
            scratch,
            counters,
            product_len,
            product_count,
            &mut sort_overflow,
        )?;
        if sort_overflow {
            return Err(CanonicalDataError::ResourceCounterOverflow {
                resource: DataResource::SortComparisons,
            });
        }
        compute_hashes(
            facts,
            scratch.atom_to_canonical,
            scratch.next_colors,
            scratch.next_hashes,
            counters,
        )?;
        enforce_budget(
            DataResource::HashEvaluations,
            counters.hash_evaluations.get(),
            budget.max_hash_evaluations,
        )?;
        let stable = same_partition(
            &scratch.colors[..product_len],
            &scratch.next_colors[..product_len],
            &scratch.product_order[..product_len],
            counters,
        )?;
        enforce_budget(
            DataResource::SortComparisons,
            counters.sort_comparisons.get(),
            budget.max_sort_comparisons,
        )?;
        if stable {
            scratch.colors[..product_len].copy_from_slice(&scratch.next_colors[..product_len]);
            scratch.hashes[..product_len].copy_from_slice(&scratch.next_hashes[..product_len]);
            return Ok(rounds);
        }
        if rounds >= product_count {
            return Err(CanonicalDataError::RefinementBound {
                rounds,
                product_count,
            });
        }
        scratch.colors[..product_len].copy_from_slice(&scratch.next_colors[..product_len]);
    }
}

fn same_partition(
    colors: &[u32],
    next_colors: &[u32],
    order: &[ProductId],
    counters: &Counters,
) -> Result<bool, CanonicalDataError> {
    for pair in order.windows(2) {
        increment(&counters.sort_comparisons);
        if counters.sort_comparisons.get() == u64::MAX {
            return Err(CanonicalDataError::ResourceCounterOverflow {
                resource: DataResource::SortComparisons,
            });
        }
        let current_same = colors[lane_index(pair[0].raw)] == colors[lane_index(pair[1].raw)];
        let next_same =
            next_colors[lane_index(pair[0].raw)] == next_colors[lane_index(pair[1].raw)];
        if current_same != next_same {
            return Ok(false);
        }
    }
    Ok(true)
}

fn compute_hashes<'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    atom_map: &[u32],
    colors: &[u32],
    hashes: &mut [u64],
    counters: &Counters,
) -> Result<(), CanonicalDataError> {
    for (ordinal, product) in facts.products.iter().copied().enumerate() {
        increment(&counters.hash_evaluations);
        if counters.hash_evaluations.get() == u64::MAX {
            return Err(CanonicalDataError::ResourceCounterOverflow {
                resource: DataResource::HashEvaluations,
            });
        }
        hashes[ordinal] = product_hash(ordinal, product, facts, atom_map, colors, counters)?;
    }
    Ok(())
}

fn product_hash<'facts, 'bytes>(
    ordinal: usize,
    product: SemanticProduct,
    facts: DataFacts<'facts, 'bytes>,
    atom_map: &[u32],
    colors: &[u32],
    counters: &Counters,
) -> Result<u64, CanonicalDataError> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let constructor = facts.constructors[ordinal];
    hash = feed_u32(hash, constructor.tag.into());
    hash = feed_u32(hash, constructor.payload0);
    hash = feed_u32(hash, constructor.payload1);
    hash = feed_u32(hash, atom_map[lane_index(product.head.raw)]);
    let span = facts.lists[lane_index(product.children.raw)];
    hash = feed_u32(hash, span.length);
    let start = lane_index(span.start);
    let end = start + lane_index(span.length);
    for child in facts.children[start..end].iter().copied() {
        increment(&counters.child_visits);
        hash = feed_byte(hash, u8::from(child.role));
        hash = match child.target {
            ProductRef::Local(target) => {
                feed_u32(feed_byte(hash, 0), colors[lane_index(target.raw)])
            }
            ProductRef::External(target) => hash_external(hash, target),
        };
    }
    Ok(hash)
}

fn feed_byte(mut hash: u64, byte: u8) -> u64 {
    hash = hash.wrapping_mul(0x1000_0000_01b3);
    hash ^ u64::from(byte)
}

fn feed_u32(mut hash: u64, value: u32) -> u64 {
    for byte in value.to_le_bytes() {
        hash = feed_byte(hash, byte);
    }
    hash
}

fn hash_external(mut hash: u64, target: ExternalProductRef) -> u64 {
    hash = feed_byte(hash, 1);
    for byte in target.fragment.as_ref() {
        hash = feed_byte(hash, *byte);
    }
    feed_u32(hash, target.ordinal)
}

fn assign_colors<'output, 'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    scratch: &mut DataScratch<'output>,
    counters: &Counters,
    product_len: usize,
    product_count: u32,
    sort_overflow: &mut bool,
) -> Result<(), CanonicalDataError> {
    let mut color = 0_u32;
    let mut previous: Option<ProductId> = None;
    let order = &scratch.product_order[..product_len];
    let products = facts.products;
    let lists = facts.lists;
    let children = facts.children;
    let atom_map = &scratch.atom_to_canonical[..facts.atoms.len()];
    let colors = &scratch.colors[..product_len];
    for source in order.iter().copied() {
        if let Some(previous) = previous {
            increment(&counters.sort_comparisons);
            if counters.sort_comparisons.get() == u64::MAX {
                *sort_overflow = true;
            }
            if compare_product_key(
                previous,
                source,
                products,
                facts.constructors,
                lists,
                children,
                atom_map,
                colors,
                counters,
            ) != Ordering::Equal
            {
                color = color
                    .checked_add(1)
                    .ok_or(CanonicalDataError::RefinementBound {
                        rounds: 0,
                        product_count,
                    })?;
            }
        }
        scratch.next_colors[lane_index(source.raw)] = color;
        previous = Some(source);
    }
    Ok(())
}

#[allow(
    clippy::too_many_arguments,
    reason = "The key compares four borrowed fact lanes and the measured counter explicitly."
)]
pub(super) fn compare_product_key(
    left: ProductId,
    right: ProductId,
    products: &[SemanticProduct],
    constructors: &[crate::ir::SemanticProductConstructor],
    lists: &[ListSpan<ProductChildren>],
    children: &[SemanticProductChild],
    atom_map: &[u32],
    colors: &[u32],
    counters: &Counters,
) -> Ordering {
    let left_product = products[lane_index(left.raw)];
    let right_product = products[lane_index(right.raw)];
    let left_constructor = constructors[lane_index(left.raw)];
    let right_constructor = constructors[lane_index(right.raw)];
    let left_span = lists[lane_index(left_product.children.raw)];
    let right_span = lists[lane_index(right_product.children.raw)];
    let mut ordering = left_constructor
        .cmp(&right_constructor)
        .then_with(|| {
            atom_map[lane_index(left_product.head.raw)]
                .cmp(&atom_map[lane_index(right_product.head.raw)])
        })
        .then_with(|| left_span.length.cmp(&right_span.length));
    if ordering != Ordering::Equal {
        return ordering;
    }
    let left_start = lane_index(left_span.start);
    let right_start = lane_index(right_span.start);
    for offset in 0..lane_index(left_span.length) {
        increment(&counters.child_visits);
        ordering = compare_child_key(
            children[left_start + offset],
            children[right_start + offset],
            colors,
        );
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    ordering
}

fn compare_child_key(
    left: SemanticProductChild,
    right: SemanticProductChild,
    colors: &[u32],
) -> Ordering {
    left.role
        .cmp(&right.role)
        .then_with(|| match (left.target, right.target) {
            (ProductRef::Local(left), ProductRef::Local(right)) => {
                colors[lane_index(left.raw)].cmp(&colors[lane_index(right.raw)])
            }
            (ProductRef::Local(_), ProductRef::External(_)) => Ordering::Less,
            (ProductRef::External(_), ProductRef::Local(_)) => Ordering::Greater,
            (ProductRef::External(left), ProductRef::External(right)) => left.cmp(&right),
        })
}

//! Open-addressed product interning.

use core::cmp::Ordering;

use super::{
    accounting::{Counters, enforce_budget, increment},
    error::{CanonicalDataError, DataCountLane},
    refinement::compare_product_key,
    types::{DataFacts, DataResource, DataResourceBudget, DataScratch, lane_index},
};
use compiler_ir_vocabulary::ProductId;

pub(super) fn intern_products<'output, 'facts, 'bytes>(
    facts: DataFacts<'facts, 'bytes>,
    scratch: &mut DataScratch<'output>,
    counters: &Counters,
    budget: &DataResourceBudget,
) -> Result<u32, CanonicalDataError> {
    let mut canonical_count = 0_u32;
    let product_len = facts.products.len();
    let slot_count = core::cmp::min(scratch.intern_slots.len(), product_len);
    let slots = &mut scratch.intern_slots[..slot_count];
    slots.fill(0);
    for source in scratch.product_order[..product_len].iter().copied() {
        let hash = scratch.hashes[lane_index(source.raw)];
        if slot_count == 0 {
            return Err(CanonicalDataError::InternTableFull {
                product: source,
                capacity: 0,
            });
        }
        let slot_width =
            u64::try_from(slot_count).map_err(|source| CanonicalDataError::NativeWork {
                actual: slot_count,
                source,
            })?;
        let mut slot = usize::try_from(hash % slot_width).map_err(|source| {
            CanonicalDataError::NativeWork {
                actual: slot_count,
                source,
            }
        })?;
        let mut probes = 0_u64;
        let representative = loop {
            increment(&counters.intern_probes);
            probes = probes
                .checked_add(1)
                .ok_or(CanonicalDataError::ResourceCounterOverflow {
                    resource: DataResource::InternProbes,
                })?;
            enforce_budget(
                DataResource::InternProbes,
                counters.intern_probes.get(),
                budget.max_intern_probes,
            )?;
            let entry = slots[slot];
            if entry == 0 {
                slots[slot] = u64::from(source.raw) + 1;
                let representative = canonical_count;
                scratch.product_representatives[lane_index(canonical_count)] = source;
                canonical_count = canonical_count.checked_add(1).ok_or(
                    CanonicalDataError::CanonicalCountOverflow {
                        lane: DataCountLane::Products,
                    },
                )?;
                break representative;
            }
            let existing_raw = entry - 1;
            let existing = ProductId::new(u32::try_from(existing_raw).map_err(|conversion| {
                CanonicalDataError::InternEntry {
                    product: source,
                    entry: existing_raw,
                    source: conversion,
                }
            })?);
            if compare_product_key(
                existing,
                source,
                facts.products,
                facts.constructors,
                facts.lists,
                facts.children,
                scratch.atom_to_canonical,
                scratch.colors,
                counters,
            ) == Ordering::Equal
            {
                break scratch.product_to_canonical[lane_index(existing.raw)];
            }
            if probes >= slot_width {
                return Err(CanonicalDataError::InternTableFull {
                    product: source,
                    capacity: slot_count,
                });
            }
            slot = if slot + 1 == slot_count { 0 } else { slot + 1 };
        };
        scratch.product_to_canonical[lane_index(source.raw)] = representative;
    }
    Ok(canonical_count)
}

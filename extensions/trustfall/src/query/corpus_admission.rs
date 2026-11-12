//! Allocation-free identity validation and exact admission scratch layouts.

use super::{QueryError, SemanticQueryEvidence, SemanticQueryFact};
use std::alloc::Layout;
use std::sync::atomic::AtomicUsize;

/// Requested-layout envelope for moving facts into an admitted corpus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemanticQueryAdmissionBudget {
    /// Maximum measured fact storage plus admission scratch and owner layouts.
    pub maximum_requested_bytes: usize,
    /// An explicit policy credit for each new allocation during admission.
    /// This is separate from allocation layouts and is not allocator overhead.
    pub allocation_quota_per_allocation: usize,
}

/// Additional requested-layout quota needed after all fact owners exist.
///
/// This precharges an exact usize identity-index slice, the pinned Rust 1.97.1 Arc
/// header plus the moved Vec owner, and the largest bounded evidence-error
/// string while validation still owns the facts and index. Physical allocator
/// metadata, size-class rounding and later query-adapter construction are not
/// part of this quota. The source fact storage is measured separately by
/// `measure_semantic_query_facts`.
pub fn semantic_query_corpus_admission_overhead(
    fact_count: usize,
    allocation_quota_per_allocation: usize,
) -> Result<usize, QueryError> {
    let index = Layout::array::<usize>(fact_count)
        .map_err(|_| QueryError::InvalidLimit)?
        .size();
    let (owner, _) = Layout::new::<(AtomicUsize, AtomicUsize)>()
        .extend(Layout::new::<Vec<SemanticQueryFact>>())
        .map_err(|_| QueryError::InvalidLimit)?;
    let allocation_count = 2usize + usize::from(fact_count != 0);
    index
        .checked_add(owner.pad_to_align().size())
        .and_then(|bytes| bytes.checked_add(maximum_evidence_error_bytes()))
        .and_then(|bytes| {
            bytes.checked_add(allocation_quota_per_allocation.checked_mul(allocation_count)?)
        })
        .ok_or(QueryError::InvalidLimit)
}

// Every allocating error reachable from corpus measurement/validation. Keeping
// the actual literals here makes the reserved error layout explicit, rather
// than attributing a universal overhead to allocations.
fn maximum_evidence_error_bytes() -> usize {
    [
        "semantic presentation contains an empty required field",
        "semantic corpus contains a duplicate row identity",
        "package presentation differs from its typed identity",
        "compiler declaration presentation differs from its typed identity",
        "compiler external target presentation differs from its typed identity",
        "compiler external target cannot claim declaration graph ownership",
        "SQL native presentation differs from its typed identity",
        "semantic row refers to a missing project",
        "semantic row crossed its typed package scope",
        "declaration evidence has no typed package scope",
        "semantic row refers to a missing graph target",
        "semantic graph edge crossed its package authority",
        "compiler external target has no compiler declaration source",
        "compiler external edge crossed its exact image authority",
        "package Cargo aliases failed their bounded admission",
        "compiler evidence language authority differs from its package coordinate",
        "compiler evidence lacks captured image provenance",
        "compiler evidence recipe differs from its language profile",
    ]
    .iter()
    .map(|message| message.len())
    .max()
    .unwrap_or(0)
}

pub(super) fn check_cancelled(cancelled: &dyn Fn() -> bool) -> Result<(), QueryError> {
    if cancelled() {
        Err(QueryError::Cancelled)
    } else {
        Ok(())
    }
}

/// Heapsort uses constant stack storage and observes cancellation on every
/// sift step; it does not allocate a sorting scratch buffer.
pub(super) fn sort_ids(
    ids: &mut [usize],
    facts: &[SemanticQueryFact],
    cancelled: &dyn Fn() -> bool,
) -> Result<(), QueryError> {
    let length = ids.len();
    for root in (0..length / 2).rev() {
        sift(ids, facts, root, length, cancelled)?;
    }
    for end in (1..ids.len()).rev() {
        check_cancelled(cancelled)?;
        ids.swap(0, end);
        sift(ids, facts, 0, end, cancelled)?;
    }
    Ok(())
}

fn sift(
    ids: &mut [usize],
    facts: &[SemanticQueryFact],
    mut root: usize,
    end: usize,
    cancelled: &dyn Fn() -> bool,
) -> Result<(), QueryError> {
    while root < end / 2 {
        check_cancelled(cancelled)?;
        let mut child = root * 2 + 1;
        if child + 1 < end
            && facts[ids[child]].presentation.id < facts[ids[child + 1]].presentation.id
        {
            child += 1;
        }
        if facts[ids[root]].presentation.id >= facts[ids[child]].presentation.id {
            break;
        }
        ids.swap(root, child);
        root = child;
    }
    Ok(())
}

pub(super) fn lookup_id<'a>(
    ids: &[usize],
    facts: &'a [SemanticQueryFact],
    id: &str,
) -> Option<&'a SemanticQueryFact> {
    ids.binary_search_by(|index| facts[*index].presentation.id.as_str().cmp(id))
        .ok()
        .map(|position| &facts[ids[position]])
}

pub(super) fn identity_matches(fact: &SemanticQueryFact) -> bool {
    let row = &fact.presentation;
    match &fact.evidence {
        SemanticQueryEvidence::Package(value) => {
            stable_id_matches(&row.id, "package:", value.package.as_bytes())
        }
        SemanticQueryEvidence::Compiler(value) => {
            let mut preimage = [0u8; 64 + 2 + 32 + 32];
            write_hex(value.package.as_bytes(), &mut preimage[..64]);
            preimage[64..66].copy_from_slice(b"::");
            write_hex(value.declaration.family.as_bytes(), &mut preimage[66..98]);
            write_hex(value.declaration.variant.as_bytes(), &mut preimage[98..]);
            symbol_id_matches(&row.id, &preimage)
        }
        SemanticQueryEvidence::CompilerExternalTarget(value) => {
            let scoped = value.target.in_scope(value.package.to_bytes(), value.image);
            let mut preimage = [0u8; 64];
            write_hex(scoped.as_bytes(), &mut preimage);
            symbol_id_matches(&row.id, &preimage)
        }
        SemanticQueryEvidence::StructuralFallback(_) => true,
        SemanticQueryEvidence::SqlNative(value) => {
            let mut ordinal = [0u8; 10];
            let mut number = value.ordinal;
            let mut start = ordinal.len();
            loop {
                start -= 1;
                ordinal[start] = b'0' + (number % 10) as u8;
                number /= 10;
                if number == 0 {
                    break;
                }
            }
            let suffix = &ordinal[start..];
            let Some(tail) = value
                .identity_preimage
                .as_bytes()
                .strip_prefix(row.coordinate.as_bytes())
            else {
                return false;
            };
            tail.strip_prefix(b"\0sql:") == Some(suffix)
                && stable_id_matches(
                    &row.id,
                    "symbol:",
                    backend_library::symbol_key(&value.identity_preimage).as_bytes(),
                )
        }
    }
}

fn symbol_id_matches(id: &str, preimage: &[u8]) -> bool {
    let Ok(preimage) = std::str::from_utf8(preimage) else {
        return false;
    };
    stable_id_matches(
        id,
        "symbol:",
        backend_library::symbol_key(preimage).as_bytes(),
    )
}

fn stable_id_matches(id: &str, prefix: &str, bytes: &[u8; 32]) -> bool {
    let Some(hex) = id.strip_prefix(prefix) else {
        return false;
    };
    if hex.len() != 64 {
        return false;
    }
    let mut expected = [0u8; 64];
    write_hex(bytes, &mut expected);
    hex.as_bytes() == expected
}

fn write_hex(bytes: &[u8], output: &mut [u8]) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (byte, pair) in bytes.iter().zip(output.chunks_exact_mut(2)) {
        pair[0] = HEX[usize::from(byte >> 4)];
        pair[1] = HEX[usize::from(byte & 15)];
    }
}

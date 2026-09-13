//! Version-bound dependency facts, reuse proofs, and reverse readers.
//!
//! A dependency manifest is part of a recipe's immutable identity. The
//! reverse reader index in this module is an execution aid derived from those
//! facts; it never becomes an alternate source of truth. In particular, an
//! index hit is only a candidate until the stored read is checked against the
//! changed selector.

mod facts;
mod generational;
mod graph;
mod index;
mod invalidation;
mod leases;
mod proof;

use backend_version::CoverageWitness;

// The public facade remains flat for existing semantic callers; the files
// below provide ownership boundaries for facts, graph admission, reverse
// indexing, invalidation, leases, and proof verification.
pub use facts::*;
pub use generational::*;
pub use graph::*;
pub use index::*;
pub use invalidation::*;
pub use leases::*;
pub use proof::*;

pub(super) fn frame(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(
        &u64::try_from(bytes.len())
            .map_or(u64::MAX, |length| length)
            .to_be_bytes(),
    );
    out.extend_from_slice(bytes);
}

pub(super) fn version(out: &mut Vec<u8>, bytes: &[u8; 32]) {
    frame(out, bytes);
}

pub(super) fn coverage(out: &mut Vec<u8>, witness: CoverageWitness) {
    out.push(match witness.state() {
        backend_version::Coverage::Complete => 0,
        backend_version::Coverage::Closed => 4,
        backend_version::Coverage::Partial => 1,
        backend_version::Coverage::Unavailable => 2,
        backend_version::Coverage::Unsupported => 3,
    });
    out.extend_from_slice(witness.scope_root().as_bytes());
    if let CoverageWitness::Complete(value) = witness {
        out.extend_from_slice(&value.producer_identity());
    }
}

pub(super) fn checked_add(left: u64, right: u64) -> Result<u64, crate::SemanticError> {
    left.checked_add(right)
        .ok_or(crate::SemanticError::Overflow)
}

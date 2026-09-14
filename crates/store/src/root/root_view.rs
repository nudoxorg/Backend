//! Defines root-view behavior for `backend-store`, whose purpose is to construct and validate immutable generation roots and locality metadata.
//! This module owns the root-view invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Borrowed validation and locality composition for canonical root bytes.

mod parse;
mod selection;

#[cfg(test)]
mod tests;

use backend_version::{ContentAuthority, GenerationId};

use crate::root::{RootEntryCount, encode::RootWireRecord};

pub use parse::RootReadError;
pub use selection::{
    BorrowedGenerationScan, BorrowedGenerationView, BorrowedSelectedGeneration,
    BorrowedSelectedScan,
};

/// Immutable facts decoded from one complete canonical root artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BorrowedRootFacts<'bytes> {
    /// Complete canonical bytes retained by reference.
    pub bytes: &'bytes [u8],
    /// Canonical generation identity of those exact bytes.
    pub id: GenerationId,
    /// Validated canonical row count.
    pub entry_count: RootEntryCount,
}

enum BorrowedRootRows<'bytes, DomainTag> {
    Empty(&'bytes [RootWireRecord]),
    Populated {
        rows: &'bytes [RootWireRecord],
        authority: ContentAuthority<DomainTag>,
    },
}

impl<DomainTag> Copy for BorrowedRootRows<'_, DomainTag> {}

impl<DomainTag> Clone for BorrowedRootRows<'_, DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<'bytes, DomainTag> BorrowedRootRows<'bytes, DomainTag> {
    const fn rows(&self) -> &'bytes [RootWireRecord] {
        match *self {
            Self::Empty(rows) | Self::Populated { rows, .. } => rows,
        }
    }
}

/// Borrowed proof that one complete byte slice is a canonical generation root.
///
/// The witness owns no row or descriptor backing. The only allocation used by
/// validation is a transient compact parent-coordinate lane which is released
/// before this value is returned.
pub struct ValidatedRoot<'bytes, DomainTag> {
    facts: BorrowedRootFacts<'bytes>,
    rows: BorrowedRootRows<'bytes, DomainTag>,
}

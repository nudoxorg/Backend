//! Defines facet-join behavior for `server-index-build`, whose purpose is to project reopened compiler IR into immutable exact and lexical segments.
//! This module owns the exact-plane facet invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! A snapshot-pinned exact-plane join over lexical name hits.
//!
//! The join is analytic O(H·(log R + decode)): each hit performs one exact lookup over the
//! selected segment range and one fixed-width value decode.

use server_index_core::{
    ExactDegradation, ExactManifest, ExactOperation, ExactResolution, ExactSegmentId,
    ExactTerminal, IndexSnapshotId, LexicalDegradation, LexicalSegmentId, LexicalSnapshotHit,
    LexicalTerminal,
};
use thiserror::Error;

use crate::{ExactEntityValue, ExactEntityValueError};

/// One closed facet cell: the three compiler entity kinds plus unresolved exact keys.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetCell {
    /// Function declarations.
    Function,
    /// Constant declarations.
    Constant,
    /// Record declarations.
    Record,
    /// Lexical hits absent from or deleted in the exact plane.
    Unresolved,
}

impl FacetCell {
    const fn count(self, table: &FacetTable) -> u32 {
        match self {
            Self::Function => table.function,
            Self::Constant => table.constant,
            Self::Record => table.record,
            Self::Unresolved => table.unresolved,
        }
    }
}

/// Caller-owned fixed-capacity facet cells for one join.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FacetTable {
    function: u32,
    constant: u32,
    record: u32,
    unresolved: u32,
}

impl Default for FacetTable {
    fn default() -> Self {
        Self::new()
    }
}

impl FacetTable {
    /// Creates an all-zero fixed facet table.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            function: 0,
            constant: 0,
            record: 0,
            unresolved: 0,
        }
    }

    /// Returns the count in one closed facet cell.
    #[must_use]
    pub const fn count(&self, cell: FacetCell) -> u32 {
        cell.count(self)
    }

    const fn clear(&mut self) {
        self.function = 0;
        self.constant = 0;
        self.record = 0;
        self.unresolved = 0;
    }

    fn increment<'value>(
        &mut self,
        cell: FacetCell,
        document: server_index_core::EntityDocumentId,
    ) -> Result<(), FacetJoinError<'value>> {
        let count = match cell {
            FacetCell::Function => &mut self.function,
            FacetCell::Constant => &mut self.constant,
            FacetCell::Record => &mut self.record,
            FacetCell::Unresolved => &mut self.unresolved,
        };
        *count = count
            .checked_add(1)
            .ok_or(FacetJoinError::CountOverflow { cell, document })?;
        Ok(())
    }
}

/// The lexical-side standing retained by a facet terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetLexicalStanding<'segments> {
    /// Every selected lexical segment was available.
    Complete,
    /// Some selected lexical segments were unavailable.
    Partial {
        /// Exact unavailable lexical segment identities.
        missing: &'segments [LexicalSegmentId],
    },
    /// The lexical route degraded while retaining its pinned snapshot.
    Degraded {
        /// Exact unavailable lexical segment identities.
        missing: &'segments [LexicalSegmentId],
        /// Typed route degradation reason.
        reason: LexicalDegradation,
    },
}

/// The exact-side standing retained by a facet terminal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FacetExactStanding<'segments> {
    /// Every consumed exact lookup was complete.
    Complete,
    /// Some consumed exact lookups had unavailable selected segments.
    Partial {
        /// Exact unavailable segment identities.
        missing: &'segments [ExactSegmentId],
    },
    /// The exact route degraded while retaining its pinned snapshot.
    Degraded {
        /// Exact unavailable segment identities.
        missing: &'segments [ExactSegmentId],
        /// Typed route degradation reason.
        reason: ExactDegradation,
    },
}

/// Successful counts and both composed plane standings for one pinned snapshot.
///
/// Constructing this view directly does not prove its facts belong to one join; only
/// [`join_facets`] creates the checked pairing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FacetTerminal<'table, 'lexical, 'exact> {
    /// Snapshot identity shared by lexical and exact planes.
    pub snapshot: IndexSnapshotId,
    /// Caller-owned counts produced by the join.
    pub counts: &'table FacetTable,
    /// Lexical availability retained from the input terminal.
    pub lexical: FacetLexicalStanding<'lexical>,
    /// Exact availability observed while consuming hits.
    pub exact: FacetExactStanding<'exact>,
}

/// A typed rejection from the snapshot-pinned facet join.
#[derive(Debug, Error)]
pub enum FacetJoinError<'value> {
    /// Lexical and exact planes were not pinned to the same snapshot.
    #[error("facet join snapshot mismatch: lexical {lexical:?}, exact {exact:?}")]
    SnapshotMismatch {
        /// Lexical terminal snapshot identity.
        lexical: IndexSnapshotId,
        /// Exact manifest snapshot identity.
        exact: IndexSnapshotId,
    },
    /// An exact value was corrupt rather than absent.
    #[error("exact value for document {document:?} could not be decoded")]
    ValueDecode {
        /// Rejected document identity.
        document: server_index_core::EntityDocumentId,
        /// Rejected value bytes.
        value: &'value [u8],
        /// Exact typed decode cause.
        #[source]
        source: ExactEntityValueError,
    },
    /// A facet cell count exceeded its integer representation.
    #[error("facet cell {cell:?} overflowed for document {document:?}")]
    CountOverflow {
        /// Cell whose count could not be incremented.
        cell: FacetCell,
        /// Document that caused the rejected increment.
        document: server_index_core::EntityDocumentId,
    },
}

/// Joins lexical name hits to the exact entity-kind plane without allocation.
///
/// The table is cleared before processing and remains caller-owned for reuse. Empty lexical hits
/// produce a complete zero-count terminal when the lexical terminal is complete.
///
/// # Errors
///
/// Returns a snapshot mismatch, source-preserving exact decode rejection, or count overflow.
pub fn join_facets<'table, 'lexical, 'manifest, 'segment>(
    lexical: &LexicalTerminal<'lexical, '_, '_>,
    exact: &ExactManifest<'manifest, 'segment>,
    table: &'table mut FacetTable,
) -> Result<FacetTerminal<'table, 'lexical, 'manifest>, FacetJoinError<'segment>> {
    let (snapshot, hits, lexical_standing) = lexical_parts(lexical);
    if snapshot != exact.snapshot {
        return Err(FacetJoinError::SnapshotMismatch {
            lexical: snapshot,
            exact: exact.snapshot,
        });
    }
    table.clear();
    let mut exact_standing = FacetExactStanding::Complete;
    for hit in hits {
        let key: [u8; server_index_core::ENTITY_DOCUMENT_ID_BYTES] = hit.document.into();
        let terminal = exact.execute(ExactOperation::new(&key));
        let resolution = match terminal {
            ExactTerminal::Complete { resolution, .. } => {
                exact_standing = widen_exact_standing(exact_standing, FacetExactStanding::Complete);
                resolution
            }
            ExactTerminal::Partial {
                resolution,
                missing,
                ..
            } => {
                exact_standing =
                    widen_exact_standing(exact_standing, FacetExactStanding::Partial { missing });
                resolution
            }
            ExactTerminal::Degraded {
                resolution,
                missing,
                reason,
                ..
            } => {
                exact_standing = widen_exact_standing(
                    exact_standing,
                    FacetExactStanding::Degraded { missing, reason },
                );
                resolution
            }
        };
        match resolution {
            ExactResolution::Present { value, .. } => {
                let decoded = ExactEntityValue::try_from(value).map_err(|source| {
                    FacetJoinError::ValueDecode {
                        document: hit.document,
                        value,
                        source,
                    }
                })?;
                table.increment(proven_kind(decoded.as_ref()), hit.document)?;
            }
            ExactResolution::Deleted { .. } | ExactResolution::Absent => {
                table.increment(FacetCell::Unresolved, hit.document)?;
            }
        }
    }
    Ok(FacetTerminal {
        snapshot,
        counts: table,
        lexical: lexical_standing,
        exact: exact_standing,
    })
}

const fn widen_exact_standing<'segments>(
    current: FacetExactStanding<'segments>,
    observed: FacetExactStanding<'segments>,
) -> FacetExactStanding<'segments> {
    if matches!(current, FacetExactStanding::Degraded { .. }) {
        return current;
    }
    if matches!(observed, FacetExactStanding::Degraded { .. }) {
        return observed;
    }
    if matches!(current, FacetExactStanding::Partial { .. }) {
        return current;
    }
    observed
}

const fn lexical_parts<'manifest, 'output, 'bytes>(
    terminal: &LexicalTerminal<'manifest, 'output, 'bytes>,
) -> (
    IndexSnapshotId,
    &'output [LexicalSnapshotHit<'bytes>],
    FacetLexicalStanding<'manifest>,
) {
    match terminal {
        LexicalTerminal::Complete { snapshot, hits } => {
            (*snapshot, hits, FacetLexicalStanding::Complete)
        }
        LexicalTerminal::Partial {
            snapshot,
            hits,
            missing,
        } => (*snapshot, hits, FacetLexicalStanding::Partial { missing }),
        LexicalTerminal::Degraded {
            snapshot,
            hits,
            missing,
            reason,
        } => (
            *snapshot,
            hits,
            FacetLexicalStanding::Degraded {
                missing,
                reason: *reason,
            },
        ),
    }
}

/// Projects the kind cell after [`ExactEntityValue::try_from`] has proven the complete wire.
fn proven_kind(value: &[u8]) -> FacetCell {
    let [_, low, high, ..] = value else {
        return FacetCell::Function;
    };
    let raw = u16::from_le_bytes([*low, *high]);
    match raw {
        0 => FacetCell::Function,
        1 => FacetCell::Constant,
        2 => FacetCell::Record,
        // `try_from` above has already proven this arm unreachable.
        _ => FacetCell::Unresolved,
    }
}

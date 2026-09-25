use core::ops::Deref;

use crate::index_core::MAX_SELECTED_SEGMENTS;
use crate::index_core::document::{ENTITY_DOCUMENT_ID_BYTES, EntityDocumentId};
use crate::index_core::lexical::{
    LexicalOperation, LexicalOutputError, LexicalRow, LexicalScore, LexicalSegment,
    LexicalSnapshotHit, LexicalTopK,
};
use crate::index_core::selection::{SelectionFault, admit_selection};
use crate::index_core::snapshot::IndexSnapshot;
use crate::index_vocabulary::{IndexSnapshotId, LexicalSegmentId};

/// A checked borrowed manifest for one immutable lexical snapshot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LexicalManifest<'manifest, 'segment> {
    view: LexicalManifestView<'manifest, 'segment>,
    availability: LexicalAvailability,
}

/// Immutable public facts of one validated lexical manifest.
///
/// This view is read-only when reached through [`LexicalManifest`].  Constructing a view directly
/// does not create a manifest proof; only [`LexicalManifest::new`] or
/// [`LexicalManifest::new_degraded`] can do that.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LexicalManifestView<'manifest, 'segment> {
    /// Immutable snapshot identity pinned by this manifest.
    pub snapshot: IndexSnapshotId,
    /// Reachable lexical segments in snapshot update order.
    pub segments: &'manifest [LexicalSegment<'segment>],
    /// Selected lexical segment identities that were unavailable.
    pub missing: &'manifest [LexicalSegmentId],
}

impl<'manifest, 'segment> Deref for LexicalManifest<'manifest, 'segment> {
    type Target = LexicalManifestView<'manifest, 'segment>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

/// One contiguous borrowed lexical match range with its immutable provenance.
///
/// The range is internal to manifest execution: its constructor is reached only while walking a
/// manifest whose selected-segment bound has already been proved.
#[derive(Clone, Copy)]
struct MatchedLexicalRange<'bytes> {
    segment: LexicalSegmentId,
    rows: &'bytes [LexicalRow<'bytes>],
}

/// Fixed-capacity directory of borrowed match ranges for one manifest operation.
///
/// `len` is incremented only after the corresponding slot is initialized.  All public execution
/// loops consume [`Self::iter`], so they cannot mix a segment identity with another range's rows.
struct MatchedLexicalRanges<'bytes> {
    entries: [Option<MatchedLexicalRange<'bytes>>; MAX_SELECTED_SEGMENTS],
    len: usize,
    rows: usize,
}

impl<'bytes> MatchedLexicalRanges<'bytes> {
    const fn new() -> Self {
        Self {
            entries: [None; MAX_SELECTED_SEGMENTS],
            len: 0,
            rows: 0,
        }
    }

    fn push(&mut self, range: MatchedLexicalRange<'bytes>) -> Result<(), LexicalQueryError> {
        let observed = self.len.saturating_add(1);
        let Some(slot) = self.entries.get_mut(self.len) else {
            return Err(LexicalQueryError::MatchingRangeCapacity {
                limit: MAX_SELECTED_SEGMENTS,
                observed,
            });
        };
        *slot = Some(range);
        self.len = observed;
        self.rows = self.rows.saturating_add(range.rows.len());
        Ok(())
    }

    fn iter(&self) -> impl Iterator<Item = MatchedLexicalRange<'bytes>> + '_ {
        self.entries[..self.len].iter().filter_map(|entry| *entry)
    }
}

impl<'manifest, 'segment> LexicalManifest<'manifest, 'segment> {
    /// Validates a healthy lexical snapshot manifest before query execution.
    pub fn new(
        snapshot: IndexSnapshot<'_>,
        segments: &'manifest [LexicalSegment<'segment>],
        missing: &'manifest [LexicalSegmentId],
    ) -> Result<Self, LexicalManifestError> {
        Self::new_with_availability(snapshot, segments, missing, LexicalAvailability::Healthy)
    }

    /// Validates a lexical manifest reached through a degraded route.
    pub fn new_degraded(
        snapshot: IndexSnapshot<'_>,
        segments: &'manifest [LexicalSegment<'segment>],
        missing: &'manifest [LexicalSegmentId],
        reason: LexicalDegradation,
    ) -> Result<Self, LexicalManifestError> {
        Self::new_with_availability(
            snapshot,
            segments,
            missing,
            LexicalAvailability::Degraded(reason),
        )
    }

    fn new_with_availability(
        snapshot: IndexSnapshot<'_>,
        segments: &'manifest [LexicalSegment<'segment>],
        missing: &'manifest [LexicalSegmentId],
        availability: LexicalAvailability,
    ) -> Result<Self, LexicalManifestError> {
        admit_selection(
            snapshot.lexical,
            segments,
            missing,
            MAX_SELECTED_SEGMENTS,
            |segment| segment.id,
        )
        .map_err(lexical_fault)?;
        Ok(Self {
            view: LexicalManifestView {
                snapshot: snapshot.id,
                segments,
                missing,
            },
            availability,
        })
    }

    /// Returns whether every selected segment is present through a healthy route.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.view.missing.is_empty() && matches!(self.availability, LexicalAvailability::Healthy)
    }

    /// Returns whether the manifest reached its snapshot through a degraded route.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        matches!(self.availability, LexicalAvailability::Degraded(_))
    }

    /// Executes one deterministic manifest-wide ranking into caller-owned output.
    ///
    /// Selected segments are ordered newest first. The first occurrence of a stable document
    /// identity wins, providing update semantics without allocating a hidden deduplication map.
    /// The caller supplies one optional-document scratch slot per matching row; its exact required
    /// capacity is reported before the first scratch or output write.
    pub fn execute<'output>(
        &self,
        operation: LexicalOperation<'_>,
        top_k: LexicalTopK,
        seen_documents: &mut [Option<EntityDocumentId>],
        output: &'output mut [LexicalSnapshotHit<'segment>],
    ) -> Result<LexicalTerminal<'manifest, 'output, 'segment>, LexicalQueryError> {
        // Cache each borrowed match range once. Preflight still deliberately checks newest-first
        // ownership before mutating caller buffers, but no longer repeats every segment's lookup
        // for every candidate row.
        let mut matching_ranges = MatchedLexicalRanges::new();
        for segment in self.view.segments {
            if let Some(rows) = segment.lookup(operation) {
                matching_ranges.push(MatchedLexicalRange {
                    segment: segment.id,
                    rows,
                })?;
            }
        }
        let matching_rows = matching_ranges.rows;
        if seen_documents.len() < matching_rows {
            return Err(LexicalQueryError::ScratchCapacity {
                required: matching_rows,
                available: seen_documents.len(),
            });
        }

        let mut unique_documents = 0_usize;
        for (segment_position, range) in matching_ranges.iter().enumerate() {
            let rows = range.rows;
            for (row_position, row) in rows.iter().enumerate() {
                let was_seen = matching_ranges
                    .iter()
                    .take(segment_position)
                    .flat_map(|previous| previous.rows.iter())
                    .any(|previous| previous.document == row.document)
                    || rows[..row_position]
                        .iter()
                        .any(|previous| previous.document == row.document);
                if !was_seen && !row.is_tombstone() {
                    unique_documents += 1;
                }
            }
        }
        let required = core::cmp::min(unique_documents, usize::from(top_k));
        if output.len() < required {
            return Err(LexicalQueryError::OutputCapacity(LexicalOutputError {
                required,
                available: output.len(),
            }));
        }

        let mut hit_count = 0_usize;
        for slot in &mut seen_documents[..matching_rows] {
            *slot = None;
        }
        for range in matching_ranges.iter() {
            let rows = range.rows;
            for row in rows.iter() {
                if !remember_document(&mut seen_documents[..matching_rows], row.document)? {
                    continue;
                }
                let Some(stored) = row.score() else {
                    continue;
                };
                let Some(score) = operation.relevance(stored, row.term.len()) else {
                    return Err(LexicalQueryError::ScoreDiscount {
                        stored,
                        prefix_bytes: operation.term.len(),
                        term_bytes: row.term.len(),
                    });
                };
                let candidate =
                    LexicalSnapshotHit::new(range.segment, row.term, row.document, score);
                let position = output[..hit_count]
                    .iter()
                    .position(|current| lexical_snapshot_order(&candidate, current).is_lt())
                    .unwrap_or(hit_count);
                if position < required {
                    let new_count = core::cmp::min(hit_count + 1, required);
                    for destination in (position + 1..new_count).rev() {
                        output[destination] = output[destination - 1];
                    }
                    output[position] = candidate;
                    hit_count = new_count;
                }
            }
        }
        let hits = &output[..hit_count];
        match self.availability {
            LexicalAvailability::Healthy if self.view.missing.is_empty() => {
                Ok(LexicalTerminal::Complete {
                    snapshot: self.view.snapshot,
                    hits,
                })
            }
            LexicalAvailability::Healthy => Ok(LexicalTerminal::Partial {
                snapshot: self.view.snapshot,
                hits,
                missing: self.view.missing,
            }),
            LexicalAvailability::Degraded(reason) => Ok(LexicalTerminal::Degraded {
                snapshot: self.view.snapshot,
                hits,
                missing: self.view.missing,
                reason,
            }),
        }
    }
}

fn lexical_fault(fault: SelectionFault<LexicalSegmentId>) -> LexicalManifestError {
    match fault {
        SelectionFault::SelectedSegmentLimit { limit, observed } => {
            LexicalManifestError::SelectedSegmentLimit { limit, observed }
        }
        SelectionFault::MissingSegmentLimit { limit, observed } => {
            LexicalManifestError::MissingSegmentLimit { limit, observed }
        }
        SelectionFault::SnapshotSelectionWidth { selected, observed } => {
            LexicalManifestError::SnapshotSelectionWidth { selected, observed }
        }
        SelectionFault::PresentNotSelected {
            present_position,
            id,
        } => LexicalManifestError::PresentNotSelected {
            present_position,
            id,
        },
        SelectionFault::PresentOrderMismatch {
            present_position,
            preceding_selected_position,
            selected_position,
            id,
        } => LexicalManifestError::PresentOrderMismatch {
            present_position,
            preceding_selected_position,
            selected_position,
            id,
        },
        SelectionFault::MissingNotSelected {
            missing_position,
            id,
        } => LexicalManifestError::MissingNotSelected {
            missing_position,
            id,
        },
        SelectionFault::DuplicatePresentSegment {
            left_position,
            right_position,
            id,
        } => LexicalManifestError::DuplicatePresentSegment {
            left_position,
            right_position,
            id,
        },
        SelectionFault::PresentAndMissing {
            present_position,
            missing_position,
            id,
        } => LexicalManifestError::PresentAndMissing {
            present_position,
            missing_position,
            id,
        },
        SelectionFault::DuplicateMissingSegment {
            left_position,
            right_position,
            id,
        } => LexicalManifestError::DuplicateMissingSegment {
            left_position,
            right_position,
            id,
        },
    }
}

fn lexical_snapshot_order(
    left: &LexicalSnapshotHit<'_>,
    right: &LexicalSnapshotHit<'_>,
) -> core::cmp::Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.document.cmp(&right.document))
        .then_with(|| left.segment.cmp(&right.segment))
}

fn remember_document(
    seen_documents: &mut [Option<EntityDocumentId>],
    document: EntityDocumentId,
) -> Result<bool, LexicalQueryError> {
    if seen_documents.is_empty() {
        return Err(LexicalQueryError::ScratchProbeExhausted { capacity: 0 });
    }
    let mut slot = document_hash(document) % seen_documents.len();
    for _ in 0..seen_documents.len() {
        match seen_documents[slot] {
            None => {
                seen_documents[slot] = Some(document);
                return Ok(true);
            }
            Some(existing) if existing == document => return Ok(false),
            Some(_) => {
                slot += 1;
                if slot == seen_documents.len() {
                    slot = 0;
                }
            }
        }
    }
    Err(LexicalQueryError::ScratchProbeExhausted {
        capacity: seen_documents.len(),
    })
}

fn document_hash(document: EntityDocumentId) -> usize {
    let bytes: [u8; ENTITY_DOCUMENT_ID_BYTES] = document.into();
    let mut hash = 2_166_136_261_usize;
    for byte in bytes {
        hash = hash
            .wrapping_mul(16_777_619_usize)
            .wrapping_add(usize::from(byte));
    }
    hash
}

/// Health provenance for a selected lexical snapshot route.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LexicalAvailability {
    /// Every selected lexical segment came from a healthy route.
    Healthy,
    /// The route degraded without changing the requested snapshot identity.
    Degraded(LexicalDegradation),
}

/// The typed reason that a lexical answer was degraded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LexicalDegradation {
    /// A stale route was retried without changing the pinned snapshot.
    StaleRoute,
    /// One selected lexical segment source was unavailable.
    SegmentSourceUnavailable,
}

/// A lexical execution rejection preserving selection or capacity operands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LexicalQueryError {
    /// Internal borrowed-range directory could not represent a manifest selection already
    /// admitted by the manifest boundary.
    MatchingRangeCapacity {
        /// Fixed internal range-directory capacity.
        limit: usize,
        /// Number of matching ranges observed before rejection.
        observed: usize,
    },
    /// Caller deduplication scratch could not retain every matching document identity.
    ScratchCapacity {
        /// Exact required optional-document slots.
        required: usize,
        /// Supplied optional-document slots.
        available: usize,
    },
    /// The caller-owned deduplication table had no empty slot after one complete bounded probe.
    ///
    /// With a cleared scratch prefix at least as long as the matching-row count this is
    /// unreachable for valid manifest rows, but it remains typed rather than relying on an
    /// unbounded probe or a panic should that invariant be violated.
    ScratchProbeExhausted {
        /// Number of caller-owned scratch slots probed.
        capacity: usize,
    },
    /// Caller output could not retain the complete requested ranking.
    OutputCapacity(LexicalOutputError),
    /// The deterministic relevance recipe could not represent one score discount.
    ScoreDiscount {
        /// Complete stored score of the rejected row.
        stored: LexicalScore,
        /// Query-prefix width.
        prefix_bytes: usize,
        /// Matched-term width.
        term_bytes: usize,
    },
}

/// The one snapshot-pinned terminal for a manifest-wide lexical ranking.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LexicalTerminal<'manifest, 'output, 'bytes> {
    /// Every selected segment was present and healthy.
    Complete {
        /// Snapshot identity used by the query.
        snapshot: IndexSnapshotId,
        /// Caller-owned ranked output borrowed for the terminal lifetime.
        hits: &'output [LexicalSnapshotHit<'bytes>],
    },
    /// Some selected lexical segments were unavailable.
    Partial {
        /// Snapshot identity used by the query.
        snapshot: IndexSnapshotId,
        /// Caller-owned ranked output borrowed for the terminal lifetime.
        hits: &'output [LexicalSnapshotHit<'bytes>],
        /// Exact selected lexical segment identities not reached by the query.
        missing: &'manifest [LexicalSegmentId],
    },
    /// The route degraded while preserving the requested lexical snapshot.
    Degraded {
        /// Snapshot identity used by the query.
        snapshot: IndexSnapshotId,
        /// Caller-owned ranked output borrowed for the terminal lifetime.
        hits: &'output [LexicalSnapshotHit<'bytes>],
        /// Exact selected lexical segment identities not reached by the query.
        missing: &'manifest [LexicalSegmentId],
        /// Route-health provenance for the degraded terminal.
        reason: LexicalDegradation,
    },
}

/// A lexical manifest admission failure preserving offending positions and identity.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LexicalManifestError {
    /// The selected present segment count exceeded the bounded query contract.
    SelectedSegmentLimit {
        /// Largest legal selected segment count.
        limit: usize,
        /// Full untrusted selected segment count.
        observed: usize,
    },
    /// The selected missing segment count exceeded the bounded query contract.
    MissingSegmentLimit {
        /// Largest legal selected segment count.
        limit: usize,
        /// Full untrusted selected segment count.
        observed: usize,
    },
    /// Reachable and missing segments did not account for the snapshot selection.
    SnapshotSelectionWidth {
        /// Number of lexical identities bound into the snapshot.
        selected: usize,
        /// Number supplied as reachable or missing.
        observed: usize,
    },
    /// A reachable segment was not selected by the snapshot authority.
    PresentNotSelected {
        /// Reachable segment position.
        present_position: usize,
        /// Unselected segment identity.
        id: LexicalSegmentId,
    },
    /// Reachable update order disagreed with the order bound into the snapshot.
    PresentOrderMismatch {
        /// Reachable segment position whose order was rejected.
        present_position: usize,
        /// Snapshot position of the preceding reachable segment.
        preceding_selected_position: usize,
        /// Snapshot position of the rejected reachable segment.
        selected_position: usize,
        /// Rejected lexical segment identity.
        id: LexicalSegmentId,
    },
    /// A missing identity was not selected by the snapshot authority.
    MissingNotSelected {
        /// Missing segment position.
        missing_position: usize,
        /// Unselected missing identity.
        id: LexicalSegmentId,
    },
    /// One segment identity appeared in two present positions.
    DuplicatePresentSegment {
        /// Earlier duplicate position.
        left_position: usize,
        /// Later duplicate position.
        right_position: usize,
        /// Repeated lexical segment identity.
        id: LexicalSegmentId,
    },
    /// One segment identity appeared as both present and unavailable.
    PresentAndMissing {
        /// Present segment position.
        present_position: usize,
        /// Missing segment position.
        missing_position: usize,
        /// Conflicting lexical segment identity.
        id: LexicalSegmentId,
    },
    /// One missing segment identity appeared in two positions.
    DuplicateMissingSegment {
        /// Earlier duplicate position.
        left_position: usize,
        /// Later duplicate position.
        right_position: usize,
        /// Repeated lexical segment identity.
        id: LexicalSegmentId,
    },
}

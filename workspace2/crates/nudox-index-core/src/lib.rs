#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Borrowed immutable exact and lexical index segments.
//!
//! The crate owns logical query semantics only.  It borrows canonical row lanes
//! and immutable manifests; storage, publication, and backend adapters remain
//! outside this portable core.

use core::ops::Deref;

mod exact;
mod lexical;
mod snapshot;

pub use exact::{
    ExactOperation, ExactRow, ExactSegment, ExactSegmentError, ExactSegmentView,
    MAX_EXACT_PAYLOAD_BYTES,
};
pub use lexical::{
    LexicalDocumentId, LexicalHit, LexicalOperation, LexicalOutputError, LexicalRow,
    LexicalRowValue, LexicalScore, LexicalSegment, LexicalSegmentError, LexicalSegmentView,
    LexicalSnapshotHit, LexicalTopK, LexicalTopKError, MAX_LEXICAL_PAYLOAD_BYTES, MAX_LEXICAL_ROWS,
    MAX_LEXICAL_TOP_K,
};
pub use nudox_index_vocab::{ExactSegmentId, IndexSnapshotId, LexicalSegmentId};
pub use snapshot::{IndexSnapshot, IndexSnapshotError, IndexSnapshotView};

/// Maximum exact or lexical segments a single borrowed manifest can select.
pub const MAX_SELECTED_SEGMENTS: usize = 8;

/// A checked borrowed manifest for one immutable exact snapshot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ExactManifest<'manifest, 'segment> {
    view: ExactManifestView<'manifest, 'segment>,
    availability: ExactAvailability,
}

/// Immutable public facts of one validated exact manifest.
///
/// This view is read-only when reached through [`ExactManifest`].  Constructing a view directly
/// does not create a manifest proof; only [`ExactManifest::new`] or
/// [`ExactManifest::new_degraded`] can do that.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ExactManifestView<'manifest, 'segment> {
    /// Immutable snapshot identity pinned by this manifest.
    pub snapshot: IndexSnapshotId,
    /// Reachable exact segments in snapshot update order.
    pub segments: &'manifest [ExactSegment<'segment>],
    /// Selected exact segment identities that were unavailable.
    pub missing: &'manifest [ExactSegmentId],
}

impl<'manifest, 'segment> Deref for ExactManifest<'manifest, 'segment> {
    type Target = ExactManifestView<'manifest, 'segment>;

    fn deref(&self) -> &Self::Target {
        &self.view
    }
}

impl<'manifest, 'segment> ExactManifest<'manifest, 'segment> {
    /// Validates selection bounds before looking for duplicate segment identities.
    pub fn new(
        snapshot: IndexSnapshot<'_>,
        segments: &'manifest [ExactSegment<'segment>],
        missing: &'manifest [ExactSegmentId],
    ) -> Result<Self, ExactManifestError> {
        Self::new_with_availability(snapshot, segments, missing, ExactAvailability::Healthy)
    }

    /// Validates a manifest that reached its pinned snapshot through a degraded path.
    pub fn new_degraded(
        snapshot: IndexSnapshot<'_>,
        segments: &'manifest [ExactSegment<'segment>],
        missing: &'manifest [ExactSegmentId],
        reason: ExactDegradation,
    ) -> Result<Self, ExactManifestError> {
        Self::new_with_availability(
            snapshot,
            segments,
            missing,
            ExactAvailability::Degraded(reason),
        )
    }

    fn new_with_availability(
        snapshot: IndexSnapshot<'_>,
        segments: &'manifest [ExactSegment<'segment>],
        missing: &'manifest [ExactSegmentId],
        availability: ExactAvailability,
    ) -> Result<Self, ExactManifestError> {
        if segments.len() > MAX_SELECTED_SEGMENTS {
            return Err(ExactManifestError::SelectedSegmentLimit {
                limit: MAX_SELECTED_SEGMENTS,
                observed: segments.len(),
            });
        }
        if missing.len() > MAX_SELECTED_SEGMENTS {
            return Err(ExactManifestError::MissingSegmentLimit {
                limit: MAX_SELECTED_SEGMENTS,
                observed: missing.len(),
            });
        }
        let selected = snapshot.exact;
        let observed_selection = segments.len() + missing.len();
        if selected.len() != observed_selection {
            return Err(ExactManifestError::SnapshotSelectionWidth {
                selected: selected.len(),
                observed: observed_selection,
            });
        }
        for (left_position, left) in segments.iter().enumerate() {
            for (right_position, right) in segments.iter().enumerate().skip(left_position + 1) {
                if left.id == right.id {
                    return Err(ExactManifestError::DuplicatePresentSegment {
                        left_position,
                        right_position,
                        id: left.id,
                    });
                }
            }
            for (missing_position, missing_id) in missing.iter().enumerate() {
                if left.id == *missing_id {
                    return Err(ExactManifestError::PresentAndMissing {
                        present_position: left_position,
                        missing_position,
                        id: left.id,
                    });
                }
            }
        }
        for (left_position, left) in missing.iter().enumerate() {
            for (right_position, right) in missing.iter().enumerate().skip(left_position + 1) {
                if *left == *right {
                    return Err(ExactManifestError::DuplicateMissingSegment {
                        left_position,
                        right_position,
                        id: *left,
                    });
                }
            }
        }
        let mut preceding_selected_position = None;
        for (present_position, segment) in segments.iter().enumerate() {
            let Some(selected_position) = selected.iter().position(|id| *id == segment.id) else {
                return Err(ExactManifestError::PresentNotSelected {
                    present_position,
                    id: segment.id,
                });
            };
            if let Some(preceding) = preceding_selected_position
                && selected_position <= preceding
            {
                return Err(ExactManifestError::PresentOrderMismatch {
                    present_position,
                    preceding_selected_position: preceding,
                    selected_position,
                    id: segment.id,
                });
            }
            preceding_selected_position = Some(selected_position);
        }
        for (missing_position, id) in missing.iter().enumerate() {
            if !selected.contains(id) {
                return Err(ExactManifestError::MissingNotSelected {
                    missing_position,
                    id: *id,
                });
            }
        }
        Ok(Self {
            view: ExactManifestView {
                snapshot: snapshot.id,
                segments,
                missing,
            },
            availability,
        })
    }

    /// Executes one borrowed exact lookup against the pinned snapshot.
    #[must_use]
    pub fn execute(&self, operation: ExactOperation<'_>) -> ExactTerminal<'manifest, 'segment> {
        let resolution = self.resolve(operation);
        match self.availability {
            ExactAvailability::Healthy if self.view.missing.is_empty() => ExactTerminal::Complete {
                snapshot: self.view.snapshot,
                resolution,
            },
            ExactAvailability::Healthy => ExactTerminal::Partial {
                snapshot: self.view.snapshot,
                resolution,
                missing: self.view.missing,
            },
            ExactAvailability::Degraded(reason) => ExactTerminal::Degraded {
                snapshot: self.view.snapshot,
                resolution,
                missing: self.view.missing,
                reason,
            },
        }
    }

    fn resolve(&self, operation: ExactOperation<'_>) -> ExactResolution<'segment> {
        for segment in self.view.segments {
            if let Some(row) = segment.lookup(operation) {
                if row.is_tombstone() {
                    return ExactResolution::Deleted {
                        segment: segment.id,
                    };
                }
                if let Some(value) = row.value_bytes() {
                    return ExactResolution::Present {
                        segment: segment.id,
                        value,
                    };
                }
            }
        }
        ExactResolution::Absent
    }
}

/// Health provenance for the selected route to a pinned exact snapshot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ExactAvailability {
    /// Every selected segment came from a healthy route.
    Healthy,
    /// A selected route degraded while still preserving the requested snapshot identity.
    Degraded(ExactDegradation),
}

/// The typed reason that a pinned exact answer was degraded.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExactDegradation {
    /// A stale route was retried against the same pinned immutable snapshot.
    StaleRoute,
    /// One selected immutable segment source was unavailable.
    SegmentSourceUnavailable,
}

/// A borrowed exact resolution with its immutable segment provenance.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExactResolution<'bytes> {
    /// The newest selected segment supplied this borrowed value.
    Present {
        /// Immutable segment supplying the value.
        segment: ExactSegmentId,
        /// Borrowed value bytes inside that segment's owner.
        value: &'bytes [u8],
    },
    /// The newest selected segment deleted this key.
    Deleted {
        /// Immutable tombstone segment.
        segment: ExactSegmentId,
    },
    /// No selected present segment named this key.
    Absent,
}

/// The one exact terminal returned for a snapshot-pinned borrowed lookup.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExactTerminal<'manifest, 'bytes> {
    /// Every selected segment was present and healthy.
    Complete {
        /// Snapshot identity used by this query.
        snapshot: IndexSnapshotId,
        /// Exact result from the newest selected segment that named the key.
        resolution: ExactResolution<'bytes>,
    },
    /// Some selected segment identities were unavailable.
    Partial {
        /// Snapshot identity used by this query.
        snapshot: IndexSnapshotId,
        /// Exact result from the reachable selected segments.
        resolution: ExactResolution<'bytes>,
        /// Exact selected immutable segment identities not reached by this query.
        missing: &'manifest [ExactSegmentId],
    },
    /// The answer retained its pinned snapshot but its selected route degraded.
    Degraded {
        /// Snapshot identity used by this query.
        snapshot: IndexSnapshotId,
        /// Exact result from the reachable selected segments.
        resolution: ExactResolution<'bytes>,
        /// Exact selected immutable segment identities not reached by this query.
        missing: &'manifest [ExactSegmentId],
        /// Route-health provenance for the degraded terminal.
        reason: ExactDegradation,
    },
}

/// A manifest admission failure retaining the rejected positions and identity.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ExactManifestError {
    /// The selected present segment count exceeded the bounded query contract.
    SelectedSegmentLimit {
        /// Largest legal selected segment count.
        limit: usize,
        /// Full untrusted selected segment count.
        observed: usize,
    },
    /// The selected missing segment count exceeded the bounded query contract.
    MissingSegmentLimit {
        /// Largest legal selected missing segment count.
        limit: usize,
        /// Full untrusted selected missing segment count.
        observed: usize,
    },
    /// Reachable and missing segments did not account for the snapshot selection.
    SnapshotSelectionWidth {
        /// Number of exact identities bound into the snapshot.
        selected: usize,
        /// Number supplied as reachable or missing.
        observed: usize,
    },
    /// A reachable segment was not selected by the snapshot authority.
    PresentNotSelected {
        /// Reachable segment position.
        present_position: usize,
        /// Unselected segment identity.
        id: ExactSegmentId,
    },
    /// Reachable update order disagreed with the order bound into the snapshot.
    PresentOrderMismatch {
        /// Reachable segment position whose order was rejected.
        present_position: usize,
        /// Snapshot position of the preceding reachable segment.
        preceding_selected_position: usize,
        /// Snapshot position of the rejected reachable segment.
        selected_position: usize,
        /// Rejected exact segment identity.
        id: ExactSegmentId,
    },
    /// A missing identity was not selected by the snapshot authority.
    MissingNotSelected {
        /// Missing segment position.
        missing_position: usize,
        /// Unselected missing identity.
        id: ExactSegmentId,
    },
    /// One segment identity appeared in two present positions.
    DuplicatePresentSegment {
        /// Earlier duplicate position.
        left_position: usize,
        /// Later duplicate position.
        right_position: usize,
        /// Repeated segment identity.
        id: ExactSegmentId,
    },
    /// One segment identity appeared as both present and unavailable.
    PresentAndMissing {
        /// Present segment position.
        present_position: usize,
        /// Missing segment position.
        missing_position: usize,
        /// Conflicting segment identity.
        id: ExactSegmentId,
    },
    /// One missing segment identity appeared in two positions.
    DuplicateMissingSegment {
        /// Earlier duplicate position.
        left_position: usize,
        /// Later duplicate position.
        right_position: usize,
        /// Repeated segment identity.
        id: ExactSegmentId,
    },
}

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
        if segments.len() > MAX_SELECTED_SEGMENTS {
            return Err(LexicalManifestError::SelectedSegmentLimit {
                limit: MAX_SELECTED_SEGMENTS,
                observed: segments.len(),
            });
        }
        if missing.len() > MAX_SELECTED_SEGMENTS {
            return Err(LexicalManifestError::MissingSegmentLimit {
                limit: MAX_SELECTED_SEGMENTS,
                observed: missing.len(),
            });
        }
        let selected = snapshot.lexical;
        let observed_selection = segments.len() + missing.len();
        if selected.len() != observed_selection {
            return Err(LexicalManifestError::SnapshotSelectionWidth {
                selected: selected.len(),
                observed: observed_selection,
            });
        }
        for (left_position, left) in segments.iter().enumerate() {
            for (right_position, right) in segments.iter().enumerate().skip(left_position + 1) {
                if left.id == right.id {
                    return Err(LexicalManifestError::DuplicatePresentSegment {
                        left_position,
                        right_position,
                        id: left.id,
                    });
                }
            }
            for (missing_position, missing_id) in missing.iter().enumerate() {
                if left.id == *missing_id {
                    return Err(LexicalManifestError::PresentAndMissing {
                        present_position: left_position,
                        missing_position,
                        id: left.id,
                    });
                }
            }
        }
        for (left_position, left) in missing.iter().enumerate() {
            for (right_position, right) in missing.iter().enumerate().skip(left_position + 1) {
                if *left == *right {
                    return Err(LexicalManifestError::DuplicateMissingSegment {
                        left_position,
                        right_position,
                        id: *left,
                    });
                }
            }
        }
        let mut preceding_selected_position = None;
        for (present_position, segment) in segments.iter().enumerate() {
            let Some(selected_position) = selected.iter().position(|id| *id == segment.id) else {
                return Err(LexicalManifestError::PresentNotSelected {
                    present_position,
                    id: segment.id,
                });
            };
            if let Some(preceding) = preceding_selected_position
                && selected_position <= preceding
            {
                return Err(LexicalManifestError::PresentOrderMismatch {
                    present_position,
                    preceding_selected_position: preceding,
                    selected_position,
                    id: segment.id,
                });
            }
            preceding_selected_position = Some(selected_position);
        }
        for (missing_position, id) in missing.iter().enumerate() {
            if !selected.contains(id) {
                return Err(LexicalManifestError::MissingNotSelected {
                    missing_position,
                    id: *id,
                });
            }
        }
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
        seen_documents: &mut [Option<LexicalDocumentId>],
        output: &'output mut [LexicalSnapshotHit<'segment>],
    ) -> Result<LexicalTerminal<'manifest, 'output, 'segment>, LexicalQueryError> {
        let matching_rows = self
            .view
            .segments
            .iter()
            .filter_map(|segment| segment.lookup(operation))
            .map(<[LexicalRow<'_>]>::len)
            .sum::<usize>();
        if seen_documents.len() < matching_rows {
            return Err(LexicalQueryError::ScratchCapacity {
                required: matching_rows,
                available: seen_documents.len(),
            });
        }

        let mut unique_documents = 0_usize;
        for (segment_position, segment) in self.view.segments.iter().enumerate() {
            if let Some(rows) = segment.lookup(operation) {
                for (row_position, row) in rows.iter().enumerate() {
                    let was_seen = self
                        .view
                        .segments
                        .iter()
                        .take(segment_position)
                        .filter_map(|previous| previous.lookup(operation))
                        .flat_map(|previous_rows| previous_rows.iter())
                        .any(|previous| previous.document == row.document)
                        || rows
                            .iter()
                            .take(row_position)
                            .any(|previous| previous.document == row.document);
                    if !was_seen && !row.is_tombstone() {
                        unique_documents += 1;
                    }
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

        let mut emitted_documents = 0_usize;
        let mut hit_count = 0_usize;
        for segment in self.view.segments {
            if let Some(rows) = segment.lookup(operation) {
                for row in rows {
                    if seen_documents[..emitted_documents].contains(&Some(row.document)) {
                        continue;
                    }
                    seen_documents[emitted_documents] = Some(row.document);
                    emitted_documents += 1;
                    let Some(score) = row.score() else {
                        continue;
                    };
                    let candidate =
                        LexicalSnapshotHit::new(segment.id, row.term, row.document, score);
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
    /// Caller deduplication scratch could not retain every matching document identity.
    ScratchCapacity {
        /// Exact required optional-document slots.
        required: usize,
        /// Supplied optional-document slots.
        available: usize,
    },
    /// Caller output could not retain the complete requested ranking.
    OutputCapacity(LexicalOutputError),
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

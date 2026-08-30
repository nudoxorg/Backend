#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! Borrowed immutable exact and lexical index segments.
//!
//! The crate owns logical query semantics only.  It borrows canonical row lanes
//! and immutable manifests; storage, publication, and backend adapters remain
//! outside this portable core.

mod exact;
mod lexical;

pub use exact::{ExactOperation, ExactRow, ExactSegment, ExactSegmentError};
pub use lexical::{
    LexicalDocumentId, LexicalHit, LexicalOperation, LexicalOutputError, LexicalRow, LexicalScore,
    LexicalSegment, LexicalSegmentError, LexicalTopK, LexicalTopKError, MAX_LEXICAL_ROWS,
    MAX_LEXICAL_TOP_K,
};
pub use nudox_index_vocab::{ExactSegmentId, IndexSnapshotId, LexicalSegmentId};

/// Maximum exact or lexical segments a single borrowed manifest can select.
pub const MAX_SELECTED_SEGMENTS: usize = 8;

/// A checked borrowed manifest for one immutable exact snapshot.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ExactManifest<'manifest, 'segment> {
    snapshot: IndexSnapshotId,
    segments: &'manifest [ExactSegment<'segment>],
    missing: &'manifest [ExactSegmentId],
    availability: ExactAvailability,
}

impl<'manifest, 'segment> ExactManifest<'manifest, 'segment> {
    /// Validates selection bounds before looking for duplicate segment identities.
    pub fn new(
        snapshot: IndexSnapshotId,
        segments: &'manifest [ExactSegment<'segment>],
        missing: &'manifest [ExactSegmentId],
    ) -> Result<Self, ExactManifestError> {
        Self::new_with_availability(snapshot, segments, missing, ExactAvailability::Healthy)
    }

    /// Validates a manifest that reached its pinned snapshot through a degraded path.
    pub fn new_degraded(
        snapshot: IndexSnapshotId,
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
        snapshot: IndexSnapshotId,
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
        for (left_position, left) in segments.iter().enumerate() {
            for (right_position, right) in segments.iter().enumerate().skip(left_position + 1) {
                if left.id() == right.id() {
                    return Err(ExactManifestError::DuplicatePresentSegment {
                        left_position,
                        right_position,
                        id: left.id(),
                    });
                }
            }
            for (missing_position, missing_id) in missing.iter().enumerate() {
                if left.id() == *missing_id {
                    return Err(ExactManifestError::PresentAndMissing {
                        present_position: left_position,
                        missing_position,
                        id: left.id(),
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
        Ok(Self {
            snapshot,
            segments,
            missing,
            availability,
        })
    }

    /// Returns the pinned immutable snapshot identity.
    #[must_use]
    pub const fn snapshot(&self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the selected present exact segments in descending update order.
    #[must_use]
    pub const fn segments(&self) -> &'manifest [ExactSegment<'segment>] {
        self.segments
    }

    /// Returns the exact selected segment identities that were unavailable.
    #[must_use]
    pub const fn missing(&self) -> &'manifest [ExactSegmentId] {
        self.missing
    }

    /// Executes one borrowed exact lookup against the pinned snapshot.
    #[must_use]
    pub fn execute(&self, operation: ExactOperation<'_>) -> ExactTerminal<'manifest, 'segment> {
        let resolution = self.resolve(operation);
        match self.availability {
            ExactAvailability::Healthy if self.missing.is_empty() => ExactTerminal::Complete {
                snapshot: self.snapshot,
                resolution,
            },
            ExactAvailability::Healthy => ExactTerminal::Partial {
                snapshot: self.snapshot,
                resolution,
                missing: self.missing,
            },
            ExactAvailability::Degraded(reason) => ExactTerminal::Degraded {
                snapshot: self.snapshot,
                resolution,
                missing: self.missing,
                reason,
            },
        }
    }

    fn resolve(&self, operation: ExactOperation<'_>) -> ExactResolution<'segment> {
        for segment in self.segments {
            if let Some(row) = segment.lookup(operation) {
                if row.is_tombstone() {
                    return ExactResolution::Deleted {
                        segment: segment.id(),
                    };
                }
                if let Some(value) = row.value_bytes() {
                    return ExactResolution::Present {
                        segment: segment.id(),
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
    snapshot: IndexSnapshotId,
    segments: &'manifest [LexicalSegment<'segment>],
    missing: &'manifest [LexicalSegmentId],
    availability: LexicalAvailability,
}

impl<'manifest, 'segment> LexicalManifest<'manifest, 'segment> {
    /// Validates a healthy lexical snapshot manifest before query execution.
    pub fn new(
        snapshot: IndexSnapshotId,
        segments: &'manifest [LexicalSegment<'segment>],
        missing: &'manifest [LexicalSegmentId],
    ) -> Result<Self, LexicalManifestError> {
        Self::new_with_availability(snapshot, segments, missing, LexicalAvailability::Healthy)
    }

    /// Validates a lexical manifest reached through a degraded route.
    pub fn new_degraded(
        snapshot: IndexSnapshotId,
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
        snapshot: IndexSnapshotId,
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
        for (left_position, left) in segments.iter().enumerate() {
            for (right_position, right) in segments.iter().enumerate().skip(left_position + 1) {
                if left.id() == right.id() {
                    return Err(LexicalManifestError::DuplicatePresentSegment {
                        left_position,
                        right_position,
                        id: left.id(),
                    });
                }
            }
            for (missing_position, missing_id) in missing.iter().enumerate() {
                if left.id() == *missing_id {
                    return Err(LexicalManifestError::PresentAndMissing {
                        present_position: left_position,
                        missing_position,
                        id: left.id(),
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
        Ok(Self {
            snapshot,
            segments,
            missing,
            availability,
        })
    }

    /// Returns the immutable snapshot identity pinned by this manifest.
    #[must_use]
    pub const fn snapshot(&self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Executes one segment-local lexical ranking over a borrowed selected segment.
    ///
    /// Segment choice is explicit because local document ordinals are meaningful only
    /// with their source segment identity. A later cross-segment merge must own a
    /// separate deduplication scratch representation rather than hiding it here.
    pub fn execute<'output>(
        &self,
        segment_position: usize,
        operation: LexicalOperation<'_>,
        top_k: LexicalTopK,
        output: &'output mut [LexicalHit<'segment>],
    ) -> Result<LexicalTerminal<'manifest, 'output, 'segment>, LexicalQueryError> {
        let Some(segment) = self.segments.get(segment_position) else {
            return Err(LexicalQueryError::UnselectedSegment {
                selected: segment_position,
                present: self.segments.len(),
            });
        };
        let hit_count = match segment.rank(operation, top_k, output) {
            Ok(hit_count) => hit_count,
            Err(error) => return Err(LexicalQueryError::OutputCapacity(error)),
        };
        let hits = &output[..hit_count];
        match self.availability {
            LexicalAvailability::Healthy if self.missing.is_empty() => {
                Ok(LexicalTerminal::Complete {
                    snapshot: self.snapshot,
                    segment: segment.id(),
                    hits,
                })
            }
            LexicalAvailability::Healthy => Ok(LexicalTerminal::Partial {
                snapshot: self.snapshot,
                segment: segment.id(),
                hits,
                missing: self.missing,
            }),
            LexicalAvailability::Degraded(reason) => Ok(LexicalTerminal::Degraded {
                snapshot: self.snapshot,
                segment: segment.id(),
                hits,
                missing: self.missing,
                reason,
            }),
        }
    }
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
    /// The requested segment position was not a present selected segment.
    UnselectedSegment {
        /// Requested selected-segment position.
        selected: usize,
        /// Number of present selected segments.
        present: usize,
    },
    /// Caller output could not retain the complete requested ranking.
    OutputCapacity(LexicalOutputError),
}

/// The one snapshot-pinned terminal for a segment-local lexical ranking.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LexicalTerminal<'manifest, 'output, 'bytes> {
    /// Every selected segment was present and healthy.
    Complete {
        /// Snapshot identity used by the query.
        snapshot: IndexSnapshotId,
        /// Source segment for the local document ordinals in `hits`.
        segment: LexicalSegmentId,
        /// Caller-owned ranked output borrowed for the terminal lifetime.
        hits: &'output [LexicalHit<'bytes>],
    },
    /// Some selected lexical segments were unavailable.
    Partial {
        /// Snapshot identity used by the query.
        snapshot: IndexSnapshotId,
        /// Source segment for the local document ordinals in `hits`.
        segment: LexicalSegmentId,
        /// Caller-owned ranked output borrowed for the terminal lifetime.
        hits: &'output [LexicalHit<'bytes>],
        /// Exact selected lexical segment identities not reached by the query.
        missing: &'manifest [LexicalSegmentId],
    },
    /// The route degraded while preserving the requested lexical snapshot.
    Degraded {
        /// Snapshot identity used by the query.
        snapshot: IndexSnapshotId,
        /// Source segment for the local document ordinals in `hits`.
        segment: LexicalSegmentId,
        /// Caller-owned ranked output borrowed for the terminal lifetime.
        hits: &'output [LexicalHit<'bytes>],
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

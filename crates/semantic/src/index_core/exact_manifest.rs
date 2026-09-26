use core::ops::Deref;

use super::exact::{ExactOperation, ExactSegment};
use super::selection::{SelectionAdmissionFault, validate_selection_admission};
use super::snapshot::IndexSnapshot;
use crate::index_vocabulary::{ExactSegmentId, IndexSnapshotId};

/// A manifest admission failure retaining the rejected positions and identity.
pub type ExactManifestError = SelectionAdmissionFault<ExactSegmentId>;

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
        validate_selection_admission(
            snapshot.exact,
            segments.len(),
            |index| segments[index].id,
            missing,
        )?;
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

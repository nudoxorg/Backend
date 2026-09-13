//! Worker request/reply protocol and the allocation-free in-memory backend.

#[cfg(test)]
use core::fmt;
use server_index_core::IndexSnapshotId;
use server_index_vocabulary::LexicalSegmentId;
use thiserror::Error;

use super::authority::{OrderingRecipe, QueryDigest, RouteId, SegmentOrdinal, TopK, WorkerId};
use super::merge::{MergeError, ObservedHit};
use super::planning::SegmentRange;

/// A fixed-width attempt ordinal.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct AttemptOrdinal(u8);

impl AttemptOrdinal {
    /// Creates an attempt ordinal.
    #[must_use]
    pub const fn new(value: u8) -> Self {
        Self(value)
    }

    /// Returns the zero-based attempt number.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}
/// A route attempt carrying every authority needed for worker validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RouteAttempt {
    /// Planned route identity.
    pub route: RouteId,
    /// Pinned snapshot identity.
    pub snapshot: IndexSnapshotId,
    /// Pinned query digest.
    pub query: QueryDigest,
    /// Canonical selected-segment position.
    pub segment_ordinal: SegmentOrdinal,
    /// Exact selected segment identity.
    pub segment: LexicalSegmentId,
    /// Exact row range proof served by this attempt.
    pub range: SegmentRange,
    /// Worker selected by rendezvous affinity for this attempt.
    pub worker: WorkerId,
    /// Zero-based retry ordinal.
    pub ordinal: AttemptOrdinal,
}

/// An observed worker reply over borrowed hit storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerReply<'rows, 'bytes> {
    /// Exact attempt this reply answers.
    pub attempt: RouteAttempt,
    /// Borrowed, untrusted worker hits; merge validation establishes only route consistency.
    pub hits: &'rows [ObservedHit<'bytes>],
}

impl<'rows, 'bytes> WorkerReply<'rows, 'bytes> {
    /// Creates a reply; [`merge_replies`] performs authority validation.
    #[must_use]
    pub const fn new(attempt: RouteAttempt, hits: &'rows [ObservedHit<'bytes>]) -> Self {
        Self { attempt, hits }
    }
}

/// A worker's explicit response.  Stale and unavailable are retryable states; malformed success
/// replies are never silently discarded and become [`ReplyError`] values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerResponse<'rows, 'bytes> {
    /// A success carrying rows with worker-claimed segment identities.
    Reply(WorkerReply<'rows, 'bytes>),
    /// The worker could not serve the pinned snapshot and should be retried.
    Stale {
        /// Exact attempt that observed the stale worker state.
        attempt: RouteAttempt,
        /// Snapshot observed by the worker, if known.
        observed: Option<IndexSnapshotId>,
    },
    /// The worker reports that the assigned segment/range is absent locally.
    Missing {
        /// Exact assignment attempt that was inspected.
        attempt: RouteAttempt,
    },
    /// The worker was unavailable for this bounded attempt.
    Unavailable {
        /// Exact attempt that could not be served.
        attempt: RouteAttempt,
    },
}

/// A worker boundary with no transport or runtime assumptions.
pub trait WorkerBackend {
    /// Executes one attempt and borrows returned rows from the worker's immutable storage.
    fn execute<'worker>(
        &'worker self,
        request: WorkerRequest<'_>,
    ) -> WorkerResponse<'worker, 'worker>;
}

/// Query input handed to one worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerRequest<'query> {
    /// Attempt authority.
    pub attempt: RouteAttempt,
    /// Borrowed query bytes.
    pub query: &'query [u8],
    /// Ordering recipe retained by the caller.
    pub ordering: OrderingRecipe,
    /// Result width.
    pub top_k: TopK,
}
/// A worker must expose its fixed identity for exact assignment checking.
pub trait IdentifiedWorker: WorkerBackend {
    /// Returns this backend's fixed identity.
    fn id(&self) -> WorkerId;
}

/// Convenience alias for the trait required by [`Coordinator::execute`].
pub trait RoutedWorker: WorkerBackend + IdentifiedWorker {}
impl<T: WorkerBackend + IdentifiedWorker> RoutedWorker for T {}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
/// Dispatch failure, including cancellation and the exact merge failure.
pub enum ExecuteError {
    /// Plan/query authorities differ.
    #[error("route plan does not match query or coordinator")]
    PlanMismatch,
    /// The supplied runtime worker directory was empty.
    #[error("runtime worker directory is empty")]
    NoWorkers,
    /// Cancellation won at a sampled dispatch boundary.
    #[error("horizontal dispatch was cancelled")]
    Cancelled,
    /// A worker reply or caller buffer was invalid.
    #[error("horizontal merge failed: {0}")]
    Merge(#[from] MergeError),
}
/// A small in-memory worker useful for tests and local execution.
#[cfg(test)]
pub struct InMemoryWorker<'hits> {
    id: WorkerId,
    hits: &'hits [ObservedHit<'hits>],
    stale: bool,
    unavailable: bool,
}

#[cfg(test)]
#[allow(
    clippy::elidable_lifetime_names,
    reason = "debug view borrows the worker hit fixture"
)]
impl<'hits> fmt::Debug for InMemoryWorker<'hits> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InMemoryWorker")
            .field("id", &self.id)
            .field("hits", &self.hits.len())
            .field("stale", &self.stale)
            .field("unavailable", &self.unavailable)
            .finish()
    }
}

#[cfg(test)]
impl<'hits> InMemoryWorker<'hits> {
    /// Creates a healthy worker over caller-owned immutable hits.
    #[must_use]
    pub const fn new(id: WorkerId, hits: &'hits [ObservedHit<'hits>]) -> Self {
        Self {
            id,
            hits,
            stale: false,
            unavailable: false,
        }
    }

    /// Marks this worker as returning explicit stale responses.
    #[must_use]
    pub const fn stale(mut self, value: bool) -> Self {
        self.stale = value;
        self
    }

    /// Marks this worker as returning explicit unavailable responses.
    #[must_use]
    pub const fn unavailable(mut self, value: bool) -> Self {
        self.unavailable = value;
        self
    }
}

#[cfg(test)]
impl WorkerBackend for InMemoryWorker<'_> {
    fn execute<'worker>(
        &'worker self,
        request: WorkerRequest<'_>,
    ) -> WorkerResponse<'worker, 'worker> {
        if self.unavailable {
            return WorkerResponse::Unavailable {
                attempt: request.attempt,
            };
        }
        if self.stale {
            return WorkerResponse::Stale {
                attempt: request.attempt,
                observed: None,
            };
        }
        WorkerResponse::Reply(WorkerReply::new(request.attempt, self.hits))
    }
}

#[cfg(test)]
impl IdentifiedWorker for InMemoryWorker<'_> {
    fn id(&self) -> WorkerId {
        self.id
    }
}

//! Snapshot-pinned planning and deterministic rendezvous affinity.

use backend_semantic::index_core::{EntityDocumentId, IndexSnapshot, IndexSnapshotId};
use backend_semantic::index_vocabulary::LexicalSegmentId;
use thiserror::Error;

use super::authority::{
    CANCELLATION_SAMPLE_INTERVAL, Cancellation, MAX_SEGMENTS, MAX_WORKERS, OrderingRecipe, Query,
    QueryDigest, RetryPolicy, RouteId, SegmentOrdinal, TopK, WorkerId,
};
use super::merge::{
    MergeError, MergeOutcome, MissingAssignment, ReplyError, RoutedHitSlot, merge_optional_replies,
    validate_reply,
};
use super::worker::{
    AttemptOrdinal, ExecuteError, IdentifiedWorker, RouteAttempt, WorkerBackend, WorkerRequest,
    WorkerResponse,
};

/// One exact segment assignment and its rendezvous-ordered candidates.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentAssignment {
    /// Canonical segment position.
    pub ordinal: SegmentOrdinal,
    /// Exact selected segment identity.
    pub segment: LexicalSegmentId,
    /// Exact row range assigned to this worker.
    pub range: SegmentRange,
    candidate_count: u8,
    candidates: [WorkerId; MAX_WORKERS],
}

impl SegmentAssignment {
    /// Returns the number of retained retry candidates.
    #[must_use]
    pub fn candidate_count(self) -> usize {
        usize::from(self.candidate_count)
    }

    /// Returns a candidate worker by retry ordinal.
    #[must_use]
    pub fn candidate(self, ordinal: usize) -> Option<WorkerId> {
        if ordinal < self.candidate_count() {
            self.candidates.get(ordinal).copied()
        } else {
            None
        }
    }
}

/// A snapshot/query-bound route plan with exact segment assignments.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoutePlan<'selection> {
    pub(crate) snapshot: IndexSnapshotId,
    pub(crate) segments: &'selection [LexicalSegmentId],
    pub(crate) query: QueryDigest,
    pub(crate) ordering: OrderingRecipe,
    pub(crate) top_k: TopK,
    pub(crate) route: RouteId,
    pub(crate) assignment_count: u8,
    pub(crate) assignments: [Option<SegmentAssignment>; MAX_SEGMENTS],
}

impl<'selection> RoutePlan<'selection> {
    /// Returns the snapshot authority.
    #[must_use]
    pub const fn snapshot(self) -> IndexSnapshotId {
        self.snapshot
    }

    /// Returns the exact query digest.
    #[must_use]
    pub const fn query(self) -> QueryDigest {
        self.query
    }

    /// Returns the route identity.
    #[must_use]
    pub const fn route(self) -> RouteId {
        self.route
    }

    /// Returns the canonical selected segment slice.
    #[must_use]
    pub const fn segments(self) -> &'selection [LexicalSegmentId] {
        self.segments
    }

    /// Returns one exact assignment by canonical segment ordinal.
    #[must_use]
    pub fn assignment(self, ordinal: usize) -> Option<SegmentAssignment> {
        if ordinal < usize::from(self.assignment_count) {
            self.assignments.get(ordinal).copied().flatten()
        } else {
            None
        }
    }
}

/// A fixed row-range proof carried by every assignment and reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentRange {
    /// The complete immutable segment.
    Whole,
}

impl SegmentRange {
    /// Creates the proof for a complete immutable segment.
    #[must_use]
    pub const fn whole() -> Self {
        Self::Whole
    }

    /// Returns whether this proof covers the complete immutable segment.
    #[must_use]
    pub const fn is_whole(self) -> bool {
        matches!(self, Self::Whole)
    }
}

/// Stateless planner using deterministic rendezvous affinity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Coordinator<'selection, 'workers> {
    snapshot: IndexSnapshotId,
    segments: &'selection [LexicalSegmentId],
    workers: &'workers [WorkerId],
    retry: RetryPolicy,
}

impl<'selection, 'workers> Coordinator<'selection, 'workers> {
    /// Validates one snapshot selection and worker directory before planning.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError`] when the snapshot selection or worker directory exceeds its fixed
    /// bounds, is empty, or contains duplicates.
    #[allow(
        clippy::indexing_slicing,
        reason = "validated bounded snapshot and worker slices"
    )]
    pub fn new(
        snapshot: IndexSnapshot<'selection>,
        workers: &'workers [WorkerId],
        retry: RetryPolicy,
    ) -> Result<Self, PlanError> {
        let segments = snapshot.lexical;
        if segments.len() > MAX_SEGMENTS {
            return Err(PlanError::Segments {
                observed: segments.len(),
            });
        }
        if workers.is_empty() {
            return Err(PlanError::NoWorkers);
        }
        if workers.len() > MAX_WORKERS {
            return Err(PlanError::Workers {
                observed: workers.len(),
            });
        }
        if let Some((left, right)) = duplicate_position(segments) {
            return Err(PlanError::DuplicateSegment {
                left,
                right,
                id: segments[left],
            });
        }
        if let Some((left, right)) = duplicate_position(workers) {
            return Err(PlanError::DuplicateWorker {
                left,
                right,
                id: workers[left],
            });
        }
        Ok(Self {
            snapshot: snapshot.id,
            segments,
            workers,
            retry,
        })
    }

    /// Builds exact assignments in the immutable selection order.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError`] when a fixed route array cannot represent the selected assignments.
    #[allow(
        clippy::indexing_slicing,
        reason = "fixed route arrays are bounded by MAX constants"
    )]
    pub fn plan(&self, query: Query<'_>) -> Result<RoutePlan<'selection>, PlanError> {
        let route = RouteId::from_raw(route_hash(
            self.snapshot,
            query.digest(),
            query.ordering(),
            query.top_k(),
        ));
        let mut assignments = [None; MAX_SEGMENTS];
        for (ordinal, segment) in self.segments.iter().copied().enumerate() {
            let mut candidates = [WorkerId::new(0); MAX_WORKERS];
            for (position, worker) in self.workers.iter().copied().enumerate() {
                candidates[position] = worker;
            }
            sort_candidates(
                &mut candidates[..self.workers.len()],
                self.snapshot,
                segment,
            );
            let ordinal_value = SegmentOrdinal::new(ordinal).map_err(|_| PlanError::Segments {
                observed: self.segments.len(),
            })?;
            let candidate_count =
                u8::try_from(self.workers.len()).map_err(|_| PlanError::Workers {
                    observed: self.workers.len(),
                })?;
            assignments[ordinal] = Some(SegmentAssignment {
                ordinal: ordinal_value,
                segment,
                range: SegmentRange::whole(),
                candidate_count,
                candidates,
            });
        }
        let assignment_count =
            u8::try_from(self.segments.len()).map_err(|_| PlanError::Segments {
                observed: self.segments.len(),
            })?;
        Ok(RoutePlan {
            snapshot: self.snapshot,
            segments: self.segments,
            query: query.digest(),
            ordering: query.ordering(),
            top_k: query.top_k(),
            route,
            assignment_count,
            assignments,
        })
    }

    /// Returns the retry policy used by this planner.
    #[must_use]
    pub const fn retry_policy(self) -> RetryPolicy {
        self.retry
    }

    /// Executes all assignments through a borrowed worker directory and merges the replies.
    ///
    /// # Errors
    ///
    /// Returns [`ExecuteError`] when the plan/query authorities differ, cancellation is sampled,
    /// a worker response is malformed, or caller-owned merge storage is insufficient.
    #[allow(
        clippy::indexing_slicing,
        clippy::needless_range_loop,
        reason = "assignment ordinals are bounded by validated fixed arrays"
    )]
    pub fn execute<'worker_rows, 'missing, 'output, W: WorkerBackend + IdentifiedWorker>(
        &self,
        plan: RoutePlan<'selection>,
        query: Query<'_>,
        workers: &'worker_rows [W],
        cancellation: &Cancellation,
        seen: &mut [Option<EntityDocumentId>],
        missing: &'missing mut [MissingAssignment],
        output: &'output mut [RoutedHitSlot<'worker_rows>],
    ) -> Result<MergeOutcome<'missing, 'output, 'worker_rows>, ExecuteError> {
        if plan.snapshot != self.snapshot
            || plan.query != query.digest()
            || plan.ordering != query.ordering()
            || plan.top_k != query.top_k()
        {
            return Err(ExecuteError::PlanMismatch);
        }
        if workers.is_empty() {
            return Err(ExecuteError::NoWorkers);
        }
        let mut replies = [None; MAX_SEGMENTS];
        for ordinal in 0..usize::from(plan.assignment_count) {
            if cancellation.is_cancelled() {
                return Err(ExecuteError::Cancelled);
            }
            let Some(assignment) = plan.assignment(ordinal) else {
                return Err(ExecuteError::PlanMismatch);
            };
            let attempts = core::cmp::min(
                usize::from(self.retry.attempts()),
                assignment.candidate_count(),
            );
            for attempt_number in 0..attempts {
                if attempt_number % CANCELLATION_SAMPLE_INTERVAL == 0 && cancellation.is_cancelled()
                {
                    return Err(ExecuteError::Cancelled);
                }
                let Some(worker_id) = assignment.candidate(attempt_number) else {
                    break;
                };
                let Some(worker) = workers.iter().find(|worker| worker.id() == worker_id) else {
                    continue;
                };
                let attempt = RouteAttempt {
                    route: plan.route,
                    snapshot: plan.snapshot,
                    query: plan.query,
                    segment_ordinal: assignment.ordinal,
                    segment: assignment.segment,
                    range: assignment.range,
                    worker: worker_id,
                    ordinal: AttemptOrdinal::new(
                        u8::try_from(attempt_number).map_err(|_| ExecuteError::PlanMismatch)?,
                    ),
                };
                let request = WorkerRequest {
                    attempt,
                    query: query.bytes(),
                    ordering: query.ordering(),
                    top_k: plan.top_k,
                };
                match worker.execute(request) {
                    WorkerResponse::Reply(reply) => {
                        if reply.attempt != attempt {
                            return Err(ExecuteError::Merge(MergeError::Reply(
                                ReplyError::ResponseAttempt,
                            )));
                        }
                        validate_reply(&plan, reply)
                            .map_err(|error| ExecuteError::Merge(MergeError::Reply(error)))?;
                        replies[ordinal] = Some(reply);
                        break;
                    }
                    WorkerResponse::Stale {
                        attempt: observed, ..
                    }
                    | WorkerResponse::Missing { attempt: observed }
                    | WorkerResponse::Unavailable { attempt: observed } => {
                        if observed != attempt {
                            return Err(ExecuteError::Merge(MergeError::Reply(
                                ReplyError::ResponseAttempt,
                            )));
                        }
                    }
                }
            }
        }
        merge_optional_replies(&plan, &replies, seen, missing, output, Some(cancellation)).map_err(
            |error| match error {
                MergeError::Cancelled => ExecuteError::Cancelled,
                other => ExecuteError::Merge(other),
            },
        )
    }
}
#[allow(
    clippy::indexing_slicing,
    reason = "duplicate scan is bounded by fixed snapshot/worker limits"
)]
fn duplicate_position<T: PartialEq>(values: &[T]) -> Option<(usize, usize)> {
    for left in 0..values.len() {
        for right in left + 1..values.len() {
            if values[left] == values[right] {
                return Some((left, right));
            }
        }
    }
    None
}

#[allow(
    clippy::indexing_slicing,
    reason = "candidate count is bounded by MAX_WORKERS"
)]
fn sort_candidates(
    candidates: &mut [WorkerId],
    snapshot: IndexSnapshotId,
    segment: LexicalSegmentId,
) {
    for index in 1..candidates.len() {
        let candidate = candidates[index];
        let mut position = index;
        while position > 0
            && (candidate_score(snapshot, segment, candidate)
                > candidate_score(snapshot, segment, candidates[position - 1])
                || (candidate_score(snapshot, segment, candidate)
                    == candidate_score(snapshot, segment, candidates[position - 1])
                    && candidate > candidates[position - 1]))
        {
            candidates[position] = candidates[position - 1];
            position -= 1;
        }
        candidates[position] = candidate;
    }
}

fn candidate_score(snapshot: IndexSnapshotId, segment: LexicalSegmentId, worker: WorkerId) -> u64 {
    let mut hasher = blake3::Hasher::new();
    hasher.update(snapshot.as_ref());
    hasher.update(segment.as_ref());
    hasher.update(&worker.get().to_le_bytes());
    let digest = hasher.finalize();
    let mut prefix = [0_u8; 8];
    prefix.copy_from_slice(&digest.as_bytes()[..8]);
    u64::from_le_bytes(prefix)
}

fn route_hash(
    snapshot: IndexSnapshotId,
    query: QueryDigest,
    ordering: OrderingRecipe,
    top_k: TopK,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(snapshot.as_ref());
    hasher.update(&query.as_bytes());
    hasher.update(&ordering.get().to_le_bytes());
    hasher.update(&top_k.get().to_le_bytes());
    let digest = hasher.finalize();
    *digest.as_bytes()
}

/// Route-plan admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum PlanError {
    /// Segment directory exceeds the bounded route width.
    #[error("selected segment count {observed} exceeds {MAX_SEGMENTS}")]
    Segments {
        /// Complete observed segment count.
        observed: usize,
    },
    /// Worker directory is empty.
    #[error("at least one worker is required")]
    NoWorkers,
    /// Worker directory exceeds the bounded candidate width.
    #[error("worker count {observed} exceeds {MAX_WORKERS}")]
    Workers {
        /// Complete observed worker count.
        observed: usize,
    },
    /// A segment identity is repeated.
    #[error("segment {id:?} is repeated at positions {left} and {right}")]
    DuplicateSegment {
        /// Earlier position.
        left: usize,
        /// Later position.
        right: usize,
        /// Repeated identity.
        id: LexicalSegmentId,
    },
    /// A worker identity is repeated.
    #[error("worker {id:?} is repeated at positions {left} and {right}")]
    DuplicateWorker {
        /// Earlier position.
        left: usize,
        /// Later position.
        right: usize,
        /// Repeated identity.
        id: WorkerId,
    },
}

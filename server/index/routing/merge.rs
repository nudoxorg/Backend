//! Provenance validation, deterministic ranking, deduplication, and terminal state.

use core::cmp::Ordering;
use core::mem::size_of;
use server_index_core::{EntityDocumentId, IndexSnapshotId};
use server_index_vocabulary::LexicalSegmentId;
use thiserror::Error;

use super::authority::{
    CANCELLATION_SAMPLE_INTERVAL, Cancellation, MAX_SEGMENTS, SegmentOrdinal, WorkerId,
};
use super::planning::{RoutePlan, SegmentRange};
use super::worker::WorkerReply;

/// One untrusted worker-produced hit carrying a claimed segment identity.
///
/// A worker cannot make this value a routed result by itself. [`merge_replies`] validates the
/// route, snapshot, query, assignment, range, and segment before creating [`MergedHit`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedHit<'bytes> {
    /// Segment that the worker claims produced this hit.
    pub segment: LexicalSegmentId,
    /// Matching term borrowed from the worker's immutable segment.
    pub term: &'bytes [u8],
    /// Stable document identity supplied by the worker.
    pub document: EntityDocumentId,
    /// Integer relevance units from the bound ordering recipe.
    pub score: u32,
}

/// One deterministic merged result row after route/provenance validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MergedHit<'bytes> {
    /// Segment assigned to the worker row after route validation.
    segment: LexicalSegmentId,
    /// Matching term borrowed from the observed worker storage.
    term: &'bytes [u8],
    /// Document identity retained after route validation.
    document: EntityDocumentId,
    /// Integer relevance units.
    score: u32,
}

/// Public merged result row type.
pub type RoutedHit<'bytes> = MergedHit<'bytes>;

/// Caller-owned output slot filled only by merge validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RoutedHitSlot<'bytes>(Option<RoutedHit<'bytes>>);

impl<'bytes> RoutedHitSlot<'bytes> {
    /// Creates a vacant caller-owned slot.
    #[must_use]
    pub const fn vacant() -> Self {
        Self(None)
    }

    /// Returns the merged row when this slot was filled.
    #[must_use]
    pub const fn get(self) -> Option<RoutedHit<'bytes>> {
        self.0
    }
}

/// An unresolved canonical assignment after the bounded retry budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MissingAssignment {
    /// Canonical selection ordinal.
    pub ordinal: SegmentOrdinal,
    /// Exact selected segment identity.
    pub segment: LexicalSegmentId,
    /// Exact range that remained unresolved.
    pub range: SegmentRange,
}

impl<'bytes> MergedHit<'bytes> {
    pub(crate) const fn from_observed(hit: ObservedHit<'bytes>) -> Self {
        Self {
            segment: hit.segment,
            term: hit.term,
            document: hit.document,
            score: hit.score,
        }
    }

    /// Returns the route-validated segment identity.
    #[must_use]
    pub const fn segment(self) -> LexicalSegmentId {
        self.segment
    }
    /// Returns the borrowed matching term.
    #[must_use]
    pub const fn term(self) -> &'bytes [u8] {
        self.term
    }
    /// Returns the stable document identity.
    #[must_use]
    pub const fn document(self) -> EntityDocumentId {
        self.document
    }
    /// Returns the integer relevance score.
    #[must_use]
    pub const fn score(self) -> u32 {
        self.score
    }
}
/// Merges already returned replies.  This is public so transports can validate and merge without
/// coupling the coordinator to their own worker lifecycle.
///
/// # Errors
///
/// Returns [`MergeError`] for malformed authority/assignment replies, duplicate claims, sampled
/// cancellation, or insufficient caller-owned scratch/output capacity.
#[allow(
    clippy::elidable_lifetime_names,
    clippy::indexing_slicing,
    reason = "explicit lifetimes preserve independent reply rows/bytes and output borrows; slices are preflight-bounded"
)]
pub fn merge_replies<'plan, 'rows, 'bytes, 'missing, 'output>(
    plan: &RoutePlan<'plan>,
    replies: &[WorkerReply<'rows, 'bytes>],
    seen: &mut [Option<EntityDocumentId>],
    missing: &'missing mut [MissingAssignment],
    output: &'output mut [RoutedHitSlot<'bytes>],
) -> Result<MergeOutcome<'missing, 'output, 'bytes>, MergeError> {
    if replies.len() > MAX_SEGMENTS {
        return Err(MergeError::TooManyReplies {
            observed: replies.len(),
        });
    }
    let mut slots = [None; MAX_SEGMENTS];
    for reply in replies.iter().copied() {
        validate_reply(plan, reply).map_err(MergeError::Reply)?;
        let ordinal = reply.attempt.segment_ordinal.get();
        let Some(slot) = slots.get_mut(ordinal) else {
            return Err(MergeError::PlanMismatch);
        };
        if slot.is_some() {
            return Err(MergeError::DuplicateReply { ordinal });
        }
        *slot = Some(reply);
    }
    merge_optional_replies(plan, &slots, seen, missing, output, None)
}

#[allow(
    clippy::elidable_lifetime_names,
    clippy::indexing_slicing,
    reason = "all slices are bounded by preflight capacities before mutation"
)]
pub(crate) fn merge_optional_replies<'plan, 'rows, 'bytes, 'missing, 'output>(
    plan: &RoutePlan<'plan>,
    replies: &[Option<WorkerReply<'rows, 'bytes>>; MAX_SEGMENTS],
    seen: &mut [Option<EntityDocumentId>],
    missing: &'missing mut [MissingAssignment],
    output: &'output mut [RoutedHitSlot<'bytes>],
    cancellation: Option<&Cancellation>,
) -> Result<MergeOutcome<'missing, 'output, 'bytes>, MergeError> {
    let required_missing = plan
        .segments
        .iter()
        .enumerate()
        .filter(|(ordinal, _)| replies.get(*ordinal).copied().flatten().is_none())
        .count();
    if missing.len() < required_missing {
        return Err(MergeError::MissingCapacity {
            required: required_missing,
            available: missing.len(),
        });
    }
    let mut total_hits = 0_usize;
    for ordinal in 0..plan.segments.len() {
        if ordinal % CANCELLATION_SAMPLE_INTERVAL == 0
            && cancellation.is_some_and(Cancellation::is_cancelled)
        {
            return Err(MergeError::Cancelled);
        }
        let reply = replies.get(ordinal).copied().flatten();
        let Some(reply) = reply else {
            continue;
        };
        validate_reply(plan, reply).map_err(MergeError::Reply)?;
        total_hits = total_hits
            .checked_add(reply.hits.len())
            .ok_or(MergeError::HitCountOverflow)?;
    }
    if plan.top_k.get() != 0 && seen.len() < total_hits {
        return Err(MergeError::ScratchCapacity {
            required: total_hits,
            available: seen.len(),
        });
    }
    let preflight_required = core::cmp::min(total_hits, plan.top_k.get());
    for cell in &mut seen[..preflight_required] {
        *cell = None;
    }
    let unique = match preflight_unique(
        plan,
        replies,
        plan.top_k.get(),
        &mut seen[..preflight_required],
        cancellation,
    ) {
        Ok(unique) => unique,
        Err(error) => {
            for cell in &mut seen[..preflight_required] {
                *cell = None;
            }
            return Err(error);
        }
    };
    let required = core::cmp::min(unique, plan.top_k.get());
    if output.len() < required {
        for cell in &mut seen[..preflight_required] {
            *cell = None;
        }
        return Err(MergeError::OutputCapacity {
            required,
            available: output.len(),
        });
    }
    if cancellation.is_some_and(Cancellation::is_cancelled) {
        for cell in &mut seen[..preflight_required] {
            *cell = None;
        }
        return Err(MergeError::Cancelled);
    }
    for cell in &mut seen[..preflight_required] {
        *cell = None;
    }
    let mut missing_count = 0_usize;
    for ordinal in 0..plan.segments.len() {
        if replies.get(ordinal).copied().flatten().is_none() {
            let Some(segment) = plan.segments.get(ordinal).copied() else {
                return Err(MergeError::PlanMismatch);
            };
            let Some(assignment) = plan.assignment(ordinal) else {
                return Err(MergeError::PlanMismatch);
            };
            missing[missing_count] = MissingAssignment {
                ordinal: assignment.ordinal,
                segment,
                range: assignment.range,
            };
            missing_count += 1;
        }
    }
    if plan.top_k.get() != 0 {
        for cell in &mut seen[..total_hits] {
            *cell = None;
        }
    }
    if plan.top_k.get() == 0 {
        return Ok(MergeOutcome {
            snapshot: plan.snapshot,
            hits: &output[..0],
            missing: &missing[..missing_count],
        });
    }
    let mut count = 0_usize;
    for ordinal in 0..plan.segments.len() {
        let Some(reply) = replies.get(ordinal).copied().flatten() else {
            continue;
        };
        for &hit in reply.hits {
            if !remember_open(&mut seen[..total_hits], hit.document)? {
                continue;
            }
            let position = match output[..count].iter().position(|current| {
                current
                    .get()
                    .is_some_and(|current| rank(hit, &current).is_lt())
            }) {
                Some(position) => position,
                None => count,
            };
            if position < required {
                let next = core::cmp::min(count + 1, required);
                for destination in (position + 1..next).rev() {
                    output[destination] = output[destination - 1];
                }
                output[position] = RoutedHitSlot(Some(MergedHit::from_observed(hit)));
                count = next;
            }
        }
    }
    Ok(MergeOutcome {
        snapshot: plan.snapshot,
        hits: &output[..count],
        missing: &missing[..missing_count],
    })
}

#[allow(
    clippy::elidable_lifetime_names,
    clippy::indexing_slicing,
    reason = "open-addressing table is checked for capacity before probing"
)]
fn preflight_unique<'plan, 'rows, 'bytes>(
    plan: &RoutePlan<'plan>,
    replies: &[Option<WorkerReply<'rows, 'bytes>>; MAX_SEGMENTS],
    limit: usize,
    table: &mut [Option<EntityDocumentId>],
    cancellation: Option<&Cancellation>,
) -> Result<usize, MergeError> {
    if limit == 0 {
        return Ok(0);
    }
    let mut unique = 0_usize;
    let mut processed = 0_usize;
    for ordinal in 0..plan.segments.len() {
        let Some(reply) = replies.get(ordinal).copied().flatten() else {
            continue;
        };
        for hit in reply.hits {
            if processed.is_multiple_of(CANCELLATION_SAMPLE_INTERVAL)
                && cancellation.is_some_and(Cancellation::is_cancelled)
            {
                return Err(MergeError::Cancelled);
            }
            processed += 1;
            if insert_open(table, hit.document)? {
                unique += 1;
                if unique >= limit {
                    return Ok(unique);
                }
            }
        }
    }
    Ok(unique)
}

#[allow(
    clippy::indexing_slicing,
    reason = "probe index is modulo checked scratch length"
)]
fn remember_open(
    seen: &mut [Option<EntityDocumentId>],
    document: EntityDocumentId,
) -> Result<bool, MergeError> {
    if seen.is_empty() {
        return Err(MergeError::ScratchProbeExhausted { capacity: 0 });
    }
    let mut index = document_hash(document) % seen.len();
    for _ in 0..seen.len() {
        match seen[index] {
            None => {
                seen[index] = Some(document);
                return Ok(true);
            }
            Some(current) if current == document => return Ok(false),
            Some(_) => {
                index = (index + 1) % seen.len();
            }
        }
    }
    Err(MergeError::ScratchProbeExhausted {
        capacity: seen.len(),
    })
}

#[allow(
    clippy::indexing_slicing,
    reason = "probe index is modulo checked scratch length"
)]
fn insert_open(
    table: &mut [Option<EntityDocumentId>],
    document: EntityDocumentId,
) -> Result<bool, MergeError> {
    if table.is_empty() {
        return Err(MergeError::ScratchProbeExhausted { capacity: 0 });
    }
    let mut index = document_hash(document) % table.len();
    for _ in 0..table.len() {
        match table[index] {
            None => {
                table[index] = Some(document);
                return Ok(true);
            }
            Some(current) if current == document => return Ok(false),
            Some(_) => index = (index + 1) % table.len(),
        }
    }
    Err(MergeError::ScratchProbeExhausted {
        capacity: table.len(),
    })
}

#[allow(
    clippy::indexing_slicing,
    reason = "BLAKE3 digest is fixed at 32 bytes"
)]
fn document_hash(document: EntityDocumentId) -> usize {
    let digest = blake3::hash(&<[u8; server_index_core::ENTITY_DOCUMENT_ID_BYTES]>::from(
        document,
    ));
    let mut bytes = [0_u8; size_of::<usize>()];
    let width = bytes.len();
    bytes.copy_from_slice(&digest.as_bytes()[..width]);
    usize::from_le_bytes(bytes)
}

fn rank(left: ObservedHit<'_>, right: &RoutedHit<'_>) -> Ordering {
    right
        .score
        .cmp(&left.score)
        .then_with(|| left.document.cmp(&right.document()))
        .then_with(|| left.segment.cmp(&right.segment()))
        .then_with(|| left.term.cmp(right.term()))
}

#[allow(
    clippy::elidable_lifetime_names,
    reason = "reply lifetime is carried into observed hit storage"
)]
pub(crate) fn validate_reply<'plan, 'rows, 'bytes>(
    plan: &RoutePlan<'plan>,
    reply: WorkerReply<'rows, 'bytes>,
) -> Result<(), ReplyError> {
    if reply.hits.len() > plan.top_k.get() {
        return Err(ReplyError::HitCount);
    }
    if reply.attempt.snapshot != plan.snapshot {
        return Err(ReplyError::AttemptSnapshot);
    }
    if reply.attempt.query != plan.query {
        return Err(ReplyError::AttemptQuery);
    }
    if reply.attempt.route != plan.route {
        return Err(ReplyError::Route);
    }
    let ordinal = reply.attempt.segment_ordinal.get();
    let Some(assignment) = plan.assignment(ordinal) else {
        return Err(ReplyError::Assignment);
    };
    if assignment.segment != reply.attempt.segment {
        return Err(ReplyError::Assignment);
    }
    if assignment.range != reply.attempt.range {
        return Err(ReplyError::Range);
    }
    let attempt = usize::from(reply.attempt.ordinal.get());
    let Some(expected_worker) = assignment.candidate(attempt) else {
        return Err(ReplyError::Attempt);
    };
    if expected_worker != reply.attempt.worker {
        return Err(ReplyError::Worker {
            expected: expected_worker,
            observed: reply.attempt.worker,
        });
    }
    for hit in reply.hits {
        if hit.segment != assignment.segment {
            return Err(ReplyError::HitSegment {
                expected: assignment.segment,
                observed: hit.segment,
            });
        }
    }
    for (index, hit) in reply.hits.iter().enumerate() {
        if reply
            .hits
            .iter()
            .take(index)
            .any(|prior| prior.document == hit.document)
        {
            return Err(ReplyError::DuplicateDocument);
        }
    }
    Ok(())
}
/// Exact result of a successful merge, including unresolved selected segments in canonical order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MergeOutcome<'missing, 'output, 'hits> {
    /// Snapshot authority retained by every emitted row.
    pub snapshot: IndexSnapshotId,
    /// Ranked, deduplicated caller-owned output.
    pub hits: &'output [RoutedHitSlot<'hits>],
    /// Selected segments not successfully served after the retry bound.
    pub missing: &'missing [MissingAssignment],
}

impl MergeOutcome<'_, '_, '_> {
    /// Returns whether one or more selected segments were unresolved.
    #[must_use]
    pub const fn is_partial(self) -> bool {
        !self.missing.is_empty()
    }
}

/// A malformed worker reply.  Every mismatch is retained as a typed failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum ReplyError {
    /// Worker returned more rows than the exact request bound.
    #[error("worker reply exceeds the requested top-k bound")]
    HitCount,
    /// A terminal worker response named an attempt other than the dispatched attempt.
    #[error("worker terminal response names a different attempt")]
    ResponseAttempt,
    /// Attempt snapshot differs from the plan.
    #[error("reply attempt snapshot differs from the route plan")]
    AttemptSnapshot,
    /// Attempt query digest differs from the plan.
    #[error("reply attempt query differs from the route plan")]
    AttemptQuery,
    /// Reply route differs from the plan.
    #[error("reply route differs from the route plan")]
    Route,
    /// Reply did not identify one selected assignment.
    #[error("reply assignment is not selected by the route plan")]
    Assignment,
    /// Reply range differs from the exact assignment proof.
    #[error("reply range differs from the route assignment")]
    Range,
    /// Reply attempt exceeds the exact retry candidates.
    #[error("reply attempt is outside the bounded retry candidates")]
    Attempt,
    /// Worker claimed a missing assignment other than the requested one.
    #[error("worker missing response names another assignment")]
    MissingAssignment,
    /// Reply worker differs from its exact rendezvous candidate.
    #[error("reply worker {observed:?} differs from expected {expected:?}")]
    Worker {
        /// Expected worker.
        expected: WorkerId,
        /// Observed worker.
        observed: WorkerId,
    },
    /// Hit provenance differs from its assigned segment.
    #[error("hit segment differs from its assigned segment")]
    HitSegment {
        /// Expected segment.
        expected: LexicalSegmentId,
        /// Observed segment.
        observed: LexicalSegmentId,
    },
    /// One worker reply repeats a document identity, leaving row precedence ambiguous.
    #[error("worker reply repeats a document identity")]
    DuplicateDocument,
}

/// Merge failure retaining malformed replies and caller-capacity faults.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum MergeError {
    /// Cancellation was sampled during bounded merge work.
    #[error("horizontal merge was cancelled")]
    Cancelled,
    /// A reply failed authority or provenance validation.
    #[error("worker reply is invalid: {0}")]
    Reply(#[from] ReplyError),
    /// More replies were supplied than the fixed selection width can represent.
    #[error("reply count {observed} exceeds {MAX_SEGMENTS}")]
    TooManyReplies {
        /// Complete observed reply count.
        observed: usize,
    },
    /// Two replies claimed one canonical segment assignment.
    #[error("multiple replies claimed segment ordinal {ordinal}")]
    DuplicateReply {
        /// Claimed canonical segment ordinal.
        ordinal: usize,
    },
    /// Replies were not representable by the route plan.
    #[error("route plan is internally inconsistent")]
    PlanMismatch,
    /// Missing-output scratch was too small.
    #[error("missing-segment scratch has {available} slots, requires {required}")]
    MissingCapacity {
        /// Exact required missing-segment slots.
        required: usize,
        /// Supplied missing-segment slots.
        available: usize,
    },
    /// Hit count overflowed the bounded merge counter.
    #[error("worker hit count overflow")]
    HitCountOverflow,
    /// Deduplication scratch was too small.
    #[error("deduplication scratch has {available} slots, requires {required}")]
    ScratchCapacity {
        /// Exact required deduplication slots.
        required: usize,
        /// Supplied deduplication slots.
        available: usize,
    },
    /// The fixed deduplication probe had no free slot for a new document.
    #[error("deduplication scratch probe exhausted at {capacity} slots")]
    ScratchProbeExhausted {
        /// Number of slots probed.
        capacity: usize,
    },
    /// Ranked output was too small.
    #[error("ranked output has {available} slots, requires {required}")]
    OutputCapacity {
        /// Exact required ranked-output slots.
        required: usize,
        /// Supplied ranked-output slots.
        available: usize,
    },
}

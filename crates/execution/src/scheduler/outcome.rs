//! Scheduler outcomes and errors.

use super::guard::Scheduled;
use crate::{
    AdmissionError, AttemptError, AttemptFence, HedgeError, Interned, OutputVersion,
    PlacementDecision, ReusableOutput, WorkKey,
};
use backend_version::Relation;
use std::sync::Arc;

/// Reusable-output or newly scheduled result.
#[must_use]
pub enum ScheduleOutcome<R: Relation> {
    /// A validated output already exists for the exact key and authority.
    Reused(ReusableOutput),
    /// A new attempt owns route and publication guards.
    Scheduled(Box<Scheduled<R>>),
    /// A bounded follower demand is attached to an existing live attempt.
    /// Poll [`Interned::reusable_output`] until the leader publishes.
    Waiting(Interned),
}

impl<R: Relation> std::fmt::Debug for ScheduleOutcome<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reused(output) => f.debug_tuple("Reused").field(output).finish(),
            Self::Scheduled(scheduled) => f.debug_tuple("Scheduled").field(scheduled).finish(),
            Self::Waiting(waiter) => f.debug_tuple("Waiting").field(waiter).finish(),
        }
    }
}

/// Receipt returned after one scheduled result is accepted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleReceipt {
    /// Semantic key that completed.
    pub(super) key: WorkKey,
    /// Accepted immutable output.
    pub(super) output: OutputVersion,
    /// Reusable capability when semantic dependency freshness was admitted.
    /// One-shot completion receipts keep their canonical bytes below without
    /// manufacturing a reusable context.
    pub(super) reusable: Option<ReusableOutput>,
    /// Canonical bytes retained by this completion receipt for its owner.
    pub(super) canonical_bytes: Arc<Vec<u8>>,
    /// Route selected for the attempt.
    pub(super) decision: PlacementDecision,
    /// Fence that won publication.
    pub(super) fence: AttemptFence,
    /// Attempt ordinal that won publication.
    pub(super) ordinal: u32,
    /// Cache entries evicted while this publication was installed. Owners of
    /// authority/replay metadata use this list to retire matching state.
    pub(super) evicted_keys: Vec<WorkKey>,
}

impl ScheduleReceipt {
    /// Returns the completed semantic work key.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns the accepted immutable output.
    #[must_use]
    pub const fn output(&self) -> OutputVersion {
        self.output
    }

    /// Returns the retained output capability installed by publication.
    #[must_use]
    pub const fn reusable(&self) -> Option<&ReusableOutput> {
        self.reusable.as_ref()
    }

    /// Returns the canonical bytes retained by this completion receipt.
    #[must_use]
    pub fn canonical_bytes_arc(&self) -> Arc<Vec<u8>> {
        Arc::clone(&self.canonical_bytes)
    }

    /// Returns the route that produced the accepted output.
    #[must_use]
    pub const fn decision(&self) -> PlacementDecision {
        self.decision
    }

    /// Returns the accepted attempt's opaque fence.
    #[must_use]
    pub const fn fence(&self) -> AttemptFence {
        self.fence
    }

    /// Returns the accepted attempt's ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns cache keys evicted by this publication.
    #[must_use]
    pub fn evicted_keys(&self) -> &[WorkKey] {
        &self.evicted_keys
    }
}

/// Failure while selecting or completing a route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ScheduleError {
    /// Resource admission failed.
    Admission(AdmissionError),
    /// Attempt ownership or receipt validation failed.
    Attempt(AttemptError),
    /// No authorized remote route exists.
    RemoteOnlyUnavailable,
    /// The local dependency closure is incomplete and no remote route exists.
    MissingLocalInput,
    /// Neither local capability nor an authorized remote route exists.
    CapabilityUnavailable,
    /// Request key did not match its identity.
    KeyMismatch,
    /// Hedge requested for a non-repeatable recipe.
    HedgeNotAllowed,
    /// A hedge candidate failed validation or lost the winner race.
    Hedge(HedgeError),
    /// An exact validated output was found by `schedule`'s reuse guard.
    Reusable(Box<ReusableOutput>),
    /// Coalesced work is currently owned by another leader.
    Coalesced,
    /// A local fallback deadline has not elapsed yet.
    FallbackNotDue,
    /// No local fallback reservation is available.
    FallbackUnavailable,
    /// A capability or cost snapshot was bound to a different work key.
    ObservationMismatch,
}

impl std::fmt::Display for ScheduleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "schedule error: {self:?}")
    }
}

impl std::error::Error for ScheduleError {}

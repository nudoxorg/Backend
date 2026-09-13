//! Attempt leases, fences, and the explicit attempt state machine.

use crate::WorkKey;
use crate::types::IdentityBinding;
use std::fmt;

/// Opaque publication fence minted only by a [`crate::AttemptManager`].
///
/// A fence is deliberately not an integer supplied by a worker. A worker may
/// replay an observed fence, but it cannot manufacture the manager's private
/// token for a future epoch/ordinal pair.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AttemptFence([u8; 32]);

impl fmt::Debug for AttemptFence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("AttemptFence").field(&self.0).finish()
    }
}

impl AttemptFence {
    pub(super) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the fence bytes for transport serialization.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One leased execution attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttemptLease {
    /// Semantic work identity.
    pub(super) key: WorkKey,
    /// Monotonic owner epoch selected by the scheduler.
    pub(super) owner_epoch: u64,
    /// Monotonic ordinal within an owner/key sequence.
    pub(super) ordinal: u32,
    /// Opaque publication fence.
    pub(super) fence: AttemptFence,
    /// Durable process incarnation. Persisted receipts must carry this fence
    /// so a scheduler restart revokes every lease from the prior process.
    pub(super) incarnation: [u8; 32],
    /// Owner-local logical expiry deadline.
    pub(super) expires_at: u64,
    /// Last owner-local clock observation used to reject time regression.
    pub(super) observed_at: u64,
    /// Explicit state-machine state.
    pub(super) state: AttemptState,
    pub(crate) binding: IdentityBinding,
}

impl AttemptLease {
    /// Returns the semantic work key owned by this lease.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns the monotonic scheduler owner epoch.
    #[must_use]
    pub const fn owner_epoch(&self) -> u64 {
        self.owner_epoch
    }

    /// Returns the monotonic ordinal within the work-key sequence.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// Returns the opaque publication fence.
    #[must_use]
    pub const fn fence(&self) -> AttemptFence {
        self.fence
    }

    /// Returns the process incarnation bound to this lease.
    #[must_use]
    pub const fn incarnation(&self) -> [u8; 32] {
        self.incarnation
    }

    /// Returns the owner-local expiry deadline.
    #[must_use]
    pub const fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Returns the last owner-local clock observation.
    #[must_use]
    pub const fn observed_at(&self) -> u64 {
        self.observed_at
    }

    /// Returns the manager's current state snapshot for this lease.
    #[must_use]
    pub const fn state(&self) -> AttemptState {
        self.state
    }

    /// Returns whether this lease is currently active.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        matches!(self.state, AttemptState::Active)
    }

    /// Returns whether the owner-local deadline has elapsed.
    #[must_use]
    pub const fn is_expired_at(&self, now: u64) -> bool {
        now >= self.expires_at
    }

    pub(crate) const fn set_state(&mut self, state: AttemptState) {
        self.state = state;
    }
}

/// Valid states of the attempt state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttemptState {
    /// Work may heartbeat and publish exactly one result.
    Active,
    /// Publication rights were frozen by cancellation or a scheduler fence.
    Frozen,
    /// The caller cancelled this attempt.
    Cancelled,
    /// The owner deadline elapsed and a takeover may be issued.
    Expired,
    /// Output failed validation and cannot be selected.
    Quarantined,
    /// One result was accepted; later receipts are rejected.
    Accepted,
}

impl AttemptState {
    /// Returns whether no further state transition is valid.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Active)
    }
}

/// Result coverage carried by a worker receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResultCoverage {
    /// Complete for the requested dependency-closed scope.
    Complete,
    /// Partial output that may be retained as an unselected cache candidate.
    Partial,
    /// The authority could not observe the requested scope.
    Unavailable,
    /// The worker does not support the requested scope.
    Unsupported,
}

impl ResultCoverage {
    pub(super) fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

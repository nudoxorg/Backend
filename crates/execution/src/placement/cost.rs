//! Completion-time cost observations and confidence-bounded snapshots.

use super::capability::ObservationError;
use crate::{VersionedWorkIdentity, WorkKey};
use backend_version::Relation;

/// Completion-time components for a remote route. The scheduler charges every
/// component, including local validation and contention; queue and transfer
/// are not hidden inside a compute-only estimate.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CompletionCost {
    /// Client-side queue before the route is dispatched.
    pub client_queue: u64,
    /// Network round trip or handshake delay.
    pub round_trip: u64,
    /// Missing input transfer time/credit.
    pub input_transfer: u64,
    /// Remote worker queue.
    pub worker_queue: u64,
    /// Capability/session warmup.
    pub warmup: u64,
    /// Execution and materialization.
    pub execution: u64,
    /// Missing output transfer time/credit.
    pub output_transfer: u64,
    /// Local decode, validation, and publication.
    pub validation: u64,
    /// Expected interference with local shared resources.
    pub contention: u64,
}

impl CompletionCost {
    /// Returns the checked critical-path total, if representable.
    #[must_use]
    pub fn checked_total(self) -> Option<u64> {
        [
            self.client_queue,
            self.round_trip,
            self.input_transfer,
            self.worker_queue,
            self.warmup,
            self.execution,
            self.output_transfer,
            self.validation,
            self.contention,
        ]
        .into_iter()
        .try_fold(0u64, u64::checked_add)
    }

    /// Returns a saturating total for diagnostics and deterministic choice.
    #[must_use]
    pub fn total(self) -> u64 {
        self.checked_total().unwrap_or(u64::MAX)
    }
}

/// Untrusted measured route costs awaiting engine policy admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CostObservation {
    /// Local end-to-end estimate.
    pub local: u64,
    /// Remote completion critical-path components.
    pub remote: CompletionCost,
    /// Measurement timestamp in the owner's logical clock.
    pub observed_at: u64,
    /// Latest timestamp at which the measurement may steer placement.
    pub expires_at: u64,
    /// Confidence in per-mille, supplied by the measurement authority.
    pub confidence_per_mille: u16,
}

/// A checked route cost snapshot bound to one work key and validity window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CostSnapshot {
    key: WorkKey,
    local: u64,
    remote: CompletionCost,
    observed_at: u64,
    expires_at: u64,
    confidence_per_mille: u16,
    /// Owner-derived p95 remote critical-path estimate used for bounded tail
    /// hedging. It is checked together with the ordinary observation.
    remote_p95: u64,
}

impl CostSnapshot {
    /// Minimum confidence required before remote cost can steer an optional
    /// route away from a valid local path.
    pub const MIN_CONFIDENCE_PER_MILLE: u16 = 500;

    /// Builds an explicitly cold snapshot for a request that has not yet
    /// received an owner-observed route sample.
    ///
    /// The snapshot carries zero confidence and a maxed remote estimate, so
    /// it cannot steer optional work to a remote route. Dispatchers replace it
    /// with their bounded owner model before choosing a route; this constructor
    /// exists only to make that cold-start state explicit at lower API seams.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::InvalidWindow`] when the owner clock is too
    /// close to its representable upper bound for a finite observation window.
    pub fn cold_start<R: Relation>(
        identity: &VersionedWorkIdentity<R>,
        now: u64,
    ) -> Result<Self, ObservationError> {
        const WINDOW_TICKS: u64 = 1_000;
        let expires_at = now
            .checked_add(WINDOW_TICKS)
            .ok_or(ObservationError::InvalidWindow)?;
        Ok(Self {
            key: identity.work_key(),
            local: 0,
            remote: CompletionCost {
                execution: u64::MAX,
                ..CompletionCost::default()
            },
            observed_at: now,
            expires_at,
            confidence_per_mille: 0,
            remote_p95: u64::MAX,
        })
    }

    /// Admits a measured cost snapshot after engine policy verification.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError`] when the verifier rejects the sample or
    /// its validity window is empty.
    pub fn admit<R: Relation, V: CostVerifier<R>>(
        identity: &VersionedWorkIdentity<R>,
        observation: CostObservation,
        verifier: &V,
    ) -> Result<Self, ObservationError> {
        if observation.expires_at <= observation.observed_at {
            return Err(ObservationError::InvalidWindow);
        }
        if observation.confidence_per_mille > 1_000 || observation.remote.checked_total().is_none()
        {
            return Err(ObservationError::InvalidMeasurement);
        }
        verifier
            .verify_cost(identity, &observation)
            .map_err(|_| ObservationError::Rejected)?;
        Ok(Self {
            key: identity.work_key(),
            local: observation.local,
            remote: observation.remote,
            observed_at: observation.observed_at,
            expires_at: observation.expires_at,
            confidence_per_mille: observation.confidence_per_mille,
            remote_p95: observation.remote.total(),
        })
    }

    /// Admits an owner-calculated snapshot with a bounded remote p95 tail.
    ///
    /// The verifier still checks the base measurement. The tail must be a
    /// valid critical-path value and is retained only as a planning hint tied
    /// to the same identity and validity window.
    /// # Errors
    ///
    /// Returns [`ObservationError`] when the base observation is invalid or
    /// when the tail cannot represent a checked remote measurement.
    pub fn admit_with_remote_p95<R: Relation, V: CostVerifier<R>>(
        identity: &VersionedWorkIdentity<R>,
        observation: CostObservation,
        remote_p95: u64,
        verifier: &V,
    ) -> Result<Self, ObservationError> {
        let mut snapshot = Self::admit(identity, observation, verifier)?;
        if remote_p95 < snapshot.remote.total() && snapshot.remote.checked_total().is_none() {
            return Err(ObservationError::InvalidMeasurement);
        }
        snapshot.remote_p95 = remote_p95;
        Ok(snapshot)
    }

    /// Returns the checked local estimate.
    #[must_use]
    pub const fn local(&self) -> u64 {
        self.local
    }

    /// Returns the checked remote critical-path estimate.
    #[must_use]
    pub const fn remote(&self) -> CompletionCost {
        self.remote
    }

    /// Returns the owner-derived p95 remote critical-path estimate.
    #[must_use]
    pub const fn remote_p95(&self) -> u64 {
        self.remote_p95
    }

    /// Returns the confidence supplied by the measurement authority.
    #[must_use]
    pub const fn confidence_per_mille(&self) -> u16 {
        self.confidence_per_mille
    }

    /// Returns the exact semantic key to which this snapshot is bound.
    #[must_use]
    pub const fn key(&self) -> WorkKey {
        self.key
    }

    /// Returns whether this snapshot is current and confident enough to steer
    /// optional placement for the supplied key/time.
    #[must_use]
    pub fn usable_for(&self, key: WorkKey, now: u64) -> bool {
        self.key == key
            && now >= self.observed_at
            && now < self.expires_at
            && self.confidence_per_mille >= Self::MIN_CONFIDENCE_PER_MILLE
    }
}

/// Engine-owned verifier for route-cost measurements.
pub trait CostVerifier<R: Relation> {
    /// Checks measurement provenance, bounds, and confidence policy.
    ///
    /// # Errors
    ///
    /// Returns [`ObservationError::Rejected`] when the sample is not trusted
    /// to steer placement.
    fn verify_cost(
        &self,
        identity: &VersionedWorkIdentity<R>,
        observation: &CostObservation,
    ) -> Result<(), ObservationError>;
}

impl<R, F> CostVerifier<R> for F
where
    R: Relation,
    F: for<'a> Fn(
        &'a VersionedWorkIdentity<R>,
        &'a CostObservation,
    ) -> Result<(), ObservationError>,
{
    fn verify_cost(
        &self,
        identity: &VersionedWorkIdentity<R>,
        observation: &CostObservation,
    ) -> Result<(), ObservationError> {
        self(identity, observation)
    }
}

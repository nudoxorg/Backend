//! Exact-base lifecycle transitions for delegated agent work.
//!
//! The ledger is intentionally a small relation state machine.  A work
//! specification is admitted once, and every later mutation replaces one
//! relation value through a [`backend_version::MapChange`].  The version
//! kernel therefore supplies the durable identity, path-copying, and stale
//! base rejection while this module owns only lifecycle rules.

use core::marker::PhantomData;
use std::sync::OnceLock;

use backend_version::{
    AuthorityScopeClaim, CoverageWitness, ProducerObservationVerifier, RelationState, ScopeRoot,
    UntrustedProducerObservation, admit_complete_scope, admit_producer_observation,
};

use crate::error::ControlError;
use crate::ids::{AgentWorkKey, AuthorityVersion, FenceSchema, WorkKeySchema};
use crate::record::{
    ControlRelation, ControlRoot, DecisionReceipt, DurableReceipt, MAX_WAITERS, WorkRecord,
    WorkStatus,
};

#[path = "ledger_lease.rs"]
mod lease;
#[path = "ledger_lifecycle.rs"]
mod lifecycle;
#[path = "ledger_projection.rs"]
mod projection;

pub use lease::{
    ActiveLeaseMarker, Candidate, CandidateStage, Evaluated, Frozen, Held, Lease, LeaseFence,
    LeaseMarker, Renewed, Reviewed,
};
pub(crate) use lease::{ensure_fence, fence_identity};
use projection::ProjectionIndex;

/// Maximum scheduler size and lifecycle budgets.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerLimits {
    /// Maximum number of running implementation attempts. Frozen candidates
    /// have independent bounded review leases and release this capacity.
    pub max_parallel: u16,
    /// Maximum number of attempts for one immutable work key.
    pub max_attempts: u16,
    /// Maximum number of clients coalesced onto one active row.
    pub max_waiters: u16,
    /// Lease duration in the caller's monotonic nanosecond clock.
    pub lease_ttl_ns: u64,
}

impl Default for SchedulerLimits {
    fn default() -> Self {
        Self {
            max_parallel: 8,
            max_attempts: 3,
            max_waiters: MAX_WAITERS,
            lease_ttl_ns: 30_000_000_000,
        }
    }
}

impl SchedulerLimits {
    pub(crate) fn validate(self) -> Result<Self, ControlError> {
        if self.max_parallel == 0 || self.max_attempts == 0 || self.max_waiters == 0 {
            return Err(ControlError::Bounds);
        }
        if self.max_waiters > MAX_WAITERS || self.lease_ttl_ns == 0 {
            return Err(ControlError::Bounds);
        }
        Ok(self)
    }
}

/// One exact relation transition produced by a lifecycle operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlDelta {
    inner: backend_version::Delta<ControlRelation>,
}

impl ControlDelta {
    /// Returns the complete relation root required as the transition base.
    #[must_use]
    pub const fn base(&self) -> ControlRoot {
        self.inner.base()
    }

    /// Returns the relation root produced by this transition.
    #[must_use]
    pub const fn target(&self) -> ControlRoot {
        self.inner.target()
    }

    /// Returns the exact transition identity.
    #[must_use]
    pub const fn id(&self) -> backend_version::DeltaId<ControlRelation> {
        self.inner.id()
    }

    /// Returns the canonical before/after encoding used by the transition id.
    #[must_use]
    pub fn canonical_bytes(&self) -> &[u8] {
        self.inner.canonical_changes_bytes()
    }
}

/// A committed lifecycle mutation and its exact versioned relation roots.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlCommit {
    /// Root on which the mutation was prepared.
    pub base: ControlRoot,
    /// Root selected after the mutation.
    pub target: ControlRoot,
    /// Exact transition, retained for replication or durable publication.
    pub delta: ControlDelta,
    projection: ProjectionIndex,
}

impl ControlCommit {
    pub(crate) fn from_delta(inner: backend_version::Delta<ControlRelation>) -> Self {
        let base = inner.base();
        let target = inner.target();
        Self {
            base,
            target,
            delta: ControlDelta { inner },
            projection: ProjectionIndex::default(),
        }
    }
}

/// Result of admitting a work specification into the relation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanResult {
    /// The specification was newly admitted and this delta must be published.
    Admitted(ControlCommit),
    /// The exact completed receipt is reusable.
    Reused(DurableReceipt),
    /// An equal immutable row already exists and can coalesce callers.
    Existing {
        /// Current lifecycle state.
        status: WorkStatus,
        /// Number of callers currently waiting on the row.
        waiters: u16,
    },
}

/// Result of a scheduler admission request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Admission {
    /// The caller owns a newly fenced attempt.
    Owned(Lease<Held>),
    /// The caller joined an already active attempt.
    Coalesced {
        /// Active state retained by the owner.
        status: WorkStatus,
        /// New waiter count.
        waiters: u16,
    },
    /// The row remains blocked on one or more prerequisites.
    Waiting,
    /// The row remains behind the active-attempt budget.
    Queued,
    /// A completed receipt was found by exact key and specification.
    Reused(DurableReceipt),
}

/// Result of one nonterminal custody attestation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StageResult<S: CandidateStage> {
    /// This authority accepted the subject and minted the capability required
    /// by the next authority.
    Advanced {
        /// Exact capability consumed by the next custody stage.
        candidate: Candidate<S>,
        /// Exact lifecycle transition.
        commit: ControlCommit,
    },
    /// This authority rejected or quarantined the subject.
    Terminal {
        /// Non-reusable terminal state.
        status: WorkStatus,
        /// Exact lifecycle transition.
        commit: ControlCommit,
    },
}

impl<S: CandidateStage> StageResult<S> {
    /// Returns the state selected by this transition.
    #[must_use]
    pub const fn status(&self) -> WorkStatus {
        match self {
            Self::Advanced { .. } => S::STATUS,
            Self::Terminal { status, .. } => *status,
        }
    }

    /// Returns the exact transition without consuming the result.
    #[must_use]
    pub const fn commit(&self) -> &ControlCommit {
        match self {
            Self::Advanced { commit, .. } | Self::Terminal { commit, .. } => commit,
        }
    }
}

/// Evaluator transition result.
pub type EvaluationResult = StageResult<Evaluated>;
/// Reviewer transition result.
pub type ReviewResult = StageResult<Reviewed>;

/// Result of Sol's final custody decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionResult {
    /// Reusable or non-reusable terminal state selected by Sol.
    pub status: WorkStatus,
    /// Immutable Sol receipt over the exact accepted review.
    pub receipt: DecisionReceipt,
    /// Exact lifecycle transition.
    pub commit: ControlCommit,
}

/// In-memory checked control relation. Cloning this value is O(1) because the
/// version kernel retains immutable tree nodes; callers can therefore prepare
/// a candidate, publish it, and retry after a crash without rebuilding rows.
#[derive(Clone, Debug)]
pub struct VersionedControlPlane {
    state: RelationState<ControlRelation>,
    limits: SchedulerLimits,
    projection: ProjectionIndex,
}

impl VersionedControlPlane {
    /// Creates an empty relation with complete coordinator coverage.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] for invalid scheduler limits or
    /// [`ControlError::Corrupt`] when the authority coverage cannot be
    /// admitted completely.
    pub fn new(limits: SchedulerLimits) -> Result<Self, ControlError> {
        let limits = limits.validate()?;
        let coverage = control_coverage()?;
        Ok(Self {
            state: RelationState::empty(coverage),
            limits,
            projection: ProjectionIndex::default(),
        })
    }

    pub(crate) fn state(&self) -> &RelationState<ControlRelation> {
        &self.state
    }

    /// Returns the exact current relation root.
    #[must_use]
    pub const fn root(&self) -> ControlRoot {
        self.state.root()
    }

    /// Returns scheduler bounds used by this plane.
    #[must_use]
    pub const fn limits(&self) -> SchedulerLimits {
        self.limits
    }

    pub(crate) fn summary(&self) -> Result<crate::ids::ControlSummary, ControlError> {
        Ok(crate::ids::ControlSummary::new(
            u64::try_from(self.projection.active_count()).map_err(|_| ControlError::Bounds)?,
            u64::try_from(self.projection.ready_count()).map_err(|_| ControlError::Bounds)?,
        ))
    }

    /// Returns a structural O(1) snapshot suitable for speculative admission.
    #[must_use]
    pub fn snapshot(&self) -> Self {
        self.clone()
    }

    /// Returns one checked row by immutable work key.
    #[must_use]
    pub fn get(&self, key: AgentWorkKey) -> Option<&WorkRecord> {
        self.state.get(&crate::record::RecordKey::from_work(key))
    }

    /// Admits a fixed-width work-key claim by looking it up in the checked
    /// relation. A matching context without a corresponding row is rejected.
    ///
    /// # Errors
    ///
    /// Returns a wire, unknown-work, or corruption error for an invalid claim
    /// or a key that is absent from the checked relation.
    pub fn lookup_work_key(&self, bytes: &[u8]) -> Result<AgentWorkKey, ControlError> {
        let bytes: [u8; backend_version::ID_BYTES] = bytes
            .try_into()
            .map_err(|_| ControlError::InvalidDigestLength)?;
        let claim = backend_version::UntrustedId::<WorkKeySchema>::from_wire(
            &bytes,
            backend_version::IdContext::schema::<WorkKeySchema>(),
        )
        .map_err(|_| ControlError::InvalidDigestLength)?;
        let record = self
            .state
            .get(&crate::record::RecordKey::from_bytes(bytes))
            .ok_or(ControlError::UnknownWork)?;
        if record.spec().key().as_bytes() != claim.as_bytes() {
            return Err(ControlError::Corrupt);
        }
        Ok(record.spec().key())
    }

    /// Rehydrates a held lease from a wire fence only after matching the
    /// relation's current, typed attempt record.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, wire, or stale-fence error when the row is not
    /// running or the claim does not match its current attempt.
    pub fn lease_for(&self, key: AgentWorkKey, fence: &[u8]) -> Result<Lease<Held>, ControlError> {
        let (_, existing) = self.current_attempt(key)?;
        if existing.status() != WorkStatus::Running {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
        let claim = backend_version::UntrustedId::<FenceSchema>::from_wire(
            fence,
            backend_version::IdContext::schema::<FenceSchema>(),
        )
        .map_err(|_| ControlError::InvalidDigestLength)?;
        if claim.as_bytes() != attempt.fence().as_bytes() {
            return Err(ControlError::StaleFence);
        }
        Ok(Lease {
            key,
            attempt,
            marker: PhantomData,
        })
    }

    /// Rehydrates a frozen typestate lease for an independent review.
    ///
    /// # Errors
    ///
    /// Returns a lifecycle, wire, or stale-fence error when the row is not
    /// frozen or the claim does not match its retained attempt.
    pub fn frozen_for(
        &self,
        key: AgentWorkKey,
        fence: &[u8],
    ) -> Result<Lease<Frozen>, ControlError> {
        let (_, existing) = self.current_attempt(key)?;
        if existing.status() != WorkStatus::Frozen {
            return Err(ControlError::InvalidTransition);
        }
        let attempt = existing.attempt_data().ok_or(ControlError::StaleFence)?;
        let claim = backend_version::UntrustedId::<FenceSchema>::from_wire(
            fence,
            backend_version::IdContext::schema::<FenceSchema>(),
        )
        .map_err(|_| ControlError::InvalidDigestLength)?;
        if claim.as_bytes() != attempt.fence().as_bytes() {
            return Err(ControlError::StaleFence);
        }
        Ok(Lease {
            key,
            attempt,
            marker: PhantomData,
        })
    }

    /// Iterates rows in canonical work-key order.
    pub fn records(&self) -> impl Iterator<Item = &WorkRecord> {
        self.state.iter().map(|(_, record)| record)
    }
}

pub(crate) fn control_coverage() -> Result<CoverageWitness, ControlError> {
    static COVERAGE: OnceLock<Result<CoverageWitness, ControlError>> = OnceLock::new();
    COVERAGE
        .get_or_init(|| {
            let scope = AuthorityVersion::from_value(&1);
            let declaration = AuthorityScopeClaim::from_object_version(scope);
            let producer = ControlEvaluatorProducer::admit(scope);
            let observation = producer.observation();
            let admitted = admit_producer_observation(observation, &producer)
                .map_err(|_| ControlError::Corrupt)?;
            match admit_complete_scope(declaration, admitted) {
                Ok(complete) => Ok(CoverageWitness::Complete(complete)),
                Err(_) => Err(ControlError::Corrupt),
            }
        })
        .clone()
}

/// The admitted local evaluator session used for the control relation's
/// complete scope. Its private constructor keeps raw scope bytes from minting
/// a complete coverage witness at the lifecycle boundary.
struct ControlEvaluatorProducer {
    scope: ScopeRoot,
    identity: [u8; backend_version::ID_BYTES],
    context: [u8; backend_version::ID_BYTES],
    evidence: Vec<u8>,
}

impl ControlEvaluatorProducer {
    fn admit(authority: AuthorityVersion) -> Self {
        let mut identity = [0; backend_version::ID_BYTES];
        identity.copy_from_slice(blake3::hash(b"backend-control/evaluator/v1").as_bytes());
        let mut context = [0; backend_version::ID_BYTES];
        context.copy_from_slice(blake3::hash(b"backend-control/evaluator/context/v1").as_bytes());
        Self {
            scope: ScopeRoot::from_bytes(authority.to_bytes()),
            identity,
            context,
            evidence: b"backend-control/evaluator/complete/v1".to_vec(),
        }
    }

    fn observation(&self) -> UntrustedProducerObservation {
        UntrustedProducerObservation::new(
            self.identity,
            self.scope,
            self.context,
            self.evidence.clone(),
        )
    }
}

impl ProducerObservationVerifier for ControlEvaluatorProducer {
    type Error = ControlError;

    fn verify(&self, observation: &UntrustedProducerObservation) -> Result<(), Self::Error> {
        if observation.producer_identity() != self.identity
            || observation.scope_root() != self.scope
            || observation.context() != self.context
            || observation.evidence() != self.evidence
        {
            return Err(ControlError::Corrupt);
        }
        Ok(())
    }
}

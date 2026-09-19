//! Exact local/remote execution dispatch.
//!
//! This module is the trust boundary between the scheduler, replication
//! envelopes, and recipe/schema validators. Byte transfer coverage is distinct
//! from semantic coverage, and remote output is publishable only after all
//! request, witness, fence, output, and authority checks pass.

mod admission;
mod authority;
mod coordinator;
mod cost;
mod coverage;
mod dependencies;
mod invalidation;
mod journal;
mod planning;
mod protocol;
mod reservations;
mod retention;
mod transport;

pub(crate) use reservations::{
    ActiveSemantic, ActiveSemanticRollback, AuthorityFreshness, CompletionIdentity,
    CompletionReservation, FreshnessRollback,
};

#[cfg(test)]
mod tests;

use backend_execution::{
    AuthorityVersion, Budget, Cancellation, HedgeSide, ResultReceipt, ScheduleOutcome,
    ScheduleRequest, Scheduler,
};
use backend_replication::{
    AttestationVerifier, AuthorityEpoch, ReplicationError, RevocationVersion,
};
use backend_version::Relation;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

pub use authority::{
    Blake3AuthorityVerifier, RemoteAuthorityPolicy, UnconfiguredAuthorityVerifier,
};
pub use coverage::{
    CompleteSemanticCoverage, CompleteSemanticCoverageCapability, RecipeScopeAuthorityValidator,
    SemanticCoverage, SemanticCoverageAdmissionError, SemanticCoverageBinding,
    SemanticCoverageClaim, SemanticCoverageSchema, SemanticCoverageState,
    SemanticCoverageValidator, UntrustedSemanticCoverage, UntrustedSemanticCoverageClaim,
    admit_semantic_coverage,
};
pub use dependencies::{
    SemanticDependencyCoordinator, SemanticDependencyError, SemanticInvalidationReport,
};
pub use journal::{
    AcceptedResultProof, AuthoritySnapshot, DISPATCH_RECORD_VERSION, DispatchAttemptKey,
    DispatchCursor, DispatchJournal, DispatchJournalError, DispatchJournalLimits,
    DispatchJournalReplay, DispatchLog, DispatchPhase, DispatchRecord, DispatchRecordError,
    DispatchRecovery, DispatchRecoveryAction, DispatchRestartAuthority, DispatchRestartDecision,
    MAX_DISPATCH_CURSOR_BYTES, MAX_DISPATCH_PROOF_BYTES, MAX_DISPATCH_REQUEST_BYTES,
    NotificationCursor, OwnerRestartAuthority, PublicationAck, RESTART_ALREADY_FENCED,
    RESTART_AUTHORITY_REVOKED, RESTART_LEASE_EXPIRED, RESTART_OWNER_TAKEOVER, RecoveredAttempt,
    RemoteAttemptIntent, RemoteDispatchJournal, RemoteDispatchRecord, RestartAuthoritySnapshot,
    TerminalState, TransferCheckpointRef,
};
pub use protocol::{
    DispatchCompletion, DispatchPlan, DispatchTicket, ExpectedInput, FenceBinding, InFlightRemote,
    PendingRemoteEnvelope, PendingRemoteKey, RemoteCorrelationKey, RemoteDispatchContract,
    WorkerReceiptId, WorkerReceiptSchema, cancellation_id_for, request_expectation, wire_request,
    worker_receipt_id,
};
pub use retention::{
    CompositeAdmissionValidator, OutputAdmissionValidator, RetainedOutput,
    UnconfiguredOutputValidator, test_util,
};
pub use transport::{
    EngineAttemptLease, EngineScheduleRequest, LoopbackTransport, RemoteTransport, StreamTransport,
};

/// Dispatch failures with explicit trust and cancellation classifications.
#[derive(Debug)]
pub enum DispatchError {
    /// Scheduler rejected route or admission.
    Schedule(backend_execution::ScheduleError),
    /// Remote wire claim failed exact admission.
    Replication(ReplicationError),
    /// Output bytes and claimed version differ.
    OutputMismatch,
    /// Output contract or scope was incomplete.
    OutputContract,
    /// A result had no complete transfer coverage witness.
    IncompleteCoverage,
    /// A semantic scope witness was partial, stale, or mismatched.
    IncompleteSemanticCoverage,
    /// The remote request could not be represented.
    InvalidAttempt,
    /// A result receipt had the wrong schema or identity.
    ReceiptMismatch,
    /// Remote authority evidence was absent, invalid, revoked, or weak.
    AuthorityRejected,
    /// Two valid results at the same authority disagreed.
    AuthorityConflict,
    /// A semantic dependency could not be registered or invalidated.
    SemanticDependency(SemanticDependencyError),
    /// The route was cancelled before publication.
    Cancelled,
    /// A result was presented to a ticket for a different work item or
    /// attempt.
    TicketMismatch,
    /// A consuming ticket operation was attempted after the affine ticket
    /// had already moved its scheduler guard into another owner.
    TicketConsumed,
    /// Durable derived-output publication rejected the admitted result.
    Workspace(String),
    /// The owner cost model could not admit a bounded planning snapshot.
    CostUnavailable,
    /// A retained output exists, but this planning call did not present the
    /// current engine-issued reuse context. The owner-controlled semantic
    /// planning path must be used to expose that output.
    ReuseContextRequired,
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "dispatch error: {self:?}")
    }
}

impl std::error::Error for DispatchError {}

/// An engine-admitted authority freshness transition. Numeric epoch and
/// revocation values are private so callers can only obtain this capability
/// through [`Dispatcher::admit_authority_transition`], which rejects rollback
/// and fences every affected publication under the dispatcher transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorityTransition {
    authority: AuthorityVersion,
    epoch: AuthorityEpoch,
    revocation_version: RevocationVersion,
    affected: Box<[backend_execution::WorkKey]>,
}

impl AuthorityTransition {
    /// Returns the authority identity whose freshness changed.
    #[must_use]
    pub const fn authority(&self) -> AuthorityVersion {
        self.authority
    }

    /// Returns the admitted authority epoch.
    #[must_use]
    pub const fn epoch(&self) -> AuthorityEpoch {
        self.epoch
    }

    /// Returns the admitted revocation version.
    #[must_use]
    pub const fn revocation_version(&self) -> RevocationVersion {
        self.revocation_version
    }

    /// Returns work keys fenced by this transition.
    #[must_use]
    pub fn affected_work_keys(&self) -> &[backend_execution::WorkKey] {
        &self.affected
    }
}

/// Scheduler plus mandatory recipe validator and remote authority verifier.
///
/// Authority ranking, replay reservations, scheduler publication, and the
/// accepted-result index share one transaction lock. A rejected or abandoned
/// route therefore cannot leave behind an authority or replay decision.
pub struct Dispatcher<V, A> {
    scheduler: Scheduler,
    validator: V,
    authority: A,
    policy: RemoteAuthorityPolicy,
    authority_lock: Mutex<()>,
    accepted: Mutex<BTreeMap<backend_execution::WorkKey, authority::AcceptedAuthority>>,
    /// Work keys with deterministic output disagreement evidence. A
    /// quarantined key cannot use a remote result or hot reuse until an
    /// independently local publication resolves the conflict.
    authority_quarantine:
        Mutex<BTreeMap<backend_execution::WorkKey, authority::AuthorityConflictEvidence>>,
    /// Reverse projection from authority identity to retained publications.
    /// It is maintained at the same publication/eviction linearization point
    /// as `accepted` so an authority transition can fence exact joins.
    authority_index: Mutex<BTreeMap<AuthorityVersion, BTreeSet<backend_execution::WorkKey>>>,
    /// Latest engine-admitted freshness for each authority identity.
    authority_transitions: Mutex<BTreeMap<AuthorityVersion, AuthorityFreshness>>,
    /// Current authority freshness observed through an engine-admitted
    /// semantic capability. A numeric epoch/revocation supplied by a caller
    /// is never sufficient to mint this record.
    authority_freshness: Mutex<BTreeMap<backend_execution::WorkKey, AuthorityFreshness>>,
    /// Raw completion capabilities issued only after the scheduler accepted a
    /// typed receipt.  Keeping this identity beside the typed accepted index
    /// lets the daemon validate an opaque wire claim without reconstructing a
    /// `WorkKey` from digest bytes.
    completed: Arc<Mutex<BTreeMap<[u8; 32], CompletionIdentity>>>,
    pending_authority: Arc<authority::PendingAuthorityState>,
    replayed: Arc<authority::ReplayState>,
    semantic: Arc<SemanticDependencyCoordinator>,
    costs: cost::RouteCostModel,
    /// Proofs retained between scheduler admission and the daemon's single
    /// workspace publication call.  The map is bounded by the same accepted
    /// authority/output lifecycle and is removed on eviction or revocation.
    derived_proofs:
        Mutex<BTreeMap<backend_execution::WorkKey, crate::workspace::catalog::DerivedOutputProof>>,
    /// Semantic registrations retained while a manifest-backed route is
    /// active. This lets selector invalidation fence live work before a
    /// completion exists, while the reservation's drop path releases the
    /// reverse-index row if the schedule is cancelled or fails.
    active_semantic: Arc<Mutex<BTreeMap<backend_execution::WorkKey, ActiveSemantic>>>,
}

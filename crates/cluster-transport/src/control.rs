//! Bounded assignment control messages over a separate authenticated Iroh ALPN.

use std::collections::HashSet;
use std::time::Duration;

use iroh::{EndpointAddr, endpoint::Connection};
use serde::{Deserialize, Serialize};

use crate::{
    AssignmentScope, CONTROL_ALPN, Capability, Endpoint, EndpointId, TransferScope, TransportError,
    read_frame, validate_claim_shape, write_frame,
};

/// Maximum duration represented by an Offer deadline.
pub const MAX_CONTROL_OFFER_LIFETIME_MS: u64 = 24 * 60 * 60 * 1_000;

/// Maximum number of object-range grants included in one bounded Offer.
pub const MAX_OFFER_CAPABILITIES: usize = 16;
/// Largest bounded sequence of object-grant pages in one attempt.
pub const MAX_CONTROL_GRANT_PAGES: u32 = 4_096;
/// Maximum messages allowed in either direction on one control stream.
pub const MAX_CONTROL_MESSAGES_PER_STREAM: u8 = 8;
/// Schema version for the durable worker-result retirement handshake messages.
pub const RESULT_RETIREMENT_SCHEMA_VERSION: u16 = 1;
/// Schema version for a cold owner query about one exact worker assignment.
pub const RESULT_RECOVERY_SCHEMA_VERSION: u16 = 1;
/// Schema version for owner-authorized compaction of exact no-result fences.
pub const NO_RESULT_RETIREMENT_SCHEMA_VERSION: u16 = 2;
const CONTROL_FINISH_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const CONTROL_ACCEPT_STREAM_TIMEOUT: Duration = Duration::from_secs(30);

/// Coordinator and worker have intentionally asymmetric control authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlRole {
    /// Owns placement and the Turso-minted attempt; sends Offer/Cancel/result ACK.
    Coordinator,
    /// Executes the assignment; sends provisional Accept, WorkerReject, ExecutionFailed,
    /// or ResultReceipt.
    Worker,
}

/// One bounded remote compile offer. Input range capabilities are scoped to the same
/// namespace/work/attempt/fence; each capability separately binds its immutable ClosureId.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlOffer {
    /// Exact assignment scope from the current authority attempt.
    pub scope: AssignmentScope,
    /// Source-aware package lineage digest.
    pub package_lineage: [u8; 32],
    /// Exact compiler target identity.
    pub target: [u8; 32],
    /// Exact compiler recipe identity.
    pub recipe: [u8; 32],
    /// Root of the exact source/workspace inputs.
    pub input_root: [u8; 32],
    /// Root of the complete positive and negative input read manifest.
    pub read_manifest: [u8; 32],
    /// Exact backend-store closure containing the compiler input manifest.
    pub input_closure_id: [u8; 32],
    /// Exact typed backend-store ObjectId of the compiler input manifest.
    pub input_manifest_object_id: [u8; 32],
    /// Selected base generation, if incremental work is permitted.
    pub selected_base: Option<[u8; 32]>,
    /// Maximum accepted output bytes for this work.
    pub max_output_bytes: u64,
    /// Absolute Unix-millisecond deadline for input transfer, compilation, and result retention.
    pub deadline_unix_ms: u64,
    /// Number of independently paged input-grant frames expected after acceptance.
    pub input_grant_pages: u32,
}

/// Worker transfer-slot reservation for one exact Offer.
///
/// `accepted` reserves capacity for input transfer only. It does not mean the worker has
/// verified the input manifest or is authorized to begin compilation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ControlAccept {
    /// Exact assignment scope being reserved or declined.
    pub scope: AssignmentScope,
    /// Whether the worker accepted the assignment.
    pub accepted: bool,
}

/// Closed reason that a worker cannot admit the exact offered input closure or invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkerRejectReason {
    /// The attempt became stale while the worker was receiving inputs.
    StaleAttempt,
    /// The exact input closure was unavailable after the declared transfer.
    ClosureUnavailable,
    /// The typed input-manifest object was missing from the input closure.
    ManifestUnavailable,
    /// The manifest object or its claims did not match the Offer.
    ManifestMismatch,
    /// The worker cannot execute the offered compiler toolchain.
    UnsupportedToolchain,
    /// The worker cannot satisfy the offered execution sandbox policy.
    SandboxUnavailable,
    /// The worker's local admission policy rejected this invocation.
    PolicyRejected,
    /// The exact Offer deadline expired before input admission completed.
    DeadlineExceeded,
}

/// Terminal worker rejection of one exact Offer's input closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerReject {
    /// Exact assignment being rejected.
    pub scope: AssignmentScope,
    /// Exact backend-store ClosureId named in the Offer.
    pub input_closure_id: [u8; 32],
    /// Closed input-admission failure code.
    pub reason: WorkerRejectReason,
}

/// Closed failure classes for work that passed input admission but could not produce a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecutionFailureReason {
    /// The compiler rejected the invocation after its input manifest was admitted.
    CompilerRejected,
    /// The compiler ran but failed to produce a valid staged result.
    CompilerFailed,
    /// The output exceeded the exact Offer's byte budget.
    OutputBudget,
    /// The worker could not durably store or reopen its staged result.
    StorageUnavailable,
    /// The exact assignment was cancelled while executing or storing the result.
    Cancelled,
    /// The exact assignment deadline elapsed before a result could be retained.
    Deadline,
}

/// Terminal worker failure after the offered input closure passed admission.
///
/// This message contains the exact input ClosureId and assignment fence. It deliberately has
/// no result ClosureId because the worker did not produce a result closure for the owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlExecutionFailed {
    /// Exact assignment that could not produce a result.
    pub scope: AssignmentScope,
    /// Exact backend-store ClosureId that was admitted before execution began.
    pub input_closure_id: [u8; 32],
    /// Closed execution failure class.
    pub reason: ExecutionFailureReason,
}

/// Why a worker should stop one exact assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CancelReason {
    /// A newer DB attempt superseded this assignment.
    Superseded,
    /// The user or owning service cancelled the work.
    Requested,
    /// The current assignment deadline elapsed.
    DeadlineExceeded,
    /// The scheduler removed this worker from the active cluster.
    WorkerUnavailable,
}

/// Cancellation is fenced to the exact current assignment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ControlCancel {
    /// Exact assignment being cancelled.
    pub scope: AssignmentScope,
    /// Bounded cancellation reason.
    pub reason: CancelReason,
}

/// Non-authoritative receipt announcing a completed worker result.
///
/// The receiver still must admit the transferred objects and closure through `ArtifactSink`,
/// then ask the index owner to validate the exact candidate before any selected-head change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResultReceipt {
    /// Exact assignment that produced this result.
    pub scope: AssignmentScope,
    /// Claimed target root.
    pub target_root: [u8; 32],
    /// Optional physical output pack identity. `None` is valid when the result closure is
    /// streamed directly into CAS without a separate pack/layout.
    pub pack_id: Option<[u8; 32]>,
    /// Exact backend-store ClosureId bytes for the completed result.
    pub closure_id: [u8; 32],
    /// Claimed number of closure members.
    pub object_count: u32,
    /// Claimed total canonical payload bytes.
    pub payload_bytes: u64,
    /// Number of independently paged result-grant frames requested from the coordinator.
    pub result_grant_pages: u32,
}

/// Bounded reason that the owner rejected one worker result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultRejectReason {
    /// The result belonged to a different or superseded assignment.
    Scope,
    /// The received ClosureId differed from the worker's exact result claim.
    Closure,
    /// Result metadata differed from checked storage facts.
    Metadata,
    /// Typed object or closure admission failed.
    Admission,
}

/// Terminal owner acknowledgement for one exact worker result closure.
///
/// `Stored` permits the worker to reclaim retained result artifacts. `Rejected` is also
/// terminal and carries only a closed reason code, never application error text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResultAck {
    /// Exact assignment that produced the result.
    pub scope: AssignmentScope,
    /// Exact backend-store ClosureId received from the worker.
    pub closure_id: [u8; 32],
    /// Final owner disposition after checking this result.
    pub disposition: ResultAckDisposition,
}

impl ControlResultAck {
    /// Constructs the terminal owner disposition for one exact result closure.
    ///
    /// # Errors
    /// Returns an error for an invalid assignment scope or empty closure ID.
    pub fn new(
        scope: AssignmentScope,
        closure_id: [u8; 32],
        disposition: ResultAckDisposition,
    ) -> Result<Self, TransportError> {
        validate_result_identity(scope, closure_id)?;
        Ok(Self {
            scope,
            closure_id,
            disposition,
        })
    }

    /// Exact assignment scope.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Exact stored result closure.
    #[must_use]
    pub const fn closure_id(&self) -> [u8; 32] {
        self.closure_id
    }

    /// Owner's terminal disposition.
    #[must_use]
    pub const fn disposition(&self) -> ResultAckDisposition {
        self.disposition
    }
}

/// Worker receipt that the exact result ACK is durably retained as a terminal
/// tombstone and can be replayed until the owner confirms retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResultRetired {
    /// Exact receipt schema version. Unknown versions fail control admission.
    pub version: u16,
    /// Exact assignment that produced the result.
    pub scope: AssignmentScope,
    /// Exact result closure retained by the worker.
    pub closure_id: [u8; 32],
    /// Terminal disposition previously received from the owner.
    pub disposition: ResultAckDisposition,
    /// Worker endpoint identity that durably retains this terminal receipt.
    pub worker_endpoint_id: [u8; 32],
}

impl ControlResultRetired {
    /// Constructs a versioned receipt for one exact worker tombstone.
    ///
    /// # Errors
    /// Returns an error for an invalid scope, closure ID, or worker identity.
    pub fn new(
        scope: AssignmentScope,
        closure_id: [u8; 32],
        disposition: ResultAckDisposition,
        worker: EndpointId,
    ) -> Result<Self, TransportError> {
        validate_result_identity(scope, closure_id)?;
        validate_endpoint_identity(worker)?;
        Ok(Self {
            version: RESULT_RETIREMENT_SCHEMA_VERSION,
            scope,
            closure_id,
            disposition,
            worker_endpoint_id: *worker.as_bytes(),
        })
    }

    /// Exact assignment scope.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Exact result closure.
    #[must_use]
    pub const fn closure_id(&self) -> [u8; 32] {
        self.closure_id
    }

    /// Exact terminal disposition.
    #[must_use]
    pub const fn disposition(&self) -> ResultAckDisposition {
        self.disposition
    }

    /// Worker endpoint recorded by the durable receipt.
    #[must_use]
    pub const fn worker_endpoint_id(&self) -> [u8; 32] {
        self.worker_endpoint_id
    }

    /// Receipt schema version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Whether this durable receipt answers the exact ACK on the authenticated
    /// worker stream.
    #[must_use]
    pub fn matches_ack(&self, ack: &ControlResultAck, authenticated_worker: EndpointId) -> bool {
        self.subject().matches(RetirementSubject::new(
            RESULT_RETIREMENT_SCHEMA_VERSION,
            ack.scope,
            ack.closure_id,
            ack.disposition,
        )) && self.worker_endpoint_id == *authenticated_worker.as_bytes()
    }

    fn subject(&self) -> RetirementSubject {
        RetirementSubject::new(self.version, self.scope, self.closure_id, self.disposition)
    }

    fn is_valid(&self) -> bool {
        self.subject().is_valid() && self.worker_endpoint_id != [0; 32]
    }
}

/// Owner confirmation that the worker may durably remove its terminal result
/// tombstone after it has recorded `ControlResultRetired`. A worker must treat
/// an exact duplicate confirmation as idempotent and return `Applied` again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResultRetirementConfirm {
    /// Exact confirmation schema version. Unknown versions fail admission.
    pub version: u16,
    /// Exact assignment that produced the result.
    pub scope: AssignmentScope,
    /// Exact result closure being retired.
    pub closure_id: [u8; 32],
    /// Terminal disposition recorded by the worker and owner.
    pub disposition: ResultAckDisposition,
    /// Authenticated owner endpoint whose durable journal accepted retirement.
    pub coordinator_endpoint_id: [u8; 32],
}

impl ControlResultRetirementConfirm {
    /// Constructs a versioned owner confirmation for the exact durable receipt.
    ///
    /// # Errors
    /// Returns an error for an invalid scope, closure ID, or owner identity.
    pub fn new(
        scope: AssignmentScope,
        closure_id: [u8; 32],
        disposition: ResultAckDisposition,
        coordinator: EndpointId,
    ) -> Result<Self, TransportError> {
        validate_result_identity(scope, closure_id)?;
        validate_endpoint_identity(coordinator)?;
        Ok(Self {
            version: RESULT_RETIREMENT_SCHEMA_VERSION,
            scope,
            closure_id,
            disposition,
            coordinator_endpoint_id: *coordinator.as_bytes(),
        })
    }

    /// Exact assignment scope.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Exact result closure.
    #[must_use]
    pub const fn closure_id(&self) -> [u8; 32] {
        self.closure_id
    }

    /// Exact terminal disposition.
    #[must_use]
    pub const fn disposition(&self) -> ResultAckDisposition {
        self.disposition
    }

    /// Owner endpoint recorded by the confirmation.
    #[must_use]
    pub const fn coordinator_endpoint_id(&self) -> [u8; 32] {
        self.coordinator_endpoint_id
    }

    /// Receipt schema version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Whether the confirmation names the exact receipt and authenticated owner.
    #[must_use]
    pub fn matches_retired(
        &self,
        retired: &ControlResultRetired,
        authenticated_coordinator: EndpointId,
    ) -> bool {
        self.is_valid()
            && retired.is_valid()
            && self.subject().matches(retired.subject())
            && self.coordinator_endpoint_id == *authenticated_coordinator.as_bytes()
    }

    fn subject(&self) -> RetirementSubject {
        RetirementSubject::new(self.version, self.scope, self.closure_id, self.disposition)
    }

    fn is_valid(&self) -> bool {
        self.subject().is_valid() && self.coordinator_endpoint_id != [0; 32]
    }
}

/// Worker receipt that the exact owner confirmation was applied and its
/// terminal result tombstone durably removed. A worker may return this again
/// for an exact duplicate confirmation after the tombstone is already absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResultRetirementApplied {
    /// Exact receipt schema version. Unknown versions fail admission.
    pub version: u16,
    /// Exact assignment that produced the result.
    pub scope: AssignmentScope,
    /// Exact result closure whose tombstone was removed.
    pub closure_id: [u8; 32],
    /// Terminal disposition previously acknowledged by the owner.
    pub disposition: ResultAckDisposition,
    /// Worker endpoint that applied the confirmation.
    pub worker_endpoint_id: [u8; 32],
}

impl ControlResultRetirementApplied {
    /// Constructs a versioned receipt after durable tombstone removal.
    ///
    /// # Errors
    /// Returns an error for an invalid scope, closure ID, or worker identity.
    pub fn new(
        scope: AssignmentScope,
        closure_id: [u8; 32],
        disposition: ResultAckDisposition,
        worker: EndpointId,
    ) -> Result<Self, TransportError> {
        validate_result_identity(scope, closure_id)?;
        validate_endpoint_identity(worker)?;
        Ok(Self {
            version: RESULT_RETIREMENT_SCHEMA_VERSION,
            scope,
            closure_id,
            disposition,
            worker_endpoint_id: *worker.as_bytes(),
        })
    }

    /// Exact assignment scope.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Exact result closure.
    #[must_use]
    pub const fn closure_id(&self) -> [u8; 32] {
        self.closure_id
    }

    /// Exact terminal disposition.
    #[must_use]
    pub const fn disposition(&self) -> ResultAckDisposition {
        self.disposition
    }

    /// Worker endpoint recorded by the final receipt.
    #[must_use]
    pub const fn worker_endpoint_id(&self) -> [u8; 32] {
        self.worker_endpoint_id
    }

    /// Receipt schema version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Whether this final receipt answers the exact confirmation on the
    /// authenticated worker stream.
    #[must_use]
    pub fn matches_confirm(
        &self,
        confirm: &ControlResultRetirementConfirm,
        authenticated_worker: EndpointId,
    ) -> bool {
        self.is_valid()
            && confirm.is_valid()
            && self.subject().matches(confirm.subject())
            && self.worker_endpoint_id == *authenticated_worker.as_bytes()
    }

    fn subject(&self) -> RetirementSubject {
        RetirementSubject::new(self.version, self.scope, self.closure_id, self.disposition)
    }

    fn is_valid(&self) -> bool {
        self.subject().is_valid() && self.worker_endpoint_id != [0; 32]
    }
}

/// Owner request to reconcile one persisted, possibly offered assignment after restart.
///
/// A worker must answer only after checking its durable running, pending-result, and terminal
/// records for this exact scope. `NoResult` is valid only after the worker has durably fenced
/// delayed duplicate Offers for the same scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResultRecoveryQuery {
    /// Exact recovery-query schema version.
    pub version: u16,
    /// Exact assignment being reconciled.
    pub scope: AssignmentScope,
    /// Coordinator identity bound to the authenticated control stream.
    pub coordinator_endpoint_id: [u8; 32],
}

impl ControlResultRecoveryQuery {
    /// Constructs a query for an exact assignment and authenticated owner.
    pub fn new(scope: AssignmentScope, coordinator: EndpointId) -> Result<Self, TransportError> {
        scope.validate()?;
        validate_endpoint_identity(coordinator)?;
        Ok(Self {
            version: RESULT_RECOVERY_SCHEMA_VERSION,
            scope,
            coordinator_endpoint_id: *coordinator.as_bytes(),
        })
    }

    /// Exact assignment scope.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Whether the query is valid and names the authenticated coordinator.
    #[must_use]
    pub fn is_valid_from(&self, authenticated_coordinator: EndpointId) -> bool {
        self.version == RESULT_RECOVERY_SCHEMA_VERSION
            && self.scope.validate().is_ok()
            && self.coordinator_endpoint_id == *authenticated_coordinator.as_bytes()
    }
}

/// Durable worker-side state found while reconciling an exact owner assignment.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ControlResultRecoveryState {
    /// The exact assignment is still executing and has no terminal result yet.
    Running,
    /// A durable result is retained and can be replayed using its exact receipt.
    Pending(ControlResultReceipt),
    /// A terminal result acknowledgement was already recorded and awaits or has received owner
    /// confirmation. The owner must continue only that exact retirement handshake.
    Retired(ControlResultRetired),
    /// No execution or result exists, and a durable tombstone now rejects delayed duplicate
    /// Offers for this exact scope.
    NoResult,
}

/// Fresh worker capacity identity attached to an exact recovery answer.
///
/// This deliberately carries no compiler capability claim: the recovery query names only an
/// assignment scope, so the worker must not infer a recipe or target from its truncated work ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlResultRecoveryCapacity {
    /// Current worker process incarnation.
    pub worker_incarnation: [u8; 32],
    /// Current bounded capacity snapshot revision.
    pub capacity_revision: [u8; 16],
    /// Configured worker resources at response time.
    pub total: super::ProbeResourceCredits,
    /// Resources occupied by other work at response time.
    pub busy: super::ProbeResourceCredits,
}

impl ControlResultRecoveryCapacity {
    fn is_valid(self) -> bool {
        self.worker_incarnation != [0; 32]
            && self.capacity_revision != [0; 16]
            && self.total.cpu_millicores > 0
            && self.total.memory_bytes > 0
            && self.total.transfer_bytes > 0
            && self.busy.cpu_millicores <= self.total.cpu_millicores
            && self.busy.memory_bytes <= self.total.memory_bytes
            && self.busy.transfer_bytes <= self.total.transfer_bytes
    }
}

/// Worker answer to a cold assignment-recovery query.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ControlResultRecoveryStatus {
    /// Exact recovery-query schema version.
    pub version: u16,
    /// Exact assignment being reconciled.
    pub scope: AssignmentScope,
    /// Durable worker state for this assignment.
    pub state: ControlResultRecoveryState,
    /// Fresh process/capacity facts used when a restarted owner adopts this route.
    pub capacity: ControlResultRecoveryCapacity,
    /// Worker identity bound to the authenticated control stream.
    pub worker_endpoint_id: [u8; 32],
}

impl ControlResultRecoveryStatus {
    /// Constructs a checked answer for one exact assignment.
    pub fn new(
        scope: AssignmentScope,
        state: ControlResultRecoveryState,
        capacity: ControlResultRecoveryCapacity,
        worker: EndpointId,
    ) -> Result<Self, TransportError> {
        scope.validate()?;
        validate_endpoint_identity(worker)?;
        let status = Self {
            version: RESULT_RECOVERY_SCHEMA_VERSION,
            scope,
            state,
            capacity,
            worker_endpoint_id: *worker.as_bytes(),
        };
        if !status.is_valid() {
            return Err(TransportError::ControlRejected(
                ControlRejectCode::MessageOrScopeMismatch,
            ));
        }
        Ok(status)
    }

    /// Exact assignment scope.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Worker endpoint identity recorded in this response.
    #[must_use]
    pub const fn worker_endpoint_id(&self) -> [u8; 32] {
        self.worker_endpoint_id
    }

    /// State observed by the worker's durable reconciliation path.
    #[must_use]
    pub const fn state(&self) -> ControlResultRecoveryState {
        self.state
    }

    /// Current authenticated worker process and capacity snapshot.
    #[must_use]
    pub const fn capacity(&self) -> ControlResultRecoveryCapacity {
        self.capacity
    }

    /// Whether this response answers the exact query on the authenticated owner and worker
    /// channel. The worker must make the `NoResult` persistence guarantee before constructing it.
    #[must_use]
    pub fn matches_query(
        &self,
        query: &ControlResultRecoveryQuery,
        authenticated_coordinator: EndpointId,
        authenticated_worker: EndpointId,
    ) -> bool {
        self.is_valid()
            && query.is_valid_from(authenticated_coordinator)
            && self.scope == query.scope
            && self.worker_endpoint_id == *authenticated_worker.as_bytes()
    }

    fn is_valid(&self) -> bool {
        if self.version != RESULT_RECOVERY_SCHEMA_VERSION
            || self.scope.validate().is_err()
            || self.worker_endpoint_id == [0; 32]
            || !self.capacity.is_valid()
        {
            return false;
        }
        match self.state {
            ControlResultRecoveryState::Running | ControlResultRecoveryState::NoResult => true,
            ControlResultRecoveryState::Pending(receipt) => {
                receipt.scope == self.scope && ControlMessage::ResultReceipt(receipt).valid()
            }
            ControlResultRecoveryState::Retired(retired) => {
                retired.scope == self.scope
                    && retired.worker_endpoint_id == self.worker_endpoint_id
                    && retired.is_valid()
            }
        }
    }
}

/// Owner proof that every assignment epoch through a namespace-local prefix is permanently
/// retired. `scope.attempt` is the newer Turso attempt that anchors the proof; the prefix is
/// strictly older. Workers key the durable floor by authenticated coordinator and namespace.
/// This is safe only because Turso increments attempts monotonically per exact namespace, across
/// work IDs. It is not a global epoch across namespaces or independent databases. A coordinator
/// database reset must rotate the coordinator identity or explicitly re-provision worker
/// retirement state; a time-based reset would re-enable delayed offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlNoResultRetireThrough {
    /// Exact no-result retirement schema version.
    pub version: u16,
    /// Exact terminal assignment whose worker tombstone created this debt.
    pub terminal_scope: AssignmentScope,
    /// Exact newer assignment attempt anchoring this retired prefix.
    pub scope: AssignmentScope,
    /// Highest attempt epoch the owner will never offer again for this coordinator+namespace.
    pub retired_through_epoch: u64,
    /// Coordinator identity bound to the authenticated control stream.
    pub coordinator_endpoint_id: [u8; 32],
}

impl ControlNoResultRetireThrough {
    /// Constructs a checked prefix retirement anchored by a newer attempt in the same namespace.
    pub fn new(
        terminal_scope: AssignmentScope,
        scope: AssignmentScope,
        retired_through_epoch: u64,
        coordinator: EndpointId,
    ) -> Result<Self, TransportError> {
        validate_endpoint_identity(coordinator)?;
        let message = Self {
            version: NO_RESULT_RETIREMENT_SCHEMA_VERSION,
            terminal_scope,
            scope,
            retired_through_epoch,
            coordinator_endpoint_id: *coordinator.as_bytes(),
        };
        if !message.is_valid() {
            return Err(TransportError::ControlRejected(
                ControlRejectCode::MessageOrScopeMismatch,
            ));
        }
        Ok(message)
    }

    /// Exact newer assignment that anchors the retirement prefix.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Exact terminal assignment whose tombstone is being retired.
    #[must_use]
    pub const fn terminal_scope(&self) -> AssignmentScope {
        self.terminal_scope
    }

    /// Highest retired attempt epoch, inclusive.
    #[must_use]
    pub const fn retired_through_epoch(&self) -> u64 {
        self.retired_through_epoch
    }

    /// Whether this command is valid on the authenticated coordinator stream.
    #[must_use]
    pub fn matches_coordinator(&self, authenticated_coordinator: EndpointId) -> bool {
        self.is_valid() && self.coordinator_endpoint_id == *authenticated_coordinator.as_bytes()
    }

    fn is_valid(&self) -> bool {
        self.version == NO_RESULT_RETIREMENT_SCHEMA_VERSION
            && self.terminal_scope.validate().is_ok()
            && self.scope.validate().is_ok()
            && self.terminal_scope.namespace_id == self.scope.namespace_id
            && self.retired_through_epoch > 0
            && self.terminal_scope.attempt <= self.retired_through_epoch
            && self.retired_through_epoch < self.scope.attempt
            && self.coordinator_endpoint_id != [0; 32]
    }
}

/// Worker receipt that an owner no-result retirement floor was durably applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlNoResultRetirementApplied {
    /// Exact no-result retirement schema version.
    pub version: u16,
    /// Exact terminal assignment covered by the durable floor.
    pub terminal_scope: AssignmentScope,
    /// Exact newer assignment from the owner's retirement command.
    pub scope: AssignmentScope,
    /// Highest retired attempt epoch, inclusive.
    pub retired_through_epoch: u64,
    /// Worker identity that durably recorded the floor before compacting exact rows.
    pub worker_endpoint_id: [u8; 32],
}

impl ControlNoResultRetirementApplied {
    /// Constructs a checked acknowledgement after durable watermark persistence.
    pub fn new(
        request: &ControlNoResultRetireThrough,
        worker: EndpointId,
    ) -> Result<Self, TransportError> {
        validate_endpoint_identity(worker)?;
        if !request.is_valid() {
            return Err(TransportError::ControlRejected(
                ControlRejectCode::MessageOrScopeMismatch,
            ));
        }
        Ok(Self {
            version: NO_RESULT_RETIREMENT_SCHEMA_VERSION,
            terminal_scope: request.terminal_scope,
            scope: request.scope,
            retired_through_epoch: request.retired_through_epoch,
            worker_endpoint_id: *worker.as_bytes(),
        })
    }

    /// Exact newer assignment from the owner's request.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.scope
    }

    /// Highest retired attempt epoch, inclusive.
    #[must_use]
    pub const fn retired_through_epoch(&self) -> u64 {
        self.retired_through_epoch
    }

    /// Whether this acknowledgement confirms the exact command on the authenticated worker.
    #[must_use]
    pub fn matches_retirement(
        &self,
        request: &ControlNoResultRetireThrough,
        authenticated_worker: EndpointId,
    ) -> bool {
        self.is_valid()
            && request.is_valid()
            && self.terminal_scope == request.terminal_scope
            && self.scope == request.scope
            && self.retired_through_epoch == request.retired_through_epoch
            && self.worker_endpoint_id == *authenticated_worker.as_bytes()
    }

    fn is_valid(&self) -> bool {
        self.version == NO_RESULT_RETIREMENT_SCHEMA_VERSION
            && self.terminal_scope.validate().is_ok()
            && self.scope.validate().is_ok()
            && self.terminal_scope.namespace_id == self.scope.namespace_id
            && self.retired_through_epoch > 0
            && self.terminal_scope.attempt <= self.retired_through_epoch
            && self.retired_through_epoch < self.scope.attempt
            && self.worker_endpoint_id != [0; 32]
    }
}

/// Final result disposition communicated back to the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultAckDisposition {
    /// The owner verified and durably stored the exact closure.
    Stored,
    /// The owner rejected the result after scope, closure, metadata, or admission checks.
    Rejected(ResultRejectReason),
}

/// Shared versioned identity carried by each role-specific retirement DTO.
/// Keeping its validation and equality rules here prevents one phase from
/// drifting away from the others while the wire DTOs remain flat and explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetirementSubject {
    version: u16,
    scope: AssignmentScope,
    closure_id: [u8; 32],
    disposition: ResultAckDisposition,
}

impl RetirementSubject {
    const fn new(
        version: u16,
        scope: AssignmentScope,
        closure_id: [u8; 32],
        disposition: ResultAckDisposition,
    ) -> Self {
        Self {
            version,
            scope,
            closure_id,
            disposition,
        }
    }

    fn is_valid(self) -> bool {
        validate_retirement_fields(self.version, self.scope, self.closure_id).is_ok()
    }

    fn matches(self, other: Self) -> bool {
        self.is_valid() && other.is_valid() && self == other
    }
}

fn validate_retirement_fields(
    version: u16,
    scope: AssignmentScope,
    closure_id: [u8; 32],
) -> Result<(), TransportError> {
    if version != RESULT_RETIREMENT_SCHEMA_VERSION {
        return Err(TransportError::ControlRejected(
            ControlRejectCode::MessageOrScopeMismatch,
        ));
    }
    scope.validate()?;
    if closure_id == [0; 32] {
        return Err(TransportError::ControlRejected(
            ControlRejectCode::MessageOrScopeMismatch,
        ));
    }
    Ok(())
}

fn validate_result_identity(
    scope: AssignmentScope,
    closure_id: [u8; 32],
) -> Result<(), TransportError> {
    validate_retirement_fields(RESULT_RETIREMENT_SCHEMA_VERSION, scope, closure_id)
}

fn validate_endpoint_identity(endpoint: EndpointId) -> Result<(), TransportError> {
    if endpoint.as_bytes() == &[0; 32] {
        return Err(TransportError::ControlRejected(
            ControlRejectCode::MessageOrScopeMismatch,
        ));
    }
    Ok(())
}

/// Which side serves the Bao transfer authorized by one grant page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GrantDirection {
    /// Coordinator serves immutable inputs; worker fetches them.
    InputsToWorker,
    /// Worker serves immutable results; coordinator fetches them.
    ResultsToCoordinator,
}

/// A deterministic, independently replayable page of signed per-object Bao grants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlGrantPage {
    /// Exact assignment scope shared by every capability in this page.
    pub scope: AssignmentScope,
    /// Whether grants authorize input or result transfer.
    pub direction: GrantDirection,
    /// Zero-based stable page position; reconnects may safely request the same page again.
    pub page_index: u32,
    /// Total page count for this fixed attempt and transfer direction.
    pub page_count: u32,
    /// At most 16 per-object capabilities; bulk bytes remain on the Bao ALPN.
    pub grants: Vec<Capability>,
}

/// One authenticated, encrypted assignment control message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ControlMessage {
    /// Coordinator offers one exact package/target attempt.
    Offer(ControlOffer),
    /// Worker accepts or declines one exact offer.
    Accept(ControlAccept),
    /// Coordinator cancels one exact current assignment.
    Cancel(ControlCancel),
    /// Worker rejects one exact offered input closure after provisional acceptance.
    WorkerReject(WorkerReject),
    /// Worker announces a completed closure for owner verification.
    ResultReceipt(ControlResultReceipt),
    /// Coordinator completes result admission with a terminal acknowledgement.
    ResultAck(ControlResultAck),
    /// Coordinator sends one bounded, deterministic page of Bao grants.
    GrantPage(ControlGrantPage),
    /// Worker reports a terminal compiler/store failure after input admission.
    ExecutionFailed(ControlExecutionFailed),
    /// Worker confirms that an exact terminal result ACK is durably retained.
    ResultRetired(ControlResultRetired),
    /// Coordinator permits the worker to durably remove its terminal result tombstone.
    ResultRetirementConfirm(ControlResultRetirementConfirm),
    /// Worker confirms that it durably applied the exact owner retirement confirmation.
    ResultRetirementApplied(ControlResultRetirementApplied),
    /// Coordinator asks the worker to reconcile one persisted assignment after restart.
    ResultRecoveryQuery(ControlResultRecoveryQuery),
    /// Worker reports the exact durable running, pending, retired, or no-result state.
    ResultRecoveryStatus(ControlResultRecoveryStatus),
    /// Coordinator advances the worker's durable no-result fence floor for an exact namespace.
    NoResultRetireThrough(ControlNoResultRetireThrough),
    /// Worker confirms that the no-result floor was fsynced before exact tombstones were removed.
    NoResultRetirementApplied(ControlNoResultRetirementApplied),
}

impl ControlMessage {
    fn scope(&self) -> AssignmentScope {
        match self {
            Self::Offer(message) => message.scope,
            Self::Accept(message) => message.scope,
            Self::Cancel(message) => message.scope,
            Self::WorkerReject(message) => message.scope,
            Self::ExecutionFailed(message) => message.scope,
            Self::ResultReceipt(message) => message.scope,
            Self::ResultAck(message) => message.scope,
            Self::GrantPage(message) => message.scope,
            Self::ResultRetired(message) => message.scope,
            Self::ResultRetirementConfirm(message) => message.scope,
            Self::ResultRetirementApplied(message) => message.scope,
            Self::ResultRecoveryQuery(message) => message.scope,
            Self::ResultRecoveryStatus(message) => message.scope,
            Self::NoResultRetireThrough(message) => message.scope,
            Self::NoResultRetirementApplied(message) => message.scope,
        }
    }

    fn allowed_from(&self, role: ControlRole) -> bool {
        matches!(
            (role, self),
            (
                ControlRole::Coordinator,
                Self::Offer(_)
                    | Self::Cancel(_)
                    | Self::ResultAck(_)
                    | Self::ResultRetirementConfirm(_)
                    | Self::ResultRecoveryQuery(_)
                    | Self::NoResultRetireThrough(_)
            ) | (
                ControlRole::Coordinator,
                Self::GrantPage(ControlGrantPage {
                    direction: GrantDirection::InputsToWorker,
                    ..
                })
            ) | (
                ControlRole::Worker,
                Self::Accept(_)
                    | Self::WorkerReject(_)
                    | Self::ExecutionFailed(_)
                    | Self::ResultReceipt(_)
                    | Self::ResultRetired(_)
                    | Self::ResultRetirementApplied(_)
                    | Self::ResultRecoveryStatus(_)
                    | Self::NoResultRetirementApplied(_)
            ) | (
                ControlRole::Worker,
                Self::GrantPage(ControlGrantPage {
                    direction: GrantDirection::ResultsToCoordinator,
                    ..
                })
            )
        )
    }

    fn valid(&self) -> bool {
        if self.scope().validate().is_err() {
            return false;
        }
        match self {
            Self::Offer(offer) => {
                offer.package_lineage != [0; 32]
                    && offer.target != [0; 32]
                    && offer.recipe != [0; 32]
                    && offer.input_root != [0; 32]
                    && offer.read_manifest != [0; 32]
                    && offer.input_closure_id != [0; 32]
                    && offer.input_manifest_object_id != [0; 32]
                    && offer.selected_base.is_none_or(|base| base != [0; 32])
                    && offer.max_output_bytes > 0
                    && offer.deadline_unix_ms > 0
                    && offer.input_grant_pages <= MAX_CONTROL_GRANT_PAGES
            }
            Self::Accept(_) | Self::Cancel(_) => true,
            Self::WorkerReject(reject) => reject.input_closure_id != [0; 32],
            Self::ExecutionFailed(failure) => failure.input_closure_id != [0; 32],
            Self::ResultReceipt(receipt) => {
                receipt.target_root != [0; 32]
                    && receipt.closure_id != [0; 32]
                    && receipt.object_count > 0
                    && receipt.payload_bytes > 0
                    && receipt.result_grant_pages <= MAX_CONTROL_GRANT_PAGES
            }
            Self::ResultAck(ack) => ack.closure_id != [0; 32],
            Self::ResultRetired(retired) => retired.is_valid(),
            Self::ResultRetirementConfirm(confirm) => confirm.is_valid(),
            Self::ResultRetirementApplied(applied) => applied.is_valid(),
            Self::ResultRecoveryQuery(query) => {
                query.version == RESULT_RECOVERY_SCHEMA_VERSION
                    && query.scope.validate().is_ok()
                    && query.coordinator_endpoint_id != [0; 32]
            }
            Self::ResultRecoveryStatus(status) => status.is_valid(),
            Self::NoResultRetireThrough(retirement) => retirement.is_valid(),
            Self::NoResultRetirementApplied(applied) => applied.is_valid(),
            Self::GrantPage(page) => {
                page.page_count > 0
                    && page.page_count <= MAX_CONTROL_GRANT_PAGES
                    && page.page_index < page.page_count
                    && !page.grants.is_empty()
                    && page.grants.len() <= MAX_OFFER_CAPABILITIES
                    && page
                        .grants
                        .iter()
                        .all(|capability| capability.claims.scope.assignment() == page.scope)
            }
        }
    }

    fn deadline_is_valid(&self, now_ms: u64) -> bool {
        match self {
            Self::Offer(offer) => {
                offer.deadline_unix_ms > now_ms
                    && offer.deadline_unix_ms.saturating_sub(now_ms)
                        <= MAX_CONTROL_OFFER_LIFETIME_MS
            }
            _ => true,
        }
    }

    fn sender_identity_matches(&self, authenticated_sender: EndpointId) -> bool {
        match self {
            Self::ResultRetired(receipt) => {
                receipt.worker_endpoint_id == *authenticated_sender.as_bytes()
            }
            Self::ResultRetirementConfirm(confirmation) => {
                confirmation.coordinator_endpoint_id == *authenticated_sender.as_bytes()
            }
            Self::ResultRetirementApplied(receipt) => {
                receipt.worker_endpoint_id == *authenticated_sender.as_bytes()
            }
            Self::ResultRecoveryQuery(query) => {
                query.coordinator_endpoint_id == *authenticated_sender.as_bytes()
            }
            Self::NoResultRetireThrough(retirement) => {
                retirement.coordinator_endpoint_id == *authenticated_sender.as_bytes()
            }
            Self::ResultRecoveryStatus(status) => {
                status.worker_endpoint_id == *authenticated_sender.as_bytes()
            }
            Self::NoResultRetirementApplied(applied) => {
                applied.worker_endpoint_id == *authenticated_sender.as_bytes()
            }
            _ => true,
        }
    }
}

/// Exact control listener admission rules.
#[derive(Debug, Clone)]
pub struct ControlAdmissionPolicy {
    /// This listener's Iroh identity.
    pub server: EndpointId,
    /// Trusted peer endpoint identities.
    pub allowed_peers: HashSet<EndpointId>,
    /// Exact assignment scope for a single-assignment listener, or `None` for bootstrap / owner
    /// multi-assignment ingress where the first bounded valid frame adopts the stream scope.
    pub scope: Option<AssignmentScope>,
    /// Local role that constrains message direction.
    pub role: ControlRole,
    /// Optional exact namespace required on a worker bootstrap stream, including idle
    /// maintenance first frames.
    worker_namespace: Option<[u8; 16]>,
    owner_routes: Option<super::OwnerClusterAdmissionRegistry>,
}

impl ControlAdmissionPolicy {
    /// Creates an allowlisted policy for one current assignment.
    pub fn new(
        server: EndpointId,
        allowed_peers: impl IntoIterator<Item = EndpointId>,
        scope: AssignmentScope,
        role: ControlRole,
    ) -> Self {
        Self {
            server,
            allowed_peers: allowed_peers.into_iter().collect(),
            scope: Some(scope),
            role,
            worker_namespace: None,
            owner_routes: None,
        }
    }

    /// Creates the worker-side bootstrap policy. Its exact scope is adopted only from the first
    /// authenticated, well-formed Coordinator Offer, before any worker response is sent.
    pub fn worker(
        server: EndpointId,
        allowed_coordinators: impl IntoIterator<Item = EndpointId>,
    ) -> Self {
        Self {
            server,
            allowed_peers: allowed_coordinators.into_iter().collect(),
            scope: None,
            role: ControlRole::Worker,
            worker_namespace: None,
            owner_routes: None,
        }
    }

    /// Creates a worker bootstrap policy that admits an idle maintenance command only when
    /// its exact typed-namespace identifier and authenticated coordinator are already trusted.
    pub fn worker_with_namespace(
        server: EndpointId,
        allowed_coordinators: impl IntoIterator<Item = EndpointId>,
        namespace_id: [u8; 16],
    ) -> Self {
        let mut policy = Self::worker(server, allowed_coordinators);
        policy.worker_namespace = Some(namespace_id);
        policy
    }

    /// Creates an owner-side ingress policy for multiple concurrent assignments from its
    /// allowlisted workers. The transport adopts the scope from each connection's first
    /// bounded, role-valid worker frame; the durable scheduler must validate that scope before
    /// granting artifacts or allocating result storage.
    pub fn coordinator_ingress(
        server: EndpointId,
        allowed_workers: impl IntoIterator<Item = EndpointId>,
    ) -> Self {
        Self {
            server,
            allowed_peers: allowed_workers.into_iter().collect(),
            scope: None,
            role: ControlRole::Coordinator,
            worker_namespace: None,
            owner_routes: None,
        }
    }

    /// Creates owner-side control ingress backed by the live exact-scope route registry.
    pub fn coordinator_router(
        server: EndpointId,
        routes: super::OwnerClusterAdmissionRegistry,
    ) -> Self {
        Self {
            server,
            allowed_peers: HashSet::new(),
            scope: None,
            role: ControlRole::Coordinator,
            worker_namespace: None,
            owner_routes: Some(routes),
        }
    }

    fn allows_peer(&self, peer: EndpointId) -> bool {
        self.owner_routes.as_ref().map_or_else(
            || self.allowed_peers.contains(&peer),
            |routes| routes.allows_peer(peer),
        )
    }

    /// Whether an unscoped worker bootstrap stream may adopt its fence from `message`. Mirrors
    /// [`ControlChannel::worker_bootstrap_message_allowed`] so the namespace-fencing rule stays
    /// testable without a live QUIC connection.
    #[cfg(test)]
    fn worker_bootstrap_message_allowed(&self, message: &ControlMessage) -> bool {
        match message {
            ControlMessage::Offer(offer) => self
                .worker_namespace
                .is_none_or(|namespace_id| offer.scope.namespace_id == namespace_id),
            ControlMessage::NoResultRetireThrough(request) => {
                self.worker_namespace.is_some_and(|namespace_id| {
                    request.terminal_scope.namespace_id == namespace_id
                        && request.scope.namespace_id == namespace_id
                })
            }
            _ => false,
        }
    }
}

/// One direct authenticated bidirectional control stream.
///
/// Each frame is postcard-encoded, capped at 16 KiB, fenced to the exact assignment, and
/// checked against the local role. QUIC flow control bounds queued data; no detached queue is
/// accumulated by this wrapper.
pub struct ControlChannel {
    _connection: Connection,
    send: iroh::endpoint::SendStream,
    receive: iroh::endpoint::RecvStream,
    local: EndpointId,
    peer: EndpointId,
    scope: Option<AssignmentScope>,
    role: ControlRole,
    worker_namespace: Option<[u8; 16]>,
    sent: u8,
    received: u8,
}

impl std::fmt::Debug for ControlChannel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ControlChannel")
            .field("peer", &self.peer)
            .field("scope", &self.scope)
            .field("role", &self.role)
            .field("sent", &self.sent)
            .field("received", &self.received)
            .finish_non_exhaustive()
    }
}

impl ControlChannel {
    /// Authenticated remote endpoint identity from the Iroh TLS certificate.
    #[must_use]
    pub const fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Exact assignment scope after the first valid scoped frame has been received.
    #[must_use]
    pub const fn scope(&self) -> Option<AssignmentScope> {
        self.scope
    }

    /// Send a bounded control message on the encrypted stream.
    pub async fn send(&mut self, message: &ControlMessage) -> Result<(), TransportError> {
        let deadline_valid = match message {
            ControlMessage::Offer(_) => {
                crate::now_unix_ms().is_ok_and(|now_ms| message.deadline_is_valid(now_ms))
            }
            _ => true,
        };
        if self.sent >= MAX_CONTROL_MESSAGES_PER_STREAM
            || !message.allowed_from(self.role)
            || self.scope != Some(message.scope())
            || !message.valid()
            || !deadline_valid
            || !validate_grant_direction(&message, self.role, self.local, self.peer)
            || !message.sender_identity_matches(self.local)
        {
            if !deadline_valid {
                return Err(TransportError::ControlRejected(
                    ControlRejectCode::DeadlineInvalid,
                ));
            }
            return Err(TransportError::ControlRejected(
                ControlRejectCode::MessageOrScopeMismatch,
            ));
        }
        write_frame(&mut self.send, message).await?;
        self.sent += 1;
        Ok(())
    }

    /// Receive and validate one message from the authenticated peer.
    pub async fn receive(&mut self) -> Result<ControlMessage, TransportError> {
        if self.received >= MAX_CONTROL_MESSAGES_PER_STREAM {
            return Err(TransportError::ControlRejected(
                ControlRejectCode::MessageLimit,
            ));
        }
        let message: ControlMessage = read_frame(&mut self.receive).await?;
        let message_scope = message.scope();
        let deadline_valid = match message {
            ControlMessage::Offer(_) => {
                crate::now_unix_ms().is_ok_and(|now_ms| message.deadline_is_valid(now_ms))
            }
            _ => true,
        };
        let scope_matches = match self.scope {
            Some(scope) => message_scope == scope,
            None => match self.role {
                ControlRole::Worker => self.worker_bootstrap_message_allowed(&message),
                // The peer allowlist, role/direction, bounded framing, and message shape are
                // checked below. The owner then delegates the adopted fence to its durable
                // scheduler before it grants access to any artifact catalog.
                ControlRole::Coordinator => true,
            },
        };
        if !message.allowed_from(peer_role(self.role))
            || !scope_matches
            || !message.valid()
            || !deadline_valid
            || !validate_grant_direction(&message, peer_role(self.role), self.peer, self.local)
            || !message.sender_identity_matches(self.peer)
        {
            if !deadline_valid {
                return Err(TransportError::ControlRejected(
                    ControlRejectCode::DeadlineInvalid,
                ));
            }
            return Err(TransportError::ControlRejected(
                ControlRejectCode::MessageOrScopeMismatch,
            ));
        }
        if self.scope.is_none() {
            self.scope = Some(message_scope);
        }
        self.received += 1;
        Ok(message)
    }

    fn worker_bootstrap_message_allowed(&self, message: &ControlMessage) -> bool {
        match message {
            ControlMessage::Offer(offer) => self
                .worker_namespace
                .is_none_or(|namespace_id| offer.scope.namespace_id == namespace_id),
            ControlMessage::NoResultRetireThrough(request) => {
                self.worker_namespace.is_some_and(|namespace_id| {
                    request.terminal_scope.namespace_id == namespace_id
                        && request.scope.namespace_id == namespace_id
                })
            }
            _ => false,
        }
    }

    /// Finish the local stream, wait for peer EOF and receipt, then close the connection.
    ///
    /// A sender must not close the QUIC connection immediately after its final frame: Iroh
    /// documents that this can discard data not yet delivered to the peer application. Both
    /// sides finish after processing the messages they received, so this bounded half-close
    /// handshake confirms the peer consumed the stream before `Connection::close` runs.
    pub async fn finish(self) -> Result<(), TransportError> {
        self.finish_with_timeout(CONTROL_FINISH_TIMEOUT).await
    }

    async fn finish_with_timeout(mut self, timeout: Duration) -> Result<(), TransportError> {
        self.send
            .finish()
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        let deadline = tokio::time::Instant::now() + timeout;

        let peer_eof = async {
            let mut trailing = [0_u8; 1];
            match self.receive.read(&mut trailing).await {
                Ok(None) => Ok(()),
                Ok(Some(_)) => Err(TransportError::ControlRejected(
                    ControlRejectCode::TrailingControlData,
                )),
                Err(error) => Err(TransportError::Iroh(error.to_string())),
            }
        };
        tokio::time::timeout_at(deadline, peer_eof)
            .await
            .map_err(|_| TransportError::ControlRejected(ControlRejectCode::FinishTimeout))??;

        match tokio::time::timeout_at(deadline, self.send.stopped()).await {
            Ok(Ok(None)) => {}
            Ok(Ok(Some(_))) => {
                return Err(TransportError::ControlRejected(
                    ControlRejectCode::FinishIncomplete,
                ));
            }
            Ok(Err(error)) => return Err(TransportError::Iroh(error.to_string())),
            Err(_) => {
                return Err(TransportError::ControlRejected(
                    ControlRejectCode::FinishTimeout,
                ));
            }
        }
        self._connection
            .close(0_u32.into(), b"control exchange complete");
        Ok(())
    }
}

fn peer_role(role: ControlRole) -> ControlRole {
    match role {
        ControlRole::Coordinator => ControlRole::Worker,
        ControlRole::Worker => ControlRole::Coordinator,
    }
}

/// Connect to one allowlisted peer over the dedicated control ALPN.
pub async fn connect_control(
    endpoint: &Endpoint,
    peer_addr: EndpointAddr,
    expected_peer: EndpointId,
    scope: AssignmentScope,
    role: ControlRole,
) -> Result<ControlChannel, TransportError> {
    scope.validate()?;
    let connection = endpoint
        .connect(peer_addr, CONTROL_ALPN)
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    let peer = connection.remote_id();
    if peer != expected_peer || connection.alpn() != CONTROL_ALPN {
        connection.close(1_u32.into(), b"unexpected control peer or ALPN");
        return Err(TransportError::ControlRejected(
            ControlRejectCode::PeerOrAlpnMismatch,
        ));
    }
    let (send, receive) = connection
        .open_bi()
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    Ok(ControlChannel {
        _connection: connection,
        send,
        receive,
        local: endpoint.id(),
        peer,
        scope: Some(scope),
        role,
        worker_namespace: None,
        sent: 0,
        received: 0,
    })
}

/// Accept one allowlisted connection for the dedicated control ALPN.
pub async fn accept_control(
    endpoint: &Endpoint,
    policy: &ControlAdmissionPolicy,
) -> Result<ControlChannel, TransportError> {
    if endpoint.id() != policy.server {
        return Err(TransportError::ControlRejected(
            ControlRejectCode::ListenerMismatch,
        ));
    }
    if policy.scope.is_some_and(|scope| scope.validate().is_err()) {
        return Err(TransportError::InvalidScope);
    }
    let incoming = endpoint
        .accept()
        .await
        .ok_or_else(|| TransportError::Iroh("endpoint closed".into()))?;
    let accepted = incoming
        .accept()
        .map_err(|error| TransportError::Iroh(error.to_string()))?
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    accept_control_connection(endpoint.id(), accepted, policy).await
}

/// Build a control channel from a TLS-authenticated connection already accepted by a shared
/// endpoint dispatcher. This does not call `Endpoint::accept`.
pub(crate) async fn accept_control_connection(
    local: EndpointId,
    accepted: Connection,
    policy: &ControlAdmissionPolicy,
) -> Result<ControlChannel, TransportError> {
    accept_control_connection_with_timeout(local, accepted, policy, CONTROL_ACCEPT_STREAM_TIMEOUT)
        .await
}

pub(crate) async fn accept_control_connection_with_timeout(
    local: EndpointId,
    accepted: Connection,
    policy: &ControlAdmissionPolicy,
    stream_timeout: Duration,
) -> Result<ControlChannel, TransportError> {
    if local != policy.server {
        accepted.close(1_u32.into(), b"control listener identity mismatch");
        return Err(TransportError::ControlRejected(
            ControlRejectCode::ListenerMismatch,
        ));
    }
    if policy.scope.is_some_and(|scope| scope.validate().is_err()) {
        accepted.close(1_u32.into(), b"invalid control assignment scope");
        return Err(TransportError::InvalidScope);
    }
    if accepted.alpn() != CONTROL_ALPN {
        accepted.close(1_u32.into(), b"unexpected control ALPN");
        return Err(TransportError::ControlRejected(
            ControlRejectCode::PeerOrAlpnMismatch,
        ));
    }
    let peer = accepted.remote_id();
    if !policy.allows_peer(peer) {
        accepted.close(1_u32.into(), b"peer is not admitted");
        return Err(TransportError::ControlRejected(
            ControlRejectCode::PeerNotAllowed,
        ));
    }
    let (send, receive) = tokio::time::timeout(stream_timeout, accepted.accept_bi())
        .await
        .map_err(|_| TransportError::Iroh("control stream open timed out".into()))?
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    Ok(ControlChannel {
        _connection: accepted,
        send,
        receive,
        local,
        peer,
        scope: policy.scope,
        role: policy.role,
        worker_namespace: policy.worker_namespace,
        sent: 0,
        received: 0,
    })
}

/// Control protocol admission failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlRejectCode {
    /// TLS-authenticated endpoint is absent from the explicit peer allowlist.
    PeerNotAllowed,
    /// Iroh identity or ALPN does not match the requested control connection.
    PeerOrAlpnMismatch,
    /// The listener policy is for another local endpoint.
    ListenerMismatch,
    /// The message has the wrong scope, direction, or bounded shape.
    MessageOrScopeMismatch,
    /// The finite per-stream message budget was exceeded.
    MessageLimit,
    /// Offer deadline was expired or exceeded the protocol's maximum lifetime.
    DeadlineInvalid,
    /// The peer did not complete the bounded control-stream close handshake.
    FinishTimeout,
    /// The peer stopped the control stream before the sender received completion.
    FinishIncomplete,
    /// Unread control frames remained when the exchange was finished.
    TrailingControlData,
}

impl ControlMessage {
    /// Ensure bulk grants carried by an Offer are under the exact attempt fence.
    #[must_use]
    pub fn grants_match_assignment(&self) -> bool {
        match self {
            Self::Offer(offer) => offer.input_grant_pages <= MAX_CONTROL_GRANT_PAGES,
            Self::GrantPage(page) => page
                .grants
                .iter()
                .all(|capability| capability.claims.scope.assignment() == page.scope),
            _ => true,
        }
    }
}

fn validate_grant_direction(
    message: &ControlMessage,
    sender_role: ControlRole,
    sender: EndpointId,
    receiver: EndpointId,
) -> bool {
    let ControlMessage::GrantPage(page) = message else {
        return true;
    };
    page.grants.iter().all(|capability| {
        let claims = &capability.claims;
        if claims.scope.assignment() != page.scope || validate_claim_shape(claims).is_err() {
            return false;
        }
        match (sender_role, page.direction) {
            (ControlRole::Coordinator, GrantDirection::InputsToWorker)
            | (ControlRole::Worker, GrantDirection::ResultsToCoordinator) => {
                claims.client == receiver && claims.server == sender
            }
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::panic)]

    use super::*;

    async fn send_bounded(
        channel: &mut ControlChannel,
        message: &ControlMessage,
    ) -> Result<(), TransportError> {
        tokio::time::timeout(std::time::Duration::from_secs(5), channel.send(message))
            .await
            .expect("control send timeout")
    }

    async fn receive_bounded(
        channel: &mut ControlChannel,
    ) -> Result<ControlMessage, TransportError> {
        tokio::time::timeout(std::time::Duration::from_secs(5), channel.receive())
            .await
            .expect("control receive timeout")
    }

    async fn finish_bounded(channel: ControlChannel) -> Result<(), TransportError> {
        tokio::time::timeout(std::time::Duration::from_secs(35), channel.finish())
            .await
            .expect("control finish timeout")
    }

    fn assignment() -> AssignmentScope {
        AssignmentScope::new([1; 16], [2; 16], 3, [4; 32]).expect("valid assignment")
    }

    fn offer(deadline_unix_ms: u64) -> ControlOffer {
        ControlOffer {
            scope: assignment(),
            package_lineage: [5; 32],
            target: [6; 32],
            recipe: [7; 32],
            input_root: [8; 32],
            read_manifest: [9; 32],
            input_closure_id: [10; 32],
            input_manifest_object_id: [11; 32],
            selected_base: None,
            max_output_bytes: 1_024,
            deadline_unix_ms,
            input_grant_pages: 0,
        }
    }

    #[test]
    fn offer_binds_input_store_identities_and_has_a_bounded_live_deadline() {
        let now = crate::now_unix_ms().expect("clock");
        let message = ControlMessage::Offer(offer(now + 1_000));
        assert!(message.valid());
        assert!(message.deadline_is_valid(now));
        assert!(!message.deadline_is_valid(now + 1_000));
        assert!(!message.deadline_is_valid(now.saturating_sub(MAX_CONTROL_OFFER_LIFETIME_MS + 1)));

        let mut missing_manifest = offer(now + 1_000);
        missing_manifest.input_manifest_object_id = [0; 32];
        assert!(!ControlMessage::Offer(missing_manifest).valid());
    }

    #[test]
    fn accept_and_cancel_reject_invalid_scopes_on_send_and_wire_decode() {
        let valid = assignment();
        for scope in [
            AssignmentScope {
                namespace_id: [0; 16],
                ..valid
            },
            AssignmentScope {
                work_id: [0; 16],
                ..valid
            },
            AssignmentScope {
                attempt: 0,
                ..valid
            },
            AssignmentScope {
                fence: [0; 32],
                ..valid
            },
        ] {
            let messages = [
                ControlMessage::Accept(ControlAccept {
                    scope,
                    accepted: true,
                }),
                ControlMessage::Cancel(ControlCancel {
                    scope,
                    reason: CancelReason::Requested,
                }),
            ];
            for message in messages {
                assert!(!message.valid());
                let wire = postcard::to_allocvec(&message).expect("serialize forged control");
                assert!(postcard::from_bytes::<ControlMessage>(&wire).is_err());
            }
        }
    }

    #[test]
    fn result_ack_is_terminal_and_owner_to_worker_only() {
        let ack = ControlMessage::ResultAck(ControlResultAck {
            scope: assignment(),
            closure_id: [12; 32],
            disposition: ResultAckDisposition::Rejected(ResultRejectReason::Admission),
        });
        assert!(ack.valid());
        assert!(ack.allowed_from(ControlRole::Coordinator));
        assert!(!ack.allowed_from(ControlRole::Worker));
        assert!(
            !ControlMessage::ResultAck(ControlResultAck {
                scope: assignment(),
                closure_id: [0; 32],
                disposition: ResultAckDisposition::Stored,
            })
            .valid()
        );
    }

    fn endpoint(seed: u8) -> EndpointId {
        crate::SecretKey::from_bytes(&[seed; 32]).public()
    }

    #[test]
    fn result_recovery_query_and_status_bind_scope_peers_and_durable_receipt_shape() {
        let scope = assignment();
        let worker = endpoint(81);
        let coordinator = endpoint(82);
        let query = ControlResultRecoveryQuery::new(scope, coordinator)
            .expect("exact coordinator recovery query");
        let receipt = ControlResultReceipt {
            scope,
            target_root: [83; 32],
            pack_id: None,
            closure_id: [84; 32],
            object_count: 2,
            payload_bytes: 256,
            result_grant_pages: 1,
        };
        let retired = ControlResultRetired::new(
            scope,
            receipt.closure_id,
            ResultAckDisposition::Stored,
            worker,
        )
        .expect("exact retained terminal receipt");
        let capacity = ControlResultRecoveryCapacity {
            worker_incarnation: [85; 32],
            capacity_revision: [86; 16],
            total: crate::ProbeResourceCredits {
                cpu_millicores: 4_000,
                memory_bytes: 8 * 1024 * 1024 * 1024,
                transfer_bytes: 512 * 1024 * 1024,
            },
            busy: crate::ProbeResourceCredits {
                cpu_millicores: 1_000,
                memory_bytes: 1024 * 1024 * 1024,
                transfer_bytes: 64 * 1024 * 1024,
            },
        };

        for state in [
            ControlResultRecoveryState::Running,
            ControlResultRecoveryState::Pending(receipt),
            ControlResultRecoveryState::Retired(retired),
            ControlResultRecoveryState::NoResult,
        ] {
            let status = ControlResultRecoveryStatus::new(scope, state, capacity, worker)
                .expect("valid exact recovery status");
            assert!(status.matches_query(&query, coordinator, worker));
            let message = ControlMessage::ResultRecoveryStatus(status);
            assert!(message.valid());
            assert!(message.allowed_from(ControlRole::Worker));
            assert!(!message.allowed_from(ControlRole::Coordinator));
            assert!(message.sender_identity_matches(worker));
            assert!(!message.sender_identity_matches(coordinator));
            let wire = postcard::to_allocvec(&message).expect("encode recovery response");
            let decoded: ControlMessage =
                postcard::from_bytes(&wire).expect("decode recovery response");
            assert!(decoded.valid());
            assert_eq!(decoded.scope(), scope);
        }

        let query_message = ControlMessage::ResultRecoveryQuery(query);
        assert!(query_message.valid());
        assert!(query_message.allowed_from(ControlRole::Coordinator));
        assert!(!query_message.allowed_from(ControlRole::Worker));
        assert!(query_message.sender_identity_matches(coordinator));
        assert!(!query_message.sender_identity_matches(worker));

        let wrong_scope =
            AssignmentScope::new([1; 16], [99; 16], 3, [4; 32]).expect("other valid scope");
        let mismatched = ControlResultRecoveryStatus::new(
            wrong_scope,
            ControlResultRecoveryState::NoResult,
            capacity,
            worker,
        )
        .expect("valid but unrelated answer");
        assert!(!mismatched.matches_query(&query, coordinator, worker));

        let mismatched_receipt = ControlResultReceipt {
            scope: wrong_scope,
            ..receipt
        };
        assert!(
            ControlResultRecoveryStatus::new(
                scope,
                ControlResultRecoveryState::Pending(mismatched_receipt),
                capacity,
                worker,
            )
            .is_err()
        );
        let other_worker = endpoint(87);
        let wrong_worker_retired = ControlResultRetired::new(
            scope,
            receipt.closure_id,
            ResultAckDisposition::Stored,
            other_worker,
        )
        .expect("valid receipt from a different worker");
        assert!(
            ControlResultRecoveryStatus::new(
                scope,
                ControlResultRecoveryState::Retired(wrong_worker_retired),
                capacity,
                worker,
            )
            .is_err()
        );

        let unknown_version = ControlResultRecoveryStatus {
            version: RESULT_RECOVERY_SCHEMA_VERSION + 1,
            ..ControlResultRecoveryStatus::new(
                scope,
                ControlResultRecoveryState::NoResult,
                capacity,
                worker,
            )
            .expect("valid response")
        };
        assert!(!ControlMessage::ResultRecoveryStatus(unknown_version).valid());
    }

    #[test]
    fn no_result_retirement_is_namespace_monotone_authenticated_and_exactly_acked() {
        let coordinator = crate::SecretKey::generate().public();
        let worker = crate::SecretKey::generate().public();
        let terminal =
            AssignmentScope::new([31; 16], [30; 16], 7, [29; 32]).expect("terminal worker scope");
        let scope =
            AssignmentScope::new([31; 16], [32; 16], 9, [33; 32]).expect("newer authority attempt");
        let request = ControlNoResultRetireThrough::new(terminal, scope, 8, coordinator)
            .expect("strictly older namespace epoch prefix");
        let message = ControlMessage::NoResultRetireThrough(request);
        assert!(message.valid());
        assert!(message.allowed_from(ControlRole::Coordinator));
        assert!(!message.allowed_from(ControlRole::Worker));
        assert!(message.sender_identity_matches(coordinator));
        assert!(!message.sender_identity_matches(worker));
        assert_eq!(message.scope(), scope);

        let encoded = postcard::to_allocvec(&message).expect("encode owner retirement");
        let decoded: ControlMessage =
            postcard::from_bytes(&encoded).expect("decode owner retirement");
        let ControlMessage::NoResultRetireThrough(decoded_request) = decoded else {
            panic!("roundtrip retains the exact retirement variant");
        };
        assert_eq!(decoded_request, request);
        assert!(decoded_request.matches_coordinator(coordinator));
        assert!(!decoded_request.matches_coordinator(worker));

        let applied = ControlNoResultRetirementApplied::new(&request, worker)
            .expect("worker ack after durable floor write");
        let ack = ControlMessage::NoResultRetirementApplied(applied);
        assert!(ack.valid());
        assert!(ack.allowed_from(ControlRole::Worker));
        assert!(!ack.allowed_from(ControlRole::Coordinator));
        assert!(ack.sender_identity_matches(worker));
        assert!(applied.matches_retirement(&request, worker));
        assert!(!applied.matches_retirement(&request, coordinator));

        assert!(ControlNoResultRetireThrough::new(terminal, scope, 0, coordinator).is_err());
        assert!(
            ControlNoResultRetireThrough::new(terminal, scope, scope.attempt, coordinator).is_err()
        );
        let wrong_namespace = AssignmentScope::new([41; 16], [30; 16], 7, [29; 32])
            .expect("other namespace terminal");
        assert!(ControlNoResultRetireThrough::new(wrong_namespace, scope, 8, coordinator).is_err());
        let mut unknown_version = request;
        unknown_version.version += 1;
        assert!(!ControlMessage::NoResultRetireThrough(unknown_version).valid());
    }

    #[test]
    fn idle_maintenance_first_frame_requires_exact_trusted_coordinator_and_namespace() {
        let worker = crate::SecretKey::generate().public();
        let coordinator = crate::SecretKey::generate().public();
        let untrusted = crate::SecretKey::generate().public();
        let namespace = [51; 16];
        let policy =
            ControlAdmissionPolicy::worker_with_namespace(worker, [coordinator], namespace);
        let terminal = AssignmentScope::new(namespace, [52; 16], 4, [53; 32])
            .expect("terminal exact namespace");
        let anchor = AssignmentScope::new(namespace, [54; 16], 5, [55; 32])
            .expect("newer maintenance barrier");
        let exact = ControlMessage::NoResultRetireThrough(
            ControlNoResultRetireThrough::new(terminal, anchor, 4, coordinator)
                .expect("valid idle maintenance command"),
        );
        assert!(policy.worker_bootstrap_message_allowed(&exact));
        assert!(policy.allows_peer(coordinator));
        assert!(!policy.allows_peer(untrusted));
        assert!(exact.sender_identity_matches(coordinator));
        assert!(!exact.sender_identity_matches(untrusted));

        let foreign_terminal =
            AssignmentScope::new([56; 16], [52; 16], 4, [53; 32]).expect("foreign typed namespace");
        let foreign_anchor = AssignmentScope::new([56; 16], [54; 16], 5, [55; 32])
            .expect("foreign maintenance barrier");
        let foreign = ControlMessage::NoResultRetireThrough(
            ControlNoResultRetireThrough::new(foreign_terminal, foreign_anchor, 4, coordinator)
                .expect("valid but foreign namespace command"),
        );
        assert!(!policy.worker_bootstrap_message_allowed(&foreign));
    }

    #[test]
    fn result_retirement_receipts_bind_exact_scope_closure_disposition_and_peer() {
        let scope = assignment();
        let worker = endpoint(91);
        let coordinator = endpoint(92);
        let disposition = ResultAckDisposition::Rejected(ResultRejectReason::Scope);
        let ack = ControlResultAck::new(scope, [93; 32], disposition).expect("valid ACK");
        let retired = ControlResultRetired::new(scope, [93; 32], disposition, worker)
            .expect("valid retirement receipt");
        let confirm =
            ControlResultRetirementConfirm::new(scope, [93; 32], disposition, coordinator)
                .expect("valid retirement confirmation");
        let applied = ControlResultRetirementApplied::new(scope, [93; 32], disposition, worker)
            .expect("valid applied receipt");

        assert!(retired.matches_ack(&ack, worker));
        assert!(confirm.matches_retired(&retired, coordinator));
        assert!(applied.matches_confirm(&confirm, worker));

        let mut substituted_scope = retired;
        substituted_scope.scope.fence[0] ^= 1;
        assert!(!substituted_scope.matches_ack(&ack, worker));
        assert!(!confirm.matches_retired(&substituted_scope, coordinator));

        let mut substituted_closure = retired;
        substituted_closure.closure_id[0] ^= 1;
        assert!(!substituted_closure.matches_ack(&ack, worker));

        let mut substituted_disposition = retired;
        substituted_disposition.disposition = ResultAckDisposition::Stored;
        assert!(!substituted_disposition.matches_ack(&ack, worker));

        let mut substituted_worker = retired;
        substituted_worker.worker_endpoint_id = *endpoint(94).as_bytes();
        assert!(!substituted_worker.matches_ack(&ack, worker));
        assert!(!ControlMessage::ResultRetired(substituted_worker).sender_identity_matches(worker));

        let mut substituted_owner = confirm;
        substituted_owner.coordinator_endpoint_id = *endpoint(95).as_bytes();
        assert!(!substituted_owner.matches_retired(&retired, coordinator));
        assert!(
            !ControlMessage::ResultRetirementConfirm(substituted_owner)
                .sender_identity_matches(coordinator)
        );

        let mut substituted_applied_scope = applied;
        substituted_applied_scope.scope.work_id[0] ^= 1;
        assert!(!substituted_applied_scope.matches_confirm(&confirm, worker));
        assert!(
            ControlMessage::ResultRetirementApplied(substituted_applied_scope)
                .sender_identity_matches(worker)
        );

        let mut substituted_applied_worker = applied;
        substituted_applied_worker.worker_endpoint_id = *endpoint(96).as_bytes();
        assert!(!substituted_applied_worker.matches_confirm(&confirm, worker));
        assert!(
            !ControlMessage::ResultRetirementApplied(substituted_applied_worker)
                .sender_identity_matches(worker)
        );
    }

    #[test]
    fn result_retirement_messages_are_versioned_role_scoped_and_strictly_decoded() {
        let worker = endpoint(101);
        let coordinator = endpoint(102);
        let ack = ControlResultAck::new(assignment(), [103; 32], ResultAckDisposition::Stored)
            .expect("valid ACK");
        let retired =
            ControlResultRetired::new(ack.scope(), ack.closure_id(), ack.disposition(), worker)
                .expect("valid retirement receipt");
        let confirm = ControlResultRetirementConfirm::new(
            ack.scope(),
            ack.closure_id(),
            ack.disposition(),
            coordinator,
        )
        .expect("valid retirement confirmation");
        let applied = ControlResultRetirementApplied::new(
            ack.scope(),
            ack.closure_id(),
            ack.disposition(),
            worker,
        )
        .expect("valid applied receipt");

        let messages = [
            ControlMessage::ResultRetired(retired),
            ControlMessage::ResultRetirementConfirm(confirm),
            ControlMessage::ResultRetirementApplied(applied),
        ];
        for message in messages {
            assert!(message.valid());
            let sender = match &message {
                ControlMessage::ResultRetired(_) | ControlMessage::ResultRetirementApplied(_) => {
                    ControlRole::Worker
                }
                ControlMessage::ResultRetirementConfirm(_) => ControlRole::Coordinator,
                _ => unreachable!(),
            };
            assert!(message.allowed_from(sender));
            assert!(!message.allowed_from(peer_role(sender)));
            let authenticated_sender = match sender {
                ControlRole::Worker => worker,
                ControlRole::Coordinator => coordinator,
            };
            assert!(message.sender_identity_matches(authenticated_sender));
            let wire = postcard::to_allocvec(&message).expect("encode receipt");
            match message {
                ControlMessage::ResultRetired(expected) => {
                    let ControlMessage::ResultRetired(decoded) =
                        postcard::from_bytes(&wire).expect("decode receipt")
                    else {
                        panic!("receipt variant changed");
                    };
                    assert_eq!(decoded, expected);
                }
                ControlMessage::ResultRetirementConfirm(expected) => {
                    let ControlMessage::ResultRetirementConfirm(decoded) =
                        postcard::from_bytes(&wire).expect("decode confirm")
                    else {
                        panic!("confirmation variant changed");
                    };
                    assert_eq!(decoded, expected);
                }
                ControlMessage::ResultRetirementApplied(expected) => {
                    let ControlMessage::ResultRetirementApplied(decoded) =
                        postcard::from_bytes(&wire).expect("decode applied")
                    else {
                        panic!("applied variant changed");
                    };
                    assert_eq!(decoded, expected);
                }
                _ => unreachable!(),
            }
        }

        let mut unknown_version = retired;
        unknown_version.version = RESULT_RETIREMENT_SCHEMA_VERSION + 1;
        assert!(!ControlMessage::ResultRetired(unknown_version).valid());
        let wire = postcard::to_allocvec(&ControlMessage::ResultRetired(unknown_version))
            .expect("encode unknown version");
        let decoded: ControlMessage = postcard::from_bytes(&wire).expect("decode unknown version");
        assert!(!decoded.valid());

        let mut empty_worker = retired;
        empty_worker.worker_endpoint_id = [0; 32];
        assert!(!ControlMessage::ResultRetired(empty_worker).valid());
        let mut empty_closure = applied;
        empty_closure.closure_id = [0; 32];
        assert!(!ControlMessage::ResultRetirementApplied(empty_closure).valid());
    }

    #[test]
    fn result_receipt_allows_streamed_cas_without_physical_pack() {
        let receipt = |pack_id| {
            ControlMessage::ResultReceipt(ControlResultReceipt {
                scope: assignment(),
                target_root: [21; 32],
                pack_id,
                closure_id: [22; 32],
                object_count: 1,
                payload_bytes: 17,
                result_grant_pages: 1,
            })
        };
        assert!(receipt(None).valid());
        assert!(receipt(Some([23; 32])).valid());
        assert!(receipt(None).allowed_from(ControlRole::Worker));
        assert!(!receipt(None).allowed_from(ControlRole::Coordinator));
    }

    #[test]
    fn execution_failed_is_worker_only_and_has_no_result_closure() {
        let failure = ControlMessage::ExecutionFailed(ControlExecutionFailed {
            scope: assignment(),
            input_closure_id: [13; 32],
            reason: ExecutionFailureReason::StorageUnavailable,
        });
        assert!(failure.valid());
        assert!(failure.allowed_from(ControlRole::Worker));
        assert!(!failure.allowed_from(ControlRole::Coordinator));
        assert!(
            !ControlMessage::ExecutionFailed(ControlExecutionFailed {
                scope: assignment(),
                input_closure_id: [0; 32],
                reason: ExecutionFailureReason::CompilerFailed,
            })
            .valid()
        );
    }

    #[tokio::test]
    async fn worker_reject_uses_a_fresh_exact_scope_control_stream() {
        let worker_key = crate::SecretKey::from_bytes(&[41; 32]);
        let owner_key = crate::SecretKey::from_bytes(&[42; 32]);
        let worker_endpoint = crate::bind_direct(
            worker_key.clone(),
            "127.0.0.1:0".parse().expect("loopback socket"),
        )
        .await
        .expect("worker endpoint");
        let owner_endpoint =
            crate::bind_direct(owner_key, "127.0.0.1:0".parse().expect("loopback socket"))
                .await
                .expect("owner endpoint");
        let expected_scope = assignment();
        let policy = ControlAdmissionPolicy::worker(worker_endpoint.id(), [owner_endpoint.id()]);
        let accept_endpoint = worker_endpoint.clone();
        let accept_task = tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                accept_control(&accept_endpoint, &policy),
            )
            .await
            .expect("worker accept timeout")
        });
        let mut owner_channel = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connect_control(
                &owner_endpoint,
                worker_endpoint.addr(),
                worker_endpoint.id(),
                expected_scope,
                ControlRole::Coordinator,
            ),
        )
        .await
        .expect("owner connect timeout")
        .expect("owner control channel");
        let input_closure_id = [10; 32];
        let now = crate::now_unix_ms().expect("clock");
        let mut offered = offer(now + 10_000);
        offered.scope = expected_scope;
        offered.input_closure_id = input_closure_id;
        send_bounded(&mut owner_channel, &ControlMessage::Offer(offered))
            .await
            .expect("send offer");
        let mut worker_channel = accept_task
            .await
            .expect("worker accept task")
            .expect("worker control channel");
        assert!(matches!(
            receive_bounded(&mut worker_channel)
                .await
                .expect("receive offer"),
            ControlMessage::Offer(_)
        ));
        send_bounded(
            &mut worker_channel,
            &ControlMessage::Accept(ControlAccept {
                scope: expected_scope,
                accepted: true,
            }),
        )
        .await
        .expect("provisionally reserve transfer slot");
        assert!(matches!(
            receive_bounded(&mut owner_channel)
                .await
                .expect("receive provisional accept"),
            ControlMessage::Accept(ControlAccept { accepted: true, .. })
        ));
        let (owner_finished, worker_finished) = tokio::join!(
            finish_bounded(owner_channel),
            finish_bounded(worker_channel)
        );
        owner_finished.expect("finish offer channel");
        worker_finished.expect("finish worker's offer channel");

        // Each exchange reconnects directly; the engine's DB authority validates that this
        // later rejection follows the accepted attempt and names the exact offered closure.
        let owner_policy = ControlAdmissionPolicy::new(
            owner_endpoint.id(),
            [worker_endpoint.id()],
            expected_scope,
            ControlRole::Coordinator,
        );
        let owner_listener = owner_endpoint.clone();
        let owner_accept_task = tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                accept_control(&owner_listener, &owner_policy),
            )
            .await
            .expect("owner accept timeout")
        });
        let mut worker_reject_channel = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            connect_control(
                &worker_endpoint,
                owner_endpoint.addr(),
                owner_endpoint.id(),
                expected_scope,
                ControlRole::Worker,
            ),
        )
        .await
        .expect("worker reconnect timeout")
        .expect("worker rejection channel");
        let rejection = |scope, input_closure_id| {
            ControlMessage::WorkerReject(WorkerReject {
                scope,
                input_closure_id,
                reason: WorkerRejectReason::ManifestMismatch,
            })
        };
        assert!(rejection(expected_scope, input_closure_id).allowed_from(ControlRole::Worker));
        assert!(
            !rejection(expected_scope, input_closure_id).allowed_from(ControlRole::Coordinator)
        );
        let stale_scope =
            AssignmentScope::new([1; 16], [2; 16], 2, [44; 32]).expect("stale attempt scope");
        assert!(matches!(
            send_bounded(
                &mut worker_reject_channel,
                &rejection(stale_scope, input_closure_id),
            )
            .await,
            Err(TransportError::ControlRejected(
                ControlRejectCode::MessageOrScopeMismatch
            ))
        ));

        // Bypass the sender wrapper to model a peer that forges a stale fenced assignment.
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            write_frame(
                &mut worker_reject_channel.send,
                &rejection(stale_scope, input_closure_id),
            ),
        )
        .await
        .expect("write forged stale frame timeout")
        .expect("write forged stale frame");
        let mut owner_reject_channel = owner_accept_task
            .await
            .expect("owner accept task")
            .expect("owner rejection channel");
        assert!(matches!(
            receive_bounded(&mut owner_reject_channel).await,
            Err(TransportError::ControlRejected(
                ControlRejectCode::MessageOrScopeMismatch
            ))
        ));
        send_bounded(
            &mut worker_reject_channel,
            &rejection(expected_scope, input_closure_id),
        )
        .await
        .expect("send exact scoped rejection");
        let worker_finish = worker_reject_channel.finish();
        tokio::pin!(worker_finish);
        tokio::select! {
            biased;
            result = &mut worker_finish => panic!("sender finish completed before the peer read: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        assert!(matches!(
            receive_bounded(&mut owner_reject_channel)
                .await
                .expect("receive exact rejection"),
            ControlMessage::WorkerReject(WorkerReject {
                scope,
                input_closure_id: closure,
                reason: WorkerRejectReason::ManifestMismatch,
            }) if scope == expected_scope && closure == input_closure_id
        ));

        let (worker_finished, owner_finished) =
            tokio::join!(worker_finish, finish_bounded(owner_reject_channel));
        worker_finished.expect("finish worker rejection channel after receipt");
        owner_finished.expect("finish owner rejection channel");
        worker_endpoint.close().await;
        owner_endpoint.close().await;
    }

    #[tokio::test]
    async fn execution_failure_is_delivered_on_a_fresh_scoped_control_stream() {
        let worker_endpoint = crate::bind_direct(
            crate::SecretKey::from_bytes(&[71; 32]),
            "127.0.0.1:0".parse().expect("loopback socket"),
        )
        .await
        .expect("worker endpoint");
        let owner_endpoint = crate::bind_direct(
            crate::SecretKey::from_bytes(&[72; 32]),
            "127.0.0.1:0".parse().expect("loopback socket"),
        )
        .await
        .expect("owner endpoint");
        let scope = assignment();
        let input_closure_id = [73; 32];
        let owner_policy = ControlAdmissionPolicy::new(
            owner_endpoint.id(),
            [worker_endpoint.id()],
            scope,
            ControlRole::Coordinator,
        );
        let owner_listener = owner_endpoint.clone();
        let owner_accept = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                accept_control(&owner_listener, &owner_policy),
            )
            .await
            .expect("owner accept timeout")
        });
        let mut worker_channel = tokio::time::timeout(
            Duration::from_secs(5),
            connect_control(
                &worker_endpoint,
                owner_endpoint.addr(),
                owner_endpoint.id(),
                scope,
                ControlRole::Worker,
            ),
        )
        .await
        .expect("worker connect timeout")
        .expect("worker channel");
        let failure = ControlMessage::ExecutionFailed(ControlExecutionFailed {
            scope,
            input_closure_id,
            reason: ExecutionFailureReason::OutputBudget,
        });
        let stale_scope =
            AssignmentScope::new([1; 16], [2; 16], 2, [74; 32]).expect("stale execution attempt");
        assert!(matches!(
            send_bounded(
                &mut worker_channel,
                &ControlMessage::ExecutionFailed(ControlExecutionFailed {
                    scope: stale_scope,
                    input_closure_id,
                    reason: ExecutionFailureReason::OutputBudget,
                }),
            )
            .await,
            Err(TransportError::ControlRejected(
                ControlRejectCode::MessageOrScopeMismatch
            ))
        ));
        send_bounded(&mut worker_channel, &failure)
            .await
            .expect("send execution failure");
        let worker_finish = worker_channel.finish();
        tokio::pin!(worker_finish);
        tokio::select! {
            biased;
            result = &mut worker_finish => panic!("sender finish completed before owner reads failure: {result:?}"),
            () = tokio::task::yield_now() => {}
        }
        let mut owner_channel = owner_accept
            .await
            .expect("owner accept task")
            .expect("owner channel");
        let received = receive_bounded(&mut owner_channel)
            .await
            .expect("receive execution failure");
        assert!(matches!(
            received,
            ControlMessage::ExecutionFailed(received)
                if received == ControlExecutionFailed {
                    scope,
                    input_closure_id,
                    reason: ExecutionFailureReason::OutputBudget,
                }
        ));
        let (worker_finished, owner_finished) =
            tokio::join!(worker_finish, finish_bounded(owner_channel));
        worker_finished.expect("finish worker channel");
        owner_finished.expect("finish owner channel");
        worker_endpoint.close().await;
        owner_endpoint.close().await;
    }

    #[tokio::test]
    async fn control_finish_times_out_if_peer_withholds_fin() {
        let worker_endpoint = crate::bind_direct(
            crate::SecretKey::from_bytes(&[61; 32]),
            "127.0.0.1:0".parse().expect("loopback socket"),
        )
        .await
        .expect("worker endpoint");
        let owner_endpoint = crate::bind_direct(
            crate::SecretKey::from_bytes(&[62; 32]),
            "127.0.0.1:0".parse().expect("loopback socket"),
        )
        .await
        .expect("owner endpoint");
        let scope = assignment();
        let policy = ControlAdmissionPolicy::worker(worker_endpoint.id(), [owner_endpoint.id()]);
        let accept_endpoint = worker_endpoint.clone();
        let accept_task = tokio::spawn(async move {
            tokio::time::timeout(
                Duration::from_secs(5),
                accept_control(&accept_endpoint, &policy),
            )
            .await
            .expect("worker accept timeout")
        });
        let mut owner = tokio::time::timeout(
            Duration::from_secs(5),
            connect_control(
                &owner_endpoint,
                worker_endpoint.addr(),
                worker_endpoint.id(),
                scope,
                ControlRole::Coordinator,
            ),
        )
        .await
        .expect("owner connect timeout")
        .expect("owner channel");
        send_bounded(
            &mut owner,
            &ControlMessage::Offer(offer(crate::now_unix_ms().expect("clock") + 10_000)),
        )
        .await
        .expect("send offer");
        let mut worker = accept_task
            .await
            .expect("worker accept task")
            .expect("worker accept timeout");
        assert!(matches!(
            receive_bounded(&mut worker).await.expect("receive offer"),
            ControlMessage::Offer(_)
        ));
        let result = owner.finish_with_timeout(Duration::from_millis(50)).await;
        assert!(matches!(
            result,
            Err(TransportError::ControlRejected(
                ControlRejectCode::FinishTimeout
            ))
        ));
        worker_endpoint.close().await;
        owner_endpoint.close().await;
    }

    #[tokio::test]
    async fn worker_control_rejects_a_tls_authenticated_but_unallowlisted_peer() {
        let worker_endpoint = crate::bind_direct(
            crate::SecretKey::from_bytes(&[51; 32]),
            "127.0.0.1:0".parse().expect("loopback socket"),
        )
        .await
        .expect("worker endpoint");
        let allowed_owner = crate::bind_direct(
            crate::SecretKey::from_bytes(&[52; 32]),
            "127.0.0.1:0".parse().expect("loopback socket"),
        )
        .await
        .expect("allowed owner endpoint");
        let untrusted_peer = crate::bind_direct(
            crate::SecretKey::from_bytes(&[53; 32]),
            "127.0.0.1:0".parse().expect("loopback socket"),
        )
        .await
        .expect("untrusted peer endpoint");
        let policy = ControlAdmissionPolicy::worker(worker_endpoint.id(), [allowed_owner.id()]);
        let accept_endpoint = worker_endpoint.clone();
        let accept_task = tokio::spawn(async move {
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                accept_control(&accept_endpoint, &policy),
            )
            .await
            .expect("worker accept timeout")
        });
        let rogue_connect = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            untrusted_peer.connect(worker_endpoint.addr(), CONTROL_ALPN),
        )
        .await
        .expect("rogue connect timeout");
        let rejected = accept_task
            .await
            .expect("worker accept task")
            .expect_err("unallowlisted identity must not enter control protocol");
        assert!(matches!(
            rejected,
            TransportError::ControlRejected(ControlRejectCode::PeerNotAllowed)
        ));
        if let Ok(connection) = rogue_connect {
            connection.close(1_u32.into(), b"test complete");
        }
        worker_endpoint.close().await;
        allowed_owner.close().await;
        untrusted_peer.close().await;
    }
}

#[allow(dead_code)]
fn _scope_type_assertions(scope: AssignmentScope, transfer: TransferScope) {
    debug_assert_eq!(transfer.assignment(), scope);
}

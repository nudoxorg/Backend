//! Direct-only Iroh worker admission and durable input-closure receive.
//!
//! This module owns the worker's network-side trust boundary: peers are pinned
//! by Iroh endpoint identity, Offers adopt one exact namespace/work/attempt/
//! fence, every range capability is verified before the local object CAS is
//! queried, and payloads enter the store only through ArtifactSink.

use backend_engine::application::{
    CorrelationId, GenerateTarget, LocalCompilerClient, LocalCompilerExecutionIdentity,
    OwnedPackageSourceSet, PackageCompileRequest, PackageSemanticError,
    PackageSemanticRuntimeError, StagedSemanticPackage,
    compiler_result_envelope_object_from_members_with_versioned_planes,
    compiler_result_output_claim, compiler_result_typed_object_claim,
    compiler_result_versioned_plane_output_claims, reopen_compiler_result_envelope,
    verify_full_workspace_closure_v2,
};
use backend_engine::cluster_transport::{
    AcceptedClusterConnection, AdmissionPolicy, AssignmentScope, Capability, CapabilityIssuer,
    ChunkRange, ClusterListener, CompilerProbeDemand, CompilerProbeIdentity, CompilerProbePage,
    CompilerProbePageReply, ControlAccept, ControlAdmissionPolicy, ControlChannel,
    ControlGrantPage, ControlMessage, ControlNoResultRetireThrough,
    ControlNoResultRetirementApplied, ControlOffer, ControlResultAck, ControlResultReceipt,
    ControlResultRecoveryCapacity, ControlResultRecoveryQuery, ControlResultRecoveryState,
    ControlResultRecoveryStatus, ControlResultRetired, ControlResultRetirementApplied,
    ControlResultRetirementConfirm, ControlRole, Endpoint, EndpointAddr, EndpointId,
    ExecutionFailureReason, GrantDirection, MAX_CONTROL_GRANT_PAGES,
    MAX_CONTROL_MESSAGES_PER_STREAM, MAX_CONTROL_OFFER_LIFETIME_MS, MAX_OFFER_CAPABILITIES,
    MAX_PROBE_CAPACITY_LEASE_MS, MAX_RESPONSE_BYTES, MaterializedObject, ProbeAdmissionPolicy,
    ProbeCapability, ProbeCapabilityReject, ProbeChannel, ProbeObjectClaim, ProbeResourceCredits,
    ProbeWorkerSnapshot, ResultAckDisposition, ResultRejectReason, ResumeState, SecretKey,
    ServeMetrics, ServerState, StoreBlobCatalog, StoreObjectMapping, TransferScope,
    VerifiedCoverage, WorkerRejectReason, accept_control, accept_probe_connection, bind_direct,
    connect_control, now_unix_ms, verify_admission,
};
use backend_engine::compiler_cluster_transport::{
    CompilationUnitKeyV2, CompilerInputManifestV2, CompilerInputMerkleTreeV2,
    CompilerInputTreeKindV2, CompilerInputTreeRecordV2, CompilerPackageTargetV2,
    CompilerWorkspaceFileRoleV2, issue_worker_result_object_range, send_worker_execution_failure,
    send_worker_input_reject, send_worker_result_grant_pages, send_worker_result_receipt,
    validate_compiler_input_path,
};
use backend_semantic::vocabulary::{LanguageProfile, Stage};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectClaim, ArtifactPlan, ClosureId, FileStore,
    GcLimits, GcRoot, GcRoots, ObjectId, StoredClosureReceipt, StreamingClosureBudget,
    StreamingClosureBuilder, TypedObject, UntrustedObjectId,
};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use thiserror::Error;

const BYTES_PER_BAO_CHUNK: u64 = 1_024;
const ARTIFACT_PUT_CHUNK_BYTES: usize = 64 * 1_024;
const RESULT_JOURNAL_MAGIC: &[u8; 8] = b"BKCWRJ02";
const RESULT_JOURNAL_DATA_BYTES: usize = 293;
const RESULT_JOURNAL_BYTES: usize = RESULT_JOURNAL_DATA_BYTES + 32;
const RUNNING_JOURNAL_MAGIC: &[u8; 8] = b"BKCWRI01";
const RUNNING_JOURNAL_DATA_BYTES: usize = 184;
const RUNNING_JOURNAL_BYTES: usize = RUNNING_JOURNAL_DATA_BYTES + 32;
const RETIRED_JOURNAL_MAGIC: &[u8; 8] = b"BKCWRT01";
const RETIRED_JOURNAL_DATA_BYTES: usize = 147;
const RETIRED_JOURNAL_BYTES: usize = RETIRED_JOURNAL_DATA_BYTES + 32;
const NO_RESULT_JOURNAL_MAGIC: &[u8; 8] = b"BKCWNR01";
const NO_RESULT_JOURNAL_DATA_BYTES: usize = 114;
const NO_RESULT_JOURNAL_BYTES: usize = NO_RESULT_JOURNAL_DATA_BYTES + 32;
const NO_RESULT_FLOOR_MAGIC: &[u8; 8] = b"BKCWNF01";
const NO_RESULT_FLOOR_V1_DATA_BYTES: usize = 122;
const NO_RESULT_FLOOR_V1_BYTES: usize = NO_RESULT_FLOOR_V1_DATA_BYTES + 32;
const NO_RESULT_FLOOR_V2_DATA_BYTES: usize = 178;
const NO_RESULT_FLOOR_V2_BYTES: usize = NO_RESULT_FLOOR_V2_DATA_BYTES + 32;
const NO_RESULT_FLOOR_V3_BASE_DATA_BYTES: usize = 126;
const NO_RESULT_TERMINAL_SCOPE_BYTES: usize = 72;
const MAX_NO_RESULT_RETIREMENT_TERMINALS: usize = 4_096;
const NO_RESULT_FLOOR_MAX_BYTES: usize = NO_RESULT_FLOOR_V3_BASE_DATA_BYTES
    + MAX_NO_RESULT_RETIREMENT_TERMINALS * NO_RESULT_TERMINAL_SCOPE_BYTES
    + 32;
const MAX_PENDING_RESULTS: usize = 1_024;
const MAX_RETIRED_RESULTS: usize = 4_096;
const MAX_ACTIVE_ARTIFACT_SERVERS: usize = 4;
const CLUSTER_LISTENER_CAPACITY: usize = 4;
const DEFAULT_CPU_MILLICORES: u32 = 1_000;
const DEFAULT_MEMORY_CREDITS_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_CPU_CREDITS_MILLICORES: u32 = 1_000_000;
const MAX_MEMORY_CREDITS_BYTES: u64 = 1024 * 1024 * 1024 * 1024;
static NEXT_WORKER_INCARNATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static NEXT_RECOVERY_CAPACITY_REVISION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// One explicitly configured direct coordinator.
#[derive(Clone, Debug)]
pub struct ClusterCoordinator {
    /// Iroh identity used for control admission and range-grant signatures.
    pub identity: EndpointId,
    /// Direct IP address supplied out of band by the local owner.
    pub address: EndpointAddr,
}

/// Fixed policy for one local compiler worker process.
#[derive(Clone, Debug)]
pub struct ClusterWorkerPolicy {
    /// Exact Turso authority namespace accepted by this worker.
    pub namespace_id: [u8; 16],
    /// Compiler recipes installed in this worker's local recipe registry.
    pub accepted_recipes: Vec<[u8; 32]>,
    /// Explicit persisted execution grants. An empty deny policy is the default.
    pub execution_policy: ClusterExecutionPolicy,
    /// Maximum output size the worker may be asked to produce.
    pub max_output_bytes: u64,
    /// Persisted scheduler CPU reservation capacity. This is a local admission credit, not an
    /// OS-level CPU isolation guarantee.
    pub cpu_millicores: u32,
    /// Persisted scheduler memory reservation capacity. This is a local admission credit, not
    /// an OS-level memory limit for trusted compiler processes.
    pub memory_bytes: u64,
    /// Maximum number of input closure members admitted in one assignment.
    pub max_input_objects: usize,
    /// Maximum aggregate canonical bytes admitted for one input closure.
    pub max_input_bytes: u64,
    /// Maximum signed range capabilities admitted for one assignment.
    pub max_capabilities: usize,
    /// Maximum grant pages accepted, bounded by the control protocol.
    pub max_grant_pages: u32,
    /// Deadline for control and individual Bao operations.
    pub io_timeout: Duration,
}

/// Typed worker execution boundary, scoped to exact coordinator and compiler identities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerExecutionClass {
    /// Claimed pure in-process recipe. This runtime rejects it until the engine exposes a
    /// verifiable execution capability proving that the selected route has no host/native work.
    PureInProcessParser,
    /// Caller explicitly trusts this exact coordinator to run tools that may read host state.
    /// This is host-trusted execution, not an OS sandbox.
    TrustedCoordinatorHostExecution,
}

/// One explicit compiler execution grant persisted in the worker's local identity config.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerExecutionGrant {
    /// Exact allowlisted coordinator identity authorized to request this invocation.
    pub coordinator: EndpointId,
    /// Exact authority namespace accepted by this grant.
    pub namespace_id: [u8; 16],
    /// Exact compiler recipe digest.
    pub recipe: [u8; 32],
    /// Closed canonical profile discriminator.
    pub profile: [u8; 2],
    /// Closed stage discriminator.
    pub stage: u8,
    /// Exact executable/toolchain identity.
    pub toolchain: [u8; 32],
    /// Exact compiler environment identity.
    pub environment: [u8; 32],
    /// Exact target platform/sysroot identity.
    pub target_platform: [u8; 32],
    /// Host-local compiler authority configuration pinned by the local owner at trust import.
    /// This value never appears in Offers, grants, or result capabilities.
    pub local_authority_fingerprint: [u8; 32],
    /// Whether this grant permits only audited pure parsing or host-trusted tools.
    pub class: WorkerExecutionClass,
}

/// Typed worker execution policy. Callers must provide exact local grants; there is no
/// implicit host-execution or sandbox claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterExecutionPolicy {
    /// Reject every package invocation.
    DenyUnconfined,
    /// Admit only exact persisted grants in this closed list.
    Allowlisted(Box<[WorkerExecutionGrant]>),
}

impl ClusterWorkerPolicy {
    pub(crate) fn validate(&self) -> Result<(), ClusterWorkerError> {
        if self.namespace_id == [0; 16]
            || self.accepted_recipes.is_empty()
            || self
                .accepted_recipes
                .iter()
                .any(|recipe| *recipe == [0; 32])
            || self.max_output_bytes == 0
            || self.cpu_millicores == 0
            || self.cpu_millicores > MAX_CPU_CREDITS_MILLICORES
            || self.memory_bytes == 0
            || self.memory_bytes > MAX_MEMORY_CREDITS_BYTES
            || self.max_input_objects == 0
            || self.max_input_bytes == 0
            || self.max_capabilities == 0
            || self.max_grant_pages == 0
            || self.max_grant_pages > MAX_CONTROL_GRANT_PAGES
            || self.io_timeout.is_zero()
        {
            return Err(ClusterWorkerError::InvalidConfig);
        }
        if let ClusterExecutionPolicy::Allowlisted(grants) = &self.execution_policy {
            let mut keys = std::collections::BTreeSet::new();
            for grant in grants.iter() {
                if grant.namespace_id != self.namespace_id
                    || grant.namespace_id == [0; 16]
                    || grant.recipe == [0; 32]
                    || grant.toolchain == [0; 32]
                    || grant.environment == [0; 32]
                    || grant.target_platform == [0; 32]
                    || grant.local_authority_fingerprint == [0; 32]
                    || grant.stage > 1
                    || !self.accepted_recipes.contains(&grant.recipe)
                    || !keys.insert((
                        grant.coordinator,
                        grant.recipe,
                        grant.profile,
                        grant.stage,
                        grant.toolchain,
                        grant.environment,
                        grant.target_platform,
                    ))
                {
                    return Err(ClusterWorkerError::InvalidConfig);
                }
            }
        }
        Ok(())
    }
}

/// Bounded direct-only worker listener configuration.
#[derive(Clone, Debug)]
pub struct ClusterWorkerConfig {
    /// Explicit local IP and UDP port. Use loopback for same-host workers.
    pub bind_address: SocketAddr,
    /// Long-lived worker Iroh identity; only its public identity is advertised.
    pub identity: SecretKey,
    /// Coordinator identities and direct addresses pinned before endpoint startup.
    pub coordinators: Vec<ClusterCoordinator>,
    /// Namespace, recipe, byte, page, and timeout policy.
    pub policy: ClusterWorkerPolicy,
}

/// Direct-only worker endpoint and its single admitted assignment slot.
#[derive(Debug)]
pub struct ClusterWorker {
    endpoint: Endpoint,
    result_issuer: CapabilityIssuer,
    coordinators: Arc<BTreeMap<EndpointId, ClusterCoordinator>>,
    policy: ClusterWorkerPolicy,
    resources: Arc<Mutex<WorkerResources>>,
    incarnation: [u8; 32],
    no_result_retirement_ready_for: Mutex<Option<PathBuf>>,
}

/// One Offer admitted against this worker's local policy and holding its only job slot.
#[derive(Debug)]
pub struct AdmittedOffer {
    /// Exact validated Offer copied from the authenticated control stream.
    pub offer: ControlOffer,
    /// Authenticated coordinator identity.
    pub coordinator: EndpointId,
    _slot: WorkerSlot,
}

impl AdmittedOffer {
    /// Returns the exact assignment scope learned from the authenticated Offer.
    #[must_use]
    pub const fn scope(&self) -> AssignmentScope {
        self.offer.scope
    }
}

/// Completed durable input closure admitted through the store's typed sink.
#[derive(Debug)]
pub struct AdmittedInputClosure {
    /// Storage-only receipt returned after exact closure verification.
    pub receipt: StoredClosureReceipt,
    /// Exact closure identity used for the receive.
    pub closure_id: [u8; 32],
    /// Durable local FileStore retained by the worker for manifest/compiler reads.
    pub store: FileStore,
    /// Offer-named typed manifest object reopened from the admitted closure.
    pub manifest_object: TypedObject,
    /// Canonical typed invocation manifest, checked against the exact Offer and receipt.
    pub manifest: CompilerInputManifestV2,
    /// Fully reopened and checked immutable workspace inventory page tree.
    pub workspace_tree: CompilerInputMerkleTreeV2,
    /// Exact local execution grant used to admit this invocation.
    pub execution_grant: WorkerExecutionGrant,
}

#[path = "cluster_runtime/workspace_snapshot.rs"]
mod workspace_snapshot;
use workspace_snapshot::{MaterializedWorkspace, materialize_workspace_snapshot};

/// Removes abandoned private compiler snapshots after startup recovery has settled every
/// durable running/result journal. A retained result closure is independent of these workspaces.
pub(crate) fn reap_orphaned_workspace_snapshots(
    parent: impl AsRef<Path>,
) -> Result<usize, ClusterWorkerError> {
    workspace_snapshot::reap_orphaned_workspace_snapshots(parent)
}

/// Durable worker result closure and its exact compiler metadata.
#[derive(Clone, Debug)]
pub struct ClusterWorkerResult {
    /// Exact assignment scope used to produce this result.
    pub scope: AssignmentScope,
    /// Exact coordinator identity authorized to receive this result.
    pub coordinator: EndpointId,
    /// Exact input closure and manifest named by the admitted Offer.
    pub input_closure_id: [u8; 32],
    /// Exact manifest object ID named by the admitted Offer.
    pub input_manifest_object_id: [u8; 32],
    /// Bounded result-transfer and ACK retention deadline, separate from compile deadline.
    pub deadline_unix_ms: u64,
    /// Storage-only receipt from reopening the durable result closure.
    pub receipt: StoredClosureReceipt,
    /// Exact checked backend-store closure identity.
    pub closure: ClosureId,
    /// Optional physical pack identity attached by a storage optimization.
    pub pack_id: Option<[u8; 32]>,
    /// Exact generation root returned by the staged compiler.
    pub target_root: [u8; 32],
    /// Canonical payload bytes across the result closure members.
    pub payload_bytes: u64,
    /// Exact member count in the result closure.
    pub object_count: u32,
    /// Durable CAS owner used for later range serving.
    pub store: FileStore,
}

/// Measurements from the Bao result transfer, suitable for local process diagnostics.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkerTransferDiagnostics {
    /// Number of successfully served Bao range connections.
    pub artifact_ranges: u64,
    /// Bao payload and proof bytes reported by the transport layer.
    pub bao_stream_bytes: u64,
    /// Nearest-rank median per-range handler latency in milliseconds.
    pub range_p50_ms: u64,
    /// Nearest-rank 95th-percentile per-range handler latency in milliseconds.
    pub range_p95_ms: u64,
    /// Process high-water RSS when the platform exposes it (`/proc/self/status`).
    pub peak_rss_bytes: Option<u64>,
}

/// Fixed-memory latency summary for a transfer that may be replayed indefinitely.
/// Each bucket stores values in `[2^n, 2^(n+1)-1]` milliseconds (bucket zero also includes 0).
#[derive(Clone, Debug)]
struct LatencyHistogram {
    buckets: [u64; 64],
    count: u64,
}

impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; 64],
            count: 0,
        }
    }
}

impl LatencyHistogram {
    fn record(&mut self, latency_ms: u64) {
        let bucket = if latency_ms == 0 {
            0
        } else {
            (63 - latency_ms.leading_zeros()) as usize
        };
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.count = self.count.saturating_add(1);
    }

    fn percentile_upper_bound(&self, percentile: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let rank = self.count.saturating_mul(percentile).div_ceil(100).max(1);
        let mut seen = 0_u64;
        for (index, count) in self.buckets.iter().enumerate() {
            seen = seen.saturating_add(*count);
            if seen >= rank {
                return if index == 63 {
                    u64::MAX
                } else {
                    (1_u64 << (index + 1)).saturating_sub(1)
                };
            }
        }
        u64::MAX
    }
}

/// Owner-visible identity of one durable result still awaiting a terminal ACK.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingResultSummary {
    /// Exact work attempt and fencing token.
    pub scope: AssignmentScope,
    /// Exact allowlisted coordinator identity.
    pub coordinator: EndpointId,
    /// Exact input closure used by the completed attempt.
    pub input_closure_id: [u8; 32],
    /// Exact typed input manifest named by the Offer.
    pub input_manifest_object_id: [u8; 32],
    /// Exact result closure retained locally.
    pub result_closure_id: [u8; 32],
    /// Stored typed payload bytes retained for transfer.
    pub payload_bytes: u64,
    /// Number of typed members in the result closure.
    pub object_count: u32,
}

#[derive(Debug)]
struct WorkerSlot(Arc<Mutex<WorkerResources>>);

impl WorkerSlot {
    fn try_acquire(resources: &Arc<Mutex<WorkerResources>>) -> Result<Self, ClusterWorkerError> {
        let mut state = resources
            .lock()
            .map_err(|_| ClusterWorkerError::Operation("worker resource lock poisoned".into()))?;
        let reservation = match std::mem::replace(&mut state.occupancy, SlotOccupancy::Idle) {
            SlotOccupancy::Idle => None,
            SlotOccupancy::Reserved(reservation) => Some(reservation),
            running @ (SlotOccupancy::Replaying { .. } | SlotOccupancy::Assigned(_)) => {
                state.occupancy = running;
                return Err(ClusterWorkerError::SlotBusy);
            }
        };
        state.occupancy = SlotOccupancy::Replaying {
            suspended_reservation: reservation,
        };
        Ok(Self(Arc::clone(resources)))
    }

    fn try_acquire_offer(
        resources: &Arc<Mutex<WorkerResources>>,
        offer: &ControlOffer,
        coordinator: EndpointId,
        now_unix_ms: u64,
    ) -> Result<Self, ClusterWorkerError> {
        let mut state = resources
            .lock()
            .map_err(|_| ClusterWorkerError::Operation("worker resource lock poisoned".into()))?;
        state.expire_reservations(now_unix_ms);
        match &state.occupancy {
            SlotOccupancy::Replaying { .. } | SlotOccupancy::Assigned(_) => {
                return Err(ClusterWorkerError::SlotBusy);
            }
            SlotOccupancy::Reserved(reservation)
                if reservation.coordinator == coordinator
                    && reservation.offer_binding.matches_offer(offer) => {}
            SlotOccupancy::Idle | SlotOccupancy::Reserved(_) => {
                return Err(ClusterWorkerError::OfferDeclined);
            }
        }
        // Consume the sole matching probe reservation. Other probes must re-probe after this
        // Offer occupies the worker's only slot.
        state.occupancy = SlotOccupancy::Assigned(ActiveAssignment {
            coordinator,
            scope: offer.scope,
        });
        Ok(Self(Arc::clone(resources)))
    }
}

impl Drop for WorkerSlot {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.lock() {
            let reservation = match &mut state.occupancy {
                SlotOccupancy::Replaying {
                    suspended_reservation,
                } => suspended_reservation.take(),
                SlotOccupancy::Assigned(_) => None,
                SlotOccupancy::Idle | SlotOccupancy::Reserved(_) => return,
            };
            state.occupancy = reservation.map_or(SlotOccupancy::Idle, SlotOccupancy::Reserved);
        }
    }
}

#[derive(Debug)]
enum SlotOccupancy {
    Idle,
    Reserved(ProbeReservation),
    /// A retained-result replay may temporarily use the slot while preserving a live probe.
    Replaying {
        suspended_reservation: Option<ProbeReservation>,
    },
    Assigned(ActiveAssignment),
}

#[derive(Debug)]
struct WorkerResources {
    total: ProbeResourceCredits,
    occupancy: SlotOccupancy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ActiveAssignment {
    coordinator: EndpointId,
    scope: AssignmentScope,
}

#[derive(Debug)]
struct ProbeReservation {
    coordinator: EndpointId,
    lease_expires_at_unix_ms: u64,
    offer_binding: ProbeOfferBinding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProbeOfferBinding {
    scope: AssignmentScope,
    package_lineage: [u8; 32],
    target: [u8; 32],
    recipe: [u8; 32],
    input_root: [u8; 32],
    read_manifest: [u8; 32],
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    selected_base: Option<[u8; 32]>,
    max_output_bytes: u64,
}

impl ProbeOfferBinding {
    fn from_probe(session: &backend_engine::cluster_transport::CompilerProbeSession) -> Self {
        let identity = &session.identity;
        Self {
            scope: session.scope,
            package_lineage: identity.package_lineage,
            target: identity.target,
            recipe: identity.recipe,
            input_root: identity.input_root,
            read_manifest: identity.read_manifest,
            input_closure_id: identity.input_closure_id,
            input_manifest_object_id: identity.input_manifest_object_id,
            selected_base: identity.selected_base,
            max_output_bytes: identity.max_output_bytes,
        }
    }

    fn matches_offer(self, offer: &ControlOffer) -> bool {
        self == (Self {
            scope: offer.scope,
            package_lineage: offer.package_lineage,
            target: offer.target,
            recipe: offer.recipe,
            input_root: offer.input_root,
            read_manifest: offer.read_manifest,
            input_closure_id: offer.input_closure_id,
            input_manifest_object_id: offer.input_manifest_object_id,
            selected_base: offer.selected_base,
            max_output_bytes: offer.max_output_bytes,
        })
    }
}

impl WorkerResources {
    fn expire_reservations(&mut self, now_unix_ms: u64) {
        let (reserved_expired, running_reservation_expired) = match &self.occupancy {
            SlotOccupancy::Reserved(reservation) => {
                (reservation.lease_expires_at_unix_ms <= now_unix_ms, false)
            }
            SlotOccupancy::Replaying {
                suspended_reservation,
            } => (
                false,
                suspended_reservation
                    .as_ref()
                    .is_some_and(|value| value.lease_expires_at_unix_ms <= now_unix_ms),
            ),
            SlotOccupancy::Assigned(_) => (false, false),
            SlotOccupancy::Idle => (false, false),
        };
        if reserved_expired {
            self.occupancy = SlotOccupancy::Idle;
        } else if running_reservation_expired
            && let SlotOccupancy::Replaying {
                suspended_reservation,
            } = &mut self.occupancy
        {
            suspended_reservation.take();
        }
    }

    fn active_assignment(&self) -> Option<ActiveAssignment> {
        match &self.occupancy {
            SlotOccupancy::Assigned(assignment) => Some(*assignment),
            SlotOccupancy::Idle | SlotOccupancy::Reserved(_) | SlotOccupancy::Replaying { .. } => {
                None
            }
        }
    }

    fn busy(&mut self, now_unix_ms: u64) -> ProbeResourceCredits {
        self.expire_reservations(now_unix_ms);
        if matches!(&self.occupancy, SlotOccupancy::Idle) {
            return ProbeResourceCredits {
                cpu_millicores: 0,
                memory_bytes: 0,
                transfer_bytes: 0,
            };
        }
        self.total
    }

    fn can_reserve(&mut self, demand: ProbeResourceCredits, now_unix_ms: u64) -> bool {
        let busy = self.busy(now_unix_ms);
        matches!(&self.occupancy, SlotOccupancy::Idle)
            && busy
                .cpu_millicores
                .checked_add(demand.cpu_millicores)
                .is_some_and(|used| used <= self.total.cpu_millicores)
            && busy
                .memory_bytes
                .checked_add(demand.memory_bytes)
                .is_some_and(|used| used <= self.total.memory_bytes)
            && busy
                .transfer_bytes
                .checked_add(demand.transfer_bytes)
                .is_some_and(|used| used <= self.total.transfer_bytes)
    }
}

/// Failure in direct worker startup, Offer admission, or durable input receive.
#[derive(Debug, Error)]
pub enum ClusterWorkerError {
    /// Runtime limits or direct coordinator identities were invalid.
    #[error("invalid local-cluster worker configuration")]
    InvalidConfig,
    /// Another assignment already owns the worker's sole compiler slot.
    #[error("worker compiler slot is busy")]
    SlotBusy,
    /// The durable unacknowledged-result quota is full; complete an ACK or explicit abandon.
    #[error("worker has reached its unacknowledged-result quota")]
    PendingResultQuota,
    /// The compact owner-retirement receipt quota is full; reconcile a coordinator first.
    #[error("worker has reached its terminal result-receipt quota")]
    RetirementQuota,
    /// No pending result matched the operator-supplied work/fence/closure triple.
    #[error("no retained result matches the exact work, fence, and closure")]
    PendingResultNotFound,
    /// An authenticated Offer was declined by local namespace or recipe policy.
    #[error("authenticated compiler Offer was declined")]
    OfferDeclined,
    /// The transfer was authenticated, but its manifest or local execution policy failed.
    #[error("compiler input closure or local execution policy was rejected: {0:?}")]
    InputRejected(WorkerRejectReason),
    /// The control stream did not contain the expected Offer, page, or cancellation.
    #[error("unexpected compiler control message")]
    UnexpectedControl,
    /// The Offer or grant set did not preserve the exact assignment scope.
    #[error("compiler assignment scope mismatch")]
    ScopeMismatch,
    /// One authenticated peer was not in the process-start coordinator allowlist.
    #[error("compiler coordinator is not allowlisted")]
    PeerNotAllowed,
    /// Input grant pages were incomplete, reordered, oversized, or inconsistent.
    #[error("input grant page series is invalid")]
    InvalidGrantPages,
    /// The scoped capabilities do not describe one complete, canonical input closure.
    #[error("input grants do not describe a complete canonical closure")]
    InvalidInputClosure,
    /// The transfer exceeded the worker's configured byte or object envelope.
    #[error("input closure exceeds the worker's configured bounds")]
    InputBounds,
    /// A transport operation exceeded the configured deadline.
    #[error("direct worker transport operation timed out")]
    Timeout,
    /// An authenticated coordinator cancelled the exact active assignment.
    #[error("compiler assignment was cancelled")]
    Cancelled,
    /// A transport, store, or filesystem operation failed.
    #[error("cluster worker operation failed: {0}")]
    Operation(String),
}

impl ClusterWorker {
    /// Binds an Iroh endpoint with relays, address lookup, and non-IP transports disabled.
    ///
    /// Every coordinator must have an explicit direct IP address, and its address identity
    /// must equal the configured allowlist identity.
    pub async fn bind(config: ClusterWorkerConfig) -> Result<Self, ClusterWorkerError> {
        config.policy.validate()?;
        if config.coordinators.is_empty() {
            return Err(ClusterWorkerError::InvalidConfig);
        }
        let mut coordinators = BTreeMap::new();
        for coordinator in config.coordinators {
            if coordinator.address.id != coordinator.identity
                || coordinator.address.ip_addrs().next().is_none()
                || coordinator.address.relay_urls().next().is_some()
                || coordinator
                    .address
                    .addrs
                    .iter()
                    .any(|address| !address.is_ip())
                || coordinators
                    .insert(coordinator.identity, coordinator)
                    .is_some()
            {
                return Err(ClusterWorkerError::InvalidConfig);
            }
        }
        if let ClusterExecutionPolicy::Allowlisted(grants) = &config.policy.execution_policy
            && grants
                .iter()
                .any(|grant| !coordinators.contains_key(&grant.coordinator))
        {
            return Err(ClusterWorkerError::InvalidConfig);
        }
        let result_issuer = CapabilityIssuer::new(config.identity.clone());
        let endpoint = bind_direct(config.identity, config.bind_address)
            .await
            .map_err(operation)?;
        let transfer_bytes = config
            .policy
            .max_input_bytes
            .checked_add(config.policy.max_output_bytes)
            .ok_or(ClusterWorkerError::InvalidConfig)?;
        let resources = Arc::new(Mutex::new(WorkerResources {
            total: ProbeResourceCredits {
                cpu_millicores: config.policy.cpu_millicores,
                memory_bytes: config.policy.memory_bytes,
                transfer_bytes,
            },
            occupancy: SlotOccupancy::Idle,
        }));
        let incarnation = worker_incarnation(endpoint.id())?;
        Ok(Self {
            endpoint,
            result_issuer,
            coordinators: Arc::new(coordinators),
            policy: config.policy,
            resources,
            incarnation,
            no_result_retirement_ready_for: Mutex::new(None),
        })
    }

    /// Returns the worker's authenticated endpoint identity.
    #[must_use]
    pub fn identity(&self) -> EndpointId {
        self.endpoint.id()
    }

    /// Returns the explicit direct address peers must receive out of band.
    #[must_use]
    pub fn address(&self) -> EndpointAddr {
        self.endpoint.addr()
    }

    fn prepare_no_result_retirement_state(
        &self,
        store: &FileStore,
    ) -> Result<(), ClusterWorkerError> {
        let store_root = store.root().to_path_buf();
        let mut prepared = self
            .no_result_retirement_ready_for
            .lock()
            .map_err(|_| ClusterWorkerError::Operation("worker recovery lock poisoned".into()))?;
        if prepared.as_ref() == Some(&store_root) {
            return Ok(());
        }
        // A floor is safe only under the exact single-coordinator policy that created it. Validate
        // it before deleting covered rows so a changed coordinator set cannot unmask old Offers.
        let floor = validate_no_result_retirement_configuration(
            store,
            &self.policy,
            self.coordinators.keys().copied(),
        )?;
        // Validate durable negative observations before compaction and again afterward. A crash
        // after the atomic floor write but before row deletion is completed here on cold restart.
        validate_no_result_records(store, &self.policy)?;
        compact_no_result_records_through_floor(store, &self.policy, floor)?;
        validate_no_result_records(store, &self.policy)?;
        *prepared = Some(store_root);
        Ok(())
    }

    fn recovery_capacity(&self) -> Result<ControlResultRecoveryCapacity, ClusterWorkerError> {
        let now = now_unix_ms().map_err(operation)?;
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| ClusterWorkerError::Operation("worker resource lock poisoned".into()))?;
        let busy = resources.busy(now);
        let total = resources.total;
        let sequence = NEXT_RECOVERY_CAPACITY_REVISION
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.recovery-capacity.v1\0");
        hasher.update(&self.incarnation);
        hasher.update(&now.to_be_bytes());
        hasher.update(&sequence.to_be_bytes());
        for credits in [total, busy] {
            hasher.update(&credits.cpu_millicores.to_be_bytes());
            hasher.update(&credits.memory_bytes.to_be_bytes());
            hasher.update(&credits.transfer_bytes.to_be_bytes());
        }
        let digest = hasher.finalize();
        let mut capacity_revision = [0_u8; 16];
        capacity_revision.copy_from_slice(&digest.as_bytes()[..16]);
        if capacity_revision == [0; 16] {
            capacity_revision[0] = 1;
        }
        Ok(ControlResultRecoveryCapacity {
            worker_incarnation: self.incarnation,
            capacity_revision,
            total,
            busy,
        })
    }

    /// Waits for one allowlisted, encrypted Offer and sends the local Accept/decline decision.
    ///
    /// One ALPN dispatcher receives both live probes and Offers. Only an Offer matching a live,
    /// capacity-reserving probe can acquire the slot; every later grant page is checked against
    /// that adopted exact scope.
    pub async fn accept_offer(
        &self,
        compiler: &LocalCompilerClient,
        input_store: &FileStore,
        output_store: &FileStore,
        outboard_root: impl AsRef<Path>,
    ) -> Result<AdmittedOffer, ClusterWorkerError> {
        self.prepare_no_result_retirement_state(output_store)?;
        let allowed = self.coordinators.keys().copied().collect::<Vec<_>>();
        let control_policy = ControlAdmissionPolicy::worker_with_namespace(
            self.endpoint.id(),
            allowed.clone(),
            self.policy.namespace_id,
        );
        let probe_policy = ProbeAdmissionPolicy::worker(self.endpoint.id(), allowed.clone());
        let placeholder_scope =
            TransferScope::new(self.policy.namespace_id, [1; 16], 1, [2; 32], [3; 32])
                .map_err(operation)?;
        let artifact_budget = ArtifactBudget::new(
            1,
            1,
            self.policy.max_output_bytes.max(1),
            ARTIFACT_PUT_CHUNK_BYTES,
            1,
        );
        let state = ServerState::new(
            AdmissionPolicy::new(
                self.endpoint.id(),
                self.endpoint.id(),
                allowed.clone(),
                placeholder_scope,
            ),
            Arc::new(StoreBlobCatalog::new(
                output_store.artifact_sink(artifact_budget),
                outboard_root.as_ref(),
            )),
        );
        let mut listener = ClusterListener::spawn_with_probe(
            self.endpoint.clone(),
            control_policy,
            state.clone(),
            probe_policy.clone(),
            CLUSTER_LISTENER_CAPACITY,
        )
        .map_err(operation)?;
        let (mut offer_channel, offer) = loop {
            let event = tokio::time::timeout(self.policy.io_timeout, listener.recv())
                .await
                .map_err(|_| ClusterWorkerError::Timeout)?;
            match event {
                None => return Err(ClusterWorkerError::UnexpectedControl),
                Some(Err(_rejected_connection)) => continue,
                Some(Ok(AcceptedClusterConnection::Probe(connection))) => {
                    let channel = match tokio::time::timeout(
                        self.policy.io_timeout,
                        accept_probe_connection(connection, &probe_policy),
                    )
                    .await
                    {
                        Ok(Ok(channel)) => channel,
                        // A malformed or stalled probe is a per-connection failure. The
                        // dispatcher remains the sole endpoint acceptor and still waits for
                        // another probe or the actual Offer.
                        Ok(Err(_)) | Err(_) => continue,
                    };
                    // A failed or expired probe is local to that stream. The ALPN dispatcher
                    // remains the only endpoint acceptor and continues waiting for an Offer.
                    let _ = self
                        .serve_probe_channel(channel, Some(compiler), input_store)
                        .await;
                }
                Some(Ok(AcceptedClusterConnection::Artifact(connection))) => {
                    let state = state.clone();
                    tokio::spawn(async move {
                        let _ = connection.serve(&state).await;
                    });
                }
                Some(Ok(AcceptedClusterConnection::Control(mut channel))) => {
                    let coordinator = channel.peer();
                    if !self.coordinators.contains_key(&coordinator) {
                        continue;
                    }
                    let message =
                        match tokio::time::timeout(self.policy.io_timeout, channel.receive()).await
                        {
                            Ok(Ok(message)) => message,
                            Ok(Err(_)) | Err(_) => continue,
                        };
                    match message {
                        ControlMessage::Offer(offer) => break (channel, offer),
                        ControlMessage::ResultRecoveryQuery(query) => {
                            // A recovery query can race a delayed Offer while this listener is
                            // the sole endpoint acceptor. The query handler fsyncs NoResult
                            // before answering, so the following Offer admission observes the
                            // same-scope fence.
                            let _ = self
                                .answer_result_recovery_query(
                                    channel,
                                    query,
                                    output_store,
                                    outboard_root.as_ref(),
                                    None,
                                )
                                .await;
                        }
                        ControlMessage::NoResultRetireThrough(request) => {
                            let _ = self
                                .apply_no_result_retirement(
                                    channel,
                                    request,
                                    output_store,
                                    self.policy.io_timeout,
                                )
                                .await;
                        }
                        ControlMessage::ResultRetirementConfirm(confirm) => {
                            // A duplicate confirmation can arrive after the worker durably
                            // removed its tombstone but before the Applied receipt reached the
                            // owner. Reconcile it on this same sole-accept listener before
                            // considering a new assignment.
                            let _ = self
                                .apply_retirement_confirmation(
                                    channel,
                                    confirm,
                                    output_store,
                                    true,
                                    self.policy.io_timeout,
                                )
                                .await;
                        }
                        ControlMessage::ResultAck(ack) => {
                            let _ = self
                                .resume_retired_acknowledgement(
                                    channel,
                                    ack,
                                    output_store,
                                    self.policy.io_timeout,
                                )
                                .await;
                        }
                        _ => {
                            let _ = self
                                .finish_control_channel_with_timeout(
                                    channel,
                                    self.policy.io_timeout,
                                )
                                .await;
                        }
                    }
                }
            }
        };
        listener.shutdown().await.map_err(operation)?;
        let coordinator = offer_channel.peer();
        let now = now_unix_ms().map_err(operation)?;
        let scope_valid = scope_is_valid(offer.scope)
            && offer_channel.scope() == Some(offer.scope)
            && offer.scope.namespace_id == self.policy.namespace_id;
        let recipe_valid = self.policy.accepted_recipes.contains(&offer.recipe);
        let input_claims_valid = offer.input_closure_id != [0; 32]
            && offer.input_manifest_object_id != [0; 32]
            && offer.input_root != [0; 32]
            && offer.read_manifest != [0; 32];
        let bounds_valid = offer.max_output_bytes > 0
            && offer.max_output_bytes <= self.policy.max_output_bytes
            && offer.input_grant_pages > 0
            && offer.input_grant_pages <= self.policy.max_grant_pages
            && offer.deadline_unix_ms > now
            && offer.deadline_unix_ms.saturating_sub(now) <= MAX_CONTROL_OFFER_LIFETIME_MS;
        let journal_clear = if scope_valid {
            !offer_scope_is_blocked_by_recovery(
                output_store,
                &self.policy,
                coordinator,
                offer.scope,
            )?
        } else {
            false
        };
        let slot =
            if scope_valid && recipe_valid && input_claims_valid && bounds_valid && journal_clear {
                WorkerSlot::try_acquire_offer(&self.resources, &offer, coordinator, now).ok()
            } else {
                None
            };
        let accepted = slot.is_some();
        offer_channel
            .send(&ControlMessage::Accept(ControlAccept {
                scope: offer.scope,
                accepted,
            }))
            .await
            .map_err(operation)?;
        // The Offer may already be expired or invalid; still complete the bounded two-sided
        // control close so the coordinator receives the explicit decline reliably.
        self.finish_control_channel_with_timeout(offer_channel, self.policy.io_timeout)
            .await?;
        let Some(slot) = slot else {
            return Err(ClusterWorkerError::OfferDeclined);
        };
        Ok(AdmittedOffer {
            offer,
            coordinator,
            _slot: slot,
        })
    }

    async fn serve_probe_channel(
        &self,
        mut channel: ProbeChannel,
        compiler: Option<&LocalCompilerClient>,
        input_store: &FileStore,
    ) -> Result<(), ClusterWorkerError> {
        let peer = channel.peer();
        let timeout = self.policy.io_timeout;
        let first = tokio::time::timeout(timeout, channel.receive_challenge_page())
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)?;
        let session = first.session.clone();
        let now = now_unix_ms().map_err(operation)?;
        let (mut inventory_valid, mut capability) = match compiler {
            Some(compiler) => self.probe_capability(compiler, peer, &session, now),
            None => (
                false,
                ProbeCapability::Rejected(ProbeCapabilityReject::PolicyRejected),
            ),
        };
        if first.total_object_count as usize > self.policy.max_input_objects
            || first.total_payload_bytes > self.policy.max_input_bytes
        {
            inventory_valid = false;
            capability = ProbeCapability::Rejected(ProbeCapabilityReject::PolicyRejected);
        }
        let snapshot = self.reserve_probe_capacity(peer, &session, capability, now)?;
        let page_count = first.page_count;
        let mut page = first;
        loop {
            let bitmap = if inventory_valid {
                let store = input_store.clone();
                let claims = page.objects.clone();
                tokio::task::spawn_blocking(move || verified_have_bitmap(&store, &claims))
                    .await
                    .map_err(|error| ClusterWorkerError::Operation(error.to_string()))?
            } else {
                vec![0; page.objects.len().div_ceil(8)]
            };
            let reply = CompilerProbePageReply::new(&page, snapshot, bitmap).map_err(operation)?;
            tokio::time::timeout(timeout, channel.send_have_page(&page, &reply))
                .await
                .map_err(|_| ClusterWorkerError::Timeout)?
                .map_err(operation)?;
            if page.page_index + 1 == page_count {
                break;
            }
            page = tokio::time::timeout(timeout, channel.receive_challenge_page())
                .await
                .map_err(|_| ClusterWorkerError::Timeout)?
                .map_err(operation)?;
        }
        tokio::time::timeout(timeout, channel.finish())
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)
    }

    fn probe_capability(
        &self,
        compiler: &LocalCompilerClient,
        coordinator: EndpointId,
        session: &backend_engine::cluster_transport::CompilerProbeSession,
        now_unix_ms: u64,
    ) -> (bool, ProbeCapability) {
        let identity = &session.identity;
        if !scope_is_valid(session.scope)
            || session.scope.namespace_id != self.policy.namespace_id
            || session.expires_at_unix_ms <= now_unix_ms
            || identity.selected_base.is_some()
            || identity.max_output_bytes > self.policy.max_output_bytes
        {
            return (
                false,
                ProbeCapability::Rejected(ProbeCapabilityReject::PolicyRejected),
            );
        }
        let expected_work_id =
            backend_engine::compiler_cluster_transport::compiler_full_workspace_transfer_work_id(
                identity.package_lineage,
                identity.target,
                identity.recipe,
                identity.read_manifest,
                identity.input_root,
                identity.input_closure_id,
                identity.input_manifest_object_id,
                identity.max_output_bytes,
            );
        if session.scope.work_id != expected_work_id {
            return (
                false,
                ProbeCapability::Rejected(ProbeCapabilityReject::PolicyRejected),
            );
        }
        let package_target = match CompilerPackageTargetV2::decode(&identity.package_target) {
            Ok(target) if target.target().as_ref() == &identity.target => target,
            _ => {
                return (
                    false,
                    ProbeCapability::Rejected(ProbeCapabilityReject::UnsupportedTarget),
                );
            }
        };
        let profile = match LanguageProfile::try_from(identity.profile) {
            Ok(profile) => profile,
            Err(_) => {
                return (
                    false,
                    ProbeCapability::Rejected(ProbeCapabilityReject::UnsupportedRecipe),
                );
            }
        };
        let stage = match Stage::try_from(identity.stage) {
            Ok(stage) => stage,
            Err(_) => {
                return (
                    false,
                    ProbeCapability::Rejected(ProbeCapabilityReject::UnsupportedRecipe),
                );
            }
        };
        let grant = match &self.policy.execution_policy {
            ClusterExecutionPolicy::DenyUnconfined => None,
            ClusterExecutionPolicy::Allowlisted(grants) => grants.iter().find(|grant| {
                grant.coordinator == coordinator
                    && grant.namespace_id == session.scope.namespace_id
                    && grant.recipe == identity.recipe
                    && grant.profile == identity.profile
                    && grant.stage == identity.stage
            }),
        };
        let Some(grant) = grant else {
            return (
                true,
                ProbeCapability::Rejected(ProbeCapabilityReject::PolicyRejected),
            );
        };
        if grant.class == WorkerExecutionClass::PureInProcessParser {
            return (
                true,
                ProbeCapability::Rejected(ProbeCapabilityReject::PolicyRejected),
            );
        }
        let Some(actual) = compiler.execution_identity_for_unit(&package_target, profile, stage)
        else {
            return (
                true,
                ProbeCapability::Rejected(ProbeCapabilityReject::UnsupportedTarget),
            );
        };
        if actual.target().as_ref() != &identity.target
            || <[u8; 2]>::from(actual.profile()) != identity.profile
            || u8::from(actual.stage()) != identity.stage
            || *actual.recipe_identity().as_ref() != identity.recipe
            || actual.toolchain_identity() != grant.toolchain
            || actual.environment_identity() != grant.environment
            || actual.target_platform_identity() != grant.target_platform
            || actual.local_authority_fingerprint() != grant.local_authority_fingerprint
            || grant.local_authority_fingerprint == [0; 32]
        {
            return (
                true,
                ProbeCapability::Rejected(ProbeCapabilityReject::UnsupportedRecipe),
            );
        }
        (
            true,
            ProbeCapability::Supported {
                capability_digest: identity.recipe,
            },
        )
    }

    fn reserve_probe_capacity(
        &self,
        coordinator: EndpointId,
        session: &backend_engine::cluster_transport::CompilerProbeSession,
        capability: ProbeCapability,
        now_unix_ms: u64,
    ) -> Result<ProbeWorkerSnapshot, ClusterWorkerError> {
        let mut resources = self
            .resources
            .lock()
            .map_err(|_| ClusterWorkerError::Operation("worker resource lock poisoned".into()))?;
        resources.expire_reservations(now_unix_ms);
        let busy = resources.busy(now_unix_ms);
        let demand = probe_demand_credits(session.demand);
        if matches!(capability, ProbeCapability::Supported { .. })
            && resources.can_reserve(demand, now_unix_ms)
        {
            let lease_expires_at_unix_ms = session
                .expires_at_unix_ms
                .min(now_unix_ms.saturating_add(u64::from(MAX_PROBE_CAPACITY_LEASE_MS)));
            resources.occupancy = SlotOccupancy::Reserved(ProbeReservation {
                coordinator,
                lease_expires_at_unix_ms,
                offer_binding: ProbeOfferBinding::from_probe(session),
            });
        }
        let capacity_lease_ms = u32::try_from(
            session
                .expires_at_unix_ms
                .saturating_sub(now_unix_ms)
                .min(u64::from(MAX_PROBE_CAPACITY_LEASE_MS)),
        )
        .map_err(operation)?;
        let capacity_revision =
            probe_capacity_revision(self.incarnation, session, busy, capability);
        Ok(ProbeWorkerSnapshot {
            worker_incarnation: self.incarnation,
            capacity_revision,
            total: resources.total,
            busy,
            capability,
            session_affinity_warm: false,
            capacity_lease_ms,
        })
    }

    /// Receives every declared input grant page in order, reopening a separately authenticated
    /// direct control stream after the transport's eight-message bound.
    pub async fn receive_input_grant_pages(
        &self,
        assignment: &AdmittedOffer,
    ) -> Result<Vec<ControlGrantPage>, ClusterWorkerError> {
        let result = self.receive_input_grant_pages_inner(assignment).await;
        if let Err(error) = &result
            && !matches!(error, ClusterWorkerError::Cancelled)
        {
            let reason = match error {
                ClusterWorkerError::Timeout => WorkerRejectReason::DeadlineExceeded,
                ClusterWorkerError::ScopeMismatch => WorkerRejectReason::StaleAttempt,
                ClusterWorkerError::PeerNotAllowed => WorkerRejectReason::PolicyRejected,
                ClusterWorkerError::InputRejected(reason) => *reason,
                _ => WorkerRejectReason::ClosureUnavailable,
            };
            self.send_input_reject(assignment, reason).await?;
        }
        result
    }

    async fn receive_input_grant_pages_inner(
        &self,
        assignment: &AdmittedOffer,
    ) -> Result<Vec<ControlGrantPage>, ClusterWorkerError> {
        let page_count = assignment.offer.input_grant_pages;
        if page_count == 0 || page_count > self.policy.max_grant_pages {
            return Err(ClusterWorkerError::InvalidGrantPages);
        }
        let coordinator = self
            .coordinators
            .get(&assignment.coordinator)
            .ok_or(ClusterWorkerError::PeerNotAllowed)?;
        let max_pages_per_stream = u32::from(MAX_CONTROL_MESSAGES_PER_STREAM);
        let mut pages = Vec::with_capacity(page_count as usize);
        let mut next_page = 0_u32;
        let mut capability_total = 0_usize;
        'next_page: while next_page < page_count {
            let admission = ControlAdmissionPolicy::new(
                self.endpoint.id(),
                [assignment.coordinator],
                assignment.offer.scope,
                ControlRole::Worker,
            );
            let mut channel = tokio::time::timeout(
                self.operation_timeout(assignment.offer.deadline_unix_ms)?,
                accept_control(&self.endpoint, &admission),
            )
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)?;
            if channel.peer() != assignment.coordinator {
                return Err(ClusterWorkerError::PeerNotAllowed);
            }
            let end_page = next_page
                .saturating_add(max_pages_per_stream)
                .min(page_count);
            while next_page < end_page {
                let message = tokio::time::timeout(
                    self.operation_timeout(assignment.offer.deadline_unix_ms)?,
                    channel.receive(),
                )
                .await
                .map_err(|_| ClusterWorkerError::Timeout)?
                .map_err(operation)?;
                let page = match message {
                    ControlMessage::GrantPage(page) => page,
                    ControlMessage::ResultRecoveryQuery(query) => {
                        self.respond_running_recovery_query(channel, query, assignment)
                            .await?;
                        continue 'next_page;
                    }
                    ControlMessage::Cancel(cancel) if cancel.scope == assignment.offer.scope => {
                        self.finish_control_channel(channel, assignment.offer.deadline_unix_ms)
                            .await?;
                        return Err(ClusterWorkerError::Cancelled);
                    }
                    _ => return Err(ClusterWorkerError::UnexpectedControl),
                };
                if page.scope != assignment.offer.scope
                    || page.direction != GrantDirection::InputsToWorker
                    || page.page_index != next_page
                    || page.page_count != page_count
                    || page.grants.is_empty()
                    || page.grants.len() > MAX_OFFER_CAPABILITIES
                {
                    return Err(ClusterWorkerError::InvalidGrantPages);
                }
                capability_total = capability_total
                    .checked_add(page.grants.len())
                    .filter(|count| *count <= self.policy.max_capabilities)
                    .ok_or(ClusterWorkerError::InvalidGrantPages)?;
                pages.push(page);
                next_page += 1;
            }
            self.finish_control_channel(channel, assignment.offer.deadline_unix_ms)
                .await?;
        }
        if coordinator.address.id != assignment.coordinator {
            return Err(ClusterWorkerError::PeerNotAllowed);
        }
        Ok(pages)
    }

    /// Validates every signed capability before local CAS lookup, Bao-fetches missing ranges,
    /// admits the declared input closure, then reopens the exact V2 manifest and workspace tree.
    /// Only fresh full-workspace captures are accepted for remote invocation.
    pub async fn receive_input_closure(
        &self,
        assignment: &AdmittedOffer,
        pages: &[ControlGrantPage],
        store: &FileStore,
        checkpoint_root: impl AsRef<Path>,
    ) -> Result<AdmittedInputClosure, ClusterWorkerError> {
        let result = self
            .receive_input_closure_inner(assignment, pages, store, checkpoint_root.as_ref())
            .await;
        if let Err(error) = &result
            && !matches!(error, ClusterWorkerError::Cancelled)
        {
            let reason = match error {
                ClusterWorkerError::Timeout => WorkerRejectReason::DeadlineExceeded,
                ClusterWorkerError::ScopeMismatch => WorkerRejectReason::StaleAttempt,
                ClusterWorkerError::InputRejected(reason) => *reason,
                ClusterWorkerError::PeerNotAllowed => WorkerRejectReason::PolicyRejected,
                _ => WorkerRejectReason::ClosureUnavailable,
            };
            self.send_input_reject(assignment, reason).await?;
        }
        result
    }

    async fn receive_input_closure_inner(
        &self,
        assignment: &AdmittedOffer,
        pages: &[ControlGrantPage],
        store: &FileStore,
        checkpoint_root: &Path,
    ) -> Result<AdmittedInputClosure, ClusterWorkerError> {
        let coordinator = self
            .coordinators
            .get(&assignment.coordinator)
            .ok_or(ClusterWorkerError::PeerNotAllowed)?;
        let closure_id = assignment.offer.input_closure_id;
        let transfer_scope = TransferScope::new(
            assignment.offer.scope.namespace_id,
            assignment.offer.scope.work_id,
            assignment.offer.scope.attempt,
            assignment.offer.scope.fence,
            closure_id,
        )
        .map_err(operation)?;
        let capabilities = flatten_and_validate_pages(
            pages,
            assignment.offer.scope,
            transfer_scope,
            self.endpoint.id(),
            coordinator.identity,
            self.policy.max_capabilities,
        )?;
        let objects = plan_input_objects(
            capabilities,
            &self.policy,
            coordinator.identity,
            self.endpoint.id(),
            transfer_scope,
        )?;
        let object_count = objects.len();
        let manifest_object_index = objects
            .iter()
            .position(|object| {
                object
                    .mapping
                    .object_id
                    .eq(&assignment.offer.input_manifest_object_id)
            })
            .ok_or(ClusterWorkerError::InvalidInputClosure)?;
        let total_payload_bytes = objects.iter().try_fold(0_u64, |total, object| {
            total.checked_add(object.mapping.payload_length)
        });
        let total_payload_bytes = total_payload_bytes.ok_or(ClusterWorkerError::InputBounds)?;
        if object_count == 0
            || object_count > self.policy.max_input_objects
            || total_payload_bytes > self.policy.max_input_bytes
        {
            return Err(ClusterWorkerError::InputBounds);
        }
        let budget = ArtifactBudget::new(
            object_count,
            self.policy.max_input_objects,
            self.policy.max_input_bytes,
            ARTIFACT_PUT_CHUNK_BYTES,
            put_call_budget(self.policy.max_input_bytes, object_count)?,
        );
        let sink = store.artifact_sink(budget);
        // All peers, scopes, signatures, ranges, typed claims, counts, and aggregate sizes are
        // checked before begin() can inspect a local object path.
        let closure_claim = ArtifactClosureClaim::from_bytes(closure_id);
        let mut session = sink
            .begin(ArtifactPlan::new(
                None,
                closure_claim,
                objects
                    .iter()
                    .map(|object| object.mapping.artifact_claim())
                    .collect(),
                Vec::new(),
            ))
            .map_err(operation)?;
        let have = session.have_bitmap();
        let checkpoint_dir = checkpoint_path_root(checkpoint_root.as_ref(), transfer_scope);
        ensure_private_directory(&checkpoint_dir)?;
        for (object_index, object) in objects.iter().enumerate() {
            if have.is_present(object_index) {
                continue;
            }
            let checkpoint =
                checkpoint_dir.join(format!("{}.checkpoint", hex(&object.mapping.object_id)));
            let Some(first_capability) = object.capabilities.first() else {
                return Err(ClusterWorkerError::InvalidInputClosure);
            };
            if let Ok(resume) = ResumeState::load(&checkpoint, &first_capability.claims)
                && resume.is_complete()
            {
                let timeout = self.operation_timeout(assignment.offer.deadline_unix_ms)?;
                tokio::time::timeout(
                    timeout,
                    resume.feed_to_artifact_session(&checkpoint, object_index, &mut session),
                )
                .await
                .map_err(|_| ClusterWorkerError::Timeout)?
                .map_err(operation)?;
                continue;
            }
            let first_capability = object
                .capabilities
                .first()
                .ok_or(ClusterWorkerError::InvalidInputClosure)?;
            let open_timeout = self.operation_timeout(assignment.offer.deadline_unix_ms)?;
            let mut coverage = tokio::time::timeout(
                open_timeout,
                VerifiedCoverage::open(
                    &self.endpoint,
                    &coordinator.address,
                    coordinator.identity,
                    first_capability,
                    transfer_scope,
                    &checkpoint,
                ),
            )
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)?;
            for capability in &object.capabilities {
                if coverage.state().is_complete() {
                    break;
                }
                let timeout = self.operation_timeout(assignment.offer.deadline_unix_ms)?;
                let fetch = coverage.fetch_range(
                    &self.endpoint,
                    coordinator.address.clone(),
                    capability.clone(),
                );
                let cancel = self.wait_for_cancel(assignment);
                tokio::select! {
                    result = tokio::time::timeout(timeout, fetch) => {
                        result.map_err(|_| ClusterWorkerError::Timeout)?.map_err(operation)?
                    }
                    cancelled = cancel => {
                        cancelled?;
                        return Err(ClusterWorkerError::Cancelled);
                    }
                }
            }
            let resume = coverage.finish();
            if !resume.is_complete() {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            let timeout = self.operation_timeout(assignment.offer.deadline_unix_ms)?;
            tokio::time::timeout(
                timeout,
                resume.feed_to_artifact_session(&checkpoint, object_index, &mut session),
            )
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)?;
        }
        let receipt = session.finish().map_err(operation)?;
        let reopened = store
            .reopen_stored_closure(closure_claim, budget)
            .map_err(operation)?;
        if receipt.closure() != reopened.closure()
            || receipt.object_count() != reopened.object_count()
            || receipt.payload_bytes() != reopened.bytes_verified()
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let manifest_id = UntrustedObjectId::from_bytes(assignment.offer.input_manifest_object_id);
        let manifest_object = store.read_object_claim(manifest_id).map_err(operation)?;
        if manifest_object.id().as_bytes() != &assignment.offer.input_manifest_object_id
            || objects.get(manifest_object_index).is_none_or(|object| {
                object.mapping.object_id != assignment.offer.input_manifest_object_id
            })
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let manifest = CompilerInputManifestV2::admit_for_offer(
            &manifest_object,
            &assignment.offer,
            reopened.closure(),
        )
        .map_err(|_| ClusterWorkerError::InputRejected(WorkerRejectReason::ManifestMismatch))?;
        let workspace_tree = verify_input_closure_members(
            store,
            reopened.closure(),
            assignment.offer.input_manifest_object_id,
            &manifest,
        )
        .map_err(|_| ClusterWorkerError::InputRejected(WorkerRejectReason::ManifestMismatch))?;
        let execution_grant = validate_manifest_for_worker(
            &manifest,
            &workspace_tree,
            &self.policy,
            assignment.coordinator,
        )
        .map_err(ClusterWorkerError::InputRejected)?;
        Ok(AdmittedInputClosure {
            receipt: reopened,
            closure_id,
            store: store.clone(),
            manifest_object,
            manifest,
            workspace_tree,
            execution_grant,
        })
    }

    async fn send_input_reject(
        &self,
        assignment: &AdmittedOffer,
        reason: WorkerRejectReason,
    ) -> Result<(), ClusterWorkerError> {
        let coordinator = self
            .coordinators
            .get(&assignment.coordinator)
            .ok_or(ClusterWorkerError::PeerNotAllowed)?;
        let mut channel = tokio::time::timeout(
            self.policy.io_timeout,
            connect_control(
                &self.endpoint,
                coordinator.address.clone(),
                coordinator.identity,
                assignment.offer.scope,
                ControlRole::Worker,
            ),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        send_worker_input_reject(
            &mut channel,
            assignment.offer.scope,
            assignment.offer.input_closure_id,
            reason,
        )
        .await
        .map_err(operation)?;
        self.finish_control_channel_with_timeout(channel, self.policy.io_timeout)
            .await
    }

    /// Materializes every declared workspace entry from the verified input CAS into a private,
    /// immutable filesystem snapshot. The resulting directory is not an OS sandbox.
    fn materialize_input_snapshot(
        &self,
        assignment: &AdmittedOffer,
        input: &AdmittedInputClosure,
        snapshot_parent: impl AsRef<Path>,
    ) -> Result<MaterializedWorkspace, ClusterWorkerError> {
        let offer_matches = input
            .manifest
            .matches_offer(&assignment.offer, input.receipt.closure())
            .map_err(|_| ClusterWorkerError::InputRejected(WorkerRejectReason::ManifestMismatch))?;
        if input.receipt.closure().as_bytes() != &assignment.offer.input_closure_id
            || input.manifest_object.id().as_bytes() != &assignment.offer.input_manifest_object_id
            || !offer_matches
        {
            return Err(ClusterWorkerError::InputRejected(
                WorkerRejectReason::ManifestMismatch,
            ));
        }
        let workspace_tree = verify_input_closure_members(
            &input.store,
            input.receipt.closure(),
            assignment.offer.input_manifest_object_id,
            &input.manifest,
        )?;
        if workspace_tree != input.workspace_tree
            || validate_manifest_for_worker(
                &input.manifest,
                &workspace_tree,
                &self.policy,
                assignment.coordinator,
            )
            .map_err(ClusterWorkerError::InputRejected)?
                != input.execution_grant
        {
            return Err(ClusterWorkerError::InputRejected(
                WorkerRejectReason::ManifestMismatch,
            ));
        }
        materialize_workspace_snapshot(input, snapshot_parent.as_ref(), self.policy.max_input_bytes)
    }

    /// Executes the exact source frontier through the existing bounded compiler lane, then
    /// streams the staged generation into a fresh unselected FileStore closure.
    ///
    /// The caller must already have admitted `input` through `receive_input_closure`. Host
    /// tool execution is allowed only for the exact `TrustedCoordinatorHostExecution` grant
    /// carried by that admission; a snapshot by itself does not confine native processes.
    pub async fn compile_and_store_result(
        &self,
        assignment: &AdmittedOffer,
        input: &AdmittedInputClosure,
        compiler: Arc<LocalCompilerClient>,
        snapshot_parent: impl AsRef<Path>,
        output_store: &FileStore,
    ) -> Result<ClusterWorkerResult, ClusterWorkerError> {
        let workspace = match self.materialize_input_snapshot(assignment, input, snapshot_parent) {
            Ok(workspace) => workspace,
            Err(error) => {
                let reason = match error {
                    ClusterWorkerError::Timeout => WorkerRejectReason::DeadlineExceeded,
                    ClusterWorkerError::InputRejected(reason) => reason,
                    _ => WorkerRejectReason::SandboxUnavailable,
                };
                self.send_input_reject(assignment, reason).await?;
                return Err(error);
            }
        };
        let request = match PackageCompileRequest::new(
            GenerateTarget {
                correlation: CorrelationId(assignment.offer.scope.attempt),
                profile: input.manifest.profile(),
                stage: input.manifest.stage(),
            },
            input.manifest.package_target().package().clone(),
        ) {
            Ok(request) => request,
            Err(error) => {
                self.send_input_reject(assignment, WorkerRejectReason::ManifestMismatch)
                    .await?;
                return Err(operation(error));
            }
        };
        let source_set = match OwnedPackageSourceSet::new_for_unit(
            request,
            input.manifest.package_target().clone(),
            workspace.root.to_path_buf(),
            workspace.sources,
        ) {
            Ok(source_set) => source_set,
            Err(error) => {
                self.send_input_reject(assignment, WorkerRejectReason::ManifestMismatch)
                    .await?;
                return Err(operation(error));
            }
        };
        let Some(execution_identity) = compiler.execution_identity_for_unit(
            input.manifest.package_target(),
            input.manifest.profile(),
            input.manifest.stage(),
        ) else {
            self.send_input_reject(assignment, WorkerRejectReason::UnsupportedToolchain)
                .await?;
            return Err(ClusterWorkerError::InputRejected(
                WorkerRejectReason::UnsupportedToolchain,
            ));
        };
        if !compiler_execution_identity_matches(
            &execution_identity,
            &input.manifest,
            input.execution_grant,
        ) {
            self.send_input_reject(assignment, WorkerRejectReason::UnsupportedToolchain)
                .await?;
            return Err(ClusterWorkerError::InputRejected(
                WorkerRejectReason::UnsupportedToolchain,
            ));
        }
        if let Err(error) = ensure_pending_result_capacity(output_store) {
            self.send_execution_failure(assignment, ExecutionFailureReason::StorageUnavailable)
                .await?;
            return Err(error);
        }
        if let Err(error) = persist_running_assignment(output_store, assignment) {
            self.send_execution_failure(assignment, ExecutionFailureReason::StorageUnavailable)
                .await?;
            return Err(error);
        }
        let compile_client = Arc::clone(&compiler);
        let compile_task = tokio::task::spawn_blocking(move || {
            compile_client.compile_package_sources_staged(source_set)
        });
        tokio::pin!(compile_task);
        let timeout = match self.operation_timeout(assignment.offer.deadline_unix_ms) {
            Ok(timeout) => timeout,
            Err(error) => {
                compiler.cancel_active();
                let _ = compile_task.await;
                self.send_execution_failure_and_clear(
                    assignment,
                    ExecutionFailureReason::Deadline,
                    output_store,
                )
                .await?;
                return Err(error);
            }
        };
        let staged = tokio::select! {
            result = &mut compile_task => {
                match result {
                    Ok(Ok(staged)) => staged,
                    Ok(Err(error)) => {
                        let reason = compiler_failure_reason(&error);
                        self.send_execution_failure_and_clear(
                            assignment,
                            reason,
                            output_store,
                        )
                        .await?;
                        return Err(operation(error));
                    }
                    Err(error) => {
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::CompilerFailed,
                            output_store,
                        )
                        .await?;
                        return Err(operation(error));
                    }
                }
            }
            cancelled = self.wait_for_cancel(assignment) => {
                compiler.cancel_active();
                match cancelled {
                    Err(ClusterWorkerError::Cancelled) => {
                        let _ = compile_task.await;
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::Cancelled,
                            output_store,
                        )
                        .await?;
                        return Err(ClusterWorkerError::Cancelled);
                    }
                    Err(ClusterWorkerError::Timeout) => {
                        let _ = compile_task.await;
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::Deadline,
                            output_store,
                        )
                        .await?;
                        return Err(ClusterWorkerError::Timeout);
                    }
                    Err(error) => {
                        let _ = compile_task.await;
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::CompilerFailed,
                            output_store,
                        )
                        .await?;
                        return Err(error);
                    }
                    Ok(()) => {
                        let _ = compile_task.await;
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::CompilerFailed,
                            output_store,
                        )
                        .await?;
                        return Err(ClusterWorkerError::UnexpectedControl);
                    }
                }
            }
            () = tokio::time::sleep(timeout) => {
                compiler.cancel_active();
                let _ = compile_task.await;
                self.send_execution_failure_and_clear(
                    assignment,
                    ExecutionFailureReason::Deadline,
                    output_store,
                )
                .await?;
                return Err(ClusterWorkerError::Timeout);
            }
        };
        let output_store_task = output_store.clone();
        let offer = assignment.offer.clone();
        let coordinator = assignment.coordinator;
        let max_output_bytes = self
            .policy
            .max_output_bytes
            .min(assignment.offer.max_output_bytes);
        let max_output_objects = self.policy.max_input_objects;
        let store_task = tokio::task::spawn_blocking(move || {
            store_staged_result(
                &staged,
                &offer,
                coordinator,
                &output_store_task,
                max_output_bytes,
                max_output_objects,
            )
        });
        tokio::pin!(store_task);
        let timeout = match self.operation_timeout(assignment.offer.deadline_unix_ms) {
            Ok(timeout) => timeout,
            Err(error) => {
                let stored = store_task.await;
                if let Ok(Ok(result)) = stored {
                    remove_pending_result(&result)?;
                }
                self.send_execution_failure_and_clear(
                    assignment,
                    ExecutionFailureReason::Deadline,
                    output_store,
                )
                .await?;
                return Err(error);
            }
        };
        tokio::select! {
            result = &mut store_task => {
                match result {
                    Ok(Ok(result)) => Ok(result),
                    Ok(Err(error)) => {
                        let reason = match &error {
                            ClusterWorkerError::InputBounds => ExecutionFailureReason::OutputBudget,
                            ClusterWorkerError::Operation(_) => ExecutionFailureReason::StorageUnavailable,
                            _ => ExecutionFailureReason::CompilerFailed,
                        };
                        self.send_execution_failure_and_clear(
                            assignment,
                            reason,
                            output_store,
                        )
                        .await?;
                        Err(error)
                    }
                    Err(error) => {
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::StorageUnavailable,
                            output_store,
                        )
                            .await?;
                        Err(operation(error))
                    }
                }
            }
            cancelled = self.wait_for_cancel(assignment) => {
                match cancelled {
                    Err(ClusterWorkerError::Cancelled) => {
                        let stored = store_task.await;
                        if let Ok(Ok(result)) = stored {
                            remove_pending_result(&result)?;
                        }
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::Cancelled,
                            output_store,
                        )
                        .await?;
                        Err(ClusterWorkerError::Cancelled)
                    }
                    Err(ClusterWorkerError::Timeout) => {
                        let stored = store_task.await;
                        if let Ok(Ok(result)) = stored {
                            remove_pending_result(&result)?;
                        }
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::Deadline,
                            output_store,
                        )
                        .await?;
                        Err(ClusterWorkerError::Timeout)
                    }
                    Err(error) => {
                        let stored = store_task.await;
                        if let Ok(Ok(result)) = stored {
                            remove_pending_result(&result)?;
                        }
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::CompilerFailed,
                            output_store,
                        )
                        .await?;
                        Err(error)
                    }
                    Ok(()) => {
                        let stored = store_task.await;
                        if let Ok(Ok(result)) = stored {
                            remove_pending_result(&result)?;
                        }
                        self.send_execution_failure_and_clear(
                            assignment,
                            ExecutionFailureReason::CompilerFailed,
                            output_store,
                        )
                        .await?;
                        Err(ClusterWorkerError::UnexpectedControl)
                    }
                }
            }
            () = tokio::time::sleep(timeout) => {
                let stored = store_task.await;
                if let Ok(Ok(result)) = stored {
                    remove_pending_result(&result)?;
                }
                self.send_execution_failure_and_clear(
                    assignment,
                    ExecutionFailureReason::Deadline,
                    output_store,
                )
                .await?;
                Err(ClusterWorkerError::Timeout)
            }
        }
    }

    /// Runs one complete assignment through input admission, staged compilation, durable result
    /// storage, Bao serving, and terminal owner acknowledgement.
    ///
    /// This is the worker's concrete service entry point for a host that has already opened the
    /// worker FileStores and configured the real local compiler. It does not select a local head.
    pub async fn run_one_assignment(
        &self,
        compiler: Arc<LocalCompilerClient>,
        input_store: &FileStore,
        checkpoint_root: impl AsRef<Path>,
        snapshot_parent: impl AsRef<Path>,
        output_store: &FileStore,
        outboard_root: impl AsRef<Path>,
    ) -> Result<ResultAckDisposition, ClusterWorkerError> {
        self.run_one_assignment_with_diagnostics(
            compiler,
            input_store,
            checkpoint_root,
            snapshot_parent,
            output_store,
            outboard_root,
        )
        .await
        .map(|(ack, _)| ack)
    }

    /// Runs one assignment and returns the measured result-transfer profile.
    pub async fn run_one_assignment_with_diagnostics(
        &self,
        compiler: Arc<LocalCompilerClient>,
        input_store: &FileStore,
        checkpoint_root: impl AsRef<Path>,
        snapshot_parent: impl AsRef<Path>,
        output_store: &FileStore,
        outboard_root: impl AsRef<Path>,
    ) -> Result<(ResultAckDisposition, WorkerTransferDiagnostics), ClusterWorkerError> {
        let retained = self.recover_after_restart(output_store).await?;
        for result in &retained {
            self.resume_retained_result_until_ack(result, outboard_root.as_ref())
                .await?;
        }
        ensure_pending_result_capacity(output_store)?;
        let assignment = self
            .accept_offer(&compiler, input_store, output_store, outboard_root.as_ref())
            .await?;
        let pages = self.receive_input_grant_pages(&assignment).await?;
        let input = self
            .receive_input_closure(&assignment, &pages, input_store, checkpoint_root)
            .await?;
        let result = self
            .compile_and_store_result(&assignment, &input, compiler, snapshot_parent, output_store)
            .await?;
        self.serve_result_until_ack_with_diagnostics(&assignment, &result, outboard_root)
            .await
    }

    /// Sends the durable result receipt and signed range grants, serves exact Bao requests, and
    /// retains the closure until the coordinator returns its terminal owner acknowledgement.
    pub async fn serve_result_until_ack(
        &self,
        assignment: &AdmittedOffer,
        result: &ClusterWorkerResult,
        outboard_root: impl AsRef<Path>,
    ) -> Result<ResultAckDisposition, ClusterWorkerError> {
        self.serve_result_until_ack_with_diagnostics(assignment, result, outboard_root)
            .await
            .map(|(ack, _)| ack)
    }

    /// As [`Self::serve_result_until_ack`], returning per-range latency and byte diagnostics.
    pub async fn serve_result_until_ack_with_diagnostics(
        &self,
        assignment: &AdmittedOffer,
        result: &ClusterWorkerResult,
        outboard_root: impl AsRef<Path>,
    ) -> Result<(ResultAckDisposition, WorkerTransferDiagnostics), ClusterWorkerError> {
        if result.scope != assignment.offer.scope
            || result.coordinator != assignment.coordinator
            || result.input_closure_id != assignment.offer.input_closure_id
            || result.input_manifest_object_id != assignment.offer.input_manifest_object_id
        {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        self.serve_result_context(result, outboard_root).await
    }

    /// Reopens all unacknowledged result records after a cold process restart.
    ///
    /// Every record is cross-checked against its durable closure and shared typed result
    /// envelope. Invalid or partial records fail closed; no local head is read or selected.
    pub fn recover_pending_results(
        &self,
        store: &FileStore,
    ) -> Result<Vec<ClusterWorkerResult>, ClusterWorkerError> {
        recover_pending_results(store, &self.policy)
    }

    /// Reconciles interrupted work before accepting new assignments after a cold restart.
    ///
    /// Completed result records are returned for re-serving. Any assignment journal that has
    /// no completed result is reported to its exact allowlisted coordinator as a typed
    /// `CompilerFailed` or `Deadline` terminal, then removed only after the control exchange
    /// completes. Callers should resume returned results before accepting a new Offer.
    pub async fn recover_after_restart(
        &self,
        store: &FileStore,
    ) -> Result<Vec<ClusterWorkerResult>, ClusterWorkerError> {
        self.recover_retired_result_records(store)?;
        self.prepare_no_result_retirement_state(store)?;
        let results = self.recover_pending_results(store)?;
        let completed_scopes = results
            .iter()
            .map(|result| result.scope)
            .collect::<Vec<_>>();
        let directory = result_journal_directory(store, false)?;
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(results),
            Err(error) => return Err(operation(error)),
        };
        let mut running_records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(operation)?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("running") {
                continue;
            }
            let metadata = fs::symlink_metadata(&path).map_err(operation)?;
            validate_journal_metadata(&metadata, RUNNING_JOURNAL_BYTES)?;
            let mut file = open_private_read_file(&path, RUNNING_JOURNAL_BYTES)?;
            let mut bytes = Vec::with_capacity(RUNNING_JOURNAL_BYTES);
            file.by_ref()
                .take((RUNNING_JOURNAL_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(operation)?;
            let record = RunningAssignmentRecord::decode(&bytes)?;
            if result_journal_path(&directory, record.scope, "running") != path
                || record.scope.namespace_id != self.policy.namespace_id
                || !self
                    .coordinators
                    .contains_key(&EndpointId::from_bytes(&record.coordinator).map_err(operation)?)
            {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            running_records.push(record);
        }
        for record in running_records {
            if completed_scopes.contains(&record.scope) {
                remove_running_assignment(store, record.scope)?;
                continue;
            }
            let coordinator_id = EndpointId::from_bytes(&record.coordinator).map_err(operation)?;
            let coordinator = self
                .coordinators
                .get(&coordinator_id)
                .ok_or(ClusterWorkerError::PeerNotAllowed)?;
            let reason = if now_unix_ms().map_err(operation)? >= record.deadline_unix_ms {
                ExecutionFailureReason::Deadline
            } else {
                ExecutionFailureReason::CompilerFailed
            };
            tokio::time::timeout(
                self.policy.io_timeout,
                send_worker_execution_failure(
                    &self.endpoint,
                    coordinator.address.clone(),
                    coordinator.identity,
                    record.scope,
                    record.input_closure_id,
                    reason,
                ),
            )
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)?;
            remove_running_assignment(store, record.scope)?;
        }
        Ok(results)
    }

    fn recover_retired_result_records(&self, store: &FileStore) -> Result<(), ClusterWorkerError> {
        for record in read_retired_result_records(store, &self.policy)? {
            let coordinator = EndpointId::from_bytes(&record.coordinator).map_err(operation)?;
            if !self.coordinators.contains_key(&coordinator) {
                return Err(ClusterWorkerError::PeerNotAllowed);
            }
            validate_retired_result_sources(store, record)?;
            retire_result_payload_state(store, record)?;
        }
        Ok(())
    }

    /// Re-serves one verified durable result after restart until the owner returns a terminal
    /// Stored/Rejected acknowledgement. The worker assignment slot remains exclusive.
    pub async fn resume_retained_result_until_ack(
        &self,
        result: &ClusterWorkerResult,
        outboard_root: impl AsRef<Path>,
    ) -> Result<ResultAckDisposition, ClusterWorkerError> {
        self.resume_retained_result_until_ack_with_diagnostics(result, outboard_root)
            .await
            .map(|(ack, _)| ack)
    }

    /// Re-serves a retained result and reports actual Bao bytes, per-range p50/p95, and RSS.
    pub async fn resume_retained_result_until_ack_with_diagnostics(
        &self,
        result: &ClusterWorkerResult,
        outboard_root: impl AsRef<Path>,
    ) -> Result<(ResultAckDisposition, WorkerTransferDiagnostics), ClusterWorkerError> {
        let _slot = WorkerSlot::try_acquire(&self.resources)?;
        self.serve_result_context(result, outboard_root).await
    }

    async fn serve_result_context(
        &self,
        result: &ClusterWorkerResult,
        outboard_root: impl AsRef<Path>,
    ) -> Result<(ResultAckDisposition, WorkerTransferDiagnostics), ClusterWorkerError> {
        if result.receipt.closure().as_bytes() != result.closure.as_bytes()
            || result.object_count == 0
            || result.payload_bytes == 0
            || !scope_is_valid(result.scope)
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let transfer_deadline_unix_ms = now_unix_ms()
            .map_err(operation)?
            .checked_add(MAX_CONTROL_OFFER_LIFETIME_MS)
            .ok_or(ClusterWorkerError::InputBounds)?;
        self.operation_timeout(transfer_deadline_unix_ms)?;
        let coordinator = self
            .coordinators
            .get(&result.coordinator)
            .ok_or(ClusterWorkerError::PeerNotAllowed)?;
        let store_budget = ArtifactBudget::new(
            self.policy.max_input_objects,
            self.policy.max_input_objects,
            self.policy.max_input_bytes.max(result.payload_bytes),
            ARTIFACT_PUT_CHUNK_BYTES,
            put_call_budget(
                self.policy.max_input_bytes.max(result.payload_bytes),
                self.policy.max_input_objects,
            )?,
        );
        let catalog = Arc::new(StoreBlobCatalog::new(
            result.store.artifact_sink(store_budget),
            outboard_root.as_ref(),
        ));
        let closure_index = result
            .store
            .read_closure_index(result.closure)
            .map_err(operation)?;
        if closure_index.object_count() != u64::from(result.object_count) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let output_sink = result.store.artifact_sink(store_budget);
        let mut capabilities = Vec::new();
        let mut indexed_payload_bytes = 0_u64;
        let mut after = None;
        loop {
            let page = closure_index.page_ids(after, 128).map_err(operation)?;
            for id in page.object_ids() {
                let reader = output_sink
                    .open_object(UntrustedObjectId::from_bytes(*id.as_bytes()))
                    .map_err(operation)?
                    .ok_or(ClusterWorkerError::InvalidInputClosure)?;
                let mapping = StoreObjectMapping::from_artifact_reader(&reader);
                drop(reader);
                indexed_payload_bytes = indexed_payload_bytes
                    .checked_add(mapping.payload_length)
                    .ok_or(ClusterWorkerError::InputBounds)?;
                let timeout = self.operation_timeout(transfer_deadline_unix_ms)?;
                let materialized =
                    catalog.register_store_object(UntrustedObjectId::from_bytes(mapping.object_id));
                let materialized = tokio::time::timeout(timeout, materialized)
                    .await
                    .map_err(|_| ClusterWorkerError::Timeout)?
                    .map_err(operation)?
                    .ok_or(ClusterWorkerError::InvalidInputClosure)?;
                let total_chunks = if mapping.payload_length == 0 {
                    0
                } else {
                    mapping
                        .payload_length
                        .checked_add(BYTES_PER_BAO_CHUNK - 1)
                        .ok_or(ClusterWorkerError::InputBounds)?
                        / BYTES_PER_BAO_CHUNK
                };
                if total_chunks == 0 {
                    let range = ChunkRange { start: 0, end: 0 };
                    let capability = issue_worker_result_object_range(
                        &self.result_issuer,
                        result.scope,
                        result.coordinator,
                        self.endpoint.id(),
                        result.closure,
                        materialized,
                        range,
                        MAX_RESPONSE_BYTES,
                        transfer_deadline_unix_ms,
                        result_capability_nonce(result.scope, mapping.object_id, range),
                    )
                    .map_err(operation)?;
                    capabilities.push(capability);
                    if capabilities.len() > self.policy.max_capabilities {
                        return Err(ClusterWorkerError::InputBounds);
                    }
                }
                let mut start = 0_u64;
                while start < total_chunks {
                    let end = start
                        .saturating_add(backend_engine::cluster_transport::MAX_RANGE_CHUNKS)
                        .min(total_chunks);
                    let range = ChunkRange { start, end };
                    let capability = issue_worker_result_object_range(
                        &self.result_issuer,
                        result.scope,
                        result.coordinator,
                        self.endpoint.id(),
                        result.closure,
                        materialized,
                        range,
                        MAX_RESPONSE_BYTES,
                        transfer_deadline_unix_ms,
                        result_capability_nonce(result.scope, mapping.object_id, range),
                    )
                    .map_err(operation)?;
                    capabilities.push(capability);
                    if capabilities.len() > self.policy.max_capabilities {
                        return Err(ClusterWorkerError::InputBounds);
                    }
                    start = end;
                }
            }
            after = page.next();
            if after.is_none() {
                break;
            }
        }
        if indexed_payload_bytes != result.payload_bytes {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let page_count = capabilities.len().div_ceil(MAX_OFFER_CAPABILITIES);
        let page_count = u32::try_from(page_count).map_err(operation)?;
        if page_count == 0 || page_count > self.policy.max_grant_pages {
            return Err(ClusterWorkerError::InputBounds);
        }
        let pages = capabilities
            .chunks(MAX_OFFER_CAPABILITIES)
            .enumerate()
            .map(|(index, grants)| ControlGrantPage {
                scope: result.scope,
                direction: GrantDirection::ResultsToCoordinator,
                page_index: index as u32,
                page_count,
                grants: grants.to_vec(),
            })
            .collect::<Vec<_>>();
        let receipt = ControlResultReceipt {
            scope: result.scope,
            target_root: result.target_root,
            pack_id: result.pack_id,
            closure_id: *result.closure.as_bytes(),
            object_count: result.object_count,
            payload_bytes: result.payload_bytes,
            result_grant_pages: page_count,
        };
        let mut receipt_channel = tokio::time::timeout(
            self.policy.io_timeout,
            connect_control(
                &self.endpoint,
                coordinator.address.clone(),
                coordinator.identity,
                result.scope,
                ControlRole::Worker,
            ),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        tokio::time::timeout(
            self.operation_timeout(transfer_deadline_unix_ms)?,
            send_worker_result_receipt(&mut receipt_channel, receipt),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        self.finish_control_channel(receipt_channel, transfer_deadline_unix_ms)
            .await?;
        tokio::time::timeout(
            self.operation_timeout(transfer_deadline_unix_ms)?,
            send_worker_result_grant_pages(
                &self.endpoint,
                coordinator.address.clone(),
                coordinator.identity,
                result.scope,
                result.closure,
                &pages,
            ),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;

        let transfer_scope =
            TransferScope::from_store_closure(result.scope, result.closure).map_err(operation)?;
        let admission = AdmissionPolicy::new(
            self.endpoint.id(),
            self.endpoint.id(),
            [result.coordinator],
            transfer_scope,
        );
        let state = ServerState::new(admission, catalog);
        let control_policy = ControlAdmissionPolicy::new(
            self.endpoint.id(),
            [result.coordinator],
            result.scope,
            ControlRole::Worker,
        );
        let probe_policy = ProbeAdmissionPolicy::worker(self.endpoint.id(), [result.coordinator]);
        let mut listener = ClusterListener::spawn_with_probe(
            self.endpoint.clone(),
            control_policy,
            state.clone(),
            probe_policy.clone(),
            CLUSTER_LISTENER_CAPACITY,
        )
        .map_err(operation)?;
        let mut artifact_tasks = tokio::task::JoinSet::new();
        let mut range_latencies_ms = LatencyHistogram::default();
        let mut bao_stream_bytes = 0_u64;
        let transfer_result = 'transfer: loop {
            let timeout = match self.operation_timeout(transfer_deadline_unix_ms) {
                Ok(timeout) => timeout,
                Err(error) => break Err(error),
            };
            tokio::select! {
                () = tokio::time::sleep(timeout) => break Err(ClusterWorkerError::Timeout),
                event = listener.recv(), if artifact_tasks.len() < MAX_ACTIVE_ARTIFACT_SERVERS => {
                    match event {
                        None => break Err(ClusterWorkerError::UnexpectedControl),
                        Some(Err(_rejected_connection)) => continue,
                        Some(Ok(AcceptedClusterConnection::Artifact(connection))) => {
                            if connection.peer() != result.coordinator {
                                break 'transfer Err(ClusterWorkerError::PeerNotAllowed);
                            }
                            let state = state.clone();
                            artifact_tasks.spawn(async move {
                                let started = Instant::now();
                                let metrics = connection
                                    .serve(&state)
                                    .await
                                    .map_err(operation)?;
                                let elapsed_ms = u64::try_from(started.elapsed().as_millis())
                                    .unwrap_or(u64::MAX);
                                Ok::<_, ClusterWorkerError>((metrics, elapsed_ms))
                            });
                        }
                        Some(Ok(AcceptedClusterConnection::Control(mut channel))) => {
                            if channel.peer() != result.coordinator {
                                break 'transfer Err(ClusterWorkerError::PeerNotAllowed);
                            }
                            let message = match tokio::time::timeout(
                                self.operation_timeout(transfer_deadline_unix_ms)?,
                                channel.receive(),
                            )
                            .await
                            {
                                Ok(Ok(message)) => message,
                                Ok(Err(error)) => break 'transfer Err(operation(error)),
                                Err(_) => break 'transfer Err(ClusterWorkerError::Timeout),
                            };
                            match message {
                                ControlMessage::ResultAck(ack)
                                    if ack.scope == result.scope
                                        && ack.closure_id == *result.closure.as_bytes() => {
                                            let disposition = self
                                                .complete_result_acknowledgement(
                                                    channel,
                                                    ack,
                                                    result,
                                                    transfer_deadline_unix_ms,
                                                )
                                                .await?;
                                            break 'transfer Ok(disposition);
                                        }
                                ControlMessage::ResultRecoveryQuery(query)
                                    if query.scope == result.scope => {
                                        // The cold-replay path already owns the listener while
                                        // waiting for an owner. Answering a recovery query here
                                        // must leave the retained result and Bao service alive;
                                        // the helper returns Pending and re-advertises its exact
                                        // grant pages on a fresh control stream.
                                        if let Err(error) = self
                                            .answer_result_recovery_query(
                                                channel,
                                                query,
                                                &result.store,
                                                outboard_root.as_ref(),
                                                Some(result),
                                            )
                                            .await
                                        {
                                            if matches!(error, ClusterWorkerError::Timeout) {
                                                break 'transfer Err(error);
                                            }
                                        }
                                        continue 'transfer;
                                    }
                                ControlMessage::ResultRetirementConfirm(confirm)
                                    if confirm.scope == result.scope => {
                                        let disposition = confirm.disposition;
                                        self.apply_retirement_confirmation(
                                            channel,
                                            confirm,
                                            &result.store,
                                            true,
                                            self.operation_timeout(transfer_deadline_unix_ms)?,
                                        )
                                        .await?;
                                        break 'transfer Ok(disposition);
                                    }
                                ControlMessage::Cancel(cancel)
                                    if cancel.scope == result.scope => {
                                        self.finish_control_channel(channel, transfer_deadline_unix_ms)
                                            .await?;
                                        break 'transfer Err(ClusterWorkerError::Cancelled);
                                    }
                                _ => {
                                    let _ = self
                                        .finish_control_channel(channel, transfer_deadline_unix_ms)
                                        .await;
                                    break 'transfer Err(ClusterWorkerError::UnexpectedControl);
                                }
                            }
                        }
                        Some(Ok(AcceptedClusterConnection::Probe(connection))) => {
                            let channel = match tokio::time::timeout(
                                self.operation_timeout(transfer_deadline_unix_ms)?,
                                accept_probe_connection(connection, &probe_policy),
                            )
                            .await
                            {
                                Ok(Ok(channel)) => channel,
                                Ok(Err(_rejected_probe)) => continue,
                                Err(_) => break 'transfer Err(ClusterWorkerError::Timeout),
                            };
                            if let Err(error) = self
                                .serve_probe_channel(channel, None, &result.store)
                                .await
                            {
                                if matches!(error, ClusterWorkerError::Timeout) {
                                    break 'transfer Err(error);
                                }
                            }
                        }
                    }
                }
                Some(joined) = artifact_tasks.join_next(), if !artifact_tasks.is_empty() => {
                    if let Err(error) = record_artifact_task(
                        joined,
                        &mut bao_stream_bytes,
                        &mut range_latencies_ms,
                    ) {
                        break 'transfer Err(error);
                    }
                }
            }
        };
        let listener_shutdown = listener.shutdown().await.map_err(operation);
        if transfer_result.is_ok() {
            while !artifact_tasks.is_empty() {
                let drain_timeout = self.operation_timeout(transfer_deadline_unix_ms)?;
                let joined = tokio::time::timeout(drain_timeout, artifact_tasks.join_next())
                    .await
                    .map_err(|_| ClusterWorkerError::Timeout)?
                    .ok_or(ClusterWorkerError::UnexpectedControl)?;
                record_artifact_task(joined, &mut bao_stream_bytes, &mut range_latencies_ms)?;
            }
        } else {
            artifact_tasks.abort_all();
            while artifact_tasks.join_next().await.is_some() {}
        }
        listener_shutdown?;
        let disposition = transfer_result?;
        let diagnostics = WorkerTransferDiagnostics {
            artifact_ranges: range_latencies_ms.count,
            bao_stream_bytes,
            range_p50_ms: range_latencies_ms.percentile_upper_bound(50),
            range_p95_ms: range_latencies_ms.percentile_upper_bound(95),
            peak_rss_bytes: process_peak_rss_bytes(),
        };
        Ok((disposition, diagnostics))
    }

    async fn complete_result_acknowledgement(
        &self,
        mut channel: ControlChannel,
        ack: ControlResultAck,
        result: &ClusterWorkerResult,
        deadline_unix_ms: u64,
    ) -> Result<ResultAckDisposition, ClusterWorkerError> {
        if channel.peer() != result.coordinator
            || channel.scope() != Some(result.scope)
            || ack.scope != result.scope
            || ack.closure_id != *result.closure.as_bytes()
        {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        let record = RetiredResultRecord {
            coordinator: *result.coordinator.as_bytes(),
            scope: result.scope,
            closure_id: ack.closure_id,
            disposition: ack.disposition,
        };
        if read_pending_result_record(&result.store, result.scope)?
            != Some(PendingResultRecord::from_result(result))
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        validate_retired_result_sources(&result.store, record)?;
        persist_retired_result_record(&result.store, record)?;
        retire_result_payload_state(&result.store, record)?;
        let retired = record.receipt(self.endpoint.id())?;
        tokio::time::timeout(
            self.operation_timeout(deadline_unix_ms)?,
            channel.send(&ControlMessage::ResultRetired(retired)),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        let message =
            tokio::time::timeout(self.operation_timeout(deadline_unix_ms)?, channel.receive())
                .await
                .map_err(|_| ClusterWorkerError::Timeout)?
                .map_err(operation)?;
        let ControlMessage::ResultRetirementConfirm(confirm) = message else {
            return Err(ClusterWorkerError::UnexpectedControl);
        };
        if !record.matches_confirm(&confirm, channel.peer())
            || !confirm.matches_retired(&retired, channel.peer())
        {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        apply_retired_result_record(&result.store, record)?;
        let applied = ControlResultRetirementApplied::new(
            record.scope,
            record.closure_id,
            record.disposition,
            self.endpoint.id(),
        )
        .map_err(operation)?;
        tokio::time::timeout(
            self.operation_timeout(deadline_unix_ms)?,
            channel.send(&ControlMessage::ResultRetirementApplied(applied)),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        self.finish_control_channel(channel, deadline_unix_ms)
            .await?;
        Ok(record.disposition)
    }

    async fn resume_retired_acknowledgement(
        &self,
        mut channel: ControlChannel,
        ack: ControlResultAck,
        store: &FileStore,
        timeout: Duration,
    ) -> Result<(), ClusterWorkerError> {
        let coordinator = channel.peer();
        if !self.coordinators.contains_key(&coordinator)
            || channel.scope() != Some(ack.scope)
            || ack.scope.namespace_id != self.policy.namespace_id
        {
            return Err(ClusterWorkerError::PeerNotAllowed);
        }
        let record = read_retired_result_record(store, ack.scope)?
            .ok_or(ClusterWorkerError::ScopeMismatch)?;
        if record.coordinator != *coordinator.as_bytes() || !record.matches_ack(&ack) {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        validate_retired_result_sources(store, record)?;
        retire_result_payload_state(store, record)?;
        let retired = record.receipt(self.endpoint.id())?;
        tokio::time::timeout(
            timeout,
            channel.send(&ControlMessage::ResultRetired(retired)),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        let message = tokio::time::timeout(timeout, channel.receive())
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)?;
        let ControlMessage::ResultRetirementConfirm(confirm) = message else {
            return Err(ClusterWorkerError::UnexpectedControl);
        };
        if !record.matches_confirm(&confirm, coordinator)
            || !confirm.matches_retired(&retired, coordinator)
        {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        self.apply_retirement_confirmation(channel, confirm, store, false, timeout)
            .await
    }

    async fn apply_retirement_confirmation(
        &self,
        mut channel: ControlChannel,
        confirm: ControlResultRetirementConfirm,
        store: &FileStore,
        allow_no_state: bool,
        timeout: Duration,
    ) -> Result<(), ClusterWorkerError> {
        let coordinator = channel.peer();
        if !self.coordinators.contains_key(&coordinator)
            || confirm.scope.namespace_id != self.policy.namespace_id
            || channel.scope() != Some(confirm.scope)
            || confirm.coordinator_endpoint_id != *coordinator.as_bytes()
        {
            return Err(ClusterWorkerError::PeerNotAllowed);
        }
        let record = read_retired_result_record(store, confirm.scope)?;
        if let Some(record) = record {
            let retired = record.receipt(self.endpoint.id())?;
            if !record.matches_confirm(&confirm, coordinator)
                || !confirm.matches_retired(&retired, coordinator)
            {
                return Err(ClusterWorkerError::ScopeMismatch);
            }
            validate_retired_result_sources(store, record)?;
            apply_retired_result_record(store, record)?;
        } else {
            if !allow_no_state
                || !has_no_live_retirement_conflict(store, &self.policy, confirm.scope)?
            {
                return Err(ClusterWorkerError::ScopeMismatch);
            }
            // This is the only no-state success: a trusted coordinator repeats the exact
            // confirmation after its AwaitingConfirm row survived while the worker's Applied
            // packet was lost. No local pending result or conflicting same-scope record exists.
        }
        let applied = ControlResultRetirementApplied::new(
            confirm.scope,
            confirm.closure_id,
            confirm.disposition,
            self.endpoint.id(),
        )
        .map_err(operation)?;
        tokio::time::timeout(
            timeout,
            channel.send(&ControlMessage::ResultRetirementApplied(applied)),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        self.finish_control_channel_with_timeout(channel, timeout)
            .await
    }

    async fn apply_no_result_retirement(
        &self,
        mut channel: ControlChannel,
        request: ControlNoResultRetireThrough,
        store: &FileStore,
        timeout: Duration,
    ) -> Result<(), ClusterWorkerError> {
        let coordinator = channel.peer();
        if channel.scope() != Some(request.scope)
            || request.scope.namespace_id != self.policy.namespace_id
            || !request.matches_coordinator(coordinator)
            || !self.coordinators.contains_key(&coordinator)
        {
            return Err(ClusterWorkerError::PeerNotAllowed);
        }
        let authorized = self.coordinators.keys().copied().collect::<Vec<_>>();
        apply_no_result_retirement_floor(store, &self.policy, &authorized, coordinator, request)?;
        let applied = ControlNoResultRetirementApplied::new(&request, self.endpoint.id())
            .map_err(operation)?;
        tokio::time::timeout(
            timeout,
            channel.send(&ControlMessage::NoResultRetirementApplied(applied)),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        self.finish_control_channel_with_timeout(channel, timeout)
            .await
    }

    async fn answer_result_recovery_query(
        &self,
        mut channel: ControlChannel,
        query: ControlResultRecoveryQuery,
        store: &FileStore,
        outboard_root: &Path,
        known_result: Option<&ClusterWorkerResult>,
    ) -> Result<(), ClusterWorkerError> {
        let coordinator = channel.peer();
        let scope = query.scope;
        if !self.coordinators.contains_key(&coordinator)
            || channel.scope() != Some(scope)
            || scope.namespace_id != self.policy.namespace_id
            || !query.is_valid_from(coordinator)
        {
            return Err(ClusterWorkerError::PeerNotAllowed);
        }

        let retired = read_retired_result_record(store, scope)?;
        let pending = read_pending_result_record(store, scope)?;
        let running = read_running_assignment_record(store, scope)?;
        let no_result = read_no_result_record(store, scope)?;
        let floor_covers = no_result_floor_blocks(store, coordinator, scope)?;
        let active = self
            .resources
            .lock()
            .map_err(|_| ClusterWorkerError::Operation("worker resource lock poisoned".into()))?
            .active_assignment();
        if no_result.is_some() && (retired.is_some() || pending.is_some() || running.is_some()) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }

        let (state, replay) = if let Some(record) = retired {
            if record.coordinator != *coordinator.as_bytes() {
                return Err(ClusterWorkerError::PeerNotAllowed);
            }
            validate_retired_result_sources(store, record)?;
            (
                ControlResultRecoveryState::Retired(record.receipt(self.endpoint.id())?),
                None,
            )
        } else if let Some(record) = pending {
            if record.coordinator != *coordinator.as_bytes() {
                return Err(ClusterWorkerError::PeerNotAllowed);
            }
            if let Some(running) = running
                && (running.coordinator != record.coordinator
                    || running.input_closure_id != record.input_closure_id
                    || running.input_manifest_object_id != record.input_manifest_object_id)
            {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            let result = if let Some(result) = known_result.filter(|result| result.scope == scope) {
                if PendingResultRecord::from_result(result) != record
                    || result.coordinator != coordinator
                    || result.receipt.closure() != result.closure
                {
                    return Err(ClusterWorkerError::InvalidInputClosure);
                }
                result.clone()
            } else {
                reopen_pending_result(store, &self.policy, record)?
            };
            let transfer_deadline = now_unix_ms()
                .map_err(operation)?
                .checked_add(MAX_CONTROL_OFFER_LIFETIME_MS)
                .ok_or(ClusterWorkerError::InputBounds)?;
            let pages = self
                .build_result_grant_pages(&result, outboard_root, transfer_deadline)
                .await?;
            let page_count = u32::try_from(pages.len()).map_err(operation)?;
            let receipt = record.receipt(page_count);
            (
                ControlResultRecoveryState::Pending(receipt),
                Some((result, pages, transfer_deadline)),
            )
        } else if let Some(record) = no_result {
            if !record.matches_query(&query, coordinator) {
                return Err(ClusterWorkerError::PeerNotAllowed);
            }
            (ControlResultRecoveryState::NoResult, None)
        } else if let Some(active) = active.filter(|active| active.scope == scope) {
            if active.coordinator != coordinator {
                return Err(ClusterWorkerError::PeerNotAllowed);
            }
            (ControlResultRecoveryState::Running, None)
        } else if floor_covers {
            // The coordinator has durably retired this namespace epoch prefix. Recreate no exact
            // row after compaction; the prefix floor remains the durable evidence for late Offers.
            (ControlResultRecoveryState::NoResult, None)
        } else {
            if let Some(running) = running {
                if running.coordinator != *coordinator.as_bytes() {
                    return Err(ClusterWorkerError::PeerNotAllowed);
                }
                remove_running_assignment(store, scope)?;
            }
            let record = NoResultRecord::new(coordinator, scope);
            persist_no_result_record(store, &self.policy, record)?;
            (ControlResultRecoveryState::NoResult, None)
        };

        let status = ControlResultRecoveryStatus::new(
            scope,
            state,
            self.recovery_capacity()?,
            self.endpoint.id(),
        )
        .map_err(operation)?;
        tokio::time::timeout(
            self.policy.io_timeout,
            channel.send(&ControlMessage::ResultRecoveryStatus(status)),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        self.finish_control_channel_with_timeout(channel, self.policy.io_timeout)
            .await?;

        if let Some((result, pages, deadline)) = replay {
            let coordinator = self
                .coordinators
                .get(&result.coordinator)
                .ok_or(ClusterWorkerError::PeerNotAllowed)?;
            tokio::time::timeout(
                self.operation_timeout(deadline)?,
                send_worker_result_grant_pages(
                    &self.endpoint,
                    coordinator.address.clone(),
                    coordinator.identity,
                    result.scope,
                    result.closure,
                    &pages,
                ),
            )
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)?;
        }
        Ok(())
    }

    async fn respond_running_recovery_query(
        &self,
        mut channel: ControlChannel,
        query: ControlResultRecoveryQuery,
        expected: &AdmittedOffer,
    ) -> Result<(), ClusterWorkerError> {
        let coordinator = channel.peer();
        let active = self
            .resources
            .lock()
            .map_err(|_| ClusterWorkerError::Operation("worker resource lock poisoned".into()))?
            .active_assignment();
        if !self.coordinators.contains_key(&coordinator)
            || coordinator != expected.coordinator
            || channel.scope() != Some(expected.offer.scope)
            || query.scope != expected.offer.scope
            || !query.is_valid_from(coordinator)
            || active
                != Some(ActiveAssignment {
                    coordinator,
                    scope: expected.offer.scope,
                })
        {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        let status = ControlResultRecoveryStatus::new(
            query.scope,
            ControlResultRecoveryState::Running,
            self.recovery_capacity()?,
            self.endpoint.id(),
        )
        .map_err(operation)?;
        tokio::time::timeout(
            self.policy.io_timeout,
            channel.send(&ControlMessage::ResultRecoveryStatus(status)),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)?;
        self.finish_control_channel_with_timeout(channel, self.policy.io_timeout)
            .await
    }

    async fn build_result_grant_pages(
        &self,
        result: &ClusterWorkerResult,
        outboard_root: &Path,
        deadline_unix_ms: u64,
    ) -> Result<Vec<ControlGrantPage>, ClusterWorkerError> {
        let store_budget = ArtifactBudget::new(
            self.policy.max_input_objects,
            self.policy.max_input_objects,
            self.policy.max_input_bytes.max(result.payload_bytes),
            ARTIFACT_PUT_CHUNK_BYTES,
            put_call_budget(
                self.policy.max_input_bytes.max(result.payload_bytes),
                self.policy.max_input_objects,
            )?,
        );
        let catalog =
            StoreBlobCatalog::new(result.store.artifact_sink(store_budget), outboard_root);
        let closure_index = result
            .store
            .read_closure_index(result.closure)
            .map_err(operation)?;
        if closure_index.object_count() != u64::from(result.object_count) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let output_sink = result.store.artifact_sink(store_budget);
        let mut capabilities = Vec::new();
        let mut indexed_payload_bytes = 0_u64;
        let mut after = None;
        loop {
            let page = closure_index.page_ids(after, 128).map_err(operation)?;
            for id in page.object_ids() {
                let reader = output_sink
                    .open_object(UntrustedObjectId::from_bytes(*id.as_bytes()))
                    .map_err(operation)?
                    .ok_or(ClusterWorkerError::InvalidInputClosure)?;
                let mapping = StoreObjectMapping::from_artifact_reader(&reader);
                drop(reader);
                indexed_payload_bytes = indexed_payload_bytes
                    .checked_add(mapping.payload_length)
                    .ok_or(ClusterWorkerError::InputBounds)?;
                let timeout = self.operation_timeout(deadline_unix_ms)?;
                let materialized = tokio::time::timeout(
                    timeout,
                    catalog.register_store_object(UntrustedObjectId::from_bytes(mapping.object_id)),
                )
                .await
                .map_err(|_| ClusterWorkerError::Timeout)?
                .map_err(operation)?
                .ok_or(ClusterWorkerError::InvalidInputClosure)?;
                let total_chunks = if mapping.payload_length == 0 {
                    0
                } else {
                    mapping
                        .payload_length
                        .checked_add(BYTES_PER_BAO_CHUNK - 1)
                        .ok_or(ClusterWorkerError::InputBounds)?
                        / BYTES_PER_BAO_CHUNK
                };
                let mut ranges = Vec::new();
                if total_chunks == 0 {
                    ranges.push(ChunkRange { start: 0, end: 0 });
                } else {
                    let mut start = 0_u64;
                    while start < total_chunks {
                        let end = start
                            .saturating_add(backend_engine::cluster_transport::MAX_RANGE_CHUNKS)
                            .min(total_chunks);
                        ranges.push(ChunkRange { start, end });
                        start = end;
                    }
                }
                for range in ranges {
                    let capability = issue_worker_result_object_range(
                        &self.result_issuer,
                        result.scope,
                        result.coordinator,
                        self.endpoint.id(),
                        result.closure,
                        materialized,
                        range,
                        MAX_RESPONSE_BYTES,
                        deadline_unix_ms,
                        result_capability_nonce(result.scope, mapping.object_id, range),
                    )
                    .map_err(operation)?;
                    capabilities.push(capability);
                    if capabilities.len() > self.policy.max_capabilities {
                        return Err(ClusterWorkerError::InputBounds);
                    }
                }
            }
            after = page.next();
            if after.is_none() {
                break;
            }
        }
        if indexed_payload_bytes != result.payload_bytes {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let page_count = u32::try_from(capabilities.len().div_ceil(MAX_OFFER_CAPABILITIES))
            .map_err(operation)?;
        if page_count == 0 || page_count > self.policy.max_grant_pages {
            return Err(ClusterWorkerError::InputBounds);
        }
        Ok(capabilities
            .chunks(MAX_OFFER_CAPABILITIES)
            .enumerate()
            .map(|(index, grants)| ControlGrantPage {
                scope: result.scope,
                direction: GrantDirection::ResultsToCoordinator,
                page_index: index as u32,
                page_count,
                grants: grants.to_vec(),
            })
            .collect())
    }

    async fn wait_for_cancel(&self, assignment: &AdmittedOffer) -> Result<(), ClusterWorkerError> {
        let admission = ControlAdmissionPolicy::new(
            self.endpoint.id(),
            [assignment.coordinator],
            assignment.offer.scope,
            ControlRole::Worker,
        );
        loop {
            let timeout = self.operation_timeout(assignment.offer.deadline_unix_ms)?;
            let mut channel =
                tokio::time::timeout(timeout, accept_control(&self.endpoint, &admission))
                    .await
                    .map_err(|_| ClusterWorkerError::Timeout)?
                    .map_err(operation)?;
            if channel.peer() != assignment.coordinator {
                return Err(ClusterWorkerError::PeerNotAllowed);
            }
            let timeout = self.operation_timeout(assignment.offer.deadline_unix_ms)?;
            let message = tokio::time::timeout(timeout, channel.receive())
                .await
                .map_err(|_| ClusterWorkerError::Timeout)?
                .map_err(operation)?;
            match message {
                ControlMessage::Cancel(cancel) if cancel.scope == assignment.offer.scope => {
                    self.finish_control_channel(channel, assignment.offer.deadline_unix_ms)
                        .await?;
                    return Err(ClusterWorkerError::Cancelled);
                }
                ControlMessage::ResultRecoveryQuery(query) => {
                    self.respond_running_recovery_query(channel, query, assignment)
                        .await?;
                }
                _ => return Err(ClusterWorkerError::UnexpectedControl),
            }
        }
    }

    fn operation_timeout(&self, deadline_unix_ms: u64) -> Result<Duration, ClusterWorkerError> {
        let now = now_unix_ms().map_err(operation)?;
        let remaining = deadline_unix_ms.saturating_sub(now);
        if remaining == 0 {
            return Err(ClusterWorkerError::Timeout);
        }
        Ok(self.policy.io_timeout.min(Duration::from_millis(remaining)))
    }

    async fn finish_control_channel(
        &self,
        channel: ControlChannel,
        deadline_unix_ms: u64,
    ) -> Result<(), ClusterWorkerError> {
        self.finish_control_channel_with_timeout(channel, self.operation_timeout(deadline_unix_ms)?)
            .await
    }

    async fn finish_control_channel_with_timeout(
        &self,
        channel: ControlChannel,
        timeout: Duration,
    ) -> Result<(), ClusterWorkerError> {
        tokio::time::timeout(timeout, channel.finish())
            .await
            .map_err(|_| ClusterWorkerError::Timeout)?
            .map_err(operation)
    }

    async fn send_execution_failure(
        &self,
        assignment: &AdmittedOffer,
        reason: ExecutionFailureReason,
    ) -> Result<(), ClusterWorkerError> {
        let coordinator = self
            .coordinators
            .get(&assignment.coordinator)
            .ok_or(ClusterWorkerError::PeerNotAllowed)?;
        tokio::time::timeout(
            self.policy.io_timeout,
            send_worker_execution_failure(
                &self.endpoint,
                coordinator.address.clone(),
                coordinator.identity,
                assignment.offer.scope,
                assignment.offer.input_closure_id,
                reason,
            ),
        )
        .await
        .map_err(|_| ClusterWorkerError::Timeout)?
        .map_err(operation)
    }

    async fn send_execution_failure_and_clear(
        &self,
        assignment: &AdmittedOffer,
        reason: ExecutionFailureReason,
        store: &FileStore,
    ) -> Result<(), ClusterWorkerError> {
        self.send_execution_failure(assignment, reason).await?;
        remove_running_assignment(store, assignment.offer.scope)
    }
}

/// Lists retained results after reopening and validating their exact typed closure envelopes.
pub fn inspect_pending_results(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
) -> Result<Vec<PendingResultSummary>, ClusterWorkerError> {
    let results = recover_pending_results(store, policy)?;
    Ok(results
        .into_iter()
        .map(|result| PendingResultSummary {
            scope: result.scope,
            coordinator: result.coordinator,
            input_closure_id: result.input_closure_id,
            input_manifest_object_id: result.input_manifest_object_id,
            result_closure_id: *result.closure.as_bytes(),
            payload_bytes: result.payload_bytes,
            object_count: result.object_count,
        })
        .collect())
}

/// Collects stale input and result objects while retaining every closure named by the durable
/// running and pending journals. The command loop calls this only between assignments, before
/// accepting another Offer, so retained ACK-governed results stay available across restarts.
pub fn collect_worker_store_garbage(
    input_store: &FileStore,
    result_store: &FileStore,
    policy: &ClusterWorkerPolicy,
) -> Result<(), ClusterWorkerError> {
    let mut input_roots = GcRoots::new();
    let mut result_roots = GcRoots::new();
    for record in read_pending_result_records(result_store, policy)? {
        // GC needs a checked durable closure root, not the semantic envelope
        // payload. Result replay performs the stricter envelope check before
        // serving anything to a coordinator. Keeping these steps separate
        // lets mark-and-sweep remain metadata-bounded for large results.
        let receipt = reopen_pending_closure(result_store, policy, record)?;
        result_roots.add(GcRoot::Closure(receipt.closure()));
    }
    let directory = result_journal_directory(result_store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            input_store
                .collect_garbage(&input_roots, GcLimits::default())
                .map_err(operation)?;
            result_store
                .collect_garbage(&result_roots, GcLimits::default())
                .map_err(operation)?;
            return Ok(());
        }
        Err(error) => return Err(operation(error)),
    };
    for entry in entries {
        let path = entry.map_err(operation)?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("running") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(operation)?;
        validate_journal_metadata(&metadata, RUNNING_JOURNAL_BYTES)?;
        let mut file = open_private_read_file(&path, RUNNING_JOURNAL_BYTES)?;
        let mut bytes = Vec::with_capacity(RUNNING_JOURNAL_BYTES);
        file.by_ref()
            .take((RUNNING_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(operation)?;
        let record = RunningAssignmentRecord::decode(&bytes)?;
        if result_journal_path(&directory, record.scope, "running") != path
            || record.scope.namespace_id != policy.namespace_id
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let input_budget = ArtifactBudget::new(
            policy.max_input_objects,
            policy.max_input_objects,
            policy.max_input_bytes,
            ARTIFACT_PUT_CHUNK_BYTES,
            put_call_budget(policy.max_input_bytes, policy.max_input_objects)?,
        );
        let input_receipt = input_store
            .reopen_stored_closure(
                ArtifactClosureClaim::from_bytes(record.input_closure_id),
                input_budget,
            )
            .map_err(operation)?;
        input_roots.add(GcRoot::Closure(input_receipt.closure()));
    }
    input_store
        .collect_garbage(&input_roots, GcLimits::default())
        .map_err(operation)?;
    result_store
        .collect_garbage(&result_roots, GcLimits::default())
        .map_err(operation)?;
    Ok(())
}

/// Explicitly abandons one exact retained transfer under local store-owner authority.
///
/// The result CAS closure is preserved for normal store GC. This removes only the replay
/// obligation, after an owner-only audit record has recorded the requested work/fence/closure.
pub fn abandon_pending_result(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    scope: AssignmentScope,
    expected_result_closure: [u8; 32],
) -> Result<(), ClusterWorkerError> {
    let results = recover_pending_results(store, policy)?;
    let result = results
        .iter()
        .find(|result| {
            result.scope == scope && result.closure.as_bytes() == &expected_result_closure
        })
        .ok_or(ClusterWorkerError::PendingResultNotFound)?;
    append_abandon_audit(store, result, "REQUEST")?;
    remove_running_assignment(store, scope)?;
    remove_pending_result(result)?;
    append_abandon_audit(store, result, "COMPLETED")?;
    Ok(())
}

fn compiler_failure_reason(error: &PackageSemanticRuntimeError) -> ExecutionFailureReason {
    match error {
        PackageSemanticRuntimeError::Admission(_) => ExecutionFailureReason::CompilerRejected,
        PackageSemanticRuntimeError::Package(PackageSemanticError::Capacity { .. }) => {
            ExecutionFailureReason::OutputBudget
        }
        PackageSemanticRuntimeError::Runtime(_) | PackageSemanticRuntimeError::Package(_) => {
            ExecutionFailureReason::CompilerFailed
        }
    }
}

fn record_artifact_task(
    joined: Result<Result<(ServeMetrics, u64), ClusterWorkerError>, tokio::task::JoinError>,
    total_bytes: &mut u64,
    range_latencies_ms: &mut LatencyHistogram,
) -> Result<(), ClusterWorkerError> {
    let (metrics, latency_ms) = joined.map_err(operation)??;
    *total_bytes = total_bytes
        .checked_add(metrics.bao_stream_bytes)
        .ok_or(ClusterWorkerError::InputBounds)?;
    range_latencies_ms.record(latency_ms);
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_peak_rss_bytes() -> Option<u64> {
    let status = fs::read_to_string("/proc/self/status").ok()?;
    let kilobytes = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))?
        .split_whitespace()
        .next()?
        .parse::<u64>()?;
    kilobytes.checked_mul(1024)
}

#[cfg(not(target_os = "linux"))]
fn process_peak_rss_bytes() -> Option<u64> {
    None
}

fn verify_input_closure_members(
    store: &FileStore,
    closure_id: ClosureId,
    manifest_object_id: [u8; 32],
    manifest: &CompilerInputManifestV2,
) -> Result<CompilerInputMerkleTreeV2, ClusterWorkerError> {
    let verified = verify_full_workspace_closure_v2(
        store,
        closure_id,
        store
            .verify_object_claim(UntrustedObjectId::from_bytes(manifest_object_id))
            .map_err(operation)?
            .id(),
        None,
    )
    .map_err(operation)?;
    if verified.manifest() != manifest
        || verified.closure() != closure_id
        || verified.manifest_object_id().as_bytes() != &manifest_object_id
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(verified.workspace_tree().clone())
}

fn validate_manifest_for_worker(
    manifest: &CompilerInputManifestV2,
    workspace_tree: &CompilerInputMerkleTreeV2,
    policy: &ClusterWorkerPolicy,
    coordinator: EndpointId,
) -> Result<WorkerExecutionGrant, WorkerRejectReason> {
    let package_target = manifest.package_target();
    let target = *package_target.target().as_ref();
    let mut source_paths = std::collections::BTreeSet::new();
    let mut file_paths = std::collections::BTreeSet::new();
    if workspace_tree.kind() != CompilerInputTreeKindV2::Workspace
        || workspace_tree.root() != manifest.workspace_root()
        || manifest.max_output_bytes() > policy.max_output_bytes
        || !policy.accepted_recipes.contains(&manifest.recipe())
    {
        return Err(WorkerRejectReason::ManifestMismatch);
    }
    let profile = <[u8; 2]>::from(manifest.profile());
    let stage = u8::from(manifest.stage());
    let mut portable_names = std::collections::BTreeSet::new();
    for page in workspace_tree.pages() {
        let record = page.record();
        let path = record.path();
        if path.is_empty() {
            continue;
        }
        validate_compiler_input_path(path).map_err(|_| WorkerRejectReason::ManifestMismatch)?;
        if !portable_names.insert(path.to_ascii_lowercase()) {
            return Err(WorkerRejectReason::ManifestMismatch);
        }
        if let CompilerInputTreeRecordV2::File { path, role, .. } = record {
            file_paths.insert(path.to_string());
            if *role == CompilerWorkspaceFileRoleV2::Source {
                source_paths.insert(path.to_string());
            }
        }
    }
    let exact_unit_present = match package_target.unit_key() {
        CompilationUnitKeyV2::PackageRoot => package_target.package().identity.as_ref() == &target,
        CompilationUnitKeyV2::RustCrate { root, .. } => source_paths.contains(root.as_ref()),
        CompilationUnitKeyV2::CSharpProject { project_path } => {
            file_paths.contains(project_path.as_ref())
        }
        CompilationUnitKeyV2::TypeScriptProgram { .. }
        | CompilationUnitKeyV2::PythonModule { .. }
        | CompilationUnitKeyV2::GoPackage { .. }
        | CompilationUnitKeyV2::JavaModule { .. }
        | CompilationUnitKeyV2::ClangTranslationUnit { .. } => false,
    };
    if !exact_unit_present || source_paths.is_empty() {
        return Err(WorkerRejectReason::ManifestMismatch);
    }
    let ClusterExecutionPolicy::Allowlisted(grants) = &policy.execution_policy else {
        return Err(WorkerRejectReason::SandboxUnavailable);
    };
    let grant = grants
        .iter()
        .find(|grant| {
            grant.coordinator == coordinator
                && grant.namespace_id == policy.namespace_id
                && grant.recipe == manifest.recipe()
                && grant.profile == profile
                && grant.stage == stage
                && grant.toolchain == manifest.toolchain()
                && grant.environment == manifest.environment()
                && grant.target_platform == manifest.target_platform()
        })
        .ok_or(WorkerRejectReason::UnsupportedToolchain)?;
    if grant.class == WorkerExecutionClass::PureInProcessParser {
        return Err(WorkerRejectReason::SandboxUnavailable);
    }
    Ok(*grant)
}

fn compiler_execution_identity_matches(
    identity: &LocalCompilerExecutionIdentity,
    manifest: &CompilerInputManifestV2,
    grant: WorkerExecutionGrant,
) -> bool {
    if identity.invocation_recipe() != manifest.invocation_recipe() {
        return false;
    }
    let facts = CompilerExecutionFacts {
        target: *identity.target().as_ref(),
        profile: <[u8; 2]>::from(identity.profile()),
        stage: u8::from(identity.stage()),
        recipe: *identity.recipe_identity().as_ref(),
        toolchain: identity.toolchain_identity(),
        local_authority_fingerprint: identity.local_authority_fingerprint(),
        environment: identity.environment_identity(),
        target_platform: identity.target_platform_identity(),
    };
    compiler_execution_facts_match(
        facts,
        *manifest.package_target().target().as_ref(),
        <[u8; 2]>::from(manifest.profile()),
        u8::from(manifest.stage()),
        manifest.recipe(),
        manifest.toolchain(),
        manifest.environment(),
        manifest.target_platform(),
        grant,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompilerExecutionFacts {
    target: [u8; 32],
    profile: [u8; 2],
    stage: u8,
    recipe: [u8; 32],
    toolchain: [u8; 32],
    local_authority_fingerprint: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
}

fn compiler_execution_facts_match(
    actual: CompilerExecutionFacts,
    manifest_target: [u8; 32],
    manifest_profile: [u8; 2],
    manifest_stage: u8,
    manifest_recipe: [u8; 32],
    manifest_toolchain: [u8; 32],
    manifest_environment: [u8; 32],
    manifest_target_platform: [u8; 32],
    grant: WorkerExecutionGrant,
) -> bool {
    actual.target == manifest_target
        && actual.profile == grant.profile
        && actual.profile == manifest_profile
        && actual.stage == grant.stage
        && actual.stage == manifest_stage
        && actual.recipe == manifest_recipe
        && actual.recipe == grant.recipe
        && actual.toolchain == manifest_toolchain
        && actual.toolchain == grant.toolchain
        && actual.local_authority_fingerprint == grant.local_authority_fingerprint
        && actual.local_authority_fingerprint != [0; 32]
        && actual.environment == manifest_environment
        && actual.environment == grant.environment
        && actual.target_platform == manifest_target_platform
        && actual.target_platform == grant.target_platform
}

fn store_staged_result(
    staged: &StagedSemanticPackage,
    offer: &ControlOffer,
    coordinator: EndpointId,
    store: &FileStore,
    max_output_bytes: u64,
    max_output_objects: usize,
) -> Result<ClusterWorkerResult, ClusterWorkerError> {
    let output_count = staged.output_object_count();
    if output_count == 0 {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    let versioned_planes = staged.versioned_planes().map_err(operation)?;
    let plane_output_count = versioned_planes
        .artifacts()
        .iter()
        .try_fold(0_usize, |total, artifact| {
            total
                .checked_add(1)
                .and_then(|count| count.checked_add(artifact.segment_count()))
        })
        .ok_or(ClusterWorkerError::InputBounds)?;
    let object_limit = output_count
        .checked_add(plane_output_count)
        .and_then(|count| count.checked_add(1))
        .ok_or(ClusterWorkerError::InputBounds)?;
    if object_limit > max_output_objects {
        return Err(ClusterWorkerError::InputBounds);
    }
    let max_object_bytes =
        usize::try_from(max_output_bytes).map_err(|_| ClusterWorkerError::InputBounds)?;
    let max_metadata_bytes =
        StreamingClosureBudget::metadata_input_bytes_for(max_output_objects).map_err(operation)?;
    let mut builder = store
        .begin_streaming_closure(StreamingClosureBudget::new(
            max_output_objects,
            max_output_bytes,
            max_object_bytes,
            ARTIFACT_PUT_CHUNK_BYTES,
            put_call_budget(max_output_bytes, max_output_objects)?,
            max_metadata_bytes,
        ))
        .map_err(operation)?;
    let mut members = Vec::new();
    members
        .try_reserve_exact(output_count)
        .map_err(|_| ClusterWorkerError::InputBounds)?;
    let mut versioned_plane_members = Vec::new();
    versioned_plane_members
        .try_reserve_exact(plane_output_count)
        .map_err(|_| ClusterWorkerError::InputBounds)?;
    let mut payload_bytes = 0_u64;
    for ordinal in 0..output_count {
        let output = staged
            .output_object(ordinal)
            .ok_or(ClusterWorkerError::InvalidInputClosure)?;
        let bytes = output.bytes();
        let claim = compiler_result_output_claim(output.claim(), bytes).map_err(operation)?;
        let object_id = stream_typed_output(
            &mut builder,
            claim.artifact_claim(),
            bytes,
            &mut payload_bytes,
            max_output_bytes,
        )?;
        members.push(claim.member(object_id));
    }
    for artifact in versioned_planes.artifacts() {
        let claims =
            compiler_result_versioned_plane_output_claims(staged, artifact).map_err(operation)?;
        if claims.len() != artifact.segment_count().saturating_add(1) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        for (claim_index, claim) in claims.iter().enumerate() {
            let bytes = if claim_index == 0 {
                artifact.manifest_bytes()
            } else {
                artifact
                    .segment(claim_index - 1)
                    .ok_or(ClusterWorkerError::InvalidInputClosure)?
                    .payload()
            };
            let object_id = stream_typed_output(
                &mut builder,
                claim.artifact_claim(),
                bytes,
                &mut payload_bytes,
                max_output_bytes,
            )?;
            versioned_plane_members.push(claim.member(object_id));
        }
    }
    let result_object = compiler_result_envelope_object_from_members_with_versioned_planes(
        offer.scope,
        offer.input_closure_id,
        offer.input_manifest_object_id,
        staged,
        &members,
        &versioned_plane_members,
    )
    .map_err(operation)?;
    let envelope_claim = compiler_result_typed_object_claim(&result_object).map_err(operation)?;
    stream_typed_output(
        &mut builder,
        envelope_claim,
        result_object.bytes(),
        &mut payload_bytes,
        max_output_bytes,
    )?;
    let generation = staged.generation_facts();
    let receipt = builder.seal().map_err(operation)?;
    let object_count = u32::try_from(receipt.object_count()).map_err(operation)?;
    let closure = receipt.closure();
    if receipt.payload_bytes() != payload_bytes
        || receipt.object_count() != u64::from(object_count)
        || receipt.object_count() != u64::try_from(object_limit).map_err(operation)?
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    let result = ClusterWorkerResult {
        scope: offer.scope,
        coordinator,
        input_closure_id: offer.input_closure_id,
        input_manifest_object_id: offer.input_manifest_object_id,
        deadline_unix_ms: now_unix_ms()
            .map_err(operation)?
            .checked_add(MAX_CONTROL_OFFER_LIFETIME_MS)
            .ok_or(ClusterWorkerError::InputBounds)?,
        receipt,
        closure,
        pack_id: None,
        target_root: *generation.pinned_root.as_ref(),
        payload_bytes,
        object_count,
        store: store.clone(),
    };
    persist_pending_result(&result)?;
    Ok(result)
}

fn stream_typed_output(
    builder: &mut StreamingClosureBuilder,
    claim: ArtifactObjectClaim,
    bytes: &[u8],
    total_payload_bytes: &mut u64,
    max_payload_bytes: u64,
) -> Result<ObjectId, ClusterWorkerError> {
    if u64::try_from(bytes.len()).ok() != Some(claim.length()) {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    let next_total = total_payload_bytes
        .checked_add(claim.length())
        .ok_or(ClusterWorkerError::InputBounds)?;
    if next_total > max_payload_bytes {
        return Err(ClusterWorkerError::InputBounds);
    }
    let mut stream = builder.begin_object(claim).map_err(operation)?;
    for chunk in bytes.chunks(ARTIFACT_PUT_CHUNK_BYTES) {
        stream.write(chunk).map_err(operation)?;
    }
    let object_id = stream.finish().map_err(operation)?;
    *total_payload_bytes = next_total;
    Ok(object_id)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingResultRecord {
    coordinator: [u8; 32],
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    deadline_unix_ms: u64,
    closure_id: [u8; 32],
    pack_id: Option<[u8; 32]>,
    target_root: [u8; 32],
    payload_bytes: u64,
    object_count: u32,
}

#[derive(Clone, Copy, Debug)]
struct RunningAssignmentRecord {
    coordinator: [u8; 32],
    scope: AssignmentScope,
    input_closure_id: [u8; 32],
    input_manifest_object_id: [u8; 32],
    deadline_unix_ms: u64,
}

impl RunningAssignmentRecord {
    fn from_assignment(assignment: &AdmittedOffer) -> Self {
        Self {
            coordinator: *assignment.coordinator.as_bytes(),
            scope: assignment.offer.scope,
            input_closure_id: assignment.offer.input_closure_id,
            input_manifest_object_id: assignment.offer.input_manifest_object_id,
            deadline_unix_ms: assignment.offer.deadline_unix_ms,
        }
    }

    fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(RUNNING_JOURNAL_BYTES);
        bytes.extend_from_slice(RUNNING_JOURNAL_MAGIC);
        bytes.extend_from_slice(&self.coordinator);
        bytes.extend_from_slice(&self.scope.namespace_id);
        bytes.extend_from_slice(&self.scope.work_id);
        bytes.extend_from_slice(&self.scope.attempt.to_be_bytes());
        bytes.extend_from_slice(&self.scope.fence);
        bytes.extend_from_slice(&self.input_closure_id);
        bytes.extend_from_slice(&self.input_manifest_object_id);
        bytes.extend_from_slice(&self.deadline_unix_ms.to_be_bytes());
        debug_assert_eq!(bytes.len(), RUNNING_JOURNAL_DATA_BYTES);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.running-assignment.v1\0");
        hasher.update(&bytes);
        bytes.extend_from_slice(hasher.finalize().as_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, ClusterWorkerError> {
        if bytes.len() != RUNNING_JOURNAL_BYTES || bytes.get(..8) != Some(RUNNING_JOURNAL_MAGIC) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let (data, checksum) = bytes.split_at(RUNNING_JOURNAL_DATA_BYTES);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.running-assignment.v1\0");
        hasher.update(data);
        if hasher.finalize().as_bytes() != checksum {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let mut reader = ResultRecordReader::new(data);
        if reader.take(8)? != RUNNING_JOURNAL_MAGIC {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let coordinator = reader.array::<32>()?;
        let namespace_id = reader.array::<16>()?;
        let work_id = reader.array::<16>()?;
        let attempt = u64::from_be_bytes(reader.array::<8>()?);
        let fence = reader.array::<32>()?;
        let record = Self {
            coordinator,
            scope: AssignmentScope::new(namespace_id, work_id, attempt, fence)
                .map_err(operation)?,
            input_closure_id: reader.array::<32>()?,
            input_manifest_object_id: reader.array::<32>()?,
            deadline_unix_ms: u64::from_be_bytes(reader.array::<8>()?),
        };
        if !reader.is_empty()
            || record.coordinator == [0; 32]
            || record.input_closure_id == [0; 32]
            || record.input_manifest_object_id == [0; 32]
            || record.deadline_unix_ms == 0
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        Ok(record)
    }
}

impl PendingResultRecord {
    fn from_result(result: &ClusterWorkerResult) -> Self {
        Self {
            coordinator: *result.coordinator.as_bytes(),
            scope: result.scope,
            input_closure_id: result.input_closure_id,
            input_manifest_object_id: result.input_manifest_object_id,
            deadline_unix_ms: result.deadline_unix_ms,
            closure_id: *result.closure.as_bytes(),
            pack_id: result.pack_id,
            target_root: result.target_root,
            payload_bytes: result.payload_bytes,
            object_count: result.object_count,
        }
    }

    fn receipt(self, result_grant_pages: u32) -> ControlResultReceipt {
        ControlResultReceipt {
            scope: self.scope,
            target_root: self.target_root,
            pack_id: self.pack_id,
            closure_id: self.closure_id,
            object_count: self.object_count,
            payload_bytes: self.payload_bytes,
            result_grant_pages,
        }
    }

    fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(RESULT_JOURNAL_BYTES);
        bytes.extend_from_slice(RESULT_JOURNAL_MAGIC);
        bytes.extend_from_slice(&self.coordinator);
        bytes.extend_from_slice(&self.scope.namespace_id);
        bytes.extend_from_slice(&self.scope.work_id);
        bytes.extend_from_slice(&self.scope.attempt.to_be_bytes());
        bytes.extend_from_slice(&self.scope.fence);
        bytes.extend_from_slice(&self.input_closure_id);
        bytes.extend_from_slice(&self.input_manifest_object_id);
        bytes.extend_from_slice(&self.deadline_unix_ms.to_be_bytes());
        bytes.extend_from_slice(&self.closure_id);
        match self.pack_id {
            Some(pack_id) => {
                bytes.push(1);
                bytes.extend_from_slice(&pack_id);
            }
            None => {
                bytes.push(0);
                bytes.extend_from_slice(&[0; 32]);
            }
        }
        bytes.extend_from_slice(&self.target_root);
        bytes.extend_from_slice(&self.payload_bytes.to_be_bytes());
        bytes.extend_from_slice(&self.object_count.to_be_bytes());
        debug_assert_eq!(bytes.len(), RESULT_JOURNAL_DATA_BYTES);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.pending-result.v2\0");
        hasher.update(&bytes);
        bytes.extend_from_slice(hasher.finalize().as_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, ClusterWorkerError> {
        if bytes.len() != RESULT_JOURNAL_BYTES || bytes.get(..8) != Some(RESULT_JOURNAL_MAGIC) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let (data, checksum) = bytes.split_at(RESULT_JOURNAL_DATA_BYTES);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.pending-result.v2\0");
        hasher.update(data);
        if hasher.finalize().as_bytes() != checksum {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let mut reader = ResultRecordReader::new(data);
        if reader.take(8)? != RESULT_JOURNAL_MAGIC {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let coordinator = reader.array::<32>()?;
        let namespace_id = reader.array::<16>()?;
        let work_id = reader.array::<16>()?;
        let attempt = u64::from_be_bytes(reader.array::<8>()?);
        let fence = reader.array::<32>()?;
        let scope =
            AssignmentScope::new(namespace_id, work_id, attempt, fence).map_err(operation)?;
        // Keep this decode order identical to encode(): input identity precedes
        // result identity. All are opaque 32-byte values, so a transposition
        // still passes the checksum and length checks; reopen_pending_result
        // binds them to the independently verified envelope and CAS closure.
        let input_closure_id = reader.array::<32>()?;
        let input_manifest_object_id = reader.array::<32>()?;
        let deadline_unix_ms = u64::from_be_bytes(reader.array::<8>()?);
        let closure_id = reader.array::<32>()?;
        let pack_tag = reader.take(1)?[0];
        let pack_bytes = reader.array::<32>()?;
        let pack_id = match pack_tag {
            0 if pack_bytes == [0; 32] => None,
            1 if pack_bytes != [0; 32] => Some(pack_bytes),
            _ => return Err(ClusterWorkerError::InvalidInputClosure),
        };
        let record = Self {
            coordinator,
            scope,
            input_closure_id,
            input_manifest_object_id,
            deadline_unix_ms,
            closure_id,
            pack_id,
            target_root: reader.array::<32>()?,
            payload_bytes: u64::from_be_bytes(reader.array::<8>()?),
            object_count: u32::from_be_bytes(reader.array::<4>()?),
        };
        if !reader.is_empty()
            || record.coordinator == [0; 32]
            || record.input_closure_id == [0; 32]
            || record.input_manifest_object_id == [0; 32]
            || record.closure_id == [0; 32]
            || record.target_root == [0; 32]
            || record.deadline_unix_ms == 0
            || record.payload_bytes == 0
            || record.object_count == 0
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        Ok(record)
    }
}

/// Durable terminal owner disposition kept until the owner's retirement confirmation is applied.
/// This small record replaces result-byte replay after the worker has received a terminal ACK.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetiredResultRecord {
    coordinator: [u8; 32],
    scope: AssignmentScope,
    closure_id: [u8; 32],
    disposition: ResultAckDisposition,
}

/// Durable negative result observation for one exact owner offer attempt. This record prevents
/// a delayed Offer from becoming runnable after recovery has told the owner that no result exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NoResultRecord {
    coordinator: [u8; 32],
    scope: AssignmentScope,
}

impl NoResultRecord {
    fn new(coordinator: EndpointId, scope: AssignmentScope) -> Self {
        Self {
            coordinator: *coordinator.as_bytes(),
            scope,
        }
    }

    fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(NO_RESULT_JOURNAL_BYTES);
        bytes.extend_from_slice(NO_RESULT_JOURNAL_MAGIC);
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes.extend_from_slice(&self.coordinator);
        bytes.extend_from_slice(&self.scope.namespace_id);
        bytes.extend_from_slice(&self.scope.work_id);
        bytes.extend_from_slice(&self.scope.attempt.to_be_bytes());
        bytes.extend_from_slice(&self.scope.fence);
        debug_assert_eq!(bytes.len(), NO_RESULT_JOURNAL_DATA_BYTES);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.no-result-recovery.v1\0");
        hasher.update(&bytes);
        bytes.extend_from_slice(hasher.finalize().as_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, ClusterWorkerError> {
        if bytes.len() != NO_RESULT_JOURNAL_BYTES || bytes.get(..8) != Some(NO_RESULT_JOURNAL_MAGIC)
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let (data, checksum) = bytes.split_at(NO_RESULT_JOURNAL_DATA_BYTES);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.no-result-recovery.v1\0");
        hasher.update(data);
        if hasher.finalize().as_bytes() != checksum {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let mut reader = ResultRecordReader::new(data);
        if reader.take(8)? != NO_RESULT_JOURNAL_MAGIC || reader.array::<2>()? != 1_u16.to_be_bytes()
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let coordinator = reader.array::<32>()?;
        let namespace_id = reader.array::<16>()?;
        let work_id = reader.array::<16>()?;
        let attempt = u64::from_be_bytes(reader.array::<8>()?);
        let fence = reader.array::<32>()?;
        let record = Self {
            coordinator,
            scope: AssignmentScope::new(namespace_id, work_id, attempt, fence)
                .map_err(operation)?,
        };
        if !reader.is_empty() || record.coordinator == [0; 32] {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        Ok(record)
    }

    fn matches_query(self, query: &ControlResultRecoveryQuery, coordinator: EndpointId) -> bool {
        self.scope == query.scope
            && self.coordinator == *coordinator.as_bytes()
            && query.is_valid_from(coordinator)
    }
}

/// Durable namespace-local epoch prefix. The proof list retains each exact NoResult scope
/// removed by a prefix, so a lost ACK can be retried without accepting a same-epoch wrong fence.
#[derive(Clone, Debug, Eq, PartialEq)]
struct NoResultRetirementFloor {
    coordinator: [u8; 32],
    scope: AssignmentScope,
    retired_through_epoch: u64,
    retired_terminals: Vec<AssignmentScope>,
}

impl NoResultRetirementFloor {
    fn from_request(request: ControlNoResultRetireThrough) -> Self {
        Self {
            coordinator: request.coordinator_endpoint_id,
            scope: request.scope,
            retired_through_epoch: request.retired_through_epoch,
            retired_terminals: vec![request.terminal_scope],
        }
    }

    fn encode(&self) -> Result<Vec<u8>, ClusterWorkerError> {
        if self.retired_terminals.len() > MAX_NO_RESULT_RETIREMENT_TERMINALS
            || self.retired_terminals.windows(2).any(|pair| {
                no_result_terminal_key(pair[0]) >= no_result_terminal_key(pair[1])
            })
            || self.retired_terminals.iter().any(|terminal| {
                terminal.namespace_id != self.scope.namespace_id
                    || terminal.attempt > self.retired_through_epoch
            })
        {
            return Err(ClusterWorkerError::InputBounds);
        }
        let mut bytes = Vec::with_capacity(
            NO_RESULT_FLOOR_V3_BASE_DATA_BYTES
                + self.retired_terminals.len() * NO_RESULT_TERMINAL_SCOPE_BYTES
                + 32,
        );
        bytes.extend_from_slice(NO_RESULT_FLOOR_MAGIC);
        bytes.extend_from_slice(&3_u16.to_be_bytes());
        bytes.extend_from_slice(&self.coordinator);
        bytes.extend_from_slice(&self.scope.namespace_id);
        bytes.extend_from_slice(&self.retired_through_epoch.to_be_bytes());
        bytes.extend_from_slice(&self.scope.work_id);
        bytes.extend_from_slice(&self.scope.attempt.to_be_bytes());
        bytes.extend_from_slice(&self.scope.fence);
        let terminal_count = u32::try_from(self.retired_terminals.len())
            .map_err(|_| ClusterWorkerError::InputBounds)?;
        bytes.extend_from_slice(&terminal_count.to_be_bytes());
        for terminal in &self.retired_terminals {
            put_no_result_terminal_scope(&mut bytes, *terminal);
        }
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.no-result-retirement-floor.v3\0");
        hasher.update(&bytes);
        bytes.extend_from_slice(hasher.finalize().as_bytes());
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, ClusterWorkerError> {
        if (bytes.len() != NO_RESULT_FLOOR_V1_BYTES
            && bytes.len() != NO_RESULT_FLOOR_V2_BYTES
            && (bytes.len() < NO_RESULT_FLOOR_V3_BASE_DATA_BYTES + 32
                || bytes.len() > NO_RESULT_FLOOR_MAX_BYTES))
            || bytes.get(..8) != Some(NO_RESULT_FLOOR_MAGIC)
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let version = u16::from_be_bytes(
            bytes
                .get(8..10)
                .ok_or(ClusterWorkerError::InvalidInputClosure)?
                .try_into()
                .map_err(operation)?,
        );
        let data_length = match version {
            1 if bytes.len() == NO_RESULT_FLOOR_V1_BYTES => NO_RESULT_FLOOR_V1_DATA_BYTES,
            2 if bytes.len() == NO_RESULT_FLOOR_V2_BYTES => NO_RESULT_FLOOR_V2_DATA_BYTES,
            3 if (bytes.len() - 32 - NO_RESULT_FLOOR_V3_BASE_DATA_BYTES)
                % NO_RESULT_TERMINAL_SCOPE_BYTES
                == 0 =>
            {
                bytes.len() - 32
            }
            _ => return Err(ClusterWorkerError::InvalidInputClosure),
        };
        let (data, checksum) = bytes.split_at(data_length);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(match version {
            1 => b"backend.worker.no-result-retirement-floor.v1\0".as_slice(),
            2 => b"backend.worker.no-result-retirement-floor.v2\0".as_slice(),
            3 => b"backend.worker.no-result-retirement-floor.v3\0".as_slice(),
            _ => return Err(ClusterWorkerError::InvalidInputClosure),
        });
        hasher.update(data);
        if hasher.finalize().as_bytes() != checksum {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let mut reader = ResultRecordReader::new(data);
        if reader.take(8)? != NO_RESULT_FLOOR_MAGIC
            || reader.array::<2>()? != version.to_be_bytes()
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let coordinator = reader.array::<32>()?;
        let namespace_id = reader.array::<16>()?;
        let legacy_terminal = if version == 2 {
            let work_id = reader.array::<16>()?;
            let attempt = u64::from_be_bytes(reader.array::<8>()?);
            let fence = reader.array::<32>()?;
            Some(
                AssignmentScope::new(namespace_id, work_id, attempt, fence)
                    .map_err(operation)?,
            )
        } else {
            None
        };
        let retired_through_epoch = u64::from_be_bytes(reader.array::<8>()?);
        let work_id = reader.array::<16>()?;
        let attempt = u64::from_be_bytes(reader.array::<8>()?);
        let fence = reader.array::<32>()?;
        let scope = AssignmentScope::new(namespace_id, work_id, attempt, fence)
            .map_err(operation)?;
        let mut retired_terminals = legacy_terminal.into_iter().collect::<Vec<_>>();
        if version == 3 {
            let count = usize::try_from(u32::from_be_bytes(reader.array::<4>()?))
                .map_err(operation)?;
            if count > MAX_NO_RESULT_RETIREMENT_TERMINALS {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            retired_terminals.reserve(count);
            for _ in 0..count {
                retired_terminals.push(read_no_result_terminal_scope(&mut reader, namespace_id)?);
            }
        }
        let record = Self {
            coordinator,
            scope,
            retired_through_epoch,
            retired_terminals,
        };
        if !reader.is_empty()
            || record.coordinator == [0; 32]
            || record.retired_through_epoch == 0
            || record.retired_through_epoch >= record.scope.attempt
            || record.retired_terminals.windows(2).any(|pair| {
                no_result_terminal_key(pair[0]) >= no_result_terminal_key(pair[1])
            })
            || record.retired_terminals.iter().any(|terminal| {
                terminal.namespace_id != record.scope.namespace_id
                    || terminal.attempt > record.retired_through_epoch
            })
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        Ok(record)
    }

    fn covers(&self, coordinator: EndpointId, scope: AssignmentScope) -> bool {
        self.coordinator == *coordinator.as_bytes()
            && self.scope.namespace_id == scope.namespace_id
            && scope.attempt <= self.retired_through_epoch
    }

    fn contains_terminal(&self, terminal: AssignmentScope) -> bool {
        self.retired_terminals.contains(&terminal)
    }

    fn record_terminal(&mut self, terminal: AssignmentScope) -> Result<(), ClusterWorkerError> {
        if terminal.namespace_id != self.scope.namespace_id
            || terminal.attempt > self.retired_through_epoch
        {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        if self.contains_terminal(terminal) {
            return Ok(());
        }
        if self.retired_terminals.len() >= MAX_NO_RESULT_RETIREMENT_TERMINALS {
            return Err(ClusterWorkerError::InputBounds);
        }
        self.retired_terminals.push(terminal);
        self.retired_terminals.sort_by_key(|scope| no_result_terminal_key(*scope));
        Ok(())
    }
}

fn no_result_terminal_key(scope: AssignmentScope) -> (u64, [u8; 16], [u8; 32]) {
    (scope.attempt, scope.work_id, scope.fence)
}

fn put_no_result_terminal_scope(bytes: &mut Vec<u8>, scope: AssignmentScope) {
    bytes.extend_from_slice(&scope.work_id);
    bytes.extend_from_slice(&scope.attempt.to_be_bytes());
    bytes.extend_from_slice(&scope.fence);
}

fn read_no_result_terminal_scope(
    reader: &mut ResultRecordReader<'_>,
    namespace_id: [u8; 16],
) -> Result<AssignmentScope, ClusterWorkerError> {
    let work_id = reader.array::<16>()?;
    let attempt = u64::from_be_bytes(reader.array::<8>()?);
    let fence = reader.array::<32>()?;
    AssignmentScope::new(namespace_id, work_id, attempt, fence).map_err(operation)
}

impl RetiredResultRecord {
    fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(RETIRED_JOURNAL_BYTES);
        bytes.extend_from_slice(RETIRED_JOURNAL_MAGIC);
        bytes.extend_from_slice(&1_u16.to_be_bytes());
        bytes.extend_from_slice(&self.coordinator);
        bytes.extend_from_slice(&self.scope.namespace_id);
        bytes.extend_from_slice(&self.scope.work_id);
        bytes.extend_from_slice(&self.scope.attempt.to_be_bytes());
        bytes.extend_from_slice(&self.scope.fence);
        bytes.extend_from_slice(&self.closure_id);
        bytes.push(encode_ack_disposition(self.disposition));
        debug_assert_eq!(bytes.len(), RETIRED_JOURNAL_DATA_BYTES);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.retired-result.v1\0");
        hasher.update(&bytes);
        bytes.extend_from_slice(hasher.finalize().as_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, ClusterWorkerError> {
        if bytes.len() != RETIRED_JOURNAL_BYTES || bytes.get(..8) != Some(RETIRED_JOURNAL_MAGIC) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let (data, checksum) = bytes.split_at(RETIRED_JOURNAL_DATA_BYTES);
        let mut hasher = backend_engine::blake3::Hasher::new();
        hasher.update(b"backend.worker.retired-result.v1\0");
        hasher.update(data);
        if hasher.finalize().as_bytes() != checksum {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let mut reader = ResultRecordReader::new(data);
        if reader.take(8)? != RETIRED_JOURNAL_MAGIC || reader.array::<2>()? != 1_u16.to_be_bytes() {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        let coordinator = reader.array::<32>()?;
        let namespace_id = reader.array::<16>()?;
        let work_id = reader.array::<16>()?;
        let attempt = u64::from_be_bytes(reader.array::<8>()?);
        let fence = reader.array::<32>()?;
        let record = Self {
            coordinator,
            scope: AssignmentScope::new(namespace_id, work_id, attempt, fence)
                .map_err(operation)?,
            closure_id: reader.array::<32>()?,
            disposition: decode_ack_disposition(reader.take(1)?[0])?,
        };
        if !reader.is_empty() || record.coordinator == [0; 32] || record.closure_id == [0; 32] {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        Ok(record)
    }

    fn matches_ack(self, ack: &ControlResultAck) -> bool {
        self.scope == ack.scope
            && self.closure_id == ack.closure_id
            && self.disposition == ack.disposition
    }

    fn matches_confirm(
        self,
        confirm: &ControlResultRetirementConfirm,
        coordinator: EndpointId,
    ) -> bool {
        self.scope == confirm.scope
            && self.closure_id == confirm.closure_id
            && self.disposition == confirm.disposition
            && self.coordinator == *coordinator.as_bytes()
            && confirm.coordinator_endpoint_id == *coordinator.as_bytes()
    }

    fn receipt(self, worker: EndpointId) -> Result<ControlResultRetired, ClusterWorkerError> {
        ControlResultRetired::new(self.scope, self.closure_id, self.disposition, worker)
            .map_err(operation)
    }
}

fn encode_ack_disposition(disposition: ResultAckDisposition) -> u8 {
    match disposition {
        ResultAckDisposition::Stored => 0,
        ResultAckDisposition::Rejected(ResultRejectReason::Scope) => 1,
        ResultAckDisposition::Rejected(ResultRejectReason::Closure) => 2,
        ResultAckDisposition::Rejected(ResultRejectReason::Metadata) => 3,
        ResultAckDisposition::Rejected(ResultRejectReason::Admission) => 4,
    }
}

fn decode_ack_disposition(code: u8) -> Result<ResultAckDisposition, ClusterWorkerError> {
    match code {
        0 => Ok(ResultAckDisposition::Stored),
        1 => Ok(ResultAckDisposition::Rejected(ResultRejectReason::Scope)),
        2 => Ok(ResultAckDisposition::Rejected(ResultRejectReason::Closure)),
        3 => Ok(ResultAckDisposition::Rejected(ResultRejectReason::Metadata)),
        4 => Ok(ResultAckDisposition::Rejected(
            ResultRejectReason::Admission,
        )),
        _ => Err(ClusterWorkerError::InvalidInputClosure),
    }
}

struct ResultRecordReader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> ResultRecordReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ClusterWorkerError> {
        let end = self
            .cursor
            .checked_add(count)
            .ok_or(ClusterWorkerError::InvalidInputClosure)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ClusterWorkerError::InvalidInputClosure)?;
        self.cursor = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ClusterWorkerError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ClusterWorkerError::InvalidInputClosure)
    }

    fn is_empty(&self) -> bool {
        self.cursor == self.bytes.len()
    }
}

fn persist_pending_result(result: &ClusterWorkerResult) -> Result<(), ClusterWorkerError> {
    ensure_pending_result_capacity(&result.store)?;
    persist_journal_record(
        &result.store,
        result.scope,
        "pending",
        &PendingResultRecord::from_result(result).encode(),
    )
}

fn ensure_pending_result_capacity(store: &FileStore) -> Result<(), ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(operation(error)),
    };
    let mut pending = 0_usize;
    for entry in entries {
        let entry = entry.map_err(operation)?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("pending") {
            pending = pending
                .checked_add(1)
                .ok_or(ClusterWorkerError::InputBounds)?;
            if pending >= MAX_PENDING_RESULTS {
                return Err(ClusterWorkerError::PendingResultQuota);
            }
        }
    }
    Ok(())
}

fn ensure_retired_result_capacity(store: &FileStore) -> Result<(), ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(operation(error)),
    };
    let mut retired = 0_usize;
    for entry in entries {
        let path = entry.map_err(operation)?.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("retired") {
            retired = retired
                .checked_add(1)
                .ok_or(ClusterWorkerError::InputBounds)?;
        }
    }
    if retired >= MAX_RETIRED_RESULTS {
        return Err(ClusterWorkerError::RetirementQuota);
    }
    Ok(())
}

fn persist_running_assignment(
    store: &FileStore,
    assignment: &AdmittedOffer,
) -> Result<(), ClusterWorkerError> {
    persist_journal_record(
        store,
        assignment.offer.scope,
        "running",
        &RunningAssignmentRecord::from_assignment(assignment).encode(),
    )
}

fn persist_journal_record(
    store: &FileStore,
    scope: AssignmentScope,
    extension: &str,
    bytes: &[u8],
) -> Result<(), ClusterWorkerError> {
    let directory = result_journal_directory(store, true)?;
    let path = result_journal_path(&directory, scope, extension);
    backend_platform::durable::write_private_atomic(&path, bytes).map_err(operation)
}

fn read_optional_journal_bytes(
    store: &FileStore,
    scope: AssignmentScope,
    extension: &str,
    expected_bytes: usize,
) -> Result<Option<Vec<u8>>, ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let path = result_journal_path(&directory, scope, extension);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(operation(error)),
    };
    validate_journal_metadata(&metadata, expected_bytes)?;
    let mut file = open_private_read_file(&path, expected_bytes)?;
    let mut bytes = Vec::with_capacity(expected_bytes);
    file.by_ref()
        .take((expected_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(operation)?;
    if bytes.len() != expected_bytes {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(Some(bytes))
}

fn read_retired_result_record(
    store: &FileStore,
    scope: AssignmentScope,
) -> Result<Option<RetiredResultRecord>, ClusterWorkerError> {
    read_optional_journal_bytes(store, scope, "retired", RETIRED_JOURNAL_BYTES)?
        .map(|bytes| RetiredResultRecord::decode(&bytes))
        .transpose()
}

fn read_no_result_record(
    store: &FileStore,
    scope: AssignmentScope,
) -> Result<Option<NoResultRecord>, ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let path = no_result_journal_path(&directory, scope);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(operation(error)),
    };
    validate_journal_metadata(&metadata, NO_RESULT_JOURNAL_BYTES)?;
    let mut file = open_private_read_file(&path, NO_RESULT_JOURNAL_BYTES)?;
    let mut bytes = Vec::with_capacity(NO_RESULT_JOURNAL_BYTES);
    file.by_ref()
        .take((NO_RESULT_JOURNAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(operation)?;
    if bytes.len() != NO_RESULT_JOURNAL_BYTES {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    let record = NoResultRecord::decode(&bytes)?;
    if record.scope != scope {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(Some(record))
}

fn read_no_result_floor(
    store: &FileStore,
    namespace_id: [u8; 16],
    coordinator: EndpointId,
) -> Result<Option<NoResultRetirementFloor>, ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let path = no_result_floor_path(&directory, namespace_id, coordinator);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(operation(error)),
    };
    let bytes = read_no_result_floor_bytes(&path, &metadata)?;
    let floor = NoResultRetirementFloor::decode(&bytes)?;
    if floor.scope.namespace_id != namespace_id
        || floor.coordinator != *coordinator.as_bytes()
        || no_result_floor_path(&directory, floor.scope.namespace_id, coordinator) != path
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(Some(floor))
}

/// Validates persisted floors against the current coordinator configuration. Multi-coordinator
/// workers retain exact tombstones and may not open a floor created under a former single-owner
/// configuration, because doing so could unmask the same old scope for another authenticated peer.
fn validate_no_result_retirement_configuration(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    authorized_coordinators: impl IntoIterator<Item = EndpointId>,
) -> Result<Option<NoResultRetirementFloor>, ClusterWorkerError> {
    let authorized = authorized_coordinators.into_iter().collect::<Vec<_>>();
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(operation(error)),
    };
    let mut found = None;
    for entry in entries {
        let path = entry.map_err(operation)?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("nrfloor") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(operation)?;
        let bytes = read_no_result_floor_bytes(&path, &metadata)?;
        let floor = NoResultRetirementFloor::decode(&bytes)?;
        let coordinator = EndpointId::from_bytes(&floor.coordinator).map_err(operation)?;
        if floor.scope.namespace_id != policy.namespace_id
            || no_result_floor_path(&directory, floor.scope.namespace_id, coordinator) != path
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        if authorized.len() != 1 || authorized.first() != Some(&coordinator) {
            return Err(ClusterWorkerError::PeerNotAllowed);
        }
        if found.replace(floor).is_some() {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
    }
    Ok(found)
}

fn read_no_result_floor_bytes(
    path: &Path,
    metadata: &fs::Metadata,
) -> Result<Vec<u8>, ClusterWorkerError> {
    let bytes = usize::try_from(metadata.len()).map_err(operation)?;
    if bytes != NO_RESULT_FLOOR_V1_BYTES
        && bytes != NO_RESULT_FLOOR_V2_BYTES
        && !(bytes >= NO_RESULT_FLOOR_V3_BASE_DATA_BYTES + 32
            && bytes <= NO_RESULT_FLOOR_MAX_BYTES)
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    validate_journal_metadata(metadata, bytes)?;
    let mut file = open_private_read_file(path, bytes)?;
    let mut contents = Vec::with_capacity(bytes);
    file.by_ref()
        .take((bytes + 1) as u64)
        .read_to_end(&mut contents)
        .map_err(operation)?;
    if contents.len() != bytes {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(contents)
}

fn ensure_exact_no_result_terminal(
    store: &FileStore,
    terminal_scope: AssignmentScope,
    coordinator: EndpointId,
) -> Result<(), ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        Err(error) => return Err(operation(error)),
    };
    let mut exact = false;
    for entry in entries {
        let path = entry.map_err(operation)?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("noresult") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(operation)?;
        validate_journal_metadata(&metadata, NO_RESULT_JOURNAL_BYTES)?;
        let mut file = open_private_read_file(&path, NO_RESULT_JOURNAL_BYTES)?;
        let mut bytes = Vec::with_capacity(NO_RESULT_JOURNAL_BYTES);
        file.by_ref()
            .take((NO_RESULT_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(operation)?;
        let record = NoResultRecord::decode(&bytes)?;
        if record.scope.namespace_id == terminal_scope.namespace_id
            && record.scope.attempt == terminal_scope.attempt
        {
            if record.scope != terminal_scope || record.coordinator != *coordinator.as_bytes() {
                return Err(ClusterWorkerError::ScopeMismatch);
            }
            exact = true;
        }
    }
    if exact {
        Ok(())
    } else {
        Err(ClusterWorkerError::ScopeMismatch)
    }
}

fn no_result_floor_blocks(
    store: &FileStore,
    coordinator: EndpointId,
    scope: AssignmentScope,
) -> Result<bool, ClusterWorkerError> {
    Ok(
        read_no_result_floor(store, scope.namespace_id, coordinator)?
            .is_some_and(|floor| floor.covers(coordinator, scope)),
    )
}

fn apply_no_result_retirement_floor(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    authorized_coordinators: &[EndpointId],
    authenticated_coordinator: EndpointId,
    request: ControlNoResultRetireThrough,
) -> Result<(), ClusterWorkerError> {
    if authorized_coordinators.len() != 1
        || authorized_coordinators.first() != Some(&authenticated_coordinator)
        || request.scope.namespace_id != policy.namespace_id
        || request.terminal_scope.namespace_id != policy.namespace_id
        || !request.matches_coordinator(authenticated_coordinator)
    {
        return Err(ClusterWorkerError::PeerNotAllowed);
    }
    let existing = validate_no_result_retirement_configuration(
        store,
        policy,
        authorized_coordinators.iter().copied(),
    )?;
    let mut next = NoResultRetirementFloor::from_request(request);
    validate_no_result_records(store, policy)?;
    if let Some(current) = existing {
        if current.contains_terminal(request.terminal_scope) {
            compact_no_result_records_through_floor(store, policy, Some(current))?;
            return Ok(());
        }
        ensure_exact_no_result_terminal(store, request.terminal_scope, authenticated_coordinator)?;
        if current.covers(authenticated_coordinator, request.terminal_scope) {
            // A prior prefix may already cover this exact terminal. Preserve its stronger floor
            // and durably add the exact scope before reporting the repeat as applied.
            next.scope = current.scope;
            next.retired_through_epoch = current.retired_through_epoch;
            next.retired_terminals = current.retired_terminals;
        } else {
            if next.retired_through_epoch <= current.retired_through_epoch
                || next.scope.attempt <= current.scope.attempt
            {
                return Err(ClusterWorkerError::ScopeMismatch);
            }
            next.retired_terminals = current.retired_terminals;
        }
    } else {
        ensure_exact_no_result_terminal(store, request.terminal_scope, authenticated_coordinator)?;
    }
    for terminal in no_result_terminal_scopes_through_floor(
        store,
        policy,
        authenticated_coordinator,
        next.retired_through_epoch,
    )? {
        next.record_terminal(terminal)?;
    }
    next.record_terminal(request.terminal_scope)?;
    let directory = result_journal_directory(store, true)?;
    backend_platform::durable::write_private_atomic(
        &no_result_floor_path(&directory, policy.namespace_id, authenticated_coordinator),
        &next.encode()?,
    )
    .map_err(operation)?;
    compact_no_result_records_through_floor(store, policy, Some(next))?;
    Ok(())
}

fn no_result_terminal_scopes_through_floor(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    coordinator: EndpointId,
    through: u64,
) -> Result<Vec<AssignmentScope>, ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(operation(error)),
    };
    let mut scopes = Vec::new();
    for entry in entries {
        let path = entry.map_err(operation)?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("noresult") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(operation)?;
        validate_journal_metadata(&metadata, NO_RESULT_JOURNAL_BYTES)?;
        let mut file = open_private_read_file(&path, NO_RESULT_JOURNAL_BYTES)?;
        let mut bytes = Vec::with_capacity(NO_RESULT_JOURNAL_BYTES);
        file.by_ref()
            .take((NO_RESULT_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(operation)?;
        let record = NoResultRecord::decode(&bytes)?;
        let record_coordinator = EndpointId::from_bytes(&record.coordinator).map_err(operation)?;
        if no_result_journal_path(&directory, record.scope) != path
            || record.scope.namespace_id != policy.namespace_id
            || !policy_coordinator_is_allowed(policy, record_coordinator)
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        if record_coordinator == coordinator && record.scope.attempt <= through {
            if scopes.len() >= MAX_NO_RESULT_RETIREMENT_TERMINALS {
                return Err(ClusterWorkerError::InputBounds);
            }
            scopes.push(record.scope);
        }
    }
    Ok(scopes)
}

fn compact_no_result_records_through_floor(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    floor: Option<NoResultRetirementFloor>,
) -> Result<usize, ClusterWorkerError> {
    let Some(floor) = floor else {
        return Ok(0);
    };
    let floor_coordinator = EndpointId::from_bytes(&floor.coordinator).map_err(operation)?;
    if floor.scope.namespace_id != policy.namespace_id
        || !policy_coordinator_is_allowed(policy, floor_coordinator)
        || floor.retired_through_epoch == 0
        || floor.retired_through_epoch >= floor.scope.attempt
        || floor.retired_terminals.iter().any(|terminal| {
            terminal.namespace_id != policy.namespace_id
                || terminal.attempt > floor.retired_through_epoch
        })
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(operation(error)),
    };
    let mut removed = 0_usize;
    for entry in entries {
        let path = entry.map_err(operation)?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("noresult") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(operation)?;
        validate_journal_metadata(&metadata, NO_RESULT_JOURNAL_BYTES)?;
        let mut file = open_private_read_file(&path, NO_RESULT_JOURNAL_BYTES)?;
        let mut bytes = Vec::with_capacity(NO_RESULT_JOURNAL_BYTES);
        file.by_ref()
            .take((NO_RESULT_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(operation)?;
        let record = NoResultRecord::decode(&bytes)?;
        if no_result_journal_path(&directory, record.scope) != path
            || record.scope.namespace_id != policy.namespace_id
            || !policy_coordinator_is_allowed(
                policy,
                EndpointId::from_bytes(&record.coordinator).map_err(operation)?,
            )
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        if floor.covers(
            EndpointId::from_bytes(&record.coordinator).map_err(operation)?,
            record.scope,
        ) {
            if read_pending_result_record(store, record.scope)?.is_some()
                || read_running_assignment_record(store, record.scope)?.is_some()
                || read_retired_result_record(store, record.scope)?.is_some()
            {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            backend_platform::durable::remove_private(&path).map_err(operation)?;
            removed = removed
                .checked_add(1)
                .ok_or(ClusterWorkerError::InputBounds)?;
        }
    }
    Ok(removed)
}

fn validate_no_result_records(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
) -> Result<(), ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(operation(error)),
    };
    for entry in entries {
        let path = entry.map_err(operation)?.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("noresult") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(operation)?;
        validate_journal_metadata(&metadata, NO_RESULT_JOURNAL_BYTES)?;
        let mut file = open_private_read_file(&path, NO_RESULT_JOURNAL_BYTES)?;
        let mut bytes = Vec::with_capacity(NO_RESULT_JOURNAL_BYTES);
        file.by_ref()
            .take((NO_RESULT_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(operation)?;
        let record = NoResultRecord::decode(&bytes)?;
        let coordinator = EndpointId::from_bytes(&record.coordinator).map_err(operation)?;
        if no_result_journal_path(&directory, record.scope) != path
            || record.scope.namespace_id != policy.namespace_id
            || !policy_coordinator_is_allowed(policy, coordinator)
            || read_pending_result_record(store, record.scope)?.is_some()
            || read_running_assignment_record(store, record.scope)?.is_some()
            || read_retired_result_record(store, record.scope)?.is_some()
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
    }
    Ok(())
}

fn policy_coordinator_is_allowed(policy: &ClusterWorkerPolicy, coordinator: EndpointId) -> bool {
    match &policy.execution_policy {
        ClusterExecutionPolicy::DenyUnconfined => true,
        ClusterExecutionPolicy::Allowlisted(grants) => {
            grants.iter().any(|grant| grant.coordinator == coordinator)
        }
    }
}

fn persist_no_result_record(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    record: NoResultRecord,
) -> Result<(), ClusterWorkerError> {
    let coordinator = EndpointId::from_bytes(&record.coordinator).map_err(operation)?;
    if record.scope.namespace_id != policy.namespace_id
        || !policy_coordinator_is_allowed(policy, coordinator)
    {
        return Err(ClusterWorkerError::PeerNotAllowed);
    }
    if let Some(mut floor) = read_no_result_floor(store, policy.namespace_id, coordinator)?
        && floor.covers(coordinator, record.scope)
    {
        // An assignment that raced with floor persistence is already retired. Remember its
        // exact scope durably and compact it without recreating a tombstone after the prefix.
        floor.record_terminal(record.scope)?;
        let directory = result_journal_directory(store, true)?;
        backend_platform::durable::write_private_atomic(
            &no_result_floor_path(&directory, policy.namespace_id, coordinator),
            &floor.encode()?,
        )
        .map_err(operation)?;
        compact_no_result_records_through_floor(store, policy, Some(floor))?;
        return Ok(());
    }
    if let Some(existing) = read_no_result_record(store, record.scope)? {
        return if existing == record {
            Ok(())
        } else {
            Err(ClusterWorkerError::ScopeMismatch)
        };
    }
    if read_pending_result_record(store, record.scope)?.is_some()
        || read_running_assignment_record(store, record.scope)?.is_some()
        || read_retired_result_record(store, record.scope)?.is_some()
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    let directory = result_journal_directory(store, true)?;
    backend_platform::durable::write_private_atomic(
        &no_result_journal_path(&directory, record.scope),
        &record.encode(),
    )
    .map_err(operation)
}

fn count_no_result_rows(store: &FileStore) -> Result<usize, ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let mut entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(operation(error)),
    };
    entries.try_fold(0_usize, |count, entry| {
        let path = entry.map_err(operation)?.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("noresult") {
            count.checked_add(1).ok_or(ClusterWorkerError::InputBounds)
        } else {
            Ok(count)
        }
    })
}

fn scope_has_recovery_record(
    store: &FileStore,
    _policy: &ClusterWorkerPolicy,
    scope: AssignmentScope,
) -> Result<bool, ClusterWorkerError> {
    Ok(read_pending_result_record(store, scope)?.is_some()
        || read_running_assignment_record(store, scope)?.is_some()
        || read_retired_result_record(store, scope)?.is_some()
        || read_no_result_record(store, scope)?.is_some())
}

fn offer_scope_is_blocked_by_recovery(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    coordinator: EndpointId,
    scope: AssignmentScope,
) -> Result<bool, ClusterWorkerError> {
    Ok(scope_has_recovery_record(store, policy, scope)?
        || no_result_floor_blocks(store, coordinator, scope)?)
}

fn read_pending_result_record(
    store: &FileStore,
    scope: AssignmentScope,
) -> Result<Option<PendingResultRecord>, ClusterWorkerError> {
    read_optional_journal_bytes(store, scope, "pending", RESULT_JOURNAL_BYTES)?
        .map(|bytes| PendingResultRecord::decode(&bytes))
        .transpose()
}

fn read_running_assignment_record(
    store: &FileStore,
    scope: AssignmentScope,
) -> Result<Option<RunningAssignmentRecord>, ClusterWorkerError> {
    read_optional_journal_bytes(store, scope, "running", RUNNING_JOURNAL_BYTES)?
        .map(|bytes| RunningAssignmentRecord::decode(&bytes))
        .transpose()
}

fn has_no_live_retirement_conflict(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    scope: AssignmentScope,
) -> Result<bool, ClusterWorkerError> {
    if read_pending_result_record(store, scope)?.is_some()
        || read_running_assignment_record(store, scope)?.is_some()
        || read_retired_result_records(store, policy)?
            .iter()
            .any(|record| record.scope == scope)
    {
        return Ok(false);
    }
    Ok(true)
}

fn persist_retired_result_record(
    store: &FileStore,
    record: RetiredResultRecord,
) -> Result<(), ClusterWorkerError> {
    if let Some(existing) = read_retired_result_record(store, record.scope)? {
        if existing != record {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        return Ok(());
    }
    ensure_retired_result_capacity(store)?;
    persist_journal_record(store, record.scope, "retired", &record.encode())
}

fn validate_retired_result_sources(
    store: &FileStore,
    record: RetiredResultRecord,
) -> Result<(), ClusterWorkerError> {
    let pending = read_pending_result_record(store, record.scope)?;
    if let Some(pending) = pending {
        if pending.scope != record.scope
            || pending.coordinator != record.coordinator
            || pending.closure_id != record.closure_id
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
    }
    let running = read_running_assignment_record(store, record.scope)?;
    if let Some(running) = running {
        if pending.is_none()
            || running.scope != record.scope
            || running.coordinator != record.coordinator
            || pending.is_some_and(|pending| {
                pending.input_closure_id != running.input_closure_id
                    || pending.input_manifest_object_id != running.input_manifest_object_id
            })
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
    }
    Ok(())
}

fn apply_retired_result_record(
    store: &FileStore,
    record: RetiredResultRecord,
) -> Result<(), ClusterWorkerError> {
    retire_result_payload_state(store, record)?;
    let directory = result_journal_directory(store, false)?;
    let path = result_journal_path(&directory, record.scope, "retired");
    match read_retired_result_record(store, record.scope)? {
        Some(existing) if existing == record => {
            backend_platform::durable::remove_private(&path).map_err(operation)
        }
        Some(_) => Err(ClusterWorkerError::InvalidInputClosure),
        None => Err(ClusterWorkerError::InvalidInputClosure),
    }
}

fn retire_result_payload_state(
    store: &FileStore,
    record: RetiredResultRecord,
) -> Result<(), ClusterWorkerError> {
    validate_retired_result_sources(store, record)?;
    // After a terminal owner disposition the payload is no longer needed for selection. The
    // compact tombstone is the sole evidence retained until Confirm; any interrupted unlink can
    // be completed idempotently from that record on restart.
    remove_running_assignment(store, record.scope)?;
    remove_pending_result_for_scope(store, record.scope)?;
    Ok(())
}

fn remove_pending_result(result: &ClusterWorkerResult) -> Result<(), ClusterWorkerError> {
    remove_pending_result_for_scope(&result.store, result.scope)
}

fn remove_pending_result_for_scope(
    store: &FileStore,
    scope: AssignmentScope,
) -> Result<(), ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let path = result_journal_path(&directory, scope, "pending");
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            validate_result_record_metadata(&metadata)?;
            backend_platform::durable::remove_private(&path).map_err(operation)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(operation(error)),
    }
}

fn remove_running_assignment(
    store: &FileStore,
    scope: AssignmentScope,
) -> Result<(), ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let path = result_journal_path(&directory, scope, "running");
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            validate_journal_metadata(&metadata, RUNNING_JOURNAL_BYTES)?;
            backend_platform::durable::remove_private(&path).map_err(operation)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(operation(error)),
    }
}

fn append_abandon_audit(
    store: &FileStore,
    result: &ClusterWorkerResult,
    phase: &str,
) -> Result<(), ClusterWorkerError> {
    let directory = result_journal_directory(store, true)?;
    let path = directory.join("abandoned.audit");
    let timestamp = now_unix_ms().map_err(operation)?;
    let line = format!(
        "{} {} {} {} {} {} {} {} {}\n",
        timestamp,
        phase,
        hex(&result.scope.namespace_id),
        hex(&result.scope.work_id),
        result.scope.attempt,
        hex(&result.scope.fence),
        hex(result.closure.as_bytes()),
        hex(result.input_closure_id.as_slice()),
        hex(result.input_manifest_object_id.as_slice()),
    );
    let mut existing = match backend_platform::durable::open_private_read(&path) {
        Ok(mut file) => {
            let metadata = file.metadata().map_err(operation)?;
            if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            file.by_ref()
                .take((16 * 1024 * 1024 + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(operation)?;
            if bytes.len() > 16 * 1024 * 1024 {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            bytes
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(operation(error)),
    };
    if existing
        .len()
        .checked_add(line.len())
        .is_none_or(|length| length > 16 * 1024 * 1024)
    {
        return Err(ClusterWorkerError::InputBounds);
    }
    existing
        .try_reserve_exact(line.len())
        .map_err(|_| ClusterWorkerError::InputBounds)?;
    existing.extend_from_slice(line.as_bytes());
    backend_platform::durable::write_private_atomic(&path, &existing).map_err(operation)
}

fn recover_pending_results(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
) -> Result<Vec<ClusterWorkerResult>, ClusterWorkerError> {
    read_pending_result_records(store, policy)?
        .into_iter()
        .map(|record| reopen_pending_result(store, policy, record))
        .collect()
}

fn read_pending_result_records(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
) -> Result<Vec<PendingResultRecord>, ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(operation(error)),
    };
    let mut records = Vec::new();
    for entry in entries {
        let entry = entry.map_err(operation)?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("pending") {
            continue;
        }
        if records.len() >= MAX_PENDING_RESULTS {
            return Err(ClusterWorkerError::InputBounds);
        }
        let metadata = fs::symlink_metadata(&path).map_err(operation)?;
        validate_result_record_metadata(&metadata)?;
        let mut file = open_private_read_file(&path, RESULT_JOURNAL_BYTES)?;
        let mut bytes = Vec::with_capacity(RESULT_JOURNAL_BYTES);
        file.by_ref()
            .take((RESULT_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(operation)?;
        let record = PendingResultRecord::decode(&bytes)?;
        let expected_path = result_journal_path(&directory, record.scope, "pending");
        if expected_path != path
            || record.scope.namespace_id != policy.namespace_id
            || record.object_count as usize > policy.max_input_objects
            || record.payload_bytes > policy.max_output_bytes
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        records.push(record);
    }
    records.sort_by_key(|record| {
        (
            record.scope.namespace_id,
            record.scope.work_id,
            record.scope.attempt,
            record.scope.fence,
        )
    });
    Ok(records)
}

fn read_retired_result_records(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
) -> Result<Vec<RetiredResultRecord>, ClusterWorkerError> {
    let directory = result_journal_directory(store, false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(operation(error)),
    };
    let mut records = Vec::new();
    let mut scopes = std::collections::BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(operation)?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("retired") {
            continue;
        }
        if records.len() >= MAX_RETIRED_RESULTS {
            return Err(ClusterWorkerError::RetirementQuota);
        }
        let metadata = fs::symlink_metadata(&path).map_err(operation)?;
        validate_journal_metadata(&metadata, RETIRED_JOURNAL_BYTES)?;
        let mut file = open_private_read_file(&path, RETIRED_JOURNAL_BYTES)?;
        let mut bytes = Vec::with_capacity(RETIRED_JOURNAL_BYTES);
        file.by_ref()
            .take((RETIRED_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(operation)?;
        let record = RetiredResultRecord::decode(&bytes)?;
        if result_journal_path(&directory, record.scope, "retired") != path
            || record.scope.namespace_id != policy.namespace_id
            || !scopes.insert((
                record.scope.namespace_id,
                record.scope.work_id,
                record.scope.attempt,
                record.scope.fence,
            ))
        {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        records.push(record);
    }
    records.sort_by_key(|record| {
        (
            record.scope.namespace_id,
            record.scope.work_id,
            record.scope.attempt,
            record.scope.fence,
        )
    });
    Ok(records)
}

fn reopen_pending_result(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    record: PendingResultRecord,
) -> Result<ClusterWorkerResult, ClusterWorkerError> {
    let coordinator = EndpointId::from_bytes(&record.coordinator).map_err(operation)?;
    let receipt = reopen_pending_closure(store, policy, record)?;
    let closure_id = receipt.closure();
    let envelope_index = reopen_compiler_result_envelope(
        store,
        closure_id,
        record.object_count,
        record.payload_bytes,
        record.scope,
        record.input_closure_id,
        record.input_manifest_object_id,
        record.target_root,
        policy.max_output_bytes,
    )
    .map_err(operation)?;
    if envelope_index.closure_id() != closure_id
        || envelope_index.object_count() != record.object_count
        || envelope_index.payload_bytes() != record.payload_bytes
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(ClusterWorkerResult {
        scope: record.scope,
        coordinator,
        input_closure_id: record.input_closure_id,
        input_manifest_object_id: record.input_manifest_object_id,
        deadline_unix_ms: record.deadline_unix_ms,
        receipt,
        closure: closure_id,
        pack_id: record.pack_id,
        target_root: record.target_root,
        payload_bytes: record.payload_bytes,
        object_count: record.object_count,
        store: store.clone(),
    })
}

fn reopen_pending_closure(
    store: &FileStore,
    policy: &ClusterWorkerPolicy,
    record: PendingResultRecord,
) -> Result<StoredClosureReceipt, ClusterWorkerError> {
    let budget = ArtifactBudget::new(
        record.object_count as usize,
        policy.max_input_objects,
        policy.max_output_bytes,
        ARTIFACT_PUT_CHUNK_BYTES,
        put_call_budget(policy.max_output_bytes, policy.max_input_objects)?,
    );
    let receipt = store
        .reopen_stored_closure(ArtifactClosureClaim::from_bytes(record.closure_id), budget)
        .map_err(operation)?;
    let closure_id = receipt.closure();
    if receipt.closure() != closure_id
        || receipt.object_count() != u64::from(record.object_count)
        || receipt.bytes_verified() != record.payload_bytes
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    Ok(receipt)
}

fn result_journal_directory(
    store: &FileStore,
    create: bool,
) -> Result<PathBuf, ClusterWorkerError> {
    let path = store.root().join("worker-results");
    match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            validate_result_directory(&metadata)?;
            #[cfg(windows)]
            backend_platform::win32::security::restrict_to_current_user(&path)
                .map_err(operation)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {
            match fs::create_dir(&path) {
                Ok(()) => {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt as _;
                        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                            .map_err(operation)?;
                    }
                    #[cfg(windows)]
                    backend_platform::win32::security::restrict_to_current_user(&path)
                        .map_err(operation)?;
                    let metadata = fs::symlink_metadata(&path).map_err(operation)?;
                    validate_result_directory(&metadata)?;
                    backend_platform::durable::sync_parent(&path).map_err(operation)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let metadata = fs::symlink_metadata(&path).map_err(operation)?;
                    validate_result_directory(&metadata)?;
                    #[cfg(windows)]
                    backend_platform::win32::security::restrict_to_current_user(&path)
                        .map_err(operation)?;
                }
                Err(error) => return Err(operation(error)),
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(path),
        Err(error) => return Err(operation(error)),
    }
    Ok(path)
}

fn result_journal_path(directory: &Path, scope: AssignmentScope, extension: &str) -> PathBuf {
    let mut hasher = backend_engine::blake3::Hasher::new();
    hasher.update(b"backend.worker.pending-result-name.v1\0");
    hasher.update(&scope.namespace_id);
    hasher.update(&scope.work_id);
    hasher.update(&scope.attempt.to_be_bytes());
    hasher.update(&scope.fence);
    directory.join(format!(
        "{}.{}",
        hex(hasher.finalize().as_bytes()),
        extension
    ))
}

/// NoResult uses a canonical full-scope filename instead of a digest so its disk index cannot
/// report a false-positive match from a filename-hash collision.
fn no_result_journal_path(directory: &Path, scope: AssignmentScope) -> PathBuf {
    directory.join(format!(
        "{}-{}-{}-{}.noresult",
        hex(&scope.namespace_id),
        hex(&scope.work_id),
        scope.attempt,
        hex(&scope.fence),
    ))
}

fn no_result_floor_path(
    directory: &Path,
    namespace_id: [u8; 16],
    coordinator: EndpointId,
) -> PathBuf {
    directory.join(format!(
        "{}-{}.nrfloor",
        hex(&namespace_id),
        hex(coordinator.as_bytes()),
    ))
}

fn validate_result_record_metadata(metadata: &fs::Metadata) -> Result<(), ClusterWorkerError> {
    validate_journal_metadata(metadata, RESULT_JOURNAL_BYTES)
}

fn open_private_read_file(path: &Path, expected_bytes: usize) -> Result<File, ClusterWorkerError> {
    let file = backend_platform::durable::open_private_read(path).map_err(operation)?;
    validate_journal_metadata(&file.metadata().map_err(operation)?, expected_bytes)?;
    Ok(file)
}

fn validate_journal_metadata(
    metadata: &fs::Metadata,
    expected_bytes: usize,
) -> Result<(), ClusterWorkerError> {
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != expected_bytes as u64
    {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 || metadata.nlink() != 1 {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
    }
    #[cfg(not(unix))]
    {
        #[cfg(windows)]
        if backend_platform::win32::security::is_endpoint_metadata(metadata) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        #[cfg(not(windows))]
        return Err(ClusterWorkerError::InvalidConfig);
    }
    Ok(())
}

fn validate_result_directory(metadata: &fs::Metadata) -> Result<(), ClusterWorkerError> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ClusterWorkerError::InvalidInputClosure);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
    }
    #[cfg(not(unix))]
    {
        #[cfg(windows)]
        if backend_platform::win32::security::is_endpoint_metadata(metadata) {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        #[cfg(not(windows))]
        return Err(ClusterWorkerError::InvalidConfig);
    }
    Ok(())
}

fn result_capability_nonce(
    scope: AssignmentScope,
    object_id: [u8; 32],
    range: ChunkRange,
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cluster.compiler-result-grant.v1\0");
    hasher.update(&scope.namespace_id);
    hasher.update(&scope.work_id);
    hasher.update(&scope.attempt.to_be_bytes());
    hasher.update(&scope.fence);
    hasher.update(&object_id);
    hasher.update(&range.start.to_be_bytes());
    hasher.update(&range.end.to_be_bytes());
    let digest = hasher.finalize();
    let mut nonce = [0; 16];
    nonce.copy_from_slice(&digest.as_bytes()[..16]);
    if nonce == [0; 16] {
        nonce[0] = 1;
    }
    nonce
}

#[derive(Debug)]
struct PlannedObject {
    mapping: StoreObjectMapping,
    capabilities: Vec<Capability>,
}

fn flatten_and_validate_pages(
    pages: &[ControlGrantPage],
    assignment_scope: AssignmentScope,
    transfer_scope: TransferScope,
    worker: EndpointId,
    coordinator: EndpointId,
    max_capabilities: usize,
) -> Result<Vec<Capability>, ClusterWorkerError> {
    if pages.is_empty()
        || pages.len() > MAX_CONTROL_GRANT_PAGES as usize
        || pages.len() != pages.first().map_or(0, |page| page.page_count as usize)
        || pages.iter().enumerate().any(|(index, page)| {
            page.scope != assignment_scope
                || page.direction != GrantDirection::InputsToWorker
                || page.page_index != index as u32
                || page.page_count as usize != pages.len()
                || page.grants.is_empty()
                || page.grants.len() > MAX_OFFER_CAPABILITIES
        })
    {
        return Err(ClusterWorkerError::InvalidGrantPages);
    }
    let capability_count = pages
        .iter()
        .try_fold(0_usize, |total, page| total.checked_add(page.grants.len()));
    let capability_count = capability_count.ok_or(ClusterWorkerError::InputBounds)?;
    if capability_count > max_capabilities {
        return Err(ClusterWorkerError::InputBounds);
    }
    let now = now_unix_ms().map_err(operation)?;
    let policy = AdmissionPolicy::new(coordinator, coordinator, [worker], transfer_scope);
    let mut capabilities = Vec::with_capacity(capability_count);
    for page in pages {
        for capability in &page.grants {
            if capability.claims.scope != transfer_scope
                || capability.claims.client != worker
                || capability.claims.server != coordinator
            {
                return Err(ClusterWorkerError::ScopeMismatch);
            }
            verify_admission(&policy, worker, capability, capability.claims.range, now)
                .map_err(|_| ClusterWorkerError::ScopeMismatch)?;
            capabilities.push(capability.clone());
        }
    }
    Ok(capabilities)
}

fn plan_input_objects(
    mut capabilities: Vec<Capability>,
    worker_policy: &ClusterWorkerPolicy,
    coordinator: EndpointId,
    worker: EndpointId,
    transfer_scope: TransferScope,
) -> Result<Vec<PlannedObject>, ClusterWorkerError> {
    let mut groups = BTreeMap::<[u8; 32], (StoreObjectMapping, Vec<Capability>)>::new();
    for capability in capabilities.drain(..) {
        let claims = &capability.claims;
        if claims.client != worker
            || claims.server != coordinator
            || claims.scope != transfer_scope
            || claims.object.payload_length > worker_policy.max_input_bytes
        {
            return Err(ClusterWorkerError::ScopeMismatch);
        }
        let id = claims.object.object_id;
        let entry = groups
            .entry(id)
            .or_insert_with(|| (claims.object, Vec::new()));
        if entry.0 != claims.object {
            return Err(ClusterWorkerError::InvalidInputClosure);
        }
        entry.1.push(capability);
    }
    if groups.len() > worker_policy.max_input_objects {
        return Err(ClusterWorkerError::InputBounds);
    }
    let mut objects = groups
        .into_values()
        .map(|(mapping, mut capabilities)| {
            capabilities.sort_by_key(|capability| capability.claims.range.start);
            let total_chunks = mapping
                .payload_length
                .checked_add(BYTES_PER_BAO_CHUNK - 1)
                .ok_or(ClusterWorkerError::InputBounds)?
                / BYTES_PER_BAO_CHUNK;
            let mut next_chunk = 0_u64;
            for capability in &capabilities {
                let range = capability.claims.range;
                if range.start != next_chunk || range.end > total_chunks {
                    return Err(ClusterWorkerError::InvalidInputClosure);
                }
                range
                    .validate(mapping.payload_length)
                    .map_err(|_| ClusterWorkerError::InvalidInputClosure)?;
                next_chunk = range.end;
            }
            if next_chunk != total_chunks {
                return Err(ClusterWorkerError::InvalidInputClosure);
            }
            Ok(PlannedObject {
                mapping,
                capabilities,
            })
        })
        .collect::<Result<Vec<_>, ClusterWorkerError>>()?;
    objects.sort_by_key(|object| {
        (
            object.mapping.schema_domain,
            object.mapping.schema_type,
            object.mapping.schema_version,
            object.mapping.key,
            object.mapping.version,
        )
    });
    Ok(objects)
}

fn put_call_budget(
    max_payload_bytes: u64,
    object_count: usize,
) -> Result<usize, ClusterWorkerError> {
    let payload_chunks = max_payload_bytes
        .checked_add(ARTIFACT_PUT_CHUNK_BYTES as u64 - 1)
        .ok_or(ClusterWorkerError::InputBounds)?
        / ARTIFACT_PUT_CHUNK_BYTES as u64;
    usize::try_from(payload_chunks)
        .ok()
        .and_then(|chunks| chunks.checked_add(object_count))
        .and_then(|calls| calls.checked_add(1))
        .ok_or(ClusterWorkerError::InputBounds)
}

fn checkpoint_path_root(root: &Path, scope: TransferScope) -> PathBuf {
    let name = format!(
        "{}-{}-{}-{}-{}",
        hex(&scope.namespace_id),
        hex(&scope.work_id),
        scope.attempt,
        hex(&scope.fence),
        hex(&scope.closure_id),
    );
    root.join(name)
}

fn ensure_private_directory(path: &Path) -> Result<(), ClusterWorkerError> {
    #[cfg(windows)]
    {
        for component in path.ancestors() {
            match fs::symlink_metadata(component) {
                Ok(metadata)
                    if metadata.file_type().is_symlink()
                        || backend_platform::win32::security::is_endpoint_metadata(&metadata) =>
                {
                    return Err(ClusterWorkerError::InvalidConfig);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(operation(error)),
            }
        }
    }
    std::fs::create_dir_all(path).map_err(operation)?;
    let metadata = std::fs::symlink_metadata(path).map_err(operation)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ClusterWorkerError::InvalidConfig);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(path, permissions).map_err(operation)?;
    }
    #[cfg(windows)]
    {
        if backend_platform::win32::security::is_endpoint_metadata(&metadata) {
            return Err(ClusterWorkerError::InvalidConfig);
        }
        backend_platform::win32::security::restrict_to_current_user(path).map_err(operation)?;
        let checked = std::fs::symlink_metadata(path).map_err(operation)?;
        if !checked.is_dir()
            || checked.file_type().is_symlink()
            || backend_platform::win32::security::is_endpoint_metadata(&checked)
        {
            return Err(ClusterWorkerError::InvalidConfig);
        }
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn scope_is_valid(scope: AssignmentScope) -> bool {
    scope.namespace_id != [0; 16]
        && scope.work_id != [0; 16]
        && scope.attempt != 0
        && scope.fence != [0; 32]
}

fn verified_have_bitmap(store: &FileStore, claims: &[ProbeObjectClaim]) -> Vec<u8> {
    let mut bitmap = vec![0_u8; claims.len().div_ceil(8)];
    for (index, claim) in claims.iter().enumerate() {
        let untrusted = UntrustedObjectId::from_bytes(claim.object_id);
        let present = store.verify_object_claim(untrusted).is_ok_and(|envelope| {
            envelope.id().as_bytes() == &claim.object_id
                && envelope.payload_len() == u64::from(claim.payload_bytes)
        });
        if present {
            bitmap[index / 8] |= 1 << (index % 8);
        }
    }
    bitmap
}

fn worker_incarnation(endpoint: EndpointId) -> Result<[u8; 32], ClusterWorkerError> {
    let now = now_unix_ms().map_err(operation)?;
    let counter = NEXT_WORKER_INCARNATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.compiler.cluster-worker.incarnation.v1\0");
    hasher.update(endpoint.as_bytes());
    hasher.update(&now.to_be_bytes());
    hasher.update(&std::process::id().to_be_bytes());
    hasher.update(&counter.to_be_bytes());
    let incarnation = *hasher.finalize().as_bytes();
    if incarnation == [0; 32] {
        return Err(ClusterWorkerError::InvalidConfig);
    }
    Ok(incarnation)
}

fn probe_capacity_revision(
    incarnation: [u8; 32],
    session: &backend_engine::cluster_transport::CompilerProbeSession,
    busy: ProbeResourceCredits,
    capability: ProbeCapability,
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.compiler.cluster-worker.capacity.v1\0");
    hasher.update(&incarnation);
    hasher.update(&session.scope.namespace_id);
    hasher.update(&session.scope.work_id);
    hasher.update(&session.scope.attempt.to_be_bytes());
    hasher.update(&session.scope.fence);
    hasher.update(&session.nonce);
    hasher.update(&busy.cpu_millicores.to_be_bytes());
    hasher.update(&busy.memory_bytes.to_be_bytes());
    hasher.update(&busy.transfer_bytes.to_be_bytes());
    match capability {
        ProbeCapability::Supported { capability_digest } => {
            hasher.update(&[1]);
            hasher.update(&capability_digest);
        }
        ProbeCapability::Rejected(reason) => {
            hasher.update(&[0, reason as u8]);
        }
    }
    let digest = hasher.finalize();
    let mut revision = [0_u8; 16];
    revision.copy_from_slice(&digest.as_bytes()[..16]);
    if revision == [0; 16] {
        revision[15] = 1;
    }
    revision
}

fn operation(error: impl std::fmt::Debug) -> ClusterWorkerError {
    ClusterWorkerError::Operation(format!("{error:?}"))
}

fn probe_demand_credits(demand: CompilerProbeDemand) -> ProbeResourceCredits {
    ProbeResourceCredits {
        cpu_millicores: demand.cpu_millicores,
        memory_bytes: demand.memory_bytes,
        transfer_bytes: demand.transfer_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::application::PackageUrl;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct GcTestObjectSchema;

    impl backend_engine::Schema for GcTestObjectSchema {
        const DOMAIN: u8 = 0xf0;
        const TYPE: u16 = 1;
        type Value = [u8];

        fn encode(value: &Self::Value, output: &mut Vec<u8>) {
            output.extend_from_slice(value);
        }
    }

    #[test]
    fn manifest_target_cannot_be_swapped_to_another_valid_package() {
        let first =
            PackageUrl::parse("pkg:cargo/first@1.0.0".to_owned()).expect("first canonical package");
        let second = PackageUrl::parse("pkg:cargo/second@1.0.0".to_owned())
            .expect("second canonical package");
        let first_target = CompilerPackageTargetV2::for_package(first.clone());
        let second_target = CompilerPackageTargetV2::for_package(second);
        assert_eq!(first_target.package().identity, first_target.target());
        assert_ne!(first_target.target(), second_target.target());
    }

    #[test]
    fn changed_local_authority_or_toolchain_fails_exact_execution_admission() {
        let grant = WorkerExecutionGrant {
            coordinator: SecretKey::generate().public(),
            namespace_id: [1; 16],
            recipe: [2; 32],
            profile: [3, 4],
            stage: 1,
            toolchain: [5; 32],
            environment: [6; 32],
            target_platform: [7; 32],
            local_authority_fingerprint: [8; 32],
            class: WorkerExecutionClass::TrustedCoordinatorHostExecution,
        };
        let facts = CompilerExecutionFacts {
            target: [9; 32],
            profile: grant.profile,
            stage: grant.stage,
            recipe: grant.recipe,
            toolchain: grant.toolchain,
            local_authority_fingerprint: grant.local_authority_fingerprint,
            environment: grant.environment,
            target_platform: grant.target_platform,
        };
        let matches = |actual| {
            compiler_execution_facts_match(
                actual,
                facts.target,
                facts.profile,
                facts.stage,
                grant.recipe,
                grant.toolchain,
                grant.environment,
                grant.target_platform,
                grant,
            )
        };
        assert!(matches(facts));
        assert!(!matches(CompilerExecutionFacts {
            local_authority_fingerprint: [10; 32],
            ..facts
        }));
        assert!(!matches(CompilerExecutionFacts {
            toolchain: [11; 32],
            ..facts
        }));
        assert!(!matches(CompilerExecutionFacts {
            environment: [12; 32],
            ..facts
        }));
        assert!(!matches(CompilerExecutionFacts {
            target_platform: [13; 32],
            ..facts
        }));
    }

    #[test]
    fn retirement_tombstone_recovers_each_durable_unlink_crash_cut() {
        for cut in 0..4 {
            let store_root = temporary_store_path(&format!("retirement-cut-{cut}"));
            let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
            let scope = test_scope();
            let coordinator = [4; 32];
            let running = RunningAssignmentRecord {
                coordinator,
                scope,
                input_closure_id: [5; 32],
                input_manifest_object_id: [6; 32],
                deadline_unix_ms: 10,
            };
            let pending = PendingResultRecord {
                coordinator,
                scope,
                input_closure_id: [5; 32],
                input_manifest_object_id: [6; 32],
                deadline_unix_ms: 20,
                closure_id: [7; 32],
                pack_id: None,
                target_root: [9; 32],
                payload_bytes: 10,
                object_count: 1,
            };
            persist_journal_record(&store, scope, "running", &running.encode())
                .expect("persist running marker");
            persist_journal_record(&store, scope, "pending", &pending.encode())
                .expect("persist pending result");
            let retired = RetiredResultRecord {
                coordinator,
                scope,
                closure_id: pending.closure_id,
                disposition: ResultAckDisposition::Stored,
            };
            persist_retired_result_record(&store, retired).expect("fsync retirement tombstone");

            // Inject process death after each durable phase: tombstone only, running unlink,
            // pending unlink, and tombstone unlink immediately before Applied is sent.
            if cut >= 1 {
                remove_running_assignment(&store, scope).expect("unlink running row");
            }
            if cut >= 2 {
                remove_pending_result_for_scope(&store, scope).expect("unlink pending row");
            }
            if cut >= 3 {
                apply_retired_result_record(&store, retired).expect("unlink tombstone last");
            }
            drop(store);

            let reopened = FileStore::open(&store_root, 8_192).expect("cold reopen worker store");
            if cut < 3 {
                let recovered = read_retired_result_record(&reopened, scope)
                    .expect("read cold retirement tombstone")
                    .expect("tombstone remains before Applied");
                assert_eq!(recovered, retired);
                retire_result_payload_state(&reopened, recovered)
                    .expect("cold recovery idempotently completes payload cleanup");
                assert!(
                    result_journal_path(
                        &result_journal_directory(&reopened, false).expect("journal directory"),
                        scope,
                        "retired"
                    )
                    .exists()
                );
            } else {
                assert!(
                    has_no_live_retirement_conflict(&reopened, &test_policy(), scope)
                        .expect("check final no-state duplicate-confirm cut")
                );
            }
            let _ = fs::remove_dir_all(store_root);
        }
    }

    #[test]
    fn retirement_confirmation_binds_owner_scope_closure_and_disposition() {
        let worker = SecretKey::generate().public();
        let coordinator = SecretKey::generate().public();
        let record = RetiredResultRecord {
            coordinator: *coordinator.as_bytes(),
            scope: test_scope(),
            closure_id: [17; 32],
            disposition: ResultAckDisposition::Rejected(ResultRejectReason::Scope),
        };
        let retired = record.receipt(worker).expect("build exact worker receipt");
        let mut corrupted_record = record.encode();
        let last = corrupted_record.len() - 1;
        corrupted_record[last] ^= 1;
        assert!(matches!(
            RetiredResultRecord::decode(&corrupted_record),
            Err(ClusterWorkerError::InvalidInputClosure)
        ));
        let confirm = ControlResultRetirementConfirm::new(
            record.scope,
            record.closure_id,
            record.disposition,
            coordinator,
        )
        .expect("construct exact owner confirmation");
        assert!(record.matches_confirm(&confirm, coordinator));
        assert!(confirm.matches_retired(&retired, coordinator));

        assert!(!record.matches_confirm(
            &ControlResultRetirementConfirm {
                closure_id: [18; 32],
                ..confirm
            },
            coordinator
        ));
        assert!(!record.matches_confirm(
            &ControlResultRetirementConfirm {
                disposition: ResultAckDisposition::Stored,
                ..confirm
            },
            coordinator
        ));
        let other_owner = SecretKey::generate().public();
        assert!(!record.matches_confirm(&confirm, other_owner));
        let empty_root = temporary_store_path("empty-retirement-state");
        let empty_store = FileStore::open(&empty_root, 8_192).expect("open empty worker store");
        assert!(
            has_no_live_retirement_conflict(&empty_store, &test_policy(), record.scope,)
                .expect("empty state is safe for duplicate Applied")
        );
        let pending = PendingResultRecord {
            coordinator: *coordinator.as_bytes(),
            scope: record.scope,
            input_closure_id: [21; 32],
            input_manifest_object_id: [22; 32],
            deadline_unix_ms: 30,
            closure_id: record.closure_id,
            pack_id: None,
            target_root: [23; 32],
            payload_bytes: 24,
            object_count: 1,
        };
        persist_journal_record(&empty_store, record.scope, "pending", &pending.encode())
            .expect("write same-scope live result fixture");
        assert!(
            !has_no_live_retirement_conflict(&empty_store, &test_policy(), record.scope,)
                .expect("inspect same-scope pending result")
        );
        remove_pending_result_for_scope(&empty_store, record.scope)
            .expect("remove pending result fixture");
        let conflicting = RetiredResultRecord {
            closure_id: [25; 32],
            ..record
        };
        persist_retired_result_record(&empty_store, conflicting)
            .expect("write conflicting tombstone fixture");
        assert!(
            !has_no_live_retirement_conflict(&empty_store, &test_policy(), record.scope,)
                .expect("inspect conflicting tombstone")
        );
        drop(empty_store);
        let _ = fs::remove_dir_all(empty_root);
    }

    #[tokio::test]
    async fn cold_restart_reports_orphaned_running_work_to_exact_coordinator() {
        let store_root = temporary_store_path("restart-running");
        let namespace_id = [1; 16];
        let coordinator_endpoint = bind_direct(
            SecretKey::generate(),
            "127.0.0.1:0".parse().expect("loopback address"),
        )
        .await
        .expect("bind test coordinator endpoint");
        let coordinator = ClusterCoordinator {
            identity: coordinator_endpoint.id(),
            address: coordinator_endpoint.addr(),
        };
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let scope = AssignmentScope::new(namespace_id, [2; 16], 3, [4; 32])
            .expect("valid assignment scope");
        let record = RunningAssignmentRecord {
            coordinator: *coordinator.identity.as_bytes(),
            scope,
            input_closure_id: [5; 32],
            input_manifest_object_id: [6; 32],
            deadline_unix_ms: 1,
        };
        persist_journal_record(&store, scope, "running", &record.encode())
            .expect("persist pre-crash assignment marker");
        drop(store);

        let worker = ClusterWorker::bind(ClusterWorkerConfig {
            bind_address: "127.0.0.1:0".parse().expect("worker loopback address"),
            identity: SecretKey::generate(),
            coordinators: vec![coordinator],
            policy: ClusterWorkerPolicy {
                namespace_id,
                accepted_recipes: vec![[7; 32]],
                execution_policy: ClusterExecutionPolicy::DenyUnconfined,
                max_output_bytes: 1024,
                cpu_millicores: DEFAULT_CPU_MILLICORES,
                memory_bytes: DEFAULT_MEMORY_CREDITS_BYTES,
                max_input_objects: 16,
                max_input_bytes: 4096,
                max_capabilities: 32,
                max_grant_pages: 4,
                io_timeout: Duration::from_secs(3),
            },
        })
        .await
        .expect("cold-bind worker from persisted journal state");
        let worker_id = worker.identity();
        let owner_listener = coordinator_endpoint.clone();
        let owner_task = tokio::spawn(async move {
            let admission = ControlAdmissionPolicy::new(
                owner_listener.id(),
                [worker_id],
                scope,
                ControlRole::Coordinator,
            );
            let mut channel = tokio::time::timeout(
                Duration::from_secs(3),
                accept_control(&owner_listener, &admission),
            )
            .await
            .map_err(|_| "coordinator control accept timed out")?
            .map_err(|error| format!("coordinator control admission failed: {error}"))?;
            let message = channel
                .receive()
                .await
                .map_err(|error| format!("execution failure receive failed: {error}"))?;
            let ControlMessage::ExecutionFailed(failure) = message else {
                return Err("worker sent the wrong cold-recovery terminal".to_owned());
            };
            if failure.scope != scope
                || failure.input_closure_id != [5; 32]
                || failure.reason != ExecutionFailureReason::Deadline
            {
                return Err("cold-recovery terminal did not retain its exact fence".to_owned());
            }
            channel
                .finish()
                .await
                .map_err(|error| format!("coordinator close handshake failed: {error}"))?;
            Ok::<(), String>(())
        });

        // Reopening the store models a cold process boundary: only the journal file and CAS
        // state survive; the worker endpoint is newly bound above.
        let reopened_store = FileStore::open(&store_root, 8_192).expect("reopen worker FileStore");
        let pending = worker
            .recover_after_restart(&reopened_store)
            .await
            .expect("report interrupted work");
        assert!(pending.is_empty());
        owner_task
            .await
            .expect("join coordinator process task")
            .expect("coordinator validates exact terminal");
        let directory = result_journal_directory(&reopened_store, false)
            .expect("result journal directory exists");
        assert!(!result_journal_path(&directory, scope, "running").exists());
        let _ = fs::remove_dir_all(store_root);
    }

    #[test]
    fn pending_result_quota_refuses_an_extra_row_without_mutating_existing_rows() {
        let store_root = temporary_store_path("pending-quota");
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let directory = result_journal_directory(&store, true).expect("create journal directory");
        for index in 0..MAX_PENDING_RESULTS {
            fs::write(directory.join(format!("row-{index}.pending")), b"row")
                .expect("write counted row");
        }
        assert!(matches!(
            ensure_pending_result_capacity(&store),
            Err(ClusterWorkerError::PendingResultQuota)
        ));
        let retained = fs::read_dir(&directory)
            .expect("read result directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|ext| ext.to_str()) == Some("pending")
            })
            .count();
        assert_eq!(retained, MAX_PENDING_RESULTS);
        drop(store);

        let reopened = FileStore::open(&store_root, 8_192).expect("cold reopen worker FileStore");
        assert!(matches!(
            ensure_pending_result_capacity(&reopened),
            Err(ClusterWorkerError::PendingResultQuota)
        ));
        let reopened_count = fs::read_dir(&directory)
            .expect("read cold-reopened result directory")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.path().extension().and_then(|ext| ext.to_str()) == Some("pending")
            })
            .count();
        assert_eq!(reopened_count, MAX_PENDING_RESULTS);
        let _ = fs::remove_dir_all(store_root);
    }

    #[test]
    fn terminal_tombstone_quota_is_separate_from_pending_payload_quota() {
        let store_root = temporary_store_path("retired-quota");
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let directory = result_journal_directory(&store, true).expect("create journal directory");
        for index in 0..MAX_RETIRED_RESULTS {
            fs::write(
                directory.join(format!("terminal-{index}.retired")),
                b"receipt",
            )
            .expect("write counted terminal row");
        }
        assert!(ensure_pending_result_capacity(&store).is_ok());
        assert!(matches!(
            ensure_retired_result_capacity(&store),
            Err(ClusterWorkerError::RetirementQuota)
        ));
        let _ = fs::remove_dir_all(store_root);
    }

    #[test]
    fn cold_reopen_rejects_a_truncated_private_pending_journal() {
        let store_root = temporary_store_path("truncated-pending");
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let scope = test_scope();
        let record = PendingResultRecord {
            coordinator: [4; 32],
            scope,
            input_closure_id: [5; 32],
            input_manifest_object_id: [6; 32],
            deadline_unix_ms: 20,
            closure_id: [7; 32],
            pack_id: None,
            target_root: [9; 32],
            payload_bytes: 10,
            object_count: 1,
        };
        persist_journal_record(&store, scope, "pending", &record.encode())
            .expect("persist full pending journal");
        let directory = result_journal_directory(&store, false).expect("journal directory");
        let path = result_journal_path(&directory, scope, "pending");
        let mut truncated = record.encode();
        truncated.pop();
        backend_platform::durable::write_private_atomic(&path, &truncated)
            .expect("durably publish truncated fixture");
        drop(store);

        let reopened = FileStore::open(&store_root, 8_192).expect("cold reopen worker store");
        assert!(matches!(
            read_pending_result_records(&reopened, &test_policy()),
            Err(ClusterWorkerError::InvalidInputClosure)
        ));

        let mut corrupted = record.encode();
        let last = corrupted.len() - 1;
        corrupted[last] ^= 1;
        backend_platform::durable::write_private_atomic(&path, &corrupted)
            .expect("durably publish checksum-corrupted fixture");
        assert!(matches!(
            read_pending_result_records(&reopened, &test_policy()),
            Err(ClusterWorkerError::InvalidInputClosure)
        ));
        let _ = fs::remove_dir_all(store_root);
    }

    #[test]
    fn recovery_latency_summary_has_fixed_memory_for_indefinite_replay() {
        let mut histogram = LatencyHistogram::default();
        for latency in 0..1_000_000_u64 {
            histogram.record(latency % 257);
        }
        assert_eq!(histogram.count, 1_000_000);
        assert!(histogram.percentile_upper_bound(50) >= 128);
        // The exact 95th percentile is 244 ms; its logarithmic bucket is
        // [128, 255], so 255 is the conservative reported bound.
        assert_eq!(histogram.percentile_upper_bound(95), 255);
        assert_eq!(
            core::mem::size_of_val(&histogram.buckets),
            64 * core::mem::size_of::<u64>()
        );
    }

    #[test]
    fn no_result_tombstone_survives_cold_reopen_and_fences_only_its_exact_offer() {
        let store_root = temporary_store_path("no-result-fence");
        let coordinator = SecretKey::generate().public();
        let scope = test_scope();
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let record = NoResultRecord::new(coordinator, scope);
        persist_no_result_record(&store, &test_policy(), record)
            .expect("persist no-result fence before replying");
        assert!(
            scope_has_recovery_record(&store, &test_policy(), scope)
                .expect("exact offered scope is fenced")
        );
        let other = AssignmentScope::new(scope.namespace_id, [33; 16], scope.attempt, scope.fence)
            .expect("different work in same namespace");
        assert!(
            !scope_has_recovery_record(&store, &test_policy(), other)
                .expect("fence does not block a different scope")
        );
        drop(store);

        let reopened = FileStore::open(&store_root, 8_192).expect("cold reopen worker store");
        assert_eq!(
            read_no_result_record(&reopened, scope).expect("read fence"),
            Some(record)
        );
        assert!(
            scope_has_recovery_record(&reopened, &test_policy(), scope)
                .expect("cold no-result row still blocks the delayed Offer")
        );
        let query = ControlResultRecoveryQuery::new(scope, coordinator)
            .expect("construct exact authenticated recovery query");
        assert!(record.matches_query(&query, coordinator));
        let mutated_scope =
            AssignmentScope::new(scope.namespace_id, [34; 16], scope.attempt, scope.fence)
                .expect("mutated work scope");
        let mutated = ControlResultRecoveryQuery::new(mutated_scope, coordinator)
            .expect("construct mutated recovery query");
        assert!(!record.matches_query(&mutated, coordinator));
        let other_owner = SecretKey::generate().public();
        assert!(!record.matches_query(&query, other_owner));
        assert_eq!(
            read_no_result_record(&reopened, scope).expect("fence remains"),
            Some(record)
        );
        let _ = fs::remove_dir_all(store_root);
    }

    #[tokio::test]
    async fn namespace_epoch_floor_compacts_two_work_history_and_blocks_late_offer_after_restart() {
        use backend_extension_turso::{
            AuthorityNamespace, SourceObservation, SourceObservationValue, TursoAuthority,
        };

        let store_root = temporary_store_path("no-result-floor-restart");
        let authority_path = temporary_store_path("no-result-floor-authority");
        let namespace = AuthorityNamespace::package_metadata(
            "pkg:cargo/widget",
            "registry:crates-io",
            "main",
            "stable",
        )
        .expect("typed authority namespace");
        let mut authority = TursoAuthority::open(&authority_path)
            .await
            .expect("open authority database");
        let observation = SourceObservation::new(
            namespace.clone(),
            Some([0x51; 32]),
            10,
            SourceObservationValue::KnownCount(2),
        )
        .expect("exact namespace source observation");
        let observation_receipt = authority
            .record_source_observation(observation)
            .await
            .expect("persist source observation");
        let work_a = authority
            .begin_attempt(&namespace, [0xA1; 32], &observation_receipt)
            .await
            .expect("mint first work attempt");
        let work_b = authority
            .begin_attempt(&namespace, [0xB2; 32], &observation_receipt)
            .await
            .expect("mint second, different work attempt in same namespace");
        let floor_anchor = authority
            .begin_attempt(&namespace, [0xC3; 32], &observation_receipt)
            .await
            .expect("mint newer namespace attempt as floor anchor");
        assert_eq!(work_a.scheduler_attempt_id(), 1);
        assert_eq!(work_b.scheduler_attempt_id(), 2);
        assert_eq!(floor_anchor.scheduler_attempt_id(), 3);

        let work_a_scope = AssignmentScope::new(
            namespace.namespace_id(),
            [0xA1; 16],
            work_a.scheduler_attempt_id(),
            work_a.fence_bytes(),
        )
        .expect("first independently minted work scope");
        let work_b_scope = AssignmentScope::new(
            namespace.namespace_id(),
            [0xB2; 16],
            work_b.scheduler_attempt_id(),
            work_b.fence_bytes(),
        )
        .expect("second independently minted work scope");
        let anchor_scope = AssignmentScope::new(
            namespace.namespace_id(),
            [0xC3; 16],
            floor_anchor.scheduler_attempt_id(),
            floor_anchor.fence_bytes(),
        )
        .expect("new exact authority anchor");
        assert_ne!(work_a_scope.work_id, work_b_scope.work_id);

        let owner_endpoint = bind_direct(
            SecretKey::generate(),
            "127.0.0.1:0".parse().expect("loopback coordinator address"),
        )
        .await
        .expect("bind authorized coordinator identity");
        let coordinator = owner_endpoint.id();
        let coordinator_config = ClusterCoordinator {
            identity: coordinator,
            address: owner_endpoint.addr(),
        };
        let mut policy = test_policy();
        policy.namespace_id = namespace.namespace_id();
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        persist_no_result_record(
            &store,
            &policy,
            NoResultRecord::new(coordinator, work_a_scope),
        )
        .expect("persist exact NoResult for the older work");
        let request = ControlNoResultRetireThrough::new(
            work_a_scope,
            anchor_scope,
            work_a_scope.attempt,
            coordinator,
        )
            .expect("construct monotone owner floor");
        let directory = result_journal_directory(&store, true).expect("private journal directory");
        let floor_record = NoResultRetirementFloor::from_request(request);
        backend_platform::durable::write_private_atomic(
            &no_result_floor_path(&directory, namespace.namespace_id(), coordinator),
            &floor_record.encode().expect("encode durable floor"),
        )
        .expect("durably write floor before the simulated crash cut");
        assert_eq!(
            count_no_result_rows(&store).expect("pre-crash exact rows"),
            1
        );
        drop(store);

        let reopened = FileStore::open(&store_root, 8_192).expect("cold-reopen worker store");
        let worker = ClusterWorker::bind(ClusterWorkerConfig {
            bind_address: "127.0.0.1:0".parse().expect("loopback worker address"),
            identity: SecretKey::generate(),
            coordinators: vec![coordinator_config],
            policy: policy.clone(),
        })
        .await
        .expect("bind worker with its persisted single-owner trust policy");
        assert!(
            worker
                .recover_after_restart(&reopened)
                .await
                .expect("restart finishes interrupted floor compaction")
                .is_empty()
        );
        assert_eq!(
            count_no_result_rows(&reopened).expect("post-restart rows"),
            0
        );
        // The owner may retry when its Applied ACK was lost. Exact replay is idempotent.
        apply_no_result_retirement_floor(&reopened, &policy, &[coordinator], coordinator, request)
            .expect("retry exact retirement after cold restart");
        let floor = validate_no_result_retirement_configuration(&reopened, &policy, [coordinator])
            .expect("single-owner floor survives restart")
            .expect("persisted floor is found");
        assert_eq!(floor.retired_through_epoch, 1);
        assert_eq!(
            compact_no_result_records_through_floor(&reopened, &policy, Some(floor))
                .expect("restart completes interrupted compaction"),
            0,
        );
        assert!(
            offer_scope_is_blocked_by_recovery(&reopened, &policy, coordinator, work_a_scope)
                .expect("old late Offer is still blocked after cold restart")
        );
        assert!(
            !offer_scope_is_blocked_by_recovery(&reopened, &policy, coordinator, work_b_scope)
                .expect("later independently minted work remains eligible after restart")
        );
        assert!(
            no_result_floor_blocks(
                &reopened,
                coordinator,
                AssignmentScope::new(namespace.namespace_id(), [0xD4; 16], 1, [0xD4; 32])
                    .expect("epoch from a reset authority database")
            )
            .expect("database reset with the same coordinator fails closed below the floor")
        );
        assert!(
            !no_result_floor_blocks(&reopened, SecretKey::generate().public(), work_a_scope)
                .expect("floor is bound to its authenticated owner")
        );
        assert!(
            matches!(
                validate_no_result_retirement_configuration(
                    &reopened,
                    &policy,
                    [coordinator, SecretKey::generate().public()],
                ),
                Err(ClusterWorkerError::PeerNotAllowed)
            ),
            "a changed multi-coordinator policy cannot reuse this single-owner floor"
        );

        drop(worker);
        drop(reopened);
        drop(authority);
        let _ = fs::remove_dir_all(store_root);
        let _ = fs::remove_file(authority_path);
    }

    #[tokio::test]
    async fn owner_retirement_control_is_durable_and_idempotent_after_lost_ack() {
        let store_root = temporary_store_path("no-result-floor-control");
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let owner_endpoint = bind_direct(
            SecretKey::generate(),
            "127.0.0.1:0".parse().expect("loopback coordinator address"),
        )
        .await
        .expect("bind coordinator endpoint");
        let coordinator = ClusterCoordinator {
            identity: owner_endpoint.id(),
            address: owner_endpoint.addr(),
        };
        let coordinator_id = coordinator.identity;
        let anchor_scope = AssignmentScope::new([1; 16], [8; 16], 4, [9; 32])
            .expect("fresh authority attempt anchors retired prefix");
        let stale_scope = AssignmentScope::new([1; 16], [2; 16], 3, [4; 32])
            .expect("stale work scope under the exact namespace");
        let policy = test_policy();
        persist_no_result_record(
            &store,
            &policy,
            NoResultRecord::new(coordinator.identity, stale_scope),
        )
        .expect("persist exact owner NoResult");
        let worker = ClusterWorker::bind(ClusterWorkerConfig {
            bind_address: "127.0.0.1:0".parse().expect("loopback worker address"),
            identity: SecretKey::generate(),
            coordinators: vec![coordinator.clone()],
            policy: policy.clone(),
        })
        .await
        .expect("bind one-coordinator worker");
        let worker_id = worker.identity();
        let worker_address_first = worker.address();
        let worker_address_retry = worker.address();
        let worker_task = tokio::spawn(async move {
            for _ in 0..2 {
                let admission = ControlAdmissionPolicy::worker_with_namespace(
                    worker.endpoint.id(),
                    [coordinator_id],
                    anchor_scope.namespace_id,
                );
                let mut channel = tokio::time::timeout(
                    Duration::from_secs(3),
                    accept_control(&worker.endpoint, &admission),
                )
                .await
                .expect("worker control accept timeout")
                .expect("accept authenticated retirement stream");
                let message = channel.receive().await.expect("receive retirement command");
                let ControlMessage::NoResultRetireThrough(request) = message else {
                    panic!("owner sends no-result retirement");
                };
                worker
                    .apply_no_result_retirement(channel, request, &store, Duration::from_secs(3))
                    .await
                    .expect("persist and acknowledge exact floor");
            }
            worker
        });

        let first_ack =
            backend_engine::compiler_cluster_transport::retire_compiler_no_result_through(
                &owner_endpoint,
                worker_address_first,
                worker_id,
                stale_scope,
                anchor_scope,
                stale_scope.attempt,
            )
            .await
            .expect("owner receives first durable floor acknowledgement");
        assert_eq!(first_ack.scope, anchor_scope);
        // Reissue the exact command on a fresh authenticated stream to model a lost owner journal
        // acknowledgement. The worker must acknowledge it idempotently after already compacting.
        let retry_ack =
            backend_engine::compiler_cluster_transport::retire_compiler_no_result_through(
                &owner_endpoint,
                worker_address_retry,
                worker_id,
                stale_scope,
                anchor_scope,
                stale_scope.attempt,
            )
            .await
            .expect("owner retry receives same durable floor acknowledgement");
        assert_eq!(retry_ack, first_ack);
        let worker = worker_task.await.expect("worker control task");
        drop(worker);
        let reopened = FileStore::open(&store_root, 8_192).expect("reopen worker FileStore");
        assert_eq!(
            count_no_result_rows(&reopened).expect("count compacted tombstones"),
            0
        );
        assert!(
            no_result_floor_blocks(&reopened, coordinator_id, stale_scope)
                .expect("floor blocks exact stale attempt after row deletion")
        );
        drop(reopened);
        let _ = fs::remove_dir_all(store_root);
    }

    #[test]
    fn no_result_retirement_rejects_wrong_namespace_and_terminal_fence() {
        let store_root = temporary_store_path("no-result-floor-exact-scope");
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let coordinator = SecretKey::generate().public();
        let mut policy = test_policy();
        policy.namespace_id = [0x71; 16];
        let terminal = AssignmentScope::new(
            policy.namespace_id,
            [0x72; 16],
            5,
            [0x73; 32],
        )
        .expect("exact terminal");
        let older_terminal = AssignmentScope::new(
            policy.namespace_id,
            [0x70; 16],
            4,
            [0x71; 32],
        )
        .expect("older exact terminal in the same prefix");
        persist_no_result_record(
            &store,
            &policy,
            NoResultRecord::new(coordinator, older_terminal),
        )
        .expect("persist older no-result scope");
        persist_no_result_record(&store, &policy, NoResultRecord::new(coordinator, terminal))
            .expect("persist exact no-result scope");
        let anchor = AssignmentScope::new(policy.namespace_id, [0x74; 16], 6, [0x75; 32])
            .expect("exact namespace anchor");
        let wrong_fence = AssignmentScope::new(
            policy.namespace_id,
            terminal.work_id,
            terminal.attempt,
            [0x76; 32],
        )
        .expect("structurally valid substituted terminal fence");
        let wrong_fence_request = ControlNoResultRetireThrough::new(
            wrong_fence,
            anchor,
            terminal.attempt,
            coordinator,
        )
        .expect("wire-valid request with the wrong terminal fence");
        assert!(matches!(
            apply_no_result_retirement_floor(
                &store,
                &policy,
                &[coordinator],
                coordinator,
                wrong_fence_request,
            ),
            Err(ClusterWorkerError::ScopeMismatch)
        ));

        let exact_request = ControlNoResultRetireThrough::new(
            terminal,
            anchor,
            terminal.attempt,
            coordinator,
        )
        .expect("exact retirement request");
        apply_no_result_retirement_floor(
            &store,
            &policy,
            &[coordinator],
            coordinator,
            exact_request,
        )
        .expect("persist exact prefix proof before removing both tombstones");
        assert_eq!(
            count_no_result_rows(&store).expect("prefix compaction removed old rows"),
            0
        );
        apply_no_result_retirement_floor(
            &store,
            &policy,
            &[coordinator],
            coordinator,
            exact_request,
        )
        .expect("exact lost-ACK retry remains idempotent");

        let older_request = ControlNoResultRetireThrough::new(
            older_terminal,
            anchor,
            older_terminal.attempt,
            coordinator,
        )
        .expect("older exact debt already covered by the prefix");
        apply_no_result_retirement_floor(
            &store,
            &policy,
            &[coordinator],
            coordinator,
            older_request,
        )
        .expect("older exact debt can be acknowledged after prefix compaction");
        let wrong_old_fence = AssignmentScope::new(
            policy.namespace_id,
            older_terminal.work_id,
            older_terminal.attempt,
            [0x7A; 32],
        )
        .expect("same-epoch different fence");
        let wrong_old_request = ControlNoResultRetireThrough::new(
            wrong_old_fence,
            anchor,
            older_terminal.attempt,
            coordinator,
        )
        .expect("wire-valid wrong old fence");
        assert!(matches!(
            apply_no_result_retirement_floor(
                &store,
                &policy,
                &[coordinator],
                coordinator,
                wrong_old_request,
            ),
            Err(ClusterWorkerError::ScopeMismatch)
        ));

        let foreign_namespace = [0x77; 16];
        let foreign_terminal = AssignmentScope::new(
            foreign_namespace,
            terminal.work_id,
            terminal.attempt,
            terminal.fence,
        )
        .expect("foreign terminal namespace");
        let foreign_anchor = AssignmentScope::new(foreign_namespace, [0x78; 16], 6, [0x79; 32])
            .expect("foreign barrier namespace");
        let foreign_request = ControlNoResultRetireThrough::new(
            foreign_terminal,
            foreign_anchor,
            terminal.attempt,
            coordinator,
        )
        .expect("wire-valid foreign namespace request");
        assert!(matches!(
            apply_no_result_retirement_floor(
                &store,
                &policy,
                &[coordinator],
                coordinator,
                foreign_request,
            ),
            Err(ClusterWorkerError::PeerNotAllowed)
        ));
        assert_eq!(
            count_no_result_rows(&store).expect("retired rows remain compacted"),
            0
        );
        drop(store);
        let _ = fs::remove_dir_all(store_root);
    }

    #[test]
    fn multi_coordinator_same_scope_keeps_exact_no_result_tombstone() {
        let store_root = temporary_store_path("no-result-floor-multi-owner");
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let coordinator_a = SecretKey::generate().public();
        let coordinator_b = SecretKey::generate().public();
        let scope = test_scope();
        let policy = test_policy();
        persist_no_result_record(&store, &policy, NoResultRecord::new(coordinator_a, scope))
            .expect("persist exact no-result fence under first owner");
        let request = ControlNoResultRetireThrough::new(
            scope,
            AssignmentScope::new(scope.namespace_id, [8; 16], 4, [9; 32])
                .expect("newer anchor scope"),
            scope.attempt,
            coordinator_a,
        )
        .expect("construct owner retirement request");

        assert!(matches!(
            apply_no_result_retirement_floor(
                &store,
                &policy,
                &[coordinator_a, coordinator_b],
                coordinator_a,
                request,
            ),
            Err(ClusterWorkerError::PeerNotAllowed)
        ));
        assert_eq!(
            count_no_result_rows(&store).expect("count retained tombstones"),
            1
        );
        assert!(
            offer_scope_is_blocked_by_recovery(&store, &policy, coordinator_a, scope)
                .expect("first peer's exact stale Offer remains blocked")
        );
        assert!(
            offer_scope_is_blocked_by_recovery(&store, &policy, coordinator_b, scope)
                .expect("same-scope Offer from another peer is not unmasked")
        );
        let directory = result_journal_directory(&store, false).expect("journal directory");
        assert!(!no_result_floor_path(&directory, scope.namespace_id, coordinator_a).exists());
        let _ = fs::remove_dir_all(store_root);
    }

    #[test]
    fn distinct_no_result_scopes_grow_exact_index_across_cold_reopen() {
        let store_root = temporary_store_path("no-result-quota");
        let store = FileStore::open(&store_root, 8_192).expect("open worker FileStore");
        let directory = result_journal_directory(&store, true).expect("create private journal");
        let coordinator = SecretKey::generate().public();
        const FENCE_FIXTURE_COUNT: usize = 4_097;
        for ordinal in 0..FENCE_FIXTURE_COUNT {
            let attempt = u64::try_from(ordinal + 1).expect("bounded test attempt");
            let scope = AssignmentScope::new(
                [1; 16],
                u128::try_from(ordinal + 1)
                    .expect("bounded work identity")
                    .to_be_bytes(),
                attempt,
                [0x55; 32],
            )
            .expect("unique valid work scope");
            let record = NoResultRecord::new(coordinator, scope);
            backend_platform::durable::write_private_atomic(
                &no_result_journal_path(&directory, scope),
                &record.encode(),
            )
            .expect("persist one distinct recovery fence");
        }
        let before = count_no_result_rows(&store).expect("count durable no-result fences");
        assert_eq!(before, FENCE_FIXTURE_COUNT);
        validate_no_result_records(&store, &test_policy())
            .expect("stream-validate exact on-disk index without retaining every scope in RAM");
        drop(store);

        let reopened = FileStore::open(&store_root, 8_192).expect("cold reopen worker FileStore");
        assert_eq!(
            count_no_result_rows(&reopened).expect("count cold-reopened no-result fences"),
            FENCE_FIXTURE_COUNT
        );
        validate_no_result_records(&reopened, &test_policy())
            .expect("cold re-open validates every exact recovery fence");
        let next_scope = AssignmentScope::new(
            [1; 16],
            u128::try_from(FENCE_FIXTURE_COUNT + 1)
                .expect("next work identity")
                .to_be_bytes(),
            u64::try_from(FENCE_FIXTURE_COUNT + 1).expect("next attempt"),
            [0x55; 32],
        )
        .expect("next valid work scope");
        persist_no_result_record(
            &reopened,
            &test_policy(),
            NoResultRecord::new(coordinator, next_scope),
        )
        .expect("4098th exact fence is admitted without a false-positive scope block");
        assert!(
            scope_has_recovery_record(&reopened, &test_policy(), next_scope)
                .expect("new exact fence blocks its delayed offer")
        );
        assert!(
            !scope_has_recovery_record(
                &reopened,
                &test_policy(),
                AssignmentScope::new([1; 16], [0x77; 16], 1, [0x55; 32])
                    .expect("unrelated fresh scope")
            )
            .expect("exact index has no false-positive scope match")
        );
        let _ = fs::remove_dir_all(store_root);
    }

    #[tokio::test]
    async fn cold_pending_result_answers_recovery_query_and_keeps_bao_replay_live() {
        let store_root = temporary_store_path("pending-recovery-query");
        let outboard_root = store_root.join("outboard");
        let checkpoint_root = store_root.join("owner-checkpoints");
        ensure_private_directory(&outboard_root).expect("private outboard directory");
        ensure_private_directory(&checkpoint_root).expect("private owner checkpoint directory");
        let namespace_id = [1; 16];
        let scope = AssignmentScope::new(namespace_id, [31; 16], 5, [32; 32])
            .expect("exact cold assignment scope");
        let coordinator_endpoint = bind_direct(
            SecretKey::generate(),
            "127.0.0.1:0".parse().expect("loopback owner address"),
        )
        .await
        .expect("bind owner endpoint");
        let coordinator = ClusterCoordinator {
            identity: coordinator_endpoint.id(),
            address: coordinator_endpoint.addr(),
        };
        let store = FileStore::open(&store_root, 8_192).expect("open result CAS");
        let original_receipt = write_gc_test_closure(&store, b"cold-result-range-payload");
        let closure = original_receipt.closure();
        let pending_record = PendingResultRecord {
            coordinator: *coordinator.identity.as_bytes(),
            scope,
            input_closure_id: [41; 32],
            input_manifest_object_id: [42; 32],
            deadline_unix_ms: 1,
            closure_id: *closure.as_bytes(),
            pack_id: None,
            target_root: [43; 32],
            payload_bytes: original_receipt.payload_bytes(),
            object_count: u32::try_from(original_receipt.object_count()).expect("one member"),
        };
        persist_journal_record(&store, scope, "pending", &pending_record.encode())
            .expect("persist result before worker restart");
        drop(store);

        let store = FileStore::open(&store_root, 8_192).expect("cold reopen result CAS");
        let reopened_receipt = reopen_pending_closure(&store, &test_policy(), pending_record)
            .expect("reopen the exact durable payload closure");
        let result = ClusterWorkerResult {
            scope,
            coordinator: coordinator.identity,
            input_closure_id: pending_record.input_closure_id,
            input_manifest_object_id: pending_record.input_manifest_object_id,
            deadline_unix_ms: pending_record.deadline_unix_ms,
            receipt: reopened_receipt,
            closure,
            pack_id: None,
            target_root: pending_record.target_root,
            payload_bytes: pending_record.payload_bytes,
            object_count: pending_record.object_count,
            store: store.clone(),
        };
        let worker = Arc::new(
            ClusterWorker::bind(ClusterWorkerConfig {
                bind_address: "127.0.0.1:0".parse().expect("loopback worker address"),
                identity: SecretKey::generate(),
                coordinators: vec![coordinator],
                policy: test_policy(),
            })
            .await
            .expect("cold-bind worker with exact owner allowlist"),
        );
        let worker_id = worker.identity();
        let owner_policy = ControlAdmissionPolicy::new(
            coordinator_endpoint.id(),
            [worker_id],
            scope,
            ControlRole::Coordinator,
        );
        let worker_for_replay = Arc::clone(&worker);
        let result_for_replay = result.clone();
        let replay_task = tokio::spawn(async move {
            worker_for_replay
                .resume_retained_result_until_ack(&result_for_replay, &outboard_root)
                .await
        });

        // Drain the normal cold replay announcement first so the worker can move on to its
        // single listener, then trigger the recovery race against that retained result.
        let mut receipt_channel = tokio::time::timeout(
            Duration::from_secs(3),
            accept_control(&coordinator_endpoint, &owner_policy),
        )
        .await
        .expect("worker announces retained result promptly")
        .expect("admit worker receipt");
        let ControlMessage::ResultReceipt(receipt) = receipt_channel
            .receive()
            .await
            .expect("receive retained result receipt")
        else {
            panic!("worker sends result receipt before grant pages");
        };
        assert_eq!(receipt.closure_id, *closure.as_bytes());
        receipt_channel
            .finish()
            .await
            .expect("close receipt stream");

        let mut initial_pages = tokio::time::timeout(
            Duration::from_secs(3),
            accept_control(&coordinator_endpoint, &owner_policy),
        )
        .await
        .expect("worker sends initial grant page")
        .expect("admit initial result grant page");
        let ControlMessage::GrantPage(initial_page) = initial_pages
            .receive()
            .await
            .expect("receive initial result grants")
        else {
            panic!("worker sends result grant page");
        };
        assert_eq!(initial_page.scope, scope);
        assert_eq!(initial_page.direction, GrantDirection::ResultsToCoordinator);
        initial_pages
            .finish()
            .await
            .expect("close initial grant stream");

        let query = ControlResultRecoveryQuery::new(scope, coordinator_endpoint.id())
            .expect("construct exact recovery query");
        let mut query_channel = connect_control_with_bounded_retries(
            &coordinator_endpoint,
            worker.address(),
            worker_id,
            scope,
        )
        .await
        .expect("connect on exact worker scope after transient loopback failures");
        query_channel
            .send(&ControlMessage::ResultRecoveryQuery(query))
            .await
            .expect("send result recovery query");
        let ControlMessage::ResultRecoveryStatus(status) = query_channel
            .receive()
            .await
            .expect("receive result recovery status")
        else {
            panic!("worker answers with a recovery status");
        };
        assert!(status.matches_query(&query, coordinator_endpoint.id(), worker_id));
        assert!(matches!(
            status.state(),
            ControlResultRecoveryState::Pending(receipt)
                if receipt.closure_id == *closure.as_bytes()
                    && receipt.object_count == pending_record.object_count
        ));
        query_channel
            .finish()
            .await
            .expect("finish recovery query stream");

        let mut replay_pages = tokio::time::timeout(
            Duration::from_secs(3),
            accept_control(&coordinator_endpoint, &owner_policy),
        )
        .await
        .expect("recovery query replays result grants")
        .expect("admit replayed result grant page");
        let ControlMessage::GrantPage(replayed_page) = replay_pages
            .receive()
            .await
            .expect("receive replayed result grants")
        else {
            panic!("worker replays the retained grant page");
        };
        assert_eq!(replayed_page.scope, initial_page.scope);
        assert_eq!(replayed_page.direction, initial_page.direction);
        assert_eq!(replayed_page.page_index, initial_page.page_index);
        assert_eq!(replayed_page.page_count, initial_page.page_count);
        assert_eq!(replayed_page.grants.len(), initial_page.grants.len());
        assert_eq!(
            replayed_page.grants[0].claims.object.object_id,
            initial_page.grants[0].claims.object.object_id,
            "restart advertises the same immutable result member"
        );
        replay_pages
            .finish()
            .await
            .expect("close replay grant stream");

        let capability = replayed_page.grants[0].clone();
        let transfer_scope = TransferScope::from_store_closure(scope, closure)
            .expect("derive exact retained-result transfer scope");
        let checkpoint = checkpoint_root.join("recovered.checkpoint");
        let mut coverage = VerifiedCoverage::open(
            &coordinator_endpoint,
            &worker.address(),
            worker_id,
            &capability,
            transfer_scope,
            &checkpoint,
        )
        .await
        .expect("open the signed range served by the retained result");
        coverage
            .fetch_range(&coordinator_endpoint, worker.address(), capability)
            .await
            .expect("fetch retained result range after recovery query");
        assert!(coverage.finish().is_complete());
        assert!(
            read_pending_result_record(&store, scope)
                .expect("read retained result")
                .is_some()
        );

        // Exercise invalid query bindings after the valid replay path has completed. This keeps
        // a rejected or slow handshake from racing the positive recovery connection.
        let changed_scope = AssignmentScope::new(namespace_id, [35; 16], 5, [32; 32])
            .expect("mutated recovery scope");
        let changed_query =
            ControlResultRecoveryQuery::new(changed_scope, coordinator_endpoint.id())
                .expect("construct mutated-scope query");
        if let Ok(Ok(mut changed_channel)) = tokio::time::timeout(
            Duration::from_secs(3),
            connect_control(
                &coordinator_endpoint,
                worker.address(),
                worker_id,
                changed_scope,
                ControlRole::Coordinator,
            ),
        )
        .await
        {
            if changed_channel
                .send(&ControlMessage::ResultRecoveryQuery(changed_query))
                .await
                .is_ok()
            {
                let response =
                    tokio::time::timeout(Duration::from_secs(3), changed_channel.receive()).await;
                assert!(
                    !matches!(response, Ok(Ok(ControlMessage::ResultRecoveryStatus(_)))),
                    "a changed assignment scope cannot query or clear a retained result"
                );
            }
        }
        let wrong_owner_endpoint = bind_direct(
            SecretKey::generate(),
            "127.0.0.1:0"
                .parse()
                .expect("loopback untrusted owner address"),
        )
        .await
        .expect("bind untrusted peer endpoint");
        if let Ok(Ok(mut wrong_peer_channel)) = tokio::time::timeout(
            Duration::from_secs(3),
            connect_control(
                &wrong_owner_endpoint,
                worker.address(),
                worker_id,
                scope,
                ControlRole::Coordinator,
            ),
        )
        .await
        {
            let forged_query = ControlResultRecoveryQuery::new(scope, coordinator_endpoint.id())
                .expect("query still names the allowlisted owner");
            if wrong_peer_channel
                .send(&ControlMessage::ResultRecoveryQuery(forged_query))
                .await
                .is_ok()
            {
                let response =
                    tokio::time::timeout(Duration::from_secs(3), wrong_peer_channel.receive())
                        .await;
                assert!(
                    !matches!(response, Ok(Ok(ControlMessage::ResultRecoveryStatus(_)))),
                    "another authenticated endpoint cannot inspect the retained result"
                );
            }
        }
        assert_eq!(
            read_pending_result_record(&store, scope).expect("wrong queries preserve pending"),
            Some(pending_record),
            "mutated scope and peer attempts leave pending bytes/journal unchanged"
        );
        replay_task.abort();
        let _ = replay_task.await;
        let _ = fs::remove_dir_all(store_root);
    }

    #[tokio::test]
    async fn active_durable_running_assignment_answers_exact_recovery_query_promptly() {
        let store_root = temporary_store_path("running-recovery-query");
        let namespace_id = [1; 16];
        let coordinator_endpoint = bind_direct(
            SecretKey::generate(),
            "127.0.0.1:0".parse().expect("loopback owner address"),
        )
        .await
        .expect("bind owner endpoint");
        let coordinator = ClusterCoordinator {
            identity: coordinator_endpoint.id(),
            address: coordinator_endpoint.addr(),
        };
        let coordinator_id = coordinator.identity;
        let worker = Arc::new(
            ClusterWorker::bind(ClusterWorkerConfig {
                bind_address: "127.0.0.1:0".parse().expect("loopback worker address"),
                identity: SecretKey::generate(),
                coordinators: vec![coordinator],
                policy: test_policy(),
            })
            .await
            .expect("bind exact-trust worker"),
        );
        let scope = AssignmentScope::new(namespace_id, [51; 16], 8, [52; 32])
            .expect("valid active assignment scope");
        let offer = ControlOffer {
            scope,
            package_lineage: [53; 32],
            target: [54; 32],
            recipe: [2; 32],
            input_root: [55; 32],
            read_manifest: [56; 32],
            input_closure_id: [57; 32],
            input_manifest_object_id: [58; 32],
            selected_base: None,
            max_output_bytes: 512,
            deadline_unix_ms: now_unix_ms().expect("clock").saturating_add(60_000),
            input_grant_pages: 1,
        };
        let slot = WorkerSlot::try_acquire(&worker.resources).expect("reserve one running slot");
        worker.resources.lock().expect("worker resources").occupancy =
            SlotOccupancy::Assigned(ActiveAssignment {
                coordinator: coordinator_id,
                scope,
            });
        let assignment = AdmittedOffer {
            offer,
            coordinator: coordinator_id,
            _slot: slot,
        };
        let store = FileStore::open(&store_root, 8_192).expect("open worker CAS");
        let expected_running = RunningAssignmentRecord::from_assignment(&assignment);
        persist_journal_record(&store, scope, "running", &expected_running.encode())
            .expect("durably record assignment before compiler invocation");

        let worker_for_wait = Arc::clone(&worker);
        let wait_task =
            tokio::spawn(async move { worker_for_wait.wait_for_cancel(&assignment).await });
        let query = ControlResultRecoveryQuery::new(scope, coordinator_endpoint.id())
            .expect("construct exact active recovery query");
        let mut channel = connect_control(
            &coordinator_endpoint,
            worker.address(),
            worker.identity(),
            scope,
            ControlRole::Coordinator,
        )
        .await
        .expect("connect to active worker assignment");
        channel
            .send(&ControlMessage::ResultRecoveryQuery(query))
            .await
            .expect("send recovery query during active compiler wait");
        let ControlMessage::ResultRecoveryStatus(status) =
            tokio::time::timeout(Duration::from_secs(3), channel.receive())
                .await
                .expect("running status is prompt")
                .expect("worker accepts exact owner query")
        else {
            panic!("worker returns a recovery status");
        };
        assert!(status.matches_query(&query, coordinator_endpoint.id(), worker.identity()));
        assert!(matches!(
            status.state(),
            ControlResultRecoveryState::Running
        ));
        channel.finish().await.expect("finish recovery query");
        assert!(
            read_running_assignment_record(&store, scope)
                .expect("read live running marker")
                .is_some_and(|record| {
                    record.coordinator == expected_running.coordinator
                        && record.scope == expected_running.scope
                        && record.input_closure_id == expected_running.input_closure_id
                        && record.input_manifest_object_id
                            == expected_running.input_manifest_object_id
                        && record.deadline_unix_ms == expected_running.deadline_unix_ms
                })
        );
        wait_task.abort();
        let _ = wait_task.await;
        remove_running_assignment(&store, scope).expect("remove fixture running marker");
        let _ = fs::remove_dir_all(store_root);
    }

    #[cfg(unix)]
    #[test]
    fn cold_gc_reclaims_acked_closures_and_retains_running_input_and_pending_result() {
        let root = temporary_store_path("ack-gc");
        let input_root = root.join("input-cas");
        let result_root = root.join("result-cas");
        let input_store = FileStore::open(&input_root, 8_192).expect("open input CAS");
        let result_store = FileStore::open(&result_root, 8_192).expect("open result CAS");
        let first_input = write_gc_test_closure(&input_store, b"first input");
        let second_input = write_gc_test_closure(&input_store, b"second input");
        let first_result = write_gc_test_closure(&result_store, b"first result");
        let second_result = write_gc_test_closure(&result_store, b"second result");

        // The first assignment completed its owner ACK and has no journal root. The second
        // assignment is still running and has an unacknowledged result after a cold restart.
        let scope = AssignmentScope::new([1; 16], [3; 16], 2, [4; 32]).expect("scope");
        let running = RunningAssignmentRecord {
            coordinator: [5; 32],
            scope,
            input_closure_id: *second_input.closure().as_bytes(),
            input_manifest_object_id: [6; 32],
            deadline_unix_ms: 10,
        };
        let pending = PendingResultRecord {
            coordinator: [5; 32],
            scope,
            input_closure_id: *second_input.closure().as_bytes(),
            input_manifest_object_id: [6; 32],
            deadline_unix_ms: 20,
            closure_id: *second_result.closure().as_bytes(),
            pack_id: None,
            target_root: [7; 32],
            payload_bytes: second_result.payload_bytes(),
            object_count: u32::try_from(second_result.object_count()).expect("one object"),
        };
        persist_journal_record(&result_store, scope, "running", &running.encode())
            .expect("persist active input root");
        persist_journal_record(&result_store, scope, "pending", &pending.encode())
            .expect("persist retained result root");

        drop(input_store);
        drop(result_store);
        let input_store = FileStore::open(&input_root, 8_192).expect("cold reopen input CAS");
        let result_store = FileStore::open(&result_root, 8_192).expect("cold reopen result CAS");
        collect_worker_store_garbage(&input_store, &result_store, &test_policy())
            .expect("collect with durable worker roots");
        assert!(
            input_store
                .read_closure_index(second_input.closure())
                .is_ok()
        );
        assert!(
            result_store
                .read_closure_index(second_result.closure())
                .is_ok()
        );
        assert!(
            input_store
                .read_closure_index(first_input.closure())
                .is_err()
        );
        assert!(
            result_store
                .read_closure_index(first_result.closure())
                .is_err()
        );

        let terminal = RetiredResultRecord {
            coordinator: [5; 32],
            scope,
            closure_id: *second_result.closure().as_bytes(),
            disposition: ResultAckDisposition::Stored,
        };
        persist_retired_result_record(&result_store, terminal)
            .expect("persist terminal owner decision");
        retire_result_payload_state(&result_store, terminal)
            .expect("release replay rows after durable terminal decision");
        collect_worker_store_garbage(&input_store, &result_store, &test_policy())
            .expect("collect after terminal ACK");
        assert!(
            input_store
                .read_closure_index(second_input.closure())
                .is_err()
        );
        assert!(
            result_store
                .read_closure_index(second_result.closure())
                .is_err()
        );
        assert!(
            result_journal_path(
                &result_journal_directory(&result_store, false).expect("journal directory"),
                scope,
                "retired"
            )
            .is_file()
        );
        let _ = fs::remove_dir_all(root);
    }

    fn test_scope() -> AssignmentScope {
        AssignmentScope::new([1; 16], [2; 16], 3, [4; 32]).expect("valid test scope")
    }

    fn test_policy() -> ClusterWorkerPolicy {
        ClusterWorkerPolicy {
            namespace_id: [1; 16],
            accepted_recipes: vec![[2; 32]],
            execution_policy: ClusterExecutionPolicy::DenyUnconfined,
            max_output_bytes: 1024,
            cpu_millicores: DEFAULT_CPU_MILLICORES,
            memory_bytes: DEFAULT_MEMORY_CREDITS_BYTES,
            max_input_objects: 32,
            max_input_bytes: 1024,
            max_capabilities: 256,
            max_grant_pages: MAX_CONTROL_GRANT_PAGES,
            io_timeout: Duration::from_secs(1),
        }
    }

    async fn connect_control_with_bounded_retries(
        endpoint: &Endpoint,
        peer_address: EndpointAddr,
        expected_peer: EndpointId,
        scope: AssignmentScope,
    ) -> Result<ControlChannel, String> {
        tokio::time::timeout(Duration::from_secs(21), async {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            let mut backoff = Duration::from_millis(25);
            let mut last_error = String::from("no connection attempt completed");
            loop {
                match connect_control(
                    endpoint,
                    peer_address.clone(),
                    expected_peer,
                    scope,
                    ControlRole::Coordinator,
                )
                .await
                {
                    Ok(channel) => return Ok(channel),
                    Err(backend_engine::cluster_transport::TransportError::Iroh(error)) => {
                        last_error = error;
                    }
                    Err(error) => {
                        return Err(format!("non-retryable control connection error: {error:?}"));
                    }
                }
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(format!("control connection retries expired: {last_error}"));
                }
                tokio::time::sleep(backoff.min(remaining)).await;
                backoff = backoff.saturating_mul(2).min(Duration::from_millis(250));
            }
        })
        .await
        .unwrap_or_else(|_| Err("control connection retry deadline elapsed".into()))
    }

    #[test]
    fn slot_occupancy_tracks_expiry_offer_match_and_raii_release() {
        let coordinator = SecretKey::generate().public();
        let scope = test_scope();
        let session = backend_engine::cluster_transport::CompilerProbeSession {
            scope,
            nonce: [12; 16],
            expires_at_unix_ms: 4_000,
            identity: CompilerProbeIdentity {
                package_lineage: [1; 32],
                target: [2; 32],
                package_target: vec![3],
                profile: [4, 5],
                stage: 1,
                recipe: [6; 32],
                input_root: [7; 32],
                read_manifest: [8; 32],
                input_closure_id: [9; 32],
                input_manifest_object_id: [10; 32],
                selected_base: None,
                max_output_bytes: 100,
            },
            demand: CompilerProbeDemand {
                cpu_millicores: 500,
                memory_bytes: 1024,
                transfer_bytes: 100,
            },
            session_affinity: None,
        };
        let total = ProbeResourceCredits {
            cpu_millicores: 1_000,
            memory_bytes: 2_048,
            transfer_bytes: 1_000,
        };
        let resources = Arc::new(Mutex::new(WorkerResources {
            total,
            occupancy: SlotOccupancy::Idle,
        }));
        {
            let mut state = resources.lock().expect("worker resources");
            assert!(state.can_reserve(probe_demand_credits(session.demand), 1_000));
            state.occupancy = SlotOccupancy::Reserved(ProbeReservation {
                coordinator,
                lease_expires_at_unix_ms: 2_000,
                offer_binding: ProbeOfferBinding::from_probe(&session),
            });
            assert!(!state.can_reserve(probe_demand_credits(session.demand), 1_001));
            assert_eq!(state.busy(1_001), total);
            assert!(state.can_reserve(probe_demand_credits(session.demand), 2_000));
            assert!(matches!(&state.occupancy, SlotOccupancy::Idle));
            state.occupancy = SlotOccupancy::Reserved(ProbeReservation {
                coordinator,
                lease_expires_at_unix_ms: 4_000,
                offer_binding: ProbeOfferBinding::from_probe(&session),
            });
        }
        let offer = ControlOffer {
            scope,
            package_lineage: session.identity.package_lineage,
            target: session.identity.target,
            recipe: session.identity.recipe,
            input_root: session.identity.input_root,
            read_manifest: session.identity.read_manifest,
            input_closure_id: session.identity.input_closure_id,
            input_manifest_object_id: session.identity.input_manifest_object_id,
            selected_base: None,
            max_output_bytes: session.identity.max_output_bytes,
            deadline_unix_ms: 60_000,
            input_grant_pages: 1,
        };
        let replay_slot = WorkerSlot::try_acquire(&resources)
            .expect("retained-result replay may temporarily use the reserved slot");
        assert!(matches!(
            &resources.lock().expect("replay resource state").occupancy,
            SlotOccupancy::Replaying {
                suspended_reservation: Some(_)
            }
        ));
        drop(replay_slot);
        assert!(matches!(
            &resources
                .lock()
                .expect("reservation restored after replay")
                .occupancy,
            SlotOccupancy::Reserved(_)
        ));

        let wrong_coordinator = SecretKey::generate().public();
        assert!(matches!(
            WorkerSlot::try_acquire_offer(&resources, &offer, wrong_coordinator, 2_001),
            Err(ClusterWorkerError::OfferDeclined)
        ));
        assert!(matches!(
            &resources
                .lock()
                .expect("wrong peer does not consume reservation")
                .occupancy,
            SlotOccupancy::Reserved(_)
        ));

        let slot = WorkerSlot::try_acquire_offer(&resources, &offer, coordinator, 2_001)
            .expect("only the matching live Offer consumes the reservation");
        assert_eq!(
            resources
                .lock()
                .expect("running resource state")
                .active_assignment(),
            Some(ActiveAssignment { coordinator, scope })
        );
        assert!(matches!(
            WorkerSlot::try_acquire_offer(&resources, &offer, coordinator, 2_002),
            Err(ClusterWorkerError::SlotBusy)
        ));
        drop(slot);
        let mut state = resources.lock().expect("released resources");
        assert!(matches!(&state.occupancy, SlotOccupancy::Idle));
        assert_eq!(
            state.busy(2_002),
            ProbeResourceCredits {
                cpu_millicores: 0,
                memory_bytes: 0,
                transfer_bytes: 0,
            }
        );
        assert!(state.can_reserve(probe_demand_credits(session.demand), 2_002));
        drop(state);

        let reacquired = WorkerSlot::try_acquire(&resources)
            .expect("RAII drop releases the slot for subsequent local replay");
        assert!(matches!(
            &resources
                .lock()
                .expect("reacquired resource state")
                .occupancy,
            SlotOccupancy::Replaying {
                suspended_reservation: None
            }
        ));
        drop(reacquired);
    }

    #[test]
    fn partial_probe_have_checks_independent_cas_objects_without_owner_closure_index() {
        let store_root = temporary_store_path("partial-have");
        let store = FileStore::open(&store_root, 8_192).expect("open worker CAS");
        let cached_closure = write_gc_test_closure(&store, b"one locally cached object");
        let cached_index = store
            .read_closure_index(cached_closure.closure())
            .expect("read local fixture index");
        let cached_id = *cached_index
            .page_ids(None, 1)
            .expect("page cached ID")
            .object_ids()[0]
            .as_bytes();
        let cached_length = u32::try_from(
            store
                .verify_object_claim(UntrustedObjectId::from_bytes(cached_id))
                .expect("verify cached typed CAS member")
                .payload_len(),
        )
        .expect("bounded test payload length");
        let mut claims = vec![
            ProbeObjectClaim {
                object_id: cached_id,
                payload_bytes: cached_length,
            },
            ProbeObjectClaim {
                object_id: [0x31; 32],
                payload_bytes: 17,
            },
            ProbeObjectClaim {
                object_id: [0x72; 32],
                payload_bytes: 29,
            },
        ];
        claims.sort_by_key(|claim| claim.object_id);
        let bitmap = verified_have_bitmap(&store, &claims);
        assert_eq!(bitmap.iter().map(|byte| byte.count_ones()).sum::<u32>(), 1);
        let cached_position = claims
            .iter()
            .position(|claim| claim.object_id == cached_id)
            .expect("cached challenge member");
        assert_ne!(
            bitmap[cached_position / 8] & (1 << (cached_position % 8)),
            0
        );
        let _ = fs::remove_dir_all(store_root);
    }

    fn write_gc_test_closure(store: &FileStore, payload: &[u8]) -> StoredClosureReceipt {
        let key = backend_engine::ObjectKey::<GcTestObjectSchema>::from_value(b"gc-test-key");
        let object = TypedObject::from_value(&key, payload);
        let claim = ArtifactObjectClaim::new(
            object.schema(),
            *object.key(),
            *object.version(),
            object.bytes().len() as u64,
        );
        let metadata_bytes =
            StreamingClosureBudget::metadata_input_bytes_for(1).expect("budget metadata");
        let mut builder = store
            .begin_streaming_closure(StreamingClosureBudget::new(
                1,
                payload.len() as u64,
                payload.len(),
                ARTIFACT_PUT_CHUNK_BYTES,
                1,
                metadata_bytes,
            ))
            .expect("begin test closure");
        let mut object_stream = builder.begin_object(claim).expect("begin test object");
        object_stream.write(payload).expect("write test payload");
        object_stream.finish().expect("finish test object");
        builder.seal().expect("seal test closure")
    }

    fn temporary_store_path(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "backend-worker-{label}-{}-{nonce}",
            std::process::id()
        ))
    }
}

//! Owner-side, fenced dispatch for semantic compiler work.
//!
//! This module owns the Iroh endpoint and storage catalog used by locald's compiler-cluster
//! path. Placement, Offer admission, input/result transfer, and durable-result acknowledgement
//! are kept together here so the local index path can hand Turso's exact attempt to one checked
//! state machine.

use super::remote_semantic_query::{self, RemoteIndexUsage};
use crate::cluster_owner::{ClusterOwnerConfig, ClusterOwnerConfigError};
use crate::compiler_trust::{
    CompilerTrustError, TRUSTED_COMPILER_POLICY_FILE_NAME, TrustedCompilerWorkerGrant,
    TrustedCompilerWorkerPolicy,
};
use backend_engine::application::{
    AdmittedRemoteCompilerCandidate, CapturedFullWorkspaceV2, CompilerAssignment,
    CompilerAssignmentOutcome, CompilerAttemptToken, CompilerBalancedRemote,
    CompilerBalancingRequest, CompilerByteCredits, CompilerClusterScheduler, CompilerCpuCredits,
    CompilerDemand, CompilerInputCaptureCacheKeyV2, CompilerMemoryCredits,
    CompilerNodeCapacityClaim, CompilerNodeCapacityError, CompilerNodeCapacityVerifier,
    CompilerPeerId, CompilerPlacementPolicy, CompilerRemotePreflightError,
    CompilerRemotePreflightVerifier, CompilerRemoteProbeBinding, CompilerResourceCredits,
    CompilerSessionAffinity, CompilerWorkIdentity, LocalCompilerAvailability,
    RemoteCompilerCapabilityClaim, RemoteCompilerCostClaim, RemoteCompilerPreflightClaim,
    RemoteHaveClaim, VerifiedCompilerInputAdmission, VerifiedCompilerNodeCapacity,
    VerifiedRemoteCompiler, admit_remote_compiler_candidate,
};
use backend_engine::cluster_transport::{
    AcceptedClusterConnection, AdmissionPolicy, AssignmentScope, Capability, CapabilityIssuer,
    ChunkRange, ClusterListener, CompilerProbeDemand, CompilerProbePage, CompilerProbeSession,
    ControlChannel, ControlGrantPage, ControlMessage, ControlResultAck, ControlResultRecoveryQuery,
    ControlResultRecoveryState, ControlResultRecoveryStatus, ControlResultRetired,
    ControlResultRetirementApplied, ControlResultRetirementConfirm, ControlRole, Endpoint,
    EndpointAddr, EndpointId, GrantDirection, MAX_CONTROL_GRANT_PAGES, MAX_OFFER_CAPABILITIES,
    MAX_PROBE_LIFETIME_MS, MAX_PROBE_OBJECTS_PER_PAGE, MAX_PROBE_PAGES, MAX_RANGE_CHUNKS,
    MAX_RESPONSE_BYTES, OwnerClusterAdmissionRegistry, ProbeCapability, ProbeInventoryDescriptor,
    ProbeInventoryHasher, ProbeObjectClaim, ProbeResourceCredits, ResultAckDisposition,
    ResultRejectReason, ResumeState, ServerState, StoreBlobCatalog, TransferScope, TransportError,
    VerifiedCoverage, bind_direct, connect_probe, verify_admission,
};
use backend_extension_turso::CandidateAttempt;
use backend_platform::durable::{
    ensure_private_directory, open_private_read, write_private_atomic,
};
use backend_replication::{AttemptId, Fence};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactObjectClaim, ArtifactPlan, ArtifactSink,
    ClosureId, FileStore, PinnedStoredClosureReceipt, UntrustedObjectId,
};
use futures_util::stream::{FuturesUnordered, StreamExt};
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::fs;
use std::future::Future;
use std::io::{self, Read};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;

const OWNER_CONFIG_FILE_NAME: &str = "cluster-owner.v1";
const CLUSTER_OUTBOARD_DIRECTORY: &str = "cluster-compiler-outboard";
const CLUSTER_CHECKPOINT_DIRECTORY: &str = "cluster-compiler-checkpoints";
const REMOTE_ASSIGNMENT_CAPACITY: usize = 4;
const CLUSTER_ARTIFACT_OBJECT_LIMIT: usize = 100_002;
const RETAINED_CAPTURE_CACHE_MAX_ENTRIES: usize = 8;
const RETAINED_CAPTURE_CACHE_MAX_TREE_BYTES: usize = 128 * 1024 * 1024;
const RETAINED_CAPTURE_CACHE_MAX_PAYLOAD_BYTES: u64 = 256 * 1024 * 1024;
const CLUSTER_ARTIFACT_BYTES_LIMIT: u64 = 512 * 1024 * 1024;
const CLUSTER_ARTIFACT_CHUNK_BYTES: usize = 64 * 1024;
const CLUSTER_ARTIFACT_PUT_LIMIT: usize = 1_000_000;
const CLUSTER_IO_TIMEOUT: Duration = Duration::from_secs(90);
const RECOVERED_RESULT_TRANSFER_WINDOW_MS: u64 = 5 * 60 * 1_000;
const RECOVERED_RUNNING_POLL_WINDOW: Duration = Duration::from_secs(2);
const RESULT_RECOVERY_QUERY_TIMEOUT: Duration = Duration::from_secs(10);
const NO_RESULT_RETIREMENT_TIMEOUT: Duration = Duration::from_millis(750);
const NO_RESULT_RETIREMENT_RETRY_DELAY: Duration = Duration::from_millis(100);
const NO_RESULT_RETIREMENT_RETRIES: usize = 2;
const NO_RESULT_RETIREMENT_MAX_PARALLEL: usize = 8;
const NO_RESULT_RETIREMENT_IDLE_DELAY: Duration = Duration::from_secs(15);
const NO_RESULT_RETIREMENT_MAX_DELAY: Duration = Duration::from_secs(60);
const NO_RESULT_RETIREMENT_FILE_NAME: &str = "compiler-no-result-retirement.v1";
const PENDING_ACK_RETRY_INITIAL_DELAY: Duration = Duration::from_secs(2);
const PENDING_ACK_RETRY_PROGRESS_DELAY: Duration = Duration::from_millis(25);
const PENDING_ACK_RETRY_IDLE_DELAY: Duration = Duration::from_secs(15);
const PENDING_ACK_RETRY_MAX_DELAY: Duration = Duration::from_secs(60);
const INTERACTIVE_EXPLORATION_MAX_SLACK_MS: u64 = 250;
const INTERACTIVE_EXPLORATION_MAX_RATIO_PER_MILLE: u64 = 1_500;
const MAX_CLUSTER_LISTENER_QUEUE: usize = 32;
const MAX_TRUSTED_PROBE_PEERS: usize = 64;
const PROBE_CANDIDATE_SETTLE_WINDOW: Duration = Duration::from_millis(350);
const MAX_CLUSTER_ARTIFACT_SERVERS: usize = 16;
const MAX_CLUSTER_CONTROL_CONNECTIONS: usize = 32;
const OWNER_CONTROL_QUEUE_PER_ASSIGNMENT: usize = 16;
const RECOVERED_ASSIGNMENT_CPU_MILLICORES: u32 = 1_000;
const RECOVERED_ASSIGNMENT_MEMORY_FLOOR_BYTES: u64 = 128 * 1024 * 1024;
const RECOVERED_ASSIGNMENT_MEMORY_PER_INPUT_BYTE: u64 = 2;
const RECOVERED_ASSIGNMENT_MEMORY_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const ROUTE_COST_MODEL_FILE_NAME: &str = "compiler-route-costs.v1";
const LOCAL_COST_MODEL_FILE_NAME: &str = "compiler-local-costs.v1";
const ROUTE_COST_MODEL_MAGIC: &[u8; 8] = b"BKRCOST1";
const LOCAL_COST_MODEL_MAGIC: &[u8; 8] = b"BKLCOST1";
const ROUTE_COST_MODEL_VERSION: u16 = 1;
const ROUTE_COST_MODEL_MAX_ROWS: usize = 128;
const ROUTE_COST_MODEL_MAX_BYTES: usize = 32 * 1024;
const ROUTE_COST_SAMPLE_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const ROUTE_COST_EXPLORATION_INTERVAL_MS: u64 = 5 * 60 * 1_000;
const ROUTE_COST_PRIOR_TRANSFER_BYTES_PER_SECOND: u64 = 64 * 1024;
const ROUTE_COST_MIN_TRANSFER_BYTES_PER_SECOND: u64 = 4 * 1024;
const ROUTE_COST_MAX_TRANSFER_BYTES_PER_SECOND: u64 = 50 * 1024 * 1024;
const ROUTE_COST_MIB: u64 = 1024 * 1024;
const ROUTE_COST_MAX_COMPONENT_MS: u64 = 24 * 60 * 60 * 1_000;
const LOCAL_COST_MODEL_MAX_ROWS: usize = 128;
const LOCAL_COST_MODEL_MAX_BYTES: usize = 32 * 1024;
const LOCAL_COST_SAMPLE_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const LOCAL_COST_PRIOR_MILLIS_PER_MIB: u64 = 30_000;
const LOCAL_COST_PRIOR_MILLIS_PER_ARTIFACT: u64 = 5_000;

type PendingCostProbeKey = ([u8; 16], [u8; 16], u64, [u8; 32]);
type PendingCostProbeMap = Arc<Mutex<HashMap<PendingCostProbeKey, PendingRemoteCostProbe>>>;

#[path = "cluster_dispatch/no_result_retirement.rs"]
mod no_result_retirement;
use no_result_retirement::{NoResultRetirementJournal, RetirementTarget};

fn cluster_artifact_budget() -> ArtifactBudget {
    ArtifactBudget::new(
        CLUSTER_ARTIFACT_OBJECT_LIMIT,
        CLUSTER_ARTIFACT_OBJECT_LIMIT,
        CLUSTER_ARTIFACT_BYTES_LIMIT,
        CLUSTER_ARTIFACT_CHUNK_BYTES,
        CLUSTER_ARTIFACT_PUT_LIMIT,
    )
}

/// Typed failure at the local compiler-cluster owner boundary.
#[derive(Debug)]
pub(crate) enum ClusterDispatchError {
    /// Persisted local owner identity could not be opened or validated.
    OwnerConfig(ClusterOwnerConfigError),
    /// Persisted compiler-worker policy could not be opened or validated.
    TrustPolicy(CompilerTrustError),
    /// The durable cluster endpoint or Tokio runtime could not be initialized.
    Runtime(io::Error),
    /// Iroh rejected an endpoint, authenticated stream, signed grant, or Bao transfer.
    Transport(String),
    /// Durable compiler artifact storage or closure admission failed.
    Store(String),
    /// The configured owner identity did not match the newly bound endpoint.
    EndpointIdentityMismatch,
    /// The owner was not configured for direct compiler-cluster execution.
    NotConfigured,
    /// A peer answered the wrong exact work scope or failed live probe admission.
    ProbeRejected,
    /// No currently trusted and reachable compiler can admit this exact work.
    NoEligibleWorker,
    /// The exact scheduler assignment was cancelled, stale, or exceeded its fence.
    AssignmentRejected,
    /// The owner could not admit the captured input or trusted-execution grant.
    InputRejected,
    /// The worker declined or failed the exact input/compile attempt.
    WorkerFailed,
    /// Result acknowledgement did not match the durable attempt and closure.
    AckRejected(String),
    /// The exact worker or assignment deadline elapsed during a bounded operation.
    DeadlineExpired,
    /// The worker declined the exact V2 Offer before input transfer.
    WorkerDeclined,
    /// The worker rejected the exact V2 input closure after provisional acceptance.
    WorkerRejected,
    /// A result failed closure, typed semantic, or CAS admission.
    ResultRejected(String),
    /// A completed worker result failed a conclusive, typed admission check.
    ///
    /// Live and recovered result paths may terminally reject only this class. Timeouts, local
    /// storage errors, allocation failures, and transport failures keep the worker's durable
    /// result pending for a later retry.
    ConclusiveResultRejection(String),
    /// Persisted owner-side route measurements are corrupt or unavailable.
    CostModel(String),
    /// Durable NoResult retirement maintenance could not admit or update its bounded outbox.
    NoResultRetirement(String),
}

impl fmt::Display for ClusterDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OwnerConfig(error) => write!(formatter, "cluster owner config: {error}"),
            Self::TrustPolicy(error) => write!(formatter, "compiler worker trust: {error}"),
            Self::Runtime(error) => write!(formatter, "cluster runtime: {error}"),
            Self::Transport(error) => write!(formatter, "cluster transport: {error}"),
            Self::Store(error) => write!(formatter, "cluster artifact store: {error}"),
            Self::EndpointIdentityMismatch => {
                formatter.write_str("bound compiler endpoint differs from its persisted identity")
            }
            Self::NotConfigured => formatter.write_str("owner compiler cluster is not configured"),
            Self::ProbeRejected => {
                formatter.write_str("compiler peer probe failed exact admission")
            }
            Self::NoEligibleWorker => {
                formatter.write_str("no live trusted compiler worker can admit this work")
            }
            Self::AssignmentRejected => {
                formatter.write_str("compiler assignment is stale or mismatched")
            }
            Self::InputRejected => {
                formatter.write_str("compiler input or execution grant was rejected")
            }
            Self::WorkerFailed => formatter.write_str("remote compiler declined or failed work"),
            Self::AckRejected(error) => write!(formatter, "remote result acknowledgement: {error}"),
            Self::DeadlineExpired => {
                formatter.write_str("remote compiler assignment deadline elapsed")
            }
            Self::WorkerDeclined => formatter.write_str("remote worker declined the exact offer"),
            Self::WorkerRejected => {
                formatter.write_str("remote worker rejected the exact input closure")
            }
            Self::ResultRejected(error) => {
                write!(formatter, "remote compiler result rejected: {error}")
            }
            Self::ConclusiveResultRejection(error) => {
                write!(
                    formatter,
                    "remote compiler result failed admission: {error}"
                )
            }
            Self::CostModel(error) => write!(formatter, "remote compiler cost model: {error}"),
            Self::NoResultRetirement(error) => {
                write!(formatter, "NoResult retirement maintenance: {error}")
            }
        }
    }
}

impl std::error::Error for ClusterDispatchError {}

/// A live direct Iroh endpoint and bounded durable transfer state for the local compiler owner.
///
/// The runtime belongs to this object so all Iroh operations run with a live Tokio reactor for
/// its full endpoint lifetime. A locald profile without `cluster-owner.v1` remains local-only.
pub(crate) struct OwnerCompilerClusterRuntime {
    runtime: tokio::runtime::Runtime,
    endpoint: Endpoint,
    owner_address: EndpointAddr,
    issuer: CapabilityIssuer,
    owner_id: EndpointId,
    scheduler: backend_engine::application::CompilerClusterScheduler,
    store: FileStore,
    catalog: StoreBlobCatalog,
    trust_policy_path: PathBuf,
    checkpoint_root: PathBuf,
    cost_model: OwnerRemoteCompilerCostModel,
    pending_cost_probes: PendingCostProbeMap,
    authority_path: PathBuf,
    retained_compiler_captures: Mutex<RetainedCompilerCaptureCache>,
    no_result_retirement: Arc<NoResultRetirementJournal>,
    no_result_retirement_notify: Arc<tokio::sync::Notify>,
    _no_result_retirement_task: tokio::task::JoinHandle<()>,
    ingress: OwnerClusterIngress,
    _ingress_task: tokio::task::JoinHandle<()>,
    pending_ack_retry: Mutex<Option<PendingAckRetryWorker>>,
}

struct RetainedCompilerCapture {
    capture: CapturedFullWorkspaceV2,
    tree_bytes: usize,
    payload_bytes: u64,
    last_used: u128,
}

#[derive(Default)]
struct RetainedCompilerCaptureCache {
    entries: BTreeMap<CompilerInputCaptureCacheKeyV2, RetainedCompilerCapture>,
    tree_bytes: usize,
    payload_bytes: u64,
    next_sequence: u128,
}

impl RetainedCompilerCaptureCache {
    fn get(&mut self, key: CompilerInputCaptureCacheKeyV2) -> Option<CapturedFullWorkspaceV2> {
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        let entry = self.entries.get_mut(&key)?;
        entry.last_used = sequence;
        Some(entry.capture.clone())
    }

    fn retain(&mut self, capture: CapturedFullWorkspaceV2) {
        let key = capture.cache_key();
        self.remove(&key);
        let tree_bytes = capture.retained_tree_bytes();
        let payload_bytes = capture.payload_bytes();
        if tree_bytes > RETAINED_CAPTURE_CACHE_MAX_TREE_BYTES
            || payload_bytes > RETAINED_CAPTURE_CACHE_MAX_PAYLOAD_BYTES
        {
            return;
        }
        let Some(tree_total) = self.tree_bytes.checked_add(tree_bytes) else {
            return;
        };
        let Some(payload_total) = self.payload_bytes.checked_add(payload_bytes) else {
            return;
        };
        let sequence = self.next_sequence;
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.entries.insert(
            key,
            RetainedCompilerCapture {
                capture,
                tree_bytes,
                payload_bytes,
                last_used: sequence,
            },
        );
        self.tree_bytes = tree_total;
        self.payload_bytes = payload_total;
        while self.entries.len() > RETAINED_CAPTURE_CACHE_MAX_ENTRIES
            || self.tree_bytes > RETAINED_CAPTURE_CACHE_MAX_TREE_BYTES
            || self.payload_bytes > RETAINED_CAPTURE_CACHE_MAX_PAYLOAD_BYTES
        {
            if !self.evict_oldest() {
                break;
            }
        }
    }

    fn remove(&mut self, key: &CompilerInputCaptureCacheKeyV2) {
        if let Some(entry) = self.entries.remove(key) {
            self.tree_bytes = self.tree_bytes.saturating_sub(entry.tree_bytes);
            self.payload_bytes = self.payload_bytes.saturating_sub(entry.payload_bytes);
        }
    }

    fn evict_oldest(&mut self) -> bool {
        let oldest = self
            .entries
            .iter()
            .min_by(|(left_key, left), (right_key, right)| {
                left.last_used
                    .cmp(&right.last_used)
                    .then_with(|| left_key.cmp(right_key))
            })
            .map(|(key, _)| *key);
        let Some(oldest) = oldest else {
            return false;
        };
        self.remove(&oldest);
        true
    }
}

/// Outcome of one bounded pending-ACK reconciliation sweep.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PendingAckRetryOutcome {
    /// No pending journal rows remain.
    Idle,
    /// One row was durably acknowledged; another row may remain.
    Progress { remaining: usize },
    /// A pending row could not be resolved or acknowledged and needs backoff.
    TransientFailure,
}

/// One serialized retry loop for owner-restart ACK reconciliation.
///
/// The callback is supplied by locald and must do at most one bounded sweep outside its command
/// adapter lock. It returns `true` while unresolved rows or transient failures remain. The
/// worker uses an OS thread because the synchronous authority path may call this runtime's
/// `block_on`; running that callback inside Tokio would make those nested I/O calls invalid.
struct PendingAckRetryWorker {
    signal: Arc<(Mutex<PendingAckRetrySignal>, Condvar)>,
    thread: Option<thread::JoinHandle<()>>,
}

#[derive(Default)]
struct PendingAckRetrySignal {
    stopped: bool,
    wake: bool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum PendingAckRetryWake {
    Stop,
    Signalled,
    Timeout,
}

impl PendingAckRetryWorker {
    fn start<F, W>(factory: F) -> io::Result<Self>
    where
        F: FnOnce() -> W + Send + 'static,
        W: FnMut() -> PendingAckRetryOutcome + 'static,
    {
        let signal = Arc::new((Mutex::new(PendingAckRetrySignal::default()), Condvar::new()));
        let thread_signal = Arc::clone(&signal);
        let worker = thread::Builder::new()
            .name("compiler-ack-retry".to_owned())
            .spawn(move || {
                let mut retry = factory();
                let mut delay = PENDING_ACK_RETRY_PROGRESS_DELAY;
                loop {
                    match wait_for_retry(&thread_signal, delay) {
                        PendingAckRetryWake::Stop => return,
                        PendingAckRetryWake::Signalled => {
                            delay = PENDING_ACK_RETRY_PROGRESS_DELAY;
                        }
                        PendingAckRetryWake::Timeout => {}
                    }
                    let outcome =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut retry))
                            .unwrap_or(PendingAckRetryOutcome::TransientFailure);
                    delay = next_pending_ack_retry_delay(delay, outcome);
                }
            })?;
        Ok(Self {
            signal,
            thread: Some(worker),
        })
    }

    fn wake(&self) {
        let (signal, wake) = &*self.signal;
        signal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .wake = true;
        wake.notify_one();
    }
}

impl Drop for PendingAckRetryWorker {
    fn drop(&mut self) {
        let (signal, wake) = &*self.signal;
        signal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stopped = true;
        wake.notify_all();
        if let Some(worker) = self.thread.take()
            && worker.thread().id() != thread::current().id()
        {
            let _ = worker.join();
        }
    }
}

fn wait_for_retry(
    signal: &(Mutex<PendingAckRetrySignal>, Condvar),
    delay: Duration,
) -> PendingAckRetryWake {
    let (signal, wake) = signal;
    let signal = signal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut signal, timed_out) = wake
        .wait_timeout_while(signal, delay, |signal| !signal.stopped && !signal.wake)
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if signal.stopped {
        PendingAckRetryWake::Stop
    } else if signal.wake {
        signal.wake = false;
        PendingAckRetryWake::Signalled
    } else if timed_out.timed_out() {
        PendingAckRetryWake::Timeout
    } else {
        PendingAckRetryWake::Timeout
    }
}

fn next_pending_ack_retry_delay(current: Duration, outcome: PendingAckRetryOutcome) -> Duration {
    match outcome {
        PendingAckRetryOutcome::Idle => PENDING_ACK_RETRY_IDLE_DELAY,
        PendingAckRetryOutcome::Progress { .. } => PENDING_ACK_RETRY_PROGRESS_DELAY,
        PendingAckRetryOutcome::TransientFailure if current < PENDING_ACK_RETRY_INITIAL_DELAY => {
            PENDING_ACK_RETRY_INITIAL_DELAY
        }
        PendingAckRetryOutcome::TransientFailure => current
            .checked_mul(2)
            .unwrap_or(PENDING_ACK_RETRY_MAX_DELAY)
            .min(PENDING_ACK_RETRY_MAX_DELAY),
    }
}

/// Checked output awaiting the index owner's durable Turso selection.
///
/// The pending transport acknowledgement is intentionally separate: the owner index path first
/// persists the exact selection intent through `publish_checked_remote`, then the owner retry
/// worker performs the retirement handshake from the journal proof.
pub(crate) struct CheckedRemoteCompilerCandidate {
    pub(crate) admitted: AdmittedRemoteCompilerCandidate,
    pub(crate) pending: PendingStoredCompilerResult,
    _output_pin: PinnedStoredClosureReceipt,
    output_transfer_bytes: u64,
    output_transfer_elapsed_ms: u64,
    pub(crate) cost_model_updated: bool,
}

impl CheckedRemoteCompilerCandidate {
    /// Separates the typed candidate and ACK token from the GC pin while allowing callers to
    /// keep that pin alive across the complete Turso publication callback.
    pub(crate) fn into_publication_parts(
        self,
    ) -> (
        AdmittedRemoteCompilerCandidate,
        PendingStoredCompilerResult,
        PinnedStoredClosureReceipt,
    ) {
        (self.admitted, self.pending, self._output_pin)
    }
}

/// Exact worker result retained until its journal-authorized terminal disposition is applied.
#[derive(Clone, Debug)]
pub(crate) struct PendingStoredCompilerResult {
    pub(crate) assignment: CompilerAssignment,
    pub(crate) namespace_id: [u8; 16],
    pub(crate) worker_address: EndpointAddr,
    pub(crate) worker_grant: TrustedCompilerWorkerGrant,
    pub(crate) stored: backend_engine::compiler_cluster_transport::StoredRemoteCompilerResult,
}

/// Exact assignment and result identity supplied to the durable owner journal before a terminal
/// result disposition is sent to the worker.
#[derive(Clone, Debug)]
pub(crate) struct CompilerResultIdentity {
    /// Exact scheduler assignment whose output is being accepted or rejected.
    pub(crate) assignment: CompilerAssignment,
    /// Namespace bound by the Turso attempt and worker control scope.
    pub(crate) namespace_id: [u8; 16],
    /// Persisted trusted worker endpoint and invocation grant.
    pub(crate) worker_grant: TrustedCompilerWorkerGrant,
    /// Nonzero result closure announced on the exact authenticated assignment.
    pub(crate) closure_id: [u8; 32],
}

/// Synchronous journal boundary events emitted by owner dispatch.
///
/// Implementations must fsync a terminal intent on `PrepareDisposition` before returning. The
/// `WorkerRetired` event follows exact authenticated receipt validation and must durably advance
/// the row before dispatch sends the final confirmation. `OfferMayBeSent` distinguishes an
/// unoffered reservation from an assignment whose worker may produce a retained result.
#[derive(Clone, Copy, Debug)]
pub(crate) enum CompilerResultJournalEvent<'a> {
    /// The coordinator is about to write the V2 Offer frame.
    OfferMayBeSent {
        /// Exact reservation scope becoming potentially visible to the worker.
        assignment: CompilerAssignment,
        /// Exact namespace bound to the assignment.
        namespace_id: [u8; 16],
    },
    /// An authenticated worker terminal message proves this assignment has no retained result.
    NoResultTerminal {
        /// Exact completed or rejected assignment.
        assignment: CompilerAssignment,
        /// Exact namespace bound to the assignment.
        namespace_id: [u8; 16],
    },
    /// A cold recovery query found no running or retained result after the worker durably fenced
    /// delayed Offers for this exact scope.
    RecoveredNoResultTerminal {
        /// Exact recovered assignment.
        assignment: CompilerAssignment,
        /// Exact namespace bound to the assignment.
        namespace_id: [u8; 16],
        /// Authenticated worker status proving the durable no-result terminal state.
        status: &'a backend_engine::cluster_transport::ControlResultRecoveryStatus,
    },
    /// A terminal disposition must be persisted before its ResultAck is sent.
    PrepareDisposition {
        /// Exact announced worker result.
        identity: &'a CompilerResultIdentity,
        /// Closed terminal disposition selected by the owner.
        disposition: ResultAckDisposition,
    },
    /// The exact worker retirement receipt must be fsynced before Confirm is sent.
    WorkerRetired {
        /// Exact announced worker result.
        identity: &'a CompilerResultIdentity,
        /// Authenticated receipt returned on the same scope-bound channel.
        retired: &'a ControlResultRetired,
    },
}

fn notify_compiler_result_journal(
    journal_hook: &mut impl FnMut(
        CompilerResultJournalEvent<'_>,
    ) -> Result<
        Option<crate::builtin::pending_stored::PreparedResultDisposition>,
        String,
    >,
    event: CompilerResultJournalEvent<'_>,
) -> Result<(), ClusterDispatchError> {
    match journal_hook(event) {
        Ok(None) => Ok(()),
        Ok(Some(_)) => Err(ClusterDispatchError::Store(
            "journal returned a prepared-result token for a non-prepare event".into(),
        )),
        Err(error) => Err(ClusterDispatchError::Store(format!(
            "persist compiler result journal transition: {error}"
        ))),
    }
}

/// Proof that an exact journal-bound recovery acknowledgement completed on the worker stream.
///
/// The constructor stays private so only `acknowledge_recovered` can authorize journal deletion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RecoveredAckReceipt {
    journal_id: [u8; 32],
    kind: crate::builtin::pending_stored::RecoveredAckKind,
}

impl RecoveredAckReceipt {
    /// Exact pending journal row whose terminal ACK was fully sent and closed.
    #[must_use]
    pub(crate) const fn journal_id(self) -> [u8; 32] {
        self.journal_id
    }

    /// Terminal disposition authorized by the sealed authority proof.
    #[must_use]
    pub(crate) const fn kind(self) -> crate::builtin::pending_stored::RecoveredAckKind {
        self.kind
    }
}

/// Owner measurements supplied to the remote-cost model after a complete authenticated probe.
///
/// The execution scheduler intentionally does not infer work history from worker configuration.
/// Implementations must combine these live values with an owner-local measured execution model;
/// returning `None` makes the worker ineligible for this attempt.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OwnerRemoteCompilerProbeMeasurements {
    /// Exact captured work identity.
    pub work: CompilerWorkIdentity,
    /// Owner request including typed resource demand and local cost estimate.
    pub request: CompilerBalancingRequest,
    /// True only when the exact local recipe/size/source-count bucket has a live sample.
    pub local_cost_is_measured: bool,
    /// Authenticated worker endpoint.
    pub peer: CompilerPeerId,
    /// Process incarnation returned by the current worker snapshot.
    pub worker_incarnation: [u8; 32],
    /// Capacity revision returned by the current worker snapshot.
    pub capacity_revision: [u8; 16],
    /// Owner-verified input closure inventory member count.
    pub input_objects: u32,
    /// Owner-verified complete input closure bytes.
    pub input_payload_bytes: u64,
    /// Worker CAS-verified exact Have member count.
    pub have_objects: u32,
    /// Exact bytes already present in worker CAS.
    pub have_payload_bytes: u64,
    /// Exact missing input object count that the owner can stream from this closure.
    pub missing_objects: u32,
    /// Exact missing input bytes that the owner can stream from this closure.
    pub missing_payload_bytes: u64,
    /// Conservative lower bound of challenge and bitmap bytes exchanged over Iroh.
    pub minimum_probe_control_bytes: u64,
    /// Owner-measured wall time for connection, all challenge pages, and responses in ms.
    pub probe_elapsed_ms: u64,
    /// Current authenticated worker resource limits and out-of-scheduler use.
    pub total: CompilerResourceCredits,
    /// Current authenticated worker resource limits and out-of-scheduler use.
    pub busy: CompilerResourceCredits,
    /// Whether the exact requested portable session affinity is warm.
    pub session_affinity_warm: bool,
    /// Owner-local start of the current probe observation window.
    pub observed_at: u64,
    /// Exclusive expiry bounded by both the probe and worker capacity lease.
    pub expires_at: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RouteCostSample {
    peer: [u8; 32],
    recipe: [u8; 32],
    transfer_bytes_per_second: u64,
    remote_millis_per_mib: u64,
    observed_at: u64,
    sample_count: u32,
    last_exploration_at: u64,
}

#[derive(Default)]
struct RouteCostState {
    rows: BTreeMap<([u8; 32], [u8; 32]), RouteCostSample>,
}

struct OwnerRemoteCompilerCostModel {
    path: PathBuf,
    state: Mutex<RouteCostState>,
    local_path: PathBuf,
    local_state: Mutex<LocalCostState>,
    local_host: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LocalCostSample {
    host: [u8; 32],
    recipe: [u8; 32],
    input_payload_bytes: u64,
    source_artifact_count: u32,
    elapsed_ms: u64,
    observed_at: u64,
    sample_count: u32,
}

#[derive(Default)]
struct LocalCostState {
    rows: BTreeMap<([u8; 32], [u8; 32], u64, u32), LocalCostSample>,
}

#[derive(Clone, Copy)]
struct PendingRemoteCostProbe {
    peer: [u8; 32],
    recipe: [u8; 32],
    cost: RemoteCompilerCostClaim,
    input_payload_bytes: u64,
    probe_elapsed_ms: u64,
    estimated_input_transfer_ms: u64,
    placement_total_ms: u64,
    exploratory: bool,
}

#[derive(Clone, Copy)]
struct RemoteRouteEstimate {
    cost: RemoteCompilerCostClaim,
    placement_total_ms: u64,
    exploratory: bool,
}

/// Cost-only observation from one completed local compile. It carries no source-authority
/// witness and cannot be used to admit or publish compiler output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LocalCompilerCostObservation {
    recipe: [u8; 32],
    input_payload_bytes: u64,
    source_artifact_count: u32,
    elapsed_ms: u64,
}

impl LocalCompilerCostObservation {
    pub(crate) fn new(
        recipe: [u8; 32],
        input_payload_bytes: u64,
        source_artifact_count: u32,
        elapsed_ms: u64,
    ) -> Result<Self, ClusterDispatchError> {
        if recipe == [0; 32]
            || input_payload_bytes == 0
            || source_artifact_count == 0
            || elapsed_ms == 0
        {
            return Err(ClusterDispatchError::CostModel(
                "local timing observation requires exact nonzero recipe, size, count, and duration"
                    .into(),
            ));
        }
        Ok(Self {
            recipe,
            input_payload_bytes,
            source_artifact_count,
            elapsed_ms: elapsed_ms.min(ROUTE_COST_MAX_COMPONENT_MS),
        })
    }
}

/// Source of the local completion-cost value supplied to placement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LocalCostProvenance {
    /// Owner-persisted measurements from completed local compiler work.
    Measured { sample_count: u32, observed_at: u64 },
    /// Conservative bounded prior used only when local history is cold or stale.
    ConservativePrior,
}

/// Owner-derived local completion estimate with explicit evidence confidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LocalCostEstimate {
    recipe: [u8; 32],
    input_payload_bytes: u64,
    source_artifact_count: u32,
    completion: backend_engine::CompletionCost,
    confidence_per_mille: u16,
    provenance: LocalCostProvenance,
}

impl LocalCostEstimate {
    #[must_use]
    pub(crate) const fn completion(self) -> backend_engine::CompletionCost {
        self.completion
    }

    #[must_use]
    pub(crate) const fn confidence_per_mille(self) -> u16 {
        self.confidence_per_mille
    }

    #[must_use]
    pub(crate) const fn provenance(self) -> LocalCostProvenance {
        self.provenance
    }

    const fn is_measured(self) -> bool {
        matches!(self.provenance, LocalCostProvenance::Measured { .. })
    }
}

/// Cost facts retained with either a remote selection or a local fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompilerDispatchCostEvidence {
    pub(crate) local: LocalCostEstimate,
    pub(crate) remote: Option<RemoteCompilerCostClaim>,
    pub(crate) remote_placement_total_ms: Option<u64>,
    pub(crate) exploratory: bool,
    pub(crate) remote_route_provenance: Option<CompilerRemoteRouteProvenance>,
}

/// Why a remote assignment was selected after live probe and cost admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompilerRemoteRouteProvenance {
    /// Persisted owner measurements show a lower remote completion cost.
    MeasuredAdvantage,
    /// A bounded first-use assignment uses an exact local sample and a conservative prior.
    BoundedInteractiveExploration,
    /// A bounded background assignment calibrates an unseen or stale route.
    BoundedBackgroundExploration,
}

/// Outcome of owner placement with a reviewable reason when local execution remains selected.
pub(crate) enum CompilerDispatchDecision {
    /// The scheduler reserved the exact live remote worker route.
    Remote {
        assignment: CompilerAssignmentLease,
        evidence: CompilerDispatchCostEvidence,
    },
    /// Keep the current Turso attempt on the local compiler path.
    LocalFallback {
        reason: CompilerLocalFallbackReason,
        evidence: CompilerDispatchCostEvidence,
    },
    /// Neither a remote route nor local execution is currently available.
    OfflineUnavailable {
        reason: CompilerLocalFallbackReason,
        evidence: CompilerDispatchCostEvidence,
    },
}

/// RAII owner of a scheduler reservation between live placement and Offer execution.
///
/// Product admission can fail after placement while validating the exact V2 capture and current
/// trust grant. Keeping the reservation in this lease makes every such early return retire only
/// its exact `(work, attempt, fence)` row. Once the runner completes the remote assignment,
/// `cancel` returns stale and cannot affect any newer assignment.
pub(crate) struct CompilerAssignmentLease {
    assignment: CompilerAssignment,
    scheduler: CompilerClusterScheduler,
    pending_cost_probes: PendingCostProbeMap,
    namespace_id: [u8; 16],
}

impl CompilerAssignmentLease {
    fn new(
        assignment: CompilerAssignment,
        scheduler: CompilerClusterScheduler,
        pending_cost_probes: PendingCostProbeMap,
        namespace_id: [u8; 16],
    ) -> Self {
        Self {
            assignment,
            scheduler,
            pending_cost_probes,
            namespace_id,
        }
    }

    /// Reads the exact assignment for capture and trust admission before transferring the lease.
    #[must_use]
    pub(crate) const fn assignment(&self) -> CompilerAssignment {
        self.assignment
    }
}

impl Drop for CompilerAssignmentLease {
    fn drop(&mut self) {
        let _ = self.scheduler.cancel(self.assignment);
        self.pending_cost_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&pending_cost_probe_key(self.namespace_id, self.assignment));
    }
}

/// Assignment reconstructed only after locald revalidates the durable Turso attempt, source
/// observation, captured V2 closure, and current worker grant.
pub(crate) struct RecoveredCompilerAssignment {
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    capture: backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
    input_admission: VerifiedCompilerInputAdmission,
    worker_grant: TrustedCompilerWorkerGrant,
    deadline_unix_ms: u64,
}

impl RecoveredCompilerAssignment {
    /// Exact worker route reconstructed from the journal's original scope.
    #[must_use]
    pub(crate) const fn assignment(&self) -> CompilerAssignment {
        self.assignment
    }

    /// Exact Turso namespace tied to the durable assignment.
    #[must_use]
    pub(crate) const fn namespace_id(&self) -> [u8; 16] {
        self.namespace_id
    }

    /// Retains the pinned V2 capture used for re-admission and output selection.
    #[must_use]
    pub(crate) const fn capture(
        &self,
    ) -> &backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2 {
        &self.capture
    }

    /// Exact current source/trust admission bound to the reconstructed assignment.
    #[must_use]
    pub(crate) const fn input_admission(&self) -> &VerifiedCompilerInputAdmission {
        &self.input_admission
    }

    /// Persisted worker grant revalidated against the live private trust policy.
    #[must_use]
    pub(crate) const fn worker_grant(&self) -> &TrustedCompilerWorkerGrant {
        &self.worker_grant
    }
}

/// Result of reconciling one persisted OfferMayBeSent reservation.
pub(crate) enum RecoveredCompilerAssignmentOutcome {
    /// The retained result passed the same bounded closure and semantic admission as a live run.
    Candidate(CheckedRemoteCompilerCandidate),
    /// The worker durably fenced the scope and proved that no result exists.
    NoResult,
    /// Worker execution is still in progress; the exact reservation must remain durable.
    Running,
    /// Worker reports a prior terminal retirement; locald must reconcile its exact journal row.
    Retired(backend_engine::cluster_transport::ControlResultRetired),
}

/// Outcome of stale-source reconciliation for an exact, previously offered assignment.
#[derive(Clone, Copy, Debug)]
pub(crate) enum StaleOfferedAssignmentOutcome {
    /// A Pending result was durably rejected and the worker completed its retirement handshake.
    RejectedAdmission,
    /// A Pending result was proven superseded and durably retired with Rejected(Scope).
    RejectedSuperseded,
    /// The worker is still executing; keep the durable reservation and retry later.
    Running,
    /// The worker durably proved no result exists and fenced delayed Offers.
    NoResult,
    /// A prior terminal ACK exists and must be reconciled against locald's authority journal.
    Retired(backend_engine::cluster_transport::ControlResultRetired),
}

fn pending_cost_probe_key(
    namespace_id: [u8; 16],
    assignment: CompilerAssignment,
) -> PendingCostProbeKey {
    (
        namespace_id,
        assignment.work().transfer_work_id(),
        assignment.token().attempt().get(),
        assignment.token().fence().as_bytes(),
    )
}

/// Why a live remote route was not selected for the exact attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompilerLocalFallbackReason {
    /// Interactive local-first budget is still open and local execution can start.
    InteractiveLocalFirst,
    /// A checked local route has a lower predicted completion cost.
    LocalCostLower,
    /// No trusted worker answered a currently valid exact probe.
    NoEligibleWorker,
    /// A cold or stale peer is outside its bounded background exploration window.
    ExplorationNotDue,
    /// The conservative remote estimate does not fit the exact request deadline.
    RemoteCostMissesDeadline,
    /// Cold route exploration exceeds measured local interactive latency by too much.
    RemoteCostExceedsInteractiveLatencyBudget,
    /// No owner-measured model or safe cold-start proxy fits the current deadline.
    CostEstimateUnavailable,
    /// The exact request missed its end-to-end deadline before placement.
    DeadlineExpired,
    /// The bounded owner scheduler has no free remote assignment slot.
    RemoteCapacityFull,
    /// A bounded background trial was reserved to calibrate this exact worker recipe.
    BackgroundExploration,
    /// Durable NoResult cleanup could not be recorded before remote work became visible.
    NoResultMaintenanceUnavailable,
}

impl OwnerRemoteCompilerCostModel {
    fn open(
        path: PathBuf,
        local_path: PathBuf,
        local_host: [u8; 32],
    ) -> Result<Self, ClusterDispatchError> {
        let state = match fs::symlink_metadata(&path) {
            Ok(_) => {
                let mut file = open_private_read(&path)
                    .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
                let mut bytes = Vec::new();
                file.by_ref()
                    .take((ROUTE_COST_MODEL_MAX_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
                if bytes.len() > ROUTE_COST_MODEL_MAX_BYTES {
                    return Err(ClusterDispatchError::CostModel(
                        "model file exceeds its size bound".into(),
                    ));
                }
                decode_route_cost_state(&bytes)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => RouteCostState::default(),
            Err(error) => return Err(ClusterDispatchError::CostModel(error.to_string())),
        };
        let local_state = match fs::symlink_metadata(&local_path) {
            Ok(_) => {
                let mut file = open_private_read(&local_path)
                    .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
                let mut bytes = Vec::new();
                file.by_ref()
                    .take((LOCAL_COST_MODEL_MAX_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
                    .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
                if bytes.len() > LOCAL_COST_MODEL_MAX_BYTES {
                    return Err(ClusterDispatchError::CostModel(
                        "local model file exceeds its size bound".into(),
                    ));
                }
                decode_local_cost_state(&bytes)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => LocalCostState::default(),
            Err(error) => return Err(ClusterDispatchError::CostModel(error.to_string())),
        };
        if local_host == [0; 32] {
            return Err(ClusterDispatchError::CostModel(
                "local host identity is invalid".into(),
            ));
        }
        Ok(Self {
            path,
            state: Mutex::new(state),
            local_path,
            local_state: Mutex::new(local_state),
            local_host,
        })
    }

    fn estimate_local(
        &self,
        recipe: [u8; 32],
        input_payload_bytes: u64,
        source_artifact_count: u32,
        now: u64,
    ) -> LocalCostEstimate {
        let key = (
            self.local_host,
            recipe,
            input_payload_bytes,
            source_artifact_count,
        );
        let state = self
            .local_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sample = state.rows.get(&key).copied().filter(|sample| {
            sample.sample_count > 0
                && now >= sample.observed_at
                && now.saturating_sub(sample.observed_at) <= LOCAL_COST_SAMPLE_MAX_AGE_MS
        });
        let (execution, confidence_per_mille, provenance) = if let Some(sample) = sample {
            let age_days = now.saturating_sub(sample.observed_at) / (24 * 60 * 60 * 1_000);
            let confidence = 700_u16
                .saturating_add(
                    u16::try_from(sample.sample_count.min(10))
                        .unwrap_or(10)
                        .saturating_mul(30),
                )
                .saturating_sub(
                    u16::try_from(age_days)
                        .unwrap_or(u16::MAX)
                        .saturating_mul(25),
                )
                .clamp(500, 1_000);
            (
                sample.elapsed_ms.clamp(1, ROUTE_COST_MAX_COMPONENT_MS),
                confidence,
                LocalCostProvenance::Measured {
                    sample_count: sample.sample_count,
                    observed_at: sample.observed_at,
                },
            )
        } else {
            let by_payload =
                millis_for_mib(LOCAL_COST_PRIOR_MILLIS_PER_MIB, input_payload_bytes.max(1))
                    .unwrap_or(ROUTE_COST_MAX_COMPONENT_MS);
            let by_artifact = u64::from(source_artifact_count.max(1))
                .saturating_mul(LOCAL_COST_PRIOR_MILLIS_PER_ARTIFACT);
            (
                by_payload
                    .max(by_artifact)
                    .clamp(1, ROUTE_COST_MAX_COMPONENT_MS),
                250,
                LocalCostProvenance::ConservativePrior,
            )
        };
        LocalCostEstimate {
            recipe,
            input_payload_bytes,
            source_artifact_count,
            completion: backend_engine::CompletionCost {
                execution,
                ..backend_engine::CompletionCost::default()
            },
            confidence_per_mille,
            provenance,
        }
    }

    fn record_local_observation(
        &self,
        observation: LocalCompilerCostObservation,
        now: u64,
    ) -> Result<(), ClusterDispatchError> {
        let mut state = self
            .local_state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        record_local_cost_observation(&mut state, self.local_host, observation, now)?;
        self.persist_local_locked(&state)
    }

    fn persist_local_locked(&self, state: &LocalCostState) -> Result<(), ClusterDispatchError> {
        let bytes = encode_local_cost_state(state)?;
        let parent = self.local_path.parent().ok_or_else(|| {
            ClusterDispatchError::CostModel("local model path has no parent".into())
        })?;
        ensure_private_directory(parent)
            .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
        write_private_atomic(&self.local_path, &bytes)
            .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))
    }

    fn estimate(
        &self,
        measurements: &OwnerRemoteCompilerProbeMeasurements,
    ) -> Option<RemoteRouteEstimate> {
        let now = measurements.observed_at;
        let key = (
            measurements.peer.as_bytes(),
            *measurements.work.recipe().as_ref(),
        );
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let sample = state.rows.get(&key).copied();
        let fresh = sample.filter(|sample| {
            sample.sample_count > 0
                && now >= sample.observed_at
                && now.saturating_sub(sample.observed_at) <= ROUTE_COST_SAMPLE_MAX_AGE_MS
        });
        let exploratory = fresh.is_none();
        let exploration_due = route_exploration_due(sample, now);
        if exploratory
            && !cold_exploration_is_allowed(
                measurements.request.demand,
                measurements.local_cost_is_measured,
                exploration_due,
            )
        {
            return None;
        }

        let transfer_bytes_per_second = fresh
            .map_or(ROUTE_COST_PRIOR_TRANSFER_BYTES_PER_SECOND, |sample| {
                sample.transfer_bytes_per_second
            })
            .clamp(
                ROUTE_COST_MIN_TRANSFER_BYTES_PER_SECOND,
                ROUTE_COST_MAX_TRANSFER_BYTES_PER_SECOND,
            );
        let total_cpu = measurements.total.cpu.millicores();
        let busy_cpu = measurements.busy.cpu.millicores();
        let total_memory = measurements.total.memory.bytes();
        let busy_memory = measurements.busy.memory.bytes();
        let available_cpu = total_cpu.saturating_sub(busy_cpu);
        let available_memory = total_memory.saturating_sub(busy_memory);
        if measurements.request.resources.cpu.millicores() > available_cpu
            || measurements.request.resources.memory.bytes() > available_memory
            || measurements.request.resources.transfer.bytes()
                < measurements
                    .missing_payload_bytes
                    .saturating_add(measurements.work.max_output_bytes())
        {
            return None;
        }

        let input_transfer = transfer_millis(
            measurements.missing_payload_bytes,
            transfer_bytes_per_second,
        )?
        .saturating_mul(2)
        .saturating_add(
            u64::from(measurements.missing_objects)
                .saturating_mul(2)
                .min(ROUTE_COST_MAX_COMPONENT_MS),
        );
        let control_transfer = transfer_millis(
            measurements.minimum_probe_control_bytes,
            transfer_bytes_per_second,
        )?;
        let output_transfer = transfer_millis(
            measurements.work.max_output_bytes(),
            transfer_bytes_per_second,
        )?
        .saturating_mul(2);
        let round_trip = measurements
            .probe_elapsed_ms
            .max(10)
            .saturating_add(control_transfer)
            .saturating_add(30)
            .min(ROUTE_COST_MAX_COMPONENT_MS);
        let cpu_pressure = if total_cpu == 0 {
            1_000
        } else {
            u64::from(busy_cpu).saturating_mul(1_000) / u64::from(total_cpu)
        };
        let memory_pressure = if total_memory == 0 {
            1_000
        } else {
            busy_memory.saturating_mul(1_000) / total_memory
        };
        let worker_queue = 250_u64
            .saturating_add(cpu_pressure.saturating_mul(500) / 1_000)
            .saturating_add(memory_pressure.saturating_mul(500) / 1_000)
            .min(ROUTE_COST_MAX_COMPONENT_MS);
        let warmup = if measurements.session_affinity_warm {
            0
        } else {
            measurements.request.local_cost.warmup.max(1_000)
        };
        let execution = if let Some(sample) = fresh {
            millis_for_mib(
                sample.remote_millis_per_mib,
                measurements.input_payload_bytes.max(1),
            )?
            .saturating_add(measurements.request.local_cost.execution / 4)
        } else {
            measurements
                .request
                .local_cost
                .execution
                .saturating_mul(2)
                .max(5_000)
                .min(ROUTE_COST_MAX_COMPONENT_MS)
        };
        let completion = backend_engine::CompletionCost {
            client_queue: 0,
            round_trip,
            input_transfer: input_transfer.min(ROUTE_COST_MAX_COMPONENT_MS),
            worker_queue,
            warmup: warmup.min(ROUTE_COST_MAX_COMPONENT_MS),
            execution: execution.min(ROUTE_COST_MAX_COMPONENT_MS),
            output_transfer: output_transfer.min(ROUTE_COST_MAX_COMPONENT_MS),
            validation: measurements.request.local_cost.validation,
            contention: 0,
        };
        let total = completion.checked_total()?;
        let scheduler_contention_ms =
            completion
                .execution
                .saturating_mul(u64::from(max_probe_pressure_per_mille(
                    measurements.total,
                    measurements.busy,
                )))
                / 1_000;
        let placement_total_ms = total.checked_add(scheduler_contention_ms)?;
        let confidence_per_mille = fresh.map_or(500, |sample| {
            let samples = u16::try_from(sample.sample_count.min(10)).unwrap_or(10);
            500_u16
                .saturating_add(samples.saturating_mul(50))
                .saturating_sub(
                    u16::try_from(now.saturating_sub(sample.observed_at) / (24 * 60 * 60 * 1_000))
                        .unwrap_or(u16::MAX)
                        .saturating_mul(20),
                )
                .clamp(500, 1_000)
        });
        Some(RemoteRouteEstimate {
            cost: RemoteCompilerCostClaim {
                completion,
                confidence_per_mille,
                observed_at: measurements.observed_at,
                expires_at: measurements.expires_at,
            },
            placement_total_ms,
            exploratory,
        })
    }

    fn claim_exploration(
        &self,
        peer: [u8; 32],
        recipe: [u8; 32],
        now: u64,
    ) -> Result<bool, ClusterDispatchError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (peer, recipe);
        evict_oldest_route_cost_sample(&mut state, key);
        let row = state.rows.entry(key).or_insert(RouteCostSample {
            peer,
            recipe,
            transfer_bytes_per_second: ROUTE_COST_PRIOR_TRANSFER_BYTES_PER_SECOND,
            remote_millis_per_mib: 60_000,
            observed_at: now,
            sample_count: 0,
            last_exploration_at: 0,
        });
        if !route_exploration_due(Some(*row), now) {
            return Ok(false);
        }
        row.last_exploration_at = now;
        self.persist_locked(&state)?;
        Ok(true)
    }

    fn observe_completion(
        &self,
        probe: PendingRemoteCostProbe,
        total_elapsed_ms: u64,
        output_transfer_bytes: u64,
        output_transfer_elapsed_ms: u64,
        now: u64,
    ) -> Result<(), ClusterDispatchError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let key = (probe.peer, probe.recipe);
        evict_oldest_route_cost_sample(&mut state, key);
        let sample = state.rows.entry(key).or_insert(RouteCostSample {
            peer: probe.peer,
            recipe: probe.recipe,
            transfer_bytes_per_second: ROUTE_COST_PRIOR_TRANSFER_BYTES_PER_SECOND,
            remote_millis_per_mib: 60_000,
            observed_at: now,
            sample_count: 0,
            last_exploration_at: 0,
        });
        if sample.sample_count > 0
            && now.saturating_sub(sample.observed_at) > ROUTE_COST_SAMPLE_MAX_AGE_MS
        {
            sample.transfer_bytes_per_second = ROUTE_COST_PRIOR_TRANSFER_BYTES_PER_SECOND;
            sample.remote_millis_per_mib = 60_000;
            sample.sample_count = 0;
        }
        let remote_elapsed_ms = total_elapsed_ms
            .saturating_sub(probe.probe_elapsed_ms)
            .max(probe.estimated_input_transfer_ms)
            .max(1);
        let workload_millis_per_mib =
            millis_for_mib(remote_elapsed_ms, probe.input_payload_bytes.max(1))
                .unwrap_or(ROUTE_COST_MAX_COMPONENT_MS)
                .clamp(1, ROUTE_COST_MAX_COMPONENT_MS);
        sample.remote_millis_per_mib =
            smooth_upper_bound(sample.remote_millis_per_mib, workload_millis_per_mib);
        if output_transfer_bytes > 0 && output_transfer_elapsed_ms > 0 {
            let measured_rate = output_transfer_bytes
                .saturating_mul(1_000)
                .checked_div(output_transfer_elapsed_ms)
                .unwrap_or(ROUTE_COST_MIN_TRANSFER_BYTES_PER_SECOND)
                .clamp(
                    ROUTE_COST_MIN_TRANSFER_BYTES_PER_SECOND,
                    ROUTE_COST_MAX_TRANSFER_BYTES_PER_SECOND,
                );
            sample.transfer_bytes_per_second =
                smooth_lower_bound(sample.transfer_bytes_per_second, measured_rate);
        }
        sample.observed_at = now;
        sample.sample_count = sample.sample_count.saturating_add(1).min(1_000_000);
        self.persist_locked(&state)
    }

    fn persist_locked(&self, state: &RouteCostState) -> Result<(), ClusterDispatchError> {
        let bytes = encode_route_cost_state(state)?;
        let parent = self.path.parent().ok_or_else(|| {
            ClusterDispatchError::CostModel("model path has no parent directory".into())
        })?;
        ensure_private_directory(parent)
            .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
        write_private_atomic(&self.path, &bytes)
            .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))
    }
}

fn record_local_cost_observation(
    state: &mut LocalCostState,
    host: [u8; 32],
    observation: LocalCompilerCostObservation,
    now: u64,
) -> Result<(), ClusterDispatchError> {
    let key = (
        host,
        observation.recipe,
        observation.input_payload_bytes,
        observation.source_artifact_count,
    );
    if !state.rows.contains_key(&key) && state.rows.len() >= LOCAL_COST_MODEL_MAX_ROWS {
        let oldest = state
            .rows
            .iter()
            .min_by_key(|(_, sample)| sample.observed_at)
            .map(|(key, _)| *key);
        if let Some(oldest) = oldest {
            state.rows.remove(&oldest);
        }
    }
    let sample = state.rows.entry(key).or_insert(LocalCostSample {
        host,
        recipe: observation.recipe,
        input_payload_bytes: observation.input_payload_bytes,
        source_artifact_count: observation.source_artifact_count,
        elapsed_ms: observation.elapsed_ms,
        observed_at: now,
        sample_count: 0,
    });
    if sample.sample_count > 0
        && (now < sample.observed_at
            || now.saturating_sub(sample.observed_at) > LOCAL_COST_SAMPLE_MAX_AGE_MS)
    {
        sample.elapsed_ms = observation.elapsed_ms;
        sample.sample_count = 0;
    } else if sample.sample_count > 0 {
        sample.elapsed_ms = smooth_upper_bound(sample.elapsed_ms, observation.elapsed_ms);
    } else {
        sample.elapsed_ms = observation.elapsed_ms;
    }
    sample.observed_at = now;
    sample.sample_count = sample.sample_count.saturating_add(1).min(1_000_000);
    Ok(())
}

fn transfer_millis(bytes: u64, bytes_per_second: u64) -> Option<u64> {
    if bytes_per_second == 0 {
        return None;
    }
    bytes
        .checked_mul(1_000)?
        .checked_add(bytes_per_second - 1)?
        .checked_div(bytes_per_second)
        .map(|millis| millis.min(ROUTE_COST_MAX_COMPONENT_MS))
}

fn millis_for_mib(millis_per_mib: u64, bytes: u64) -> Option<u64> {
    millis_per_mib
        .checked_mul(bytes)?
        .checked_add(ROUTE_COST_MIB - 1)?
        .checked_div(ROUTE_COST_MIB)
        .map(|millis| millis.min(ROUTE_COST_MAX_COMPONENT_MS))
}

fn smooth_lower_bound(previous: u64, measured: u64) -> u64 {
    if measured <= previous {
        previous.saturating_mul(3).saturating_add(measured) / 4
    } else {
        previous.saturating_add(measured.saturating_sub(previous) / 16)
    }
    .clamp(
        ROUTE_COST_MIN_TRANSFER_BYTES_PER_SECOND,
        ROUTE_COST_MAX_TRANSFER_BYTES_PER_SECOND,
    )
}

fn smooth_upper_bound(previous: u64, measured: u64) -> u64 {
    if measured >= previous {
        measured
    } else {
        previous.saturating_mul(7).saturating_add(measured) / 8
    }
    .clamp(1, ROUTE_COST_MAX_COMPONENT_MS)
}

fn evict_oldest_route_cost_sample(state: &mut RouteCostState, incoming: ([u8; 32], [u8; 32])) {
    if state.rows.contains_key(&incoming) || state.rows.len() < ROUTE_COST_MODEL_MAX_ROWS {
        return;
    }
    if let Some(oldest) = state
        .rows
        .iter()
        .min_by_key(|(_, sample)| sample.observed_at)
        .map(|(key, _)| *key)
    {
        state.rows.remove(&oldest);
    }
}

fn route_exploration_due(sample: Option<RouteCostSample>, now: u64) -> bool {
    let Some(sample) = sample else {
        return true;
    };
    let has_fresh_measurement = sample.sample_count > 0
        && now >= sample.observed_at
        && now.saturating_sub(sample.observed_at) <= ROUTE_COST_SAMPLE_MAX_AGE_MS;
    !has_fresh_measurement
        && (sample.last_exploration_at == 0
            || now
                >= sample
                    .last_exploration_at
                    .saturating_add(ROUTE_COST_EXPLORATION_INTERVAL_MS))
}

fn interactive_local_first_window(
    interactive: bool,
    local_available: bool,
    submitted_at: u64,
    now: u64,
    first_budget: u64,
    local_start_delay: u64,
) -> bool {
    interactive
        && local_available
        && now >= submitted_at
        && now.saturating_sub(submitted_at) < first_budget
        && local_start_delay <= first_budget
}

fn interactive_local_cost_evidence_required(
    demand: CompilerDemand,
    local: LocalCompilerAvailability,
    local_cost: LocalCostEstimate,
) -> bool {
    demand == CompilerDemand::Interactive
        && local != LocalCompilerAvailability::Unavailable
        && !local_cost.is_measured()
}

fn cold_exploration_is_allowed(
    demand: CompilerDemand,
    exact_local_sample_exists: bool,
    route_exploration_due: bool,
) -> bool {
    route_exploration_due && (demand == CompilerDemand::Background || exact_local_sample_exists)
}

fn interactive_cold_exploration_fits_latency_budget(
    local_completion_ms: u64,
    remote_completion_ms: u64,
) -> bool {
    if local_completion_ms == 0 || remote_completion_ms == 0 {
        return false;
    }
    let additive_limit = local_completion_ms.saturating_add(INTERACTIVE_EXPLORATION_MAX_SLACK_MS);
    let ratio_limit =
        local_completion_ms.saturating_mul(INTERACTIVE_EXPLORATION_MAX_RATIO_PER_MILLE) / 1_000;
    remote_completion_ms <= additive_limit.min(ratio_limit)
}

fn route_cost_meets_deadline(now: u64, estimated_total_ms: u64, deadline: Option<u64>) -> bool {
    deadline.is_none_or(|deadline| now.saturating_add(estimated_total_ms) <= deadline)
}

fn remote_route_provenance(
    demand: CompilerDemand,
    exploratory: bool,
) -> CompilerRemoteRouteProvenance {
    match (demand, exploratory) {
        (CompilerDemand::Interactive, true) => {
            CompilerRemoteRouteProvenance::BoundedInteractiveExploration
        }
        (CompilerDemand::Background, true) => {
            CompilerRemoteRouteProvenance::BoundedBackgroundExploration
        }
        (_, false) => CompilerRemoteRouteProvenance::MeasuredAdvantage,
    }
}

fn encode_route_cost_state(state: &RouteCostState) -> Result<Vec<u8>, ClusterDispatchError> {
    if state.rows.len() > ROUTE_COST_MODEL_MAX_ROWS {
        return Err(ClusterDispatchError::CostModel(
            "model row count exceeds its bound".into(),
        ));
    }
    let mut body = Vec::with_capacity(12 + state.rows.len() * 100);
    body.extend_from_slice(ROUTE_COST_MODEL_MAGIC);
    body.extend_from_slice(&ROUTE_COST_MODEL_VERSION.to_be_bytes());
    body.extend_from_slice(
        &u16::try_from(state.rows.len())
            .map_err(|_| ClusterDispatchError::CostModel("row count overflow".into()))?
            .to_be_bytes(),
    );
    for ((peer, recipe), row) in &state.rows {
        if row.peer != *peer
            || row.recipe != *recipe
            || row.transfer_bytes_per_second == 0
            || row.remote_millis_per_mib == 0
        {
            return Err(ClusterDispatchError::CostModel(
                "model row identity or rate is invalid".into(),
            ));
        }
        body.extend_from_slice(peer);
        body.extend_from_slice(recipe);
        body.extend_from_slice(&row.transfer_bytes_per_second.to_be_bytes());
        body.extend_from_slice(&row.remote_millis_per_mib.to_be_bytes());
        body.extend_from_slice(&row.observed_at.to_be_bytes());
        body.extend_from_slice(&row.sample_count.to_be_bytes());
        body.extend_from_slice(&row.last_exploration_at.to_be_bytes());
    }
    let checksum = blake3::hash(&body);
    body.extend_from_slice(checksum.as_bytes());
    if body.len() > ROUTE_COST_MODEL_MAX_BYTES {
        return Err(ClusterDispatchError::CostModel(
            "encoded model exceeds its size bound".into(),
        ));
    }
    Ok(body)
}

fn decode_route_cost_state(bytes: &[u8]) -> Result<RouteCostState, ClusterDispatchError> {
    if bytes.len() < 12 + 32 || bytes.len() > ROUTE_COST_MODEL_MAX_BYTES {
        return Err(ClusterDispatchError::CostModel(
            "invalid model length".into(),
        ));
    }
    let checksum_offset = bytes.len() - 32;
    if blake3::hash(&bytes[..checksum_offset]).as_bytes() != &bytes[checksum_offset..] {
        return Err(ClusterDispatchError::CostModel(
            "model checksum mismatch".into(),
        ));
    }
    let mut cursor = std::io::Cursor::new(&bytes[..checksum_offset]);
    let mut magic = [0; 8];
    cursor
        .read_exact(&mut magic)
        .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
    let version = read_u16(&mut cursor)?;
    let count = usize::from(read_u16(&mut cursor)?);
    if magic != *ROUTE_COST_MODEL_MAGIC
        || version != ROUTE_COST_MODEL_VERSION
        || count > ROUTE_COST_MODEL_MAX_ROWS
    {
        return Err(ClusterDispatchError::CostModel(
            "model header is invalid".into(),
        ));
    }
    let mut rows = BTreeMap::new();
    for _ in 0..count {
        let peer = read_array::<32>(&mut cursor)?;
        let recipe = read_array::<32>(&mut cursor)?;
        let row = RouteCostSample {
            peer,
            recipe,
            transfer_bytes_per_second: read_u64(&mut cursor)?,
            remote_millis_per_mib: read_u64(&mut cursor)?,
            observed_at: read_u64(&mut cursor)?,
            sample_count: read_u32(&mut cursor)?,
            last_exploration_at: read_u64(&mut cursor)?,
        };
        if peer == [0; 32]
            || recipe == [0; 32]
            || row.transfer_bytes_per_second.clamp(
                ROUTE_COST_MIN_TRANSFER_BYTES_PER_SECOND,
                ROUTE_COST_MAX_TRANSFER_BYTES_PER_SECOND,
            ) != row.transfer_bytes_per_second
            || row.remote_millis_per_mib == 0
            || row.remote_millis_per_mib > ROUTE_COST_MAX_COMPONENT_MS
            || row.sample_count == 0 && row.last_exploration_at == 0
            || rows.insert((peer, recipe), row).is_some()
        {
            return Err(ClusterDispatchError::CostModel(
                "model row is invalid".into(),
            ));
        }
    }
    if usize::try_from(cursor.position()).ok() != Some(checksum_offset) {
        return Err(ClusterDispatchError::CostModel(
            "model has trailing bytes".into(),
        ));
    }
    Ok(RouteCostState { rows })
}

fn encode_local_cost_state(state: &LocalCostState) -> Result<Vec<u8>, ClusterDispatchError> {
    if state.rows.len() > LOCAL_COST_MODEL_MAX_ROWS {
        return Err(ClusterDispatchError::CostModel(
            "local model row count exceeds its bound".into(),
        ));
    }
    let mut body = Vec::with_capacity(12 + state.rows.len() * 96);
    body.extend_from_slice(LOCAL_COST_MODEL_MAGIC);
    body.extend_from_slice(&ROUTE_COST_MODEL_VERSION.to_be_bytes());
    body.extend_from_slice(
        &u16::try_from(state.rows.len())
            .map_err(|_| ClusterDispatchError::CostModel("local row count overflow".into()))?
            .to_be_bytes(),
    );
    for ((host, recipe, input_payload_bytes, source_artifact_count), row) in &state.rows {
        if row.host != *host
            || row.recipe != *recipe
            || row.input_payload_bytes != *input_payload_bytes
            || row.source_artifact_count != *source_artifact_count
            || *host == [0; 32]
            || *recipe == [0; 32]
            || *input_payload_bytes == 0
            || *source_artifact_count == 0
            || row.elapsed_ms == 0
            || row.elapsed_ms > ROUTE_COST_MAX_COMPONENT_MS
            || row.sample_count == 0
        {
            return Err(ClusterDispatchError::CostModel(
                "local model row identity or cost is invalid".into(),
            ));
        }
        body.extend_from_slice(host);
        body.extend_from_slice(recipe);
        body.extend_from_slice(&input_payload_bytes.to_be_bytes());
        body.extend_from_slice(&source_artifact_count.to_be_bytes());
        body.extend_from_slice(&row.elapsed_ms.to_be_bytes());
        body.extend_from_slice(&row.observed_at.to_be_bytes());
        body.extend_from_slice(&row.sample_count.to_be_bytes());
    }
    let checksum = blake3::hash(&body);
    body.extend_from_slice(checksum.as_bytes());
    if body.len() > LOCAL_COST_MODEL_MAX_BYTES {
        return Err(ClusterDispatchError::CostModel(
            "encoded local model exceeds its size bound".into(),
        ));
    }
    Ok(body)
}

fn decode_local_cost_state(bytes: &[u8]) -> Result<LocalCostState, ClusterDispatchError> {
    if bytes.len() < 12 + 32 || bytes.len() > LOCAL_COST_MODEL_MAX_BYTES {
        return Err(ClusterDispatchError::CostModel(
            "invalid local model length".into(),
        ));
    }
    let checksum_offset = bytes.len() - 32;
    if blake3::hash(&bytes[..checksum_offset]).as_bytes() != &bytes[checksum_offset..] {
        return Err(ClusterDispatchError::CostModel(
            "local model checksum mismatch".into(),
        ));
    }
    let mut cursor = std::io::Cursor::new(&bytes[..checksum_offset]);
    let mut magic = [0; 8];
    cursor
        .read_exact(&mut magic)
        .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
    let version = read_u16(&mut cursor)?;
    let count = usize::from(read_u16(&mut cursor)?);
    if magic != *LOCAL_COST_MODEL_MAGIC || version != ROUTE_COST_MODEL_VERSION {
        return Err(ClusterDispatchError::CostModel(
            "local model header is invalid".into(),
        ));
    }
    if count > LOCAL_COST_MODEL_MAX_ROWS {
        return Err(ClusterDispatchError::CostModel(
            "local model row count exceeds its bound".into(),
        ));
    }
    let mut rows = BTreeMap::new();
    for _ in 0..count {
        let host = read_array::<32>(&mut cursor)?;
        let recipe = read_array::<32>(&mut cursor)?;
        let input_payload_bytes = read_u64(&mut cursor)?;
        let source_artifact_count = read_u32(&mut cursor)?;
        let row = LocalCostSample {
            host,
            recipe,
            input_payload_bytes,
            source_artifact_count,
            elapsed_ms: read_u64(&mut cursor)?,
            observed_at: read_u64(&mut cursor)?,
            sample_count: read_u32(&mut cursor)?,
        };
        if host == [0; 32]
            || recipe == [0; 32]
            || input_payload_bytes == 0
            || source_artifact_count == 0
            || row.elapsed_ms == 0
            || row.elapsed_ms > ROUTE_COST_MAX_COMPONENT_MS
            || row.sample_count == 0
            || rows
                .insert(
                    (host, recipe, input_payload_bytes, source_artifact_count),
                    row,
                )
                .is_some()
        {
            return Err(ClusterDispatchError::CostModel(
                "local model row is invalid".into(),
            ));
        }
    }
    if usize::try_from(cursor.position()).ok() != Some(checksum_offset) {
        return Err(ClusterDispatchError::CostModel(
            "local model has trailing bytes".into(),
        ));
    }
    Ok(LocalCostState { rows })
}

fn millis_per_mib(elapsed_ms: u64, bytes: u64) -> Option<u64> {
    elapsed_ms
        .checked_mul(ROUTE_COST_MIB)?
        .checked_add(bytes.saturating_sub(1))?
        .checked_div(bytes.max(1))
        .map(|millis| millis.min(ROUTE_COST_MAX_COMPONENT_MS))
}

fn read_array<const N: usize>(
    cursor: &mut std::io::Cursor<&[u8]>,
) -> Result<[u8; N], ClusterDispatchError> {
    let mut value = [0; N];
    cursor
        .read_exact(&mut value)
        .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
    Ok(value)
}

fn read_u16(cursor: &mut std::io::Cursor<&[u8]>) -> Result<u16, ClusterDispatchError> {
    let mut bytes = [0; 2];
    cursor
        .read_exact(&mut bytes)
        .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(cursor: &mut std::io::Cursor<&[u8]>) -> Result<u32, ClusterDispatchError> {
    let mut bytes = [0; 4];
    cursor
        .read_exact(&mut bytes)
        .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(cursor: &mut std::io::Cursor<&[u8]>) -> Result<u64, ClusterDispatchError> {
    let mut bytes = [0; 8];
    cursor
        .read_exact(&mut bytes)
        .map_err(|error| ClusterDispatchError::CostModel(error.to_string()))?;
    Ok(u64::from_be_bytes(bytes))
}

#[derive(Debug)]
struct ResultGrantObject {
    mapping: backend_engine::cluster_transport::StoreObjectMapping,
    blob_hash: backend_engine::cluster_transport::BlobHash,
    grants: Vec<Capability>,
}

#[derive(Debug)]
struct OrderedResultObject {
    claim: ArtifactObjectClaim,
    object_id: [u8; 32],
    grants: Vec<Capability>,
}

struct ExactRemotePreflightVerifier(RemoteCompilerPreflightClaim);

impl CompilerRemotePreflightVerifier for ExactRemotePreflightVerifier {
    fn verify_remote_preflight(
        &self,
        expected: CompilerWorkIdentity,
        claim: RemoteCompilerPreflightClaim,
    ) -> Result<(), CompilerRemotePreflightError> {
        if claim == self.0 && claim.work == expected {
            Ok(())
        } else {
            Err(CompilerRemotePreflightError::Rejected)
        }
    }
}

struct ExactNodeCapacityVerifier(CompilerNodeCapacityClaim);

impl CompilerNodeCapacityVerifier for ExactNodeCapacityVerifier {
    fn verify_node_capacity(
        &self,
        claim: &CompilerNodeCapacityClaim,
    ) -> Result<(), CompilerNodeCapacityError> {
        if claim == &self.0 {
            Ok(())
        } else {
            Err(CompilerNodeCapacityError::Rejected)
        }
    }
}

struct OwnerControlEvent {
    channel: backend_engine::cluster_transport::ControlChannel,
    first_message: ControlMessage,
}

#[derive(Clone)]
struct OwnerClusterIngress {
    registry: OwnerClusterAdmissionRegistry,
    routes: Arc<Mutex<HashMap<AssignmentScope, mpsc::Sender<OwnerControlEvent>>>>,
}

impl OwnerClusterIngress {
    fn new(registry: OwnerClusterAdmissionRegistry) -> Self {
        Self {
            registry,
            routes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn register(
        &self,
        peer: EndpointId,
        input_scope: TransferScope,
    ) -> Result<(OwnerClusterRouteGuard, mpsc::Receiver<OwnerControlEvent>), ClusterDispatchError>
    {
        let assignment_scope = input_scope.assignment();
        let (sender, receiver) = mpsc::channel(OWNER_CONTROL_QUEUE_PER_ASSIGNMENT);
        let mut routes = self
            .routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if routes.contains_key(&assignment_scope) {
            return Err(ClusterDispatchError::AssignmentRejected);
        }
        self.registry
            .register(peer, input_scope)
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        routes.insert(assignment_scope, sender);
        Ok((
            OwnerClusterRouteGuard {
                ingress: self.clone(),
                peer,
                input_scope,
            },
            receiver,
        ))
    }

    async fn dispatch(&self, event: OwnerControlEvent) {
        let peer = event.channel.peer();
        let Some(scope) = event.channel.scope() else {
            return;
        };
        if !self.registry.authorizes_control(peer, scope) {
            return;
        }
        let sender = self
            .routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&scope)
            .cloned();
        if let Some(sender) = sender {
            let _ = sender.send(event).await;
        }
    }
}

struct OwnerClusterRouteGuard {
    ingress: OwnerClusterIngress,
    peer: EndpointId,
    input_scope: TransferScope,
}

impl Drop for OwnerClusterRouteGuard {
    fn drop(&mut self) {
        self.ingress
            .routes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.input_scope.assignment());
        self.ingress
            .registry
            .unregister(self.peer, self.input_scope);
    }
}

async fn run_owner_cluster_ingress(
    mut listener: ClusterListener,
    ingress: OwnerClusterIngress,
    artifact_state: ServerState,
    local_endpoint: PathBuf,
    remote_index_usage: RemoteIndexUsage,
) {
    let artifact_slots = Arc::new(tokio::sync::Semaphore::new(MAX_CLUSTER_ARTIFACT_SERVERS));
    let control_slots = Arc::new(tokio::sync::Semaphore::new(MAX_CLUSTER_CONTROL_CONNECTIONS));
    while let Some(event) = listener.recv().await {
        match event {
            Ok(AcceptedClusterConnection::Control(channel)) => {
                let permit = match control_slots.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => break,
                };
                let ingress = ingress.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let mut channel = channel;
                    let first_message =
                        match tokio::time::timeout(CLUSTER_IO_TIMEOUT, channel.receive()).await {
                            Ok(Ok(message)) => message,
                            Ok(Err(_)) | Err(_) => return,
                        };
                    ingress
                        .dispatch(OwnerControlEvent {
                            channel,
                            first_message,
                        })
                        .await;
                });
            }
            Ok(AcceptedClusterConnection::Artifact(connection)) => {
                let permit = match artifact_slots.clone().acquire_owned().await {
                    Ok(permit) => permit,
                    Err(_) => break,
                };
                let state = artifact_state.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = connection.serve(&state).await;
                });
            }
            Ok(AcceptedClusterConnection::RemoteIndex(connection)) => {
                let permit = match remote_semantic_query::connection_slots().try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => continue,
                };
                let endpoint = local_endpoint.clone();
                let usage = remote_index_usage.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Ok(session) = connection.accept().await {
                        remote_semantic_query::serve_connection(session, endpoint, usage).await;
                    }
                });
            }
            Ok(AcceptedClusterConnection::Probe(_)) | Err(_) => {}
        }
    }
}

impl fmt::Debug for OwnerCompilerClusterRuntime {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnerCompilerClusterRuntime")
            .field("owner_id", &self.owner_id)
            .field("endpoint", &self.owner_address)
            .field("scheduler", &self.scheduler)
            .field("trust_policy_path", &self.trust_policy_path)
            .finish_non_exhaustive()
    }
}

impl OwnerCompilerClusterRuntime {
    /// Opens the configured owner endpoint and bounded Bao catalog once for locald's lifetime.
    ///
    /// # Errors
    ///
    /// Returns a typed configuration, trust-policy, Tokio, or endpoint admission error. The
    /// endpoint is never rebound to a different identity or address after a failed open.
    pub(crate) fn open(
        owner_config: ClusterOwnerConfig,
        store: FileStore,
        trust_policy_path: impl Into<PathBuf>,
        outboard_path: impl Into<PathBuf>,
        checkpoint_root: impl Into<PathBuf>,
        local_endpoint: PathBuf,
    ) -> Result<Self, ClusterDispatchError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(ClusterDispatchError::Runtime)?;
        let expected_id = owner_config.endpoint_id();
        let secret = owner_config.secret_key();
        let bind_address = owner_config.bind_address();
        let advertised_address = owner_config.advertised_address();
        let endpoint = runtime
            .block_on(bind_direct(secret, bind_address))
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        if endpoint.id() != expected_id {
            return Err(ClusterDispatchError::EndpointIdentityMismatch);
        }

        let trust_policy_path = trust_policy_path.into();
        // Loading here validates owner-only persisted trust before the live endpoint can accept
        // compiler work. The dispatch path reloads it before each probe and result selection.
        TrustedCompilerWorkerPolicy::load(&trust_policy_path)
            .map_err(ClusterDispatchError::TrustPolicy)?;
        let authority_path = trust_policy_path
            .parent()
            .ok_or_else(|| {
                ClusterDispatchError::NoResultRetirement("trust path has no parent".into())
            })?
            .join(backend_extension_turso::AUTHORITY_FILE_NAME);
        let retirement_path = trust_policy_path
            .parent()
            .ok_or_else(|| {
                ClusterDispatchError::NoResultRetirement("trust path has no parent".into())
            })?
            .join(NO_RESULT_RETIREMENT_FILE_NAME);
        let no_result_retirement = Arc::new(
            NoResultRetirementJournal::open(&retirement_path, expected_id)
                .map_err(|error| ClusterDispatchError::NoResultRetirement(error.to_string()))?,
        );
        let no_result_retirement_notify = Arc::new(tokio::sync::Notify::new());
        let catalog = StoreBlobCatalog::new(
            store.artifact_sink(cluster_artifact_budget()),
            outboard_path.into(),
        );
        let cost_model_path = trust_policy_path
            .parent()
            .ok_or_else(|| ClusterDispatchError::CostModel("trust path has no parent".into()))?
            .join(ROUTE_COST_MODEL_FILE_NAME);
        let local_cost_model_path = trust_policy_path
            .parent()
            .ok_or_else(|| ClusterDispatchError::CostModel("trust path has no parent".into()))?
            .join(LOCAL_COST_MODEL_FILE_NAME);
        let cost_model = OwnerRemoteCompilerCostModel::open(
            cost_model_path,
            local_cost_model_path,
            *expected_id.as_bytes(),
        )?;
        let admission_registry = OwnerClusterAdmissionRegistry::default();
        let ingress = OwnerClusterIngress::new(admission_registry.clone());
        let usage_path = trust_policy_path
            .parent()
            .ok_or_else(|| ClusterDispatchError::Transport("trust path has no parent".into()))?
            .join("remote-index-grants.v1");
        let remote_index_usage = match RemoteIndexUsage::open(usage_path, expected_id) {
            Ok(usage) => usage,
            Err(error) => {
                eprintln!(
                    "locald: remote read-only index is disabled because its grant budget ledger could not be admitted: {error}"
                );
                RemoteIndexUsage::disabled()
            }
        };
        let (listener, artifact_state) = runtime
            .block_on(async {
                ClusterListener::spawn_owner_router(
                    endpoint.clone(),
                    expected_id,
                    admission_registry,
                    Arc::new(catalog.clone()),
                    MAX_CLUSTER_LISTENER_QUEUE,
                )
            })
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let ingress_task = runtime.spawn(run_owner_cluster_ingress(
            listener,
            ingress.clone(),
            artifact_state,
            local_endpoint,
            remote_index_usage,
        ));
        let no_result_retirement_task = runtime.spawn(run_no_result_retirement_collector(
            Arc::clone(&no_result_retirement),
            Arc::clone(&no_result_retirement_notify),
            endpoint.clone(),
            authority_path.clone(),
            trust_policy_path.clone(),
            expected_id,
        ));
        let owner_address = EndpointAddr::new(expected_id).with_ip_addr(advertised_address);
        Ok(Self {
            runtime,
            endpoint,
            owner_address,
            issuer: CapabilityIssuer::new(owner_config.secret_key()),
            owner_id: expected_id,
            scheduler: backend_engine::application::CompilerClusterScheduler::with_balancing_limits(
                NonZeroUsize::new(REMOTE_ASSIGNMENT_CAPACITY)
                    .ok_or(ClusterDispatchError::NoEligibleWorker)?,
                NonZeroUsize::new(REMOTE_ASSIGNMENT_CAPACITY)
                    .ok_or(ClusterDispatchError::NoEligibleWorker)?,
                CompilerResourceCredits::default(),
                0,
            ),
            store,
            catalog,
            trust_policy_path,
            checkpoint_root: checkpoint_root.into(),
            cost_model,
            pending_cost_probes: Arc::new(Mutex::new(HashMap::new())),
            authority_path,
            retained_compiler_captures: Mutex::new(RetainedCompilerCaptureCache::default()),
            no_result_retirement,
            no_result_retirement_notify,
            _no_result_retirement_task: no_result_retirement_task,
            ingress,
            _ingress_task: ingress_task,
            pending_ack_retry: Mutex::new(None),
        })
    }

    /// Opens the owner cluster when the persisted owner identity exists, otherwise retains the
    /// local-only profile without manufacturing endpoint or worker authority.
    pub(crate) fn open_if_configured(
        workspace: &Path,
        store: FileStore,
        local_endpoint: &Path,
    ) -> Result<Option<Self>, ClusterDispatchError> {
        let owner_path = workspace.join(OWNER_CONFIG_FILE_NAME);
        match fs::symlink_metadata(&owner_path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ClusterDispatchError::Runtime(error)),
        }
        let owner_config =
            ClusterOwnerConfig::load(&owner_path).map_err(ClusterDispatchError::OwnerConfig)?;
        let trust_policy_path = workspace.join(TRUSTED_COMPILER_POLICY_FILE_NAME);
        let outboard_path = workspace.join(CLUSTER_OUTBOARD_DIRECTORY);
        let checkpoint_root = workspace.join(CLUSTER_CHECKPOINT_DIRECTORY);
        Self::open(
            owner_config,
            store,
            trust_policy_path,
            outboard_path,
            checkpoint_root,
            local_endpoint.to_path_buf(),
        )
        .map(Some)
    }

    /// Returns the owner's authenticated endpoint ID for an owner-signed capability or probe.
    #[must_use]
    pub(crate) const fn owner_id(&self) -> EndpointId {
        self.owner_id
    }

    /// Returns the persisted direct address workers must use to connect to this owner.
    #[must_use]
    pub(crate) fn owner_address(&self) -> &EndpointAddr {
        &self.owner_address
    }

    /// Returns the owner's durable compiler CAS handle for exact closure admission.
    #[must_use]
    pub(crate) const fn store(&self) -> &FileStore {
        &self.store
    }

    /// Clones the exact retained capture as an optional persistent-tree update base.
    pub(crate) fn prior_compiler_capture(
        &self,
        key: CompilerInputCaptureCacheKeyV2,
    ) -> Option<CapturedFullWorkspaceV2> {
        self.retained_compiler_captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
    }

    /// Retains one bounded capture for warm path-copy updates under the same authority tuple.
    pub(crate) fn remember_compiler_capture(&self, capture: CapturedFullWorkspaceV2) {
        self.retained_compiler_captures
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(capture);
    }

    /// Scheduler for this owner's active assignment and completion fence checks.
    #[must_use]
    pub(crate) const fn scheduler(&self) -> &CompilerClusterScheduler {
        &self.scheduler
    }

    /// Reloads persisted worker trust for each live route decision.
    pub(crate) fn trusted_workers(
        &self,
    ) -> Result<TrustedCompilerWorkerPolicy, ClusterDispatchError> {
        TrustedCompilerWorkerPolicy::load(&self.trust_policy_path)
            .map_err(ClusterDispatchError::TrustPolicy)
    }

    /// Estimates local completion for one exact recipe from the owner's measured model or its
    /// explicitly low-confidence cold prior.
    pub(crate) fn local_cost_estimate(
        &self,
        recipe: [u8; 32],
        input_payload_bytes: u64,
        source_artifact_count: u32,
    ) -> Result<LocalCostEstimate, ClusterDispatchError> {
        if input_payload_bytes == 0 || source_artifact_count == 0 {
            return Err(ClusterDispatchError::CostModel(
                "local estimate requires nonzero input bytes and source count".into(),
            ));
        }
        Ok(self.cost_model.estimate_local(
            recipe,
            input_payload_bytes,
            source_artifact_count,
            current_unix_ms()?,
        ))
    }

    /// Persists cost-only timing from a local compiler run, without treating it as an authority
    /// proof for the partial local source set or its output.
    pub(crate) fn record_local_observation(
        &self,
        observation: LocalCompilerCostObservation,
    ) -> Result<(), ClusterDispatchError> {
        self.cost_model
            .record_local_observation(observation, current_unix_ms()?)
    }

    /// Starts the owner-lifetime retry loop for durable worker result ACKs.
    ///
    /// The factory runs on the dedicated OS thread and may construct thread-local authority or
    /// journal handles that are not `Send`. Its worker reports `Progress` after one durable row
    /// transition, `TransientFailure` when a pending row cannot be resolved, and `Idle` when none
    /// remain. Each sweep must be bounded (preferably to one row) and avoid holding locald's
    /// command-adapter lock across authority queries or network I/O. Repeated calls are idempotent.
    pub(crate) fn start_pending_ack_retry_worker<F, W>(
        &self,
        factory: F,
    ) -> Result<(), ClusterDispatchError>
    where
        F: FnOnce() -> W + Send + 'static,
        W: FnMut() -> PendingAckRetryOutcome + 'static,
    {
        let mut worker = self
            .pending_ack_retry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if worker.is_none() {
            *worker =
                Some(PendingAckRetryWorker::start(factory).map_err(ClusterDispatchError::Runtime)?);
        }
        Ok(())
    }

    /// Interrupts the retry backoff after a journal transition makes work immediately actionable.
    pub(crate) fn wake_pending_ack_retry_worker(&self) {
        if let Some(worker) = self
            .pending_ack_retry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            worker.wake();
        }
    }

    /// Rebuilds a remote assignment only from locald's already revalidated cold-recovery facts.
    ///
    /// The caller must first prove the same current Turso attempt/fence and source observation,
    /// reopen the exact V2 capture with a retained GC pin, and revalidate current worker trust.
    /// This method repeats the store, admission, grant, and scope bindings before returning the
    /// private-field wrapper consumed by `recover_offered_assignment`.
    pub(crate) fn recover_assignment(
        &self,
        work: CompilerWorkIdentity,
        token: CompilerAttemptToken,
        peer: CompilerPeerId,
        expected_scope: AssignmentScope,
        capture: backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
        input_admission: VerifiedCompilerInputAdmission,
        worker_grant: TrustedCompilerWorkerGrant,
        deadline_unix_ms: u64,
    ) -> Result<RecoveredCompilerAssignment, ClusterDispatchError> {
        let assignment = CompilerAssignment::recover_exact(
            work,
            token,
            peer,
            expected_scope.work_id,
            AttemptId::new(expected_scope.attempt)
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?,
            Fence::from_bytes(expected_scope.fence)
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?,
        )
        .map_err(|error| ClusterDispatchError::ResultRejected(error.to_string()))?;
        let namespace_id = expected_scope.namespace_id;
        if backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            assignment,
            namespace_id,
        )
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?
            != expected_scope
            || !input_admission.matches(assignment, namespace_id, &capture)
            || capture.verify_in_store(&self.store).is_err()
            || deadline_unix_ms == 0
        {
            return Err(ClusterDispatchError::InputRejected);
        }
        let current_grant = self.resolve_trusted_worker(&input_admission)?;
        if current_grant != worker_grant
            || worker_grant.peer().as_bytes() != &peer.as_bytes()
            || worker_grant.namespace_id() != namespace_id
        {
            return Err(ClusterDispatchError::NoEligibleWorker);
        }
        Ok(RecoveredCompilerAssignment {
            assignment,
            namespace_id,
            capture,
            input_admission,
            worker_grant,
            deadline_unix_ms,
        })
    }

    /// Reconciles one durable OfferMayBeSent row using the exact recovered V2 capture and worker
    /// trust facts. It never sends a second Offer. Pending output follows the same page, Bao, CAS,
    /// and semantic admission path as a live result; an authenticated NoResult status releases
    /// the reservation only after the journal hook durably records that terminal proof.
    pub(crate) fn recover_offered_assignment(
        &self,
        scheduler: &CompilerClusterScheduler,
        recovered: RecoveredCompilerAssignment,
        mut journal_hook: impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<RecoveredCompilerAssignmentOutcome, ClusterDispatchError> {
        let assignment = recovered.assignment;
        let expected_peer = EndpointId::from_bytes(recovered.worker_grant.peer().as_bytes())
            .map_err(|_| ClusterDispatchError::InputRejected)?;
        let expected_scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            assignment,
            recovered.namespace_id,
        )
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let route = TransferScope::from_store_closure(expected_scope, recovered.capture.closure())
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let (_route_guard, mut control_events) = self.ingress.register(expected_peer, route)?;
        let mut announced_result = None;
        let result = self.block_on(async {
            tokio::time::timeout(
                RESULT_RECOVERY_QUERY_TIMEOUT
                    .saturating_add(RECOVERED_RUNNING_POLL_WINDOW)
                    .saturating_add(Duration::from_millis(RECOVERED_RESULT_TRANSFER_WINDOW_MS)),
                self.recover_offered_assignment_async(
                    scheduler,
                    recovered,
                    expected_peer,
                    expected_scope,
                    &mut announced_result,
                    &mut journal_hook,
                    &mut control_events,
                ),
            )
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
        });
        match result {
            Ok(candidate) => Ok(candidate),
            Err(error) => {
                if is_conclusive_result_rejection(&error)
                    && let Some(identity) = announced_result
                {
                    self.block_on(tokio::time::timeout(
                        CLUSTER_IO_TIMEOUT,
                        self.retire_result_disposition(
                            &identity,
                            ResultAckDisposition::Rejected(ResultRejectReason::Admission),
                            &mut journal_hook,
                        ),
                    ))
                    .map_err(|_| ClusterDispatchError::DeadlineExpired)??;
                }
                Err(error)
            }
        }
    }

    /// Reconciles a stale or revoked offered assignment without rebuilding compiler input.
    ///
    /// The caller must already have proved the exact persisted assignment is no longer eligible
    /// under current source/Turso authority. This method permits only terminal Rejected(Admission)
    /// for an authenticated Pending result, and permits a journaled NoResult transition for a
    /// worker-authenticated no-result tombstone. It never admits, stores, or reoffers work and it
    /// intentionally does not require the old worker grant to remain authorized for new work.
    pub(crate) fn reject_stale_offered_assignment(
        &self,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        worker_grant: TrustedCompilerWorkerGrant,
        mut journal_hook: impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<StaleOfferedAssignmentOutcome, ClusterDispatchError> {
        let scope =
            validate_terminal_assignment_identity(assignment, namespace_id, &worker_grant, None)?;
        self.reject_offered_assignment_with_disposition(
            assignment,
            namespace_id,
            worker_grant,
            scope,
            ResultAckDisposition::Rejected(ResultRejectReason::Admission),
            &mut journal_hook,
        )
    }

    /// Reconciles a persisted offer that Turso has proved superseded. The opaque proof must bind
    /// this exact attempt epoch/fence and a strictly newer current epoch. Only an authenticated
    /// Pending result receives Rejected(Scope); current input admission is not reconstructed.
    pub(crate) fn reject_superseded_offered_assignment(
        &self,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        worker_grant: TrustedCompilerWorkerGrant,
        proof: &backend_extension_turso::SupersededAttemptProof,
        mut journal_hook: impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<StaleOfferedAssignmentOutcome, ClusterDispatchError> {
        let scope =
            validate_terminal_assignment_identity(assignment, namespace_id, &worker_grant, None)?;
        let token = assignment.token();
        if proof.namespace().namespace_id() != namespace_id
            || proof.epoch() != token.attempt().get()
            || *proof.fence() != token.fence().as_bytes()
            || proof.current_epoch() <= proof.epoch()
        {
            return Err(ClusterDispatchError::AckRejected(
                "superseded proof differs from the exact offered assignment".into(),
            ));
        }
        self.reject_offered_assignment_with_disposition(
            assignment,
            namespace_id,
            worker_grant,
            scope,
            ResultAckDisposition::Rejected(ResultRejectReason::Scope),
            &mut journal_hook,
        )
    }

    fn reject_offered_assignment_with_disposition(
        &self,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        worker_grant: TrustedCompilerWorkerGrant,
        scope: AssignmentScope,
        disposition: ResultAckDisposition,
        journal_hook: &mut impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<StaleOfferedAssignmentOutcome, ClusterDispatchError> {
        self.block_on(tokio::time::timeout(CLUSTER_IO_TIMEOUT, async {
            let worker_peer = EndpointId::from_bytes(worker_grant.peer().as_bytes())
                .map_err(|_| ClusterDispatchError::AckRejected("invalid worker endpoint".into()))?;
            let worker_address =
                EndpointAddr::new(worker_peer).with_ip_addr(worker_grant.address());
            let query = ControlResultRecoveryQuery::new(scope, self.owner_id)
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
            let mut channel = tokio::time::timeout(
                RESULT_RECOVERY_QUERY_TIMEOUT,
                backend_engine::cluster_transport::connect_control(
                    &self.endpoint,
                    worker_address,
                    worker_peer,
                    scope,
                    ControlRole::Coordinator,
                ),
            )
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
            if channel.peer() != worker_peer || channel.scope() != Some(scope) {
                return Err(ClusterDispatchError::AssignmentRejected);
            }
            channel
                .send(&ControlMessage::ResultRecoveryQuery(query))
                .await
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
            let status =
                match tokio::time::timeout(RESULT_RECOVERY_QUERY_TIMEOUT, channel.receive())
                    .await
                    .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                    .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?
                {
                    ControlMessage::ResultRecoveryStatus(status)
                        if status.matches_query(&query, self.owner_id, worker_peer) =>
                    {
                        status
                    }
                    _ => return Err(ClusterDispatchError::AssignmentRejected),
                };
            channel
                .finish()
                .await
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
            match status.state() {
                ControlResultRecoveryState::Running => Ok(StaleOfferedAssignmentOutcome::Running),
                ControlResultRecoveryState::NoResult => {
                    self.record_no_result_terminal(assignment, namespace_id, worker_peer)
                        .await?;
                    notify_compiler_result_journal(
                        journal_hook,
                        CompilerResultJournalEvent::RecoveredNoResultTerminal {
                            assignment,
                            namespace_id,
                            status: &status,
                        },
                    )?;
                    Ok(StaleOfferedAssignmentOutcome::NoResult)
                }
                ControlResultRecoveryState::Retired(retired) => {
                    Ok(StaleOfferedAssignmentOutcome::Retired(retired))
                }
                ControlResultRecoveryState::Pending(receipt) => {
                    let identity = CompilerResultIdentity {
                        assignment,
                        namespace_id,
                        worker_grant,
                        closure_id: receipt.closure_id,
                    };
                    self.retire_result_disposition(&identity, disposition, journal_hook)
                        .await?;
                    Ok(match disposition {
                        ResultAckDisposition::Rejected(ResultRejectReason::Admission) => {
                            StaleOfferedAssignmentOutcome::RejectedAdmission
                        }
                        ResultAckDisposition::Rejected(ResultRejectReason::Scope) => {
                            StaleOfferedAssignmentOutcome::RejectedSuperseded
                        }
                        ResultAckDisposition::Stored | ResultAckDisposition::Rejected(_) => {
                            return Err(ClusterDispatchError::AckRejected(
                                "stale offered recovery selected an unsupported disposition".into(),
                            ));
                        }
                    })
                }
            }
        }))
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
    }

    async fn record_no_result_terminal(
        &self,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        peer: EndpointId,
    ) -> Result<(), ClusterDispatchError> {
        if assignment_peer_id(assignment)? != peer {
            return Err(ClusterDispatchError::AssignmentRejected);
        }
        let scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            assignment,
            namespace_id,
        )
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let mut authority = backend_extension_turso::TursoAuthority::open(&self.authority_path)
            .await
            .map_err(|error| ClusterDispatchError::NoResultRetirement(error.to_string()))?;
        let namespace = authority
            .authority_namespace_for_id(namespace_id)
            .await
            .map_err(|error| ClusterDispatchError::NoResultRetirement(error.to_string()))?
            .ok_or_else(|| {
                ClusterDispatchError::NoResultRetirement(
                    "terminal scope has no canonical Turso namespace".into(),
                )
            })?;
        self.no_result_retirement
            .record_no_result_in_namespace(scope, peer, Some(namespace))
            .map_err(|error| ClusterDispatchError::NoResultRetirement(error.to_string()))?;
        self.no_result_retirement_notify.notify_one();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn recover_offered_assignment_async(
        &self,
        scheduler: &CompilerClusterScheduler,
        recovered: RecoveredCompilerAssignment,
        expected_peer: EndpointId,
        expected_scope: AssignmentScope,
        announced_result: &mut Option<CompilerResultIdentity>,
        journal_hook: &mut impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
        control_events: &mut mpsc::Receiver<OwnerControlEvent>,
    ) -> Result<RecoveredCompilerAssignmentOutcome, ClusterDispatchError> {
        let worker_address =
            EndpointAddr::new(expected_peer).with_ip_addr(recovered.worker_grant.address());
        let query = ControlResultRecoveryQuery::new(expected_scope, self.owner_id)
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let mut channel = tokio::time::timeout(
            RESULT_RECOVERY_QUERY_TIMEOUT,
            backend_engine::cluster_transport::connect_control(
                &self.endpoint,
                worker_address.clone(),
                expected_peer,
                expected_scope,
                ControlRole::Coordinator,
            ),
        )
        .await
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        if channel.peer() != expected_peer || channel.scope() != Some(expected_scope) {
            return Err(ClusterDispatchError::AssignmentRejected);
        }
        channel
            .send(&ControlMessage::ResultRecoveryQuery(query))
            .await
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let status = match tokio::time::timeout(RESULT_RECOVERY_QUERY_TIMEOUT, channel.receive())
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?
        {
            ControlMessage::ResultRecoveryStatus(status)
                if status.matches_query(&query, self.owner_id, expected_peer) =>
            {
                status
            }
            _ => return Err(ClusterDispatchError::AssignmentRejected),
        };
        channel
            .finish()
            .await
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;

        match status.state() {
            ControlResultRecoveryState::NoResult => {
                let peer = EndpointId::from_bytes(recovered.worker_grant.peer().as_bytes())
                    .map_err(|_| ClusterDispatchError::AssignmentRejected)?;
                self.record_no_result_terminal(recovered.assignment, recovered.namespace_id, peer)
                    .await?;
                notify_compiler_result_journal(
                    journal_hook,
                    CompilerResultJournalEvent::RecoveredNoResultTerminal {
                        assignment: recovered.assignment,
                        namespace_id: recovered.namespace_id,
                        status: &status,
                    },
                )?;
                Ok(RecoveredCompilerAssignmentOutcome::NoResult)
            }
            ControlResultRecoveryState::Retired(retired) => {
                // A worker tombstone does not prove the owner durably selected or rejected its
                // result. The normal ACK recovery worker must resolve its journal proof and then
                // perform the confirm-only handshake on a separate exact-scope stream.
                if retired.scope() != expected_scope
                    || retired.worker_endpoint_id() != *expected_peer.as_bytes()
                {
                    return Err(ClusterDispatchError::AckRejected(
                        "recovered worker retirement changed the exact route".into(),
                    ));
                }
                Ok(RecoveredCompilerAssignmentOutcome::Retired(retired))
            }
            ControlResultRecoveryState::Running | ControlResultRecoveryState::Pending(_) => {
                let capacity = status.capacity();
                let receipt = match status.state() {
                    ControlResultRecoveryState::Pending(receipt) => Some(receipt),
                    ControlResultRecoveryState::Running => None,
                    ControlResultRecoveryState::NoResult
                    | ControlResultRecoveryState::Retired(_) => unreachable!("handled above"),
                };
                if receipt.is_some() {
                    pause_after_pending_recovery_status().await?;
                }
                if let Some(receipt) = receipt {
                    *announced_result = Some(CompilerResultIdentity {
                        assignment: recovered.assignment,
                        namespace_id: recovered.namespace_id,
                        worker_grant: recovered.worker_grant.clone(),
                        closure_id: receipt.closure_id,
                    });
                }
                let resources = recovered_resource_demand(
                    &recovered.capture,
                    recovered.assignment,
                    receipt.map_or(recovered.assignment.work().max_output_bytes(), |receipt| {
                        receipt.payload_bytes
                    }),
                )
                .map_err(|error| {
                    if receipt.is_some() {
                        ClusterDispatchError::ConclusiveResultRejection(format!(
                            "worker result receipt exceeds the exact assignment bounds: {error}"
                        ))
                    } else {
                        error
                    }
                })?;
                // A retained result has already consumed the worker's execution capacity and
                // completed its bounded output. Requiring today's capacity total to fit the
                // original work would strand valid results after a worker restart or resize.
                // Keep the checked resource envelope derived above, but require current capacity
                // only when adopting a still-running assignment with no result receipt.
                if !recovered_capacity_allows(resources, capacity.total, receipt.is_some()) {
                    return Err(ClusterDispatchError::NoEligibleWorker);
                }
                scheduler
                    .adopt_recovered_remote(
                        recovered.assignment,
                        resources,
                        capacity.worker_incarnation,
                    )
                    .map_err(|_| ClusterDispatchError::AssignmentRejected)?;
                let _assignment_lease = CompilerAssignmentLease::new(
                    recovered.assignment,
                    scheduler.clone(),
                    Arc::clone(&self.pending_cost_probes),
                    recovered.namespace_id,
                );
                let Some(receipt) = receipt else {
                    return Ok(RecoveredCompilerAssignmentOutcome::Running);
                };
                let input_closure = recovered.capture.closure();
                let fenced_result =
                    backend_engine::compiler_cluster_transport::admit_compiler_result(
                        scheduler,
                        recovered.assignment,
                        recovered.namespace_id,
                        receipt,
                    )
                    .map_err(map_recovered_result_bridge_error)?;
                let mut receiver =
                    backend_engine::compiler_cluster_transport::CompilerResultGrantPageReceiver::new(
                        scheduler,
                        recovered.assignment,
                        recovered.namespace_id,
                        fenced_result,
                    )
                    .map_err(map_recovered_result_bridge_error)?;
                let transfer_deadline =
                    current_unix_ms()?.saturating_add(RECOVERED_RESULT_TRANSFER_WINDOW_MS);
                while !receiver.is_complete() {
                    let event = tokio::time::timeout(
                        remaining_duration(transfer_deadline)?,
                        control_events.recv(),
                    )
                    .await
                    .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                    .ok_or(ClusterDispatchError::WorkerFailed)?;
                    let OwnerControlEvent {
                        mut channel,
                        first_message,
                    } = event;
                    match first_message {
                        ControlMessage::GrantPage(page) => {
                            if !exact_control_route(&channel, expected_peer, expected_scope) {
                                channel.finish().await.map_err(|error| {
                                    ClusterDispatchError::Transport(error.to_string())
                                })?;
                                return Err(ClusterDispatchError::AssignmentRejected);
                            }
                            receiver
                                .receive_channel_with_first(scheduler, channel, page)
                                .await
                                .map_err(map_recovered_result_bridge_error)?;
                        }
                        ControlMessage::ResultReceipt(replayed) => {
                            let exact_route =
                                exact_control_route(&channel, expected_peer, expected_scope);
                            let same_receipt = same_result_receipt(replayed, receipt);
                            channel.finish().await.map_err(|error| {
                                ClusterDispatchError::Transport(error.to_string())
                            })?;
                            if !exact_route {
                                return Err(ClusterDispatchError::AssignmentRejected);
                            }
                            if !same_receipt {
                                return Err(post_receipt_terminal_error(
                                    true,
                                    "worker replayed a result receipt that contradicts its durable pending result",
                                ));
                            }
                        }
                        ControlMessage::WorkerReject(_) | ControlMessage::ExecutionFailed(_) => {
                            let exact_route =
                                exact_control_route(&channel, expected_peer, expected_scope);
                            channel.finish().await.map_err(|error| {
                                ClusterDispatchError::Transport(error.to_string())
                            })?;
                            return Err(post_receipt_terminal_error(
                                exact_route,
                                "worker sent a terminal failure after reporting a durable result",
                            ));
                        }
                        _ => {
                            let exact_route =
                                exact_control_route(&channel, expected_peer, expected_scope);
                            channel.finish().await.map_err(|error| {
                                ClusterDispatchError::Transport(error.to_string())
                            })?;
                            return Err(post_receipt_terminal_error(
                                exact_route,
                                "worker sent an unexpected control message after reporting a durable result",
                            ));
                        }
                    }
                }
                let pages = receiver
                    .into_pages()
                    .map_err(map_recovered_result_bridge_error)?;
                let output_sink = self.store.artifact_sink(cluster_artifact_budget());
                let (pinned_stored_receipt, output_transfer_bytes, output_transfer_elapsed_ms) =
                    self.receive_result_closure(
                        scheduler,
                        recovered.assignment,
                        recovered.namespace_id,
                        expected_peer,
                        worker_address.clone(),
                        fenced_result,
                        &pages,
                        &output_sink,
                        transfer_deadline,
                    )
                    .await
                    .map_err(classify_received_result_transfer_error)?;
                let stored = backend_engine::compiler_cluster_transport::store_compiler_result(
                    scheduler,
                    fenced_result,
                    pinned_stored_receipt.receipt(),
                )
                .map_err(map_recovered_result_bridge_error)?;
                let admitted = admit_remote_compiler_candidate(
                    &self.store,
                    stored,
                    recovered.assignment,
                    recovered.namespace_id,
                    &recovered.capture,
                    recovered.input_admission.clone(),
                    cluster_artifact_budget(),
                )
                .map_err(map_recovered_candidate_error)?;
                *announced_result = None;
                Ok(RecoveredCompilerAssignmentOutcome::Candidate(
                    CheckedRemoteCompilerCandidate {
                        admitted,
                        pending: PendingStoredCompilerResult {
                            assignment: recovered.assignment,
                            namespace_id: recovered.namespace_id,
                            worker_address,
                            worker_grant: recovered.worker_grant,
                            stored,
                        },
                        _output_pin: pinned_stored_receipt,
                        output_transfer_bytes,
                        output_transfer_elapsed_ms,
                        cost_model_updated: false,
                    },
                ))
            }
        }
    }

    /// Probes trusted workers from the exact Turso attempt and places only with live evidence.
    ///
    /// The candidate attempt owns the namespace, input revision, ordinal, and fence. No worker
    /// is offered work by this method: it returns a remote assignment only after the exact
    /// capability, closure Have bitmap, capacity snapshot, owner cost estimate, and scheduler
    /// fence have all been admitted. The result retains an explicit local fallback reason so
    /// the index owner can keep local-first semantics visible to diagnostics.
    pub(crate) fn probe_and_place_assignment(
        &self,
        scheduler: &CompilerClusterScheduler,
        placement_policy: &CompilerPlacementPolicy,
        request: CompilerBalancingRequest,
        local_cost: LocalCostEstimate,
        attempt: &CandidateAttempt,
        capture: &backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
        deadline_unix_ms: u64,
    ) -> Result<CompilerDispatchDecision, ClusterDispatchError> {
        self.block_on(self.probe_and_place_assignment_async(
            scheduler,
            placement_policy,
            request,
            local_cost,
            attempt,
            capture,
            deadline_unix_ms,
        ))
    }

    async fn probe_and_place_assignment_async(
        &self,
        scheduler: &CompilerClusterScheduler,
        placement_policy: &CompilerPlacementPolicy,
        mut request: CompilerBalancingRequest,
        local_cost: LocalCostEstimate,
        attempt: &CandidateAttempt,
        capture: &backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
        deadline_unix_ms: u64,
    ) -> Result<CompilerDispatchDecision, ClusterDispatchError> {
        let work = request.work;
        request.local_cost = local_cost.completion();
        if local_cost.recipe != *work.recipe().as_ref()
            || local_cost.input_payload_bytes != capture.payload_bytes()
            || local_cost.source_artifact_count == 0
        {
            return Err(ClusterDispatchError::InputRejected);
        }
        let deadline_unix_ms = request
            .deadline_at
            .map_or(deadline_unix_ms, |deadline| deadline.min(deadline_unix_ms));
        request.deadline_at = Some(deadline_unix_ms);
        let namespace_id = attempt.namespace().namespace_id();
        let attempt_id = AttemptId::new(attempt.scheduler_attempt_id())
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let fence = Fence::from_bytes(attempt.fence_bytes())
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let token = CompilerAttemptToken::new(attempt_id, fence);
        self.validate_attempt_capture(attempt, work, namespace_id, capture)?;
        if !request.resources.is_positive() {
            return Ok(if request.local == LocalCompilerAvailability::Unavailable {
                CompilerDispatchDecision::OfflineUnavailable {
                    reason: CompilerLocalFallbackReason::NoEligibleWorker,
                    evidence: empty_dispatch_cost_evidence(local_cost),
                }
            } else {
                CompilerDispatchDecision::LocalFallback {
                    reason: CompilerLocalFallbackReason::CostEstimateUnavailable,
                    evidence: empty_dispatch_cost_evidence(local_cost),
                }
            });
        }
        if deadline_unix_ms <= current_unix_ms()? || request.submitted_at > current_unix_ms()? {
            return Ok(if request.local == LocalCompilerAvailability::Unavailable {
                CompilerDispatchDecision::OfflineUnavailable {
                    reason: CompilerLocalFallbackReason::DeadlineExpired,
                    evidence: empty_dispatch_cost_evidence(local_cost),
                }
            } else {
                CompilerDispatchDecision::LocalFallback {
                    reason: CompilerLocalFallbackReason::DeadlineExpired,
                    evidence: empty_dispatch_cost_evidence(local_cost),
                }
            });
        }
        let started_at = current_unix_ms()?;
        if interactive_local_first_window(
            request.demand == backend_engine::application::CompilerDemand::Interactive,
            request.local != LocalCompilerAvailability::Unavailable,
            request.submitted_at,
            started_at,
            request.local_first_budget,
            request.local_start_delay,
        ) {
            return Ok(CompilerDispatchDecision::LocalFallback {
                reason: CompilerLocalFallbackReason::InteractiveLocalFirst,
                evidence: empty_dispatch_cost_evidence(local_cost),
            });
        }
        if interactive_local_cost_evidence_required(request.demand, request.local, local_cost) {
            return Ok(CompilerDispatchDecision::LocalFallback {
                reason: CompilerLocalFallbackReason::CostEstimateUnavailable,
                evidence: empty_dispatch_cost_evidence(local_cost),
            });
        }

        let verified_input = capture
            .verify_in_store(&self.store)
            .map_err(|error| ClusterDispatchError::Store(error.to_string()))?;
        let input_closure = capture.closure();
        let inventory = tokio::time::timeout(
            remaining_duration(deadline_unix_ms)?,
            self.describe_input_inventory(input_closure, &verified_input),
        )
        .await
        .map_err(|_| ClusterDispatchError::DeadlineExpired)??;
        let trust = self.trusted_workers()?;
        let manifest = capture.manifest();
        let applicable_grants = trust
            .grants()
            .iter()
            .filter(|grant| {
                trust.authorizes(
                    grant.peer(),
                    namespace_id,
                    manifest.recipe(),
                    manifest.profile(),
                    manifest.stage(),
                    manifest.toolchain(),
                    manifest.environment(),
                    manifest.target_platform(),
                )
            })
            .take(MAX_TRUSTED_PROBE_PEERS + 1)
            .collect::<Vec<_>>();
        if applicable_grants.len() > MAX_TRUSTED_PROBE_PEERS {
            return Err(ClusterDispatchError::NoEligibleWorker);
        }
        let mut probes = FuturesUnordered::new();
        for grant in applicable_grants {
            probes.push(self.probe_trusted_worker(
                scheduler,
                request,
                local_cost.is_measured(),
                token,
                namespace_id,
                capture,
                inventory,
                grant,
                deadline_unix_ms,
            ));
        }
        let outcomes =
            match collect_probe_outcomes(probes, deadline_unix_ms, PROBE_CANDIDATE_SETTLE_WINDOW)
                .await
            {
                Ok(outcomes) => outcomes,
                Err(ClusterDispatchError::DeadlineExpired) => Vec::new(),
                Err(error) => return Err(error),
            };
        let mut candidates = Vec::new();
        let mut first_failure = None;
        for outcome in outcomes {
            match outcome {
                Ok(Some(candidate)) => candidates.push(candidate),
                Ok(None)
                | Err(ClusterDispatchError::ProbeRejected)
                | Err(ClusterDispatchError::Transport(_))
                | Err(ClusterDispatchError::DeadlineExpired) => {}
                Err(error) => {
                    first_failure.get_or_insert(error);
                }
            }
        }

        // A peer's malformed response or local probe failure must not erase a complete live
        // route from another peer. Preserve the first substantive error only when no route
        // survived the concurrent probe batch.
        if candidates.is_empty() {
            if let Some(error) = first_failure {
                return Err(error);
            }
        }

        let now = current_unix_ms()?;
        if now >= deadline_unix_ms {
            return Ok(if request.local == LocalCompilerAvailability::Unavailable {
                CompilerDispatchDecision::OfflineUnavailable {
                    reason: CompilerLocalFallbackReason::DeadlineExpired,
                    evidence: empty_dispatch_cost_evidence(local_cost),
                }
            } else {
                CompilerDispatchDecision::LocalFallback {
                    reason: CompilerLocalFallbackReason::DeadlineExpired,
                    evidence: empty_dispatch_cost_evidence(local_cost),
                }
            });
        }
        if candidates.is_empty() {
            return Ok(if request.local == LocalCompilerAvailability::Unavailable {
                CompilerDispatchDecision::OfflineUnavailable {
                    reason: CompilerLocalFallbackReason::NoEligibleWorker,
                    evidence: empty_dispatch_cost_evidence(local_cost),
                }
            } else {
                CompilerDispatchDecision::LocalFallback {
                    reason: CompilerLocalFallbackReason::NoEligibleWorker,
                    evidence: empty_dispatch_cost_evidence(local_cost),
                }
            });
        }
        candidates.sort_by_key(|(candidate, _)| candidate.node().peer());
        let best_remote = candidates
            .iter()
            .min_by_key(|(candidate, probe)| (probe.placement_total_ms, candidate.node().peer()))
            .map(|(_, probe)| (probe.cost, probe.placement_total_ms, probe.exploratory));
        let fallback_evidence = CompilerDispatchCostEvidence {
            local: local_cost,
            remote: best_remote.map(|(cost, _, _)| cost),
            remote_placement_total_ms: best_remote.map(|(_, total, _)| total),
            exploratory: best_remote.is_some_and(|(_, _, exploratory)| exploratory),
            remote_route_provenance: None,
        };
        let checked_candidates = candidates
            .iter()
            .map(|(candidate, _)| candidate.clone())
            .collect::<Vec<_>>();
        let local_resources_available = request.local != LocalCompilerAvailability::Unavailable;
        let chosen = placement_policy.choose_balanced(
            request,
            local_resources_available,
            now,
            &checked_candidates,
        );
        let mut chosen_peer = match chosen {
            backend_engine::application::CompilerPlacement::Remote(peer) => Some(peer),
            backend_engine::application::CompilerPlacement::Local => None,
            backend_engine::application::CompilerPlacement::OfflineUnavailable => None,
        };
        let mut fallback_reason = match chosen {
            backend_engine::application::CompilerPlacement::Local => {
                CompilerLocalFallbackReason::LocalCostLower
            }
            backend_engine::application::CompilerPlacement::OfflineUnavailable => {
                CompilerLocalFallbackReason::NoEligibleWorker
            }
            backend_engine::application::CompilerPlacement::Remote(_) => {
                CompilerLocalFallbackReason::LocalCostLower
            }
        };
        if chosen_peer.is_none()
            && request.demand == CompilerDemand::Interactive
            && local_cost.is_measured()
            && candidates.iter().any(|(_, probe)| probe.exploratory)
            && !candidates.iter().any(|(_, probe)| {
                probe.exploratory
                    && route_cost_meets_deadline(now, probe.placement_total_ms, request.deadline_at)
            })
        {
            fallback_reason = CompilerLocalFallbackReason::RemoteCostMissesDeadline;
        }
        if chosen_peer.is_none()
            && request.demand == CompilerDemand::Interactive
            && local_cost.is_measured()
            && candidates.iter().any(|(_, probe)| {
                probe.exploratory
                    && route_cost_meets_deadline(now, probe.placement_total_ms, request.deadline_at)
                    && !interactive_cold_exploration_fits_latency_budget(
                        local_cost.completion.total(),
                        probe.placement_total_ms,
                    )
            })
        {
            fallback_reason =
                CompilerLocalFallbackReason::RemoteCostExceedsInteractiveLatencyBudget;
        }
        if chosen_peer.is_none()
            && request.demand == CompilerDemand::Interactive
            && local_cost.is_measured()
        {
            chosen_peer = candidates
                .iter()
                .filter(|(_, probe)| {
                    cold_exploration_is_allowed(request.demand, true, probe.exploratory)
                })
                .filter(|(_, probe)| {
                    route_cost_meets_deadline(now, probe.placement_total_ms, request.deadline_at)
                })
                .filter(|(_, probe)| {
                    interactive_cold_exploration_fits_latency_budget(
                        local_cost.completion.total(),
                        probe.placement_total_ms,
                    )
                })
                .min_by_key(|(candidate, probe)| {
                    (probe.placement_total_ms, candidate.node().peer())
                })
                .map(|(candidate, _)| candidate.node().peer());
        }
        if chosen_peer.is_none()
            && request.demand == backend_engine::application::CompilerDemand::Background
        {
            chosen_peer = candidates
                .iter()
                .filter(|(_, probe)| probe.exploratory)
                .filter(|(_, probe)| {
                    route_cost_meets_deadline(now, probe.placement_total_ms, request.deadline_at)
                })
                .min_by_key(|(candidate, probe)| {
                    (probe.placement_total_ms, candidate.node().peer())
                })
                .map(|(candidate, _)| candidate.node().peer());
            if chosen_peer.is_some() {
                fallback_reason = CompilerLocalFallbackReason::BackgroundExploration;
            }
        }
        let Some(chosen_peer) = chosen_peer else {
            return Ok(if request.local == LocalCompilerAvailability::Unavailable {
                CompilerDispatchDecision::OfflineUnavailable {
                    reason: fallback_reason,
                    evidence: fallback_evidence,
                }
            } else {
                CompilerDispatchDecision::LocalFallback {
                    reason: fallback_reason,
                    evidence: fallback_evidence,
                }
            });
        };
        let Some((chosen_candidate, cost_probe)) = candidates
            .into_iter()
            .find(|(candidate, _)| candidate.node().peer() == chosen_peer)
        else {
            return Err(ClusterDispatchError::NoEligibleWorker);
        };

        // Placement has already compared this remote path with the caller's actual local route.
        // The remote-only assignment request below reserves the checked peer without asking this
        // owner runtime to invent local capacity it does not control.
        let mut remote_only_request = request;
        remote_only_request.local = LocalCompilerAvailability::Unavailable;
        let outcome = scheduler
            .place_balanced_and_assign(
                placement_policy,
                remote_only_request,
                now,
                &[chosen_candidate],
                token,
            )
            .map_err(|_| ClusterDispatchError::AssignmentRejected)?;
        match outcome {
            CompilerAssignmentOutcome::Assigned(assignment)
                if assignment.route()
                    == backend_engine::application::CompilerAssignmentRoute::Remote(
                        chosen_peer,
                    ) =>
            {
                if cost_probe.exploratory {
                    match self
                        .cost_model
                        .claim_exploration(cost_probe.peer, cost_probe.recipe, now)
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            let _ = scheduler.cancel(assignment);
                            return Ok(
                                if request.local == LocalCompilerAvailability::Unavailable {
                                    CompilerDispatchDecision::OfflineUnavailable {
                                        reason: CompilerLocalFallbackReason::ExplorationNotDue,
                                        evidence: fallback_evidence,
                                    }
                                } else {
                                    CompilerDispatchDecision::LocalFallback {
                                        reason: CompilerLocalFallbackReason::ExplorationNotDue,
                                        evidence: fallback_evidence,
                                    }
                                },
                            );
                        }
                        Err(error) => {
                            let _ = scheduler.cancel(assignment);
                            return Err(error);
                        }
                    }
                }
                self.pending_cost_probes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(pending_cost_probe_key(namespace_id, assignment), cost_probe);
                Ok(CompilerDispatchDecision::Remote {
                    assignment: CompilerAssignmentLease::new(
                        assignment,
                        scheduler.clone(),
                        Arc::clone(&self.pending_cost_probes),
                        namespace_id,
                    ),
                    evidence: CompilerDispatchCostEvidence {
                        local: local_cost,
                        remote: Some(cost_probe.cost),
                        remote_placement_total_ms: Some(cost_probe.placement_total_ms),
                        exploratory: cost_probe.exploratory,
                        remote_route_provenance: Some(remote_route_provenance(
                            request.demand,
                            cost_probe.exploratory,
                        )),
                    },
                })
            }
            CompilerAssignmentOutcome::Assigned(assignment) => {
                let _ = scheduler.cancel(assignment);
                Ok(CompilerDispatchDecision::OfflineUnavailable {
                    reason: CompilerLocalFallbackReason::RemoteCapacityFull,
                    evidence: fallback_evidence,
                })
            }
            CompilerAssignmentOutcome::OfflineUnavailable => {
                Ok(if request.local == LocalCompilerAvailability::Unavailable {
                    CompilerDispatchDecision::OfflineUnavailable {
                        reason: CompilerLocalFallbackReason::RemoteCapacityFull,
                        evidence: fallback_evidence,
                    }
                } else {
                    CompilerDispatchDecision::LocalFallback {
                        reason: CompilerLocalFallbackReason::RemoteCapacityFull,
                        evidence: fallback_evidence,
                    }
                })
            }
        }
    }

    fn validate_attempt_capture(
        &self,
        attempt: &CandidateAttempt,
        work: CompilerWorkIdentity,
        namespace_id: [u8; 16],
        capture: &backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
    ) -> Result<(), ClusterDispatchError> {
        let manifest = capture.manifest();
        let full_workspace = work
            .input()
            .full_workspace()
            .ok_or(ClusterDispatchError::InputRejected)?;
        let claim = full_workspace.claim();
        let manifest_identity = manifest
            .identity_claim()
            .map_err(|_| ClusterDispatchError::InputRejected)?;
        if namespace_id == [0; 16]
            || attempt.namespace().namespace_id() != namespace_id
            || attempt.input_digest() != &manifest.input_root()
            || attempt.observation().observation().revision().as_ref()
                != Some(attempt.input_digest())
            || manifest_identity != work.input_identity()
            || manifest.max_output_bytes() != work.max_output_bytes()
            || work.selected_base().is_some()
            || claim.input_closure_id != *capture.closure().as_bytes()
            || claim.manifest_object_id != *capture.manifest_object_id().as_bytes()
            || manifest.source_fence_digest() != capture.source_fence_digest()
        {
            return Err(ClusterDispatchError::InputRejected);
        }
        Ok(())
    }

    async fn describe_input_inventory(
        &self,
        closure: ClosureId,
        verified: &backend_engine::compiler_cluster_transport::VerifiedFullWorkspaceClosureV2,
    ) -> Result<ProbeInventoryDescriptor, ClusterDispatchError> {
        let verified_object_count = u32::try_from(verified.member_ids().len())
            .map_err(|_| ClusterDispatchError::InputRejected)?;
        let mut hasher = ProbeInventoryHasher::new();
        for object_id in verified.member_ids() {
            let member = tokio::time::timeout(
                CLUSTER_IO_TIMEOUT,
                self.catalog.register_closure_member(
                    closure,
                    UntrustedObjectId::from_bytes(*object_id.as_bytes()),
                ),
            )
            .await
            .map_err(|_| ClusterDispatchError::InputRejected)?
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?
            .ok_or(ClusterDispatchError::InputRejected)?;
            if member.closure() != closure || member.object_id() != object_id {
                return Err(ClusterDispatchError::InputRejected);
            }
            let payload_bytes = u32::try_from(member.materialized().object.payload_length)
                .map_err(|_| ClusterDispatchError::InputRejected)?;
            hasher
                .push(ProbeObjectClaim {
                    object_id: *object_id.as_bytes(),
                    payload_bytes,
                })
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        }
        let descriptor = hasher
            .finish()
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        if descriptor.object_count != verified_object_count {
            return Err(ClusterDispatchError::InputRejected);
        }
        Ok(descriptor)
    }

    #[allow(clippy::too_many_arguments)]
    async fn probe_trusted_worker(
        &self,
        scheduler: &CompilerClusterScheduler,
        request: CompilerBalancingRequest,
        local_cost_is_measured: bool,
        token: backend_engine::application::CompilerAttemptToken,
        namespace_id: [u8; 16],
        capture: &backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
        inventory: ProbeInventoryDescriptor,
        grant: &TrustedCompilerWorkerGrant,
        assignment_deadline_unix_ms: u64,
    ) -> Result<Option<(CompilerBalancedRemote, PendingRemoteCostProbe)>, ClusterDispatchError>
    {
        let manifest = capture.manifest();
        let worker_id = grant.peer();
        if !self.trusted_workers()?.authorizes(
            worker_id,
            namespace_id,
            manifest.recipe(),
            manifest.profile(),
            manifest.stage(),
            manifest.toolchain(),
            manifest.environment(),
            manifest.target_platform(),
        ) {
            return Ok(None);
        }
        let peer = CompilerPeerId::new(*worker_id.as_bytes())
            .map_err(|_| ClusterDispatchError::NoEligibleWorker)?;
        let probe_started_at = current_unix_ms()?;
        let probe_expires_at = probe_started_at
            .checked_add(MAX_PROBE_LIFETIME_MS)
            .ok_or(ClusterDispatchError::DeadlineExpired)?
            .min(assignment_deadline_unix_ms);
        if probe_expires_at <= probe_started_at {
            return Err(ClusterDispatchError::DeadlineExpired);
        }
        let mut nonce = [0_u8; 16];
        fill_os_random(&mut nonce).map_err(ClusterDispatchError::Runtime)?;
        if nonce == [0; 16] {
            return Err(ClusterDispatchError::ProbeRejected);
        }
        let scope = AssignmentScope::new(
            namespace_id,
            request.work.transfer_work_id(),
            token.attempt().get(),
            token.fence().as_bytes(),
        )
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let identity = backend_engine::cluster_transport::CompilerProbeIdentity {
            package_lineage: manifest.package_lineage(),
            target: *manifest.package_target().target().as_ref(),
            package_target: manifest
                .package_target()
                .encode()
                .map_err(|_| ClusterDispatchError::InputRejected)?,
            profile: <[u8; 2]>::from(manifest.profile()),
            stage: u8::from(manifest.stage()),
            recipe: manifest.recipe(),
            input_root: manifest.input_root(),
            read_manifest: manifest.read_manifest(),
            input_closure_id: *capture.closure().as_bytes(),
            input_manifest_object_id: *capture.manifest_object_id().as_bytes(),
            selected_base: manifest.selected_base(),
            max_output_bytes: manifest.max_output_bytes(),
        };
        let transfer_bytes = request.resources.transfer.bytes();
        let session = CompilerProbeSession {
            scope,
            nonce,
            expires_at_unix_ms: probe_expires_at,
            identity,
            demand: CompilerProbeDemand {
                cpu_millicores: request.resources.cpu.millicores(),
                memory_bytes: request.resources.memory.bytes(),
                transfer_bytes,
            },
            session_affinity: request
                .session_affinity
                .map(CompilerSessionAffinity::as_bytes),
        };
        session
            .validate(probe_started_at)
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;

        let worker_address = EndpointAddr::new(worker_id).with_ip_addr(grant.address());
        let mut channel = tokio::time::timeout(
            remaining_duration(probe_expires_at)?,
            connect_probe(&self.endpoint, worker_address, worker_id, scope),
        )
        .await
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
        .map_err(|error| ClusterDispatchError::ProbeRejected)?;
        if channel.peer() != worker_id || channel.scope() != Some(scope) {
            return Err(ClusterDispatchError::ProbeRejected);
        }

        let page_count = inventory
            .object_count
            .div_ceil(MAX_PROBE_OBJECTS_PER_PAGE as u32);
        if page_count == 0 || page_count > MAX_PROBE_PAGES {
            return Err(ClusterDispatchError::ProbeRejected);
        }
        let mut cursor = None;
        let mut have_objects = 0_u32;
        let mut have_payload_bytes = 0_u64;
        let mut missing_objects = 0_u32;
        let mut missing_payload_bytes = 0_u64;
        let mut minimum_probe_control_bytes = 0_u64;
        let mut snapshot = None;
        let mut transcript = blake3::Hasher::new();
        transcript.update(b"backend.compiler.owner-probe-transcript.v1\0");
        transcript.update(worker_id.as_bytes());
        transcript.update(&nonce);
        transcript.update(&scope.namespace_id);
        transcript.update(&scope.work_id);
        transcript.update(&scope.attempt.to_be_bytes());
        transcript.update(&scope.fence);
        transcript.update(&inventory.digest);
        for page_index in 0..page_count {
            let store = self.store.clone();
            let closure = capture.closure();
            let after = cursor;
            let index_page = tokio::time::timeout(
                remaining_duration(probe_expires_at)?,
                tokio::task::spawn_blocking(move || {
                    store
                        .read_closure_index(closure)
                        .and_then(|index| index.page_ids(after, MAX_PROBE_OBJECTS_PER_PAGE))
                }),
            )
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|error| ClusterDispatchError::Store(format!("{error:?}")))?
            .map_err(|error| ClusterDispatchError::Store(format!("{error:?}")))?;
            let mut objects = Vec::with_capacity(index_page.object_ids().len());
            for object_id in index_page.object_ids() {
                let member = tokio::time::timeout(
                    CLUSTER_IO_TIMEOUT,
                    self.catalog.register_closure_member(
                        capture.closure(),
                        UntrustedObjectId::from_bytes(*object_id.as_bytes()),
                    ),
                )
                .await
                .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                .map_err(|error| ClusterDispatchError::Store(error.to_string()))?
                .ok_or(ClusterDispatchError::InputRejected)?;
                if member.closure() != capture.closure() || member.object_id() != *object_id {
                    return Err(ClusterDispatchError::InputRejected);
                }
                objects.push(ProbeObjectClaim {
                    object_id: *object_id.as_bytes(),
                    payload_bytes: u32::try_from(member.materialized().object.payload_length)
                        .map_err(|_| ClusterDispatchError::InputRejected)?,
                });
            }
            if objects.is_empty() {
                return Err(ClusterDispatchError::ProbeRejected);
            }
            let page = CompilerProbePage::new(session.clone(), inventory, page_index, objects)
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
            tokio::time::timeout(
                remaining_duration(probe_expires_at)?,
                channel.send_challenge_page(&page),
            )
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|_| ClusterDispatchError::ProbeRejected)?;
            let reply = tokio::time::timeout(
                remaining_duration(probe_expires_at)?,
                channel.receive_have_page(&page),
            )
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|_| ClusterDispatchError::ProbeRejected)?;
            let now = current_unix_ms()?;
            let missing = reply
                .missing_objects(&page, now)
                .map_err(|_| ClusterDispatchError::ProbeRejected)?;
            let page_missing_bytes = missing.iter().try_fold(0_u64, |sum, object| {
                sum.checked_add(u64::from(object.payload_bytes))
            });
            have_objects = have_objects
                .checked_add(reply.have_count)
                .ok_or(ClusterDispatchError::ProbeRejected)?;
            have_payload_bytes = have_payload_bytes
                .checked_add(reply.have_payload_bytes)
                .ok_or(ClusterDispatchError::ProbeRejected)?;
            missing_objects = missing_objects
                .checked_add(
                    u32::try_from(missing.len())
                        .map_err(|_| ClusterDispatchError::ProbeRejected)?,
                )
                .ok_or(ClusterDispatchError::ProbeRejected)?;
            missing_payload_bytes = missing_payload_bytes
                .checked_add(page_missing_bytes.ok_or(ClusterDispatchError::ProbeRejected)?)
                .ok_or(ClusterDispatchError::ProbeRejected)?;
            let request_bytes = u64::try_from(page.objects.len())
                .ok()
                .and_then(|count| count.checked_mul(36))
                .ok_or(ClusterDispatchError::ProbeRejected)?;
            let response_bytes = u64::try_from(reply.have_bitmap.len())
                .map_err(|_| ClusterDispatchError::ProbeRejected)?;
            minimum_probe_control_bytes = minimum_probe_control_bytes
                .checked_add(request_bytes)
                .and_then(|sum| sum.checked_add(response_bytes))
                .ok_or(ClusterDispatchError::ProbeRejected)?;
            transcript.update(
                &page
                    .request_digest()
                    .map_err(|_| ClusterDispatchError::ProbeRejected)?,
            );
            transcript.update(&reply.page_digest);
            transcript.update(&reply.have_bitmap);
            transcript.update(&reply.have_count.to_be_bytes());
            transcript.update(&reply.have_payload_bytes.to_be_bytes());
            transcript.update(
                &reply
                    .snapshot
                    .digest()
                    .map_err(|_| ClusterDispatchError::ProbeRejected)?,
            );
            if snapshot.is_some_and(|existing| existing != reply.snapshot) {
                return Err(ClusterDispatchError::ProbeRejected);
            }
            snapshot = Some(reply.snapshot);
            cursor = index_page.next();
        }
        tokio::time::timeout(remaining_duration(probe_expires_at)?, channel.finish())
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|_| ClusterDispatchError::ProbeRejected)?;
        if cursor.is_some()
            || have_objects.checked_add(missing_objects) != Some(inventory.object_count)
            || have_payload_bytes.checked_add(missing_payload_bytes)
                != Some(inventory.payload_bytes)
        {
            return Err(ClusterDispatchError::ProbeRejected);
        }
        let snapshot = snapshot.ok_or(ClusterDispatchError::ProbeRejected)?;
        let ProbeCapability::Supported { capability_digest } = snapshot.capability else {
            return Ok(None);
        };
        let worker_lease_expires_at = probe_started_at
            .checked_add(u64::from(snapshot.capacity_lease_ms))
            .ok_or(ClusterDispatchError::DeadlineExpired)?;
        let expires_at = probe_expires_at.min(worker_lease_expires_at);
        if expires_at <= current_unix_ms()? {
            return Ok(None);
        }
        let binding = CompilerRemoteProbeBinding::new(
            namespace_id,
            request.work.transfer_work_id(),
            token.attempt().get(),
            token.fence().as_bytes(),
            nonce,
            probe_started_at,
            expires_at,
        )
        .map_err(|_| ClusterDispatchError::ProbeRejected)?;
        let measurements = OwnerRemoteCompilerProbeMeasurements {
            work: request.work,
            request,
            local_cost_is_measured,
            peer,
            worker_incarnation: snapshot.worker_incarnation,
            capacity_revision: snapshot.capacity_revision,
            input_objects: inventory.object_count,
            input_payload_bytes: inventory.payload_bytes,
            have_objects,
            have_payload_bytes,
            missing_objects,
            missing_payload_bytes,
            minimum_probe_control_bytes,
            probe_elapsed_ms: current_unix_ms()?.saturating_sub(probe_started_at),
            total: compiler_resource_credits(snapshot.total),
            busy: compiler_resource_credits(snapshot.busy),
            session_affinity_warm: snapshot.session_affinity_warm,
            observed_at: probe_started_at,
            expires_at,
        };
        let Some(estimate) = self.cost_model.estimate(&measurements) else {
            return Ok(None);
        };
        let cost = estimate.cost;
        if cost.observed_at != probe_started_at
            || cost.expires_at != expires_at
            || cost.confidence_per_mille < 500
            || cost.confidence_per_mille > 1_000
            || cost.completion.checked_total().is_none()
        {
            return Ok(None);
        }
        let have = RemoteHaveClaim {
            input_manifest: request.work.input_identity().manifest,
            input_closure_id: *capture.closure().as_bytes(),
            manifest_object_id: *capture.manifest_object_id().as_bytes(),
            inventory_digest: inventory.digest,
            required_objects: inventory.object_count,
            verified_have_objects: have_objects,
            owner_streamable_objects: missing_objects,
            missing_bytes: missing_payload_bytes,
            proof_digest: *transcript.finalize().as_bytes(),
        };
        let preflight = RemoteCompilerPreflightClaim {
            peer,
            work: request.work,
            binding,
            worker_incarnation: snapshot.worker_incarnation,
            capability: RemoteCompilerCapabilityClaim {
                target: request.work.target(),
                recipe: request.work.recipe(),
                supported: true,
                capability_digest,
            },
            have,
            cost,
        };
        let verified_work = VerifiedRemoteCompiler::admit_preflight(
            request.work,
            preflight,
            &ExactRemotePreflightVerifier(preflight),
        )
        .map_err(|error| {
            ClusterDispatchError::ResultRejected(format!("remote preflight: {error}"))
        })?;

        let warm_sessions = match request.session_affinity {
            Some(affinity) if snapshot.session_affinity_warm => vec![affinity],
            _ => Vec::new(),
        };
        let node_claim = CompilerNodeCapacityClaim {
            peer,
            binding,
            worker_incarnation: snapshot.worker_incarnation,
            capacity_revision: snapshot.capacity_revision,
            total: measurements.total,
            busy: measurements.busy,
            warm_sessions,
            observed_at: probe_started_at,
            expires_at,
        };
        let verified_node = VerifiedCompilerNodeCapacity::admit(
            node_claim.clone(),
            &ExactNodeCapacityVerifier(node_claim),
        )
        .map_err(|error| {
            ClusterDispatchError::ResultRejected(format!("remote capacity: {error}"))
        })?;
        let restart = scheduler
            .observe_peer_incarnation(peer, snapshot.worker_incarnation, probe_started_at)
            .map_err(|_| ClusterDispatchError::ProbeRejected)?;
        for invalidated in restart.invalidated {
            let Ok(invalidated_peer) = assignment_peer_id(invalidated) else {
                continue;
            };
            if CompilerPeerId::new(*invalidated_peer.as_bytes()).ok() != Some(peer) {
                continue;
            }
            let Ok(scope) = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
                invalidated,
                namespace_id,
            ) else {
                continue;
            };
            let Ok(mut cancellation) = backend_engine::cluster_transport::connect_control(
                &self.endpoint,
                EndpointAddr::new(invalidated_peer).with_ip_addr(grant.address()),
                invalidated_peer,
                scope,
                backend_engine::cluster_transport::ControlRole::Coordinator,
            )
            .await
            else {
                continue;
            };
            let _ = backend_engine::compiler_cluster_transport::send_compiler_cancel(
                &mut cancellation,
                invalidated,
                namespace_id,
                backend_engine::cluster_transport::CancelReason::WorkerUnavailable,
            )
            .await;
            let _ = cancellation.finish().await;
        }
        let candidate = CompilerBalancedRemote::new(verified_work, verified_node).map_err(
            |error: CompilerNodeCapacityError| {
                ClusterDispatchError::ResultRejected(format!("remote preflight pairing: {error}"))
            },
        )?;
        Ok(Some((
            candidate,
            PendingRemoteCostProbe {
                peer: *worker_id.as_bytes(),
                recipe: manifest.recipe(),
                cost,
                input_payload_bytes: inventory.payload_bytes,
                probe_elapsed_ms: measurements.probe_elapsed_ms,
                estimated_input_transfer_ms: cost.completion.input_transfer,
                placement_total_ms: estimate.placement_total_ms,
                exploratory: estimate.exploratory,
            },
        )))
    }

    /// Offer one already admitted, exact remote assignment and return only a checked candidate.
    ///
    /// This helper is intentionally pre-admitted: production placement must first use the live
    /// probe path to construct the exact `CompilerBalancedRemote` and scheduler assignment. The
    /// helper reopens persisted trust, validates the capture and admission grant, offers V2, and
    /// performs bounded CAS-backed input/result transfer. It does not select a Turso head or
    /// send a Stored ACK. A result is journaled as a rejection only after a typed semantic check
    /// conclusively rejects it; transport, deadline, local storage, and allocation failures leave
    /// the worker result pending for recovery. The caller publishes checked output through
    /// `publish_checked_remote` and retains the selection intent until owner ACK recovery.
    pub(crate) fn run_pre_admitted_assignment(
        &self,
        scheduler: &CompilerClusterScheduler,
        assignment_lease: CompilerAssignmentLease,
        namespace_id: [u8; 16],
        capture: &backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
        input_admission: VerifiedCompilerInputAdmission,
        deadline_unix_ms: u64,
        mut journal_hook: impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<CheckedRemoteCompilerCandidate, ClusterDispatchError> {
        let assignment = assignment_lease.assignment();
        let assignment_key = pending_cost_probe_key(namespace_id, assignment);
        let cost_probe = self
            .pending_cost_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&assignment_key);
        let assignment_started = std::time::Instant::now();
        let mut worker_address = None;
        let mut rejected_result = None;
        let result = (|| {
            scheduler
                .validate_assignment(assignment)
                .map_err(|_| ClusterDispatchError::AssignmentRejected)?;
            if !input_admission.matches(assignment, namespace_id, capture) {
                return Err(ClusterDispatchError::InputRejected);
            }
            let worker = self.resolve_trusted_worker(&input_admission)?;
            let address = EndpointAddr::new(worker.peer()).with_ip_addr(worker.address());
            worker_address = Some(address.clone());
            self.block_on(async {
                tokio::time::timeout(
                    remaining_duration(deadline_unix_ms)?,
                    self.run_pre_admitted_assignment_async(
                        scheduler,
                        assignment,
                        namespace_id,
                        worker.clone(),
                        address,
                        capture,
                        input_admission,
                        deadline_unix_ms,
                        &mut rejected_result,
                        &mut journal_hook,
                    ),
                )
                .await
                .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            })
        })();
        match result {
            Ok(mut candidate) => {
                if let Some(probe) = cost_probe {
                    let total_elapsed_ms = u64::try_from(assignment_started.elapsed().as_millis())
                        .unwrap_or(u64::MAX)
                        .max(1);
                    candidate.cost_model_updated = current_unix_ms().is_ok_and(|now| {
                        self.cost_model
                            .observe_completion(
                                probe,
                                total_elapsed_ms,
                                candidate.output_transfer_bytes,
                                candidate.output_transfer_elapsed_ms,
                                now,
                            )
                            .is_ok()
                    });
                }
                Ok(candidate)
            }
            Err(error) => {
                let mut ack_error = None;
                let result_was_announced = rejected_result.is_some();
                if is_conclusive_result_rejection(&error)
                    && let Some(identity) = rejected_result.take()
                {
                    // Only a typed, conclusive result-admission failure can retire the worker's
                    // durable result. The helper sends no ResultAck if the durable intent fails.
                    match self.block_on(tokio::time::timeout(
                        CLUSTER_IO_TIMEOUT,
                        self.retire_result_disposition(
                            &identity,
                            ResultAckDisposition::Rejected(ResultRejectReason::Admission),
                            &mut journal_hook,
                        ),
                    )) {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => ack_error = Some(error),
                        Err(_) => {
                            ack_error = Some(ClusterDispatchError::AckRejected(
                                "result rejection acknowledgement timed out".into(),
                            ));
                        }
                    }
                }
                // A receipt followed by a timeout, transport error, local CAS error, or allocation
                // failure is still a recoverable worker result. Drop only the in-memory identity;
                // keep the worker's durable result and the journal reservation for a later sweep.
                rejected_result = None;
                if !result_was_announced && let Some(address) = worker_address {
                    // Tell a still-running worker to stop before freeing the local capacity slot.
                    // The authenticated control stream remains tied to the same exact attempt.
                    let cancellation = self.block_on(tokio::time::timeout(
                        CLUSTER_IO_TIMEOUT,
                        async {
                            let mut channel =
                                backend_engine::compiler_cluster_transport::connect_compiler_control(
                                    scheduler,
                                    &self.endpoint,
                                    address,
                                    assignment,
                                    namespace_id,
                                )
                                .await?;
                            backend_engine::compiler_cluster_transport::send_compiler_cancel(
                                &mut channel,
                                assignment,
                                namespace_id,
                                backend_engine::cluster_transport::CancelReason::Requested,
                            )
                            .await?;
                            channel.finish().await?;
                            Ok::<(), backend_engine::compiler_cluster_transport::CompilerTransportBridgeError>(())
                        },
                    ));
                    if let Err(transport_error) = cancellation {
                        // A timeout or lost peer cannot retain the owner's bounded scheduler slot.
                        let _ = transport_error;
                    }
                }
                match scheduler.cancel(assignment) {
                    Ok(_)
                    | Err(backend_engine::application::CompilerAssignmentError::StaleAttempt) => {}
                    Err(_) => return Err(ClusterDispatchError::AssignmentRejected),
                }
                self.pending_cost_probes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(&assignment_key);
                if let Some(ack_error) = ack_error {
                    return Err(ack_error);
                }
                Err(error)
            }
        }
    }

    /// Acknowledge a result only after the caller's durable authority selected its candidate.
    /// The worker's retained result row makes retrying this operation safe after an owner crash.
    pub(crate) fn acknowledge_stored(
        &self,
        pending: &PendingStoredCompilerResult,
        mut journal_hook: impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<(), ClusterDispatchError> {
        let trust = self.trusted_workers()?;
        if !trust.grants().contains(&pending.worker_grant)
            || pending.worker_grant.namespace_id() != pending.namespace_id
            || EndpointAddr::new(pending.worker_grant.peer())
                .with_ip_addr(pending.worker_grant.address())
                != pending.worker_address
        {
            return Err(ClusterDispatchError::AckRejected(
                "worker trust grant changed before Stored acknowledgement".into(),
            ));
        }
        let identity = CompilerResultIdentity {
            assignment: pending.assignment,
            namespace_id: pending.namespace_id,
            worker_grant: pending.worker_grant.clone(),
            closure_id: pending.stored.receipt().closure_id,
        };
        self.block_on(tokio::time::timeout(
            CLUSTER_IO_TIMEOUT,
            self.retire_result_disposition(
                &identity,
                ResultAckDisposition::Stored,
                &mut journal_hook,
            ),
        ))
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
    }

    /// Rejects a checked worker result only when the authority proves that no selection attempt
    /// is pending for it (for example, a scanner-fence check failed before candidate creation).
    /// Ambiguous compare-and-select outcomes must go through the durable pending-ACK proof path.
    pub(crate) fn reject_unselected_stored(
        &self,
        pending: &PendingStoredCompilerResult,
        mut journal_hook: impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<(), ClusterDispatchError> {
        let trust = self.trusted_workers()?;
        if !trust.grants().contains(&pending.worker_grant)
            || pending.worker_grant.namespace_id() != pending.namespace_id
            || EndpointAddr::new(pending.worker_grant.peer())
                .with_ip_addr(pending.worker_grant.address())
                != pending.worker_address
        {
            return Err(ClusterDispatchError::AckRejected(
                "worker trust grant changed before rejection acknowledgement".into(),
            ));
        }
        let identity = CompilerResultIdentity {
            assignment: pending.assignment,
            namespace_id: pending.namespace_id,
            worker_grant: pending.worker_grant.clone(),
            closure_id: pending.stored.receipt().closure_id,
        };
        self.block_on(tokio::time::timeout(
            CLUSTER_IO_TIMEOUT,
            self.retire_result_disposition(
                &identity,
                ResultAckDisposition::Rejected(ResultRejectReason::Admission),
                &mut journal_hook,
            ),
        ))
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
    }

    /// Terminally retires an exact recovered result after locald proved the current attempt is
    /// rejected by its source fence or execution grant. The old grant is used only to reach the
    /// same authenticated peer; current-trust checks are intentionally not performed here, so a
    /// revoked worker can still release its durable result. The durable journal hook must return
    /// an exact prepared Rejected(Admission) token before any ACK is sent.
    pub(crate) fn reject_recovered_identity(
        &self,
        identity: &CompilerResultIdentity,
        mut journal_hook: impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<(), ClusterDispatchError> {
        validate_terminal_assignment_identity(
            identity.assignment,
            identity.namespace_id,
            &identity.worker_grant,
            Some(identity.closure_id),
        )?;
        self.block_on(tokio::time::timeout(
            CLUSTER_IO_TIMEOUT,
            self.retire_result_disposition(
                identity,
                ResultAckDisposition::Rejected(ResultRejectReason::Admission),
                &mut journal_hook,
            ),
        ))
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
    }

    /// Retry an owner-restart result disposition only from a sealed locald token.
    ///
    /// ACK-pending tokens require current Turso proof and worker trust, except the sealed
    /// selected-history cleanup token, which can send only Stored for an already-selected exact
    /// result. AwaitingRetirementConfirm is confirmation-only and can be minted only after an
    /// exact authenticated retirement receipt. Neither cleanup-only token authorizes new work.
    pub(crate) fn acknowledge_recovered(
        &self,
        proof: &crate::builtin::pending_stored::RecoveredAckScope,
        before_retirement_confirm: impl FnOnce(&ControlResultRetired) -> Result<(), String>,
    ) -> Result<RecoveredAckReceipt, ClusterDispatchError> {
        if proof.owner_endpoint_id() != *self.owner_id.as_bytes() {
            return Err(ClusterDispatchError::AckRejected(
                "recovery proof names another owner endpoint".into(),
            ));
        }
        let awaiting_retirement_confirmation = proof.awaiting_retirement_confirmation();
        let selected_stored_cleanup_only = proof.selected_stored_cleanup_only();
        let disposition = proof.disposition();
        if selected_stored_cleanup_only && disposition != ResultAckDisposition::Stored {
            return Err(ClusterDispatchError::AckRejected(
                "selected-history cleanup scope does not authorize Stored disposition".into(),
            ));
        }
        if !awaiting_retirement_confirmation && !selected_stored_cleanup_only {
            let current_trust = self.trusted_workers()?;
            if !proof.is_currently_trusted(&current_trust) {
                return Err(ClusterDispatchError::AckRejected(
                    "worker trust changed before recovery acknowledgement".into(),
                ));
            }
        }
        // The sealed journal proof owns the complete disposition, including
        // preselection Rejected(Admission). Do not derive it from a partial
        // kind mapping here: that could turn an exact durable rejection into a
        // different worker terminal result.
        let mut before_retirement_confirm = Some(before_retirement_confirm);
        self.block_on(async {
            let mut channel = tokio::time::timeout(
                CLUSTER_IO_TIMEOUT,
                backend_engine::cluster_transport::connect_control(
                    &self.endpoint,
                    proof.worker_address(),
                    proof.expected_peer(),
                    proof.scope(),
                    backend_engine::cluster_transport::ControlRole::Coordinator,
                ),
            )
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
            if channel.peer() != proof.expected_peer() || channel.scope() != Some(proof.scope()) {
                return Err(ClusterDispatchError::AckRejected(
                    "recovery control stream changed peer or exact scope".into(),
                ));
            }
            let mut retired_for_confirmation = None;
            if !awaiting_retirement_confirmation {
                let ack = ControlResultAck::new(proof.scope(), proof.closure_id(), disposition)
                    .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?;
                tokio::time::timeout(
                    CLUSTER_IO_TIMEOUT,
                    channel.send(&ControlMessage::ResultAck(ack)),
                )
                .await
                .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?;
                let retired = match tokio::time::timeout(CLUSTER_IO_TIMEOUT, channel.receive())
                    .await
                    .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                    .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?
                {
                    ControlMessage::ResultRetired(retired) => retired,
                    _ => {
                        return Err(ClusterDispatchError::AckRejected(
                            "worker omitted the exact durable retirement receipt".into(),
                        ));
                    }
                };
                if !retired.matches_ack(&ack, channel.peer())
                    || channel.peer() != proof.expected_peer()
                {
                    return Err(ClusterDispatchError::AckRejected(
                        "worker retirement receipt changed peer, scope, closure, or disposition"
                            .into(),
                    ));
                }
                let persist = before_retirement_confirm.take().ok_or_else(|| {
                    ClusterDispatchError::AckRejected(
                        "retirement persistence callback was already consumed".into(),
                    )
                })?;
                tokio::task::block_in_place(|| persist(&retired)).map_err(|error| {
                    ClusterDispatchError::AckRejected(format!(
                        "persist worker retirement before confirmation: {error}"
                    ))
                })?;
                retired_for_confirmation = Some(retired);
            }
            let confirm = ControlResultRetirementConfirm::new(
                proof.scope(),
                proof.closure_id(),
                disposition,
                self.owner_id,
            )
            .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?;
            if let Some(retired) = retired_for_confirmation.as_ref()
                && !confirm.matches_retired(retired, self.owner_id)
            {
                return Err(ClusterDispatchError::AckRejected(
                    "owner confirmation changed the durable worker retirement receipt".into(),
                ));
            }
            tokio::time::timeout(
                CLUSTER_IO_TIMEOUT,
                channel.send(&ControlMessage::ResultRetirementConfirm(confirm)),
            )
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?;
            let applied = match tokio::time::timeout(CLUSTER_IO_TIMEOUT, channel.receive())
                .await
                .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?
            {
                ControlMessage::ResultRetirementApplied(applied) => applied,
                _ => {
                    return Err(ClusterDispatchError::AckRejected(
                        "worker omitted the exact retirement-applied receipt".into(),
                    ));
                }
            };
            if !applied.matches_confirm(&confirm, channel.peer())
                || channel.peer() != proof.expected_peer()
            {
                return Err(ClusterDispatchError::AckRejected(
                    "worker retirement-applied receipt changed peer, scope, closure, or disposition"
                        .into(),
                ));
            }
            tokio::time::timeout(CLUSTER_IO_TIMEOUT, channel.finish())
                .await
                .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))
        })?;
        Ok(RecoveredAckReceipt {
            journal_id: proof.journal_id(),
            kind: proof.kind(),
        })
    }

    fn resolve_trusted_worker(
        &self,
        input_admission: &VerifiedCompilerInputAdmission,
    ) -> Result<TrustedCompilerWorkerGrant, ClusterDispatchError> {
        let grant = input_admission.execution_grant();
        let manifest = input_admission.evidence().capture().manifest();
        let endpoint_id = EndpointId::from_bytes(&grant.peer().as_bytes())
            .map_err(|_| ClusterDispatchError::InputRejected)?;
        let policy = self.trusted_workers()?;
        policy
            .grants()
            .iter()
            .find(|candidate| {
                candidate.peer() == endpoint_id
                    && candidate.namespace_id() == grant.namespace_id()
                    && policy.authorizes(
                        endpoint_id,
                        grant.namespace_id(),
                        manifest.recipe(),
                        manifest.profile(),
                        manifest.stage(),
                        manifest.toolchain(),
                        manifest.environment(),
                        manifest.target_platform(),
                    )
            })
            .cloned()
            .ok_or(ClusterDispatchError::NoEligibleWorker)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_pre_admitted_assignment_async(
        &self,
        scheduler: &CompilerClusterScheduler,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        worker_grant: TrustedCompilerWorkerGrant,
        worker_address: EndpointAddr,
        capture: &backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
        input_admission: VerifiedCompilerInputAdmission,
        deadline_unix_ms: u64,
        rejected_result: &mut Option<CompilerResultIdentity>,
        journal_hook: &mut impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<CheckedRemoteCompilerCandidate, ClusterDispatchError> {
        scheduler
            .validate_assignment(assignment)
            .map_err(|_| ClusterDispatchError::AssignmentRejected)?;
        if deadline_unix_ms <= current_unix_ms()? {
            return Err(ClusterDispatchError::DeadlineExpired);
        }
        let peer = assignment_peer_id(assignment)?;
        let scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            assignment,
            namespace_id,
        )
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let input_closure = capture.closure();
        let verified_input = capture
            .verify_in_store(&self.store)
            .map_err(|error| ClusterDispatchError::Store(error.to_string()))?;
        let input_pages = self
            .build_input_grant_pages(
                scheduler,
                assignment,
                namespace_id,
                input_closure,
                &verified_input,
                deadline_unix_ms,
            )
            .await?;

        let input_transfer_scope = TransferScope::from_store_closure(scope, input_closure)
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let (_route_guard, mut control_events) =
            self.ingress.register(peer, input_transfer_scope)?;

        let mut offer_channel =
            backend_engine::compiler_cluster_transport::connect_compiler_control(
                scheduler,
                &self.endpoint,
                worker_address.clone(),
                assignment,
                namespace_id,
            )
            .await
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        notify_compiler_result_journal(
            journal_hook,
            CompilerResultJournalEvent::OfferMayBeSent {
                assignment,
                namespace_id,
            },
        )?;
        backend_engine::compiler_cluster_transport::send_compiler_offer_v2(
            &mut offer_channel,
            scheduler,
            assignment,
            namespace_id,
            input_closure,
            capture.manifest(),
            u32::try_from(input_pages.len()).map_err(|_| ClusterDispatchError::InputRejected)?,
            deadline_unix_ms,
        )
        .await
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let accepted = backend_engine::compiler_cluster_transport::receive_compiler_accept(
            &mut offer_channel,
            scheduler,
            assignment,
            namespace_id,
        )
        .await
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        offer_channel
            .finish()
            .await
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        if !accepted {
            self.record_no_result_terminal(assignment, namespace_id, peer)
                .await?;
            notify_compiler_result_journal(
                journal_hook,
                CompilerResultJournalEvent::NoResultTerminal {
                    assignment,
                    namespace_id,
                },
            )?;
            return Err(ClusterDispatchError::WorkerDeclined);
        }
        backend_engine::compiler_cluster_transport::send_compiler_grant_pages(
            scheduler,
            &self.endpoint,
            worker_address.clone(),
            assignment,
            namespace_id,
            input_closure,
            &input_pages,
        )
        .await
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;

        let mut fenced_result: Option<
            backend_engine::compiler_cluster_transport::FencedCompilerResult,
        > = None;
        let mut result_page_receiver = None;
        let mut result_pages = None;
        while result_pages.is_none() {
            let event =
                tokio::time::timeout(remaining_duration(deadline_unix_ms)?, control_events.recv())
                    .await
                    .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                    .ok_or(ClusterDispatchError::WorkerFailed)?;
            let OwnerControlEvent {
                mut channel,
                first_message: message,
            } = event;
            match message {
                ControlMessage::ResultReceipt(receipt) => {
                    let expected_scope =
                        backend_engine::compiler_cluster_transport::compiler_assignment_scope(
                            assignment,
                            namespace_id,
                        )
                        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                    if let Some(previous) = fenced_result {
                        let exact_route = exact_control_route(&channel, peer, expected_scope);
                        let same_receipt = same_result_receipt(receipt, previous.receipt());
                        channel
                            .finish()
                            .await
                            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                        if !exact_route {
                            return Err(ClusterDispatchError::AssignmentRejected);
                        }
                        if !same_receipt {
                            return Err(post_receipt_terminal_error(
                                true,
                                "worker replayed a result receipt that contradicts its prior receipt",
                            ));
                        }
                        continue;
                    }
                    if channel.peer() == peer
                        && channel.scope() == Some(expected_scope)
                        && receipt.scope == expected_scope
                        && receipt.closure_id != [0; 32]
                    {
                        *rejected_result = Some(CompilerResultIdentity {
                            assignment,
                            namespace_id,
                            worker_grant: worker_grant.clone(),
                            closure_id: receipt.closure_id,
                        });
                    }
                    let terminal =
                                backend_engine::compiler_cluster_transport::admit_compiler_terminal_message(
                                    &channel,
                                    scheduler,
                                    assignment,
                                    namespace_id,
                                    *input_closure.as_bytes(),
                                    ControlMessage::ResultReceipt(receipt),
                                )
                                .map_err(map_recovered_result_bridge_error)?;
                    channel
                        .finish()
                        .await
                        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                    match terminal {
                                backend_engine::compiler_cluster_transport::FencedCompilerTerminalMessage::Result(result) => {
                                    result_page_receiver = Some(
                                        backend_engine::compiler_cluster_transport::CompilerResultGrantPageReceiver::new(
                                            scheduler,
                                            assignment,
                                            namespace_id,
                                            result,
                                        )
                                        .map_err(map_recovered_result_bridge_error)?,
                                    );
                                    fenced_result = Some(result);
                                    pause_after_live_result_receipt().await?;
                                }
                                backend_engine::compiler_cluster_transport::FencedCompilerTerminalMessage::ExecutionFailed(_) => {
                                    self.record_no_result_terminal(assignment, namespace_id, peer)
                                        .await?;
                                    notify_compiler_result_journal(
                                        journal_hook,
                                        CompilerResultJournalEvent::NoResultTerminal {
                                            assignment,
                                            namespace_id,
                                        },
                                    )?;
                                    return Err(ClusterDispatchError::WorkerFailed);
                                }
                            }
                }
                ControlMessage::WorkerReject(reject) => {
                    let expected_scope =
                        backend_engine::compiler_cluster_transport::compiler_assignment_scope(
                            assignment,
                            namespace_id,
                        )
                        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                    let exact_route = exact_control_route(&channel, peer, expected_scope);
                    if rejected_result.is_some() {
                        channel
                            .finish()
                            .await
                            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                        return Err(post_receipt_terminal_error(
                            exact_route,
                            "worker rejected input after reporting a durable result",
                        ));
                    }
                    if channel.peer() != peer
                        || channel.scope() != Some(expected_scope)
                        || reject.scope != expected_scope
                        || reject.input_closure_id != *input_closure.as_bytes()
                    {
                        return Err(ClusterDispatchError::InputRejected);
                    }
                    channel
                        .finish()
                        .await
                        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                    self.record_no_result_terminal(assignment, namespace_id, peer)
                        .await?;
                    notify_compiler_result_journal(
                        journal_hook,
                        CompilerResultJournalEvent::NoResultTerminal {
                            assignment,
                            namespace_id,
                        },
                    )?;
                    return Err(ClusterDispatchError::WorkerRejected);
                }
                ControlMessage::ExecutionFailed(failure) => {
                    let expected_scope =
                        backend_engine::compiler_cluster_transport::compiler_assignment_scope(
                            assignment,
                            namespace_id,
                        )
                        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                    let exact_route = exact_control_route(&channel, peer, expected_scope);
                    if rejected_result.is_some() {
                        channel
                            .finish()
                            .await
                            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                        return Err(post_receipt_terminal_error(
                            exact_route,
                            "worker reported execution failure after reporting a durable result",
                        ));
                    }
                    if channel.peer() != peer
                        || channel.scope() != Some(expected_scope)
                        || failure.scope != expected_scope
                        || failure.input_closure_id != *input_closure.as_bytes()
                    {
                        return Err(ClusterDispatchError::AssignmentRejected);
                    }
                    channel
                        .finish()
                        .await
                        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                    self.record_no_result_terminal(assignment, namespace_id, peer)
                        .await?;
                    notify_compiler_result_journal(
                        journal_hook,
                        CompilerResultJournalEvent::NoResultTerminal {
                            assignment,
                            namespace_id,
                        },
                    )?;
                    return Err(ClusterDispatchError::WorkerFailed);
                }
                ControlMessage::GrantPage(first_page) => {
                    if !exact_control_route(&channel, peer, scope) {
                        channel
                            .finish()
                            .await
                            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                        return Err(ClusterDispatchError::AssignmentRejected);
                    }
                    let receiver = result_page_receiver.as_mut().ok_or(
                        ClusterDispatchError::ResultRejected(
                            "result grant arrived before its receipt".into(),
                        ),
                    )?;
                    receiver
                        .receive_channel_with_first(scheduler, channel, first_page)
                        .await
                        .map_err(map_recovered_result_bridge_error)?;
                    if receiver.is_complete() {
                        result_pages = Some(
                            result_page_receiver
                                .take()
                                .ok_or(ClusterDispatchError::ResultRejected(
                                    "result page receiver disappeared".into(),
                                ))?
                                .into_pages()
                                .map_err(map_recovered_result_bridge_error)?,
                        );
                    }
                }
                _ => {
                    let exact_route = exact_control_route(&channel, peer, scope);
                    channel
                        .finish()
                        .await
                        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                    return Err(post_receipt_terminal_error(
                        rejected_result.is_some() && exact_route,
                        "worker sent a contradictory control message after reporting a durable result",
                    ));
                }
            }
        }

        let fenced_result = fenced_result.ok_or(ClusterDispatchError::ResultRejected(
            "worker result pages arrived without an admitted receipt".into(),
        ))?;
        let result_pages = result_pages.ok_or(ClusterDispatchError::ResultRejected(
            "worker result grant series is incomplete".into(),
        ))?;
        let output_sink = self.store.artifact_sink(cluster_artifact_budget());
        let (pinned_stored_receipt, output_transfer_bytes, output_transfer_elapsed_ms) = self
            .receive_result_closure(
                scheduler,
                assignment,
                namespace_id,
                peer,
                worker_address.clone(),
                fenced_result,
                &result_pages,
                &output_sink,
                deadline_unix_ms,
            )
            .await
            .map_err(classify_received_result_transfer_error)?;
        let stored_receipt = pinned_stored_receipt.receipt();
        let stored = backend_engine::compiler_cluster_transport::store_compiler_result(
            scheduler,
            fenced_result,
            stored_receipt,
        )
        .map_err(map_recovered_result_bridge_error)?;
        let admitted = match admit_remote_compiler_candidate(
            &self.store,
            stored,
            assignment,
            namespace_id,
            capture,
            input_admission,
            cluster_artifact_budget(),
        ) {
            Ok(candidate) => candidate,
            Err(error) => return Err(map_recovered_candidate_error(error)),
        };
        *rejected_result = None;
        let pending = PendingStoredCompilerResult {
            assignment,
            namespace_id,
            worker_address,
            worker_grant,
            stored,
        };
        Ok(CheckedRemoteCompilerCandidate {
            admitted,
            pending,
            _output_pin: pinned_stored_receipt,
            output_transfer_bytes,
            output_transfer_elapsed_ms,
            cost_model_updated: false,
        })
    }

    async fn build_input_grant_pages(
        &self,
        scheduler: &CompilerClusterScheduler,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        closure: ClosureId,
        verified: &backend_engine::compiler_cluster_transport::VerifiedFullWorkspaceClosureV2,
        deadline_unix_ms: u64,
    ) -> Result<Vec<ControlGrantPage>, ClusterDispatchError> {
        let mut grants = Vec::new();
        let mut payload_bytes = 0_u64;
        for object_id in verified.member_ids() {
            let object_id_untrusted = UntrustedObjectId::from_bytes(*object_id.as_bytes());
            let member = tokio::time::timeout(
                CLUSTER_IO_TIMEOUT,
                self.catalog
                    .register_closure_member(closure, object_id_untrusted),
            )
            .await
            .map_err(|_| ClusterDispatchError::InputRejected)?
            .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?
            .ok_or(ClusterDispatchError::InputRejected)?;
            let materialized = member.materialized();
            payload_bytes = payload_bytes
                .checked_add(materialized.object.payload_length)
                .ok_or(ClusterDispatchError::InputRejected)?;
            if payload_bytes > CLUSTER_ARTIFACT_BYTES_LIMIT {
                return Err(ClusterDispatchError::InputRejected);
            }
            for range in object_ranges(materialized.object.payload_length)? {
                grants.push(
                    backend_engine::compiler_cluster_transport::issue_compiler_object_range(
                        scheduler,
                        &self.issuer,
                        assignment,
                        namespace_id,
                        self.owner_id,
                        closure,
                        member,
                        range,
                        MAX_RESPONSE_BYTES,
                        deadline_unix_ms,
                        capability_nonce(assignment, closure, materialized.object.object_id, range),
                    )
                    .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?,
                );
                if grants.len() > MAX_CONTROL_GRANT_PAGES as usize * MAX_OFFER_CAPABILITIES {
                    return Err(ClusterDispatchError::InputRejected);
                }
            }
        }
        if grants.is_empty() {
            return Err(ClusterDispatchError::InputRejected);
        }
        let page_count = grants.len().div_ceil(MAX_OFFER_CAPABILITIES);
        if page_count > MAX_CONTROL_GRANT_PAGES as usize {
            return Err(ClusterDispatchError::InputRejected);
        }
        let page_count =
            u32::try_from(page_count).map_err(|_| ClusterDispatchError::InputRejected)?;
        let scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            assignment,
            namespace_id,
        )
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let page_capacity =
            usize::try_from(page_count).map_err(|_| ClusterDispatchError::InputRejected)?;
        let mut pages = Vec::with_capacity(page_capacity);
        for (page_index, page_grants) in grants.chunks(MAX_OFFER_CAPABILITIES).enumerate() {
            pages.push(ControlGrantPage {
                scope,
                direction: GrantDirection::InputsToWorker,
                page_index: u32::try_from(page_index)
                    .map_err(|_| ClusterDispatchError::InputRejected)?,
                page_count,
                grants: page_grants.to_vec(),
            });
        }
        Ok(pages)
    }

    #[allow(clippy::too_many_arguments)]
    async fn receive_result_closure(
        &self,
        scheduler: &CompilerClusterScheduler,
        assignment: CompilerAssignment,
        namespace_id: [u8; 16],
        peer: EndpointId,
        worker_address: EndpointAddr,
        result: backend_engine::compiler_cluster_transport::FencedCompilerResult,
        pages: &[ControlGrantPage],
        output_sink: &ArtifactSink,
        deadline_unix_ms: u64,
    ) -> Result<(PinnedStoredClosureReceipt, u64, u64), ClusterDispatchError> {
        let receipt = result.receipt();
        let scope = backend_engine::compiler_cluster_transport::compiler_assignment_scope(
            assignment,
            namespace_id,
        )
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        // Result closure bytes remain an untrusted receipt claim until the complete grant
        // inventory is admitted and the bounded ArtifactSession verifies the stored closure.
        let transfer_scope = TransferScope::new(
            scope.namespace_id,
            scope.work_id,
            scope.attempt,
            scope.fence,
            receipt.closure_id,
        )
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        let worker_artifact_policy =
            AdmissionPolicy::new(peer, peer, [self.owner_id], transfer_scope);
        let now_ms = current_unix_ms()?;
        let result_objects = validate_result_grants(
            pages,
            receipt,
            self.owner_id,
            peer,
            transfer_scope,
            &worker_artifact_policy,
            now_ms,
        )?;
        let claims = result_objects
            .iter()
            .map(|object| object.claim)
            .collect::<Vec<_>>();
        let mut session = output_sink
            .clone()
            .begin(ArtifactPlan::new(
                None,
                ArtifactClosureClaim::from_bytes(receipt.closure_id),
                claims,
                Vec::new(),
            ))
            .map_err(|error| ClusterDispatchError::Store(format!("{error:?}")))?;
        let have = session.have_bitmap();
        if have.len() != result_objects.len() {
            return Err(ClusterDispatchError::Store(
                "CAS have bitmap does not match the owner-validated result inventory".into(),
            ));
        }
        let output_transfer_started = std::time::Instant::now();
        let mut output_transfer_bytes = 0_u64;
        for (object_index, object) in result_objects.iter().enumerate() {
            if have.is_present(object_index) {
                continue;
            }
            let checkpoint =
                self.result_checkpoint_path(assignment, receipt.closure_id, object.object_id);
            let first_capability =
                object
                    .grants
                    .first()
                    .ok_or(ClusterDispatchError::ResultRejected(
                        "result object has no Bao range grant".into(),
                    ))?;
            let prior_state = match ResumeState::load(&checkpoint, &first_capability.claims) {
                Ok(state) => Some(state),
                Err(TransportError::Io(error)) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(ClusterDispatchError::Transport(error.to_string()));
                }
            };
            let mut final_resume = None;
            let mut object_downloaded_bytes = 0_u64;
            if let Some(state) = prior_state.as_ref()
                && state.is_complete()
            {
                tokio::time::timeout(
                    remaining_duration(deadline_unix_ms)?,
                    state.verify_complete_payload(&checkpoint),
                )
                .await
                .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                final_resume = Some(state.clone());
            }
            if final_resume.is_none() {
                let first_missing = prior_state.as_ref().map(first_missing_chunk).unwrap_or(0);
                let first_missing_capability = object
                    .grants
                    .iter()
                    .find(|capability| capability.claims.range.start == first_missing)
                    .ok_or_else(|| {
                        ClusterDispatchError::Transport(
                            "checkpoint coverage does not align with admitted result grants".into(),
                        )
                    })?;
                let mut coverage = tokio::time::timeout(
                    remaining_duration(deadline_unix_ms)?,
                    VerifiedCoverage::open(
                        &self.endpoint,
                        &worker_address,
                        peer,
                        first_missing_capability,
                        transfer_scope,
                        &checkpoint,
                    ),
                )
                .await
                .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                for capability in &object.grants {
                    if resume_covers_range(&coverage, capability.claims.range) {
                        continue;
                    }
                    scheduler
                        .validate_assignment(assignment)
                        .map_err(|_| ClusterDispatchError::AssignmentRejected)?;
                    tokio::time::timeout(
                        remaining_duration(deadline_unix_ms)?,
                        coverage.fetch_range(
                            &self.endpoint,
                            worker_address.clone(),
                            capability.clone(),
                        ),
                    )
                    .await
                    .map_err(|_| ClusterDispatchError::DeadlineExpired)?
                    .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
                    let range_bytes = capability
                        .claims
                        .range
                        .end
                        .saturating_sub(capability.claims.range.start)
                        .saturating_mul(1_024);
                    object_downloaded_bytes = object_downloaded_bytes
                        .saturating_add(range_bytes)
                        .min(object.claim.length());
                }
                final_resume = Some(coverage.finish());
            }
            let resume = final_resume.ok_or(ClusterDispatchError::ResultRejected(
                "result object has no Bao range grant".into(),
            ))?;
            if !resume.is_complete() {
                return Err(ClusterDispatchError::ResultRejected(
                    "result object transfer ended with uncovered Bao ranges".into(),
                ));
            }
            resume
                .feed_to_artifact_session(&checkpoint, object_index, &mut session)
                .await
                .map_err(|error| ClusterDispatchError::Store(error.to_string()))?;
            output_transfer_bytes = output_transfer_bytes
                .checked_add(object_downloaded_bytes)
                .ok_or_else(|| {
                    ClusterDispatchError::ResultRejected("result byte count overflow".into())
                })?;
        }
        let stored = tokio::time::timeout(remaining_duration(deadline_unix_ms)?, async move {
            session.finish_pinned()
        })
        .await
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
        .map_err(|error| ClusterDispatchError::Store(format!("{error:?}")))?;
        let output_transfer_elapsed_ms =
            u64::try_from(output_transfer_started.elapsed().as_millis())
                .unwrap_or(u64::MAX)
                .max(1);
        Ok((stored, output_transfer_bytes, output_transfer_elapsed_ms))
    }

    fn result_checkpoint_path(
        &self,
        assignment: CompilerAssignment,
        closure_id: [u8; 32],
        object_id: [u8; 32],
    ) -> PathBuf {
        let scope = assignment.token();
        self.checkpoint_root
            .join(hex(&assignment.work().transfer_work_id()))
            .join(format!(
                "{}-{}-{}",
                scope.attempt().get(),
                hex(&scope.fence().as_bytes()),
                hex(&closure_id)
            ))
            .join(format!("{}.checkpoint", hex(&object_id)))
    }

    async fn retire_result_disposition(
        &self,
        identity: &CompilerResultIdentity,
        disposition: ResultAckDisposition,
        journal_hook: &mut impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<(), ClusterDispatchError> {
        validate_terminal_assignment_identity(
            identity.assignment,
            identity.namespace_id,
            &identity.worker_grant,
            Some(identity.closure_id),
        )?;
        let prepared = journal_hook(CompilerResultJournalEvent::PrepareDisposition {
            identity,
            disposition,
        })
        .map_err(|error| {
            ClusterDispatchError::Store(format!("persist terminal compiler result intent: {error}"))
        })?
        .ok_or_else(|| {
            ClusterDispatchError::Store(
                "journal did not return a prepared terminal disposition".into(),
            )
        })?;
        if !prepared.matches(identity, disposition) {
            return Err(ClusterDispatchError::AckRejected(
                "prepared journal token changed exact worker result identity".into(),
            ));
        }
        self.send_prepared_result_disposition(identity, disposition, &prepared, journal_hook)
            .await
    }

    async fn send_prepared_result_disposition(
        &self,
        identity: &CompilerResultIdentity,
        disposition: ResultAckDisposition,
        prepared: &crate::builtin::pending_stored::PreparedResultDisposition,
        journal_hook: &mut impl FnMut(
            CompilerResultJournalEvent<'_>,
        ) -> Result<
            Option<crate::builtin::pending_stored::PreparedResultDisposition>,
            String,
        >,
    ) -> Result<(), ClusterDispatchError> {
        let scope = validate_terminal_assignment_identity(
            identity.assignment,
            identity.namespace_id,
            &identity.worker_grant,
            Some(identity.closure_id),
        )?;
        let expected_peer = EndpointId::from_bytes(identity.worker_grant.peer().as_bytes())
            .map_err(|_| ClusterDispatchError::AckRejected("invalid worker endpoint".into()))?;
        let worker_address =
            EndpointAddr::new(expected_peer).with_ip_addr(identity.worker_grant.address());
        if identity.closure_id == [0; 32]
            || identity.worker_grant.namespace_id() != identity.namespace_id
            || !prepared.matches(identity, disposition)
        {
            return Err(ClusterDispatchError::AckRejected(
                "terminal result identity is incomplete or cross-namespace".into(),
            ));
        }
        let mut channel = tokio::time::timeout(
            CLUSTER_IO_TIMEOUT,
            backend_engine::cluster_transport::connect_control(
                &self.endpoint,
                worker_address,
                expected_peer,
                scope,
                backend_engine::cluster_transport::ControlRole::Coordinator,
            ),
        )
        .await
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))?;
        if channel.peer() != expected_peer || channel.scope() != Some(scope) {
            return Err(ClusterDispatchError::AckRejected(
                "terminal result stream changed exact peer or assignment scope".into(),
            ));
        }
        let ack = ControlResultAck::new(scope, identity.closure_id, disposition)
            .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?;
        tokio::time::timeout(
            CLUSTER_IO_TIMEOUT,
            channel.send(&ControlMessage::ResultAck(ack)),
        )
        .await
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
        .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?;
        let retired = match tokio::time::timeout(CLUSTER_IO_TIMEOUT, channel.receive())
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?
        {
            ControlMessage::ResultRetired(retired) => retired,
            _ => {
                return Err(ClusterDispatchError::AckRejected(
                    "worker omitted the exact durable retirement receipt".into(),
                ));
            }
        };
        if !retired.matches_ack(&ack, channel.peer()) || channel.peer() != expected_peer {
            return Err(ClusterDispatchError::AckRejected(
                "worker retirement receipt changed peer, scope, closure, or disposition".into(),
            ));
        }
        notify_compiler_result_journal(
            journal_hook,
            CompilerResultJournalEvent::WorkerRetired {
                identity,
                retired: &retired,
            },
        )?;
        let confirm = ControlResultRetirementConfirm::new(
            scope,
            identity.closure_id,
            disposition,
            self.owner_id,
        )
        .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?;
        if !confirm.matches_retired(&retired, self.owner_id) {
            return Err(ClusterDispatchError::AckRejected(
                "owner confirmation changed the durable worker retirement receipt".into(),
            ));
        }
        tokio::time::timeout(
            CLUSTER_IO_TIMEOUT,
            channel.send(&ControlMessage::ResultRetirementConfirm(confirm)),
        )
        .await
        .map_err(|_| ClusterDispatchError::DeadlineExpired)?
        .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?;
        let applied = match tokio::time::timeout(CLUSTER_IO_TIMEOUT, channel.receive())
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))?
        {
            ControlMessage::ResultRetirementApplied(applied) => applied,
            _ => {
                return Err(ClusterDispatchError::AckRejected(
                    "worker omitted the exact retirement-applied receipt".into(),
                ));
            }
        };
        if !applied.matches_confirm(&confirm, channel.peer()) || channel.peer() != expected_peer {
            return Err(ClusterDispatchError::AckRejected(
                "worker retirement-applied receipt changed peer, scope, closure, or disposition"
                    .into(),
            ));
        }
        tokio::time::timeout(CLUSTER_IO_TIMEOUT, channel.finish())
            .await
            .map_err(|_| ClusterDispatchError::DeadlineExpired)?
            .map_err(|error| ClusterDispatchError::AckRejected(error.to_string()))
    }

    fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.block_on(future)
    }
}

async fn retire_no_result_prefix_with_retry(
    endpoint: Endpoint,
    worker: EndpointId,
    worker_address: std::net::SocketAddr,
    target: RetirementTarget,
) -> bool {
    let worker_addr = EndpointAddr::new(worker).with_ip_addr(worker_address);
    for retry in 0..NO_RESULT_RETIREMENT_RETRIES {
        let result = tokio::time::timeout(
            NO_RESULT_RETIREMENT_TIMEOUT,
            backend_engine::compiler_cluster_transport::retire_compiler_no_result_through(
                &endpoint,
                worker_addr.clone(),
                worker,
                target.no_result_scope,
                target.anchor_scope,
                target.retired_through_epoch,
            ),
        )
        .await;
        if matches!(
            result,
            Ok(Ok(applied))
                if applied.terminal_scope == target.no_result_scope
                    && applied.scope == target.anchor_scope
                    && applied.retired_through_epoch == target.retired_through_epoch
                    && applied.worker_endpoint_id == *worker.as_bytes()
        ) {
            return true;
        }
        if retry + 1 < NO_RESULT_RETIREMENT_RETRIES {
            tokio::time::sleep(NO_RESULT_RETIREMENT_RETRY_DELAY).await;
        }
    }
    false
}

fn trusted_no_result_retirement_routes(
    policy: &TrustedCompilerWorkerPolicy,
) -> BTreeMap<([u8; 16], [u8; 32]), (EndpointId, std::net::SocketAddr)> {
    let mut routes = BTreeMap::new();
    for grant in policy.grants() {
        routes
            .entry((grant.namespace_id(), *grant.peer().as_bytes()))
            .or_insert((grant.peer(), grant.address()));
    }
    routes
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NoResultRetirementSweepReport {
    attempted: usize,
    confirmed: usize,
}

async fn sweep_no_result_retirement_targets<F, Fut>(
    journal: Arc<NoResultRetirementJournal>,
    targets: &[RetirementTarget],
    send: F,
) -> NoResultRetirementSweepReport
where
    F: Fn(RetirementTarget) -> Fut + Clone,
    Fut: Future<Output = bool>,
{
    let mut report = NoResultRetirementSweepReport::default();
    for batch in targets.chunks(NO_RESULT_RETIREMENT_MAX_PARALLEL) {
        let mut attempts = FuturesUnordered::new();
        for target in batch.iter().cloned() {
            let send = send.clone();
            attempts.push(async move {
                let applied = send(target.clone()).await;
                (target, applied)
            });
        }
        while let Some((target, applied)) = attempts.next().await {
            report.attempted = report.attempted.saturating_add(1);
            if applied && journal.acknowledge(target).unwrap_or(false) {
                report.confirmed = report.confirmed.saturating_add(1);
            }
        }
    }
    report
}

async fn run_no_result_retirement_collector(
    journal: Arc<NoResultRetirementJournal>,
    notify: Arc<tokio::sync::Notify>,
    endpoint: Endpoint,
    authority_path: PathBuf,
    trust_policy_path: PathBuf,
    owner_id: EndpointId,
) {
    let mut retry_delay = Duration::from_secs(1);
    loop {
        let unanchored = journal.pending_barriers();
        if !unanchored.is_empty()
            && let Ok(mut authority) =
                backend_extension_turso::TursoAuthority::open(&authority_path).await
        {
            for target in unanchored {
                let namespace = match target.authority_namespace.clone() {
                    Some(namespace) if namespace.namespace_id() == target.namespace_id => {
                        Some(namespace)
                    }
                    Some(_) => None,
                    None => authority
                        .authority_namespace_for_id(target.namespace_id)
                        .await
                        .ok()
                        .flatten(),
                };
                let Some(namespace) = namespace else {
                    continue;
                };
                let peer = match EndpointId::from_bytes(&target.peer) {
                    Ok(peer) => peer,
                    Err(_) => continue,
                };
                if journal
                    .record_no_result_in_namespace(
                        target.no_result_scope,
                        peer,
                        Some(namespace.clone()),
                    )
                    .is_err()
                {
                    continue;
                }
                let barrier = match authority
                    .mint_no_result_retirement_barrier(
                        &namespace,
                        target.no_result_scope.work_id,
                        target.no_result_scope.attempt,
                        target.no_result_scope.fence,
                    )
                    .await
                {
                    Ok(barrier) => barrier,
                    Err(_) => continue,
                };
                if barrier.namespace_id() != target.namespace_id
                    || barrier.terminal_work_id() != &target.no_result_scope.work_id
                    || barrier.terminal_epoch() != target.no_result_scope.attempt
                    || barrier.terminal_fence() != &target.no_result_scope.fence
                {
                    continue;
                }
                let anchor_scope = match AssignmentScope::new(
                    target.namespace_id,
                    *barrier.barrier_work_id(),
                    barrier.barrier_epoch(),
                    *barrier.barrier_fence(),
                ) {
                    Ok(scope) => scope,
                    Err(_) => continue,
                };
                let _ = journal.anchor_turso_barrier(
                    target.no_result_scope,
                    peer,
                    namespace,
                    anchor_scope,
                    barrier.retired_through_epoch(),
                );
            }
        }
        let targets = journal.pending();
        let mut made_progress = false;
        if !targets.is_empty()
            && let Ok(policy) = TrustedCompilerWorkerPolicy::load(&trust_policy_path)
        {
            let trusted = Arc::new(trusted_no_result_retirement_routes(&policy));
            let report = sweep_no_result_retirement_targets(Arc::clone(&journal), &targets, {
                let trusted = Arc::clone(&trusted);
                let endpoint = endpoint.clone();
                move |target| {
                    let trusted = Arc::clone(&trusted);
                    let endpoint = endpoint.clone();
                    async move {
                        let Some((worker, address)) =
                            trusted.get(&(target.namespace_id, target.peer)).copied()
                        else {
                            // Revoked or unavailable trust does not delete maintenance debt.
                            return false;
                        };
                        retire_no_result_prefix_with_retry(endpoint, worker, address, target).await
                    }
                }
            })
            .await;
            made_progress = report.confirmed > 0;
        }

        if journal.pending().is_empty() && journal.pending_barriers().is_empty() {
            retry_delay = NO_RESULT_RETIREMENT_IDLE_DELAY;
        } else if made_progress {
            retry_delay = Duration::from_secs(1);
        } else {
            retry_delay = retry_delay
                .saturating_mul(2)
                .min(NO_RESULT_RETIREMENT_MAX_DELAY);
        }
        tokio::select! {
            _ = notify.notified() => {
                retry_delay = Duration::from_secs(1);
            }
            _ = tokio::time::sleep(retry_delay) => {}
        }
        // Recheck owner identity on each sweep. The file itself is owner-bound, and this guards
        // against accidental future endpoint replacement in a long-lived process.
        if journal.owner_id() != *owner_id.as_bytes() {
            return;
        }
    }
}

fn assignment_peer_id(assignment: CompilerAssignment) -> Result<EndpointId, ClusterDispatchError> {
    let backend_engine::application::CompilerAssignmentRoute::Remote(peer) = assignment.route()
    else {
        return Err(ClusterDispatchError::AssignmentRejected);
    };
    EndpointId::from_bytes(&peer.as_bytes()).map_err(|_| ClusterDispatchError::AssignmentRejected)
}

fn validate_terminal_assignment_identity(
    assignment: CompilerAssignment,
    namespace_id: [u8; 16],
    worker_grant: &TrustedCompilerWorkerGrant,
    closure_id: Option<[u8; 32]>,
) -> Result<AssignmentScope, ClusterDispatchError> {
    let assignment_peer = assignment_peer_id(assignment)?;
    let grant_peer = EndpointId::from_bytes(worker_grant.peer().as_bytes())
        .map_err(|_| ClusterDispatchError::AckRejected("invalid worker endpoint".into()))?;
    if assignment_peer != grant_peer
        || worker_grant.namespace_id() != namespace_id
        || closure_id.is_some_and(|closure| closure == [0; 32])
    {
        return Err(ClusterDispatchError::AckRejected(
            "terminal result identity changed exact peer, namespace, or closure".into(),
        ));
    }
    backend_engine::compiler_cluster_transport::compiler_assignment_scope(assignment, namespace_id)
        .map_err(|error| ClusterDispatchError::Transport(error.to_string()))
}

fn is_conclusive_result_rejection(error: &ClusterDispatchError) -> bool {
    matches!(error, ClusterDispatchError::ConclusiveResultRejection(_))
}

fn exact_control_route(
    channel: &ControlChannel,
    expected_peer: EndpointId,
    expected_scope: AssignmentScope,
) -> bool {
    channel.peer() == expected_peer && channel.scope() == Some(expected_scope)
}

fn post_receipt_terminal_error(exact_route: bool, reason: &str) -> ClusterDispatchError {
    if exact_route {
        ClusterDispatchError::ConclusiveResultRejection(reason.into())
    } else {
        ClusterDispatchError::AssignmentRejected
    }
}

fn map_recovered_result_bridge_error(
    error: backend_engine::compiler_cluster_transport::CompilerTransportBridgeError,
) -> ClusterDispatchError {
    use backend_engine::compiler_cluster_transport::CompilerTransportBridgeError as BridgeError;
    match error {
        BridgeError::Transport(error) => ClusterDispatchError::Transport(error.to_string()),
        BridgeError::Assignment(_) => ClusterDispatchError::AssignmentRejected,
        error => ClusterDispatchError::ConclusiveResultRejection(error.to_string()),
    }
}

fn map_recovered_candidate_error(
    error: backend_engine::application::CompilerResultError,
) -> ClusterDispatchError {
    match error {
        backend_engine::application::CompilerResultError::Store(error) => {
            ClusterDispatchError::Store(format!("{error:?}"))
        }
        backend_engine::application::CompilerResultError::Allocation => {
            ClusterDispatchError::ResultRejected(
                "compiler result allocation failed; retry admission".into(),
            )
        }
        error => ClusterDispatchError::ConclusiveResultRejection(error.to_string()),
    }
}

fn classify_received_result_transfer_error(error: ClusterDispatchError) -> ClusterDispatchError {
    match error {
        ClusterDispatchError::ResultRejected(reason) => {
            ClusterDispatchError::ConclusiveResultRejection(reason)
        }
        retryable => retryable,
    }
}

#[cfg(all(feature = "cluster-process-journey-hooks", unix))]
async fn pause_after_pending_recovery_status() -> Result<(), ClusterDispatchError> {
    use std::io::Write;

    let Some(marker) = std::env::var_os("BACKEND_JOURNEY_RECOVERED_PENDING_MARKER") else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let marker = PathBuf::from(marker);
        if let Some(parent) = marker.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(error),
        };
        file.write_all(b"pending-result-before-grant-pages\n")?;
        file.sync_all()?;
        if let Some(parent) = marker.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        loop {
            thread::sleep(Duration::from_millis(20));
        }
    })
    .await
    .map_err(|error| ClusterDispatchError::Runtime(io::Error::other(error.to_string())))?
    .map_err(ClusterDispatchError::Runtime)
}

#[cfg(not(all(feature = "cluster-process-journey-hooks", unix)))]
async fn pause_after_pending_recovery_status() -> Result<(), ClusterDispatchError> {
    Ok(())
}

#[cfg(all(feature = "cluster-process-journey-hooks", unix))]
async fn pause_after_live_result_receipt() -> Result<(), ClusterDispatchError> {
    use std::io::Write;

    let Some(marker) = std::env::var_os("BACKEND_JOURNEY_LIVE_RECEIPT_MARKER") else {
        return Ok(());
    };
    tokio::task::spawn_blocking(move || -> io::Result<()> {
        let marker = PathBuf::from(marker);
        if let Some(parent) = marker.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
            Err(error) => return Err(error),
        };
        file.write_all(b"live-result-receipt-before-grant-pages\n")?;
        file.sync_all()?;
        if let Some(parent) = marker.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        loop {
            thread::sleep(Duration::from_millis(20));
        }
    })
    .await
    .map_err(|error| ClusterDispatchError::Runtime(io::Error::other(error.to_string())))?
    .map_err(ClusterDispatchError::Runtime)
}

#[cfg(not(all(feature = "cluster-process-journey-hooks", unix)))]
async fn pause_after_live_result_receipt() -> Result<(), ClusterDispatchError> {
    Ok(())
}

fn object_ranges(payload_length: u64) -> Result<Vec<ChunkRange>, ClusterDispatchError> {
    if payload_length > backend_engine::cluster_transport::MAX_OBJECT_BYTES {
        return Err(ClusterDispatchError::InputRejected);
    }
    if payload_length == 0 {
        return Ok(vec![ChunkRange { start: 0, end: 0 }]);
    }
    let total_chunks = payload_length
        .checked_add(1_023)
        .ok_or(ClusterDispatchError::InputRejected)?
        / 1_024;
    let mut ranges = Vec::new();
    let mut start = 0_u64;
    while start < total_chunks {
        let end = start.saturating_add(MAX_RANGE_CHUNKS).min(total_chunks);
        ranges.push(ChunkRange { start, end });
        start = end;
    }
    Ok(ranges)
}

fn validate_result_grants(
    pages: &[ControlGrantPage],
    receipt: backend_engine::cluster_transport::ControlResultReceipt,
    owner: EndpointId,
    worker: EndpointId,
    expected_scope: TransferScope,
    worker_policy: &AdmissionPolicy,
    now_ms: u64,
) -> Result<Vec<OrderedResultObject>, ClusterDispatchError> {
    if pages.len() != receipt.result_grant_pages as usize
        || pages.is_empty()
        || pages.len() > MAX_CONTROL_GRANT_PAGES as usize
    {
        return Err(ClusterDispatchError::ResultRejected(
            "result grant page count differs from the receipt".into(),
        ));
    }
    let mut objects = BTreeMap::<[u8; 32], ResultGrantObject>::new();
    let mut capability_count = 0_usize;
    for (page_index, page) in pages.iter().enumerate() {
        if page.scope != expected_scope.assignment()
            || page.direction != GrantDirection::ResultsToCoordinator
            || page.page_index != page_index as u32
            || page.page_count != pages.len() as u32
            || page.grants.is_empty()
            || page.grants.len() > MAX_OFFER_CAPABILITIES
        {
            return Err(ClusterDispatchError::ResultRejected(
                "result pages are not a complete ordered series".into(),
            ));
        }
        capability_count = capability_count
            .checked_add(page.grants.len())
            .ok_or_else(|| {
                ClusterDispatchError::ResultRejected("result grant count overflow".into())
            })?;
        if capability_count > MAX_CONTROL_GRANT_PAGES as usize * MAX_OFFER_CAPABILITIES {
            return Err(ClusterDispatchError::ResultRejected(
                "result grant inventory exceeds the protocol bound".into(),
            ));
        }
        for capability in &page.grants {
            let claims = &capability.claims;
            verify_admission(worker_policy, owner, capability, claims.range, now_ms).map_err(
                |error| {
                    ClusterDispatchError::ResultRejected(format!(
                        "worker result capability failed signature/scope admission: {error:?}"
                    ))
                },
            )?;
            if claims.client != owner
                || claims.server != worker
                || claims.scope != expected_scope
                || claims.object.object_id == [0; 32]
            {
                return Err(ClusterDispatchError::ResultRejected(
                    "worker result capability has a mismatched endpoint or closure".into(),
                ));
            }
            match objects.get_mut(&claims.object.object_id) {
                Some(existing) => {
                    if existing.mapping != claims.object || existing.blob_hash != claims.blob_hash {
                        return Err(ClusterDispatchError::ResultRejected(
                            "worker changed an object mapping between Bao ranges".into(),
                        ));
                    }
                    existing.grants.push(capability.clone());
                }
                None => {
                    objects.insert(
                        claims.object.object_id,
                        ResultGrantObject {
                            mapping: claims.object,
                            blob_hash: claims.blob_hash,
                            grants: vec![capability.clone()],
                        },
                    );
                }
            }
        }
    }
    if objects.len() != receipt.object_count as usize {
        return Err(ClusterDispatchError::ResultRejected(
            "unique result objects differ from the worker receipt".into(),
        ));
    }
    let mut total_payload_bytes = 0_u64;
    let mut ordered = Vec::with_capacity(objects.len());
    for (object_id, mut object) in objects {
        total_payload_bytes = total_payload_bytes
            .checked_add(object.mapping.payload_length)
            .ok_or_else(|| {
                ClusterDispatchError::ResultRejected("result payload length overflow".into())
            })?;
        object
            .grants
            .sort_by_key(|capability| capability.claims.range.start);
        let total_chunks = if object.mapping.payload_length == 0 {
            0
        } else {
            object.mapping.payload_length.div_ceil(1_024)
        };
        let mut cursor = 0_u64;
        for capability in &object.grants {
            let range = capability.claims.range;
            if object.mapping.payload_length == 0 {
                if object.grants.len() != 1 || range != (ChunkRange { start: 0, end: 0 }) {
                    return Err(ClusterDispatchError::ResultRejected(
                        "empty result object has invalid range coverage".into(),
                    ));
                }
            } else if range.start != cursor || range.end <= range.start {
                return Err(ClusterDispatchError::ResultRejected(
                    "result Bao ranges overlap or leave a gap".into(),
                ));
            } else {
                cursor = range.end;
            }
        }
        if object.mapping.payload_length != 0 && cursor != total_chunks {
            return Err(ClusterDispatchError::ResultRejected(
                "result Bao ranges do not cover the complete object".into(),
            ));
        }
        let claim = object.mapping.artifact_claim();
        ordered.push(OrderedResultObject {
            claim,
            object_id,
            grants: object.grants,
        });
    }
    if total_payload_bytes != receipt.payload_bytes
        || total_payload_bytes > CLUSTER_ARTIFACT_BYTES_LIMIT
    {
        return Err(ClusterDispatchError::ResultRejected(
            "result payload bytes differ from the receipt or owner budget".into(),
        ));
    }
    ordered.sort_by_key(|object| {
        (
            object.claim.schema(),
            *object.claim.key(),
            *object.claim.version(),
        )
    });
    if ordered.windows(2).any(|window| {
        (
            window[0].claim.schema(),
            *window[0].claim.key(),
            *window[0].claim.version(),
        ) >= (
            window[1].claim.schema(),
            *window[1].claim.key(),
            *window[1].claim.version(),
        )
    }) {
        return Err(ClusterDispatchError::ResultRejected(
            "result contains duplicate logical object claims".into(),
        ));
    }
    Ok(ordered)
}

fn first_missing_chunk(state: &ResumeState) -> u64 {
    let mut cursor = 0_u64;
    for range in &state.covered {
        if range.start > cursor {
            break;
        }
        cursor = cursor.max(range.end);
    }
    cursor
}

fn resume_covers_range(coverage: &VerifiedCoverage, range: ChunkRange) -> bool {
    coverage
        .state()
        .covered
        .iter()
        .any(|covered| covered.start <= range.start && covered.end >= range.end)
}

fn capability_nonce(
    assignment: CompilerAssignment,
    closure: ClosureId,
    object_id: [u8; 32],
    range: ChunkRange,
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.compiler.owner-bao-grant.v2\0");
    hasher.update(&assignment.work().transfer_work_id());
    hasher.update(&assignment.token().attempt().get().to_be_bytes());
    hasher.update(&assignment.token().fence().as_bytes());
    hasher.update(closure.as_bytes());
    hasher.update(&object_id);
    hasher.update(&range.start.to_be_bytes());
    hasher.update(&range.end.to_be_bytes());
    let digest = hasher.finalize();
    let mut nonce = [0; 16];
    nonce.copy_from_slice(&digest.as_bytes()[..16]);
    if nonce == [0; 16] {
        nonce[15] = 1;
    }
    nonce
}

fn remaining_duration(deadline_unix_ms: u64) -> Result<Duration, ClusterDispatchError> {
    let remaining = deadline_unix_ms
        .checked_sub(current_unix_ms()?)
        .filter(|remaining| *remaining > 0)
        .ok_or(ClusterDispatchError::DeadlineExpired)?;
    Ok(Duration::from_millis(remaining))
}

fn empty_dispatch_cost_evidence(local: LocalCostEstimate) -> CompilerDispatchCostEvidence {
    CompilerDispatchCostEvidence {
        local,
        remote: None,
        remote_placement_total_ms: None,
        exploratory: false,
        remote_route_provenance: None,
    }
}

/// Gathers live probe outcomes concurrently, but stops waiting shortly after the first usable
/// route. This lets placement compare a small set of peers without allowing one black-holed
/// endpoint to consume the request's whole deadline. Dropping this local stream cancels every
/// remaining future; no detached probe task can outlive the placement decision.
async fn collect_probe_outcomes<F, T>(
    mut probes: FuturesUnordered<F>,
    overall_deadline_unix_ms: u64,
    settle_window: Duration,
) -> Result<Vec<Result<Option<T>, ClusterDispatchError>>, ClusterDispatchError>
where
    F: Future<Output = Result<Option<T>, ClusterDispatchError>>,
{
    let mut outcomes = Vec::new();
    let mut candidate_settle_deadline = None;
    while !probes.is_empty() {
        let deadline = candidate_settle_deadline
            .unwrap_or(overall_deadline_unix_ms)
            .min(overall_deadline_unix_ms);
        match tokio::time::timeout(remaining_duration(deadline)?, probes.next()).await {
            Ok(Some(outcome)) => {
                let found_candidate = matches!(&outcome, Ok(Some(_)));
                outcomes.push(outcome);
                if found_candidate && candidate_settle_deadline.is_none() {
                    let settle_ms = u64::try_from(settle_window.as_millis()).unwrap_or(u64::MAX);
                    candidate_settle_deadline = Some(
                        current_unix_ms()?
                            .saturating_add(settle_ms)
                            .min(overall_deadline_unix_ms),
                    );
                }
            }
            Ok(None) => break,
            Err(_) if candidate_settle_deadline.is_some() => break,
            Err(_) => return Err(ClusterDispatchError::DeadlineExpired),
        }
    }
    Ok(outcomes)
}

fn current_unix_ms() -> Result<u64, ClusterDispatchError> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| ClusterDispatchError::Runtime(io::Error::other(error)))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| ClusterDispatchError::Runtime(io::Error::other("Unix time overflow")))
}

fn compiler_resource_credits(credits: ProbeResourceCredits) -> CompilerResourceCredits {
    CompilerResourceCredits {
        cpu: CompilerCpuCredits::new(credits.cpu_millicores),
        memory: CompilerMemoryCredits::new(credits.memory_bytes),
        transfer: CompilerByteCredits::new(credits.transfer_bytes),
    }
}

fn recovered_capacity_allows(
    resources: CompilerResourceCredits,
    current_total: ProbeResourceCredits,
    result_already_pending: bool,
) -> bool {
    result_already_pending || resources.fits_within(compiler_resource_credits(current_total))
}

fn recovered_resource_demand(
    capture: &backend_engine::compiler_cluster_transport::CapturedFullWorkspaceV2,
    assignment: CompilerAssignment,
    output_bytes: u64,
) -> Result<CompilerResourceCredits, ClusterDispatchError> {
    let input_bytes = capture.payload_bytes();
    let transfer_bytes = input_bytes
        .checked_add(output_bytes)
        .filter(|bytes| *bytes > 0 && *bytes <= CLUSTER_ARTIFACT_BYTES_LIMIT.saturating_mul(2))
        .ok_or(ClusterDispatchError::InputRejected)?;
    if input_bytes == 0
        || output_bytes == 0
        || output_bytes > assignment.work().max_output_bytes()
        || output_bytes > CLUSTER_ARTIFACT_BYTES_LIMIT
    {
        return Err(ClusterDispatchError::InputRejected);
    }
    let memory_bytes = input_bytes
        .saturating_mul(RECOVERED_ASSIGNMENT_MEMORY_PER_INPUT_BYTE)
        .saturating_add(RECOVERED_ASSIGNMENT_MEMORY_FLOOR_BYTES)
        .clamp(
            RECOVERED_ASSIGNMENT_MEMORY_FLOOR_BYTES,
            RECOVERED_ASSIGNMENT_MEMORY_MAX_BYTES,
        );
    Ok(CompilerResourceCredits {
        cpu: CompilerCpuCredits::new(RECOVERED_ASSIGNMENT_CPU_MILLICORES),
        memory: CompilerMemoryCredits::new(memory_bytes),
        transfer: CompilerByteCredits::new(transfer_bytes),
    })
}

fn same_result_receipt(
    left: backend_engine::cluster_transport::ControlResultReceipt,
    right: backend_engine::cluster_transport::ControlResultReceipt,
) -> bool {
    left.scope == right.scope
        && left.target_root == right.target_root
        && left.pack_id == right.pack_id
        && left.closure_id == right.closure_id
        && left.object_count == right.object_count
        && left.payload_bytes == right.payload_bytes
        && left.result_grant_pages == right.result_grant_pages
}

fn max_probe_pressure_per_mille(
    total: CompilerResourceCredits,
    busy: CompilerResourceCredits,
) -> u16 {
    let ratios = [
        (u64::from(busy.cpu.millicores()) * 1_000) / u64::from(total.cpu.millicores().max(1)),
        busy.memory.bytes().saturating_mul(1_000) / total.memory.bytes().max(1),
        busy.transfer.bytes().saturating_mul(1_000) / total.transfer.bytes().max(1),
    ];
    ratios.into_iter().max().unwrap_or(0).min(1_000) as u16
}

#[cfg(unix)]
fn fill_os_random(bytes: &mut [u8]) -> io::Result<()> {
    fs::File::open("/dev/urandom")?.read_exact(bytes)
}

#[cfg(windows)]
fn fill_os_random(bytes: &mut [u8]) -> io::Result<()> {
    backend_platform::win32::random::fill(bytes)
}

#[cfg(not(any(unix, windows)))]
fn fill_os_random(_bytes: &mut [u8]) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "operating-system random source is unavailable",
    ))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod route_cost_tests {
    use super::*;
    use backend_engine::application::{
        CompilerInputIdentityClaim, CompilerInputScope, CompilerNodeCapacityClaim,
        CompilerNodeCapacityError, CompilerNodeCapacityVerifier, CompilerPeerId,
        CompilerRemotePreflightError, CompilerRemotePreflightVerifier, FullWorkspaceInputClaim,
        FullWorkspaceInputError, FullWorkspaceInputVerifier, PackageLineageId,
        RemoteCompilerCapabilityClaim, RemoteCompilerPreflightClaim, RemoteHaveClaim,
        VerifiedCompilerInput, VerifiedCompilerNodeCapacity, VerifierAcceptedFullWorkspaceInput,
    };
    use backend_version::{CompilationTargetDomain, CompileRecipeDomain, ContentId, GenerationId};

    struct AcceptCapture;

    impl FullWorkspaceInputVerifier for AcceptCapture {
        fn verify_full_workspace_capture(
            &self,
            _claim: FullWorkspaceInputClaim,
        ) -> Result<(), FullWorkspaceInputError> {
            Ok(())
        }
    }

    struct AcceptPreflight;

    impl CompilerRemotePreflightVerifier for AcceptPreflight {
        fn verify_remote_preflight(
            &self,
            _expected: CompilerWorkIdentity,
            _claim: RemoteCompilerPreflightClaim,
        ) -> Result<(), CompilerRemotePreflightError> {
            Ok(())
        }
    }

    struct AcceptNodeCapacity;

    impl CompilerNodeCapacityVerifier for AcceptNodeCapacity {
        fn verify_node_capacity(
            &self,
            _claim: &CompilerNodeCapacityClaim,
        ) -> Result<(), CompilerNodeCapacityError> {
            Ok(())
        }
    }

    fn lease_test_work() -> Result<CompilerWorkIdentity, Box<dyn std::error::Error>> {
        let package = PackageLineageId::from_canonical_parts(
            b"registry:test",
            b"pkg://rust/crates/lease-regression",
            b"branch:main",
        )?;
        let target = ContentId::<CompilationTargetDomain>::from_canonical_bytes(b"lease-target");
        let recipe = ContentId::<CompileRecipeDomain>::from_canonical_bytes(b"lease-recipe");
        let identity = CompilerInputIdentityClaim {
            scope: CompilerInputScope {
                package,
                target,
                recipe,
            },
            input_root: GenerationId::from_canonical_bytes(b"lease-input-root"),
            manifest: backend_engine::ReadManifestId::from_value(b"lease-read-manifest"),
        };
        let captured_claim = FullWorkspaceInputClaim {
            identity,
            input_closure_id: [0x51; 32],
            manifest_object_id: [0x52; 32],
        };
        let captured = VerifierAcceptedFullWorkspaceInput::admit(captured_claim, &AcceptCapture)?;
        Ok(CompilerWorkIdentity::new(
            package,
            target,
            recipe,
            VerifiedCompilerInput::FullWorkspaceFresh(captured),
            None,
            1_000_000,
        )?)
    }

    fn lease_test_remote(
        work: CompilerWorkIdentity,
        attempt: u64,
        fence_value: u64,
        now: u64,
    ) -> Result<CompilerBalancedRemote, Box<dyn std::error::Error>> {
        let peer = CompilerPeerId::new([0x61; 32])?;
        let token =
            CompilerAttemptToken::new(AttemptId::new(attempt)?, Fence::from_u64(fence_value)?);
        let binding = CompilerRemoteProbeBinding::new(
            [0x62; 16],
            work.transfer_work_id(),
            token.attempt().get(),
            token.fence().as_bytes(),
            [0x63; 16],
            now,
            now + 1_000,
        )?;
        let full_workspace = work
            .input()
            .full_workspace()
            .expect("fresh test input")
            .claim();
        let preflight = RemoteCompilerPreflightClaim {
            peer,
            work,
            binding,
            worker_incarnation: [0x64; 32],
            capability: RemoteCompilerCapabilityClaim {
                target: work.target(),
                recipe: work.recipe(),
                supported: true,
                capability_digest: [0x65; 32],
            },
            have: RemoteHaveClaim {
                input_manifest: work.input_identity().manifest,
                input_closure_id: full_workspace.input_closure_id,
                manifest_object_id: full_workspace.manifest_object_id,
                inventory_digest: [0x66; 32],
                required_objects: 1,
                verified_have_objects: 1,
                owner_streamable_objects: 0,
                missing_bytes: 0,
                proof_digest: [0x67; 32],
            },
            cost: RemoteCompilerCostClaim {
                completion: backend_engine::CompletionCost {
                    execution: 10,
                    ..backend_engine::CompletionCost::default()
                },
                confidence_per_mille: 900,
                observed_at: now,
                expires_at: now + 1_000,
            },
        };
        let work = VerifiedRemoteCompiler::admit_preflight(work, preflight, &AcceptPreflight)?;
        let node = VerifiedCompilerNodeCapacity::admit(
            CompilerNodeCapacityClaim {
                peer,
                binding,
                worker_incarnation: [0x64; 32],
                capacity_revision: [0x68; 16],
                total: CompilerResourceCredits {
                    cpu: CompilerCpuCredits::new(4_000),
                    memory: CompilerMemoryCredits::new(2 * 1024 * 1024 * 1024),
                    transfer: CompilerByteCredits::new(16 * 1024 * 1024),
                },
                busy: CompilerResourceCredits::default(),
                warm_sessions: Vec::new(),
                observed_at: now,
                expires_at: now + 1_000,
            },
            &AcceptNodeCapacity,
        )?;
        Ok(CompilerBalancedRemote::new(work, node)?)
    }

    fn lease_test_assignment(
        scheduler: &CompilerClusterScheduler,
        work: CompilerWorkIdentity,
        attempt: u64,
        fence_value: u64,
        now: u64,
    ) -> Result<CompilerAssignment, Box<dyn std::error::Error>> {
        let remote = lease_test_remote(work, attempt, fence_value, now)?;
        let peer = remote.node().peer();
        scheduler.observe_peer_incarnation(peer, [0x64; 32], now)?;
        let token =
            CompilerAttemptToken::new(AttemptId::new(attempt)?, Fence::from_u64(fence_value)?);
        let request = CompilerBalancingRequest {
            work,
            demand: CompilerDemand::Background,
            local: LocalCompilerAvailability::Unavailable,
            local_cost: backend_engine::CompletionCost::default(),
            local_start_delay: 0,
            submitted_at: now,
            local_first_budget: 0,
            resources: CompilerResourceCredits {
                cpu: CompilerCpuCredits::new(1_000),
                memory: CompilerMemoryCredits::new(1024 * 1024 * 1024),
                transfer: CompilerByteCredits::new(work.max_output_bytes()),
            },
            session_affinity: None,
            deadline_at: Some(now + 500),
        };
        match scheduler.place_balanced_and_assign(
            &CompilerPlacementPolicy,
            request,
            now,
            &[remote],
            token,
        )? {
            CompilerAssignmentOutcome::Assigned(assignment) => Ok(assignment),
            CompilerAssignmentOutcome::OfflineUnavailable => {
                Err("remote assignment unexpectedly unavailable".into())
            }
        }
    }

    #[test]
    fn pending_ack_retry_drains_progress_without_exponential_backoff() {
        let mut delay = PENDING_ACK_RETRY_PROGRESS_DELAY;
        let mut elapsed = Duration::ZERO;
        for remaining in (0..64).rev() {
            delay =
                next_pending_ack_retry_delay(delay, PendingAckRetryOutcome::Progress { remaining });
            elapsed = elapsed.saturating_add(delay);
        }
        assert_eq!(elapsed, Duration::from_millis(64 * 25));
        assert_eq!(
            next_pending_ack_retry_delay(
                PENDING_ACK_RETRY_PROGRESS_DELAY,
                PendingAckRetryOutcome::TransientFailure,
            ),
            PENDING_ACK_RETRY_INITIAL_DELAY,
        );
        assert_eq!(
            next_pending_ack_retry_delay(
                PENDING_ACK_RETRY_INITIAL_DELAY,
                PendingAckRetryOutcome::TransientFailure,
            ),
            Duration::from_secs(4),
        );
        assert_eq!(
            next_pending_ack_retry_delay(
                PENDING_ACK_RETRY_MAX_DELAY,
                PendingAckRetryOutcome::TransientFailure,
            ),
            PENDING_ACK_RETRY_MAX_DELAY,
        );
        assert_eq!(
            next_pending_ack_retry_delay(
                PENDING_ACK_RETRY_PROGRESS_DELAY,
                PendingAckRetryOutcome::Idle,
            ),
            PENDING_ACK_RETRY_IDLE_DELAY,
        );
    }

    #[test]
    fn pending_ack_retry_factory_runs_thread_local_and_stops_cleanly() {
        let (started_sender, started_receiver) = std::sync::mpsc::channel();
        let worker = PendingAckRetryWorker::start(move || {
            let thread_local_state = std::rc::Rc::new(());
            move || {
                let _ = std::rc::Rc::strong_count(&thread_local_state);
                let _ = started_sender.send(());
                PendingAckRetryOutcome::Idle
            }
        })
        .expect("spawn bounded retry thread");
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("retry factory executes on its OS thread");
        drop(worker);
    }

    #[test]
    fn dropped_assignment_lease_releases_capacity_without_touching_newer_fence()
    -> Result<(), Box<dyn std::error::Error>> {
        let scheduler = CompilerClusterScheduler::with_balancing_limits(
            NonZeroUsize::new(1).expect("positive remote capacity"),
            NonZeroUsize::new(1).expect("positive local capacity"),
            CompilerResourceCredits::default(),
            0,
        );
        let pending_cost_probes = Arc::new(Mutex::new(HashMap::new()));
        let work = lease_test_work()?;
        let namespace_id = [0x62; 16];

        let first = lease_test_assignment(&scheduler, work, 1, 1, 100)?;
        let first_key = pending_cost_probe_key(namespace_id, first);
        pending_cost_probes
            .lock()
            .expect("pending cost probe lock")
            .insert(
                first_key,
                PendingRemoteCostProbe {
                    peer: [0x61; 32],
                    recipe: *work.recipe().as_ref(),
                    cost: RemoteCompilerCostClaim {
                        completion: backend_engine::CompletionCost {
                            execution: 10,
                            ..backend_engine::CompletionCost::default()
                        },
                        confidence_per_mille: 900,
                        observed_at: 100,
                        expires_at: 1_100,
                    },
                    input_payload_bytes: 1,
                    probe_elapsed_ms: 1,
                    estimated_input_transfer_ms: 0,
                    placement_total_ms: 10,
                    exploratory: false,
                },
            );
        let first_lease = CompilerAssignmentLease::new(
            first,
            scheduler.clone(),
            Arc::clone(&pending_cost_probes),
            namespace_id,
        );

        // This is the product path's caller-side V2 evidence/trust failure before the runner is
        // invoked. Dropping its decision lease must free the fixed-capacity slot.
        drop(first_lease);
        assert_eq!(scheduler.live_assignments(), 0);
        assert!(
            pending_cost_probes
                .lock()
                .expect("pending cost probe lock")
                .is_empty()
        );

        let second = lease_test_assignment(&scheduler, work, 2, 2, 101)?;
        let stale_lease = CompilerAssignmentLease::new(
            second,
            scheduler.clone(),
            Arc::clone(&pending_cost_probes),
            namespace_id,
        );
        scheduler.cancel(second)?;
        let newer = lease_test_assignment(&scheduler, work, 3, 3, 102)?;
        let newer_key = pending_cost_probe_key(namespace_id, newer);
        pending_cost_probes
            .lock()
            .expect("pending cost probe lock")
            .insert(
                newer_key,
                PendingRemoteCostProbe {
                    peer: [0x61; 32],
                    recipe: *work.recipe().as_ref(),
                    cost: RemoteCompilerCostClaim {
                        completion: backend_engine::CompletionCost {
                            execution: 10,
                            ..backend_engine::CompletionCost::default()
                        },
                        confidence_per_mille: 900,
                        observed_at: 102,
                        expires_at: 1_102,
                    },
                    input_payload_bytes: 1,
                    probe_elapsed_ms: 1,
                    estimated_input_transfer_ms: 0,
                    placement_total_ms: 10,
                    exploratory: false,
                },
            );
        drop(stale_lease);
        assert!(scheduler.validate_assignment(newer).is_ok());
        assert!(
            pending_cost_probes
                .lock()
                .expect("pending cost probe lock")
                .contains_key(&newer_key)
        );
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_probe_batch_keeps_fast_routes_and_cancels_a_dead_peer() {
        use tokio::sync::oneshot;

        struct NotifyDrop(Option<oneshot::Sender<()>>);

        impl Drop for NotifyDrop {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let (dropped_sender, dropped_receiver) = oneshot::channel();
        let fast: std::pin::Pin<
            Box<dyn Future<Output = Result<Option<u8>, ClusterDispatchError>>>,
        > = Box::pin(async {
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok::<_, ClusterDispatchError>(Some(1_u8))
        });
        let delayed: std::pin::Pin<
            Box<dyn Future<Output = Result<Option<u8>, ClusterDispatchError>>>,
        > = Box::pin(async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            Ok::<_, ClusterDispatchError>(Some(2_u8))
        });
        let dead: std::pin::Pin<
            Box<dyn Future<Output = Result<Option<u8>, ClusterDispatchError>>>,
        > = Box::pin(async move {
            let _notify_drop = NotifyDrop(Some(dropped_sender));
            std::future::pending::<Result<Option<u8>, ClusterDispatchError>>().await
        });
        let mut probes = FuturesUnordered::new();
        probes.push(fast);
        probes.push(delayed);
        probes.push(dead);
        let started = std::time::Instant::now();
        let deadline = current_unix_ms().expect("clock") + 5_000;
        let outcomes = collect_probe_outcomes(probes, deadline, Duration::from_millis(80))
            .await
            .expect("the healthy peers answered before the overall deadline");

        let mut values = outcomes
            .into_iter()
            .filter_map(Result::ok)
            .flatten()
            .collect::<Vec<_>>();
        values.sort_unstable();
        assert_eq!(values, [1, 2]);
        assert!(started.elapsed() < Duration::from_secs(1));
        dropped_receiver
            .await
            .expect("unfinished peer future was dropped with the batch");
    }

    #[test]
    fn persisted_route_costs_are_bounded_and_checksummed() {
        let mut state = RouteCostState::default();
        state.rows.insert(
            ([1; 32], [2; 32]),
            RouteCostSample {
                peer: [1; 32],
                recipe: [2; 32],
                transfer_bytes_per_second: ROUTE_COST_PRIOR_TRANSFER_BYTES_PER_SECOND,
                remote_millis_per_mib: 20_000,
                observed_at: 100,
                sample_count: 1,
                last_exploration_at: 50,
            },
        );
        let encoded = encode_route_cost_state(&state).expect("encode checked cost model");
        let decoded = decode_route_cost_state(&encoded).expect("decode checked cost model");
        assert_eq!(decoded.rows, state.rows);

        let mut corrupted = encoded;
        corrupted[12] ^= 1;
        assert!(decode_route_cost_state(&corrupted).is_err());
    }

    #[test]
    fn a_cold_peer_gets_only_one_background_calibration_window() {
        assert!(route_exploration_due(None, 10_000));
        let cold = RouteCostSample {
            peer: [1; 32],
            recipe: [2; 32],
            transfer_bytes_per_second: ROUTE_COST_PRIOR_TRANSFER_BYTES_PER_SECOND,
            remote_millis_per_mib: 60_000,
            observed_at: 10_000,
            sample_count: 0,
            last_exploration_at: 10_000,
        };
        assert!(!route_exploration_due(Some(cold), 10_001));
        assert!(route_exploration_due(
            Some(cold),
            10_000 + ROUTE_COST_EXPLORATION_INTERVAL_MS
        ));
        assert!(!route_exploration_due(
            Some(RouteCostSample {
                sample_count: 1,
                ..cold
            }),
            10_001
        ));
        assert!(route_exploration_due(
            Some(RouteCostSample {
                sample_count: 3,
                observed_at: 10_000,
                ..cold
            }),
            10_000 + ROUTE_COST_SAMPLE_MAX_AGE_MS + ROUTE_COST_EXPLORATION_INTERVAL_MS
        ));
    }

    #[test]
    fn local_cost_model_marks_prior_and_measured_provenance() {
        let host = [9; 32];
        let recipe = [2; 32];
        let model = OwnerRemoteCompilerCostModel {
            path: PathBuf::new(),
            state: Mutex::new(RouteCostState::default()),
            local_path: PathBuf::new(),
            local_state: Mutex::new(LocalCostState::default()),
            local_host: host,
        };
        let cold = model.estimate_local(recipe, ROUTE_COST_MIB, 2, 10_000);
        assert_eq!(cold.provenance(), LocalCostProvenance::ConservativePrior);
        assert_eq!(cold.confidence_per_mille(), 250);
        assert!(cold.completion().execution >= LOCAL_COST_PRIOR_MILLIS_PER_MIB);

        assert!(interactive_local_cost_evidence_required(
            CompilerDemand::Interactive,
            LocalCompilerAvailability::Ready,
            cold,
        ));
        assert!(!cold_exploration_is_allowed(
            CompilerDemand::Interactive,
            false,
            true,
        ));
        let observation = LocalCompilerCostObservation::new(recipe, ROUTE_COST_MIB, 2, 8_000)
            .expect("bounded exact local observation");
        let mut state = model.local_state.lock().expect("local sample lock");
        record_local_cost_observation(&mut state, host, observation, 10_000)
            .expect("record cost-only local observation");
        drop(state);
        let measured = model.estimate_local(recipe, ROUTE_COST_MIB, 2, 10_001);
        assert_eq!(
            measured.provenance(),
            LocalCostProvenance::Measured {
                sample_count: 1,
                observed_at: 10_000,
            }
        );
        assert_eq!(measured.completion().execution, 8_000);
        assert!(!interactive_local_cost_evidence_required(
            CompilerDemand::Interactive,
            LocalCompilerAvailability::Ready,
            measured,
        ));
        assert!(cold_exploration_is_allowed(
            CompilerDemand::Interactive,
            true,
            true,
        ));
        assert!(!cold_exploration_is_allowed(
            CompilerDemand::Interactive,
            true,
            false,
        ));
        assert!(!interactive_local_first_window(
            true, true, 10_000, 10_001, 0, 0
        ));
        assert!(interactive_cold_exploration_fits_latency_budget(20, 30));
        assert!(!interactive_cold_exploration_fits_latency_budget(20, 31));
        assert!(!interactive_cold_exploration_fits_latency_budget(20, 6_300));
        assert!(interactive_cold_exploration_fits_latency_budget(
            1_000, 1_250
        ));
        assert!(!interactive_cold_exploration_fits_latency_budget(
            1_000, 1_251
        ));
        assert!(!interactive_cold_exploration_fits_latency_budget(0, 1));
        assert_eq!(
            remote_route_provenance(CompilerDemand::Interactive, true),
            CompilerRemoteRouteProvenance::BoundedInteractiveExploration,
        );
        assert!(route_cost_meets_deadline(10_000, 3_000, Some(13_000)));
        assert!(!route_cost_meets_deadline(10_000, 3_001, Some(13_000)));
        assert_eq!(
            model
                .estimate_local(recipe, ROUTE_COST_MIB + 1, 2, 10_001)
                .provenance(),
            LocalCostProvenance::ConservativePrior,
            "observations apply only to the exact input-size/source-count bucket"
        );
        assert_eq!(
            model
                .estimate_local([3; 32], ROUTE_COST_MIB, 2, 10_001)
                .provenance(),
            LocalCostProvenance::ConservativePrior,
            "a different recipe cannot borrow the measured sample"
        );

        let stale = model.estimate_local(
            recipe,
            ROUTE_COST_MIB,
            2,
            10_000 + LOCAL_COST_SAMPLE_MAX_AGE_MS + 1,
        );
        assert_eq!(stale.provenance(), LocalCostProvenance::ConservativePrior);
    }

    #[test]
    fn persisted_local_costs_are_bounded_and_checksummed() {
        let mut state = LocalCostState::default();
        state.rows.insert(
            ([1; 32], [2; 32], ROUTE_COST_MIB, 3),
            LocalCostSample {
                host: [1; 32],
                recipe: [2; 32],
                input_payload_bytes: ROUTE_COST_MIB,
                source_artifact_count: 3,
                elapsed_ms: 20_000,
                observed_at: 100,
                sample_count: 1,
            },
        );
        let encoded = encode_local_cost_state(&state).expect("encode local cost model");
        assert_eq!(
            decode_local_cost_state(&encoded)
                .expect("decode local cost model")
                .rows,
            state.rows
        );
        let mut corrupted = encoded;
        corrupted[12] ^= 1;
        assert!(decode_local_cost_state(&corrupted).is_err());
    }

    #[test]
    fn interactive_local_first_window_precedes_remote_cost() {
        assert!(interactive_local_first_window(true, true, 100, 120, 50, 25));
        assert!(!interactive_local_first_window(
            false, true, 100, 120, 50, 25
        ));
        assert!(!interactive_local_first_window(
            true, false, 100, 120, 50, 25
        ));
        assert!(!interactive_local_first_window(
            true, true, 100, 160, 50, 25
        ));
    }

    #[test]
    fn completed_recovered_result_survives_worker_capacity_shrink() {
        let original_demand = CompilerResourceCredits {
            cpu: CompilerCpuCredits::new(1_000),
            memory: CompilerMemoryCredits::new(1024 * 1024 * 1024),
            transfer: CompilerByteCredits::new(4 * 1024 * 1024),
        };
        let smaller_current_capacity = ProbeResourceCredits {
            cpu_millicores: 500,
            memory_bytes: 512 * 1024 * 1024,
            transfer_bytes: 2 * 1024 * 1024,
        };

        assert!(recovered_capacity_allows(
            original_demand,
            smaller_current_capacity,
            true,
        ));
        assert!(!recovered_capacity_allows(
            original_demand,
            smaller_current_capacity,
            false,
        ));
    }

    #[test]
    fn result_rejections_are_limited_to_conclusive_admission_failures() {
        assert!(is_conclusive_result_rejection(
            &ClusterDispatchError::ConclusiveResultRejection("typed mismatch".into())
        ));
        let metadata_mismatch = map_recovered_result_bridge_error(
            backend_engine::compiler_cluster_transport::CompilerTransportBridgeError::
                ResultMetadataMismatch,
        );
        assert!(is_conclusive_result_rejection(&metadata_mismatch));
        let contradictory_terminal = post_receipt_terminal_error(
            true,
            "worker contradicted its authenticated pending receipt",
        );
        assert!(is_conclusive_result_rejection(&contradictory_terminal));
        let unauthenticated_contradiction =
            post_receipt_terminal_error(false, "wrong peer or assignment scope");
        assert!(matches!(
            &unauthenticated_contradiction,
            ClusterDispatchError::AssignmentRejected
        ));
        assert!(!is_conclusive_result_rejection(
            &unauthenticated_contradiction
        ));
        let allocation = map_recovered_candidate_error(
            backend_engine::application::CompilerResultError::Allocation,
        );
        assert!(matches!(
            &allocation,
            ClusterDispatchError::ResultRejected(_)
        ));
        assert!(!is_conclusive_result_rejection(&allocation));
        let input_store_io =
            map_recovered_candidate_error(backend_engine::application::CompilerResultError::Store(
                backend_store::StoreError::Io("temporary CAS read failure".into()),
            ));
        assert!(matches!(&input_store_io, ClusterDispatchError::Store(_)));
        assert!(!is_conclusive_result_rejection(&input_store_io));
        let input_revalidation = map_recovered_candidate_error(
            backend_engine::application::CompilerResultError::InputClosureMismatch,
        );
        assert!(is_conclusive_result_rejection(&input_revalidation));
        let local_io = classify_received_result_transfer_error(ClusterDispatchError::Store(
            "temporary CAS write failure".into(),
        ));
        assert!(matches!(&local_io, ClusterDispatchError::Store(_)));
        assert!(!is_conclusive_result_rejection(&local_io));
        let local_inventory_fault =
            classify_received_result_transfer_error(ClusterDispatchError::Store(
                "CAS have bitmap does not match the owner-validated result inventory".into(),
            ));
        assert!(matches!(
            &local_inventory_fault,
            ClusterDispatchError::Store(_)
        ));
        assert!(!is_conclusive_result_rejection(&local_inventory_fault));
        let malformed_inventory = classify_received_result_transfer_error(
            ClusterDispatchError::ResultRejected("signed range inventory has a gap".into()),
        );
        assert!(is_conclusive_result_rejection(&malformed_inventory));
        for transient in [
            ClusterDispatchError::DeadlineExpired,
            ClusterDispatchError::Transport("stream reset".into()),
            ClusterDispatchError::Store("local fsync failed".into()),
            allocation,
            input_store_io,
            ClusterDispatchError::AssignmentRejected,
            ClusterDispatchError::NoEligibleWorker,
            ClusterDispatchError::ResultRejected("grant page unavailable".into()),
        ] {
            assert!(
                !is_conclusive_result_rejection(&transient),
                "transient result failure must preserve the worker result: {transient}"
            );
        }
    }

    #[test]
    fn unexpected_post_receipt_control_is_terminal_only_on_exact_route() {
        let exact_route =
            post_receipt_terminal_error(true, "worker sent Accept instead of a result grant page");
        assert!(is_conclusive_result_rejection(&exact_route));

        let wrong_route =
            post_receipt_terminal_error(false, "control message came from another peer or scope");
        assert!(matches!(
            &wrong_route,
            ClusterDispatchError::AssignmentRejected
        ));
        assert!(!is_conclusive_result_rejection(&wrong_route));
    }
}

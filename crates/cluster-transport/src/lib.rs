//! Private, capability-scoped Iroh transport for immutable index artifacts.
//!
//! Endpoints created by [`bind_direct`] use Iroh's TLS identity and QUIC encryption,
//! while disabling relays and address lookup. Callers must provide the exact direct
//! address and maintain an explicit peer allowlist. Bulk bytes are raw payload blobs,
//! identified by [`BlobHash`]; a signed [`StoreObjectMapping`] binds each blob to the
//! backend-store object ABI. A receiving [`ArtifactSink`](backend_store::ArtifactSink)
//! must still admit the typed object and exact store closure before CAS publication.
//! `TransferScope::namespace_id` is opaque and must come from Turso's
//! `AuthorityNamespace::namespace_id()`; this crate does not reproduce that hash grammar.

#[cfg(test)]
use std::collections::BTreeMap;
use std::{
    collections::{HashMap, HashSet},
    fmt,
    fs::File,
    future::Future,
    io::{self, Read, Seek, SeekFrom, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use backend_store::{
    ArtifactClosureClaim, ArtifactObjectClaim, ArtifactObjectReader, ArtifactSession, ArtifactSink,
    ClosureId, TypedObject, UntrustedObjectId, VerifiedClosureMember,
};
use backend_version::SchemaIdentity;
use bao_tree::{
    BaoTree, BlockSize, ChunkNum, ChunkRanges, TreeNode,
    io::fsm::{CreateOutboard, Outboard, decode_ranges, encode_ranges_validated},
};
use bytes::Bytes;
pub use iroh::{Endpoint, EndpointAddr, EndpointId, SecretKey};
use iroh::{
    RelayMode, Signature,
    endpoint::{Connection, RecvStream, SendStream, presets},
};
use iroh_io::{
    AsyncSliceReader, AsyncSliceWriter, AsyncStreamWriter, TokioStreamReader, TokioStreamWriter,
};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

/// ALPN reserved for artifact range transfer.
pub const ALPN: &[u8] = b"/backend/index-artifacts/1";
/// ALPN reserved for the current bounded compile-assignment control contract.
pub const CONTROL_ALPN: &[u8] = b"/backend/cluster-control/5";
/// Bao uses 16 KiB blocks (16 BLAKE3 chunks) in this protocol version.
pub const BAO_BLOCK_SIZE: BlockSize = BlockSize::from_chunk_log(4);
/// Largest object this bounded transport slice admits.
pub const MAX_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
/// Maximum number of 1 KiB BLAKE3 chunks in one range request (1 MiB of application data).
pub const MAX_RANGE_CHUNKS: u64 = 1_024;
/// Maximum signed response budget per capability.
pub const MAX_RESPONSE_BYTES: u32 = 2 * 1024 * 1024;
const MAX_CONTROL_FRAME: usize = 16 * 1024;
const MAX_CHECKPOINT_BYTES: u64 = 1024 * 1024;
const IO_CHUNK_BYTES: usize = 64 * 1024;
const RANGE_ACK_TIMEOUT: Duration = Duration::from_secs(30);
const CLUSTER_LISTENER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CLUSTER_ACCEPT_QUEUE: usize = 64;
const MAX_CLUSTER_ADMISSION_IN_FLIGHT: usize = 8;
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

type ClusterAdmissionTask = (
    mpsc::OwnedPermit<Result<AcceptedClusterConnection, TransportError>>,
    Result<AcceptedClusterConnection, TransportError>,
);

fn publish_cluster_admission(joined: Option<Result<ClusterAdmissionTask, tokio::task::JoinError>>) {
    if let Some(Ok((permit, event))) = joined {
        permit.send(event);
    }
}

mod control;
pub use control::{
    CancelReason, ControlAccept, ControlAdmissionPolicy, ControlCancel, ControlChannel,
    ControlExecutionFailed, ControlGrantPage, ControlMessage, ControlNoResultRetireThrough,
    ControlNoResultRetirementApplied, ControlOffer, ControlRejectCode, ControlResultAck,
    ControlResultReceipt, ControlResultRecoveryCapacity, ControlResultRecoveryQuery,
    ControlResultRecoveryState, ControlResultRecoveryStatus, ControlResultRetired,
    ControlResultRetirementApplied, ControlResultRetirementConfirm, ControlRole,
    ExecutionFailureReason, GrantDirection, MAX_CONTROL_GRANT_PAGES,
    MAX_CONTROL_MESSAGES_PER_STREAM, MAX_CONTROL_OFFER_LIFETIME_MS, MAX_OFFER_CAPABILITIES,
    NO_RESULT_RETIREMENT_SCHEMA_VERSION, RESULT_RECOVERY_SCHEMA_VERSION,
    RESULT_RETIREMENT_SCHEMA_VERSION, ResultAckDisposition, ResultRejectReason, WorkerReject,
    WorkerRejectReason, accept_control, connect_control,
};
mod invite;
pub use invite::{ClusterExecutionClass, ClusterInviteError, ScopedClusterInvite};
mod probe;
pub use probe::{
    CompilerProbeDemand, CompilerProbeIdentity, CompilerProbePage, CompilerProbePageReply,
    CompilerProbeSession, MAX_PROBE_CAPACITY_LEASE_MS, MAX_PROBE_FRAME_BYTES,
    MAX_PROBE_LIFETIME_MS, MAX_PROBE_OBJECTS_PER_PAGE, MAX_PROBE_PAGES,
    MAX_PROBE_TARGET_DESCRIPTOR_BYTES, PROBE_ALPN, ProbeAdmissionPolicy, ProbeCapability,
    ProbeCapabilityReject, ProbeChannel, ProbeInventoryDescriptor, ProbeInventoryHasher,
    ProbeObjectClaim, ProbeResourceCredits, ProbeRole, ProbeWorkerSnapshot,
    accept_probe_connection, connect_probe, probe_inventory_descriptor,
};

/// Raw BLAKE3 digest of immutable payload bytes used by Bao.
///
/// This is deliberately distinct from backend-store's domain-separated typed `ObjectId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct BlobHash(pub [u8; 32]);

/// Wire representation of backend-store's typed object identity tuple.
///
/// These fields are signed claims, not admitted store values. The receiver converts them to
/// an `ArtifactObjectClaim`; backend-store recomputes and checks the `object_id` from the
/// payload before a write enters CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoreObjectMapping {
    /// Stable schema domain byte.
    pub schema_domain: u8,
    /// Stable schema type tag.
    pub schema_type: u16,
    /// Canonical encoding version.
    pub schema_version: u8,
    /// Typed logical key digest.
    pub key: [u8; 32],
    /// Complete typed object version digest.
    pub version: [u8; 32],
    /// Exact canonical payload byte length.
    pub payload_length: u64,
    /// Untrusted bytes of backend-store's domain-separated `ObjectId`.
    pub object_id: [u8; 32],
}

impl StoreObjectMapping {
    /// Extract the exact checked mapping from an admitted backend-store object.
    pub fn from_typed_object(object: &TypedObject) -> Self {
        Self {
            schema_domain: object.schema().domain(),
            schema_type: object.schema().ty(),
            schema_version: object.schema().version(),
            key: *object.key(),
            version: *object.version(),
            payload_length: object.bytes().len() as u64,
            object_id: *object.id().as_bytes(),
        }
    }

    /// Copy the checked typed header facts from an ArtifactSink-verified reader.
    pub fn from_artifact_reader(reader: &ArtifactObjectReader) -> Self {
        let schema = reader.schema();
        Self {
            schema_domain: schema.domain(),
            schema_type: schema.ty(),
            schema_version: schema.version(),
            key: *reader.key(),
            version: *reader.version(),
            payload_length: reader.payload_len(),
            object_id: *reader.id().as_bytes(),
        }
    }

    fn from_verified_closure_member(member: &VerifiedClosureMember) -> Self {
        let object = member.object();
        let schema = object.schema();
        Self {
            schema_domain: schema.domain(),
            schema_type: schema.ty(),
            schema_version: schema.version(),
            key: *object.key(),
            version: *object.version(),
            payload_length: object.payload_len(),
            object_id: *object.id().as_bytes(),
        }
    }

    /// Convert the signed mapping into the untrusted input accepted by ArtifactSink.
    ///
    /// The returned value still requires `ArtifactSession::put` and `finish` to verify the
    /// bytes, typed identity, closure, and durable CAS links.
    #[must_use]
    pub fn artifact_claim(self) -> ArtifactObjectClaim {
        ArtifactObjectClaim::new(
            SchemaIdentity::new(self.schema_domain, self.schema_type, self.schema_version),
            self.key,
            self.version,
            self.payload_length,
        )
        .with_object_id(UntrustedObjectId::from_bytes(self.object_id))
    }
}

/// Exact assignment scope minted by the package-index authority.
///
/// `namespace_id` must come from Turso's `AuthorityNamespace::namespace_id()` and `attempt`
/// plus `fence` from the same current assignment. A result closure does not exist until after
/// the Offer, so this control-plane type deliberately has no closure field.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct AssignmentScope {
    /// Opaque ID from Turso `AuthorityNamespace::namespace_id()`.
    pub namespace_id: [u8; 16],
    /// Stable work identity.
    pub work_id: [u8; 16],
    /// Scheduler attempt ordinal for diagnostics.
    pub attempt: u64,
    /// Exact nonzero 32-byte attempt/publication fence minted by the scheduler database.
    pub fence: [u8; 32],
}

impl AssignmentScope {
    /// Construct the control-plane scope from trusted Turso assignment bytes.
    pub fn new(
        namespace_id: [u8; 16],
        work_id: [u8; 16],
        attempt: u64,
        fence: [u8; 32],
    ) -> Result<Self, TransportError> {
        let scope = Self {
            namespace_id,
            work_id,
            attempt,
            fence,
        };
        scope.validate()?;
        Ok(scope)
    }

    /// Check that the scope contains a nonzero canonical namespace, work ID, attempt, and fence.
    pub fn validate(self) -> Result<(), TransportError> {
        if self.namespace_id == [0; 16]
            || self.work_id == [0; 16]
            || self.attempt == 0
            || self.fence == [0; 32]
        {
            return Err(TransportError::InvalidScope);
        }
        Ok(())
    }

    /// Opaque identifier returned by Turso's canonical authority namespace.
    #[must_use]
    pub const fn namespace_id(self) -> [u8; 16] {
        self.namespace_id
    }

    /// Stable work identity inside the authority namespace.
    #[must_use]
    pub const fn work_id(self) -> [u8; 16] {
        self.work_id
    }

    /// Persisted scheduler attempt ordinal.
    #[must_use]
    pub const fn attempt(self) -> u64 {
        self.attempt
    }

    /// Exact DB-minted attempt fence.
    #[must_use]
    pub const fn fence(self) -> [u8; 32] {
        self.fence
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignmentScopeWire {
    namespace_id: [u8; 16],
    work_id: [u8; 16],
    attempt: u64,
    fence: [u8; 32],
}

impl<'de> Deserialize<'de> for AssignmentScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = AssignmentScopeWire::deserialize(deserializer)?;
        Self::new(wire.namespace_id, wire.work_id, wire.attempt, wire.fence)
            .map_err(serde::de::Error::custom)
    }
}

/// Exact object-transfer scope: authority assignment plus typed stored closure identity.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct TransferScope {
    /// Opaque ID from Turso `AuthorityNamespace::namespace_id()`.
    pub namespace_id: [u8; 16],
    /// Stable work identity.
    pub work_id: [u8; 16],
    /// Scheduler attempt ordinal for diagnostics.
    pub attempt: u64,
    /// Exact nonzero 32-byte attempt/publication fence minted by the scheduler database.
    pub fence: [u8; 32],
    /// Exact backend-store `ClosureId` bytes for this immutable object transfer.
    pub closure_id: [u8; 32],
}

impl TransferScope {
    /// Construct a transfer scope from one exact assignment and typed stored closure ID.
    pub fn from_store_closure(
        assignment: AssignmentScope,
        closure: ClosureId,
    ) -> Result<Self, TransportError> {
        Self::new(
            assignment.namespace_id,
            assignment.work_id,
            assignment.attempt,
            assignment.fence,
            *closure.as_bytes(),
        )
    }

    /// Construct a transfer scope from trusted authority bytes and a closure ID.
    pub fn new(
        namespace_id: [u8; 16],
        work_id: [u8; 16],
        attempt: u64,
        fence: [u8; 32],
        closure_id: [u8; 32],
    ) -> Result<Self, TransportError> {
        let assignment = AssignmentScope::new(namespace_id, work_id, attempt, fence)?;
        if closure_id == [0; 32] {
            return Err(TransportError::InvalidScope);
        }
        Ok(Self {
            namespace_id: assignment.namespace_id,
            work_id: assignment.work_id,
            attempt: assignment.attempt,
            fence: assignment.fence,
            closure_id,
        })
    }

    /// Check that this scope contains a valid assignment and nonzero stored closure ID.
    pub fn validate(self) -> Result<(), TransportError> {
        self.assignment().validate()?;
        if self.closure_id == [0; 32] {
            return Err(TransportError::InvalidScope);
        }
        Ok(())
    }

    /// Return the exact DB-minted control-plane assignment scope.
    #[must_use]
    pub const fn assignment(self) -> AssignmentScope {
        AssignmentScope {
            namespace_id: self.namespace_id,
            work_id: self.work_id,
            attempt: self.attempt,
            fence: self.fence,
        }
    }

    /// Exact backend-store ClosureId attached to this immutable transfer.
    #[must_use]
    pub const fn closure_id(self) -> [u8; 32] {
        self.closure_id
    }

    /// Opaque identifier returned by Turso's canonical authority namespace.
    #[must_use]
    pub const fn namespace_id(self) -> [u8; 16] {
        self.namespace_id
    }

    /// Stable work identity inside the authority namespace.
    #[must_use]
    pub const fn work_id(self) -> [u8; 16] {
        self.work_id
    }

    /// Persisted scheduler attempt ordinal.
    #[must_use]
    pub const fn attempt(self) -> u64 {
        self.attempt
    }

    /// Exact DB-minted attempt fence.
    #[must_use]
    pub const fn fence(self) -> [u8; 32] {
        self.fence
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TransferScopeWire {
    namespace_id: [u8; 16],
    work_id: [u8; 16],
    attempt: u64,
    fence: [u8; 32],
    closure_id: [u8; 32],
}

impl<'de> Deserialize<'de> for TransferScope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TransferScopeWire::deserialize(deserializer)?;
        Self::new(
            wire.namespace_id,
            wire.work_id,
            wire.attempt,
            wire.fence,
            wire.closure_id,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// A half-open range of 1 KiB BLAKE3 chunks used by Bao range queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkRange {
    /// First included BLAKE3 chunk.
    pub start: u64,
    /// First excluded BLAKE3 chunk.
    pub end: u64,
}

impl ChunkRange {
    /// Validate bounds and the protocol's per-request range limit.
    pub fn validate(self, object_size: u64) -> Result<(), RejectCode> {
        if object_size == 0 && self == (Self { start: 0, end: 0 }) {
            return Ok(());
        }
        let total = ChunkNum::chunks(object_size).0;
        if self.start >= self.end || self.end > total || self.end - self.start > MAX_RANGE_CHUNKS {
            return Err(RejectCode::InvalidRange);
        }
        Ok(())
    }
}

/// Signed authority for one peer, work attempt, typed object, blob, and Bao range.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityClaims {
    /// Endpoint identity allowed to request this range.
    pub client: EndpointId,
    /// Endpoint identity allowed to serve this range.
    pub server: EndpointId,
    /// Exact scheduled work/fence/closure scope.
    pub scope: TransferScope,
    /// Raw BLAKE3 digest of the canonical typed payload bytes.
    pub blob_hash: BlobHash,
    /// Mapping from raw transfer blob to backend-store's typed object ABI.
    pub object: StoreObjectMapping,
    /// The one contiguous range granted by this token.
    pub range: ChunkRange,
    /// Upper bound on Bao bytes served for this request.
    pub byte_budget: u32,
    /// Unix time in milliseconds after which the grant expires.
    pub expires_at_unix_ms: u64,
    /// Unique grant identifier for audit and checkpoint binding.
    pub nonce: [u8; 16],
}

/// Authority-signed transfer capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capability {
    /// Signed claims.
    pub claims: CapabilityClaims,
    /// Ed25519 signature over the canonical postcard encoding of `claims`.
    pub signature: Signature,
}

/// Signs tightly scoped range capabilities using the scheduler/controller identity.
#[derive(Debug, Clone)]
pub struct CapabilityIssuer(SecretKey);

impl CapabilityIssuer {
    /// Construct an issuer from its private Iroh identity key.
    pub fn new(key: SecretKey) -> Self {
        Self(key)
    }

    /// Public key trusted by workers and artifact servers.
    pub fn public_key(&self) -> EndpointId {
        self.0.public()
    }

    /// Sign a range capability after checking its shape and limits.
    pub fn issue(&self, claims: CapabilityClaims) -> Result<Capability, TransportError> {
        validate_claim_shape(&claims).map_err(TransportError::Rejected)?;
        let message = postcard::to_allocvec(&claims).map_err(frame_error)?;
        Ok(Capability {
            claims,
            signature: self.0.sign(&message),
        })
    }
}

/// Trusted server admission rules for one active work scope.
#[derive(Debug, Clone)]
pub struct AdmissionPolicy {
    /// This endpoint's expected TLS identity.
    pub server: EndpointId,
    /// Trusted scheduler/controller signing key.
    pub issuer: EndpointId,
    /// Explicit endpoint identities allowed to connect.
    pub allowed_peers: HashSet<EndpointId>,
    /// The one exact work/attempt/fence/closure accepted by this listener.
    pub scope: TransferScope,
    owner_routes: Option<OwnerClusterAdmissionRegistry>,
}

impl AdmissionPolicy {
    /// Create an exact-scope admission policy.
    pub fn new(
        server: EndpointId,
        issuer: EndpointId,
        allowed_peers: impl IntoIterator<Item = EndpointId>,
        scope: TransferScope,
    ) -> Self {
        Self {
            server,
            issuer,
            allowed_peers: allowed_peers.into_iter().collect(),
            scope,
            owner_routes: None,
        }
    }

    fn owner_router(
        server: EndpointId,
        issuer: EndpointId,
        routes: OwnerClusterAdmissionRegistry,
    ) -> Self {
        Self {
            server,
            issuer,
            allowed_peers: HashSet::new(),
            // This placeholder is never accepted by the router policy. Every request must match
            // a currently registered peer and exact V2 input closure in `owner_routes`.
            scope: TransferScope {
                namespace_id: [0; 16],
                work_id: [0; 16],
                attempt: 0,
                fence: [0; 32],
                closure_id: [0; 32],
            },
            owner_routes: Some(routes),
        }
    }

    fn allows_artifact(&self, peer: EndpointId, scope: TransferScope) -> bool {
        if let Some(routes) = &self.owner_routes {
            routes.authorizes_artifact(peer, scope)
        } else {
            self.allowed_peers.contains(&peer) && scope == self.scope
        }
    }

    fn allows_peer(&self, peer: EndpointId) -> bool {
        self.owner_routes.as_ref().map_or_else(
            || self.allowed_peers.contains(&peer),
            |routes| routes.allows_peer(peer),
        )
    }
}

/// Live exact-scope allowlist for one owner endpoint accepting concurrent worker streams.
///
/// Registration is tied to a trusted assignment route and its input closure. The listener may
/// accept authenticated worker connections concurrently, but artifact admission still requires
/// the exact registered `(peer, assignment, closure)` capability before opening local CAS.
#[derive(Clone, Debug, Default)]
pub struct OwnerClusterAdmissionRegistry {
    routes: Arc<RwLock<HashMap<AssignmentScope, (EndpointId, TransferScope)>>>,
}

impl OwnerClusterAdmissionRegistry {
    /// Register one exact owner assignment and its V2 input transfer closure.
    pub fn register(&self, peer: EndpointId, scope: TransferScope) -> Result<(), TransportError> {
        scope.validate()?;
        let assignment = scope.assignment();
        let mut routes = self
            .routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if routes.contains_key(&assignment) {
            return Err(TransportError::Frame(
                "owner assignment route is already registered".into(),
            ));
        }
        routes.insert(assignment, (peer, scope));
        Ok(())
    }

    /// Remove only the exact peer and input closure route originally registered.
    pub fn unregister(&self, peer: EndpointId, scope: TransferScope) {
        let mut routes = self
            .routes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if routes.get(&scope.assignment()) == Some(&(peer, scope)) {
            routes.remove(&scope.assignment());
        }
    }

    /// Whether a peer has any currently registered owner assignment.
    #[must_use]
    pub fn allows_peer(&self, peer: EndpointId) -> bool {
        self.routes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|(allowed_peer, _)| *allowed_peer == peer)
    }

    /// Whether the peer is registered for this exact assignment control stream.
    #[must_use]
    pub fn authorizes_control(&self, peer: EndpointId, scope: AssignmentScope) -> bool {
        if scope.validate().is_err() {
            return false;
        }
        self.routes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&scope)
            .is_some_and(|(allowed_peer, _)| *allowed_peer == peer)
    }

    /// Whether the peer is registered for this exact assignment and V2 input closure.
    #[must_use]
    pub fn authorizes_artifact(&self, peer: EndpointId, scope: TransferScope) -> bool {
        if scope.validate().is_err() {
            return false;
        }
        self.routes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&scope.assignment())
            .is_some_and(|(allowed_peer, allowed_scope)| {
                *allowed_peer == peer && *allowed_scope == scope
            })
    }
}

/// Durable or fixture-provided random-access source.
pub trait RandomAccessSource: fmt::Debug + Send + Sync + 'static {
    /// Exact source size.
    fn len(&self) -> u64;
    /// Read at most `len` bytes from `offset`.
    fn read_at(&self, offset: u64, len: usize) -> io::Result<Bytes>;
}

/// Opened verified-payload bytes and their durable Bao outboard bytes.
#[derive(Debug, Clone)]
pub struct BlobObject {
    /// Raw payload BLAKE3 identity.
    pub blob_hash: BlobHash,
    /// Random-access immutable payload source.
    pub data: SourceReader,
    /// Random-access preorder Bao outboard source.
    pub outboard: SourceReader,
}

/// Object catalog used by the server after request authorization.
pub trait BlobCatalog: fmt::Debug + Send + Sync + 'static {
    /// Open a blob only after a signed capability has been admitted.
    fn open(&self, claims: &CapabilityClaims) -> Result<Option<BlobObject>, TransportError>;
}

/// In-memory fixture for transport tests. Production callers should use a durable
/// `BlobCatalog` implementation such as [`DirectoryBlobCatalog`] or a checked CAS adapter.
#[cfg(test)]
#[derive(Debug, Default, Clone)]
pub struct MemoryBlobCatalog {
    objects: BTreeMap<BlobHash, BlobObject>,
}

#[cfg(test)]
impl MemoryBlobCatalog {
    /// Insert payload bytes and construct a Bao outboard for the test fixture.
    pub fn insert_payload(&mut self, bytes: impl Into<Bytes>) -> Result<BlobHash, TransportError> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() as u64 > MAX_OBJECT_BYTES {
            return Err(TransportError::ObjectTooLarge);
        }
        let blob_hash = BlobHash(*blake3::hash(&bytes).as_bytes());
        let outboard = bao_tree::io::outboard::PreOrderMemOutboard::create(&bytes, BAO_BLOCK_SIZE);
        let blob = BlobObject {
            blob_hash,
            data: SourceReader(Arc::new(MemoryRangeSource(bytes))),
            outboard: SourceReader(Arc::new(MemoryRangeSource(Bytes::from(outboard.data)))),
        };
        self.objects.insert(blob_hash, blob);
        Ok(blob_hash)
    }
}

#[cfg(test)]
impl BlobCatalog for MemoryBlobCatalog {
    fn open(&self, claims: &CapabilityClaims) -> Result<Option<BlobObject>, TransportError> {
        Ok(self.objects.get(&claims.blob_hash).cloned())
    }
}

/// Durable directory catalog keyed by raw BLAKE3 payload hash.
///
/// Each blob is stored as `<hex>.blob`; its preorder Bao outboard is `<hex>.bao`. The
/// capability supplies the typed ObjectId mapping and exact closure fence; the destination
/// still has to pass the completed payload through backend-store's ArtifactSink.
#[derive(Debug, Clone)]
pub struct DirectoryBlobCatalog {
    root: PathBuf,
}

impl DirectoryBlobCatalog {
    /// Open a durable payload/outboard directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Return the canonical payload path for an untrusted blob hash.
    #[must_use]
    pub fn blob_path(&self, blob_hash: BlobHash) -> PathBuf {
        self.root.join(format!("{}.blob", hex(&blob_hash.0)))
    }

    /// Return the canonical Bao outboard path for an untrusted blob hash.
    #[must_use]
    pub fn outboard_path(&self, blob_hash: BlobHash) -> PathBuf {
        self.root.join(format!("{}.bao", hex(&blob_hash.0)))
    }

    /// Build and durably save the Bao outboard for an already staged payload file.
    /// The complete payload is hashed incrementally and must match `expected_hash`.
    pub async fn build_outboard(
        &self,
        expected_hash: BlobHash,
        payload_path: impl AsRef<Path>,
    ) -> Result<(), TransportError> {
        let payload_path = payload_path.as_ref();
        let mut data_source = FileRangeSource::open_readonly(payload_path)?;
        let data_len = data_source.len();
        if data_len > MAX_OBJECT_BYTES {
            return Err(TransportError::ObjectTooLarge);
        }
        if hash_source(&mut data_source, data_len).await? != expected_hash {
            return Err(TransportError::BlobHashMismatch);
        }
        safe_ensure_directory(&self.root)?;
        let tree = BaoTree::new(data_len, BAO_BLOCK_SIZE);
        let outboard_len = tree.outboard_size();
        let outboard_path = self.outboard_path(expected_hash);
        safe_remove_regular(&outboard_path)?;
        let outboard_file = FileRangeSource::create_sparse(&outboard_path, outboard_len)?;
        let mut outboard = bao_tree::io::outboard::PreOrderOutboard {
            root: blake3::hash(&[]),
            tree,
            data: outboard_file,
        };
        let data = tokio::fs::File::from_std(safe_open_read(payload_path)?);
        CreateOutboard::init_from(&mut outboard, TokioStreamReader::new(data)).await?;
        outboard.data.sync().await?;
        if outboard.root != blake3::Hash::from(expected_hash.0) {
            return Err(TransportError::BlobHashMismatch);
        }
        Ok(())
    }
}

/// Mapping produced when a checked store object is indexed for Bao transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaterializedObject {
    /// Raw BLAKE3 payload hash used by Bao.
    pub blob_hash: BlobHash,
    /// Typed backend-store mapping carried by signed capabilities.
    pub object: StoreObjectMapping,
}

/// Nonconstructible proof that one Bao mapping belongs to an exact durable closure.
///
/// The proof is minted by StoreBlobCatalog only after backend-store authenticates closure-index
/// membership and fully verifies the object envelope. It is required by grant issuers so a
/// caller cannot pair an arbitrary materialized object with a different closure ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifiedStoreClosureMember {
    member: VerifiedClosureMember,
    materialized: MaterializedObject,
}

impl VerifiedStoreClosureMember {
    /// Exact durable closure whose authenticated index contains this object.
    #[must_use]
    pub const fn closure(&self) -> ClosureId {
        self.member.closure()
    }

    /// Checked Bao payload hash and typed store mapping.
    #[must_use]
    pub const fn materialized(&self) -> MaterializedObject {
        self.materialized
    }

    /// Checked backend-store object ID.
    #[must_use]
    pub const fn object_id(&self) -> backend_store::ObjectId {
        self.member.object_id()
    }
}

/// CAS-backed blob catalog that stores only Bao outboards and a small typed-ID mapping.
///
/// Payload bytes remain in `ArtifactSink`'s existing durable object file. Both registration
/// and each later open use `ArtifactSink::open_object`, so the store ABI is re-verified before
/// a Bao root or source reader is trusted. Outboards are rebuilt through bounded range reads;
/// this catalog never writes a second `.blob` payload copy.
#[derive(Clone)]
pub struct StoreBlobCatalog {
    sink: ArtifactSink,
    root: PathBuf,
}

impl fmt::Debug for StoreBlobCatalog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoreBlobCatalog")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl StoreBlobCatalog {
    /// Create a catalog over an existing durable artifact sink and Bao metadata directory.
    pub fn new(sink: ArtifactSink, outboard_root: impl Into<PathBuf>) -> Self {
        Self {
            sink,
            root: outboard_root.into(),
        }
    }

    /// Stream a checked CAS payload once to build its Bao outboard and persist the typed map.
    ///
    /// The `.bao` file is atomically committed before the checksummed `.map` record. Payload
    /// bytes are read directly from `ArtifactObjectReader`; this operation does not stage a
    /// second copy of the object on disk or buffer a whole object in memory.
    pub async fn register_store_object(
        &self,
        object_id: UntrustedObjectId,
    ) -> Result<Option<MaterializedObject>, TransportError> {
        let sink = self.sink.clone();
        let reader =
            tokio::task::spawn_blocking(move || sink.open_object(object_id).map_err(store_error))
                .await
                .map_err(|error| TransportError::Iroh(error.to_string()))??;
        let Some(reader) = reader else {
            return Ok(None);
        };
        let mapping = StoreObjectMapping::from_artifact_reader(&reader);
        if mapping.payload_length > MAX_OBJECT_BYTES {
            return Err(TransportError::ObjectTooLarge);
        }

        // The CAS reader has just authenticated the typed object. Bao metadata is a
        // physical layout of those immutable bytes, so a complete cached layout can
        // be reused across probes and assignments without hashing the payload and
        // fsyncing two replacement files again. `BlobCatalog::open` still checks
        // the exact closure and object before serving any range.
        if let Some(cached) = self.cached_store_object(mapping)? {
            return Ok(Some(cached));
        }

        safe_ensure_directory(&self.root)?;
        let tree = BaoTree::new(mapping.payload_length, BAO_BLOCK_SIZE);
        let stage = unique_temp_path(&self.root, "store-bao");
        let outboard_file = FileRangeSource::create_sparse(&stage, tree.outboard_size())?;
        let source = ArtifactPayloadSource::new(reader);
        let mut outboard = bao_tree::io::outboard::PreOrderOutboard {
            root: blake3::hash(&[]),
            tree,
            data: outboard_file.clone(),
        };
        let created = outboard
            .init_from(ArtifactPayloadStream::new(source))
            .await
            .map_err(|error| TransportError::Bao(error.to_string()));
        if let Err(error) = created {
            let _ = safe_remove_regular(&stage);
            return Err(error);
        }
        outboard.data.sync().await?;
        outboard_file.sync_all()?;
        let blob_hash = BlobHash(*outboard.root.as_bytes());
        drop(outboard);
        drop(outboard_file);

        let outboard_digest = checked_outboard_digest(
            &FileRangeSource::open_readonly(&stage)?,
            tree.outboard_size(),
        )?
        .ok_or(TransportError::ObjectUnavailable)?;

        safe_replace(&stage, &self.outboard_path(blob_hash))?;
        safe_sync_directory(&self.root)?;
        self.write_mapping(mapping, blob_hash, outboard_digest)?;
        Ok(Some(MaterializedObject {
            blob_hash,
            object: mapping,
        }))
    }

    /// Verifies exact closure membership, then materializes the member's Bao metadata.
    ///
    /// A plain object ID is never enough to authorize a grant. The backend-store proof binds
    /// the checked envelope to the requested closure; its identity must also match the
    /// materialized Bao mapping before the opaque grant token is returned.
    pub async fn register_closure_member(
        &self,
        closure: ClosureId,
        object_id: UntrustedObjectId,
    ) -> Result<Option<VerifiedStoreClosureMember>, TransportError> {
        let sink = self.sink.clone();
        let closure_claim = ArtifactClosureClaim::from_id(closure);
        let member = tokio::task::spawn_blocking(move || {
            sink.verify_closure_member_claim(closure_claim, object_id)
                .map_err(store_error)
        })
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))??;
        let Some(member) = member else {
            return Ok(None);
        };
        let Some(materialized) = self.register_store_object(object_id).await? else {
            return Err(TransportError::ObjectUnavailable);
        };
        if member.closure() != closure
            || member.object_id().as_bytes() != &materialized.object.object_id
            || member.object().payload_len() != materialized.object.payload_length
        {
            return Err(TransportError::ObjectUnavailable);
        }
        Ok(Some(VerifiedStoreClosureMember {
            member,
            materialized,
        }))
    }

    /// Return the canonical durable outboard path for a raw Bao payload identity.
    #[must_use]
    pub fn outboard_path(&self, blob_hash: BlobHash) -> PathBuf {
        self.root.join(format!("{}.bao", hex(&blob_hash.0)))
    }

    fn mapping_path(&self, object_id: [u8; 32]) -> PathBuf {
        self.root.join(format!("{}.map", hex(&object_id)))
    }

    fn cached_store_object(
        &self,
        object: StoreObjectMapping,
    ) -> Result<Option<MaterializedObject>, TransportError> {
        let record = match self.read_mapping_record(object.object_id) {
            Ok(Some(record)) if record.object == object => record,
            Ok(_) | Err(TransportError::ObjectUnavailable) => return Ok(None),
            Err(error) => return Err(error),
        };
        let expected_size = BaoTree::new(object.payload_length, BAO_BLOCK_SIZE).outboard_size();
        let outboard = match FileRangeSource::open_readonly(self.outboard_path(record.blob_hash)) {
            Ok(outboard) => outboard,
            Err(TransportError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let Some(digest) = checked_outboard_digest(&outboard, expected_size)? else {
            return Ok(None);
        };
        if digest != record.outboard_digest {
            return Ok(None);
        }
        Ok(Some(MaterializedObject {
            blob_hash: record.blob_hash,
            object,
        }))
    }

    fn write_mapping(
        &self,
        object: StoreObjectMapping,
        blob_hash: BlobHash,
        outboard_digest: [u8; 32],
    ) -> Result<(), TransportError> {
        let mut record = StoreBlobMappingRecord {
            version: 2,
            object,
            blob_hash,
            outboard_digest,
            checksum: [0; 32],
        };
        record.checksum = record.compute_checksum()?;
        let encoded = postcard::to_allocvec(&record).map_err(frame_error)?;
        if encoded.len() > MAX_STORE_MAP_BYTES {
            return Err(TransportError::ObjectUnavailable);
        }
        let destination = self.mapping_path(object.object_id);
        let stage = unique_temp_path(&self.root, "store-map");
        let mut file = safe_create_new(&stage)?;
        if let Err(error) = file.write_all(&encoded).and_then(|()| file.sync_all()) {
            let _ = safe_remove_regular(&stage);
            return Err(TransportError::Io(error));
        }
        drop(file);
        safe_replace(&stage, &destination)?;
        safe_sync_directory(&self.root)?;
        Ok(())
    }

    fn read_mapping_record(
        &self,
        object_id: [u8; 32],
    ) -> Result<Option<StoreBlobMappingRecord>, TransportError> {
        let path = self.mapping_path(object_id);
        let mut file = match safe_open_read(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(TransportError::Io(error)),
        };
        let metadata = file.metadata()?;
        if metadata.len() == 0 || metadata.len() > MAX_STORE_MAP_BYTES as u64 {
            return Err(TransportError::ObjectUnavailable);
        }
        let mut encoded = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut encoded)?;
        let record: StoreBlobMappingRecord = postcard::from_bytes(&encoded).map_err(frame_error)?;
        if record.version != 2 || record.compute_checksum()? != record.checksum {
            return Err(TransportError::ObjectUnavailable);
        }
        Ok(Some(record))
    }

    fn load_mapping(
        &self,
        claims: &CapabilityClaims,
    ) -> Result<Option<StoreBlobMappingRecord>, TransportError> {
        let Some(record) = self.read_mapping_record(claims.object.object_id)? else {
            return Ok(None);
        };
        if record.object != claims.object || record.blob_hash != claims.blob_hash {
            return Err(TransportError::ObjectUnavailable);
        }
        Ok(Some(record))
    }
}

impl BlobCatalog for StoreBlobCatalog {
    fn open(&self, claims: &CapabilityClaims) -> Result<Option<BlobObject>, TransportError> {
        let Some(_record) = self.load_mapping(claims)? else {
            return Ok(None);
        };
        let object_id = UntrustedObjectId::from_bytes(claims.object.object_id);
        let closure_claim = ArtifactClosureClaim::from_bytes(claims.scope.closure_id);
        let Some(member) = self
            .sink
            .verify_closure_member_claim(closure_claim, object_id)
            .map_err(store_error)?
        else {
            return Ok(None);
        };
        if member.closure().as_bytes() != &claims.scope.closure_id
            || member.object_id().as_bytes() != &claims.object.object_id
            || StoreObjectMapping::from_verified_closure_member(&member) != claims.object
        {
            return Err(TransportError::ObjectUnavailable);
        }
        let Some(reader) = self.sink.open_object(object_id).map_err(store_error)? else {
            return Ok(None);
        };
        if StoreObjectMapping::from_artifact_reader(&reader) != claims.object {
            return Err(TransportError::ObjectUnavailable);
        }
        let tree = BaoTree::new(claims.object.payload_length, BAO_BLOCK_SIZE);
        let outboard = match FileRangeSource::open_readonly(self.outboard_path(claims.blob_hash)) {
            Ok(outboard) => outboard,
            Err(TransportError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        if checked_outboard_digest(&outboard, tree.outboard_size())?
            != Some(_record.outboard_digest)
        {
            return Err(TransportError::ObjectUnavailable);
        }
        Ok(Some(BlobObject {
            blob_hash: claims.blob_hash,
            data: SourceReader(Arc::new(ArtifactPayloadSource::new(reader))),
            outboard: SourceReader(Arc::new(outboard)),
        }))
    }
}

const MAX_STORE_MAP_BYTES: usize = 1024;

/// Hashes only the bounded Bao layout, which is much smaller than its CAS
/// payload. A cache entry is reusable only when the complete immutable layout
/// still matches the digest committed beside its blob mapping.
fn checked_outboard_digest(
    outboard: &FileRangeSource,
    expected_size: u64,
) -> Result<Option<[u8; 32]>, TransportError> {
    if outboard.len() != expected_size
        || outboard
            .file
            .lock()
            .map_err(|_| TransportError::FileLock)?
            .metadata()?
            .len()
            != expected_size
    {
        return Ok(None);
    }
    let mut digest = blake3::Hasher::new();
    let mut offset = 0_u64;
    while offset < expected_size {
        let take = usize::try_from((expected_size - offset).min(64 * 1024))
            .map_err(|_| TransportError::ObjectUnavailable)?;
        let chunk = outboard.read_sync(offset, take)?;
        if chunk.len() != take {
            return Ok(None);
        }
        digest.update(&chunk);
        offset = offset
            .checked_add(chunk.len() as u64)
            .ok_or(TransportError::ObjectUnavailable)?;
    }
    if outboard
        .file
        .lock()
        .map_err(|_| TransportError::FileLock)?
        .metadata()?
        .len()
        != expected_size
    {
        return Ok(None);
    }
    Ok(Some(*digest.finalize().as_bytes()))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct StoreBlobMappingRecord {
    version: u8,
    object: StoreObjectMapping,
    blob_hash: BlobHash,
    outboard_digest: [u8; 32],
    checksum: [u8; 32],
}

impl StoreBlobMappingRecord {
    fn compute_checksum(&self) -> Result<[u8; 32], TransportError> {
        let encoded = postcard::to_allocvec(&(
            self.version,
            self.object,
            self.blob_hash,
            self.outboard_digest,
        ))
        .map_err(frame_error)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.cluster.store-blob-map.v2\0");
        hasher.update(&encoded);
        Ok(*hasher.finalize().as_bytes())
    }
}

#[derive(Clone)]
struct ArtifactPayloadSource {
    reader: Arc<Mutex<ArtifactObjectReader>>,
    len: u64,
}

impl fmt::Debug for ArtifactPayloadSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactPayloadSource")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl ArtifactPayloadSource {
    fn new(reader: ArtifactObjectReader) -> Self {
        let len = reader.payload_len();
        Self {
            reader: Arc::new(Mutex::new(reader)),
            len,
        }
    }
}

impl RandomAccessSource for ArtifactPayloadSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, requested: usize) -> io::Result<Bytes> {
        if offset >= self.len {
            return Ok(Bytes::new());
        }
        let length = requested
            .min(IO_CHUNK_BYTES)
            .min(usize::try_from(self.len - offset).unwrap_or(usize::MAX));
        let mut bytes = vec![0; length];
        let count = self
            .reader
            .lock()
            .map_err(|_| io::Error::other("artifact reader lock poisoned"))?
            .read_payload_range(offset, &mut bytes)
            .map_err(|error| io::Error::other(format!("{error:?}")))?;
        if count != length {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        bytes.truncate(count);
        Ok(bytes.into())
    }
}

struct ArtifactPayloadStream {
    source: ArtifactPayloadSource,
    offset: u64,
}

impl ArtifactPayloadStream {
    fn new(source: ArtifactPayloadSource) -> Self {
        Self { source, offset: 0 }
    }
}

impl iroh_io::AsyncStreamReader for ArtifactPayloadStream {
    fn read_bytes(&mut self, len: usize) -> impl Future<Output = io::Result<Bytes>> {
        async move {
            if len == 0 {
                return Ok(Bytes::new());
            }
            let source = self.source.clone();
            let offset = self.offset;
            let bytes = tokio::task::spawn_blocking(move || source.read_at(offset, len))
                .await
                .map_err(io::Error::other)??;
            self.offset = self
                .offset
                .checked_add(bytes.len() as u64)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "offset overflow"))?;
            Ok(bytes)
        }
    }

    fn read<const L: usize>(&mut self) -> impl Future<Output = io::Result<[u8; L]>> {
        async move {
            let bytes = self.read_bytes(L).await?;
            bytes
                .as_ref()
                .try_into()
                .map_err(|_| io::Error::from(io::ErrorKind::UnexpectedEof))
        }
    }
}

fn unique_temp_path(root: &Path, label: &str) -> PathBuf {
    root.join(format!(
        ".{label}-{}-{}.tmp",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed),
    ))
}

impl BlobCatalog for DirectoryBlobCatalog {
    fn open(&self, claims: &CapabilityClaims) -> Result<Option<BlobObject>, TransportError> {
        let data_path = self.blob_path(claims.blob_hash);
        let outboard_path = self.outboard_path(claims.blob_hash);
        let data = match FileRangeSource::open_readonly(data_path) {
            Ok(data) => data,
            Err(TransportError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let outboard = match FileRangeSource::open_readonly(outboard_path) {
            Ok(outboard) => outboard,
            Err(TransportError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let tree = BaoTree::new(claims.object.payload_length, BAO_BLOCK_SIZE);
        if data.len() != claims.object.payload_length || outboard.len() != tree.outboard_size() {
            return Err(TransportError::ObjectUnavailable);
        }
        Ok(Some(BlobObject {
            blob_hash: claims.blob_hash,
            data: SourceReader(Arc::new(data)),
            outboard: SourceReader(Arc::new(outboard)),
        }))
    }
}

/// Bounded random-access file used for durable source and sparse receive files.
#[derive(Debug, Clone)]
pub struct FileRangeSource {
    file: Arc<Mutex<File>>,
    len: Arc<AtomicU64>,
}

#[cfg(unix)]
mod safe_fs {
    use std::{
        ffi::{OsStr, OsString},
        io,
        os::unix::fs::MetadataExt,
        path::{Component, Path},
    };

    use rustix::{
        fs::{AtFlags, Mode, OFlags, mkdirat, open, openat, renameat, unlinkat},
        io::Errno,
    };

    use super::File;

    struct Parent {
        directory: File,
        leaf: OsString,
    }

    fn error(errno: Errno) -> io::Error {
        let kind = if errno == Errno::NOENT {
            io::ErrorKind::NotFound
        } else if errno == Errno::EXIST {
            io::ErrorKind::AlreadyExists
        } else {
            io::ErrorKind::Other
        };
        io::Error::new(kind, errno.to_string())
    }

    fn root_dir() -> io::Result<File> {
        open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(error)
    }

    fn absolute_path(path: &Path) -> io::Result<std::path::PathBuf> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        #[cfg(target_os = "macos")]
        {
            // macOS exposes these common system paths as symlinks. Resolve just these
            // fixed OS aliases so all caller-controlled components still go through
            // the descriptor-relative O_NOFOLLOW walk below.
            if let Ok(suffix) = absolute.strip_prefix("/var") {
                return Ok(Path::new("/private/var").join(suffix));
            }
            if let Ok(suffix) = absolute.strip_prefix("/tmp") {
                return Ok(Path::new("/private/tmp").join(suffix));
            }
        }
        Ok(absolute)
    }

    fn parent(path: &Path, create: bool) -> io::Result<Parent> {
        let leaf = path
            .file_name()
            .filter(|name| *name != "." && *name != "..")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?
            .to_os_string();
        let parent_path = path.parent().unwrap_or_else(|| Path::new("."));
        let absolute = absolute_path(parent_path)?;
        let mut directory = root_dir()?;
        for component in absolute.components() {
            let name = match component {
                Component::RootDir | Component::CurDir => continue,
                Component::ParentDir => OsStr::new(".."),
                Component::Normal(name) => name,
                Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unsupported path prefix",
                    ));
                }
            };
            let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            let next = match openat(&directory, name, flags, Mode::empty()) {
                Ok(fd) => fd,
                Err(err) if err == Errno::NOENT && create => {
                    if name != ".." {
                        let _ = mkdirat(&directory, name, Mode::RWXU);
                    }
                    openat(&directory, name, flags, Mode::empty()).map_err(error)?
                }
                Err(err) => return Err(error(err)),
            };
            directory = File::from(next);
        }
        Ok(Parent { directory, leaf })
    }

    fn open_directory(path: &Path) -> io::Result<File> {
        let absolute = absolute_path(path)?;
        let mut directory = root_dir()?;
        for component in absolute.components() {
            let name = match component {
                Component::RootDir | Component::CurDir => continue,
                Component::ParentDir => OsStr::new(".."),
                Component::Normal(name) => name,
                Component::Prefix(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "unsupported path prefix",
                    ));
                }
            };
            let next = openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(error)?;
            directory = File::from(next);
        }
        Ok(directory)
    }

    fn regular(file: File) -> io::Result<File> {
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "expected a singly-linked regular file",
            ));
        }
        Ok(file)
    }

    pub(super) fn open_read(path: &Path) -> io::Result<File> {
        let parent = parent(path, false)?;
        let file = openat(
            &parent.directory,
            &parent.leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(error)?;
        regular(file)
    }

    pub(super) fn open_rw(path: &Path) -> io::Result<File> {
        let parent = parent(path, false)?;
        let file = openat(
            &parent.directory,
            &parent.leaf,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(error)?;
        regular(file)
    }

    pub(super) fn create_new(path: &Path) -> io::Result<File> {
        let parent = parent(path, true)?;
        let file = openat(
            &parent.directory,
            &parent.leaf,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(error)?;
        regular(file)
    }

    pub(super) fn open_or_create(path: &Path) -> io::Result<(File, bool)> {
        match open_rw(path) {
            Ok(file) => Ok((file, false)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => match create_new(path) {
                Ok(file) => Ok((file, true)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    open_rw(path).map(|file| (file, false))
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    pub(super) fn ensure_directory(path: &Path) -> io::Result<()> {
        if path == Path::new(".") || path == Path::new("/") {
            return Ok(());
        }
        let leaf = path
            .file_name()
            .filter(|name| *name != "." && *name != "..")
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing directory name"))?;
        let parent = parent(path, true)?;
        match mkdirat(&parent.directory, leaf, Mode::RWXU) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(err) => return Err(error(err)),
        }
        let directory = openat(
            &parent.directory,
            leaf,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(error)?;
        File::from(directory).sync_all()
    }

    pub(super) fn replace(source: &Path, destination: &Path) -> io::Result<()> {
        let source_parent = parent(source, false)?;
        let destination_parent = parent(destination, true)?;
        let source_file = openat(
            &source_parent.directory,
            &source_parent.leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(error)?;
        drop(regular(source_file)?);
        match openat(
            &destination_parent.directory,
            &destination_parent.leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => {
                let _ = regular(File::from(fd))?;
            }
            Err(err) if err == Errno::NOENT => {}
            Err(err) => return Err(error(err)),
        }
        renameat(
            &source_parent.directory,
            &source_parent.leaf,
            &destination_parent.directory,
            &destination_parent.leaf,
        )
        .map_err(error)?;
        destination_parent.directory.sync_all()
    }

    pub(super) fn remove_regular(path: &Path) -> io::Result<()> {
        let parent = match parent(path, false) {
            Ok(parent) => parent,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        match openat(
            &parent.directory,
            &parent.leaf,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => drop(regular(File::from(fd))?),
            Err(err) if err == Errno::NOENT => return Ok(()),
            Err(err) => return Err(error(err)),
        }
        unlinkat(&parent.directory, &parent.leaf, AtFlags::empty()).map_err(error)?;
        parent.directory.sync_all()
    }

    pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
        open_directory(path)?.sync_all()
    }
}

#[cfg(not(unix))]
mod safe_fs {
    use std::{
        fs::{self, File, OpenOptions},
        io,
        path::Path,
    };

    fn check_regular(path: &Path, file: &File) -> io::Result<()> {
        let path_metadata = fs::symlink_metadata(path)?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_file()
            || !file.metadata()?.is_file()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe file type",
            ));
        }
        Ok(())
    }

    pub(super) fn open_read(path: &Path) -> io::Result<File> {
        let file = OpenOptions::new().read(true).open(path)?;
        check_regular(path, &file)?;
        Ok(file)
    }

    pub(super) fn open_rw(path: &Path) -> io::Result<File> {
        let file = OpenOptions::new().read(true).write(true).open(path)?;
        check_regular(path, &file)?;
        Ok(file)
    }

    pub(super) fn create_new(path: &Path) -> io::Result<File> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        check_regular(path, &file)?;
        Ok(file)
    }

    pub(super) fn open_or_create(path: &Path) -> io::Result<(File, bool)> {
        match open_rw(path) {
            Ok(file) => Ok((file, false)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => match create_new(path) {
                Ok(file) => Ok((file, true)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    open_rw(path).map(|file| (file, false))
                }
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        }
    }

    pub(super) fn ensure_directory(path: &Path) -> io::Result<()> {
        if path == Path::new(".") {
            return Ok(());
        }
        fs::create_dir_all(path)?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsafe directory type",
            ));
        }
        Ok(())
    }

    pub(super) fn replace(source: &Path, destination: &Path) -> io::Result<()> {
        if destination.exists() {
            let file = open_rw(destination)?;
            drop(file);
        }
        fs::rename(source, destination)
    }

    pub(super) fn remove_regular(path: &Path) -> io::Result<()> {
        match open_rw(path) {
            Ok(file) => drop(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        fs::remove_file(path)
    }

    pub(super) fn sync_directory(path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }
}

fn safe_open_read(path: &Path) -> io::Result<File> {
    safe_fs::open_read(path)
}

fn safe_create_new(path: &Path) -> io::Result<File> {
    safe_fs::create_new(path)
}

fn safe_open_or_create(path: &Path) -> io::Result<(File, bool)> {
    safe_fs::open_or_create(path)
}

fn safe_ensure_directory(path: &Path) -> io::Result<()> {
    safe_fs::ensure_directory(path)
}

fn safe_replace(source: &Path, destination: &Path) -> io::Result<()> {
    safe_fs::replace(source, destination)
}

fn safe_remove_regular(path: &Path) -> io::Result<()> {
    safe_fs::remove_regular(path)
}

fn safe_sync_directory(path: &Path) -> io::Result<()> {
    safe_fs::sync_directory(path)
}

impl FileRangeSource {
    /// Open an existing file for bounded random-access reads.
    pub fn open_readonly(path: impl AsRef<Path>) -> Result<Self, TransportError> {
        let file = safe_open_read(path.as_ref())?;
        let len = file.metadata()?.len();
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            len: Arc::new(AtomicU64::new(len)),
        })
    }

    /// Create or reopen an exact-sized sparse receive/outboard file.
    pub fn create_sparse(path: impl AsRef<Path>, len: u64) -> Result<Self, TransportError> {
        let path = path.as_ref();
        let (file, created) = safe_open_or_create(path)?;
        if created {
            file.set_len(len)?;
        } else if file.metadata()?.len() != len {
            return Err(TransportError::FileLengthMismatch);
        }
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            len: Arc::new(AtomicU64::new(len)),
        })
    }

    /// Synchronize payload or outboard bytes to stable storage.
    pub fn sync_all(&self) -> Result<(), TransportError> {
        self.file
            .lock()
            .map_err(|_| TransportError::FileLock)?
            .sync_all()?;
        Ok(())
    }

    fn read_sync(&self, offset: u64, length: usize) -> io::Result<Bytes> {
        let source_len = self.len.load(Ordering::Acquire);
        if offset >= source_len {
            return Ok(Bytes::new());
        }
        let max = usize::try_from((source_len - offset).min(length as u64))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "range too large"))?;
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("range file lock poisoned"))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::with_capacity(max.min(IO_CHUNK_BYTES));
        (&mut *file).take(max as u64).read_to_end(&mut bytes)?;
        Ok(bytes.into())
    }

    fn write_sync(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "range overflow"))?;
        if end > self.len.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "write exceeds sparse file bound",
            ));
        }
        let mut file = self
            .file
            .lock()
            .map_err(|_| io::Error::other("range file lock poisoned"))?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(bytes)
    }
}

impl RandomAccessSource for FileRangeSource {
    fn len(&self) -> u64 {
        self.len.load(Ordering::Acquire)
    }

    fn read_at(&self, offset: u64, length: usize) -> io::Result<Bytes> {
        self.read_sync(offset, length)
    }
}

impl AsyncSliceReader for FileRangeSource {
    async fn read_at(&mut self, offset: u64, len: usize) -> io::Result<Bytes> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.read_sync(offset, len))
            .await
            .map_err(io::Error::other)?
    }

    async fn size(&mut self) -> io::Result<u64> {
        Ok(self.len())
    }
}

impl AsyncSliceWriter for FileRangeSource {
    async fn write_bytes_at(&mut self, offset: u64, data: Bytes) -> io::Result<()> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || this.write_sync(offset, &data))
            .await
            .map_err(io::Error::other)?
    }

    async fn write_at(&mut self, offset: u64, data: &[u8]) -> io::Result<()> {
        let this = self.clone();
        let bytes = data.to_vec();
        tokio::task::spawn_blocking(move || this.write_sync(offset, &bytes))
            .await
            .map_err(io::Error::other)?
    }

    async fn set_len(&mut self, len: u64) -> io::Result<()> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            this.file
                .lock()
                .map_err(|_| io::Error::other("range file lock poisoned"))?
                .set_len(len)?;
            this.len.store(len, Ordering::Release);
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }

    async fn sync(&mut self) -> io::Result<()> {
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            this.file
                .lock()
                .map_err(|_| io::Error::other("range file lock poisoned"))?
                .sync_all()
        })
        .await
        .map_err(io::Error::other)?
    }
}

#[derive(Debug, Clone)]
pub struct SourceReader(Arc<dyn RandomAccessSource>);

impl SourceReader {
    /// Open a dynamic random-access reader over a durable or fixture source.
    pub fn new(source: Arc<dyn RandomAccessSource>) -> Self {
        Self(source)
    }

    /// Return source length.
    pub fn len(&self) -> u64 {
        self.0.len()
    }
}

impl AsyncSliceReader for SourceReader {
    async fn read_at(&mut self, offset: u64, len: usize) -> io::Result<Bytes> {
        let source = Arc::clone(&self.0);
        tokio::task::spawn_blocking(move || source.read_at(offset, len))
            .await
            .map_err(io::Error::other)?
    }

    async fn size(&mut self) -> io::Result<u64> {
        Ok(self.len())
    }
}

#[cfg(test)]
#[derive(Debug, Clone)]
struct MemoryRangeSource(Bytes);

#[cfg(test)]
impl RandomAccessSource for MemoryRangeSource {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, offset: u64, length: usize) -> io::Result<Bytes> {
        if offset >= self.0.len() as u64 {
            return Ok(Bytes::new());
        }
        let start = offset as usize;
        let end = start.saturating_add(length).min(self.0.len());
        Ok(self.0.slice(start..end))
    }
}

#[derive(Debug, Clone)]
struct SourceOutboard {
    root: blake3::Hash,
    tree: BaoTree,
    data: SourceReader,
}

impl Outboard for SourceOutboard {
    fn root(&self) -> blake3::Hash {
        self.root
    }

    fn tree(&self) -> BaoTree {
        self.tree
    }

    async fn load(&mut self, node: TreeNode) -> io::Result<Option<(blake3::Hash, blake3::Hash)>> {
        let Some(offset) = self.tree.pre_order_offset(node) else {
            return Ok(None);
        };
        let bytes = self.data.read_at(offset * 64, 64).await?;
        if bytes.len() != 64 {
            return Ok(Some((
                blake3::Hash::from([0; 32]),
                blake3::Hash::from([0; 32]),
            )));
        }
        let mut left = [0; 32];
        let mut right = [0; 32];
        left.copy_from_slice(&bytes[..32]);
        right.copy_from_slice(&bytes[32..]);
        Ok(Some((blake3::Hash::from(left), blake3::Hash::from(right))))
    }
}

/// Durable sparse transfer state for reconnect and cold resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeState {
    /// Exact scheduler work/fence/closure identity.
    pub scope: TransferScope,
    /// Raw BLAKE3 payload identity.
    pub blob_hash: BlobHash,
    /// Typed store identity mapping; still requires ArtifactSink admission.
    pub object: StoreObjectMapping,
    /// Sorted, merged coverage of verified Bao chunks.
    pub covered: Vec<ChunkRange>,
    /// Integrity checksum over metadata and coverage.
    pub checksum: [u8; 32],
}

/// One live receive session whose existing checkpoint coverage was verified on open.
///
/// A cold session revalidates every previously covered Bao range once. Subsequent range fetches
/// verify only their new proof and atomically extend the checkpoint, avoiding quadratic
/// re-verification when one object arrives in many bounded requests. Restarting the process
/// creates a new session and repeats the full cold validation before trusting progress.
#[derive(Debug)]
pub struct VerifiedCoverage {
    checkpoint: PathBuf,
    state: ResumeState,
    issuer: EndpointId,
    client: EndpointId,
    server: EndpointId,
}

impl VerifiedCoverage {
    /// Load or create an exact-scope sparse checkpoint and verify all persisted coverage once.
    pub async fn open(
        endpoint: &Endpoint,
        server_addr: &EndpointAddr,
        trusted_issuer: EndpointId,
        capability: &Capability,
        expected_scope: TransferScope,
        checkpoint_path: impl AsRef<Path>,
    ) -> Result<Self, TransportError> {
        let client = endpoint.id();
        let claims = &capability.claims;
        verify_client_capability(
            trusted_issuer,
            client,
            server_addr.id,
            capability,
            expected_scope,
            now_unix_ms()?,
        )?;
        let checkpoint = checkpoint_path.as_ref().to_path_buf();
        let state = match safe_open_read(&checkpoint) {
            Ok(_) => ResumeState::load(&checkpoint, claims)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ResumeState::empty(&checkpoint, claims)?
            }
            Err(error) => return Err(TransportError::Io(error)),
        };
        state.verify_covered_ranges(&checkpoint).await?;
        validate_next_range(&state, claims.range)?;
        Ok(Self {
            checkpoint,
            state,
            issuer: trusted_issuer,
            client,
            server: server_addr.id,
        })
    }

    /// Current persisted scope and coverage state. Completion still requires full-payload hash
    /// verification before a caller may feed bytes to ArtifactSink.
    #[must_use]
    pub fn state(&self) -> &ResumeState {
        &self.state
    }

    /// Fetch one next contiguous capability range into this verified session.
    pub async fn fetch_range(
        &mut self,
        endpoint: &Endpoint,
        server_addr: EndpointAddr,
        capability: Capability,
    ) -> Result<(), TransportError> {
        let claims = capability.claims.clone();
        if endpoint.id() != self.client || server_addr.id != self.server {
            return Err(TransportError::Rejected(RejectCode::ScopeMismatch));
        }
        verify_client_capability(
            self.issuer,
            self.client,
            self.server,
            &capability,
            self.state.scope,
            now_unix_ms()?,
        )?;
        if claims.blob_hash != self.state.blob_hash || claims.object != self.state.object {
            return Err(TransportError::Rejected(RejectCode::ScopeMismatch));
        }
        validate_next_range(&self.state, claims.range)?;

        let mut data_file = FileRangeSource::create_sparse(
            ResumeState::data_path(&self.checkpoint),
            claims.object.payload_length,
        )?;
        let outboard_file = FileRangeSource::create_sparse(
            ResumeState::outboard_path(&self.checkpoint),
            BaoTree::new(claims.object.payload_length, BAO_BLOCK_SIZE).outboard_size(),
        )?;

        let connection = endpoint
            .connect(server_addr, ALPN)
            .await
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        if connection.remote_id() != claims.server {
            connection.close(1_u32.into(), b"unexpected server identity");
            return Err(TransportError::Rejected(RejectCode::ScopeMismatch));
        }
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        write_frame(
            &mut send,
            &RangeRequest {
                capability,
                range: claims.range,
            },
        )
        .await?;

        match read_frame::<WireReply>(&mut recv).await? {
            WireReply::Accepted => {}
            WireReply::Rejected(code) => return Err(TransportError::Rejected(code)),
        }

        let tree = BaoTree::new(claims.object.payload_length, BAO_BLOCK_SIZE);
        let mut outboard = bao_tree::io::outboard::PreOrderOutboard {
            root: blake3::Hash::from(claims.blob_hash.0),
            tree,
            data: outboard_file.clone(),
        };
        let mut trailing = [0_u8; 1];
        let trailing_len = if claims.object.payload_length == 0 {
            // Bao-tree's range iterator requires a nonempty query. A zero-byte object has no
            // Bao chunks, so the signed 0..0 grant is authenticated over Iroh and the client
            // checks BLAKE3(empty) locally instead of invoking the partial-range decoder.
            recv.read(&mut trailing)
                .await
                .map_err(|error| TransportError::Iroh(error.to_string()))?
                .unwrap_or(0)
        } else {
            let mut reader = TokioStreamReader::new(recv);
            let ranges =
                ChunkRanges::from(ChunkNum(claims.range.start)..ChunkNum(claims.range.end));
            decode_ranges(&mut reader, ranges, &mut data_file, &mut outboard)
                .await
                .map_err(|error| TransportError::Bao(error.to_string()))?;
            reader
                .0
                .read(&mut trailing)
                .await
                .map_err(|error| TransportError::Iroh(error.to_string()))?
                .unwrap_or(0)
        };
        if trailing_len != 0 {
            return Err(TransportError::Frame(
                "trailing bytes after Bao range".into(),
            ));
        }

        data_file.sync_all()?;
        outboard_file.sync_all()?;
        self.state.add_coverage(claims.range);
        if self.state.is_complete() {
            self.state.verify_complete_payload(&self.checkpoint).await?;
        }
        self.state.save_atomic(&self.checkpoint)?;
        write_frame(
            &mut send,
            &RangeAck {
                scope: claims.scope,
                nonce: claims.nonce,
                range: claims.range,
            },
        )
        .await?;
        send.finish()
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        let mut receipt_stream = tokio::time::timeout(RANGE_ACK_TIMEOUT, connection.accept_uni())
            .await
            .map_err(|_| TransportError::Iroh("range confirmation timed out".into()))?
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        let receipt: RangeReceipt = read_frame(&mut receipt_stream).await?;
        if receipt
            != (RangeReceipt {
                scope: claims.scope,
                nonce: claims.nonce,
                range: claims.range,
            })
        {
            return Err(TransportError::Rejected(RejectCode::ScopeMismatch));
        }
        Ok(())
    }

    /// Finish the live session and return its authenticated sparse checkpoint state.
    ///
    /// Complete sessions already hash-checked the full payload before persisting the final
    /// coverage update. `feed_to_artifact_session` independently rechecks that invariant before
    /// any bytes enter store admission.
    pub fn finish(self) -> ResumeState {
        self.state
    }
}

impl ResumeState {
    /// Create an empty checkpoint and bounded sparse payload/outboard files.
    pub fn empty(
        path: impl AsRef<Path>,
        claims: &CapabilityClaims,
    ) -> Result<Self, TransportError> {
        validate_claim_shape(claims).map_err(TransportError::Rejected)?;
        let path = path.as_ref();
        create_sidecar(path, "data", claims.object.payload_length, true)?;
        let tree = BaoTree::new(claims.object.payload_length, BAO_BLOCK_SIZE);
        create_sidecar(path, "bao", tree.outboard_size(), true)?;
        let mut state = Self {
            scope: claims.scope,
            blob_hash: claims.blob_hash,
            object: claims.object,
            covered: Vec::new(),
            checksum: [0; 32],
        };
        state.refresh_checksum()?;
        Ok(state)
    }

    /// Exact data sidecar path for a checkpoint.
    #[must_use]
    pub fn data_path(path: impl AsRef<Path>) -> PathBuf {
        sidecar_path(path.as_ref(), "data")
    }

    /// Exact Bao outboard sidecar path for a checkpoint.
    #[must_use]
    pub fn outboard_path(path: impl AsRef<Path>) -> PathBuf {
        sidecar_path(path.as_ref(), "bao")
    }

    /// Whether all Bao chunks have been verified. Call `verify_complete_payload` before CAS use.
    pub fn is_complete(&self) -> bool {
        if self.object.payload_length == 0 {
            return self.covered.len() == 1 && self.covered[0] == (ChunkRange { start: 0, end: 0 });
        }
        let total = ChunkNum::chunks(self.object.payload_length).0;
        self.covered.len() == 1
            && self.covered[0]
                == ChunkRange {
                    start: 0,
                    end: total,
                }
    }

    /// Verify the entire sparse payload against its raw BlobHash with bounded memory.
    pub async fn verify_complete_payload(
        &self,
        checkpoint: impl AsRef<Path>,
    ) -> Result<(), TransportError> {
        if !self.is_complete() {
            return Err(TransportError::IncompleteTransfer);
        }
        self.verify_covered_ranges(&checkpoint).await?;
        let mut file = FileRangeSource::open_readonly(Self::data_path(checkpoint))?;
        if hash_source(&mut file, self.object.payload_length).await? != self.blob_hash {
            return Err(TransportError::BlobHashMismatch);
        }
        Ok(())
    }

    /// Re-verify persisted coverage from its sparse data and Bao outboard before cold resume.
    ///
    /// A checkpoint checksum detects accidental metadata damage only. It is not an authority
    /// signature, so all covered byte ranges are checked again against the capability's raw
    /// BlobHash root before the first-missing cursor is trusted.
    pub async fn verify_covered_ranges(
        &self,
        checkpoint: impl AsRef<Path>,
    ) -> Result<(), TransportError> {
        if self.covered.is_empty() {
            return Ok(());
        }
        let checkpoint = checkpoint.as_ref();
        let data = FileRangeSource::open_readonly(Self::data_path(checkpoint))?;
        let outboard_file = FileRangeSource::open_readonly(Self::outboard_path(checkpoint))?;
        let tree = BaoTree::new(self.object.payload_length, BAO_BLOCK_SIZE);
        if data.len() != self.object.payload_length || outboard_file.len() != tree.outboard_size() {
            return Err(TransportError::CheckpointInvalid);
        }
        if self.object.payload_length == 0 {
            if self.covered.len() != 1
                || self.covered[0] != (ChunkRange { start: 0, end: 0 })
                || self.blob_hash != BlobHash(*blake3::hash(&[]).as_bytes())
            {
                return Err(TransportError::BlobHashMismatch);
            }
            return Ok(());
        }
        let source = SourceReader(Arc::new(data));
        let outboard = SourceOutboard {
            root: blake3::Hash::from(self.blob_hash.0),
            tree,
            data: SourceReader(Arc::new(outboard_file)),
        };
        for range in &self.covered {
            let ranges = ChunkRanges::from(ChunkNum(range.start)..ChunkNum(range.end));
            let writer = TokioStreamWriter(tokio::io::sink());
            encode_ranges_validated(source.clone(), outboard.clone(), &ranges, writer)
                .await
                .map_err(|error| TransportError::Bao(error.to_string()))?;
        }
        Ok(())
    }

    /// Feed a fully Bao-verified payload through backend-store's bounded typed admission.
    ///
    /// The caller must have begun the `ArtifactSession` with the exact `StoreObjectMapping`
    /// and target `ArtifactClosureClaim`. `put` independently recomputes typed keys, versions,
    /// the physical ObjectId, and closure membership before `finish` returns a storage receipt.
    pub async fn feed_to_artifact_session(
        &self,
        checkpoint: impl AsRef<Path>,
        object_index: usize,
        session: &mut ArtifactSession,
    ) -> Result<(), TransportError> {
        self.verify_complete_payload(&checkpoint).await?;
        let mut payload = FileRangeSource::open_readonly(Self::data_path(checkpoint))?;
        let mut offset = 0_u64;
        while offset < self.object.payload_length {
            let take =
                usize::try_from((self.object.payload_length - offset).min(IO_CHUNK_BYTES as u64))
                    .map_err(|_| TransportError::ObjectTooLarge)?;
            let bytes = AsyncSliceReader::read_at(&mut payload, offset, take).await?;
            if bytes.len() != take {
                return Err(TransportError::Io(io::Error::from(
                    io::ErrorKind::UnexpectedEof,
                )));
            }
            session
                .put(object_index, offset, &bytes)
                .map_err(store_error)?;
            offset += bytes.len() as u64;
        }
        Ok(())
    }

    /// Load and validate a checkpoint for the exact signed work/object fence.
    pub fn load(
        path: impl AsRef<Path>,
        expected: &CapabilityClaims,
    ) -> Result<Self, TransportError> {
        let path = path.as_ref();
        let mut file = safe_open_read(path)?;
        let metadata = file.metadata()?;
        if metadata.len() > MAX_CHECKPOINT_BYTES {
            return Err(TransportError::CheckpointInvalid);
        }
        let mut encoded = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut encoded)?;
        let state: Self = postcard::from_bytes(&encoded).map_err(frame_error)?;
        state.validate_for(expected)?;
        Ok(state)
    }

    /// Persist the small checkpoint atomically after sidecars are synchronized.
    pub fn save_atomic(&mut self, path: impl AsRef<Path>) -> Result<(), TransportError> {
        self.scope.validate()?;
        self.refresh_checksum()?;
        let encoded = postcard::to_allocvec(self).map_err(frame_error)?;
        if encoded.len() as u64 > MAX_CHECKPOINT_BYTES {
            return Err(TransportError::CheckpointInvalid);
        }
        let path = path.as_ref();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        safe_ensure_directory(parent)?;
        let temp = checkpoint_temp_path(path);
        let mut file = safe_create_new(&temp)?;
        file.write_all(&encoded)?;
        file.sync_all()?;
        drop(file);
        safe_replace(&temp, path)?;
        safe_sync_directory(parent)?;
        Ok(())
    }

    fn validate_for(&self, expected: &CapabilityClaims) -> Result<(), TransportError> {
        validate_claim_shape(expected).map_err(TransportError::Rejected)?;
        if self.scope != expected.scope
            || self.blob_hash != expected.blob_hash
            || self.object != expected.object
            || self.object.payload_length != expected.object.payload_length
        {
            return Err(TransportError::CheckpointScopeMismatch);
        }
        if !self.coverage_is_canonical() || self.compute_checksum()? != self.checksum {
            return Err(TransportError::CheckpointInvalid);
        }
        Ok(())
    }

    fn coverage_is_canonical(&self) -> bool {
        let total = ChunkNum::chunks(self.object.payload_length).0;
        if self.object.payload_length == 0 {
            return self.covered.is_empty()
                || (self.covered.len() == 1
                    && self.covered[0] == (ChunkRange { start: 0, end: 0 }));
        }
        let mut previous_end = 0;
        for (index, range) in self.covered.iter().enumerate() {
            if range.start >= range.end
                || range.end > total
                || (index > 0 && range.start <= previous_end)
            {
                return false;
            }
            previous_end = range.end;
        }
        true
    }

    fn add_coverage(&mut self, range: ChunkRange) {
        self.covered.push(range);
        self.covered.sort_by_key(|entry| entry.start);
        let mut merged: Vec<ChunkRange> = Vec::with_capacity(self.covered.len());
        for next in self.covered.drain(..) {
            if let Some(last) = merged.last_mut() {
                if last.end == next.start {
                    last.end = next.end;
                    continue;
                }
            }
            merged.push(next);
        }
        self.covered = merged;
    }

    fn first_missing_chunk(&self) -> u64 {
        let mut cursor = 0;
        for range in &self.covered {
            if range.start > cursor {
                break;
            }
            cursor = cursor.max(range.end);
        }
        cursor
    }

    fn compute_checksum(&self) -> Result<[u8; 32], TransportError> {
        let payload =
            postcard::to_allocvec(&(self.scope, self.blob_hash, self.object, &self.covered))
                .map_err(frame_error)?;
        Ok(*blake3::hash(&payload).as_bytes())
    }

    fn refresh_checksum(&mut self) -> Result<(), TransportError> {
        self.checksum = self.compute_checksum()?;
        Ok(())
    }
}

/// Rejection reason sent before any artifact bytes are available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RejectCode {
    /// TLS-authenticated peer is absent from the allowlist.
    PeerNotAllowed,
    /// Capability signature or validity window failed.
    InvalidCapability,
    /// Work, attempt, exact fence, server, client, or closure differs.
    ScopeMismatch,
    /// Request does not match the one signed range.
    InvalidRange,
    /// Object is absent or its size differs from the claim.
    ObjectUnavailable,
    /// Per-request response budget is outside protocol bounds.
    InvalidBudget,
}

/// Transport protocol errors.
#[derive(Debug, Error)]
pub enum TransportError {
    /// I/O failure.
    #[error("transport I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Iroh endpoint or connection failed.
    #[error("Iroh failed: {0}")]
    Iroh(String),
    /// Protocol admission rejected the request.
    #[error("request rejected: {0:?}")]
    Rejected(RejectCode),
    /// Protocol framing or serialization failed.
    #[error("invalid protocol frame: {0}")]
    Frame(String),
    /// Bao could not encode or verify the requested range.
    #[error("Bao range failed: {0}")]
    Bao(String),
    /// Work/attempt fence or closure identity is invalid.
    #[error("invalid transfer scope")]
    InvalidScope,
    /// Object exceeds the bounded first-slice limit.
    #[error("object exceeds transport limit")]
    ObjectTooLarge,
    /// The complete raw payload digest did not match its capability.
    #[error("raw BLAKE3 payload hash mismatch")]
    BlobHashMismatch,
    /// The checkpoint is malformed, corrupted, or outside bounds.
    #[error("invalid transfer checkpoint")]
    CheckpointInvalid,
    /// The checkpoint belongs to a different work, attempt, fence, closure, or object.
    #[error("checkpoint does not match this exact transfer scope")]
    CheckpointScopeMismatch,
    /// The object has not received every verified Bao range.
    #[error("transfer is incomplete")]
    IncompleteTransfer,
    /// A file source lock was poisoned.
    #[error("range file lock poisoned")]
    FileLock,
    /// A durable source or checkpoint sidecar has an unexpected exact length.
    #[error("durable range file has an unexpected length")]
    FileLengthMismatch,
    /// Requested source object is unavailable.
    #[error("object is unavailable")]
    ObjectUnavailable,
    /// Store's typed object or closure admission rejected the received payload.
    #[error("backend-store artifact admission failed: {0}")]
    Store(String),
    /// Authenticated control protocol rejected a connection or message.
    #[error("control message rejected: {0:?}")]
    ControlRejected(ControlRejectCode),
}

/// Current Unix time as a checked millisecond count.
pub fn now_unix_ms() -> io::Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?;
    u64::try_from(duration.as_millis())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Unix time overflow"))
}

/// Create a direct-only encrypted endpoint bound to one explicit address.
///
/// The endpoint has no relay transport and no DNS/Pkarr address lookup. `bind_addr`
/// should be an interface reachable only by trusted cluster peers; tests use loopback.
pub async fn bind_direct(
    secret: SecretKey,
    bind_addr: SocketAddr,
) -> Result<Endpoint, TransportError> {
    let builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret)
        .alpns(vec![
            ALPN.to_vec(),
            CONTROL_ALPN.to_vec(),
            PROBE_ALPN.to_vec(),
        ])
        .relay_mode(RelayMode::Disabled)
        .clear_address_lookup()
        .clear_ip_transports()
        .bind_addr(bind_addr)
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    builder
        .bind()
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))
}

/// Server protocol state. Admission always precedes catalog lookup.
#[derive(Clone)]
pub struct ServerState {
    /// Peer and work capability admission policy.
    pub policy: AdmissionPolicy,
    /// Durable or fixture-backed immutable blob catalog.
    pub objects: Arc<dyn BlobCatalog>,
}

impl fmt::Debug for ServerState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerState")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl ServerState {
    /// Create server state around a durable or fixture catalog.
    pub fn new(policy: AdmissionPolicy, objects: Arc<dyn BlobCatalog>) -> Self {
        Self { policy, objects }
    }
}

/// Per-range application-stream accounting for tests and operators.
///
/// `bao_stream_bytes` counts encoded Bao data and proof bytes. It excludes QUIC/IP/network
/// headers and the small request/accept/ack control frames, which Iroh does not expose here.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServeMetrics {
    /// Bao proof and payload bytes written on the artifact data stream.
    pub bao_stream_bytes: u64,
}

/// One authenticated inbound protocol connection selected by the cluster ALPN dispatcher.
#[derive(Debug)]
pub enum AcceptedClusterConnection {
    /// Control messages on the assignment-scoped control ALPN.
    Control(ControlChannel),
    /// Capability-scoped Bao range on the artifact ALPN.
    Artifact(ArtifactConnection),
    /// Authenticated compiler Have/capacity probe on the probe ALPN.
    Probe(ProbeConnection),
}

/// An authenticated inbound probe connection selected by the ALPN dispatcher.
///
/// The QUIC connection remains private; callers must pass it to the probe acceptor with
/// the worker's explicit coordinator allowlist.
pub struct ProbeConnection {
    connection: Connection,
    local: EndpointId,
}

impl fmt::Debug for ProbeConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeConnection")
            .field("local", &self.local)
            .field("peer", &self.connection.remote_id())
            .finish_non_exhaustive()
    }
}

impl ProbeConnection {
    /// Authenticated coordinator identity.
    #[must_use]
    pub fn peer(&self) -> EndpointId {
        self.connection.remote_id()
    }
}

/// An authenticated artifact connection that has already passed ALPN and peer admission.
///
/// The Iroh connection is private so callers can only hand it to the matching artifact handler;
/// no generic connection or raw stream access escapes the transport crate.
pub struct ArtifactConnection {
    connection: Connection,
    local: EndpointId,
}

impl fmt::Debug for ArtifactConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactConnection")
            .field("local", &self.local)
            .field("peer", &self.connection.remote_id())
            .finish_non_exhaustive()
    }
}

impl ArtifactConnection {
    /// Authenticated remote endpoint identity.
    #[must_use]
    pub fn peer(&self) -> EndpointId {
        self.connection.remote_id()
    }

    /// Serve the one artifact request on this already accepted connection.
    pub async fn serve(self, state: &ServerState) -> Result<ServeMetrics, TransportError> {
        if state.policy.server != self.local {
            self.connection
                .close(1_u32.into(), b"artifact listener identity mismatch");
            return Err(TransportError::Rejected(RejectCode::ScopeMismatch));
        }
        if self.connection.alpn() != ALPN {
            self.connection
                .close(1_u32.into(), b"unexpected artifact ALPN");
            return Err(TransportError::Frame("unexpected artifact ALPN".into()));
        }
        if !state.policy.allows_peer(self.connection.remote_id()) {
            self.connection.close(1_u32.into(), b"peer is not admitted");
            return Err(TransportError::Rejected(RejectCode::PeerNotAllowed));
        }
        serve_artifact_connection(self.connection, self.local, state).await
    }
}

/// A single bounded owner of `Endpoint::accept` for artifact and control ALPNs.
///
/// Spawn this instead of running separate `accept_control` and `serve_one` loops concurrently
/// on the same endpoint. The bounded channel applies backpressure before the next connection
/// is accepted. Dropping the listener requests shutdown; [`ClusterListener::shutdown`] also
/// waits for the dispatcher task to stop.
pub struct ClusterListener {
    receiver: mpsc::Receiver<Result<AcceptedClusterConnection, TransportError>>,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl fmt::Debug for ClusterListener {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClusterListener")
            .field("queued", &self.receiver.len())
            .finish_non_exhaustive()
    }
}

impl ClusterListener {
    /// Start one bounded ALPN dispatcher for an endpoint and its two protocol policies.
    ///
    /// `capacity` must be in `1..=64`. The endpoint must be a direct-only endpoint created by
    /// [`bind_direct`], and its local identity must match both supplied policies.
    pub fn spawn(
        endpoint: Endpoint,
        control_policy: ControlAdmissionPolicy,
        artifact_state: ServerState,
        capacity: usize,
    ) -> Result<Self, TransportError> {
        Self::spawn_inner(
            endpoint,
            control_policy,
            artifact_state,
            None,
            capacity,
            control::CONTROL_ACCEPT_STREAM_TIMEOUT,
        )
    }

    /// Start the shared bounded dispatcher with authenticated probe ingress enabled.
    ///
    /// The probe policy's coordinator allowlist is checked before a connection is returned.
    /// The worker still validates the exact live assignment and page scope before computing
    /// any Have response or opening CAS objects.
    pub fn spawn_with_probe(
        endpoint: Endpoint,
        control_policy: ControlAdmissionPolicy,
        artifact_state: ServerState,
        probe_policy: ProbeAdmissionPolicy,
        capacity: usize,
    ) -> Result<Self, TransportError> {
        Self::spawn_inner(
            endpoint,
            control_policy,
            artifact_state,
            Some(probe_policy),
            capacity,
            control::CONTROL_ACCEPT_STREAM_TIMEOUT,
        )
    }

    /// Start one owner-side dispatcher for all currently registered assignment routes.
    ///
    /// Unlike one listener per assignment, this owns the endpoint's single accept loop and
    /// admits artifacts only when their signed capability matches a live exact peer, attempt,
    /// and input closure in `routes`. Control ingress is peer-allowlisted from the same registry;
    /// callers must demultiplex the first worker frame by its exact assignment scope.
    pub fn spawn_owner_router(
        endpoint: Endpoint,
        owner: EndpointId,
        routes: OwnerClusterAdmissionRegistry,
        objects: Arc<dyn BlobCatalog>,
        capacity: usize,
    ) -> Result<(Self, ServerState), TransportError> {
        let control_policy = ControlAdmissionPolicy::coordinator_router(owner, routes.clone());
        let artifact_policy = AdmissionPolicy::owner_router(owner, owner, routes);
        let artifact_state = ServerState::new(artifact_policy, objects);
        let listener = Self::spawn(endpoint, control_policy, artifact_state.clone(), capacity)?;
        Ok((listener, artifact_state))
    }

    fn spawn_inner(
        endpoint: Endpoint,
        control_policy: ControlAdmissionPolicy,
        artifact_state: ServerState,
        probe_policy: Option<ProbeAdmissionPolicy>,
        capacity: usize,
        control_stream_timeout: Duration,
    ) -> Result<Self, TransportError> {
        if capacity == 0 || capacity > MAX_CLUSTER_ACCEPT_QUEUE {
            return Err(TransportError::Frame(
                "cluster listener capacity must be in 1..=64".into(),
            ));
        }
        let local = endpoint.id();
        if control_policy
            .scope
            .is_some_and(|scope| scope.validate().is_err())
        {
            return Err(TransportError::InvalidScope);
        }
        if control_policy.server != local
            || artifact_state.policy.server != local
            || probe_policy
                .as_ref()
                .is_some_and(|policy| policy.server != local)
        {
            return Err(TransportError::ControlRejected(
                ControlRejectCode::ListenerMismatch,
            ));
        }
        let artifact_policy = artifact_state.policy.clone();
        let probe_peers = probe_policy.map(|policy| policy.allowed_coordinators);
        let (sender, receiver) = mpsc::channel(capacity);
        let (shutdown, mut stop) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut in_flight = tokio::task::JoinSet::new();
            let in_flight_limit = capacity.min(MAX_CLUSTER_ADMISSION_IN_FLIGHT);
            loop {
                if in_flight.len() >= in_flight_limit {
                    let joined = tokio::select! {
                        _ = &mut stop => break,
                        joined = in_flight.join_next() => joined,
                    };
                    publish_cluster_admission(joined);
                    continue;
                }

                let permit = loop {
                    let has_in_flight = !in_flight.is_empty();
                    tokio::select! {
                        _ = &mut stop => return,
                        joined = in_flight.join_next(), if has_in_flight => {
                            publish_cluster_admission(joined);
                        }
                        permit = sender.clone().reserve_owned() => match permit {
                            Ok(permit) => break permit,
                            Err(_) => return,
                        },
                    }
                };

                let has_in_flight = !in_flight.is_empty();
                let incoming = tokio::select! {
                    _ = &mut stop => break,
                    joined = in_flight.join_next(), if has_in_flight => {
                        publish_cluster_admission(joined);
                        continue;
                    }
                    incoming = endpoint.accept() => match incoming {
                        Some(incoming) => incoming,
                        None => break,
                    },
                };

                let task_control_policy = control_policy.clone();
                let task_artifact_policy = artifact_policy.clone();
                let task_probe_peers = probe_peers.clone();
                in_flight.spawn(async move {
                    let accepted = match incoming.accept() {
                        Ok(connecting) => connecting
                            .await
                            .map_err(|error| TransportError::Iroh(error.to_string())),
                        Err(error) => Err(TransportError::Iroh(error.to_string())),
                    };
                    let event = match accepted {
                        Err(error) => Err(error),
                        Ok(connection) if connection.alpn() == CONTROL_ALPN => {
                            control::accept_control_connection_with_timeout(
                                local,
                                connection,
                                &task_control_policy,
                                control_stream_timeout,
                            )
                            .await
                            .map(AcceptedClusterConnection::Control)
                        }
                        Ok(connection) if connection.alpn() == ALPN => {
                            if !task_artifact_policy.allows_peer(connection.remote_id()) {
                                connection.close(1_u32.into(), b"peer is not admitted");
                                Err(TransportError::Rejected(RejectCode::PeerNotAllowed))
                            } else {
                                Ok(AcceptedClusterConnection::Artifact(ArtifactConnection {
                                    connection,
                                    local,
                                }))
                            }
                        }
                        Ok(connection) if connection.alpn() == PROBE_ALPN => {
                            if !task_probe_peers
                                .as_ref()
                                .is_some_and(|peers| peers.contains(&connection.remote_id()))
                            {
                                connection.close(1_u32.into(), b"probe peer is not admitted");
                                Err(TransportError::Rejected(RejectCode::PeerNotAllowed))
                            } else {
                                Ok(AcceptedClusterConnection::Probe(ProbeConnection {
                                    connection,
                                    local,
                                }))
                            }
                        }
                        Ok(connection) => {
                            connection.close(1_u32.into(), b"unsupported cluster ALPN");
                            Err(TransportError::ControlRejected(
                                ControlRejectCode::PeerOrAlpnMismatch,
                            ))
                        }
                    };
                    (permit, event)
                });
            }
        });
        Ok(Self {
            receiver,
            shutdown: Some(shutdown),
            task: Some(task),
        })
    }

    /// Receive the next authenticated control or artifact connection, or `None` after shutdown.
    pub async fn recv(&mut self) -> Option<Result<AcceptedClusterConnection, TransportError>> {
        self.receiver.recv().await
    }

    /// Stop accepting connections and wait boundedly for the dispatcher task to exit.
    pub async fn shutdown(mut self) -> Result<(), TransportError> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let Some(mut task) = self.task.take() else {
            return Ok(());
        };
        match tokio::time::timeout(CLUSTER_LISTENER_SHUTDOWN_TIMEOUT, &mut task).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(error)) => Err(TransportError::Iroh(error.to_string())),
            Err(_) => {
                task.abort();
                let _ = task.await;
                Err(TransportError::Iroh(
                    "cluster listener shutdown timed out".into(),
                ))
            }
        }
    }
}

impl Drop for ClusterListener {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

/// Serve one capability-scoped Bao range on an already bound endpoint.
///
/// The authenticated Iroh peer ID is allowlisted before a stream is accepted. The signed
/// scope, object mapping, exact fence, range, expiry, and byte budget are checked before
/// catalog lookup or an availability response.
pub async fn serve_one(endpoint: &Endpoint, state: &ServerState) -> Result<(), TransportError> {
    serve_one_measured(endpoint, state).await.map(|_| ())
}

/// Serve one capability-scoped range and report Bao application-stream bytes.
///
/// This is not a QUIC wire-byte metric: transport, IP, and control-frame overhead are excluded.
pub async fn serve_one_measured(
    endpoint: &Endpoint,
    state: &ServerState,
) -> Result<ServeMetrics, TransportError> {
    let incoming = endpoint
        .accept()
        .await
        .ok_or_else(|| TransportError::Iroh("endpoint closed".into()))?;
    let accepted = incoming
        .accept()
        .map_err(|error| TransportError::Iroh(error.to_string()))?
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    serve_artifact_connection(accepted, endpoint.id(), state).await
}

async fn serve_artifact_connection(
    accepted: Connection,
    local: EndpointId,
    state: &ServerState,
) -> Result<ServeMetrics, TransportError> {
    if local != state.policy.server {
        accepted.close(1_u32.into(), b"artifact listener identity mismatch");
        return Err(TransportError::Rejected(RejectCode::ScopeMismatch));
    }
    if accepted.alpn() != ALPN {
        accepted.close(1_u32.into(), b"unexpected artifact ALPN");
        return Err(TransportError::Iroh(
            "unexpected ALPN for artifact listener".into(),
        ));
    }
    let peer = accepted.remote_id();
    if !state.policy.allowed_peers.contains(&peer) {
        accepted.close(1_u32.into(), b"peer is not admitted");
        return Err(TransportError::Rejected(RejectCode::PeerNotAllowed));
    }

    let (mut send, mut recv) = accepted
        .accept_bi()
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    let request: RangeRequest = read_frame(&mut recv).await?;
    if let Err(code) = verify_admission(
        &state.policy,
        peer,
        &request.capability,
        request.range,
        now_unix_ms()?,
    ) {
        write_frame(&mut send, &WireReply::Rejected(code)).await?;
        send.finish()
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        return Err(TransportError::Rejected(code));
    }

    // The content catalog is not consulted until peer and capability admission succeed.
    let Some(object) = state.objects.open(&request.capability.claims)? else {
        write_frame(
            &mut send,
            &WireReply::Rejected(RejectCode::ObjectUnavailable),
        )
        .await?;
        send.finish()
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        return Err(TransportError::Rejected(RejectCode::ObjectUnavailable));
    };
    let claims = &request.capability.claims;
    let tree = BaoTree::new(claims.object.payload_length, BAO_BLOCK_SIZE);
    if object.blob_hash != claims.blob_hash
        || object.data.len() != claims.object.payload_length
        || object.outboard.len() != tree.outboard_size()
    {
        write_frame(
            &mut send,
            &WireReply::Rejected(RejectCode::ObjectUnavailable),
        )
        .await?;
        send.finish()
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        return Err(TransportError::ObjectUnavailable);
    }

    write_frame(&mut send, &WireReply::Accepted).await?;
    let ranges = ChunkRanges::from(ChunkNum(request.range.start)..ChunkNum(request.range.end));
    let outboard = SourceOutboard {
        root: blake3::Hash::from(claims.blob_hash.0),
        tree,
        data: object.outboard,
    };
    let bao_stream_bytes = if claims.object.payload_length == 0 {
        // There is no Bao chunk or proof to serialize for an empty tree. The capability's
        // exact typed ID, empty BLAKE3 root, and 0..0 range are checked before this branch.
        send.finish()
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        0
    } else {
        let mut writer = BudgetWriter {
            inner: TokioStreamWriter(send),
            remaining: claims.byte_budget as usize,
            written: 0,
        };
        encode_ranges_validated(object.data, outboard, &ranges, &mut writer)
            .await
            .map_err(|error| TransportError::Bao(error.to_string()))?;
        let bao_stream_bytes = writer.written as u64;
        writer
            .inner
            .0
            .finish()
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        bao_stream_bytes
    };
    // Keep the authenticated connection alive until the requester has consumed and
    // durably checkpointed the verified range. Dropping the last Iroh Connection handle
    // after `finish()` implicitly closes QUIC and can race the receiver's final read.
    let acknowledgement =
        tokio::time::timeout(RANGE_ACK_TIMEOUT, read_frame::<RangeAck>(&mut recv))
            .await
            .map_err(|_| TransportError::Iroh("range acknowledgement timed out".into()))??;
    if acknowledgement
        != (RangeAck {
            scope: claims.scope,
            nonce: claims.nonce,
            range: request.range,
        })
    {
        return Err(TransportError::Rejected(RejectCode::ScopeMismatch));
    }
    let mut receipt = accepted
        .open_uni()
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    write_frame(
        &mut receipt,
        &RangeReceipt {
            scope: claims.scope,
            nonce: claims.nonce,
            range: request.range,
        },
    )
    .await?;
    receipt
        .finish()
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    // The client drops this one-request connection only after consuming RangeReceipt.
    // Waiting for its close keeps QUIC alive until the server confirmation was received.
    tokio::time::timeout(RANGE_ACK_TIMEOUT, accepted.closed())
        .await
        .map_err(|_| TransportError::Iroh("range confirmation close timed out".into()))?;
    Ok(ServeMetrics { bao_stream_bytes })
}

/// Fetch one signed Bao range into sparse durable files and atomically update a checkpoint.
pub async fn fetch_range(
    endpoint: &Endpoint,
    server_addr: EndpointAddr,
    trusted_issuer: EndpointId,
    capability: Capability,
    expected_scope: TransferScope,
    checkpoint_path: impl AsRef<Path>,
) -> Result<ResumeState, TransportError> {
    let mut verified = VerifiedCoverage::open(
        endpoint,
        &server_addr,
        trusted_issuer,
        &capability,
        expected_scope,
        checkpoint_path,
    )
    .await?;
    verified
        .fetch_range(endpoint, server_addr, capability)
        .await?;
    Ok(verified.finish())
}

fn validate_next_range(state: &ResumeState, range: ChunkRange) -> Result<(), TransportError> {
    if state.object.payload_length == 0 {
        if range == (ChunkRange { start: 0, end: 0 })
            && (state.covered.is_empty()
                || (state.covered.len() == 1
                    && state.covered[0] == (ChunkRange { start: 0, end: 0 })))
        {
            return Ok(());
        }
        return Err(TransportError::Rejected(RejectCode::InvalidRange));
    }
    if range.start != state.first_missing_chunk()
        || state
            .covered
            .iter()
            .any(|covered| range.start < covered.end && range.end > covered.start)
    {
        return Err(TransportError::Rejected(RejectCode::InvalidRange));
    }
    Ok(())
}

/// Verify peer admission and the exact signed range request before object lookup.
pub fn verify_admission(
    policy: &AdmissionPolicy,
    peer: EndpointId,
    capability: &Capability,
    range: ChunkRange,
    now_ms: u64,
) -> Result<(), RejectCode> {
    if !policy.allows_peer(peer) {
        return Err(RejectCode::PeerNotAllowed);
    }
    let claims = &capability.claims;
    let message = postcard::to_allocvec(claims).map_err(|_| RejectCode::InvalidCapability)?;
    if policy
        .issuer
        .verify(&message, &capability.signature)
        .is_err()
        || claims.expires_at_unix_ms <= now_ms
    {
        return Err(RejectCode::InvalidCapability);
    }
    validate_claim_shape(claims)?;
    if claims.server != policy.server || claims.client != peer {
        return Err(RejectCode::ScopeMismatch);
    }
    if !policy.allows_artifact(peer, claims.scope) {
        return Err(RejectCode::ScopeMismatch);
    }
    if range != claims.range {
        return Err(RejectCode::InvalidRange);
    }
    Ok(())
}

fn verify_client_capability(
    issuer: EndpointId,
    client: EndpointId,
    server: EndpointId,
    capability: &Capability,
    expected_scope: TransferScope,
    now_ms: u64,
) -> Result<(), TransportError> {
    let claims = &capability.claims;
    let message = postcard::to_allocvec(claims).map_err(frame_error)?;
    if issuer.verify(&message, &capability.signature).is_err()
        || claims.expires_at_unix_ms <= now_ms
    {
        return Err(TransportError::Rejected(RejectCode::InvalidCapability));
    }
    validate_claim_shape(claims).map_err(TransportError::Rejected)?;
    if claims.client != client || claims.server != server || claims.scope != expected_scope {
        return Err(TransportError::Rejected(RejectCode::ScopeMismatch));
    }
    Ok(())
}

fn validate_claim_shape(claims: &CapabilityClaims) -> Result<(), RejectCode> {
    if claims.scope.namespace_id == [0; 16]
        || claims.scope.work_id == [0; 16]
        || claims.scope.attempt == 0
        || claims.scope.fence == [0; 32]
        || claims.scope.closure_id == [0; 32]
        || claims.object.object_id == [0; 32]
        || claims.object.payload_length > MAX_OBJECT_BYTES
        || claims.blob_hash.0 == [0; 32]
    {
        return Err(RejectCode::InvalidCapability);
    }
    if claims.object.payload_length == 0
        && claims.blob_hash != BlobHash(*blake3::hash(&[]).as_bytes())
    {
        return Err(RejectCode::InvalidCapability);
    }
    claims.range.validate(claims.object.payload_length)?;
    if claims.byte_budget > MAX_RESPONSE_BYTES
        || (claims.object.payload_length > 0 && claims.byte_budget == 0)
    {
        return Err(RejectCode::InvalidBudget);
    }
    Ok(())
}

/// Incrementally hash a random-access source with bounded memory.
async fn hash_source<S: AsyncSliceReader + ?Sized>(
    source: &mut S,
    length: u64,
) -> Result<BlobHash, TransportError> {
    let mut hasher = blake3::Hasher::new();
    let mut offset = 0_u64;
    while offset < length {
        let take = usize::try_from((length - offset).min(IO_CHUNK_BYTES as u64))
            .map_err(|_| TransportError::ObjectTooLarge)?;
        let bytes = source.read_at(offset, take).await?;
        if bytes.len() != take {
            return Err(TransportError::Io(io::Error::from(
                io::ErrorKind::UnexpectedEof,
            )));
        }
        hasher.update(&bytes);
        offset += take as u64;
    }
    Ok(BlobHash(*hasher.finalize().as_bytes()))
}

#[derive(Debug, Serialize, Deserialize)]
struct RangeRequest {
    capability: Capability,
    range: ChunkRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct RangeAck {
    scope: TransferScope,
    nonce: [u8; 16],
    range: ChunkRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct RangeReceipt {
    scope: TransferScope,
    nonce: [u8; 16],
    range: ChunkRange,
}

#[derive(Debug, Serialize, Deserialize)]
enum WireReply {
    Accepted,
    Rejected(RejectCode),
}

#[derive(Debug)]
struct BudgetWriter<W> {
    inner: W,
    remaining: usize,
    written: usize,
}

impl<W: AsyncStreamWriter> AsyncStreamWriter for BudgetWriter<W> {
    async fn write(&mut self, data: &[u8]) -> io::Result<()> {
        self.reserve(data.len())?;
        self.inner.write(data).await
    }

    async fn write_bytes(&mut self, data: Bytes) -> io::Result<()> {
        self.reserve(data.len())?;
        self.inner.write_bytes(data).await
    }

    async fn sync(&mut self) -> io::Result<()> {
        self.inner.sync().await
    }
}

impl<W> BudgetWriter<W> {
    fn reserve(&mut self, amount: usize) -> io::Result<()> {
        if amount > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capability byte budget exceeded",
            ));
        }
        self.remaining -= amount;
        self.written = self.written.saturating_add(amount);
        Ok(())
    }
}

async fn write_frame<T: Serialize>(
    stream: &mut SendStream,
    value: &T,
) -> Result<(), TransportError> {
    let bytes = postcard::to_allocvec(value).map_err(frame_error)?;
    if bytes.len() > MAX_CONTROL_FRAME {
        return Err(TransportError::Frame("control frame exceeds limit".into()));
    }
    let len = u32::try_from(bytes.len())
        .map_err(|_| TransportError::Frame("control frame length overflow".into()))?;
    stream
        .write_all(&len.to_be_bytes())
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    Ok(())
}

async fn read_frame<T: DeserializeOwned>(stream: &mut RecvStream) -> Result<T, TransportError> {
    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len == 0 || len > MAX_CONTROL_FRAME {
        return Err(TransportError::Frame(
            "control frame length is invalid".into(),
        ));
    }
    let mut bytes = vec![0; len];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    postcard::from_bytes(&bytes).map_err(frame_error)
}

fn frame_error(error: impl fmt::Display) -> TransportError {
    TransportError::Frame(error.to_string())
}

fn store_error(error: backend_store::StoreError) -> TransportError {
    TransportError::Store(format!("{error:?}"))
}

fn create_sidecar(
    checkpoint: &Path,
    suffix: &str,
    length: u64,
    reset: bool,
) -> Result<(), TransportError> {
    let path = sidecar_path(checkpoint, suffix);
    if let Some(parent) = path.parent() {
        safe_ensure_directory(parent)?;
    }
    if reset {
        safe_remove_regular(&path)?;
    }
    drop(FileRangeSource::create_sparse(path, length)?);
    Ok(())
}

fn sidecar_path(checkpoint: &Path, suffix: &str) -> PathBuf {
    let mut name = checkpoint.as_os_str().to_os_string();
    name.push(format!(".{suffix}"));
    PathBuf::from(name)
}

fn checkpoint_temp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(name)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(DIGITS[(byte >> 4) as usize] as char);
        result.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(seed: u8) -> SecretKey {
        SecretKey::from_bytes(&[seed; 32])
    }

    fn claims_and_policy() -> (
        CapabilityIssuer,
        CapabilityClaims,
        AdmissionPolicy,
        EndpointId,
    ) {
        let issuer = CapabilityIssuer::new(secret(1));
        let server = secret(2).public();
        let client = secret(3).public();
        let scope =
            TransferScope::new([4; 16], [3; 16], 9, [5; 32], [6; 32]).expect("valid exact scope");
        let object = StoreObjectMapping {
            schema_domain: 8,
            schema_type: 10,
            schema_version: 1,
            key: [11; 32],
            version: [12; 32],
            payload_length: 4096,
            object_id: [13; 32],
        };
        let blob_hash = BlobHash([14; 32]);
        let claims = CapabilityClaims {
            client,
            server,
            scope,
            blob_hash,
            object,
            range: ChunkRange { start: 0, end: 4 },
            byte_budget: MAX_RESPONSE_BYTES,
            expires_at_unix_ms: now_unix_ms().expect("clock") + 60_000,
            nonce: [15; 16],
        };
        let policy = AdmissionPolicy::new(server, issuer.public_key(), [client], scope);
        (issuer, claims, policy, client)
    }

    #[tokio::test]
    async fn cluster_listener_bounds_control_admission_and_cleans_up_stream_timeouts() {
        let server = bind_direct(secret(71), "127.0.0.1:0".parse().expect("loopback socket"))
            .await
            .expect("server endpoint");
        let clients = [
            bind_direct(secret(72), "127.0.0.1:0".parse().expect("loopback socket"))
                .await
                .expect("slow client endpoint"),
            bind_direct(secret(73), "127.0.0.1:0".parse().expect("loopback socket"))
                .await
                .expect("fast client endpoint"),
        ];
        let scope = TransferScope::new([74; 16], [75; 16], 1, [76; 32], [77; 32])
            .expect("valid listener scope");
        let control_policy = ControlAdmissionPolicy::coordinator_ingress(
            server.id(),
            clients.iter().map(|client| client.id()),
        );
        let artifact_policy = AdmissionPolicy::new(
            server.id(),
            server.id(),
            clients.iter().map(|client| client.id()),
            scope,
        );
        let mut listener = ClusterListener::spawn_inner(
            server.clone(),
            control_policy,
            ServerState::new(artifact_policy, Arc::new(MemoryBlobCatalog::default())),
            None,
            MAX_CLUSTER_ADMISSION_IN_FLIGHT,
            Duration::from_secs(5),
        )
        .expect("bounded cluster listener");
        let server_address = server.addr();

        // This authenticated peer deliberately opens CONTROL_ALPN without a bidi stream.
        let slow_connection = clients[0]
            .connect(server_address.clone(), CONTROL_ALPN)
            .await
            .expect("slow peer control connection");

        // A second allowlisted peer can complete admission while the first waits.
        let mut fast_client_channel = connect_control(
            &clients[1],
            server_address.clone(),
            server.id(),
            scope.assignment(),
            ControlRole::Worker,
        )
        .await
        .expect("fast peer opens its control stream");
        fast_client_channel
            .send(&ControlMessage::Accept(ControlAccept {
                scope: scope.assignment(),
                accepted: true,
            }))
            .await
            .expect("fast peer sends its first control frame");
        let accepted = tokio::time::timeout(Duration::from_secs(2), listener.recv())
            .await
            .expect("fast peer admission was not head-of-line blocked")
            .expect("listener remains open")
            .expect("fast peer admitted");
        let AcceptedClusterConnection::Control(mut fast_channel) = accepted else {
            panic!("control ALPN was routed to the wrong protocol");
        };
        assert_eq!(fast_channel.peer(), clients[1].id());
        assert!(matches!(
            fast_channel.receive().await.expect("fast peer first frame"),
            ControlMessage::Accept(_)
        ));
        drop(fast_channel);
        drop(fast_client_channel);

        // Fill the explicit in-flight cap with established, allowlisted connections whose
        // peers have not opened streams. A further handshake cannot consume an unbounded task.
        let mut held_connections = vec![slow_connection];
        for index in 1..MAX_CLUSTER_ADMISSION_IN_FLIGHT {
            let client = &clients[index % clients.len()];
            held_connections.push(
                client
                    .connect(server_address.clone(), CONTROL_ALPN)
                    .await
                    .expect("held control connection reaches the admission cap"),
            );
        }
        assert_eq!(held_connections.len(), MAX_CLUSTER_ADMISSION_IN_FLIGHT);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                clients[1].connect(server_address.clone(), CONTROL_ALPN),
            )
            .await
            .is_err(),
            "listener accepted beyond its in-flight cap"
        );

        // The bounded stream-open deadline releases every held permit and connection. The
        // cap probe above is still queued at the endpoint when its client-side timeout drops
        // the handshake, so the listener also reports that one expected peer-aborted handshake
        // after a held admission frees capacity.
        let (stream_timeouts, cap_probe_handshake_closes) = tokio::time::timeout(
            Duration::from_secs(7),
            async {
                let mut stream_timeouts = 0;
                let mut cap_probe_handshake_closes = 0;
                for _ in 0..=MAX_CLUSTER_ADMISSION_IN_FLIGHT {
                    let event = listener.recv().await.expect("listener remains open");
                    match event {
                        Err(TransportError::Iroh(message)) => {
                            if message == "control stream open timed out" {
                                stream_timeouts += 1;
                            } else if message == "aborted by peer: the application or application protocol caused the connection to be closed during the handshake" {
                                cap_probe_handshake_closes += 1;
                            } else {
                                panic!("unexpected held admission error: {message}");
                            }
                        }
                        other => panic!("expected held-stream timeout, received {other:?}"),
                    }
                }
                (stream_timeouts, cap_probe_handshake_closes)
            },
        )
        .await
        .expect("held stream admissions did not time out within their bound");
        assert_eq!(stream_timeouts, MAX_CLUSTER_ADMISSION_IN_FLIGHT);
        assert_eq!(cap_probe_handshake_closes, 1);

        let mut recovered = connect_control(
            &clients[1],
            server_address,
            server.id(),
            scope.assignment(),
            ControlRole::Worker,
        )
        .await
        .expect("listener accepts after timed-out admissions are cleaned up");
        recovered
            .send(&ControlMessage::Accept(ControlAccept {
                scope: scope.assignment(),
                accepted: true,
            }))
            .await
            .expect("recovered peer sends its first control frame");
        let accepted = tokio::time::timeout(Duration::from_secs(2), listener.recv())
            .await
            .expect("listener did not recover after timeout cleanup")
            .expect("listener remains open")
            .expect("recovered control peer admitted");
        let AcceptedClusterConnection::Control(mut recovered_channel) = accepted else {
            panic!("recovered control ALPN was routed to the wrong protocol");
        };
        assert_eq!(recovered_channel.peer(), clients[1].id());
        assert!(matches!(
            recovered_channel
                .receive()
                .await
                .expect("recovered peer first frame"),
            ControlMessage::Accept(_)
        ));
        drop(recovered_channel);
        drop(recovered);
        for connection in held_connections {
            connection.close(0_u32.into(), b"listener test complete");
        }
        listener
            .shutdown()
            .await
            .expect("listener shuts down cleanly");
    }

    #[tokio::test]
    async fn owner_router_interleaves_concurrent_assignment_scopes_without_crossing_peers() {
        let owner = bind_direct(secret(61), "127.0.0.1:0".parse().expect("loopback socket"))
            .await
            .expect("owner endpoint");
        let workers = [
            bind_direct(secret(62), "127.0.0.1:0".parse().expect("loopback socket"))
                .await
                .expect("first worker endpoint"),
            bind_direct(secret(63), "127.0.0.1:0".parse().expect("loopback socket"))
                .await
                .expect("second worker endpoint"),
        ];
        let transfer_scopes = [
            TransferScope::new([64; 16], [65; 16], 1, [66; 32], [67; 32])
                .expect("first exact input scope"),
            TransferScope::new([74; 16], [75; 16], 2, [76; 32], [77; 32])
                .expect("second exact input scope"),
        ];
        let routes = OwnerClusterAdmissionRegistry::default();
        let invalid_scope = TransferScope {
            namespace_id: [0; 16],
            ..transfer_scopes[0]
        };
        assert!(routes.register(workers[0].id(), invalid_scope).is_err());
        assert!(!routes.authorizes_control(workers[0].id(), invalid_scope.assignment()));
        assert!(!routes.authorizes_artifact(workers[0].id(), invalid_scope));
        for (worker, scope) in workers.iter().zip(transfer_scopes) {
            routes
                .register(worker.id(), scope)
                .expect("register exact owner route");
        }
        let (mut listener, _artifact_state) = ClusterListener::spawn_owner_router(
            owner.clone(),
            owner.id(),
            routes.clone(),
            Arc::new(MemoryBlobCatalog::default()),
            8,
        )
        .expect("single owner endpoint accept/demux listener");
        let owner_address = owner.addr();
        let owner_id = owner.id();
        let worker_tasks = workers
            .iter()
            .zip(transfer_scopes)
            .map(|(worker, transfer_scope)| {
                let worker = worker.clone();
                let owner_address = owner_address.clone();
                tokio::spawn(async move {
                    let mut channel = connect_control(
                        &worker,
                        owner_address,
                        owner_id,
                        transfer_scope.assignment(),
                        ControlRole::Worker,
                    )
                    .await
                    .expect("worker control connection");
                    channel
                        .send(&ControlMessage::ResultReceipt(ControlResultReceipt {
                            scope: transfer_scope.assignment(),
                            target_root: [81; 32],
                            pack_id: None,
                            closure_id: [82; 32],
                            object_count: 1,
                            payload_bytes: 1,
                            result_grant_pages: 1,
                        }))
                        .await
                        .expect("worker first frame");
                    channel
                        .finish()
                        .await
                        .expect("worker waits for owner scope demux");
                })
            })
            .collect::<Vec<_>>();
        let mut owner_channels = Vec::new();
        let mut observed = HashSet::new();
        for _ in 0..workers.len() {
            let event = tokio::time::timeout(Duration::from_secs(5), listener.recv())
                .await
                .expect("listener connection timeout")
                .expect("listener remains open")
                .expect("authenticated worker control connection");
            let AcceptedClusterConnection::Control(mut channel) = event else {
                panic!("owner router yielded a non-control event");
            };
            let first = channel.receive().await.expect("read first worker frame");
            let scope = channel.scope().expect("adopted exact assignment scope");
            assert!(matches!(first, ControlMessage::ResultReceipt(_)));
            assert!(routes.authorizes_control(channel.peer(), scope));
            assert!(observed.insert((channel.peer(), scope)));
            owner_channels.push(channel);
        }
        assert_eq!(observed.len(), workers.len());
        assert!(routes.authorizes_artifact(workers[0].id(), transfer_scopes[0]));
        assert!(!routes.authorizes_artifact(
            workers[0].id(),
            TransferScope {
                closure_id: [99; 32],
                ..transfer_scopes[0]
            }
        ));

        let first_owner = owner_channels.remove(0);
        let second_owner = owner_channels.remove(0);
        let mut worker_tasks = worker_tasks.into_iter();
        let first_worker_task = worker_tasks.next().expect("first worker task");
        let second_worker_task = worker_tasks.next().expect("second worker task");
        let (first_finish, second_finish, _first_worker, _second_worker) = tokio::join!(
            first_owner.finish(),
            second_owner.finish(),
            async {
                first_worker_task.await.expect("first worker completed");
            },
            async {
                second_worker_task.await.expect("second worker completed");
            },
        );
        first_finish.expect("finish first owner channel");
        second_finish.expect("finish second owner channel");
    }

    #[test]
    fn grants_bind_peer_work_exact_fence_and_store_closure() {
        let (issuer, claims, policy, client) = claims_and_policy();
        let capability = issuer.issue(claims.clone()).expect("grant");
        assert_eq!(
            verify_admission(
                &policy,
                client,
                &capability,
                claims.range,
                now_unix_ms().expect("clock")
            ),
            Ok(())
        );
        assert_eq!(
            verify_admission(
                &policy,
                secret(8).public(),
                &capability,
                claims.range,
                now_unix_ms().expect("clock")
            ),
            Err(RejectCode::PeerNotAllowed)
        );

        let wrong_fence = TransferScope {
            fence: [77; 32],
            ..claims.scope
        };
        let wrong_policy =
            AdmissionPolicy::new(policy.server, policy.issuer, [client], wrong_fence);
        assert_eq!(
            verify_admission(
                &wrong_policy,
                client,
                &capability,
                claims.range,
                now_unix_ms().expect("clock")
            ),
            Err(RejectCode::ScopeMismatch)
        );
        assert_eq!(
            verify_admission(
                &policy,
                client,
                &capability,
                ChunkRange { start: 4, end: 8 },
                now_unix_ms().expect("clock")
            ),
            Err(RejectCode::InvalidRange)
        );
    }

    #[test]
    fn zero_byte_capability_requires_the_exact_empty_blob_and_empty_range() {
        let (issuer, mut claims, policy, client) = claims_and_policy();
        claims.object.payload_length = 0;
        claims.blob_hash = BlobHash(*blake3::hash(&[]).as_bytes());
        claims.range = ChunkRange { start: 0, end: 0 };
        claims.byte_budget = 0;
        let capability = issuer.issue(claims.clone()).expect("signed empty range");
        assert_eq!(
            verify_admission(
                &policy,
                client,
                &capability,
                claims.range,
                now_unix_ms().unwrap()
            ),
            Ok(())
        );

        let mut nonempty_range = claims.clone();
        nonempty_range.range.end = 1;
        assert!(matches!(
            issuer.issue(nonempty_range),
            Err(TransportError::Rejected(RejectCode::InvalidRange))
        ));
        let mut wrong_hash = claims;
        wrong_hash.blob_hash = BlobHash([14; 32]);
        assert!(matches!(
            issuer.issue(wrong_hash),
            Err(TransportError::Rejected(RejectCode::InvalidCapability))
        ));
    }

    #[test]
    fn scope_rejects_unfenced_or_untyped_closure_claims() {
        assert!(TransferScope::new([1; 16], [2; 16], 1, [0; 32], [3; 32]).is_err());
        assert!(TransferScope::new([1; 16], [2; 16], 1, [2; 32], [0; 32]).is_err());
    }

    #[test]
    fn scope_wire_deserialization_rejects_zero_namespace_work_attempt_fence_and_closure() {
        let valid_assignment =
            AssignmentScope::new([1; 16], [2; 16], 3, [4; 32]).expect("valid test assignment");
        for invalid in [
            AssignmentScope {
                namespace_id: [0; 16],
                ..valid_assignment
            },
            AssignmentScope {
                work_id: [0; 16],
                ..valid_assignment
            },
            AssignmentScope {
                attempt: 0,
                ..valid_assignment
            },
            AssignmentScope {
                fence: [0; 32],
                ..valid_assignment
            },
        ] {
            assert!(invalid.validate().is_err());
            let wire = postcard::to_allocvec(&invalid).expect("serialize forged assignment");
            assert!(postcard::from_bytes::<AssignmentScope>(&wire).is_err());
        }

        let valid_transfer =
            TransferScope::new([5; 16], [6; 16], 7, [8; 32], [9; 32]).expect("valid test transfer");
        for invalid in [
            TransferScope {
                namespace_id: [0; 16],
                ..valid_transfer
            },
            TransferScope {
                work_id: [0; 16],
                ..valid_transfer
            },
            TransferScope {
                closure_id: [0; 32],
                ..valid_transfer
            },
        ] {
            assert!(invalid.validate().is_err());
            let wire = postcard::to_allocvec(&invalid).expect("serialize forged transfer");
            assert!(postcard::from_bytes::<TransferScope>(&wire).is_err());
        }
    }

    #[test]
    fn capability_claims_reject_zero_namespace_and_work_id() {
        let (issuer, claims, _, _) = claims_and_policy();
        let mut invalid_namespace = claims.clone();
        invalid_namespace.scope.namespace_id = [0; 16];
        assert!(matches!(
            issuer.issue(invalid_namespace),
            Err(TransportError::Rejected(RejectCode::InvalidCapability))
        ));

        let mut invalid_work = claims;
        invalid_work.scope.work_id = [0; 16];
        assert!(matches!(
            issuer.issue(invalid_work),
            Err(TransportError::Rejected(RejectCode::InvalidCapability))
        ));
    }

    #[tokio::test]
    async fn bao_decoder_rejects_truncated_and_reordered_proofs() {
        let bytes = Bytes::from(vec![39_u8; 8 * 1024]);
        let outboard = bao_tree::io::outboard::PreOrderMemOutboard::create(&bytes, BAO_BLOCK_SIZE);
        let requested = ChunkRanges::from(ChunkNum(0)..ChunkNum(4));
        let mut encoded = Vec::new();
        encode_ranges_validated(bytes.clone(), outboard.clone(), &requested, &mut encoded)
            .await
            .expect("encode valid range");

        let truncated = Bytes::copy_from_slice(&encoded[..encoded.len() - 1]);
        let mut target = Vec::new();
        let mut decoded_outboard = bao_tree::io::outboard::PreOrderMemOutboard {
            root: outboard.root,
            tree: outboard.tree,
            data: vec![0; outboard.data.len()],
        };
        assert!(
            decode_ranges(
                truncated,
                requested.clone(),
                &mut target,
                &mut decoded_outboard
            )
            .await
            .is_err()
        );

        let mut reordered_target = Vec::new();
        let mut reordered_outboard = bao_tree::io::outboard::PreOrderMemOutboard {
            root: outboard.root,
            tree: outboard.tree,
            data: vec![0; outboard.data.len()],
        };
        let wrong_ranges = ChunkRanges::from(ChunkNum(4)..ChunkNum(8));
        assert!(
            decode_ranges(
                Bytes::from(encoded),
                wrong_ranges,
                &mut reordered_target,
                &mut reordered_outboard
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn one_mebibyte_range_fits_the_explicit_bao_response_ceiling() {
        let length = usize::try_from(MAX_RANGE_CHUNKS * 1_024).expect("bounded test size");
        let payload = Bytes::from(vec![0x5A; length]);
        let outboard =
            bao_tree::io::outboard::PreOrderMemOutboard::create(&payload, BAO_BLOCK_SIZE);
        let range = ChunkRange {
            start: 0,
            end: MAX_RANGE_CHUNKS,
        };
        assert_eq!(range.validate(length as u64), Ok(()));
        assert_eq!(
            ChunkRange {
                start: 0,
                end: MAX_RANGE_CHUNKS + 1,
            }
            .validate(length as u64 + 1_024),
            Err(RejectCode::InvalidRange)
        );

        let mut encoded = Vec::new();
        encode_ranges_validated(
            payload,
            outboard,
            &ChunkRanges::from(ChunkNum(range.start)..ChunkNum(range.end)),
            &mut encoded,
        )
        .await
        .expect("encode max-sized contiguous range");
        assert!(encoded.len() <= MAX_RESPONSE_BYTES as usize);
    }

    #[tokio::test]
    async fn cold_resume_reverifies_covered_payload_before_trusting_it() {
        let bytes = Bytes::from((0..8 * 1024).map(|index| index as u8).collect::<Vec<_>>());
        let outboard = bao_tree::io::outboard::PreOrderMemOutboard::create(&bytes, BAO_BLOCK_SIZE);
        let scope = TransferScope::new([31; 16], [32; 16], 4, [33; 32], [34; 32])
            .expect("exact resume scope");
        let claims = CapabilityClaims {
            client: secret(35).public(),
            server: secret(36).public(),
            scope,
            blob_hash: BlobHash(*blake3::hash(&bytes).as_bytes()),
            object: StoreObjectMapping {
                schema_domain: 1,
                schema_type: 1,
                schema_version: 1,
                key: [37; 32],
                version: [38; 32],
                payload_length: bytes.len() as u64,
                object_id: [39; 32],
            },
            range: ChunkRange { start: 0, end: 8 },
            byte_budget: MAX_RESPONSE_BYTES,
            expires_at_unix_ms: now_unix_ms().expect("clock") + 60_000,
            nonce: [40; 16],
        };
        let directory = test_temp_dir("resume-tamper");
        let checkpoint = directory.join("resume.chk");
        let mut state = ResumeState::empty(&checkpoint, &claims).expect("sparse checkpoint");

        let mut data = FileRangeSource::create_sparse(
            ResumeState::data_path(&checkpoint),
            claims.object.payload_length,
        )
        .expect("open data sidecar");
        data.write_sync(0, &bytes).expect("write fixture payload");
        data.sync_all().expect("sync payload");
        let mut proof = FileRangeSource::create_sparse(
            ResumeState::outboard_path(&checkpoint),
            outboard.data.len() as u64,
        )
        .expect("open outboard sidecar");
        proof
            .write_sync(0, &outboard.data)
            .expect("write fixture outboard");
        proof.sync_all().expect("sync outboard");
        state.add_coverage(claims.range);
        state
            .save_atomic(&checkpoint)
            .expect("persist covered range");

        let cold = ResumeState::load(&checkpoint, &claims).expect("valid checkpoint metadata");
        let mut tampered = FileRangeSource::create_sparse(
            ResumeState::data_path(&checkpoint),
            claims.object.payload_length,
        )
        .expect("reopen sparse data");
        tampered
            .write_sync(0, &[0xFF])
            .expect("tamper persisted data");
        tampered.sync_all().expect("sync tampering");
        assert!(cold.verify_covered_ranges(&checkpoint).await.is_err());
        std::fs::remove_dir_all(directory).expect("clean resume fixture");
    }

    #[tokio::test]
    async fn zero_byte_checkpoint_is_incomplete_until_verified_then_rejects_nonempty_mutation() {
        let (issuer, mut claims, _, _) = claims_and_policy();
        claims.object.payload_length = 0;
        claims.blob_hash = BlobHash(*blake3::hash(&[]).as_bytes());
        claims.range = ChunkRange { start: 0, end: 0 };
        claims.byte_budget = 0;
        let _capability = issuer.issue(claims.clone()).expect("empty object grant");
        let directory = test_temp_dir("empty-checkpoint");
        let checkpoint = directory.join("empty.chk");
        let mut state = ResumeState::empty(&checkpoint, &claims).expect("empty sparse state");
        assert!(!state.is_complete());
        assert!(matches!(
            state.verify_complete_payload(&checkpoint).await,
            Err(TransportError::IncompleteTransfer)
        ));
        state.add_coverage(claims.range);
        assert!(state.is_complete());
        state
            .save_atomic(&checkpoint)
            .expect("save verified empty range");
        state
            .verify_complete_payload(&checkpoint)
            .await
            .expect("empty BLAKE3 payload verifies");

        let cold = ResumeState::load(&checkpoint, &claims).expect("cold load empty checkpoint");
        cold.verify_covered_ranges(&checkpoint)
            .await
            .expect("cold revalidate empty coverage");
        cold.verify_complete_payload(&checkpoint)
            .await
            .expect("cold empty payload hash");

        let data_path = ResumeState::data_path(&checkpoint);
        let mut nonempty_mutation = std::fs::OpenOptions::new()
            .write(true)
            .open(&data_path)
            .expect("open empty payload sidecar for adversarial mutation");
        nonempty_mutation.set_len(1).expect("make sidecar nonempty");
        nonempty_mutation
            .seek(SeekFrom::Start(0))
            .expect("seek in mutated sidecar");
        nonempty_mutation
            .write_all(b"x")
            .expect("write mistaken nonempty byte");
        assert!(matches!(
            cold.verify_complete_payload(&checkpoint).await,
            Err(TransportError::CheckpointInvalid)
        ));
        std::fs::remove_dir_all(directory).expect("clean empty checkpoint fixture");
    }

    #[cfg(unix)]
    #[test]
    fn sparse_file_helpers_reject_symlinks_without_touching_the_target() {
        use std::os::unix::fs::symlink;

        let directory = test_temp_dir("symlink");
        let target = directory.join("target");
        let alias = directory.join("alias");
        std::fs::write(&target, b"keep this target unchanged").expect("write target");
        symlink(&target, &alias).expect("create test symlink");

        assert!(FileRangeSource::create_sparse(&alias, 1).is_err());
        assert!(FileRangeSource::open_readonly(&alias).is_err());
        assert_eq!(
            std::fs::read(&target).expect("target unchanged"),
            b"keep this target unchanged"
        );
        std::fs::remove_dir_all(directory).expect("clean symlink fixture");
    }

    fn test_temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "backend-cluster-transport-{label}-{}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&path).expect("create test directory");
        path
    }
}

//! Authenticated live compiler preflight and object-level Have proofs.
//!
//! Each probe is tied to one owner-minted assignment attempt and streams a sorted closure
//! inventory in bounded pages. A worker reports Have bits only after checking the exact typed
//! object identity in durable CAS. The owner derives missing objects and bytes from its opened
//! closure index. These precompile facts contain no output-coverage claim.

use std::{
    fmt,
    time::{Duration, Instant},
};

use iroh::{EndpointAddr, endpoint::Connection};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{AssignmentScope, Endpoint, EndpointId, TransportError, now_unix_ms};

/// Dedicated encrypted Iroh ALPN for compiler preflight probes.
pub const PROBE_ALPN: &[u8] = b"/backend/cluster-probe/1";
/// Maximum ObjectIds carried in one challenge page.
pub const MAX_PROBE_OBJECTS_PER_PAGE: usize = 4_096;
/// Maximum members in one full-workspace closure inventory.
pub const MAX_PROBE_OBJECTS: u32 = 2_000_001;
/// Maximum pages exchanged over one bounded probe stream.
pub const MAX_PROBE_PAGES: u32 = 512;
/// Maximum encoded canonical package-target/unit descriptor.
pub const MAX_PROBE_TARGET_DESCRIPTOR_BYTES: usize = 4_096;
/// Maximum serialized challenge or response frame.
pub const MAX_PROBE_FRAME_BYTES: usize = 256 * 1024;
/// Maximum lifetime of a one-time probe challenge.
pub const MAX_PROBE_LIFETIME_MS: u64 = 60_000;
/// Maximum relative capacity lease reported by a worker.
pub const MAX_PROBE_CAPACITY_LEASE_MS: u32 = 30_000;
const FINISH_TIMEOUT: Duration = Duration::from_secs(30);

/// Exact portable invocation facts used to scope one live worker probe.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompilerProbeIdentity {
    /// Source-aware package lineage.
    pub package_lineage: [u8; 32],
    /// Exact canonical target ID. Workers resolve this through their configured
    /// portable capability registry; the probe never serializes host identity.
    pub target: [u8; 32],
    /// Canonical CompilerPackageTargetV2 encoding, including package URL and native unit.
    /// The engine worker decodes it and recomputes target before reporting capability or Have.
    pub package_target: Vec<u8>,
    /// Canonical two-byte LanguageProfile code.
    pub profile: [u8; 2],
    /// Canonical Stage code: 0=Parse, 1=LowerIr.
    pub stage: u8,
    /// Exact toolchain, environment, and invocation recipe identity.
    pub recipe: [u8; 32],
    /// Exact captured workspace or input root.
    pub input_root: [u8; 32],
    /// Exact typed input manifest identity.
    pub read_manifest: [u8; 32],
    /// Exact backend-store input closure.
    pub input_closure_id: [u8; 32],
    /// Exact typed V2 input-manifest object.
    pub input_manifest_object_id: [u8; 32],
    /// Selected base, if the input authority admitted a proven incremental read set.
    pub selected_base: Option<[u8; 32]>,
    /// Maximum result bytes allowed by the owner.
    pub max_output_bytes: u64,
}

/// Owner-side CPU, memory, and transfer demand sent to the worker for admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompilerProbeDemand {
    /// Required CPU millicores.
    pub cpu_millicores: u32,
    /// Required memory bytes.
    pub memory_bytes: u64,
    /// Input bytes still to fetch plus the maximum result transfer allowance.
    pub transfer_bytes: u64,
}

/// Exact assignment challenge shared by every page in a probe stream.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompilerProbeSession {
    /// DB-minted namespace, work ID, attempt ordinal, and exact fence.
    pub scope: AssignmentScope,
    /// Owner-generated cryptographically random one-time challenge.
    pub nonce: [u8; 16],
    /// Absolute expiry checked by both endpoints.
    pub expires_at_unix_ms: u64,
    /// Exact compiler input and output budget.
    pub identity: CompilerProbeIdentity,
    /// Current job demand used for worker admission.
    pub demand: CompilerProbeDemand,
    /// Optional portable session key; the worker answers only whether this key is warm.
    pub session_affinity: Option<[u8; 32]>,
}

impl CompilerProbeSession {
    /// Validates exact work facts and the short one-time challenge lifetime.
    pub fn validate(&self, now_unix_ms: u64) -> Result<(), TransportError> {
        let identity = &self.identity;
        if self.scope.namespace_id == [0; 16]
            || self.scope.work_id == [0; 16]
            || self.scope.attempt == 0
            || self.scope.fence == [0; 32]
            || self.nonce == [0; 16]
            || self.expires_at_unix_ms <= now_unix_ms
            || self.expires_at_unix_ms.saturating_sub(now_unix_ms) > MAX_PROBE_LIFETIME_MS
            || identity.package_lineage == [0; 32]
            || identity.target == [0; 32]
            || identity.package_target.is_empty()
            || identity.package_target.len() > MAX_PROBE_TARGET_DESCRIPTOR_BYTES
            || identity.stage > 1
            || identity.recipe == [0; 32]
            || identity.input_root == [0; 32]
            || identity.read_manifest == [0; 32]
            || identity.input_closure_id == [0; 32]
            || identity.input_manifest_object_id == [0; 32]
            || identity.selected_base.is_some_and(|base| base == [0; 32])
            || identity.max_output_bytes == 0
            || self.demand.cpu_millicores == 0
            || self.demand.memory_bytes == 0
            || self.demand.transfer_bytes == 0
            || self.session_affinity.is_some_and(|key| key == [0; 32])
        {
            return Err(probe_error(
                "probe assignment identity or expiry is invalid",
            ));
        }
        Ok(())
    }
}

/// One exact ObjectId and its canonical payload length from the opened owner closure index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProbeObjectClaim {
    /// Content-addressed typed backend-store object identity.
    pub object_id: [u8; 32],
    /// Exact canonical payload bytes; admitted objects are bounded to 1 GiB.
    pub payload_bytes: u32,
}

/// Inventory digest and totals independently derived from sorted closure members.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeInventoryDescriptor {
    /// Number of closure members, including manifest and page objects.
    pub object_count: u32,
    /// Sum of member payload lengths.
    pub payload_bytes: u64,
    /// Domain-separated digest over sorted ObjectIds and exact lengths.
    pub digest: [u8; 32],
}

/// Incremental bounded-memory hash of an owner-opened sorted closure inventory.
pub struct ProbeInventoryHasher {
    hasher: blake3::Hasher,
    object_count: u32,
    payload_bytes: u64,
    previous_object_id: Option<[u8; 32]>,
}

impl fmt::Debug for ProbeInventoryHasher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeInventoryHasher")
            .field("object_count", &self.object_count)
            .field("payload_bytes", &self.payload_bytes)
            .finish_non_exhaustive()
    }
}

impl Default for ProbeInventoryHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl ProbeInventoryHasher {
    /// Starts a canonical inventory digest.
    #[must_use]
    pub fn new() -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.compiler.cluster-probe.inventory.v1\0");
        Self {
            hasher,
            object_count: 0,
            payload_bytes: 0,
            previous_object_id: None,
        }
    }

    /// Adds one strictly sorted object claim without retaining its identity in memory.
    pub fn push(&mut self, object: ProbeObjectClaim) -> Result<(), TransportError> {
        if object.object_id == [0; 32]
            || u64::from(object.payload_bytes) > crate::MAX_OBJECT_BYTES
            || self
                .previous_object_id
                .is_some_and(|previous| previous >= object.object_id)
            || self.object_count >= MAX_PROBE_OBJECTS
        {
            return Err(probe_error(
                "probe inventory is not canonical or exceeds its bound",
            ));
        }
        self.object_count = self
            .object_count
            .checked_add(1)
            .ok_or_else(|| probe_error("probe inventory count overflow"))?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(u64::from(object.payload_bytes))
            .ok_or_else(|| probe_error("probe inventory byte count overflow"))?;
        self.hasher.update(&object.object_id);
        self.hasher.update(&object.payload_bytes.to_be_bytes());
        self.previous_object_id = Some(object.object_id);
        Ok(())
    }

    /// Finishes the inventory with a checked nonempty member count.
    pub fn finish(self) -> Result<ProbeInventoryDescriptor, TransportError> {
        if self.object_count == 0 || self.object_count > MAX_PROBE_OBJECTS {
            return Err(probe_error("probe inventory is empty or oversized"));
        }
        let mut hasher = self.hasher;
        hasher.update(&self.object_count.to_be_bytes());
        hasher.update(&self.payload_bytes.to_be_bytes());
        Ok(ProbeInventoryDescriptor {
            object_count: self.object_count,
            payload_bytes: self.payload_bytes,
            digest: *hasher.finalize().as_bytes(),
        })
    }
}

/// One bounded Have challenge page. Every page repeats the exact session and inventory root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompilerProbePage {
    /// Exact work and attempt challenge.
    pub session: CompilerProbeSession,
    /// Zero-based page within the complete sorted inventory.
    pub page_index: u32,
    /// Exact page count derived from total member count.
    pub page_count: u32,
    /// Total member count across all pages.
    pub total_object_count: u32,
    /// Total payload bytes across all pages.
    pub total_payload_bytes: u64,
    /// Digest from the owner’s complete closure-index inventory scan.
    pub inventory_digest: [u8; 32],
    /// Page-specific digest over its canonical ordered member list and challenge binding.
    pub page_digest: [u8; 32],
    /// Sorted closure members challenged on this page.
    pub objects: Vec<ProbeObjectClaim>,
}

impl CompilerProbePage {
    /// Creates one page from exact opened-closure members.
    pub fn new(
        session: CompilerProbeSession,
        inventory: ProbeInventoryDescriptor,
        page_index: u32,
        objects: Vec<ProbeObjectClaim>,
    ) -> Result<Self, TransportError> {
        let page_count = inventory_page_count(inventory.object_count)?;
        let mut page = Self {
            session,
            page_index,
            page_count,
            total_object_count: inventory.object_count,
            total_payload_bytes: inventory.payload_bytes,
            inventory_digest: inventory.digest,
            page_digest: [0; 32],
            objects,
        };
        page.validate_shape()?;
        page.page_digest = page.compute_digest()?;
        Ok(page)
    }

    /// Digest of the exact encoded request page, echoed by the worker.
    pub fn request_digest(&self) -> Result<[u8; 32], TransportError> {
        let bytes = postcard::to_allocvec(self).map_err(frame_error)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.compiler.cluster-probe.request.v1\0");
        hasher.update(&bytes);
        Ok(*hasher.finalize().as_bytes())
    }

    /// Validates identities, expiry, fixed page geometry, and the page digest.
    pub fn validate(&self, now_unix_ms: u64) -> Result<(), TransportError> {
        self.session.validate(now_unix_ms)?;
        self.validate_shape()?;
        if self.inventory_digest == [0; 32] || self.compute_digest()? != self.page_digest {
            return Err(probe_error(
                "probe page digest or inventory identity is invalid",
            ));
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<(), TransportError> {
        if self.total_object_count == 0 || self.total_object_count > MAX_PROBE_OBJECTS {
            return Err(probe_error("probe object count is outside protocol bounds"));
        }
        let expected_pages = inventory_page_count(self.total_object_count)?;
        if self.page_count != expected_pages
            || self.page_index >= self.page_count
            || self.objects.is_empty()
            || self.objects.len() > MAX_PROBE_OBJECTS_PER_PAGE
        {
            return Err(probe_error("probe page geometry is invalid"));
        }
        let offset = self
            .page_index
            .checked_mul(MAX_PROBE_OBJECTS_PER_PAGE as u32)
            .ok_or_else(|| probe_error("probe page index overflow"))?;
        let expected_len = self
            .total_object_count
            .saturating_sub(offset)
            .min(MAX_PROBE_OBJECTS_PER_PAGE as u32) as usize;
        if self.objects.len() != expected_len {
            return Err(probe_error(
                "probe page has a missing or extra object claim",
            ));
        }
        if self
            .objects
            .windows(2)
            .any(|pair| pair[0].object_id >= pair[1].object_id)
            || self.objects.iter().any(|object| {
                object.object_id == [0; 32]
                    || u64::from(object.payload_bytes) > crate::MAX_OBJECT_BYTES
            })
        {
            return Err(probe_error("probe page object claims are not canonical"));
        }
        let page_bytes = self.objects.iter().try_fold(0_u64, |total, object| {
            total.checked_add(u64::from(object.payload_bytes))
        });
        if page_bytes.is_none_or(|bytes| bytes > self.total_payload_bytes) {
            return Err(probe_error("probe page bytes exceed inventory total"));
        }
        Ok(())
    }

    fn compute_digest(&self) -> Result<[u8; 32], TransportError> {
        let session = postcard::to_allocvec(&self.session).map_err(frame_error)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.compiler.cluster-probe.page.v1\0");
        hasher.update(&session);
        hasher.update(&self.page_index.to_be_bytes());
        hasher.update(&self.page_count.to_be_bytes());
        hasher.update(&self.total_object_count.to_be_bytes());
        hasher.update(&self.total_payload_bytes.to_be_bytes());
        hasher.update(&self.inventory_digest);
        for object in &self.objects {
            hasher.update(&object.object_id);
            hasher.update(&object.payload_bytes.to_be_bytes());
        }
        Ok(*hasher.finalize().as_bytes())
    }
}

/// Closed reason a portable invocation capability is unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProbeCapabilityReject {
    /// Exact compiler/toolchain recipe is unavailable.
    UnsupportedRecipe,
    /// Exact target is unsupported by configured compiler capability.
    UnsupportedTarget,
    /// Owner policy denies this exact remote execution class.
    PolicyRejected,
}

/// Exact worker capability decision without host-specific fingerprints.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ProbeCapability {
    /// Configured portable capability profile accepts this exact work recipe.
    Supported { capability_digest: [u8; 32] },
    /// This worker cannot execute the exact requested recipe.
    Rejected(ProbeCapabilityReject),
}

/// Current resource credits reported by the worker at one bounded snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProbeResourceCredits {
    /// CPU millicores.
    pub cpu_millicores: u32,
    /// Memory bytes.
    pub memory_bytes: u64,
    /// Network and durable transfer bytes.
    pub transfer_bytes: u64,
}

/// Capability and capacity snapshot kept identical over all pages in one probe stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProbeWorkerSnapshot {
    /// Worker process incarnation; a restart fences prior observations.
    pub worker_incarnation: [u8; 32],
    /// Opaque revision for this exact capacity snapshot.
    pub capacity_revision: [u8; 16],
    /// Current configured resource limits.
    pub total: ProbeResourceCredits,
    /// Resources occupied outside this owner's scheduler.
    pub busy: ProbeResourceCredits,
    /// Portable compiler/toolchain compatibility for this exact recipe.
    pub capability: ProbeCapability,
    /// Whether the exact requested session-affinity key is already warm.
    pub session_affinity_warm: bool,
    /// Relative lease from the first response page, bounded by the receiver.
    pub capacity_lease_ms: u32,
}

impl ProbeWorkerSnapshot {
    /// Checks the bounded worker capacity/capability response before owner admission.
    pub fn validate(&self) -> Result<(), TransportError> {
        if self.worker_incarnation == [0; 32]
            || self.capacity_revision == [0; 16]
            || self.total.cpu_millicores == 0
            || self.total.memory_bytes == 0
            || self.total.transfer_bytes == 0
            || self.busy.cpu_millicores > self.total.cpu_millicores
            || self.busy.memory_bytes > self.total.memory_bytes
            || self.busy.transfer_bytes > self.total.transfer_bytes
            || self.capacity_lease_ms == 0
            || self.capacity_lease_ms > MAX_PROBE_CAPACITY_LEASE_MS
            || matches!(self.capability, ProbeCapability::Supported { capability_digest } if capability_digest == [0; 32])
        {
            return Err(probe_error("probe worker snapshot is invalid"));
        }
        Ok(())
    }

    /// Stable digest proving every page observed the same worker snapshot.
    pub fn digest(&self) -> Result<[u8; 32], TransportError> {
        self.validate()?;
        let bytes = postcard::to_allocvec(self).map_err(frame_error)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.compiler.cluster-probe.snapshot.v1\0");
        hasher.update(&bytes);
        Ok(*hasher.finalize().as_bytes())
    }
}

/// Exact bitmap response for one challenged inventory page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CompilerProbePageReply {
    /// Exact assignment attempt and fence.
    pub scope: AssignmentScope,
    /// One-time nonce from the owner challenge.
    pub nonce: [u8; 16],
    /// Exact challenge deadline echoed unchanged.
    pub expires_at_unix_ms: u64,
    /// Hash of exact request bytes, including ObjectIds and lengths.
    pub request_digest: [u8; 32],
    /// Whole-closure inventory digest.
    pub inventory_digest: [u8; 32],
    /// Exact page index and page count.
    pub page_index: u32,
    /// Exact total pages in the inventory.
    pub page_count: u32,
    /// Page-specific sorted ObjectId and length digest.
    pub page_digest: [u8; 32],
    /// Worker capability, incarnation, and current-credit observation.
    pub snapshot: ProbeWorkerSnapshot,
    /// One bit per challenged ObjectId, least-significant bit first.
    pub have_bitmap: Vec<u8>,
    /// Number of set Have bits, checked against the bitmap.
    pub have_count: u32,
    /// Payload bytes for set Have bits, checked against challenge lengths.
    pub have_payload_bytes: u64,
}

impl CompilerProbePageReply {
    /// Creates a reply from the worker's CAS-verified bitmap and current worker snapshot.
    pub fn new(
        request: &CompilerProbePage,
        snapshot: ProbeWorkerSnapshot,
        have_bitmap: Vec<u8>,
    ) -> Result<Self, TransportError> {
        snapshot.validate()?;
        let mut reply = Self {
            scope: request.session.scope,
            nonce: request.session.nonce,
            expires_at_unix_ms: request.session.expires_at_unix_ms,
            request_digest: request.request_digest()?,
            inventory_digest: request.inventory_digest,
            page_index: request.page_index,
            page_count: request.page_count,
            page_digest: request.page_digest,
            snapshot,
            have_bitmap,
            have_count: 0,
            have_payload_bytes: 0,
        };
        (reply.have_count, reply.have_payload_bytes) = reply.count_have(request)?;
        Ok(reply)
    }

    /// Checks exact challenge, expiry, page hashes, bitmap geometry, and Have byte totals.
    pub fn validate_for(
        &self,
        request: &CompilerProbePage,
        now_unix_ms: u64,
    ) -> Result<(), TransportError> {
        request.validate(now_unix_ms)?;
        self.snapshot.validate()?;
        if self.scope != request.session.scope
            || self.nonce != request.session.nonce
            || self.expires_at_unix_ms != request.session.expires_at_unix_ms
            || now_unix_ms >= self.expires_at_unix_ms
            || self.request_digest != request.request_digest()?
            || self.inventory_digest != request.inventory_digest
            || self.page_index != request.page_index
            || self.page_count != request.page_count
            || self.page_digest != request.page_digest
        {
            return Err(probe_error(
                "probe response is stale or belongs to another challenge",
            ));
        }
        if self.count_have(request)? != (self.have_count, self.have_payload_bytes) {
            return Err(probe_error("probe Have summary differs from its bitmap"));
        }
        Ok(())
    }

    /// Returns missing ObjectIds and lengths derived from the exact owner challenge.
    pub fn missing_objects(
        &self,
        request: &CompilerProbePage,
        now_unix_ms: u64,
    ) -> Result<Vec<ProbeObjectClaim>, TransportError> {
        self.validate_for(request, now_unix_ms)?;
        let mut missing = Vec::new();
        missing
            .try_reserve_exact(
                request
                    .objects
                    .len()
                    .saturating_sub(self.have_count as usize),
            )
            .map_err(|_| probe_error("probe missing-list allocation failed"))?;
        for (index, object) in request.objects.iter().enumerate() {
            if !bitmap_has(&self.have_bitmap, index) {
                missing.push(*object);
            }
        }
        Ok(missing)
    }

    fn count_have(&self, request: &CompilerProbePage) -> Result<(u32, u64), TransportError> {
        let expected_bitmap_bytes = request.objects.len().div_ceil(8);
        if self.have_bitmap.len() != expected_bitmap_bytes {
            return Err(probe_error("probe Have bitmap has invalid length"));
        }
        let used_last = request.objects.len() % 8;
        if used_last != 0 {
            let valid_mask = (1_u8 << used_last) - 1;
            if self
                .have_bitmap
                .last()
                .is_some_and(|byte| byte & !valid_mask != 0)
            {
                return Err(probe_error("probe Have bitmap has nonzero padding bits"));
            }
        }
        let mut count = 0_u32;
        let mut bytes = 0_u64;
        for (index, object) in request.objects.iter().enumerate() {
            if bitmap_has(&self.have_bitmap, index) {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| probe_error("probe Have count overflow"))?;
                bytes = bytes
                    .checked_add(u64::from(object.payload_bytes))
                    .ok_or_else(|| probe_error("probe Have payload sum overflow"))?;
            }
        }
        Ok((count, bytes))
    }
}

/// Authenticated channel role for the precompile probe.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeRole {
    /// Index owner challenges exact closure members and receives Have/capacity facts.
    Coordinator,
    /// Worker checks CAS identities and replies with a bounded exact bitmap.
    Worker,
}

/// Worker endpoint identity and allowed coordinator set for probe admission.
#[derive(Clone, Debug)]
pub struct ProbeAdmissionPolicy {
    /// Worker endpoint serving probes.
    pub server: EndpointId,
    /// Trusted index-owner endpoints.
    pub allowed_coordinators: std::collections::HashSet<EndpointId>,
}

impl ProbeAdmissionPolicy {
    /// Creates an explicit worker-side coordinator allowlist.
    pub fn worker(
        server: EndpointId,
        allowed_coordinators: impl IntoIterator<Item = EndpointId>,
    ) -> Self {
        Self {
            server,
            allowed_coordinators: allowed_coordinators.into_iter().collect(),
        }
    }
}

/// One bounded encrypted stream for a complete live object-level Have probe.
pub struct ProbeChannel {
    connection: Connection,
    send: iroh::endpoint::SendStream,
    receive: iroh::endpoint::RecvStream,
    peer: EndpointId,
    role: ProbeRole,
    scope: Option<AssignmentScope>,
    session: Option<CompilerProbeSession>,
    inventory_descriptor: Option<ProbeInventoryDescriptor>,
    inventory_hasher: Option<ProbeInventoryHasher>,
    previous_object_id: Option<[u8; 32]>,
    next_page: u32,
    snapshot_digest: Option<[u8; 32]>,
    snapshot_started: Option<Instant>,
    sent_pages: u32,
    received_pages: u32,
    pending_page_digest: Option<[u8; 32]>,
}

impl fmt::Debug for ProbeChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProbeChannel")
            .field("peer", &self.peer)
            .field("scope", &self.scope)
            .field("role", &self.role)
            .field("next_page", &self.next_page)
            .finish_non_exhaustive()
    }
}

impl ProbeChannel {
    /// Authenticated remote worker identity.
    #[must_use]
    pub const fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Assignment scope learned from the first challenge or supplied at connect time.
    #[must_use]
    pub const fn scope(&self) -> Option<AssignmentScope> {
        self.scope
    }

    /// Local endpoint role on this channel.
    #[must_use]
    pub const fn role(&self) -> ProbeRole {
        self.role
    }

    /// Sends one owner challenge page.
    pub async fn send_challenge_page(
        &mut self,
        page: &CompilerProbePage,
    ) -> Result<(), TransportError> {
        if self.role != ProbeRole::Coordinator {
            return Err(probe_error("worker cannot send a probe challenge"));
        }
        if self.pending_page_digest.is_some() {
            return Err(probe_error("previous probe page has no Have response"));
        }
        page.validate(now_unix_ms()?)?;
        self.admit_page(page)?;
        write_probe_frame(&mut self.send, page).await?;
        self.pending_page_digest = Some(page.request_digest()?);
        self.sent_pages = self
            .sent_pages
            .checked_add(1)
            .ok_or_else(|| probe_error("probe message count overflow"))?;
        Ok(())
    }

    /// Receives one Have response at the owner and validates the exact challenge binding.
    pub async fn receive_have_page(
        &mut self,
        request: &CompilerProbePage,
    ) -> Result<CompilerProbePageReply, TransportError> {
        if self.role != ProbeRole::Coordinator {
            return Err(probe_error("worker cannot receive a probe response"));
        }
        let reply: CompilerProbePageReply = read_probe_frame(&mut self.receive).await?;
        if self.pending_page_digest != Some(reply.request_digest) {
            return Err(probe_error(
                "probe response does not answer the outstanding page",
            ));
        }
        reply.validate_for(request, now_unix_ms()?)?;
        self.admit_snapshot(&reply.snapshot)?;
        self.pending_page_digest = None;
        self.received_pages = self
            .received_pages
            .checked_add(1)
            .ok_or_else(|| probe_error("probe message count overflow"))?;
        Ok(reply)
    }

    /// Receives one exact object challenge page at the worker.
    pub async fn receive_challenge_page(&mut self) -> Result<CompilerProbePage, TransportError> {
        if self.role != ProbeRole::Worker {
            return Err(probe_error("coordinator cannot receive a worker challenge"));
        }
        if self.pending_page_digest.is_some() {
            return Err(probe_error("previous probe page has no Have response"));
        }
        let page: CompilerProbePage = read_probe_frame(&mut self.receive).await?;
        page.validate(now_unix_ms()?)?;
        self.admit_page(&page)?;
        self.pending_page_digest = Some(page.request_digest()?);
        self.received_pages = self
            .received_pages
            .checked_add(1)
            .ok_or_else(|| probe_error("probe message count overflow"))?;
        Ok(page)
    }

    /// Sends one Have response after the worker has checked each set bit against durable CAS.
    pub async fn send_have_page(
        &mut self,
        request: &CompilerProbePage,
        reply: &CompilerProbePageReply,
    ) -> Result<(), TransportError> {
        if self.role != ProbeRole::Worker {
            return Err(probe_error(
                "coordinator cannot send a worker Have response",
            ));
        }
        reply.validate_for(request, now_unix_ms()?)?;
        if self.session.as_ref() != Some(&request.session)
            || self.next_page != request.page_index + 1
            || self.pending_page_digest != Some(request.request_digest()?)
        {
            return Err(probe_error(
                "Have reply is outside the received probe sequence",
            ));
        }
        self.admit_snapshot(&reply.snapshot)?;
        write_probe_frame(&mut self.send, reply).await?;
        self.sent_pages = self
            .sent_pages
            .checked_add(1)
            .ok_or_else(|| probe_error("probe message count overflow"))?;
        self.pending_page_digest = None;
        Ok(())
    }

    /// Completes the stream after every declared inventory page has been exchanged.
    pub async fn finish(mut self) -> Result<(), TransportError> {
        let descriptor = self
            .inventory_descriptor
            .ok_or_else(|| probe_error("empty probe stream"))?;
        let page_count = inventory_page_count(descriptor.object_count)?;
        if self.next_page != page_count
            || self.sent_pages != page_count
            || self.received_pages != page_count
            || self.pending_page_digest.is_some()
        {
            return Err(probe_error(
                "probe stream ended before all inventory pages were exchanged",
            ));
        }
        self.send
            .finish()
            .map_err(|error| TransportError::Iroh(error.to_string()))?;
        let deadline = tokio::time::Instant::now() + FINISH_TIMEOUT;
        let peer_eof = async {
            let mut trailing = [0_u8; 1];
            match self.receive.read(&mut trailing).await {
                Ok(None) => Ok(()),
                Ok(Some(_)) => Err(probe_error("trailing bytes after probe completion")),
                Err(error) => Err(TransportError::Iroh(error.to_string())),
            }
        };
        tokio::time::timeout_at(deadline, peer_eof)
            .await
            .map_err(|_| probe_error("probe finish timed out"))??;
        match tokio::time::timeout_at(deadline, self.send.stopped()).await {
            Ok(Ok(None)) => {}
            Ok(Ok(Some(_))) => return Err(probe_error("peer aborted probe delivery")),
            Ok(Err(error)) => return Err(TransportError::Iroh(error.to_string())),
            Err(_) => return Err(probe_error("probe finish timed out")),
        }
        self.connection
            .close(0_u32.into(), b"compiler probe complete");
        Ok(())
    }

    fn admit_page(&mut self, page: &CompilerProbePage) -> Result<(), TransportError> {
        let descriptor = ProbeInventoryDescriptor {
            object_count: page.total_object_count,
            payload_bytes: page.total_payload_bytes,
            digest: page.inventory_digest,
        };
        if page.page_index != self.next_page
            || self.next_page >= MAX_PROBE_PAGES
            || self.scope.is_some_and(|scope| scope != page.session.scope)
            || self
                .session
                .as_ref()
                .is_some_and(|session| session != &page.session)
            || self
                .inventory_descriptor
                .is_some_and(|expected| expected != descriptor)
            || self
                .previous_object_id
                .zip(page.objects.first().map(|object| object.object_id))
                .is_some_and(|(previous, next)| previous >= next)
        {
            return Err(probe_error(
                "probe page order, assignment, or inventory changed",
            ));
        }
        let mut hasher = self
            .inventory_hasher
            .take()
            .unwrap_or_else(ProbeInventoryHasher::new);
        for object in &page.objects {
            hasher.push(*object)?;
        }
        if page.page_index + 1 == page.page_count {
            let computed = hasher.finish()?;
            if computed != descriptor {
                return Err(probe_error(
                    "probe pages do not match the complete inventory digest",
                ));
            }
        } else {
            self.inventory_hasher = Some(hasher);
        }
        self.previous_object_id = page.objects.last().map(|object| object.object_id);
        self.scope = Some(page.session.scope);
        self.session = Some(page.session.clone());
        self.inventory_descriptor = Some(descriptor);
        self.next_page += 1;
        Ok(())
    }

    fn admit_snapshot(&mut self, snapshot: &ProbeWorkerSnapshot) -> Result<(), TransportError> {
        let digest = snapshot.digest()?;
        if self
            .snapshot_digest
            .is_some_and(|existing| existing != digest)
        {
            return Err(probe_error(
                "worker capability or capacity changed during a probe",
            ));
        }
        if let Some(started) = self.snapshot_started {
            if started.elapsed() > Duration::from_millis(u64::from(snapshot.capacity_lease_ms)) {
                return Err(probe_error("worker capacity snapshot lease expired"));
            }
        } else {
            self.snapshot_started = Some(Instant::now());
        }
        self.snapshot_digest = Some(digest);
        Ok(())
    }
}

/// Opens an encrypted probe connection to one exact assigned worker endpoint.
pub async fn connect_probe(
    endpoint: &Endpoint,
    peer_addr: EndpointAddr,
    expected_peer: EndpointId,
    scope: AssignmentScope,
) -> Result<ProbeChannel, TransportError> {
    let connection = endpoint
        .connect(peer_addr, PROBE_ALPN)
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    if connection.remote_id() != expected_peer || connection.alpn() != PROBE_ALPN {
        connection.close(1_u32.into(), b"unexpected probe peer or ALPN");
        return Err(probe_error("probe peer identity or ALPN mismatch"));
    }
    let (send, receive) = connection
        .open_bi()
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    Ok(ProbeChannel {
        connection,
        send,
        receive,
        peer: expected_peer,
        role: ProbeRole::Coordinator,
        scope: Some(scope),
        session: None,
        inventory_descriptor: None,
        inventory_hasher: None,
        previous_object_id: None,
        next_page: 0,
        snapshot_digest: None,
        snapshot_started: None,
        sent_pages: 0,
        received_pages: 0,
        pending_page_digest: None,
    })
}

/// Accepts an authenticated probe connection handed off by ClusterListener.
pub async fn accept_probe_connection(
    accepted: crate::ProbeConnection,
    policy: &ProbeAdmissionPolicy,
) -> Result<ProbeChannel, TransportError> {
    let local = accepted.local;
    let connection = accepted.connection;
    if policy.server != local
        || connection.alpn() != PROBE_ALPN
        || !policy
            .allowed_coordinators
            .contains(&connection.remote_id())
    {
        connection.close(1_u32.into(), b"probe coordinator is not admitted");
        return Err(probe_error("probe peer or local endpoint is not admitted"));
    }
    let peer = connection.remote_id();
    let (send, receive) = connection
        .accept_bi()
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    Ok(ProbeChannel {
        connection,
        send,
        receive,
        peer,
        role: ProbeRole::Worker,
        scope: None,
        session: None,
        inventory_descriptor: None,
        inventory_hasher: None,
        previous_object_id: None,
        next_page: 0,
        snapshot_digest: None,
        snapshot_started: None,
        sent_pages: 0,
        received_pages: 0,
        pending_page_digest: None,
    })
}

/// Computes an inventory descriptor for a sorted member slice.
pub fn probe_inventory_descriptor(
    objects: &[ProbeObjectClaim],
) -> Result<ProbeInventoryDescriptor, TransportError> {
    let mut hasher = ProbeInventoryHasher::new();
    for object in objects {
        hasher.push(*object)?;
    }
    hasher.finish()
}

fn inventory_page_count(object_count: u32) -> Result<u32, TransportError> {
    if object_count == 0 || object_count > MAX_PROBE_OBJECTS {
        return Err(probe_error("probe object count is outside protocol bounds"));
    }
    let count = object_count.div_ceil(MAX_PROBE_OBJECTS_PER_PAGE as u32);
    if count == 0 || count > MAX_PROBE_PAGES {
        return Err(probe_error("probe page count is outside protocol bounds"));
    }
    Ok(count)
}

fn bitmap_has(bitmap: &[u8], index: usize) -> bool {
    bitmap[index / 8] & (1 << (index % 8)) != 0
}

async fn write_probe_frame<T: Serialize>(
    stream: &mut iroh::endpoint::SendStream,
    value: &T,
) -> Result<(), TransportError> {
    use tokio::io::AsyncWriteExt;

    let bytes = postcard::to_allocvec(value).map_err(frame_error)?;
    if bytes.is_empty() || bytes.len() > MAX_PROBE_FRAME_BYTES {
        return Err(probe_error("probe frame exceeds its byte bound"));
    }
    let len = u32::try_from(bytes.len()).map_err(frame_error)?;
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

async fn read_probe_frame<T: DeserializeOwned>(
    stream: &mut iroh::endpoint::RecvStream,
) -> Result<T, TransportError> {
    use tokio::io::AsyncReadExt;

    let mut prefix = [0_u8; 4];
    stream
        .read_exact(&mut prefix)
        .await
        .map_err(|error| TransportError::Iroh(error.to_string()))?;
    let len = u32::from_be_bytes(prefix) as usize;
    if len == 0 || len > MAX_PROBE_FRAME_BYTES {
        return Err(probe_error("probe frame length is invalid"));
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

fn probe_error(message: &str) -> TransportError {
    TransportError::Frame(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(attempt: u64) -> AssignmentScope {
        AssignmentScope::new([1; 16], [2; 16], attempt, [3; 32])
            .expect("valid probe test assignment")
    }

    fn session() -> CompilerProbeSession {
        CompilerProbeSession {
            scope: scope(7),
            nonce: [4; 16],
            expires_at_unix_ms: 10_000,
            identity: CompilerProbeIdentity {
                package_lineage: [5; 32],
                target: [6; 32],
                package_target: vec![1, 2, 3],
                profile: [1, 2],
                stage: 1,
                recipe: [7; 32],
                input_root: [8; 32],
                read_manifest: [9; 32],
                input_closure_id: [10; 32],
                input_manifest_object_id: [11; 32],
                selected_base: None,
                max_output_bytes: 1_000,
            },
            demand: CompilerProbeDemand {
                cpu_millicores: 500,
                memory_bytes: 1024,
                transfer_bytes: 4096,
            },
            session_affinity: Some([12; 32]),
        }
    }

    fn object(last: u32, payload_bytes: u32) -> ProbeObjectClaim {
        let mut object_id = [0; 32];
        object_id[28..].copy_from_slice(&last.to_be_bytes());
        ProbeObjectClaim {
            object_id,
            payload_bytes,
        }
    }

    fn snapshot() -> ProbeWorkerSnapshot {
        ProbeWorkerSnapshot {
            worker_incarnation: [13; 32],
            capacity_revision: [14; 16],
            total: ProbeResourceCredits {
                cpu_millicores: 4000,
                memory_bytes: 16_384,
                transfer_bytes: 1_000_000,
            },
            busy: ProbeResourceCredits {
                cpu_millicores: 500,
                memory_bytes: 1024,
                transfer_bytes: 100,
            },
            capability: ProbeCapability::Supported {
                capability_digest: [15; 32],
            },
            session_affinity_warm: true,
            capacity_lease_ms: 5000,
        }
    }

    #[test]
    fn paged_inventory_digest_is_stable_and_rejects_order_mutations() {
        let objects: Vec<_> = (1..=4097).map(|index| object(index, index % 257)).collect();
        let mut inventory = ProbeInventoryHasher::new();
        for claim in &objects {
            inventory.push(*claim).expect("canonical inventory member");
        }
        let descriptor = inventory.finish().expect("complete inventory");
        assert_eq!(descriptor.object_count, 4097);
        assert_eq!(
            descriptor.payload_bytes,
            objects
                .iter()
                .map(|item| u64::from(item.payload_bytes))
                .sum::<u64>()
        );

        let first = CompilerProbePage::new(
            session(),
            descriptor,
            0,
            objects[..MAX_PROBE_OBJECTS_PER_PAGE].to_vec(),
        )
        .expect("first bounded page");
        let last = CompilerProbePage::new(
            session(),
            descriptor,
            1,
            objects[MAX_PROBE_OBJECTS_PER_PAGE..].to_vec(),
        )
        .expect("last bounded page");
        first.validate(9_000).expect("valid first page");
        last.validate(9_000).expect("valid last page");
        let mut bad_order = ProbeInventoryHasher::new();
        bad_order.push(objects[1]).expect("first member");
        assert!(bad_order.push(objects[0]).is_err());
    }

    #[test]
    fn have_bitmap_is_exactly_bound_to_page_nonce_and_lengths() {
        let objects = vec![object(1, 4097), object(2, 8193), object(3, 1)];
        let inventory = probe_inventory_descriptor(&objects).expect("inventory descriptor");
        let page =
            CompilerProbePage::new(session(), inventory, 0, objects).expect("challenge page");
        let reply = CompilerProbePageReply::new(&page, snapshot(), vec![0b0000_0101])
            .expect("CAS-verified Have bitmap");
        reply
            .validate_for(&page, 9_000)
            .expect("exact page response");
        assert_eq!(reply.have_count, 2);
        assert_eq!(reply.have_payload_bytes, 4098);
        assert_eq!(
            reply.missing_objects(&page, 9_000).expect("missing set"),
            vec![object(2, 8193)]
        );

        let mut forged = reply.clone();
        forged.have_payload_bytes += 1;
        assert!(forged.validate_for(&page, 9_000).is_err());
        let mut wrong_nonce = page.clone();
        wrong_nonce.session.nonce[0] ^= 1;
        assert!(reply.validate_for(&wrong_nonce, 9_000).is_err());
        let mut padded = CompilerProbePageReply::new(&page, snapshot(), vec![0b0000_0101])
            .expect("valid response");
        padded.have_bitmap[0] |= 0b1000_0000;
        assert!(padded.validate_for(&page, 9_000).is_err());
    }
}

//! Capability-gated Iroh access to the existing local command and hydration APIs.

use backend_client::{CommandTransport, Session, UnixCommandTransport};
use backend_engine::cluster_transport::{
    MAX_REMOTE_INDEX_BODY_BYTES, RemoteIndexCapability, RemoteIndexChannel, RemoteIndexOutcome,
    RemoteIndexReject, RemoteIndexRequest, RemoteIndexResponse, RemoteIndexSession,
    prepare_remote_index_response,
};
use backend_library::{
    Command, CommandDto, CommandReply, ProductText, SurfaceCommand, SurfaceReply,
    decode_command_body, decode_reply_body,
};
use backend_replication::{
    LocalControlClient, LocalControlLimits, LocalControlRequest, LocalControlResponse,
    SelectedGenerationStamp, SelectedSemanticImageChunk, SelectedSemanticImageGet,
    SemanticCatalogChunk, SemanticCatalogGet, SemanticManifestChunk, SemanticManifestGet,
    SemanticRangeChunk, SemanticRangeGet, SemanticTargetKey, decode_request, encode_response,
};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Read};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_REMOTE_INDEX_CONNECTIONS: usize = 8;
const MAX_REMOTE_INDEX_REQUESTS_PER_SESSION: u32 = 4_096;
const MAX_REMOTE_INDEX_RESPONSE_BYTES: u64 = MAX_REMOTE_INDEX_BODY_BYTES as u64;
const REMOTE_CONTROL_LIMITS: LocalControlLimits = LocalControlLimits {
    max_frame: backend_replication::LOCAL_CONTROL_MAX_FRAME,
    max_cursor: backend_replication::LOCAL_CONTROL_MAX_CURSOR,
    max_error: backend_replication::LOCAL_CONTROL_MAX_ERROR,
};
const GRANT_USAGE_MAGIC: &[u8; 8] = b"BKRUGR01";
const GRANT_USAGE_CHECKSUM_BYTES: usize = 32;
const MAX_GRANT_USAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_GRANT_USAGE_ENTRIES: usize = 256;

#[derive(Clone, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct GrantUsage {
    capability: RemoteIndexCapability,
    requests: u32,
    response_bytes: u64,
    revoked: bool,
}

#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct PersistedGrantUsage {
    version: u16,
    owner: backend_engine::cluster_transport::EndpointId,
    entries: BTreeMap<[u8; 16], GrantUsage>,
}

/// Durable admission permit for one exact canonical response frame.
///
/// The ledger reservation is the response/revocation linearization point. A
/// permit admitted before a revoke may finish sending after revoke returns;
/// the permit holds no ledger lock across network I/O.
#[must_use = "a response may be sent only after durable admission"]
struct RemoteIndexResponsePermit {
    grant_id: [u8; 16],
    wire_bytes: u64,
}

enum RemoteResponseFailure {
    Admission(RemoteIndexReject),
    Transport,
}

/// Public, key-free view of one owner-issued remote read grant.
#[derive(Clone, Debug)]
pub struct RemoteIndexGrantSummary {
    /// Random grant identity printed by `cluster owner grant list`.
    pub grant_id: [u8; 16],
    /// Exact authorized Iroh client peer.
    pub client: backend_engine::cluster_transport::EndpointId,
    /// Expiry time in Unix milliseconds.
    pub expires_at_unix_ms: u64,
    /// Whether the owner has revoked this exact grant.
    pub revoked: bool,
    /// Cumulative admitted requests across reconnects and owner restarts.
    pub requests: u32,
    /// Signed request limit for this grant.
    pub request_budget: u32,
    /// Cumulative reserved response bytes across reconnects and owner restarts.
    pub response_bytes: u64,
    /// Signed response-byte limit for this grant.
    pub byte_budget: u64,
    /// Optional exact product root and operation scope.
    pub product: Option<backend_engine::cluster_transport::RemoteIndexProductScope>,
    /// Optional exact semantic target and selection scope.
    pub semantic: Option<backend_engine::cluster_transport::RemoteIndexSemanticSelection>,
}

/// Durable owner-bound capability registry and cumulative budget meter.
#[derive(Clone)]
pub struct RemoteIndexUsage {
    entries: Arc<Mutex<BTreeMap<[u8; 16], GrantUsage>>>,
    path: Option<Arc<PathBuf>>,
    owner: Option<backend_engine::cluster_transport::EndpointId>,
    disabled: bool,
}

impl Default for RemoteIndexUsage {
    fn default() -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            path: None,
            owner: None,
            disabled: false,
        }
    }
}

impl RemoteIndexUsage {
    /// Opens the durable owner-bound capability registry and budget ledger.
    pub fn open(
        path: impl Into<PathBuf>,
        owner: backend_engine::cluster_transport::EndpointId,
    ) -> io::Result<Self> {
        let path = path.into();
        let _file_lock = lock_usage_ledger(&path)?;
        let mut entries = read_usage_ledger(&path, owner)?;
        let now = backend_engine::cluster_transport::remote_index_now()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        entries.retain(|_, usage| usage.capability.claims.expires_at_unix_ms > now);
        let result = Self {
            entries: Arc::new(Mutex::new(entries)),
            path: Some(Arc::new(path)),
            owner: Some(owner),
            disabled: false,
        };
        {
            let entries = result
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            result.persist(&entries)?;
        }
        Ok(result)
    }

    pub(crate) fn disabled() -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            path: None,
            owner: None,
            disabled: true,
        }
    }

    /// Registers an exact owner-signed capability without resetting its existing meter.
    pub fn register_capability(
        &self,
        capability: &RemoteIndexCapability,
        now_ms: u64,
    ) -> io::Result<()> {
        if self.disabled {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger is disabled",
            ));
        }
        let owner = self.owner.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger is not owner-bound",
            )
        })?;
        capability
            .verify(owner, capability.claims.client, now_ms)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
        let mut cached = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.path.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger has no durable path",
            )
        })?;
        let _file_lock = lock_usage_ledger(path)?;
        let mut entries = read_usage_ledger(path, owner)?;
        entries.retain(|_, usage| usage.capability.claims.expires_at_unix_ms > now_ms);
        let grant_id = capability.grant_id();
        match entries.get(&grant_id) {
            Some(existing) if existing.capability == *capability && !existing.revoked => {}
            Some(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "grant identity is already registered or revoked",
                ));
            }
            None if entries.len() >= MAX_GRANT_USAGE_ENTRIES => {
                return Err(io::Error::other("remote-index grant registry is full"));
            }
            None => {
                entries.insert(
                    grant_id,
                    GrantUsage {
                        capability: capability.clone(),
                        requests: 0,
                        response_bytes: 0,
                        revoked: false,
                    },
                );
            }
        }
        self.persist(&entries)?;
        *cached = entries;
        Ok(())
    }

    /// Revokes one registered capability. Repeating a revoke is safe and returns `false`.
    pub fn revoke(&self, grant_id: [u8; 16]) -> io::Result<bool> {
        let owner = self.owner.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger is not owner-bound",
            )
        })?;
        let mut cached = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.path.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger has no durable path",
            )
        })?;
        let _file_lock = lock_usage_ledger(path)?;
        let now = backend_engine::cluster_transport::remote_index_now()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let mut entries = read_usage_ledger(path, owner)?;
        entries.retain(|_, usage| usage.capability.claims.expires_at_unix_ms > now);
        let grant = entries.get_mut(&grant_id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "active remote-index grant was not found",
            )
        })?;
        if grant.capability.claims.server != owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "grant belongs to a different owner",
            ));
        }
        let changed = !grant.revoked;
        grant.revoked = true;
        self.persist(&entries)?;
        *cached = entries;
        Ok(changed)
    }

    /// Lists current unexpired grant scopes and cumulative usage without exposing keys.
    pub fn list(&self) -> io::Result<Vec<RemoteIndexGrantSummary>> {
        let owner = self.owner.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger is not owner-bound",
            )
        })?;
        let mut cached = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let path = self.path.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger has no durable path",
            )
        })?;
        let _file_lock = lock_usage_ledger(path)?;
        let now = backend_engine::cluster_transport::remote_index_now()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let mut entries = read_usage_ledger(path, owner)?;
        entries.retain(|_, usage| usage.capability.claims.expires_at_unix_ms > now);
        let result = entries
            .iter()
            .map(|(grant_id, usage)| RemoteIndexGrantSummary {
                grant_id: *grant_id,
                client: usage.capability.claims.client,
                expires_at_unix_ms: usage.capability.claims.expires_at_unix_ms,
                revoked: usage.revoked,
                requests: usage.requests,
                request_budget: usage.capability.claims.request_budget,
                response_bytes: usage.response_bytes,
                byte_budget: usage.capability.claims.byte_budget,
                product: usage.capability.claims.product.clone(),
                semantic: usage.capability.claims.semantic.clone(),
            })
            .collect();
        if entries.len() != cached.len()
            || entries.iter().any(|(id, entry)| {
                cached.get(id).map_or(true, |cached| {
                    cached.capability != entry.capability
                        || cached.revoked != entry.revoked
                        || cached.requests != entry.requests
                        || cached.response_bytes != entry.response_bytes
                })
            })
        {
            self.persist(&entries)?;
        }
        *cached = entries;
        Ok(result)
    }

    fn charge_request(&self, capability: &RemoteIndexCapability) -> Result<(), RemoteIndexReject> {
        if self.disabled {
            return Err(RemoteIndexReject::OwnerUnavailable);
        }
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = self.owner.ok_or(RemoteIndexReject::OwnerUnavailable)?;
        let path = self
            .path
            .as_deref()
            .ok_or(RemoteIndexReject::OwnerUnavailable)?;
        let _file_lock =
            lock_usage_ledger(path).map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        let mut candidate =
            read_usage_ledger(path, owner).map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        let now = backend_engine::cluster_transport::remote_index_now()
            .map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        candidate.retain(|_, usage| usage.capability.claims.expires_at_unix_ms > now);
        let usage = candidate
            .get_mut(&capability.claims.grant_id)
            .ok_or(RemoteIndexReject::StaleCapability)?;
        if usage.capability != *capability || usage.capability.claims.server != owner {
            return Err(RemoteIndexReject::StaleCapability);
        }
        if usage.revoked {
            return Err(RemoteIndexReject::CapabilityRevoked);
        }
        let next = usage.requests.saturating_add(1);
        if next > capability.claims.request_budget {
            return Err(RemoteIndexReject::ReplayOrBudget);
        }
        usage.requests = next;
        self.persist(&candidate)
            .map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        *entries = candidate;
        Ok(())
    }

    fn admit_response(
        &self,
        capability: &RemoteIndexCapability,
        wire_bytes: usize,
    ) -> Result<RemoteIndexResponsePermit, RemoteIndexReject> {
        self.reserve_response_bytes(capability, wire_bytes, false)
    }

    /// Admits only the payload-free typed notice for an already revoked grant.
    /// The denial itself is metered and does not admit or charge a request.
    fn admit_revocation_notice(
        &self,
        capability: &RemoteIndexCapability,
        wire_bytes: usize,
    ) -> Result<RemoteIndexResponsePermit, RemoteIndexReject> {
        self.reserve_response_bytes(capability, wire_bytes, true)
    }

    fn reserve_response_bytes(
        &self,
        capability: &RemoteIndexCapability,
        wire_bytes: usize,
        revocation_notice: bool,
    ) -> Result<RemoteIndexResponsePermit, RemoteIndexReject> {
        if self.disabled {
            return Err(RemoteIndexReject::OwnerUnavailable);
        }
        let amount = u64::try_from(wire_bytes).map_err(|_| RemoteIndexReject::ReplayOrBudget)?;
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let owner = self.owner.ok_or(RemoteIndexReject::OwnerUnavailable)?;
        let path = self
            .path
            .as_deref()
            .ok_or(RemoteIndexReject::OwnerUnavailable)?;
        let _file_lock =
            lock_usage_ledger(path).map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        let mut candidate =
            read_usage_ledger(path, owner).map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        let usage = candidate
            .get_mut(&capability.claims.grant_id)
            .ok_or(RemoteIndexReject::StaleCapability)?;
        if usage.capability != *capability || usage.capability.claims.server != owner {
            return Err(RemoteIndexReject::StaleCapability);
        }
        if usage.revoked && !revocation_notice {
            return Err(RemoteIndexReject::CapabilityRevoked);
        }
        if revocation_notice && !usage.revoked {
            return Err(RemoteIndexReject::InvalidRequest);
        }
        let now = backend_engine::cluster_transport::remote_index_now()
            .map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        if capability.claims.expires_at_unix_ms <= now {
            return Err(RemoteIndexReject::StaleCapability);
        }
        let next = usage
            .response_bytes
            .checked_add(amount)
            .ok_or(RemoteIndexReject::ReplayOrBudget)?;
        if next > capability.claims.byte_budget {
            return Err(RemoteIndexReject::ReplayOrBudget);
        }
        usage.response_bytes = next;
        self.persist(&candidate)
            .map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        *entries = candidate;
        Ok(RemoteIndexResponsePermit {
            grant_id: capability.claims.grant_id,
            wire_bytes: amount,
        })
    }

    fn persist(&self, entries: &BTreeMap<[u8; 16], GrantUsage>) -> io::Result<()> {
        let Some(path) = self.path.as_deref() else {
            return Ok(());
        };
        let owner = self.owner.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger is not owner-bound",
            )
        })?;
        let persisted = PersistedGrantUsage {
            version: 1,
            owner,
            entries: entries.clone(),
        };
        let mut bytes = Vec::with_capacity(GRANT_USAGE_MAGIC.len() + entries.len() * 32);
        bytes.extend_from_slice(GRANT_USAGE_MAGIC);
        bytes.extend_from_slice(&postcard::to_allocvec(&persisted).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "remote-index usage serialization failed",
            )
        })?);
        let checksum = blake3::hash(&bytes);
        bytes.extend_from_slice(checksum.as_bytes());
        if bytes.len() > MAX_GRANT_USAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "remote-index usage ledger is full",
            ));
        }
        backend_platform::durable::write_private_atomic(path, &bytes)
    }
}

fn lock_usage_ledger(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let lock_path = parent.join(format!("{name}.lock"));
    let file = backend_platform::durability::open_or_create_regular_file_nofollow(&lock_path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote-index ledger lock has an invalid type",
        ));
    }
    #[cfg(unix)]
    {
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index ledger lock must be owned by this user and have one link",
            ));
        }
        if metadata.permissions().mode() & 0o777 != 0o600 {
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        let metadata = file.metadata()?;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index ledger lock must have mode 0600",
            ));
        }
    }
    file.lock()?;
    Ok(file)
}

fn read_usage_ledger(
    path: &Path,
    owner: backend_engine::cluster_transport::EndpointId,
) -> io::Result<BTreeMap<[u8; 16], GrantUsage>> {
    let file = match backend_platform::durability::open_regular_file_nofollow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_GRANT_USAGE_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote-index grant ledger has an invalid type or size",
        ));
    }
    #[cfg(unix)]
    {
        if metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o777 != 0o600
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger must be a single-link owner-only file",
            ));
        }
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_GRANT_USAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_GRANT_USAGE_BYTES
        || bytes.get(..GRANT_USAGE_MAGIC.len()) != Some(GRANT_USAGE_MAGIC.as_slice())
        || bytes.len() < GRANT_USAGE_MAGIC.len() + GRANT_USAGE_CHECKSUM_BYTES
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote-index grant ledger header, truncation, or size is invalid",
        ));
    }
    let content_end = bytes.len() - GRANT_USAGE_CHECKSUM_BYTES;
    if blake3::hash(&bytes[..content_end]).as_bytes() != &bytes[content_end..] {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote-index grant ledger checksum failed",
        ));
    }
    let payload = &bytes[GRANT_USAGE_MAGIC.len()..content_end];
    let persisted: PersistedGrantUsage = postcard::from_bytes(payload).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "remote-index grant ledger encoding is invalid",
        )
    })?;
    let canonical = postcard::to_allocvec(&persisted).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "remote-index grant ledger could not be canonicalized",
        )
    })?;
    if persisted.version != 1
        || persisted.owner != owner
        || persisted.entries.len() > MAX_GRANT_USAGE_ENTRIES
        || canonical.as_slice() != payload
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "remote-index grant ledger version, owner, or contents are invalid",
        ));
    }
    for (grant_id, usage) in &persisted.entries {
        let capability = &usage.capability;
        if capability.grant_id() != *grant_id
            || capability.claims.server != owner
            || usage.requests > capability.claims.request_budget
            || usage.response_bytes > capability.claims.byte_budget
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "remote-index grant record does not match its signed scope or budgets",
            ));
        }
        capability
            .verify(
                owner,
                capability.claims.client,
                capability.claims.issued_at_unix_ms,
            )
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "remote-index grant signature failed ledger admission",
                )
            })?;
    }
    Ok(persisted.entries)
}

pub(crate) async fn serve_connection(
    mut session: RemoteIndexSession,
    local_endpoint: PathBuf,
    usage: RemoteIndexUsage,
) {
    let capability = session.capability().clone();
    let mut requests = 0_u32;
    loop {
        let request = match tokio::time::timeout(
            backend_engine::cluster_transport::REMOTE_INDEX_SESSION_TIMEOUT,
            session.receive_request(),
        )
        .await
        {
            Ok(Ok(request)) => request,
            _ => return,
        };
        let next_request = requests.saturating_add(1);
        if next_request > MAX_REMOTE_INDEX_REQUESTS_PER_SESSION {
            let _ = send_remote_response(
                &mut session,
                &usage,
                &capability,
                RemoteIndexResponse {
                    request_id: request.request_id,
                    outcome: RemoteIndexOutcome::Rejected(RemoteIndexReject::ReplayOrBudget),
                },
            )
            .await;
            return;
        }
        requests = next_request;
        // This durable reservation is the request admission point. If it wins
        // a race with revoke, the bounded read-only owner call may still run;
        // its separate response permit prevents a result from escaping if
        // revoke wins before that result is admitted.
        let meter = usage.clone();
        let request_capability = capability.clone();
        let charged =
            tokio::task::spawn_blocking(move || meter.charge_request(&request_capability))
                .await
                .unwrap_or(Err(RemoteIndexReject::OwnerUnavailable));
        if charged.is_err() {
            let reason = charged.err().unwrap_or(RemoteIndexReject::ReplayOrBudget);
            let _ = send_remote_response(
                &mut session,
                &usage,
                &capability,
                RemoteIndexResponse {
                    request_id: request.request_id,
                    outcome: RemoteIndexOutcome::Rejected(reason),
                },
            )
            .await;
            return;
        }

        let channel = session.channel();
        let claims = capability.clone();
        let endpoint = local_endpoint.clone();
        let task = tokio::task::spawn_blocking(move || match channel {
            RemoteIndexChannel::ProductQuery => serve_product_request(&endpoint, &claims, request),
            RemoteIndexChannel::SemanticHydration => {
                serve_semantic_request(&endpoint, &claims, request)
            }
        });
        let response = match tokio::time::timeout(
            backend_engine::cluster_transport::REMOTE_INDEX_SESSION_TIMEOUT,
            task,
        )
        .await
        {
            Ok(Ok(response)) => response,
            _ => return,
        };
        if send_remote_response(&mut session, &usage, &capability, response)
            .await
            .is_err()
        {
            return;
        }
    }
}

async fn send_remote_response(
    session: &mut RemoteIndexSession,
    usage: &RemoteIndexUsage,
    capability: &RemoteIndexCapability,
    response: RemoteIndexResponse,
) -> Result<(), RemoteIndexReject> {
    let request_id = response.request_id;
    match admit_and_send_remote_response(session, usage, capability, response).await {
        Ok(()) => Ok(()),
        Err(RemoteResponseFailure::Transport) => Err(RemoteIndexReject::OwnerUnavailable),
        Err(RemoteResponseFailure::Admission(reason)) => {
            // If a result cannot be admitted under the remaining byte budget,
            // send a small typed refusal only when that refusal itself obtains
            // a durable byte permit. Revocation/ledger failure therefore sends
            // no unmetered response bytes.
            let rejected = RemoteIndexResponse {
                request_id,
                outcome: RemoteIndexOutcome::Rejected(reason),
            };
            admit_and_send_remote_response(session, usage, capability, rejected)
                .await
                .map_err(|failure| match failure {
                    RemoteResponseFailure::Admission(reason) => reason,
                    RemoteResponseFailure::Transport => RemoteIndexReject::OwnerUnavailable,
                })
        }
    }
}

async fn admit_and_send_remote_response(
    session: &mut RemoteIndexSession,
    usage: &RemoteIndexUsage,
    capability: &RemoteIndexCapability,
    response: RemoteIndexResponse,
) -> Result<(), RemoteResponseFailure> {
    let revocation_notice = matches!(
        &response.outcome,
        RemoteIndexOutcome::Rejected(RemoteIndexReject::CapabilityRevoked)
    );
    let prepared = prepare_remote_index_response(response)
        .map_err(|_| RemoteResponseFailure::Admission(RemoteIndexReject::InvalidRequest))?;
    let wire_bytes = prepared.wire_bytes();
    let meter = usage.clone();
    let response_capability = capability.clone();
    let permit = tokio::task::spawn_blocking(move || {
        if revocation_notice {
            meter.admit_revocation_notice(&response_capability, wire_bytes)
        } else {
            meter.admit_response(&response_capability, wire_bytes)
        }
    })
    .await
    .unwrap_or(Err(RemoteIndexReject::OwnerUnavailable))
    .map_err(RemoteResponseFailure::Admission)?;
    if permit.grant_id != capability.claims.grant_id
        || usize::try_from(permit.wire_bytes).ok() != Some(wire_bytes)
    {
        return Err(RemoteResponseFailure::Admission(
            RemoteIndexReject::OwnerUnavailable,
        ));
    }
    // Revocation that wins before this permit prevents this response. If this
    // permit wins, the admitted in-flight response may finish after revocation.
    // The permit contains only its durable accounting proof; no ledger lock is
    // held while this QUIC write awaits backpressure.
    let sent = tokio::time::timeout(
        backend_engine::cluster_transport::REMOTE_INDEX_SESSION_TIMEOUT,
        session.send_prepared_response(prepared),
    )
    .await;
    drop(permit);
    match sent {
        Ok(Ok(())) => Ok(()),
        _ => Err(RemoteResponseFailure::Transport),
    }
}

fn serve_product_request(
    endpoint: &Path,
    capability: &RemoteIndexCapability,
    request: RemoteIndexRequest,
) -> RemoteIndexResponse {
    let denied = || RemoteIndexResponse {
        request_id: request.request_id,
        outcome: RemoteIndexOutcome::Rejected(RemoteIndexReject::ScopeDenied),
    };
    let Some(scope) = capability.claims.product.as_ref() else {
        return denied();
    };
    if request.body.len() > backend_library::MAX_COMMAND_BODY {
        return invalid(request.request_id);
    }
    let command = match decode_command_body(&request.body) {
        Ok(command) if command.request_id == request.request_id => command,
        _ => return invalid(request.request_id),
    };
    let operation = match product_operation(&command.command) {
        Some(operation) => Some(operation),
        None if matches!(command.command, Command::Revision) => None,
        None => return denied(),
    };
    if operation.is_some_and(|operation| !scope.operations.contains(&operation)) {
        return denied();
    }
    if let Some(basis) = product_basis(&command.command)
        && basis.as_bytes() != &scope.view_root
    {
        return denied();
    }

    let revision = match product_revision(endpoint) {
        Ok(revision) => revision,
        Err(()) => return owner_unavailable(request.request_id),
    };
    let observed = revision.root().to_bytes();
    if observed != scope.view_root {
        return RemoteIndexResponse {
            request_id: request.request_id,
            outcome: RemoteIndexOutcome::StaleProductRoot {
                expected: scope.view_root,
                observed,
            },
        };
    }
    let index_search = matches!(
        &command.command,
        Command::Surface(SurfaceCommand::IndexSearch { .. })
    );
    let expected_search_snapshot = if index_search {
        let Some(expected) = scope.index_search_snapshot else {
            return denied();
        };
        let current = match product_index_search_snapshot(endpoint) {
            Ok(snapshot) => snapshot,
            Err(()) => return owner_unavailable(request.request_id),
        };
        if current != expected {
            return RemoteIndexResponse {
                request_id: request.request_id,
                outcome: RemoteIndexOutcome::StaleProductSnapshot {
                    expected,
                    observed: current,
                },
            };
        }
        Some(expected)
    } else {
        None
    };
    let mut transport = match UnixCommandTransport::connect(endpoint) {
        Ok(transport) => transport,
        Err(_) => return owner_unavailable(request.request_id),
    };
    let reply = match transport.request(command) {
        Ok(reply) => reply,
        Err(backend_client::ClientError::StaleCursor) => {
            let current_root = match product_revision(endpoint) {
                Ok(revision) => revision.root().to_bytes(),
                Err(()) => return owner_unavailable(request.request_id),
            };
            if current_root != scope.view_root {
                return RemoteIndexResponse {
                    request_id: request.request_id,
                    outcome: RemoteIndexOutcome::StaleProductRoot {
                        expected: scope.view_root,
                        observed: current_root,
                    },
                };
            }
            if let Some(expected) = expected_search_snapshot {
                let current = match product_index_search_snapshot(endpoint) {
                    Ok(snapshot) => snapshot,
                    Err(()) => return owner_unavailable(request.request_id),
                };
                if current != expected {
                    return RemoteIndexResponse {
                        request_id: request.request_id,
                        outcome: RemoteIndexOutcome::StaleProductSnapshot {
                            expected,
                            observed: current,
                        },
                    };
                }
            }
            // A cursor can also be malformed or bound to another query while
            // both signed snapshots remain current. Do not mislabel that as a
            // grant change or return owner diagnostics over the wire.
            return invalid(request.request_id);
        }
        Err(_) => return owner_unavailable(request.request_id),
    };
    // Commands with a typed basis are admitted against the grant root by the
    // local owner. IndexSearch carries its own composite page snapshot, checked
    // below against the signed scope and current owner state. Re-read the
    // selected root after either command before exposing its reply.
    let after = match product_revision(endpoint) {
        Ok(revision) => revision,
        Err(()) => return owner_unavailable(request.request_id),
    };
    let after_root = after.root().to_bytes();
    if after_root != scope.view_root {
        return RemoteIndexResponse {
            request_id: request.request_id,
            outcome: RemoteIndexOutcome::StaleProductRoot {
                expected: scope.view_root,
                observed: after_root,
            },
        };
    }
    if let Some(expected) = expected_search_snapshot {
        let observed_reply = match &reply.reply {
            CommandReply::Surface(SurfaceReply::IndexSearchPage(page)) => page.snapshot,
            _ => return owner_unavailable(request.request_id),
        };
        if observed_reply != expected {
            return RemoteIndexResponse {
                request_id: request.request_id,
                outcome: RemoteIndexOutcome::StaleProductSnapshot {
                    expected,
                    observed: observed_reply,
                },
            };
        }
        let after_snapshot = match product_index_search_snapshot(endpoint) {
            Ok(snapshot) => snapshot,
            Err(()) => return owner_unavailable(request.request_id),
        };
        if after_snapshot != expected {
            return RemoteIndexResponse {
                request_id: request.request_id,
                outcome: RemoteIndexOutcome::StaleProductSnapshot {
                    expected,
                    observed: after_snapshot,
                },
            };
        }
    }
    let body = match backend_engine::encode_reply_dto(&reply) {
        Ok(body) if !body.is_empty() && body.len() <= MAX_REMOTE_INDEX_BODY_BYTES => body,
        _ => return owner_unavailable(request.request_id),
    };
    RemoteIndexResponse {
        request_id: request.request_id,
        outcome: RemoteIndexOutcome::Payload(body.into_boxed_slice()),
    }
}

fn product_revision(endpoint: &Path) -> Result<backend_library::RevisionReceipt, ()> {
    let mut transport = UnixCommandTransport::connect(endpoint).map_err(|_| ())?;
    match transport
        .request(CommandDto::new(1, Command::Revision))
        .map_err(|_| ())?
        .reply
    {
        CommandReply::Revision(revision) => Ok(revision),
        _ => Err(()),
    }
}

fn product_index_search_snapshot(endpoint: &Path) -> Result<[u8; 32], ()> {
    let mut session = Session::connect(endpoint).map_err(|_| ())?;
    match session
        .surface(SurfaceCommand::IndexSearch {
            query: ProductText::from_static("__remote-index-capability-snapshot__"),
            limit: 1,
            cursor: None,
        })
        .map_err(|_| ())?
    {
        SurfaceReply::IndexSearchPage(page) => Ok(page.snapshot),
        _ => Err(()),
    }
}

fn product_operation(
    command: &Command,
) -> Option<backend_engine::cluster_transport::RemoteIndexQueryOperation> {
    use backend_engine::cluster_transport::RemoteIndexQueryOperation as Operation;
    match command {
        Command::Surface(SurfaceCommand::IndexSearch { .. }) => Some(Operation::IndexSearch),
        Command::Search(_) => Some(Operation::Search),
        Command::Name(_) => Some(Operation::Names),
        Command::Document(_) => Some(Operation::Document),
        Command::Source(_) => Some(Operation::Source),
        Command::Outline(_) | Command::OutlinePage { .. } => Some(Operation::Outline),
        Command::Graph(_) | Command::GraphPage { .. } => Some(Operation::Graph),
        Command::Related(_) => Some(Operation::Related),
        _ => None,
    }
}

fn product_basis(command: &Command) -> Option<backend_library::ViewRevision> {
    match command {
        Command::Search(query) => Some(query.basis()),
        Command::Name(query) => Some(query.basis()),
        Command::Document(query) | Command::Source(query) => Some(query.basis()),
        Command::Outline(query) => Some(query.basis()),
        Command::OutlinePage { page, .. } | Command::GraphPage { page, .. } => Some(page.basis()),
        Command::Graph(query) | Command::Related(query) => Some(query.basis()),
        _ => None,
    }
}

fn serve_semantic_request(
    endpoint: &Path,
    capability: &RemoteIndexCapability,
    request: RemoteIndexRequest,
) -> RemoteIndexResponse {
    let request_id = request.request_id;
    let Some(scope) = capability.claims.semantic.as_ref() else {
        return denied(request_id);
    };
    if request.body.len() > backend_replication::LOCAL_CONTROL_MAX_FRAME {
        return invalid(request_id);
    }
    let local = match decode_request(&request.body, REMOTE_CONTROL_LIMITS) {
        Ok(local) if local.request_id() == request.request_id => local,
        Err(_) => return invalid(request_id),
        _ => return invalid(request_id),
    };
    let permitted_bytes = match semantic_request_bytes(&local, scope) {
        Some(bytes) => bytes,
        None => return denied(request_id),
    };
    if permitted_bytes == 0 || permitted_bytes > MAX_REMOTE_INDEX_RESPONSE_BYTES {
        return denied(request_id);
    }
    let mut client = match connect_local_control(endpoint) {
        Ok(client) => client,
        Err(_) => return owner_unavailable(request_id),
    };
    let response = match client.request(&local) {
        Ok(response) => response,
        Err(_) => return owner_unavailable(request_id),
    };
    let outcome = match admit_semantic_response(&local, response, scope) {
        Ok(LocalControlResponse::SemanticStaleSelection { .. }) => {
            RemoteIndexOutcome::StaleSemanticSelection
        }
        Ok(response) => match encode_response(&response, REMOTE_CONTROL_LIMITS) {
            Ok(body) if !body.is_empty() && body.len() <= MAX_REMOTE_INDEX_BODY_BYTES => {
                RemoteIndexOutcome::Payload(body.into_boxed_slice())
            }
            _ => RemoteIndexOutcome::Rejected(RemoteIndexReject::OwnerUnavailable),
        },
        Err(_) => RemoteIndexOutcome::Rejected(RemoteIndexReject::StaleCapability),
    };
    RemoteIndexResponse {
        request_id,
        outcome,
    }
}

fn semantic_request_bytes(
    request: &LocalControlRequest,
    scope: &backend_engine::cluster_transport::RemoteIndexSemanticSelection,
) -> Option<u64> {
    match request {
        LocalControlRequest::SemanticRangeGet { payload, .. } => {
            let get = SemanticRangeGet::decode(payload).ok()?;
            if get.request_id != request.request_id()
                || !target_matches(&get.target, scope)
                || !stamp_matches(get.selected_stamp, scope)
            {
                return None;
            }
            Some(get.byte_range.len)
        }
        LocalControlRequest::SemanticMetadataGet { payload, .. } => {
            if let Ok(get) = SemanticCatalogGet::decode(payload) {
                if get.request_id != request.request_id()
                    || !target_matches(&get.target, scope)
                    || get
                        .selected_stamp
                        .is_some_and(|stamp| !stamp_matches(stamp, scope))
                    || get
                        .catalog_root
                        .is_some_and(|root| root.as_bytes() != &scope.catalog_root)
                {
                    return None;
                }
                Some(get.byte_range.len)
            } else if let Ok(get) = SemanticManifestGet::decode(payload) {
                if get.request_id != request.request_id()
                    || !target_matches(&get.target, scope)
                    || !stamp_matches(get.selected_stamp, scope)
                    || get.catalog_root.as_bytes() != &scope.catalog_root
                {
                    return None;
                }
                Some(get.byte_range.len)
            } else if let Ok(get) = SelectedSemanticImageGet::decode(payload) {
                if get.request_id != request.request_id()
                    || !target_matches(&get.target, scope)
                    || !stamp_matches(get.selected_stamp, scope)
                {
                    return None;
                }
                Some(get.byte_range.len)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn admit_semantic_response(
    request: &LocalControlRequest,
    response: LocalControlResponse,
    scope: &backend_engine::cluster_transport::RemoteIndexSemanticSelection,
) -> Result<LocalControlResponse, ()> {
    match (&response, request) {
        (
            LocalControlResponse::SemanticStaleSelection { .. },
            LocalControlRequest::SemanticRangeGet { .. }
            | LocalControlRequest::SemanticMetadataGet { .. },
        ) => Ok(response),
        (
            LocalControlResponse::SemanticRangeChunk { payload, .. },
            LocalControlRequest::SemanticRangeGet {
                payload: get_bytes, ..
            },
        ) => {
            let get = SemanticRangeGet::decode(get_bytes).map_err(|_| ())?;
            let chunk = SemanticRangeChunk::decode(payload).map_err(|_| ())?;
            if get.request_id != request.request_id()
                || !stamp_matches(chunk.selected_stamp, scope)
                || chunk.request_id != get.request_id
                || chunk.byte_range != get.byte_range
            {
                return Err(());
            }
            Ok(response)
        }
        (
            LocalControlResponse::SemanticMetadataChunk { payload, .. },
            LocalControlRequest::SemanticMetadataGet {
                payload: get_bytes, ..
            },
        ) => {
            let exact = if let Ok(get) = SemanticCatalogGet::decode(get_bytes) {
                let chunk = SemanticCatalogChunk::decode(payload).map_err(|_| ())?;
                get.request_id == request.request_id()
                    && target_matches(&chunk.target, scope)
                    && stamp_matches(chunk.selected_stamp, scope)
                    && chunk.catalog_root.as_bytes() == &scope.catalog_root
                    && chunk.request_id == get.request_id
                    && chunk.byte_range == get.byte_range
            } else if let Ok(get) = SemanticManifestGet::decode(get_bytes) {
                let chunk = SemanticManifestChunk::decode(payload).map_err(|_| ())?;
                get.request_id == request.request_id()
                    && target_matches(&chunk.target, scope)
                    && stamp_matches(chunk.selected_stamp, scope)
                    && chunk.catalog_root.as_bytes() == &scope.catalog_root
                    && chunk.request_id == get.request_id
                    && chunk.byte_range == get.byte_range
            } else if let Ok(get) = SelectedSemanticImageGet::decode(get_bytes) {
                let chunk = SelectedSemanticImageChunk::decode(payload).map_err(|_| ())?;
                get.request_id == request.request_id()
                    && target_matches(&chunk.target, scope)
                    && stamp_matches(chunk.selected_stamp, scope)
                    && chunk.request_id == get.request_id
                    && chunk.byte_range == get.byte_range
            } else {
                false
            };
            if exact { Ok(response) } else { Err(()) }
        }
        _ => Err(()),
    }
}

fn target_matches(
    target: &SemanticTargetKey,
    scope: &backend_engine::cluster_transport::RemoteIndexSemanticSelection,
) -> bool {
    target.package() == scope.package
        && target.coordinate() == scope.coordinate
        && <[u8; 2]>::from(target.profile()) == scope.profile
}

fn stamp_matches(
    stamp: SelectedGenerationStamp,
    scope: &backend_engine::cluster_transport::RemoteIndexSemanticSelection,
) -> bool {
    stamp.namespace() == &scope.namespace
        && <[u8; 2]>::from(stamp.profile()) == scope.profile
        && stamp.source_coordinate() == &scope.source_coordinate
        && stamp.selection_revision() == scope.selection_revision
        && stamp.selected_root() == &scope.selected_root
        && stamp.closure_id() == &scope.closure_id
        && stamp.catalog_root().as_bytes() == &scope.catalog_root
}

fn connect_local_control(
    endpoint: &Path,
) -> Result<LocalControlClient<backend_replication::LocalStream>, io::Error> {
    let endpoint_ref = backend_replication::UnixEndpointRef::new(endpoint)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid local endpoint"))?;
    let stream = backend_replication::LocalStream::connect(endpoint_ref.as_path())?;
    backend_replication::AuthenticatedLocalPeer::authenticate(&stream, endpoint_ref.as_path())
        .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error.to_string()))?;
    Ok(LocalControlClient::new(stream, REMOTE_CONTROL_LIMITS))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use backend_engine::cluster_transport::{
        RemoteIndexCapabilityClaims, RemoteIndexCapabilityIssuer, RemoteIndexPermission,
        RemoteIndexProductScope, RemoteIndexQueryOperation, SecretKey,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(label: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("wall clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "backend-remote-index-usage-{label}-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("scratch directory");
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("private scratch directory");
        path
    }

    fn capability(request_budget: u32, byte_budget: u64) -> RemoteIndexCapability {
        let owner = SecretKey::from_bytes(&[51; 32]);
        let client = SecretKey::from_bytes(&[52; 32]);
        let now = backend_engine::cluster_transport::remote_index_now().expect("current time");
        let server = owner.public();
        let client = client.public();
        RemoteIndexCapabilityIssuer::new(owner)
            .issue(
                RemoteIndexCapabilityClaims {
                    version: 2,
                    server,
                    client,
                    grant_id: [53; 16],
                    issued_at_unix_ms: now,
                    expires_at_unix_ms: now + 60_000,
                    request_budget,
                    byte_budget,
                    permissions: vec![RemoteIndexPermission::ProductRead],
                    product: Some(RemoteIndexProductScope {
                        view_root: [54; 32],
                        operations: vec![RemoteIndexQueryOperation::Search],
                        index_search_snapshot: None,
                    }),
                    semantic: None,
                },
                now,
            )
            .expect("signed test grant")
    }

    #[test]
    fn grant_budgets_survive_ledger_cold_reopen_and_corruption_fails_closed() {
        let root = scratch("restart");
        let path = root.join("remote-index-grants.v1");
        let capability = capability(2, 10);
        let owner = capability.claims.server;
        let usage = RemoteIndexUsage::open(&path, owner).expect("fresh ledger");
        usage
            .register_capability(&capability, capability.claims.issued_at_unix_ms)
            .expect("register signed grant");
        usage.charge_request(&capability).expect("first request");
        usage.admit_response(&capability, 8).expect("reserve bytes");
        drop(usage);

        let reopened = RemoteIndexUsage::open(&path, owner).expect("cold-reopened ledger");
        assert!(
            RemoteIndexUsage::open(
                &path,
                backend_engine::cluster_transport::SecretKey::from_bytes(&[99; 32]).public(),
            )
            .is_err()
        );
        reopened
            .charge_request(&capability)
            .expect("second request");
        assert_eq!(
            reopened.charge_request(&capability),
            Err(RemoteIndexReject::ReplayOrBudget)
        );
        assert_eq!(
            reopened
                .admit_response(&capability, 3)
                .err()
                .expect("over-budget response admission fails"),
            RemoteIndexReject::ReplayOrBudget
        );
        drop(reopened);

        let mut bytes = fs::read(&path).expect("read ledger");
        let index = bytes.len().saturating_sub(GRANT_USAGE_CHECKSUM_BYTES + 1);
        bytes[index] ^= 0x80;
        fs::write(&path, bytes).expect("corrupt ledger fixture");
        assert!(RemoteIndexUsage::open(&path, owner).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cumulative_request_budget_can_continue_after_a_cold_session_boundary() {
        let root = scratch("multiple-sessions");
        let path = root.join("remote-index-grants.v1");
        let capability = capability(MAX_REMOTE_INDEX_REQUESTS_PER_SESSION + 8, 64);
        let owner = capability.claims.server;
        let usage = RemoteIndexUsage::open(&path, owner).expect("fresh ledger");
        usage
            .register_capability(&capability, capability.claims.issued_at_unix_ms)
            .expect("register signed grant");
        {
            let _lock = lock_usage_ledger(&path).expect("ledger lock");
            let mut entries = read_usage_ledger(&path, owner).expect("read ledger");
            entries
                .get_mut(&capability.grant_id())
                .expect("registered grant")
                .requests = MAX_REMOTE_INDEX_REQUESTS_PER_SESSION;
            usage
                .persist(&entries)
                .expect("persist previous session usage");
        }
        drop(usage);

        let reopened = RemoteIndexUsage::open(&path, owner).expect("cold-reopened ledger");
        reopened
            .charge_request(&capability)
            .expect("new connection can use the remaining signed budget");
        assert_eq!(reopened.list().expect("grant summary")[0].requests, 4_097);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn exact_grant_revocation_survives_cold_reopen_and_is_idempotent() {
        let root = scratch("revoke");
        let path = root.join("remote-index-grants.v1");
        let capability = capability(4, 64);
        let owner = capability.claims.server;
        let usage = RemoteIndexUsage::open(&path, owner).expect("fresh ledger");
        usage
            .register_capability(&capability, capability.claims.issued_at_unix_ms)
            .expect("register signed grant");
        assert!(usage.revoke(capability.grant_id()).expect("revoke grant"));
        drop(usage);

        let reopened = RemoteIndexUsage::open(&path, owner).expect("reopen revoked ledger");
        assert_eq!(
            reopened.charge_request(&capability),
            Err(RemoteIndexReject::CapabilityRevoked)
        );
        assert_eq!(
            reopened
                .admit_response(&capability, 1)
                .err()
                .expect("revoked response admission fails"),
            RemoteIndexReject::CapabilityRevoked
        );
        let denial = prepare_remote_index_response(RemoteIndexResponse {
            request_id: 1,
            outcome: RemoteIndexOutcome::Rejected(RemoteIndexReject::CapabilityRevoked),
        })
        .expect("prepare revoked-grant notice");
        let denial_bytes = denial.wire_bytes();
        let denial_bytes_u64 = u64::try_from(denial_bytes).expect("bounded denial frame fits u64");
        let notice_permit = reopened
            .admit_revocation_notice(&capability, denial_bytes)
            .expect("meter typed revocation notice");
        assert_eq!(notice_permit.wire_bytes, denial_bytes_u64);
        assert_eq!(
            reopened.list().expect("list revoked grant")[0].response_bytes,
            denial_bytes_u64
        );
        assert!(
            !reopened
                .revoke(capability.grant_id())
                .expect("repeat revoke")
        );
        let grants = reopened.list().expect("list grant");
        assert_eq!(grants.len(), 1);
        assert!(grants[0].revoked);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn response_admission_is_the_revoke_linearization_point() {
        let before_root = scratch("revoke-before-response-admission");
        let before_path = before_root.join("remote-index-grants.v1");
        let before_capability = capability(4, 512);
        let before = RemoteIndexUsage::open(&before_path, before_capability.claims.server)
            .expect("open pre-admission ledger");
        before
            .register_capability(
                &before_capability,
                before_capability.claims.issued_at_unix_ms,
            )
            .expect("register pre-admission grant");
        before
            .revoke(before_capability.grant_id())
            .expect("revoke before response permit");
        assert_eq!(
            before
                .admit_response(&before_capability, 96)
                .err()
                .expect("revoked response admission fails"),
            RemoteIndexReject::CapabilityRevoked
        );
        drop(before);
        let _ = fs::remove_dir_all(before_root);

        let after_root = scratch("revoke-after-response-admission");
        let after_path = after_root.join("remote-index-grants.v1");
        let after_capability = capability(4, 512);
        let after = RemoteIndexUsage::open(&after_path, after_capability.claims.server)
            .expect("open post-admission ledger");
        after
            .register_capability(&after_capability, after_capability.claims.issued_at_unix_ms)
            .expect("register post-admission grant");
        let permit = after
            .admit_response(&after_capability, 96)
            .expect("durable response permit wins");
        assert!(
            after
                .revoke(after_capability.grant_id())
                .expect("revoke after response admission")
        );
        assert_eq!(permit.grant_id, after_capability.grant_id());
        assert_eq!(permit.wire_bytes, 96);
        assert_eq!(
            after.charge_request(&after_capability),
            Err(RemoteIndexReject::CapabilityRevoked)
        );
        // Keeping this permit alive represents a response already admitted:
        // a later revoke blocks new admissions but cannot unsend that frame.
        drop(permit);
        drop(after);
        let _ = fs::remove_dir_all(after_root);
    }

    #[test]
    fn concurrent_revoke_and_response_admission_has_one_durable_winner() {
        let root = scratch("revoke-admission-race");
        for sequence in 0..8 {
            let path = root.join(format!("remote-index-grants-{sequence}.v1"));
            let capability = capability(4, 512);
            let owner = capability.claims.server;
            let admitting = RemoteIndexUsage::open(&path, owner).expect("open race ledger");
            admitting
                .register_capability(&capability, capability.claims.issued_at_unix_ms)
                .expect("register race grant");
            let revoking = RemoteIndexUsage::open(&path, owner).expect("open second ledger handle");
            let barrier = Arc::new(std::sync::Barrier::new(3));

            let admit_barrier = Arc::clone(&barrier);
            let admit_capability = capability.clone();
            let admit_thread = std::thread::spawn(move || {
                admit_barrier.wait();
                admitting.admit_response(&admit_capability, 96)
            });

            let revoke_barrier = Arc::clone(&barrier);
            let grant_id = capability.grant_id();
            let revoke_thread = std::thread::spawn(move || {
                revoke_barrier.wait();
                revoking.revoke(grant_id)
            });
            barrier.wait();

            let response_permit = admit_thread.join().expect("join response admission");
            let revoked = revoke_thread
                .join()
                .expect("join concurrent revoke")
                .expect("persist concurrent revoke");
            assert!(revoked);
            let admitted = match response_permit {
                Ok(permit) => {
                    assert_eq!(permit.grant_id, capability.grant_id());
                    assert_eq!(permit.wire_bytes, 96);
                    true
                }
                Err(RemoteIndexReject::CapabilityRevoked) => false,
                Err(reason) => panic!("unexpected response admission failure: {reason:?}"),
            };

            let reopened = RemoteIndexUsage::open(&path, owner).expect("read race outcome");
            let summary = reopened.list().expect("list race outcome");
            assert!(summary[0].revoked);
            assert_eq!(summary[0].response_bytes, if admitted { 96 } else { 0 });
            assert_eq!(
                reopened.charge_request(&capability),
                Err(RemoteIndexReject::CapabilityRevoked)
            );
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_root_response_reserves_its_full_canonical_wire_size() {
        let response = RemoteIndexResponse {
            request_id: 7,
            outcome: RemoteIndexOutcome::StaleProductRoot {
                expected: [1; 32],
                observed: [2; 32],
            },
        };
        let prepared = prepare_remote_index_response(response).expect("prepare typed response");
        let wire_bytes = prepared.wire_bytes();
        assert!(wire_bytes > 64, "stale-root frame must include both roots");
        let wire_bytes_u64 = u64::try_from(wire_bytes).expect("bounded frame size fits u64");

        let root = scratch("canonical-response-meter");
        let path = root.join("remote-index-grants.v1");
        let capability = capability(2, wire_bytes_u64);
        let usage =
            RemoteIndexUsage::open(&path, capability.claims.server).expect("open response meter");
        usage
            .register_capability(&capability, capability.claims.issued_at_unix_ms)
            .expect("register response meter");
        let permit = usage
            .admit_response(&capability, wire_bytes)
            .expect("reserve exact canonical wire size");
        assert_eq!(permit.wire_bytes, wire_bytes_u64);
        assert_eq!(
            usage.list().expect("read usage")[0].response_bytes,
            wire_bytes_u64
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn grant_ledger_and_lock_reject_symbolic_links() {
        use std::os::unix::fs::symlink;

        let root = scratch("symlinks");
        let target = root.join("target");
        fs::write(&target, b"sensitive target").expect("write target");

        let ledger_link = root.join("ledger-link");
        symlink(&target, &ledger_link).expect("symlink ledger");
        let capability = capability(4, 64);
        assert!(RemoteIndexUsage::open(&ledger_link, capability.claims.server).is_err());

        let ledger = root.join("lock-link-ledger");
        let lock_link = root.join("lock-link-ledger.lock");
        symlink(&target, &lock_link).expect("symlink lock");
        assert!(RemoteIndexUsage::open(&ledger, capability.claims.server).is_err());
        assert_eq!(fs::read(&target).expect("read target"), b"sensitive target");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn grant_ledger_works_beneath_execute_only_ancestor() {
        let root = scratch("execute-only");
        let opaque = root.join("opaque");
        let state = opaque.join("state");
        fs::create_dir_all(&state).expect("create nested state");
        fs::set_permissions(&opaque, fs::Permissions::from_mode(0o700))
            .expect("set private ancestor");
        fs::set_permissions(&state, fs::Permissions::from_mode(0o700))
            .expect("set private state directory");
        fs::set_permissions(&opaque, fs::Permissions::from_mode(0o100))
            .expect("make ancestor execute-only");
        let path = state.join("remote-index-grants.v1");
        let capability = capability(2, 64);
        let usage = RemoteIndexUsage::open(&path, capability.claims.server)
            .expect("open through execute-only ancestor");
        usage
            .register_capability(&capability, capability.claims.issued_at_unix_ms)
            .expect("persist grant through execute-only ancestor");
        drop(usage);
        fs::set_permissions(&opaque, fs::Permissions::from_mode(0o700))
            .expect("restore cleanup access");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn independent_usage_handles_serialize_request_reservations() {
        let root = scratch("concurrent");
        let path = root.join("remote-index-grants.v1");
        let capability = capability(20, 1_024);
        let owner = capability.claims.server;
        let first = RemoteIndexUsage::open(&path, owner).expect("fresh ledger");
        first
            .register_capability(&capability, capability.claims.issued_at_unix_ms)
            .expect("register signed grant");
        let second = RemoteIndexUsage::open(&path, owner).expect("independent ledger handle");
        let test_capability = capability.clone();
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let first_barrier = Arc::clone(&barrier);
        let first_capability = capability.clone();
        let first_thread = std::thread::spawn(move || {
            first_barrier.wait();
            for _ in 0..10 {
                first
                    .charge_request(&first_capability)
                    .expect("first handle reservation");
            }
        });
        let second_barrier = Arc::clone(&barrier);
        let second_thread = std::thread::spawn(move || {
            second_barrier.wait();
            for _ in 0..10 {
                second
                    .charge_request(&capability)
                    .expect("second handle reservation");
            }
        });
        barrier.wait();
        first_thread.join().expect("first reservation thread");
        second_thread.join().expect("second reservation thread");
        let current = RemoteIndexUsage::open(&path, owner).expect("cold-read final ledger");
        let grants = current.list().expect("read grant usage");
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].requests, 20);
        assert_eq!(
            current.charge_request(&test_capability),
            Err(RemoteIndexReject::ReplayOrBudget)
        );
        let _ = fs::remove_dir_all(root);
    }
}

fn denied(request_id: u64) -> RemoteIndexResponse {
    RemoteIndexResponse {
        request_id,
        outcome: RemoteIndexOutcome::Rejected(RemoteIndexReject::ScopeDenied),
    }
}

fn invalid(request_id: u64) -> RemoteIndexResponse {
    RemoteIndexResponse {
        request_id,
        outcome: RemoteIndexOutcome::Rejected(RemoteIndexReject::InvalidRequest),
    }
}

fn owner_unavailable(request_id: u64) -> RemoteIndexResponse {
    RemoteIndexResponse {
        request_id,
        outcome: RemoteIndexOutcome::Rejected(RemoteIndexReject::OwnerUnavailable),
    }
}

pub(crate) fn connection_slots() -> Arc<tokio::sync::Semaphore> {
    static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    Arc::clone(
        SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_REMOTE_INDEX_CONNECTIONS))),
    )
}

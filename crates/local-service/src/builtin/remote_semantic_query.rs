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
use backend_platform::directory::{DirectoryCapability, DirectoryRenameError};
use backend_replication::{
    LocalControlClient, LocalControlLimits, LocalControlRequest, LocalControlResponse,
    SelectedGenerationStamp, SelectedSemanticImageChunk, SelectedSemanticImageGet,
    SemanticCatalogChunk, SemanticCatalogGet, SemanticManifestChunk, SemanticManifestGet,
    SemanticRangeChunk, SemanticRangeGet, SemanticTargetKey, decode_request, encode_response,
};
use std::collections::BTreeMap;
#[cfg(test)]
use std::fs;
use std::fs::File;
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const MAX_REMOTE_INDEX_CONNECTIONS: usize = 8;
const MAX_REMOTE_INDEX_OWNER_WORK: usize = 8;
const MAX_REMOTE_INDEX_REQUESTS_PER_SESSION: u32 = 4_096;
const MAX_REMOTE_INDEX_RESPONSE_BYTES: u64 = MAX_REMOTE_INDEX_BODY_BYTES as u64;
const REMOTE_INDEX_LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REMOTE_INDEX_LOCAL_IO_TIMEOUT: Duration = Duration::from_secs(20);
const REMOTE_CONTROL_LIMITS: LocalControlLimits = LocalControlLimits {
    max_frame: backend_replication::LOCAL_CONTROL_MAX_FRAME,
    max_cursor: backend_replication::LOCAL_CONTROL_MAX_CURSOR,
    max_error: backend_replication::LOCAL_CONTROL_MAX_ERROR,
};
const GRANT_USAGE_MAGIC: &[u8; 8] = b"BKRUGR01";
const GRANT_USAGE_CHECKSUM_BYTES: usize = 32;
const MAX_GRANT_USAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_GRANT_USAGE_ENTRIES: usize = 256;
static NEXT_LEDGER_TEMP: AtomicU64 = AtomicU64::new(0);

#[derive(Clone)]
struct GrantUsageLedger {
    directory: DirectoryCapability,
    name: String,
}

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
    ledger: Option<Arc<GrantUsageLedger>>,
    owner: Option<backend_engine::cluster_transport::EndpointId>,
    disabled: bool,
}

fn remote_index_owner_work_slots() -> &'static Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(MAX_REMOTE_INDEX_OWNER_WORK)))
}

async fn acquire_remote_index_work() -> Option<Arc<tokio::sync::OwnedSemaphorePermit>> {
    tokio::time::timeout(
        backend_engine::cluster_transport::REMOTE_INDEX_SESSION_TIMEOUT,
        Arc::clone(remote_index_owner_work_slots()).acquire_owned(),
    )
    .await
    .ok()?
    .ok()
    .map(Arc::new)
}

fn spawn_bounded_remote_index_work<T: Send + 'static>(
    permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    work: impl FnOnce() -> T + Send + 'static,
) -> tokio::task::JoinHandle<T> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
}

impl Default for RemoteIndexUsage {
    fn default() -> Self {
        Self {
            entries: Arc::new(Mutex::new(BTreeMap::new())),
            ledger: None,
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
        let ledger = open_usage_ledger(path.into())?;
        let _file_lock = lock_usage_ledger(&ledger)?;
        let mut entries = read_usage_ledger(&ledger, owner)?;
        let now = backend_engine::cluster_transport::remote_index_now()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        entries.retain(|_, usage| usage.capability.claims.expires_at_unix_ms > now);
        let result = Self {
            entries: Arc::new(Mutex::new(entries)),
            ledger: Some(Arc::new(ledger)),
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
            ledger: None,
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
        let ledger = self.ledger.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger has no durable path",
            )
        })?;
        let _file_lock = lock_usage_ledger(ledger)?;
        let mut entries = read_usage_ledger(ledger, owner)?;
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
        let ledger = self.ledger.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger has no durable path",
            )
        })?;
        let _file_lock = lock_usage_ledger(ledger)?;
        let now = backend_engine::cluster_transport::remote_index_now()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let mut entries = read_usage_ledger(ledger, owner)?;
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
        let ledger = self.ledger.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote-index grant ledger has no durable path",
            )
        })?;
        let _file_lock = lock_usage_ledger(ledger)?;
        let now = backend_engine::cluster_transport::remote_index_now()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let mut entries = read_usage_ledger(ledger, owner)?;
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
        let ledger = self
            .ledger
            .as_deref()
            .ok_or(RemoteIndexReject::OwnerUnavailable)?;
        let _file_lock =
            lock_usage_ledger(ledger).map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        let mut candidate =
            read_usage_ledger(ledger, owner).map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
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
        let ledger = self
            .ledger
            .as_deref()
            .ok_or(RemoteIndexReject::OwnerUnavailable)?;
        let _file_lock =
            lock_usage_ledger(ledger).map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
        let mut candidate =
            read_usage_ledger(ledger, owner).map_err(|_| RemoteIndexReject::OwnerUnavailable)?;
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
        let Some(ledger) = self.ledger.as_deref() else {
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
        write_usage_ledger(ledger, &bytes)
    }
}

fn open_usage_ledger(path: PathBuf) -> io::Result<GrantUsageLedger> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid grant ledger name"))?
        .to_owned();
    let directory = DirectoryCapability::open(parent)?;
    directory.validate_private()?;
    Ok(GrantUsageLedger { directory, name })
}

fn lock_usage_ledger(ledger: &GrantUsageLedger) -> io::Result<File> {
    let lock_name = format!("{}.lock", ledger.name);
    let file = ledger
        .directory
        .open_private_file_read_write(&lock_name, true)?;
    #[cfg(unix)]
    if file.metadata()?.permissions().mode() & 0o777 != 0o600 {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.lock()?;
    Ok(file)
}

fn read_usage_ledger(
    ledger: &GrantUsageLedger,
    owner: backend_engine::cluster_transport::EndpointId,
) -> io::Result<BTreeMap<[u8; 16], GrantUsage>> {
    let file = match ledger.directory.open_private_file(&ledger.name) {
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

fn write_usage_ledger(ledger: &GrantUsageLedger, bytes: &[u8]) -> io::Result<()> {
    for _ in 0..128 {
        let sequence = NEXT_LEDGER_TEMP.fetch_add(1, Ordering::Relaxed);
        let temporary = format!(".{}.tmp-{}-{sequence}", ledger.name, std::process::id());
        let mut file = match ledger.directory.create_file_exclusive(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = write_result {
            let _ = ledger.directory.remove_file(&temporary);
            return Err(error);
        }
        match ledger
            .directory
            .rename_with_outcome(&temporary, &ledger.name, true)
        {
            Ok(()) => return Ok(()),
            Err(DirectoryRenameError::NotCommitted(error)) => {
                let _ = ledger.directory.remove_file(&temporary);
                return Err(error);
            }
            Err(error @ DirectoryRenameError::CommittedButNotDurable(_)) => {
                return Err(error.into_io_error());
            }
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique remote-index ledger temporary",
    ))
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
        let Some(work_permit) = acquire_remote_index_work().await else {
            return;
        };
        let next_request = requests.saturating_add(1);
        if next_request > MAX_REMOTE_INDEX_REQUESTS_PER_SESSION {
            let _ = send_remote_response(
                &mut session,
                &usage,
                &capability,
                Arc::clone(&work_permit),
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
        let request_work = Arc::clone(&work_permit);
        let charge_task = spawn_bounded_remote_index_work(request_work, move || {
            meter.charge_request(&request_capability)
        });
        let charged = match tokio::time::timeout(
            backend_engine::cluster_transport::REMOTE_INDEX_SESSION_TIMEOUT,
            charge_task,
        )
        .await
        {
            Ok(Ok(charged)) => charged,
            _ => return,
        };
        if charged.is_err() {
            let reason = charged.err().unwrap_or(RemoteIndexReject::ReplayOrBudget);
            let _ = send_remote_response(
                &mut session,
                &usage,
                &capability,
                Arc::clone(&work_permit),
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
        let query_work = Arc::clone(&work_permit);
        let task = spawn_bounded_remote_index_work(query_work, move || match channel {
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
        if send_remote_response(
            &mut session,
            &usage,
            &capability,
            Arc::clone(&work_permit),
            response,
        )
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
    work_permit: Arc<tokio::sync::OwnedSemaphorePermit>,
    response: RemoteIndexResponse,
) -> Result<(), RemoteIndexReject> {
    let request_id = response.request_id;
    match admit_and_send_remote_response(
        session,
        usage,
        capability,
        Arc::clone(&work_permit),
        response,
    )
    .await
    {
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
            admit_and_send_remote_response(session, usage, capability, work_permit, rejected)
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
    work_permit: Arc<tokio::sync::OwnedSemaphorePermit>,
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
    let response_work = Arc::clone(&work_permit);
    let admission = spawn_bounded_remote_index_work(response_work, move || {
        if revocation_notice {
            meter.admit_revocation_notice(&response_capability, wire_bytes)
        } else {
            meter.admit_response(&response_capability, wire_bytes)
        }
    });
    let permit = match tokio::time::timeout(
        backend_engine::cluster_transport::REMOTE_INDEX_SESSION_TIMEOUT,
        admission,
    )
    .await
    {
        Ok(Ok(Ok(permit))) => permit,
        Ok(Ok(Err(reason))) => return Err(RemoteResponseFailure::Admission(reason)),
        _ => return Err(RemoteResponseFailure::Transport),
    };
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
    let selected_source = revision.source().to_bytes();
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
    let mut transport = match UnixCommandTransport::connect_with_timeouts(
        endpoint,
        REMOTE_INDEX_LOCAL_CONNECT_TIMEOUT,
        REMOTE_INDEX_LOCAL_IO_TIMEOUT,
    ) {
        Ok(transport) => transport,
        Err(_) => return owner_unavailable(request.request_id),
    };
    let reply = match transport.request(command) {
        Ok(reply) => reply,
        Err(backend_client::ClientError::StaleCursor)
        | Err(backend_client::ClientError::BasisMismatch { .. }) => {
            let current = match product_revision(endpoint) {
                Ok(revision) => (revision.root().to_bytes(), revision.source().to_bytes()),
                Err(()) => return owner_unavailable(request.request_id),
            };
            let current_root = current.0;
            if current_root != scope.view_root {
                return RemoteIndexResponse {
                    request_id: request.request_id,
                    outcome: RemoteIndexOutcome::StaleProductRoot {
                        expected: scope.view_root,
                        observed: current_root,
                    },
                };
            }
            if current.1 != selected_source {
                return RemoteIndexResponse {
                    request_id: request.request_id,
                    outcome: RemoteIndexOutcome::StaleProductSource {
                        expected: selected_source,
                        observed: current.1,
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
            // A cursor or basis can be malformed even while both signed
            // snapshots remain current. Do not mislabel that as a grant
            // change or return owner diagnostics over the wire.
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
    let after_source = after.source().to_bytes();
    if after_source != selected_source {
        return RemoteIndexResponse {
            request_id: request.request_id,
            outcome: RemoteIndexOutcome::StaleProductSource {
                expected: selected_source,
                observed: after_source,
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
    let mut transport = UnixCommandTransport::connect_with_timeouts(
        endpoint,
        REMOTE_INDEX_LOCAL_CONNECT_TIMEOUT,
        REMOTE_INDEX_LOCAL_IO_TIMEOUT,
    )
    .map_err(|_| ())?;
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
    let transport = UnixCommandTransport::connect_with_timeouts(
        endpoint,
        REMOTE_INDEX_LOCAL_CONNECT_TIMEOUT,
        REMOTE_INDEX_LOCAL_IO_TIMEOUT,
    )
    .map_err(|_| ())?;
    let mut session = Session::from_transport(endpoint.to_path_buf(), transport);
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
    connect_local_control_with_deadlines(
        endpoint,
        REMOTE_INDEX_LOCAL_CONNECT_TIMEOUT,
        REMOTE_INDEX_LOCAL_IO_TIMEOUT,
    )
}

fn connect_local_control_with_deadlines(
    endpoint: &Path,
    connect_timeout: Duration,
    io_timeout: Duration,
) -> Result<LocalControlClient<backend_replication::LocalStream>, io::Error> {
    let endpoint_ref = backend_replication::UnixEndpointRef::new(endpoint)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid local endpoint"))?;
    let stream = backend_platform::local::connect_timeout(endpoint_ref.as_path(), connect_timeout)?;
    stream.set_read_timeout(Some(io_timeout))?;
    stream.set_write_timeout(Some(io_timeout))?;
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
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
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

    #[tokio::test]
    async fn detached_blocking_owner_work_keeps_its_slot_until_completion() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::new(
            Arc::clone(&slots)
                .acquire_owned()
                .await
                .expect("work semaphore is open"),
        );
        let (release, blocked) = std::sync::mpsc::channel();
        let mut task = spawn_bounded_remote_index_work(permit, move || {
            blocked.recv().expect("test releases withheld owner work");
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(20), &mut task)
                .await
                .is_err(),
            "the fake owner must remain blocked past the caller deadline"
        );
        assert_eq!(slots.available_permits(), 0);
        release.send(()).expect("release withheld owner work");
        task.await.expect("owner work completes");
        assert_eq!(slots.available_permits(), 1);
    }

    #[tokio::test]
    async fn blocking_work_slot_is_released_after_worker_panic() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::new(
            Arc::clone(&slots)
                .acquire_owned()
                .await
                .expect("work semaphore is open"),
        );
        let task = spawn_bounded_remote_index_work(permit, || panic!("injected worker panic"));
        assert!(task.await.is_err());
        assert_eq!(slots.available_permits(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn withheld_local_owner_response_hits_the_configured_socket_deadline() {
        use std::os::unix::fs::PermissionsExt as _;
        use std::time::Instant;

        let endpoint = std::env::temp_dir().join(format!(
            "ri-{}-{}.sock",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("wall clock")
                .as_nanos()
        ));
        let listener =
            backend_replication::LocalListener::bind(&endpoint).expect("bind withheld test owner");
        fs::set_permissions(&endpoint, fs::Permissions::from_mode(0o600))
            .expect("make owner socket private");
        let (accepted, accepted_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept local client");
            accepted.send(()).expect("signal accepted local client");
            let _ = release_rx.recv_timeout(Duration::from_secs(2));
            drop(stream);
        });

        let mut client = connect_local_control_with_deadlines(
            &endpoint,
            Duration::from_secs(1),
            Duration::from_millis(100),
        )
        .expect("connect and authenticate local owner");
        accepted_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("local owner accepted the request stream");
        let start = Instant::now();
        let result = client.request(&LocalControlRequest::Complete {
            request_id: 1,
            work_key: [1; 32],
            output: [2; 32],
            ordinal: 1,
            fence: [3; 32],
        });
        assert!(result.is_err(), "withheld owner response must time out");
        assert!(start.elapsed() < Duration::from_secs(1));

        release.send(()).expect("release withheld owner thread");
        server.join().expect("join withheld owner thread");
        fs::remove_file(endpoint).expect("remove withheld owner socket");
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
        let _response_permit = usage.admit_response(&capability, 8).expect("reserve bytes");
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
            let ledger = open_usage_ledger(path.clone()).expect("open ledger capability");
            let _lock = lock_usage_ledger(&ledger).expect("ledger lock");
            let mut entries = read_usage_ledger(&ledger, owner).expect("read ledger");
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

        let outside = root.join("outside");
        fs::create_dir(&outside).expect("create ancestor symlink target");
        let ancestor_link = root.join("ancestor-link");
        symlink(&outside, &ancestor_link).expect("symlink ledger ancestor");
        assert!(
            RemoteIndexUsage::open(
                ancestor_link.join("nested-ledger.v1"),
                capability.claims.server
            )
            .is_err()
        );

        assert_eq!(fs::read(&target).expect("read target"), b"sensitive target");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(all(unix, not(target_vendor = "apple")))]
    #[test]
    fn grant_ledger_and_lock_refuse_fifos_without_waiting_for_a_writer() {
        use rustix::fs::{CWD, Mode, mkfifoat};

        let root = scratch("fifo");
        let capability = capability(4, 64);
        let ledger = root.join("fifo-ledger.v1");
        mkfifoat(CWD, &ledger, Mode::from_bits_truncate(0o600)).expect("create ledger FIFO");
        assert!(RemoteIndexUsage::open(&ledger, capability.claims.server).is_err());

        let lock_ledger = root.join("fifo-lock-ledger.v1");
        let lock = root.join("fifo-lock-ledger.v1.lock");
        mkfifoat(CWD, &lock, Mode::from_bits_truncate(0o600)).expect("create lock FIFO");
        assert!(RemoteIndexUsage::open(&lock_ledger, capability.claims.server).is_err());
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

    #[cfg(unix)]
    #[test]
    fn grant_ledger_updates_stay_under_the_pinned_parent_after_path_replacement() {
        let root = scratch("pinned-parent");
        let parent = root.join("state");
        fs::create_dir(&parent).expect("create ledger state directory");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))
            .expect("make original ledger state directory private");
        let path = parent.join("remote-index-grants.v1");
        let capability = capability(4, 64);
        let usage = RemoteIndexUsage::open(&path, capability.claims.server)
            .expect("pin ledger state directory");

        let moved = root.join("moved-state");
        fs::rename(&parent, &moved).expect("move original state directory");
        fs::create_dir(&parent).expect("replace original path with fresh directory");
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o700))
            .expect("protect replacement directory");

        usage
            .register_capability(&capability, capability.claims.issued_at_unix_ms)
            .expect("write through the retained directory handle");
        assert!(moved.join("remote-index-grants.v1").is_file());
        assert!(moved.join("remote-index-grants.v1.lock").is_file());
        assert!(!parent.join("remote-index-grants.v1").exists());
        drop(usage);

        let fresh = RemoteIndexUsage::open(&path, capability.claims.server)
            .expect("open replacement directory as a different ledger");
        assert!(fresh.list().expect("list replacement ledger").is_empty());
        drop(fresh);
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

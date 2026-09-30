//! Bounded direct-to-S3-compatible storage for immutable compiler objects.
//!
//! The index owner mints one expiring, object-scoped PUT capability and, when
//! cold reads are needed, a separate GET capability. This crate holds no S3
//! credentials and has no API for selecting a semantic head. Every PUT is a
//! single bounded object request with a signed `If-None-Match: *` and S3
//! `x-amz-checksum-sha256`; application object IDs are still checked with
//! `backend-store`'s object-envelope ABI, independently of the transport
//! checksum and any ETag returned by the service.
//!
//! Single-object storage remains available for simple callers. The pack route
//! batches many typed envelopes into one immutable S3 object and authenticates
//! each complete envelope extent with a Merkle proof before store admission.
//! Application envelopes are never mapped to S3 multipart parts; packs remain
//! bounded single-PutObject requests and their page proof is independent of
//! S3's `206`/`Content-Range` transport metadata.
//!
//! The API intentionally emits object receipts only. Direct S3 storage of a
//! complete compiler closure is not complete until the index-owned closure
//! verifier composes these receipts with a checked closure manifest; neither
//! an object receipt nor this crate can advance a selected head.
//!
//! Presigned URLs are bearer secrets. URL strings are redacted from `Debug`
//! and errors, redirects are disabled, environment proxies are disabled, and
//! every capability origin must be in the route's configured allowlist.
//! HTTP is rejected except for explicit loopback-only test endpoints.

#![forbid(unsafe_code)]

#[cfg(feature = "test-support")]
pub mod test_support;

use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::IpAddr,
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use backend_store::{
    ObjectId, RelationAdmissionRegistry, StoreError, TypedObject, UntrustedObjectId,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DEFAULT_MAX_ACK_BYTES: u64 = 8 * 1024;
const DEFAULT_MAX_ATTEMPTS: u8 = 3;
const SINGLE_OBJECT_CEILING_BYTES: u64 = 64 * 1024 * 1024;
const PACK_ROUTE_CEILING_BYTES: u64 = backend_store::artifact_pack::MAX_ARTIFACT_PACK_BYTES;
const MAX_RETRY_DELAY: Duration = Duration::from_secs(2);
const REQUIRED_IF_NONE_MATCH: &str = "*";
const REQUIRED_CONTENT_ENCODING: &str = "identity";
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// Fence copied from the index owner's current compile assignment.
///
/// A matching storage receipt remains storage evidence only. The index owner
/// must compare this fence with its current assignment and validate the
/// complete closure before selecting any semantic root.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct WorkFence {
    /// Tenant/workspace namespace fixed by the index owner.
    pub namespace_id: [u8; 16],
    /// Stable identity of the scheduled work cell.
    pub work_id: [u8; 16],
    /// Monotonically changing attempt fence for that work cell.
    pub attempt: u64,
    /// Exact persisted attempt/publication fence from the index authority.
    /// This is an opaque fencing token, separate from the output identity.
    pub fence: [u8; 32],
    /// Exact result closure root claimed by the current attempt.
    pub closure_root: [u8; 32],
}

/// A configured S3-compatible service origin.
///
/// Production origins must use HTTPS. Loopback HTTP is constructed only by
/// unit tests so a real local HTTP server can exercise request semantics.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct S3Endpoint {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl S3Endpoint {
    /// Parses an HTTPS origin with no path, query, fragment, or user info.
    ///
    /// # Errors
    /// Returns [`RemoteStoreError::Capability`] for a malformed or non-HTTPS
    /// endpoint. Plain HTTP is accepted only by the private loopback test
    /// helper.
    pub fn https(origin: &str) -> Result<Self, RemoteStoreError> {
        Self::parse(origin, false)
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Constructs an HTTP endpoint for a loopback-only test server.
    ///
    /// This helper rejects non-loopback hosts; production callers must use [`Self::https`].
    ///
    /// # Errors
    /// Returns [`RemoteStoreError::Capability`] for a malformed endpoint or non-loopback host.
    pub fn loopback_http(origin: &str) -> Result<Self, RemoteStoreError> {
        Self::parse(origin, true)
    }

    fn parse(origin: &str, allow_loopback_http: bool) -> Result<Self, RemoteStoreError> {
        let uri = origin
            .parse::<ureq::http::Uri>()
            .map_err(|_| RemoteStoreError::Capability)?;
        let scheme = uri
            .scheme_str()
            .ok_or(RemoteStoreError::Capability)?
            .to_ascii_lowercase();
        let authority = uri.authority().ok_or(RemoteStoreError::Capability)?;
        if authority.as_str().contains('@')
            || uri
                .path_and_query()
                .is_some_and(|path| path.path() != "/" || path.query().is_some())
        {
            return Err(RemoteStoreError::Capability);
        }
        let host = authority.host().to_ascii_lowercase();
        let port = authority.port_u16();
        let secure = scheme == "https";
        let loopback = match host.as_str() {
            "localhost" => true,
            host => host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback()),
        };
        if !secure && !(allow_loopback_http && scheme == "http" && loopback) {
            return Err(RemoteStoreError::Capability);
        }
        if scheme != "https" && scheme != "http" {
            return Err(RemoteStoreError::Capability);
        }
        let port = port.or_else(|| match scheme.as_str() {
            "https" => Some(443),
            "http" => Some(80),
            _ => None,
        });
        Ok(Self { scheme, host, port })
    }

    fn matches_uri(&self, uri: &ureq::http::Uri) -> bool {
        let Some(authority) = uri.authority() else {
            return false;
        };
        uri.scheme_str()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case(&self.scheme))
            && authority.host().eq_ignore_ascii_case(&self.host)
            && authority.port_u16().or_else(|| match self.scheme.as_str() {
                "https" => Some(443),
                "http" => Some(80),
                _ => None,
            }) == self.port
            && !authority.as_str().contains('@')
    }
}

/// HTTP and resource limits for one route instance.
#[derive(Clone, Debug)]
pub struct S3RouteConfig {
    endpoints: BTreeSet<S3Endpoint>,
    max_object_bytes: u64,
    max_attempts: u8,
    retry_delay: Duration,
    max_ack_bytes: u64,
}

impl S3RouteConfig {
    /// Creates a route allowlisting one or more HTTPS object-store origins.
    ///
    /// `max_attempts` includes the initial request and is clamped to 1–3.
    /// This is the largest physical S3 object staged on disk for checksums.
    /// The simple object route is capped at 64 MiB. Use
    /// [`Self::new_for_pack_route`] for packs, whose 160 MiB ceiling can carry
    /// one 128 MiB canonical NXFI image; ordinary pack construction still
    /// recommends flushing around 8 MiB. Packs use one ordinary PutObject and
    /// do not map application envelopes onto S3 multipart parts.
    ///
    /// # Errors
    /// Returns [`RemoteStoreError::Capability`] if there are no allowed
    /// endpoints or any endpoint is malformed.
    pub fn new(
        endpoints: impl IntoIterator<Item = S3Endpoint>,
        max_object_bytes: u64,
        max_attempts: u8,
        retry_delay: Duration,
    ) -> Result<Self, RemoteStoreError> {
        Self::new_bounded(
            endpoints,
            max_object_bytes,
            max_attempts,
            retry_delay,
            SINGLE_OBJECT_CEILING_BYTES,
        )
    }

    /// Creates a route configuration for the immutable pack protocol.
    ///
    /// Pack sizes are storage-neutral and bounded at 160 MiB so one pack can
    /// carry a full 128 MiB NXFI image. The private-file route still streams
    /// the pack body and stores fetched member extents on disk.
    pub fn new_for_pack_route(
        endpoints: impl IntoIterator<Item = S3Endpoint>,
        max_pack_bytes: u64,
        max_attempts: u8,
        retry_delay: Duration,
    ) -> Result<Self, RemoteStoreError> {
        Self::new_bounded(
            endpoints,
            max_pack_bytes,
            max_attempts,
            retry_delay,
            PACK_ROUTE_CEILING_BYTES,
        )
    }

    fn new_bounded(
        endpoints: impl IntoIterator<Item = S3Endpoint>,
        max_object_bytes: u64,
        max_attempts: u8,
        retry_delay: Duration,
        ceiling_bytes: u64,
    ) -> Result<Self, RemoteStoreError> {
        let endpoints: BTreeSet<_> = endpoints.into_iter().collect();
        if endpoints.is_empty() || max_object_bytes == 0 || max_object_bytes > ceiling_bytes {
            return Err(RemoteStoreError::Capability);
        }
        Ok(Self {
            endpoints,
            max_object_bytes,
            max_attempts: max_attempts.clamp(1, DEFAULT_MAX_ATTEMPTS),
            retry_delay: retry_delay.min(MAX_RETRY_DELAY),
            max_ack_bytes: DEFAULT_MAX_ACK_BYTES,
        })
    }
}

/// A signed, object-specific PutObject capability issued by the index owner.
///
/// It contains no AWS credentials. `if_none_match_signed` and
/// `checksum_signed` record the owner's promise that both required request
/// headers are in the SigV4 signed-header set. The URL itself is still the
/// bearer authority; the index must bind it to this object's key and the work
/// fence before sending it to the compiler node.
#[derive(Clone, Deserialize, Serialize)]
pub struct UploadCapability {
    url: String,
    object_id: [u8; 32],
    object_bytes: u64,
    sha256: [u8; 32],
    expires_at_unix_seconds: u64,
    fence: WorkFence,
    if_none_match_signed: bool,
    checksum_signed: bool,
}

impl UploadCapability {
    /// Constructs capability data received from the trusted index owner.
    ///
    /// Construction is data-only because serialized grants can bypass this
    /// method. The route revalidates URL, endpoint, expiry, object identity,
    /// size, nonzero persisted fence, and signed-header claims immediately
    /// before every HTTP request.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        object_id: ObjectId,
        object_bytes: u64,
        sha256: [u8; 32],
        expires_at_unix_seconds: u64,
        fence: WorkFence,
        if_none_match_signed: bool,
        checksum_signed: bool,
    ) -> Self {
        Self {
            url: url.into(),
            object_id: *object_id.as_bytes(),
            object_bytes,
            sha256,
            expires_at_unix_seconds,
            fence,
            if_none_match_signed,
            checksum_signed,
        }
    }

    /// Returns the attempt fence bound by the index owner.
    #[must_use]
    pub const fn fence(&self) -> WorkFence {
        self.fence
    }
}

impl fmt::Debug for UploadCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UploadCapability")
            .field("url", &"[REDACTED]")
            .field("object_id", &self.object_id)
            .field("object_bytes", &self.object_bytes)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("fence", &self.fence)
            .finish_non_exhaustive()
    }
}

/// Optional exact range a GET capability was minted to read.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AllowedRange {
    /// Inclusive first byte in the object envelope.
    pub start: u64,
    /// Inclusive final byte in the object envelope.
    pub end_inclusive: u64,
}

/// A signed, object-specific GetObject capability issued by the index owner.
#[derive(Clone, Deserialize, Serialize)]
pub struct ReadCapability {
    url: String,
    object_id: [u8; 32],
    object_bytes: u64,
    expires_at_unix_seconds: u64,
    fence: WorkFence,
    exact_range: Option<AllowedRange>,
    range_signed: bool,
}

impl ReadCapability {
    /// Constructs capability data received from the trusted index owner.
    ///
    /// When `exact_range` is set, the route will request exactly those bytes
    /// and requires the owner to mark the Range request header signed. The
    /// route validates the complete grant, including its nonzero persisted
    /// fence, when it is used.
    #[must_use]
    pub fn new(
        url: impl Into<String>,
        object_id: ObjectId,
        object_bytes: u64,
        expires_at_unix_seconds: u64,
        fence: WorkFence,
        exact_range: Option<AllowedRange>,
        range_signed: bool,
    ) -> Self {
        Self {
            url: url.into(),
            object_id: *object_id.as_bytes(),
            object_bytes,
            expires_at_unix_seconds,
            fence,
            exact_range,
            range_signed,
        }
    }

    /// Returns the attempt fence bound by the index owner.
    #[must_use]
    pub const fn fence(&self) -> WorkFence {
        self.fence
    }
}

impl fmt::Debug for ReadCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReadCapability")
            .field("url", &"[REDACTED]")
            .field("object_id", &self.object_id)
            .field("object_bytes", &self.object_bytes)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("fence", &self.fence)
            .field("exact_range", &self.exact_range)
            .finish_non_exhaustive()
    }
}

/// A complete store-owned physical envelope ready for a bounded PUT.
///
/// The contained ID comes from `TypedObject::id`; envelope encoding is
/// delegated to backend-store so this crate does not copy or fork its
/// commitment ABI.
pub struct ObjectEnvelope {
    id: ObjectId,
    file: TempEnvelope,
    sha256: [u8; 32],
}

impl ObjectEnvelope {
    /// Encodes one already admitted typed object under backend-store's exact
    /// physical object envelope and enforces the configured object ceiling.
    ///
    /// # Errors
    /// Returns a bounds or store admission error if the envelope exceeds the
    /// route limit or cannot be encoded.
    pub fn from_typed(
        object: &TypedObject,
        max_object_bytes: u64,
    ) -> Result<Self, RemoteStoreError> {
        let id = object.id();
        let file = TempEnvelope::from_typed(object, max_object_bytes)?;
        Ok(Self {
            id,
            sha256: file.sha256,
            file,
        })
    }

    /// Returns the checked physical object ID.
    #[must_use]
    pub const fn id(&self) -> ObjectId {
        self.id
    }

    /// Returns the immutable envelope byte length.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.file.len
    }

    /// Whether this envelope has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.file.len == 0
    }

    /// Returns the SHA-256 checksum sent in the S3 REST checksum header.
    #[must_use]
    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

impl fmt::Debug for ObjectEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectEnvelope")
            .field("id", &self.id)
            .field("bytes", &self.file.len)
            .field("sha256", &self.sha256)
            .finish()
    }
}

/// Receipt for successful physical storage of one immutable envelope.
///
/// This receipt is not a checked closure receipt and cannot select a semantic
/// head. The index owner must bind all members into a verified closure and
/// compare `fence` with its current attempt before publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StoredObjectReceipt {
    object_id: ObjectId,
    object_bytes: u64,
    sha256: [u8; 32],
    fence: WorkFence,
    reused_after_412: bool,
}

impl StoredObjectReceipt {
    /// Returns the object ID acknowledged by the storage route.
    #[must_use]
    pub const fn object_id(&self) -> ObjectId {
        self.object_id
    }

    /// Returns the exact stored envelope length.
    #[must_use]
    pub const fn object_bytes(&self) -> u64 {
        self.object_bytes
    }

    /// Returns the independently calculated SHA-256 transport checksum.
    #[must_use]
    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// Returns the index assignment fence bound by the grant.
    #[must_use]
    pub const fn fence(&self) -> WorkFence {
        self.fence
    }

    /// True when an existing object passed a full GET and exact-byte check
    /// after S3 returned `412 Precondition Failed`.
    #[must_use]
    pub const fn reused_after_412(&self) -> bool {
        self.reused_after_412
    }
}

/// Raw byte range returned after validating HTTP 206 metadata.
///
/// The bytes are deliberately unverified. A correct `Content-Range` proves
/// only which representation extent the endpoint returned; it does not prove
/// that the extent belongs to the requested content address. Require a page
/// checksum, Bao proof, or equivalent authenticated outboard before decoding
/// it as semantic data.
pub struct ObjectByteRange {
    object_id: [u8; 32],
    total_bytes: u64,
    range: AllowedRange,
    fence: WorkFence,
    bytes: Vec<u8>,
}

impl ObjectByteRange {
    /// Requested object ID.
    #[must_use]
    pub const fn object_id(&self) -> &[u8; 32] {
        &self.object_id
    }

    /// Total envelope size returned in `Content-Range`.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Inclusive byte range returned by the service.
    #[must_use]
    pub const fn range(&self) -> AllowedRange {
        self.range
    }

    /// Work fence bound to the read capability.
    #[must_use]
    pub const fn fence(&self) -> WorkFence {
        self.fence
    }

    /// Unverified range bytes. A separate proof is required before use.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

impl fmt::Debug for ObjectByteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ObjectByteRange")
            .field("object_id", &self.object_id)
            .field("total_bytes", &self.total_bytes)
            .field("range", &self.range)
            .field("fence", &self.fence)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// One admitted full object returned by a remote cold read.
pub struct CheckedRemoteObject {
    id: ObjectId,
    file: TempEnvelope,
    sha256: [u8; 32],
    fence: WorkFence,
}

impl CheckedRemoteObject {
    /// Returns the ID recomputed by backend-store's envelope verifier.
    #[must_use]
    pub const fn id(&self) -> ObjectId {
        self.id
    }

    /// Opens the checked physical object envelope at byte zero.
    #[must_use]
    pub fn open_envelope(&self) -> Result<File, RemoteStoreError> {
        self.file.open()
    }

    /// Returns the computed transport checksum for diagnostics/accounting.
    #[must_use]
    pub const fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }

    /// Returns the attempt fence attached to this read grant.
    #[must_use]
    pub const fn fence(&self) -> WorkFence {
        self.fence
    }
}

/// Failures while using an S3-compatible immutable object capability.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum RemoteStoreError {
    /// Capability shape, scope, URL, origin, expiry, or signed header claim failed.
    #[error("invalid or out-of-scope object-store capability")]
    Capability,
    /// Caller fence differs from the work fence attached by the index owner.
    #[error("object-store capability is fenced to another attempt")]
    StaleFence,
    /// Object exceeds the configured byte limit or an arithmetic bound.
    #[error("object-store byte bound exceeded")]
    Bounds,
    /// The source or server payload does not have the claimed object identity.
    #[error("object identity or checksum validation failed")]
    Identity,
    /// The service returned a nonconforming status, length, range, or encoding.
    #[error("object-store HTTP response violated the transfer contract")]
    Protocol,
    /// The object was already present but no verified full read was authorized.
    #[error("existing object could not be verified after conditional PUT")]
    ExistingUnverified,
    /// The service returned a response that is not retryable or exhausted retries.
    #[error("object-store request failed after bounded retry policy")]
    Unavailable,
    /// Backend-store rejected the envelope or canonical schema identity.
    #[error("backend-store rejected the object envelope")]
    Store(StoreError),
}

impl From<StoreError> for RemoteStoreError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

fn map_remote_admission_error(error: StoreError) -> RemoteStoreError {
    match error {
        StoreError::Corrupt => RemoteStoreError::Identity,
        StoreError::Bounds => RemoteStoreError::Bounds,
        StoreError::Io(_) | StoreError::TemporaryIo(_) => RemoteStoreError::Unavailable,
        error => RemoteStoreError::Store(error),
    }
}

/// Direct object access over signed capabilities.
#[derive(Clone)]
pub struct S3ObjectRoute {
    config: S3RouteConfig,
    agent: ureq::Agent,
}

impl fmt::Debug for S3ObjectRoute {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("S3ObjectRoute")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl S3ObjectRoute {
    /// Creates an HTTP client that rejects redirects and bypasses ambient
    /// proxy configuration so bearer URLs do not leak through a proxy.
    #[must_use]
    pub fn new(config: S3RouteConfig) -> Self {
        let agent_config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(60)))
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .proxy(None)
            .build();
        Self {
            config,
            agent: agent_config.new_agent(),
        }
    }

    /// Retries only idempotent GETs after a transient transport or service
    /// failure. The complete response still passes the caller's identity and
    /// range checks; a malformed successful response is never retried.
    fn get_response(
        &self,
        url: &str,
        range: Option<&str>,
    ) -> Result<ureq::http::Response<ureq::Body>, RemoteStoreError> {
        let mut attempt = 0;
        loop {
            let mut request = self
                .agent
                .get(url)
                .header("accept-encoding", REQUIRED_CONTENT_ENCODING);
            if let Some(range) = range {
                request = request.header("range", range);
            }
            match request.call() {
                Ok(response) if is_retryable_status(response.status().as_u16()) => {
                    if self.retry_or_stop(&mut attempt) {
                        continue;
                    }
                    return Err(RemoteStoreError::Unavailable);
                }
                Ok(response) => return Ok(response),
                Err(error) => {
                    let error = map_ureq_error(error);
                    if error == RemoteStoreError::Unavailable && self.retry_or_stop(&mut attempt) {
                        continue;
                    }
                    return Err(error);
                }
            }
        }
    }

    /// Streams one verified immutable object to S3 with one conditional PUT.
    ///
    /// The bounded envelope is regenerated for each retry. A `412` is only
    /// treated as idempotent success when `existing_read` authorizes a full
    /// GET whose complete object envelope passes store admission and whose
    /// bytes exactly match the intended upload. The service's ETag is ignored.
    ///
    /// # Errors
    /// Returns an error before network access for mismatched identity, length,
    /// SHA-256, fence, expiry, or an invalid URL. Transient transport and
    /// conditional-conflict responses are retried only within the configured
    /// attempt bound.
    pub fn put_object(
        &self,
        object: &TypedObject,
        grant: &UploadCapability,
        fence: WorkFence,
        existing_read: Option<&ReadCapability>,
        registry: &RelationAdmissionRegistry,
    ) -> Result<StoredObjectReceipt, RemoteStoreError> {
        self.validate_upload_grant(grant, fence)?;
        if let Some(read) = existing_read {
            if read.object_id != grant.object_id || read.object_bytes != grant.object_bytes {
                return Err(RemoteStoreError::Capability);
            }
            self.validate_read_grant(read, fence, None)?;
        }
        let envelope = ObjectEnvelope::from_typed(object, self.config.max_object_bytes)?;
        if envelope.id.as_bytes() != &grant.object_id
            || envelope.len() != grant.object_bytes
            || envelope.sha256 != grant.sha256
        {
            return Err(RemoteStoreError::Identity);
        }
        let checksum = BASE64.encode(envelope.sha256);
        let mut attempt = 0;
        loop {
            self.validate_upload_grant(grant, fence)?;
            let result = self
                .agent
                .put(&grant.url)
                .header("if-none-match", REQUIRED_IF_NONE_MATCH)
                .header("x-amz-checksum-sha256", &checksum)
                .header("accept-encoding", REQUIRED_CONTENT_ENCODING)
                .send(envelope.file.open()?);
            match result {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if matches!(status, 200 | 201 | 204) {
                        check_identity_response(&response)?;
                        drain_ack(response.body_mut(), self.config.max_ack_bytes)?;
                        return Ok(StoredObjectReceipt {
                            object_id: envelope.id,
                            object_bytes: grant.object_bytes,
                            sha256: grant.sha256,
                            fence,
                            reused_after_412: false,
                        });
                    }
                    if status == 412 {
                        let read = existing_read.ok_or(RemoteStoreError::ExistingUnverified)?;
                        let checked = self.get_full_object(read, fence, registry)?;
                        if checked.id != envelope.id
                            || checked.sha256 != envelope.sha256
                            || !checked.file.same_bytes(&envelope.file)?
                        {
                            return Err(RemoteStoreError::Identity);
                        }
                        return Ok(StoredObjectReceipt {
                            object_id: envelope.id,
                            object_bytes: grant.object_bytes,
                            sha256: grant.sha256,
                            fence,
                            reused_after_412: true,
                        });
                    }
                    if status == 409 || is_retryable_status(status) {
                        if self.retry_or_stop(&mut attempt) {
                            continue;
                        }
                        return Err(RemoteStoreError::Unavailable);
                    }
                    return Err(RemoteStoreError::Protocol);
                }
                Err(error) => {
                    // ureq's error can contain request details. It is never
                    // formatted or returned because the request URI is a
                    // bearer secret.
                    if is_redirect_error(&error) {
                        return Err(RemoteStoreError::Protocol);
                    }
                    if self.retry_or_stop(&mut attempt) {
                        continue;
                    }
                    return Err(RemoteStoreError::Unavailable);
                }
            }
        }
    }

    /// Downloads and admits a complete immutable object envelope from cold
    /// storage. The physical `ObjectId` is recomputed by backend-store.
    ///
    /// # Errors
    /// Returns a protocol or identity error if the service returns a partial,
    /// oversized, transformed, malformed, or mismatched object.
    pub fn get_full_object(
        &self,
        grant: &ReadCapability,
        fence: WorkFence,
        registry: &RelationAdmissionRegistry,
    ) -> Result<CheckedRemoteObject, RemoteStoreError> {
        self.validate_read_grant(grant, fence, None)?;
        if grant.exact_range.is_some() {
            return Err(RemoteStoreError::Capability);
        }
        let mut response = self.get_response(&grant.url, None)?;
        if response.status().as_u16() != 200 {
            return Err(RemoteStoreError::Protocol);
        }
        check_identity_response(&response)?;
        let declared_length = parse_content_length(&response)?;
        if declared_length != grant.object_bytes || declared_length > self.config.max_object_bytes {
            return Err(RemoteStoreError::Identity);
        }
        let file = TempEnvelope::from_body(response.body_mut(), declared_length)?;
        if file.len != declared_length {
            return Err(RemoteStoreError::Identity);
        }
        let expected = UntrustedObjectId::from_bytes(grant.object_id);
        let mut input = file.open()?;
        let admitted = backend_store::admit_object_envelope(
            &mut input,
            usize::try_from(self.config.max_object_bytes).map_err(|_| RemoteStoreError::Bounds)?,
            registry,
            expected,
        )
        .map_err(map_remote_admission_error)?;
        let id = admitted.id();
        if id.as_bytes() != &grant.object_id {
            return Err(RemoteStoreError::Identity);
        }
        file.verify_snapshot()?;
        let sha256 = file.sha256;
        Ok(CheckedRemoteObject {
            id,
            file,
            sha256,
            fence,
        })
    }

    /// Fetches one exact HTTP byte range and validates status, content length,
    /// content encoding, and `Content-Range` metadata. Returned bytes remain
    /// unverified until a page checksum or Merkle/Bao proof is applied.
    ///
    /// # Errors
    /// Returns an error for malformed or out-of-bound ranges, capability
    /// scope mismatches, an expired grant, or nonconforming HTTP responses.
    pub fn get_range(
        &self,
        grant: &ReadCapability,
        requested: AllowedRange,
        fence: WorkFence,
    ) -> Result<ObjectByteRange, RemoteStoreError> {
        self.validate_read_grant(grant, fence, Some(requested))?;
        if requested.start > requested.end_inclusive
            || requested.end_inclusive >= grant.object_bytes
        {
            return Err(RemoteStoreError::Bounds);
        }
        let span = requested
            .end_inclusive
            .checked_sub(requested.start)
            .and_then(|size| size.checked_add(1))
            .ok_or(RemoteStoreError::Bounds)?;
        if span > self.config.max_object_bytes {
            return Err(RemoteStoreError::Bounds);
        }
        let range_header = format!("bytes={}-{}", requested.start, requested.end_inclusive);
        let mut response = self.get_response(&grant.url, Some(&range_header))?;
        if response.status().as_u16() != 206 {
            return Err(RemoteStoreError::Protocol);
        }
        check_identity_response(&response)?;
        let content_range = response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .ok_or(RemoteStoreError::Protocol)?;
        if parse_content_range(content_range)? != (requested, grant.object_bytes) {
            return Err(RemoteStoreError::Protocol);
        }
        if parse_content_length(&response)? != span {
            return Err(RemoteStoreError::Protocol);
        }
        let bytes = read_bounded(response.body_mut(), span)?;
        if u64::try_from(bytes.len()).map_err(|_| RemoteStoreError::Bounds)? != span {
            return Err(RemoteStoreError::Protocol);
        }
        Ok(ObjectByteRange {
            object_id: grant.object_id,
            total_bytes: grant.object_bytes,
            range: requested,
            fence,
            bytes,
        })
    }

    fn validate_upload_grant(
        &self,
        grant: &UploadCapability,
        fence: WorkFence,
    ) -> Result<(), RemoteStoreError> {
        if grant.fence != fence {
            return Err(RemoteStoreError::StaleFence);
        }
        if fence.fence == [0; 32] {
            return Err(RemoteStoreError::Capability);
        }
        if grant.object_bytes == 0
            || grant.object_bytes > self.config.max_object_bytes
            || !grant.if_none_match_signed
            || !grant.checksum_signed
        {
            return Err(RemoteStoreError::Capability);
        }
        self.validate_url(&grant.url)?;
        if !url_signs_headers(
            &grant.url,
            &["host", "if-none-match", "x-amz-checksum-sha256"],
        ) {
            return Err(RemoteStoreError::Capability);
        }
        ensure_unexpired(grant.expires_at_unix_seconds)
    }

    fn validate_read_grant(
        &self,
        grant: &ReadCapability,
        fence: WorkFence,
        requested: Option<AllowedRange>,
    ) -> Result<(), RemoteStoreError> {
        if grant.fence != fence {
            return Err(RemoteStoreError::StaleFence);
        }
        if fence.fence == [0; 32] {
            return Err(RemoteStoreError::Capability);
        }
        if grant.object_bytes == 0 || grant.object_bytes > self.config.max_object_bytes {
            return Err(RemoteStoreError::Capability);
        }
        match (grant.exact_range, requested) {
            (Some(allowed), Some(requested)) if allowed == requested && grant.range_signed => {}
            (Some(_), _) => return Err(RemoteStoreError::Capability),
            (None, Some(_)) => {}
            (None, None) => {}
        }
        self.validate_url(&grant.url)?;
        if grant.exact_range.is_some()
            && (!grant.range_signed || !url_signs_headers(&grant.url, &["host", "range"]))
        {
            return Err(RemoteStoreError::Capability);
        }
        ensure_unexpired(grant.expires_at_unix_seconds)
    }

    fn validate_url(&self, url: &str) -> Result<(), RemoteStoreError> {
        let uri = url
            .parse::<ureq::http::Uri>()
            .map_err(|_| RemoteStoreError::Capability)?;
        if uri
            .path_and_query()
            .and_then(|path| path.query())
            .is_none_or(str::is_empty)
            || uri
                .path_and_query()
                .is_some_and(|path| path.path().is_empty())
            || !url_has_query_parameter(&uri, "X-Amz-Signature")
        {
            return Err(RemoteStoreError::Capability);
        }
        if self
            .config
            .endpoints
            .iter()
            .any(|endpoint| endpoint.matches_uri(&uri))
        {
            Ok(())
        } else {
            Err(RemoteStoreError::Capability)
        }
    }

    fn retry_or_stop(&self, attempt: &mut u8) -> bool {
        *attempt = attempt.saturating_add(1);
        if *attempt >= self.config.max_attempts {
            return false;
        }
        if !self.config.retry_delay.is_zero() {
            std::thread::sleep(self.config.retry_delay);
        }
        true
    }
}

/// Checks the actual SigV4 presign query as well as the index owner's typed
/// claim. A public constructor boolean alone must not make an unsigned
/// conditional or range header appear protected by the URL signature.
fn url_signs_headers(url: &str, required: &[&str]) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    let Some(query) = uri.path_and_query().and_then(|path| path.query()) else {
        return false;
    };
    let signed_headers = query.split('&').find_map(|field| {
        let (raw_name, raw_value) = field.split_once('=')?;
        let name = percent_decode_query_component(raw_name)?;
        if !name.eq_ignore_ascii_case("X-Amz-SignedHeaders") {
            return None;
        }
        percent_decode_query_component(raw_value)
    });
    let Some(signed_headers) = signed_headers else {
        return false;
    };
    let signed_headers: BTreeSet<_> = signed_headers
        .split(';')
        .map(str::trim)
        .filter(|header| !header.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    required
        .iter()
        .all(|header| signed_headers.contains(*header))
}

fn url_has_query_parameter(uri: &ureq::http::Uri, expected_name: &str) -> bool {
    uri.path_and_query()
        .and_then(|path| path.query())
        .is_some_and(|query| {
            query.split('&').any(|field| {
                let Some((raw_name, raw_value)) = field.split_once('=') else {
                    return false;
                };
                percent_decode_query_component(raw_name)
                    .is_some_and(|name| name.eq_ignore_ascii_case(expected_name))
                    && percent_decode_query_component(raw_value)
                        .is_some_and(|value| !value.is_empty())
            })
        })
}

fn percent_decode_query_component(raw: &str) -> Option<String> {
    let raw = raw.as_bytes();
    let mut decoded = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%' {
            let high = *raw.get(index + 1)?;
            let low = *raw.get(index + 2)?;
            decoded.push((hex_nibble(high)? << 4) | hex_nibble(low)?);
            index += 3;
        } else {
            decoded.push(raw[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn ensure_unexpired(expires_at_unix_seconds: u64) -> Result<(), RemoteStoreError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RemoteStoreError::Capability)?
        .as_secs();
    if expires_at_unix_seconds <= now {
        Err(RemoteStoreError::Capability)
    } else {
        Ok(())
    }
}

fn check_identity_response(
    response: &ureq::http::Response<ureq::Body>,
) -> Result<(), RemoteStoreError> {
    if let Some(encoding) = response.headers().get("content-encoding") {
        let encoding = encoding.to_str().map_err(|_| RemoteStoreError::Protocol)?;
        if !encoding.eq_ignore_ascii_case(REQUIRED_CONTENT_ENCODING) {
            return Err(RemoteStoreError::Protocol);
        }
    }
    Ok(())
}

fn parse_content_length(
    response: &ureq::http::Response<ureq::Body>,
) -> Result<u64, RemoteStoreError> {
    response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .ok_or(RemoteStoreError::Protocol)?
        .parse::<u64>()
        .map_err(|_| RemoteStoreError::Protocol)
}

fn drain_ack(body: &mut ureq::Body, maximum: u64) -> Result<(), RemoteStoreError> {
    let mut bytes = Vec::new();
    body.as_reader()
        .take(maximum.checked_add(1).ok_or(RemoteStoreError::Bounds)?)
        .read_to_end(&mut bytes)
        .map_err(|_| RemoteStoreError::Unavailable)?;
    if u64::try_from(bytes.len()).map_err(|_| RemoteStoreError::Bounds)? > maximum {
        return Err(RemoteStoreError::Protocol);
    }
    Ok(())
}

fn read_bounded(body: &mut ureq::Body, expected: u64) -> Result<Vec<u8>, RemoteStoreError> {
    let maximum = usize::try_from(expected).map_err(|_| RemoteStoreError::Bounds)?;
    let mut bytes = Vec::with_capacity(maximum);
    body.as_reader()
        .take(expected.checked_add(1).ok_or(RemoteStoreError::Bounds)?)
        .read_to_end(&mut bytes)
        .map_err(|_| RemoteStoreError::Unavailable)?;
    if bytes.len() > maximum {
        return Err(RemoteStoreError::Bounds);
    }
    Ok(bytes)
}

fn parse_content_range(raw: &str) -> Result<(AllowedRange, u64), RemoteStoreError> {
    let rest = raw
        .strip_prefix("bytes ")
        .ok_or(RemoteStoreError::Protocol)?;
    let (range, total) = rest.split_once('/').ok_or(RemoteStoreError::Protocol)?;
    let (start, end_inclusive) = range.split_once('-').ok_or(RemoteStoreError::Protocol)?;
    let start = start.parse().map_err(|_| RemoteStoreError::Protocol)?;
    let end_inclusive = end_inclusive
        .parse()
        .map_err(|_| RemoteStoreError::Protocol)?;
    let total = total.parse().map_err(|_| RemoteStoreError::Protocol)?;
    if start > end_inclusive || end_inclusive >= total {
        return Err(RemoteStoreError::Protocol);
    }
    Ok((
        AllowedRange {
            start,
            end_inclusive,
        },
        total,
    ))
}

/// Private short-lived envelope spool. It bounds resident memory to transport
/// buffers and lets ureq send a known-length File body for S3 PutObject.
struct TempEnvelope {
    path: PathBuf,
    file: Arc<File>,
    identity: TempFileSnapshot,
    len: u64,
    sha256: [u8; 32],
    snapshot: Option<TempFileSnapshot>,
}

/// Cheap metadata captured after the spool is complete. Retained descriptors
/// prevent path replacement from redirecting reads; this snapshot detects
/// in-place writes or truncation without rehashing the complete envelope on
/// every page.
#[derive(Clone, Debug, Eq, PartialEq)]
struct TempFileSnapshot {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(unix)]
    ctime: i64,
    #[cfg(unix)]
    ctime_nsec: i64,
    #[cfg(unix)]
    nlink: u64,
    #[cfg(windows)]
    volume_serial: Option<u32>,
    #[cfg(windows)]
    file_index: Option<u64>,
    #[cfg(windows)]
    last_write_time: u64,
}

impl TempFileSnapshot {
    fn from_metadata(metadata: &fs::Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;

        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            dev: metadata.dev(),
            #[cfg(unix)]
            ino: metadata.ino(),
            #[cfg(unix)]
            ctime: metadata.ctime(),
            #[cfg(unix)]
            ctime_nsec: metadata.ctime_nsec(),
            #[cfg(unix)]
            nlink: metadata.nlink(),
            #[cfg(windows)]
            volume_serial: metadata.volume_serial_number(),
            #[cfg(windows)]
            file_index: metadata.file_index(),
            #[cfg(windows)]
            last_write_time: metadata.last_write_time(),
        }
    }

    fn same_file(&self, other: &Self) -> bool {
        #[cfg(unix)]
        {
            self.dev == other.dev && self.ino == other.ino
        }
        #[cfg(windows)]
        {
            self.volume_serial.is_some()
                && self.file_index.is_some()
                && self.volume_serial == other.volume_serial
                && self.file_index == other.file_index
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = other;
            false
        }
    }
}

impl TempEnvelope {
    fn from_typed(object: &TypedObject, maximum_bytes: u64) -> Result<Self, RemoteStoreError> {
        let (mut temp, file) = Self::create()?;
        let mut writer = HashingWriter::new(file);
        backend_store::write_object_envelope(
            object,
            &mut writer,
            usize::try_from(maximum_bytes).map_err(|_| RemoteStoreError::Bounds)?,
        )?;
        let (mut file, len, sha256) = writer.finish();
        file.flush().map_err(|_| RemoteStoreError::Unavailable)?;
        if len == 0 || len > maximum_bytes {
            return Err(RemoteStoreError::Bounds);
        }
        drop(file);
        temp.len = len;
        temp.sha256 = sha256;
        temp.capture_snapshot()?;
        Ok(temp)
    }

    fn from_body(body: &mut ureq::Body, expected_bytes: u64) -> Result<Self, RemoteStoreError> {
        let (mut temp, file) = Self::create()?;
        let mut writer = HashingWriter::new(file);
        let copied = io::copy(
            &mut body.as_reader().take(
                expected_bytes
                    .checked_add(1)
                    .ok_or(RemoteStoreError::Bounds)?,
            ),
            &mut writer,
        )
        .map_err(|_| RemoteStoreError::Unavailable)?;
        let (mut file, len, sha256) = writer.finish();
        file.flush().map_err(|_| RemoteStoreError::Unavailable)?;
        if copied != len || len != expected_bytes {
            return Err(RemoteStoreError::Identity);
        }
        drop(file);
        temp.len = len;
        temp.sha256 = sha256;
        temp.capture_snapshot()?;
        Ok(temp)
    }

    fn create() -> Result<(Self, File), RemoteStoreError> {
        let directory = std::env::temp_dir();
        for _ in 0..16 {
            let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!(
                "backend-store-s3-{}-{sequence}.tmp",
                std::process::id()
            ));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    let retained = file
                        .try_clone()
                        .map_err(|_| RemoteStoreError::Unavailable)?;
                    let identity = TempFileSnapshot::from_metadata(
                        &retained
                            .metadata()
                            .map_err(|_| RemoteStoreError::Unavailable)?,
                    );
                    return Ok((
                        Self {
                            path,
                            file: Arc::new(retained),
                            identity,
                            len: 0,
                            sha256: [0; 32],
                            snapshot: None,
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(RemoteStoreError::Unavailable),
            }
        }
        Err(RemoteStoreError::Unavailable)
    }

    fn open(&self) -> Result<File, RemoteStoreError> {
        self.verify_snapshot()?;
        let file = File::open(&self.path).map_err(|_| RemoteStoreError::Unavailable)?;
        let opened = TempFileSnapshot::from_metadata(
            &file.metadata().map_err(|_| RemoteStoreError::Unavailable)?,
        );
        if !self.identity.same_file(&opened)
            || self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| *snapshot != opened)
        {
            return Err(RemoteStoreError::Identity);
        }
        Ok(file)
    }

    fn capture_snapshot(&mut self) -> Result<(), RemoteStoreError> {
        let metadata = self
            .file
            .metadata()
            .map_err(|_| RemoteStoreError::Unavailable)?;
        if metadata.len() != self.len {
            return Err(RemoteStoreError::Identity);
        }
        self.snapshot = Some(TempFileSnapshot::from_metadata(&metadata));
        Ok(())
    }

    fn verify_snapshot(&self) -> Result<(), RemoteStoreError> {
        let Some(expected) = self.snapshot.as_ref() else {
            return Ok(());
        };
        let metadata = self
            .file
            .metadata()
            .map_err(|_| RemoteStoreError::Unavailable)?;
        if TempFileSnapshot::from_metadata(&metadata) != *expected {
            return Err(RemoteStoreError::Identity);
        }
        let path_metadata =
            fs::symlink_metadata(&self.path).map_err(|_| RemoteStoreError::Identity)?;
        let path_snapshot = TempFileSnapshot::from_metadata(&path_metadata);
        if !path_metadata.file_type().is_file() || !self.identity.same_file(&path_snapshot) {
            return Err(RemoteStoreError::Identity);
        }
        Ok(())
    }

    fn read_exact_at(&self, offset: u64, output: &mut [u8]) -> Result<(), RemoteStoreError> {
        self.verify_snapshot()?;
        let mut consumed = 0_usize;
        while consumed < output.len() {
            let read = read_file_at(
                &self.file,
                &mut output[consumed..],
                offset
                    .checked_add(u64::try_from(consumed).map_err(|_| RemoteStoreError::Bounds)?)
                    .ok_or(RemoteStoreError::Bounds)?,
            )
            .map_err(|_| RemoteStoreError::Unavailable)?;
            if read == 0 {
                return Err(RemoteStoreError::Identity);
            }
            consumed = consumed.checked_add(read).ok_or(RemoteStoreError::Bounds)?;
        }
        self.verify_snapshot()
    }

    fn same_bytes(&self, other: &Self) -> Result<bool, RemoteStoreError> {
        if self.len != other.len || self.sha256 != other.sha256 {
            return Ok(false);
        }
        let mut left = self.open()?;
        let mut right = other.open()?;
        let mut left_buffer = [0_u8; 16 * 1024];
        let mut right_buffer = [0_u8; 16 * 1024];
        loop {
            let left_read = left
                .read(&mut left_buffer)
                .map_err(|_| RemoteStoreError::Unavailable)?;
            let right_read = right
                .read(&mut right_buffer)
                .map_err(|_| RemoteStoreError::Unavailable)?;
            if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
                return Ok(false);
            }
            if left_read == 0 {
                return Ok(true);
            }
        }
    }
}

impl Drop for TempEnvelope {
    fn drop(&mut self) {
        if self.path_is_same_file() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl TempEnvelope {
    fn path_is_same_file(&self) -> bool {
        let Ok(metadata) = fs::symlink_metadata(&self.path) else {
            return false;
        };
        metadata.file_type().is_file()
            && self
                .identity
                .same_file(&TempFileSnapshot::from_metadata(&metadata))
    }
}

#[cfg(unix)]
fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buffer, offset)
}

#[cfg(windows)]
fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buffer, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_file_at(_file: &File, _buffer: &mut [u8], _offset: u64) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "positional temp-file reads are unavailable on this platform",
    ))
}

/// Writes one envelope while computing the REST checksum without allocating a
/// second copy of the complete object.
struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    written: u64,
}

impl<W> HashingWriter<W> {
    fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
            written: 0,
        }
    }

    fn finish(self) -> (W, u64, [u8; 32]) {
        (self.inner, self.written, self.hasher.finalize().into())
    }
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        self.written = self
            .written
            .checked_add(u64::try_from(written).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("object envelope size overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

fn is_redirect_error(error: &ureq::Error) -> bool {
    matches!(
        error,
        ureq::Error::RedirectFailed | ureq::Error::TooManyRedirects
    )
}

fn map_ureq_error(error: ureq::Error) -> RemoteStoreError {
    if is_redirect_error(&error) {
        RemoteStoreError::Protocol
    } else {
        // Never format or expose ureq errors: they can retain bearer URLs.
        RemoteStoreError::Unavailable
    }
}

#[cfg(test)]
mod tests;

mod pack;
pub use pack::{
    CheckedPackedObject, ImmutableS3Pack, MAX_COALESCED_DIRECTORY_PREFIX_BYTES, MAX_PACK_BYTES,
    MAX_PACK_MANIFEST_BYTES, MAX_PACK_OBJECTS, MAX_PARALLEL_PACK_READS, PackExtent,
    PackReadCapability, PackUploadCapability, RECOMMENDED_PACK_BYTES, S3LayoutId, S3PackBuilder,
    S3PackId, S3PackManifest, S3PackRoute, S3RemotePack, StoredPackReceipt,
};

//! Bounded registry transport and the production HTTP adapter.

use std::{
    collections::{BTreeSet, VecDeque},
    fmt,
    fs::{self, File, OpenOptions},
    io::{Cursor, Read, Seek, SeekFrom, Write},
    net::IpAddr,
    path::PathBuf,
    sync::Arc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use backend_version::Schema;
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256, Sha512};

use super::identity::{RegistryCredentialPolicy, same_authority};
use super::{
    AcquisitionError, AcquisitionLimits, AuthenticationToken, CanonicalFeedV1, ChecksumAlgorithm,
    EcosystemAdapter, FeedCursor, FeedSchema, PackageCoordinate, ProvenanceDigest,
    RegistryChecksum, RegistryEcosystem, RegistryEndpoint, ReleaseFacts, RemoteRegistry,
};
use crate::capability::CapabilityArtifactId;
use backend_advisory::AdvisoryObservation;

/// Request for the page following one durable cursor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeedRequest<S: FeedSchema = CanonicalFeedV1> {
    /// Current committed feed cursor.
    pub cursor: FeedCursor<RemoteRegistry, S>,
    /// Maximum number of entries the owner can admit.
    pub max_items: usize,
}

/// Untrusted package descriptor returned by a registry feed.
#[derive(Clone, Eq, PartialEq)]
pub struct RemotePackage {
    /// Exact package coordinate.
    pub coordinate: PackageCoordinate,
    /// Expected content identity of the archive bytes.
    pub(super) integrity: ArchiveIntegrity,
    /// Digest of signature/transparency/provenance evidence verified by the adapter.
    pub provenance: ProvenanceDigest,
    /// Mutable policy and observations detached from immutable archive identity.
    pub facts: ReleaseFacts,
    /// Advisory observation for this exact selected version. `None` is typed absence and is
    /// fail-closed when an advisory gate is configured by the product composition.
    pub advisory: Option<AdvisoryObservation>,
    /// Native or archive dependency facts carried with the same source frontier.
    pub dependency_facts: backend_library::DependencyFacts<
        Box<[backend_library::PackageDependencyRecord]>,
    >,
    pub(super) archive_url: Arc<str>,
}

/// One bounded archive handoff owned by the caller after `fetch_archive`.
///
/// Native metadata sometimes needs to download an archive before it can
/// construct an authenticated checksum (NuGet package hashes may be absent;
/// Conan v2 file listings do not publish one). The transport stages that
/// response into a private temporary file and transfers the file ownership to
/// the registry owner. This avoids retaining a process-wide byte cache while
/// still coalescing metadata and publication into one upstream download.
pub struct ArchiveArtifact {
    source: ArchiveArtifactSource,
    length: u64,
}

enum ArchiveArtifactSource {
    Bytes(Vec<u8>),
    File(PathBuf),
}

impl fmt::Debug for ArchiveArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchiveArtifact")
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

impl ArchiveArtifact {
    /// Constructs an in-memory artifact for deterministic transport seams.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            length: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            source: ArchiveArtifactSource::Bytes(bytes),
        }
    }

    fn from_file(path: PathBuf, length: u64) -> Self {
        Self {
            source: ArchiveArtifactSource::File(path),
            length,
        }
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    pub(crate) fn into_reader(self, maximum: usize) -> Result<ArchiveReader, TransportFailure> {
        let length = usize::try_from(self.length).map_err(|_| TransportFailure::Bounds)?;
        if length > maximum {
            return Err(TransportFailure::Overrun {
                measured: self.length,
                limit: u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?,
            });
        }
        let mut this = self;
        let source = std::mem::replace(&mut this.source, ArchiveArtifactSource::Bytes(Vec::new()));
        match source {
            ArchiveArtifactSource::Bytes(bytes) => Ok(ArchiveReader {
                source: ArchiveReaderSource::Bytes(Cursor::new(bytes)),
                cleanup: None,
                length: this.length,
            }),
            ArchiveArtifactSource::File(path) => {
                let file = match File::open(&path) {
                    Ok(file) => file,
                    Err(_) => {
                        let _ = fs::remove_file(path);
                        return Err(TransportFailure::Protocol);
                    }
                };
                Ok(ArchiveReader {
                    source: ArchiveReaderSource::File(file),
                    cleanup: Some(path),
                    length: this.length,
                })
            }
        }
    }

    pub(crate) fn into_bytes(self, maximum: usize) -> Result<Vec<u8>, TransportFailure> {
        let length = usize::try_from(self.length).map_err(|_| TransportFailure::Bounds)?;
        if length > maximum {
            return Err(TransportFailure::Overrun {
                measured: self.length,
                limit: u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?,
            });
        }
        let mut this = self;
        let source = std::mem::replace(&mut this.source, ArchiveArtifactSource::Bytes(Vec::new()));
        match source {
            ArchiveArtifactSource::Bytes(bytes) => Ok(bytes),
            ArchiveArtifactSource::File(path) => {
                let result = (|| {
                    let mut file = File::open(&path).map_err(|_| TransportFailure::Protocol)?;
                    let mut bytes = Vec::with_capacity(length);
                    file.take(this.length.saturating_add(1))
                        .read_to_end(&mut bytes)
                        .map_err(|_| TransportFailure::Protocol)?;
                    if bytes.len() != length {
                        return Err(TransportFailure::Protocol);
                    }
                    Ok(bytes)
                })();
                let _ = fs::remove_file(path);
                result
            }
        }
    }

    fn digest_hex(&self, algorithm: ChecksumAlgorithm) -> Result<String, TransportFailure> {
        let mut reader = match &self.source {
            ArchiveArtifactSource::Bytes(bytes) => DigestReader::Bytes(bytes.as_slice()),
            ArchiveArtifactSource::File(path) => {
                DigestReader::File(File::open(path).map_err(|_| TransportFailure::Protocol)?)
            }
        };
        let mut buffer = [0_u8; 64 * 1024];
        let mut total = 0_u64;
        let digest = match algorithm {
            ChecksumAlgorithm::Sha1 => {
                let mut hasher = sha1::Sha1::new();
                while let Some(read) = reader.read_chunk(&mut buffer)? {
                    total = total
                        .checked_add(u64::try_from(read).map_err(|_| TransportFailure::Bounds)?)
                        .ok_or(TransportFailure::Bounds)?;
                    hasher.update(&buffer[..read]);
                }
                hex_digest(hasher.finalize().as_slice())
            }
            ChecksumAlgorithm::Sha256 => {
                let mut hasher = sha2::Sha256::new();
                while let Some(read) = reader.read_chunk(&mut buffer)? {
                    total = total
                        .checked_add(u64::try_from(read).map_err(|_| TransportFailure::Bounds)?)
                        .ok_or(TransportFailure::Bounds)?;
                    hasher.update(&buffer[..read]);
                }
                hex_digest(hasher.finalize().as_slice())
            }
            ChecksumAlgorithm::Sha512 => {
                let mut hasher = Sha512::new();
                while let Some(read) = reader.read_chunk(&mut buffer)? {
                    total = total
                        .checked_add(u64::try_from(read).map_err(|_| TransportFailure::Bounds)?)
                        .ok_or(TransportFailure::Bounds)?;
                    hasher.update(&buffer[..read]);
                }
                hex_digest(hasher.finalize().as_slice())
            }
            ChecksumAlgorithm::GoModule => return Err(TransportFailure::Protocol),
        };
        if total != self.length {
            return Err(TransportFailure::Protocol);
        }
        Ok(digest)
    }
}

impl Drop for ArchiveArtifact {
    fn drop(&mut self) {
        if let ArchiveArtifactSource::File(path) = &self.source {
            let _ = fs::remove_file(path);
        }
    }
}

enum DigestReader<'a> {
    Bytes(&'a [u8]),
    File(File),
}

impl DigestReader<'_> {
    fn read_chunk(&mut self, buffer: &mut [u8]) -> Result<Option<usize>, TransportFailure> {
        let read = match self {
            Self::Bytes(bytes) => {
                if bytes.is_empty() {
                    0
                } else {
                    let count = bytes.len().min(buffer.len());
                    buffer[..count].copy_from_slice(&bytes[..count]);
                    *bytes = &bytes[count..];
                    count
                }
            }
            Self::File(file) => file.read(buffer).map_err(|_| TransportFailure::Protocol)?,
        };
        Ok((read != 0).then_some(read))
    }
}

pub(crate) struct ArchiveReader {
    source: ArchiveReaderSource,
    cleanup: Option<PathBuf>,
    length: u64,
}

enum ArchiveReaderSource {
    Bytes(Cursor<Vec<u8>>),
    File(File),
}

impl ArchiveReader {
    pub(crate) fn length(&self) -> u64 {
        self.length
    }
}

impl Read for ArchiveReader {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match &mut self.source {
            ArchiveReaderSource::Bytes(reader) => reader.read(buffer),
            ArchiveReaderSource::File(reader) => reader.read(buffer),
        }
    }
}

impl Seek for ArchiveReader {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        match &mut self.source {
            ArchiveReaderSource::Bytes(reader) => reader.seek(position),
            ArchiveReaderSource::File(reader) => reader.seek(position),
        }
    }
}

impl Drop for ArchiveReader {
    fn drop(&mut self) {
        if let Some(path) = self.cleanup.take() {
            let _ = fs::remove_file(path);
        }
    }
}

struct ArchiveHandoff {
    key: [u8; 32],
    artifact: ArchiveArtifact,
}

static NEXT_ARCHIVE_FILE: AtomicU64 = AtomicU64::new(0);

fn stage_archive<R: Read>(
    reader: &mut R,
    maximum: usize,
) -> Result<ArchiveArtifact, TransportFailure> {
    let mut path = std::env::temp_dir();
    let process = std::process::id();
    let nonce = NEXT_ARCHIVE_FILE.fetch_add(1, Ordering::Relaxed);
    path.push(format!("nudox-registry-archive-{process}-{nonce}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|_| TransportFailure::Protocol)?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0usize;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(read) => read,
            Err(_) => {
                drop(file);
                let _ = fs::remove_file(&path);
                return Err(TransportFailure::Protocol);
            }
        };
        if read == 0 {
            break;
        }
        total = match total.checked_add(read) {
            Some(total) => total,
            None => {
                drop(file);
                let _ = fs::remove_file(&path);
                return Err(TransportFailure::Bounds);
            }
        };
        if total > maximum {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(total).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?,
            });
        }
        if file.write_all(&buffer[..read]).is_err() {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(TransportFailure::Protocol);
        }
    }
    drop(file);
    Ok(ArchiveArtifact::from_file(
        path,
        u64::try_from(total).map_err(|_| TransportFailure::Bounds)?,
    ))
}

impl RemotePackage {
    pub(crate) fn verify_archive(
        &self,
        bytes: &[u8],
    ) -> Result<CapabilityArtifactId, TransportFailure> {
        let artifact = CapabilityArtifactId::from_value(bytes);
        let valid = match &self.integrity {
            ArchiveIntegrity::Canonical(expected) => artifact.as_bytes() == expected,
            ArchiveIntegrity::Native(checksum) => checksum.verifies(bytes),
        };
        valid.then_some(artifact).ok_or(TransportFailure::Integrity)
    }

    pub(crate) fn stream_to<R: Read + Seek, W: Write>(
        &self,
        reader: &mut R,
        writer: &mut W,
        length: u64,
    ) -> Result<CapabilityArtifactId, TransportFailure> {
        let payload_length = usize::try_from(length).map_err(|_| TransportFailure::Bounds)?;
        let schema = backend_version::SchemaIdentity::new(
            crate::capability::CapabilityArtifactSchema::DOMAIN,
            crate::capability::CapabilityArtifactSchema::TYPE,
            crate::capability::CapabilityArtifactSchema::VERSION,
        );
        let mut object = backend_version::ObjectVersionHasher::new(schema, payload_length)
            .map_err(|_| TransportFailure::Bounds)?;
        let mut sha1 = matches!(
            &self.integrity,
            ArchiveIntegrity::Native(checksum)
                if checksum.algorithm() == ChecksumAlgorithm::Sha1
        )
        .then(sha1::Sha1::new);
        let mut sha256 = matches!(
            &self.integrity,
            ArchiveIntegrity::Native(checksum)
                if checksum.algorithm() == ChecksumAlgorithm::Sha256
        )
        .then(sha2::Sha256::new);
        let mut sha512 = matches!(
            &self.integrity,
            ArchiveIntegrity::Native(checksum)
                if checksum.algorithm() == ChecksumAlgorithm::Sha512
        )
        .then(Sha512::new);
        let mut buffer = [0_u8; 64 * 1024];
        let mut total = 0_u64;
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|_| TransportFailure::Protocol)?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(read).map_err(|_| TransportFailure::Bounds)?)
                .ok_or(TransportFailure::Bounds)?;
            if total > length {
                return Err(TransportFailure::Overrun {
                    measured: total,
                    limit: length,
                });
            }
            writer
                .write_all(&buffer[..read])
                .map_err(|_| TransportFailure::Protocol)?;
            object
                .update(&buffer[..read])
                .map_err(|_| TransportFailure::Bounds)?;
            if let Some(hasher) = &mut sha1 {
                hasher.update(&buffer[..read]);
            }
            if let Some(hasher) = &mut sha256 {
                hasher.update(&buffer[..read]);
            }
            if let Some(hasher) = &mut sha512 {
                hasher.update(&buffer[..read]);
            }
        }
        if total != length {
            return Err(TransportFailure::Protocol);
        }
        let artifact = object
            .finish_version::<crate::capability::CapabilityArtifactSchema>()
            .map_err(|_| TransportFailure::Protocol)?;
        let valid = match &self.integrity {
            ArchiveIntegrity::Canonical(expected) => artifact.as_bytes() == expected,
            ArchiveIntegrity::Native(checksum) => match checksum.algorithm() {
                ChecksumAlgorithm::Sha1 => sha1
                    .take()
                    .is_some_and(|hasher| hasher.finalize().as_slice() == checksum.bytes.as_ref()),
                ChecksumAlgorithm::Sha256 => sha256
                    .take()
                    .is_some_and(|hasher| hasher.finalize().as_slice() == checksum.bytes.as_ref()),
                ChecksumAlgorithm::Sha512 => sha512
                    .take()
                    .is_some_and(|hasher| hasher.finalize().as_slice() == checksum.bytes.as_ref()),
                ChecksumAlgorithm::GoModule => {
                    reader
                        .seek(SeekFrom::Start(0))
                        .map_err(|_| TransportFailure::Protocol)?;
                    checksum.verifies_reader(reader)
                }
            },
        };
        valid.then_some(artifact).ok_or(TransportFailure::Integrity)
    }

    pub(crate) const fn canonical_digest(&self) -> Option<[u8; 32]> {
        match self.integrity {
            ArchiveIntegrity::Canonical(value) => Some(value),
            ArchiveIntegrity::Native(_) => None,
        }
    }

    pub(crate) fn integrity_version(&self) -> [u8; 32] {
        if let ArchiveIntegrity::Native(checksum) = &self.integrity {
            return checksum.cache_key();
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.registry.upstream-integrity.v1\0");
        match &self.integrity {
            ArchiveIntegrity::Canonical(value) => {
                hasher.update(&[0]);
                hasher.update(value);
            }
            ArchiveIntegrity::Native(_) => unreachable!("native integrity returned above"),
        }
        *hasher.finalize().as_bytes()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum ArchiveIntegrity {
    Canonical([u8; 32]),
    Native(RegistryChecksum),
}

impl fmt::Debug for RemotePackage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemotePackage")
            .field("coordinate", &self.coordinate)
            .field("integrity", &self.integrity)
            .field("provenance", &self.provenance)
            .field("facts", &self.facts)
            .field("archive_url", &"[REDACTED]")
            .finish()
    }
}

/// One admitted, bounded feed page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeedPage<S: FeedSchema = CanonicalFeedV1> {
    /// Cursor against which this page was fetched.
    pub base: FeedCursor<RemoteRegistry, S>,
    /// Opaque continuation token committed only with all publications.
    pub next_token: [u8; 32],
    /// Strictly ordered, unique package entries.
    pub packages: Vec<RemotePackage>,
}

/// Typed transport availability outcome.
#[derive(Debug)]
pub enum TransportResult<T> {
    /// A definitive response was received.
    Available(T),
    /// Endpoint could not be reached before the deadline.
    Unavailable,
    /// Endpoint explicitly requested a later retry.
    RetryAfter(Duration),
    /// Conditional metadata request confirmed the cached representation.
    NotModified,
}

/// Terminal transport or protocol rejection. Diagnostics contain no endpoint or credential.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportFailure {
    /// Configuration violates source or deadline policy.
    Configuration,
    /// Response exceeded an admission bound.
    Bounds,
    /// Response exceeded an admission bound, with the measured extent.
    ///
    /// Carries the observed byte count and the configured cap so a legitimate
    /// large archive or metadata page can be distinguished from a malformed or
    /// hostile response, and the cap raised deliberately.
    Overrun {
        /// Bytes actually read from the bounded response.
        measured: u64,
        /// Configured maximum admitted bytes.
        limit: u64,
    },
    /// Remote response did not match the canonical feed grammar.
    Protocol,
    /// Registry rejected the request permanently.
    Rejected(u16),
    /// Archive bytes did not match their authenticated manifest identity.
    Integrity,
    /// Upstream metadata exists but does not expose a verifiable archive
    /// download for this release.
    DownloadUnavailable,
}

/// Network seam for deterministic process/fault testing.
pub trait RegistryTransport<S: FeedSchema = CanonicalFeedV1> {
    /// Fetches one page without advancing durable progress.
    ///
    /// # Errors
    /// Returns a terminal configuration, bounds, protocol, or service rejection.
    fn fetch_page(
        &mut self,
        request: FeedRequest<S>,
    ) -> Result<TransportResult<FeedPage<S>>, TransportFailure>;
    /// Fetches one exact archive without publishing it.
    ///
    /// # Errors
    /// Returns a terminal configuration, bounds, integrity, or service rejection.
    fn fetch_archive(
        &mut self,
        package: &RemotePackage,
    ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure>;
}

/// Bounded authenticated HTTP(S) implementation of the production registry protocol.
pub struct HttpRegistryTransport {
    endpoint: RegistryEndpoint,
    ecosystem: RegistryEcosystem,
    credentials: RegistryCredentialPolicy,
    limits: AcquisitionLimits,
    agent: ureq::Agent,
    mode: HttpFeedMode,
    /// Archives fetched while resolving metadata are handed to the owner once.
    /// The artifact itself is file-backed; only its immutable checksum key and
    /// bounded length live in this transport.
    archive_handoffs: VecDeque<ArchiveHandoff>,
    archive_handoff_bytes: usize,
    /// Resource authorities explicitly advertised by a trusted source (for
    /// example NuGet's registration and flat-container resources).  No
    /// authentication token is ever sent to these authorities.
    trusted_followup_hosts: BTreeSet<String>,
}

enum HttpFeedMode {
    Canonical,
    Native(EcosystemAdapter),
}

impl fmt::Debug for HttpRegistryTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpRegistryTransport")
            .field("endpoint", &self.endpoint)
            .field("ecosystem", &self.ecosystem)
            .field("credentials", &self.credentials)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl HttpRegistryTransport {
    /// Constructs a transport after validating every resource and source policy.
    ///
    /// # Errors
    /// Returns invalid configuration when an endpoint or resource bound is unsafe.
    pub fn new(
        endpoint: RegistryEndpoint,
        authentication: Option<AuthenticationToken>,
        limits: AcquisitionLimits,
    ) -> Result<Self, AcquisitionError> {
        let limits = limits.validate()?;
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(limits.connect_timeout))
            .timeout_recv_response(Some(limits.read_timeout))
            .timeout_recv_body(Some(limits.read_timeout))
            // Respect the process proxy policy for real upstream verification;
            // ureq does not consult proxy environment variables implicitly.
            .proxy(ureq::Proxy::try_from_env())
            // Credentials and archive requests are bound to one configured
            // authority; a response cannot redirect either to another host.
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        let credentials = RegistryCredentialPolicy::new(&endpoint, authentication);
        Ok(Self {
            ecosystem: endpoint.ecosystem(),
            endpoint,
            credentials,
            limits,
            agent: config.new_agent(),
            mode: HttpFeedMode::Canonical,
            archive_handoffs: VecDeque::new(),
            archive_handoff_bytes: 0,
            trusted_followup_hosts: BTreeSet::new(),
        })
    }

    /// Constructs the bounded transport over one native ecosystem grammar.
    ///
    /// # Errors
    /// Returns invalid configuration when the source or resource policy is unsafe.
    pub fn for_native(
        adapter: EcosystemAdapter,
        authentication: Option<AuthenticationToken>,
        limits: AcquisitionLimits,
    ) -> Result<Self, AcquisitionError> {
        let endpoint = adapter.endpoint().clone();
        let mut transport = Self::new(endpoint, authentication, limits)?;
        transport.mode = HttpFeedMode::Native(adapter);
        Ok(transport)
    }

    fn get(&self, url: &str, maximum: usize) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
        self.get_with_accept(url, maximum, None)
    }

    fn get_with_accept(
        &self,
        url: &str,
        maximum: usize,
        accept: Option<&str>,
    ) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
        if !self.followup_url_is_admitted(url) {
            return Err(TransportFailure::Configuration);
        }
        let maximum_u64 = u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?;
        for attempt in 1..=self.limits.attempts.get() {
            let request = self.agent.get(url).header("accept-encoding", "identity");
            let request = match accept {
                Some(value) => request.header("accept", value),
                None => request,
            };
            let request = match self.credentials.authorization_for(url) {
                Some(value) => request.header("authorization", value),
                None => request,
            };
            let result = request.call();
            match result {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if status == 304 {
                        return Ok(TransportResult::NotModified);
                    }
                    if status == 429 {
                        return Ok(TransportResult::RetryAfter(retry_after(&response)));
                    }
                    if matches!(status, 408 | 502 | 503 | 504) {
                        let delay = retry_after(&response);
                        if attempt == self.limits.attempts.get() {
                            return Ok(TransportResult::RetryAfter(delay));
                        }
                        continue;
                    }
                    if !(200..300).contains(&status) {
                        return Err(TransportFailure::Rejected(status));
                    }
                    let mut bytes = Vec::new();
                    response
                        .body_mut()
                        .as_reader()
                        .take(maximum_u64.saturating_add(1))
                        .read_to_end(&mut bytes)
                        .map_err(|_| TransportFailure::Protocol)?;
                    if bytes.len() > maximum {
                        return Err(TransportFailure::Overrun {
                            measured: u64::try_from(bytes.len())
                                .map_err(|_| TransportFailure::Bounds)?,
                            limit: u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?,
                        });
                    }
                    return Ok(TransportResult::Available(bytes));
                }
                Err(_) if attempt == self.limits.attempts.get() => {
                    return Ok(TransportResult::Unavailable);
                }
                Err(_) => {}
            }
        }
        Ok(TransportResult::Unavailable)
    }

    fn get_archive(
        &self,
        url: &str,
        maximum: usize,
    ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
        if !self.followup_url_is_admitted(url) {
            return Err(TransportFailure::Configuration);
        }
        for attempt in 1..=self.limits.attempts.get() {
            let request = self.agent.get(url).header("accept-encoding", "identity");
            let request = match self.credentials.authorization_for(url) {
                Some(value) => request.header("authorization", value),
                None => request,
            };
            match request.call() {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if status == 304 {
                        return Ok(TransportResult::NotModified);
                    }
                    if status == 429 {
                        return Ok(TransportResult::RetryAfter(retry_after(&response)));
                    }
                    if matches!(status, 408 | 502 | 503 | 504) {
                        let delay = retry_after(&response);
                        if attempt == self.limits.attempts.get() {
                            return Ok(TransportResult::RetryAfter(delay));
                        }
                        continue;
                    }
                    if !(200..300).contains(&status) {
                        return Err(TransportFailure::Rejected(status));
                    }
                    let mut reader = response.body_mut().as_reader();
                    return stage_archive(&mut reader, maximum).map(TransportResult::Available);
                }
                Err(_) if attempt == self.limits.attempts.get() => {
                    return Ok(TransportResult::Unavailable);
                }
                Err(_) => {}
            }
        }
        Ok(TransportResult::Unavailable)
    }

    fn feed_url<S: FeedSchema>(&self, request: &FeedRequest<S>) -> String {
        format!(
            "{}/feed?schema={}&cursor={}&limit={}",
            self.endpoint.url(),
            S::VERSION,
            hex(&request.cursor.token()),
            request.max_items
        )
    }

    fn fetch_native_page(
        &mut self,
        adapter: &EcosystemAdapter,
        request: FeedRequest,
    ) -> Result<TransportResult<FeedPage>, TransportFailure> {
        let (metadata, accept) = match adapter.ecosystem() {
            RegistryEcosystem::Pypi => (
                adapter.metadata_url(),
                Some("application/vnd.pypi.simple.v1+json"),
            ),
            RegistryEcosystem::Cargo => (adapter.metadata_url(), Some("application/json")),
            RegistryEcosystem::Npm => (
                adapter.metadata_url(),
                Some("application/vnd.npm.install-v1+json"),
            ),
            RegistryEcosystem::Maven => (
                adapter.metadata_url(),
                Some("application/xml, text/xml;q=0.9, */*;q=0.8"),
            ),
            RegistryEcosystem::Nuget | RegistryEcosystem::Golang | RegistryEcosystem::Cpp => {
                (adapter.metadata_url(), Some("application/json"))
            }
        };
        let bytes = match self.get_with_accept(&metadata, self.limits.max_feed_bytes, accept)? {
            TransportResult::Available(value) => value,
            TransportResult::Unavailable => return Ok(TransportResult::Unavailable),
            TransportResult::RetryAfter(delay) => return Ok(TransportResult::RetryAfter(delay)),
            TransportResult::NotModified => return Ok(TransportResult::NotModified),
        };
        let page = match adapter.ecosystem() {
            RegistryEcosystem::Cargo | RegistryEcosystem::Npm | RegistryEcosystem::Pypi => {
                adapter.admit_page(&bytes, request)?
            }
            RegistryEcosystem::Nuget => self.nuget_page(adapter, bytes, request)?,
            RegistryEcosystem::Maven => self.maven_page(adapter, bytes, request)?,
            RegistryEcosystem::Golang => self.go_page(adapter, bytes, request)?,
            RegistryEcosystem::Cpp => self.conan_page(adapter, bytes, request)?,
        };
        Ok(TransportResult::Available(page))
    }

    fn nuget_page(
        &mut self,
        adapter: &EcosystemAdapter,
        service_index: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let root: serde_json::Value =
            serde_json::from_slice(&service_index).map_err(|_| TransportFailure::Protocol)?;
        let resources = root
            .get("resources")
            .and_then(serde_json::Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        let registration = nuget_resource(resources, "RegistrationsBaseUrl")
            .ok_or(TransportFailure::DownloadUnavailable)?;
        let package_base = nuget_resource(resources, "PackageBaseAddress");
        self.admit_resource_origin(registration)?;
        if let Some(package_base) = package_base {
            self.admit_resource_origin(package_base)?;
        }
        let url = format!(
            "{}/{}/index.json",
            registration.trim_end_matches('/'),
            adapter.package_name().to_ascii_lowercase()
        );
        let registration = match self.get(&url, self.limits.max_feed_bytes)? {
            TransportResult::Available(value) => value,
            TransportResult::Unavailable => return Err(TransportFailure::Protocol),
            TransportResult::RetryAfter(_) | TransportResult::NotModified => {
                return Err(TransportFailure::Protocol);
            }
        };
        let mut metadata = adapter.nuget_metadata(&registration, package_base)?;
        if let Some(target) = adapter.target_version() {
            metadata.retain(|release| release.version == target);
        }
        let total = metadata.len();
        let (start, prefix) = adapter.page_start(&registration, request, total)?;
        let selected = metadata
            .into_iter()
            .skip(start)
            .take(request.max_items)
            .collect::<Vec<_>>();
        let mut releases = Vec::with_capacity(selected.len());
        for release in selected {
            let checksum = match release.checksum {
                Some(checksum) => checksum,
                None => {
                    let archive = match self
                        .get_archive(&release.archive_url, self.limits.max_archive_bytes)
                    {
                        Ok(TransportResult::Available(value)) => value,
                        Ok(TransportResult::Unavailable | TransportResult::RetryAfter(_)) => {
                            return Err(TransportFailure::DownloadUnavailable);
                        }
                        Ok(TransportResult::NotModified) => return Err(TransportFailure::Protocol),
                        Err(TransportFailure::Rejected(404)) => {
                            return Err(TransportFailure::DownloadUnavailable);
                        }
                        Err(error) => return Err(error),
                    };
                    let checksum = RegistryChecksum::sha512_hex(
                        &archive.digest_hex(ChecksumAlgorithm::Sha512)?,
                    )?;
                    self.cache_archive(checksum.cache_key(), archive)?;
                    checksum
                }
            };
            releases.push(adapter.release_from_checksum(
                &release.version,
                release.archive_url,
                checksum,
                &release.provenance,
                release.facts,
            )?);
        }
        adapter.admit_window(releases, request, start, prefix, total)
    }

    fn maven_page(
        &self,
        adapter: &EcosystemAdapter,
        metadata: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let mut versions = adapter.maven_versions(&metadata)?;
        if let Some(target) = adapter.target_version() {
            versions.retain(|version| version == target);
        }
        let (start, prefix) = adapter.page_start(&metadata, request, versions.len())?;
        let selected = versions
            .iter()
            .skip(start)
            .take(request.max_items)
            .collect::<Vec<_>>();
        let mut releases = Vec::with_capacity(selected.len());
        for version in selected {
            // Maven's primary JAR is bytecode. The source classifier is the
            // real archive that can pass through the shared source ingester,
            // semantic pipeline, and desktop code-search journey.
            let archive_url = adapter.maven_source_archive_url(version);
            let (checksum, body) = self.maven_checksum(&archive_url)?;
            releases.push(adapter.maven_release(version, checksum, &body)?);
        }
        adapter.admit_window(releases, request, start, prefix, versions.len())
    }

    fn maven_checksum(
        &self,
        archive_url: &str,
    ) -> Result<(RegistryChecksum, Vec<u8>), TransportFailure> {
        for (suffix, parser) in [
            (".sha256", ChecksumAlgorithm::Sha256),
            (".sha512", ChecksumAlgorithm::Sha512),
            (".sha1", ChecksumAlgorithm::Sha1),
        ] {
            let response = self.get(&format!("{archive_url}{suffix}"), 512);
            let body = match response {
                Ok(TransportResult::Available(body)) => body,
                Ok(TransportResult::Unavailable | TransportResult::RetryAfter(_)) => {
                    return Err(TransportFailure::DownloadUnavailable);
                }
                Ok(TransportResult::NotModified) => return Err(TransportFailure::Protocol),
                Err(TransportFailure::Rejected(404)) => continue,
                Err(error) => return Err(error),
            };
            let text = std::str::from_utf8(&body).map_err(|_| TransportFailure::Protocol)?;
            let token = text
                .split_ascii_whitespace()
                .next()
                .ok_or(TransportFailure::Protocol)?;
            let checksum = match parser {
                ChecksumAlgorithm::Sha1 => RegistryChecksum::sha1_hex(token)?,
                ChecksumAlgorithm::Sha256 => RegistryChecksum::sha256_hex(token)?,
                ChecksumAlgorithm::Sha512 => RegistryChecksum::sha512_hex(token)?,
                ChecksumAlgorithm::GoModule => return Err(TransportFailure::Protocol),
            };
            return Ok((checksum, body));
        }
        Err(TransportFailure::DownloadUnavailable)
    }

    fn go_page(
        &self,
        adapter: &EcosystemAdapter,
        listing: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let mut versions = adapter.go_versions(&listing)?;
        if let Some(target) = adapter.target_version() {
            versions.retain(|version| version == target);
        }
        let (start, prefix) = adapter.page_start(&listing, request, versions.len())?;
        let selected = versions
            .iter()
            .skip(start)
            .take(request.max_items)
            .collect::<Vec<_>>();
        let mut releases = Vec::with_capacity(selected.len());
        for version in selected {
            let info = self.required(&adapter.go_info_url(version))?;
            let module = self.required(&adapter.go_mod_url(version))?;
            let sum = self.required(&adapter.go_sum_lookup_url(version))?;
            let checksum = go_checksum(&sum, version)?;
            let mut provenance =
                Vec::with_capacity(listing.len() + info.len() + module.len() + sum.len());
            provenance.extend_from_slice(&listing);
            provenance.extend_from_slice(&info);
            provenance.extend_from_slice(&module);
            provenance.extend_from_slice(&sum);
            releases.push(adapter.go_release(
                version,
                checksum,
                &provenance,
                ReleaseFacts::default(),
            )?);
        }
        adapter.admit_window(releases, request, start, prefix, versions.len())
    }

    fn conan_page(
        &mut self,
        adapter: &EcosystemAdapter,
        revisions: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let revision = adapter.conan_revision(&revisions)?;
        let files = self.required(&adapter.conan_files_url(&revision))?;
        let archive_name = adapter.conan_archive_name(&files)?;
        let archive_url = adapter.conan_archive_url(&revision, &archive_name);
        let recipe = match self.get_archive(&archive_url, self.limits.max_archive_bytes)? {
            TransportResult::Available(value) => value,
            TransportResult::Unavailable | TransportResult::RetryAfter(_) => {
                return Err(TransportFailure::DownloadUnavailable);
            }
            TransportResult::NotModified => return Err(TransportFailure::Protocol),
        };
        let recipe = recipe.into_bytes(self.limits.max_archive_bytes)?;
        let (checksum, archive, source_url) = if let Some(source) = Self::conan_source_spec(
            &recipe,
            adapter.conan_recipe_version(),
            self.limits.max_archive_bytes,
        )? {
            let mut integrity_failure = false;
            let mut source_archive = None;
            for url in source.urls {
                // Conan's authenticated export is the authority that names
                // this source mirror. Keep the mirror policy closed: a recipe
                // cannot turn this package add into arbitrary HTTPS egress.
                // Admit the host for this one bounded handoff before asking
                // the shared archive transport to read it; redirects and
                // credentials remain forbidden.
                if !conan_source_mirror_allowed(&url) {
                    return Err(TransportFailure::Configuration);
                }
                self.admit_resource_origin(&url)?;
                let fetched = match self.get_archive(&url, self.limits.max_archive_bytes)? {
                    TransportResult::Available(value) => value,
                    TransportResult::Unavailable | TransportResult::RetryAfter(_) => continue,
                    TransportResult::NotModified => return Err(TransportFailure::Protocol),
                };
                let bytes = fetched.into_bytes(self.limits.max_archive_bytes)?;
                if source.checksum.verifies(&bytes) {
                    source_archive = Some((url, bytes));
                    break;
                }
                integrity_failure = true;
            }
            let Some((source_url, bytes)) = source_archive else {
                return Err(if integrity_failure {
                    TransportFailure::Integrity
                } else {
                    TransportFailure::DownloadUnavailable
                });
            };
            let checksum = source.checksum;
            self.cache_archive(checksum.cache_key(), ArchiveArtifact::from_bytes(bytes))?;
            // The source mirror is the verified content origin, but the
            // release descriptor must retain Conan as its authoritative
            // archive authority. `fetch_archive` consumes the staged handoff
            // keyed by this checksum, so this does not download the recipe
            // a second time.
            (checksum, archive_url.clone(), Some(source_url))
        } else {
            let checksum =
                RegistryChecksum::sha256_hex(&hex_digest(Sha256::digest(&recipe).as_slice()))?;
            self.cache_archive(checksum.cache_key(), ArchiveArtifact::from_bytes(recipe))?;
            (checksum, archive_url.clone(), None)
        };
        let mut provenance = Vec::with_capacity(revisions.len() + files.len());
        provenance.extend_from_slice(&revisions);
        provenance.extend_from_slice(&files);
        provenance.extend_from_slice(archive.as_bytes());
        if let Some(source_url) = source_url {
            provenance.extend_from_slice(source_url.as_bytes());
        }
        let release = adapter.release_from_checksum(
            adapter.conan_recipe_version(),
            archive,
            checksum,
            &provenance,
            ReleaseFacts::default(),
        )?;
        let (start, prefix) = adapter.page_start(&provenance, request, 1)?;
        adapter.admit_window(vec![release], request, start, prefix, 1)
    }

    fn conan_source_spec(
        recipe: &[u8],
        version: &str,
        maximum: usize,
    ) -> Result<Option<ConanSourceSpec>, TransportFailure> {
        let Some(metadata) = conan_export_file(recipe, maximum)? else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&metadata).map_err(|_| TransportFailure::Protocol)?;
        let mut in_sources = false;
        let mut sources_indent = 0usize;
        let mut in_target = false;
        let mut target_indent = 0usize;
        let mut in_urls = false;
        let mut urls = Vec::new();
        let mut checksum = None;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = line.len() - line.trim_start_matches(' ').len();
            if !in_sources {
                if trimmed == "sources:" {
                    in_sources = true;
                    sources_indent = indent;
                }
                continue;
            }
            if !in_target {
                if indent <= sources_indent {
                    break;
                }
                if trimmed == format!("{version}:") {
                    in_target = true;
                    target_indent = indent;
                }
                continue;
            }
            if indent <= target_indent {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("sha256:") {
                checksum = Some(RegistryChecksum::sha256_hex(value.trim())?);
                in_urls = false;
            } else if trimmed == "url:" {
                in_urls = true;
            } else if in_urls && indent > target_indent {
                if let Some(value) = trimmed.strip_prefix("-") {
                    let value = value.trim().trim_matches(['\'', '"']);
                    if !value.is_empty() {
                        urls.push(value.to_owned());
                    }
                } else {
                    in_urls = false;
                }
            } else {
                in_urls = false;
            }
        }
        let Some(checksum) = checksum else {
            return Err(TransportFailure::Protocol);
        };
        if urls.is_empty() {
            return Err(TransportFailure::DownloadUnavailable);
        }
        Ok(Some(ConanSourceSpec { urls, checksum }))
    }

    fn required(&self, url: &str) -> Result<Vec<u8>, TransportFailure> {
        match self.get(url, self.limits.max_feed_bytes)? {
            TransportResult::Available(value) => Ok(value),
            TransportResult::Unavailable => Err(TransportFailure::DownloadUnavailable),
            TransportResult::RetryAfter(_) => Err(TransportFailure::DownloadUnavailable),
            TransportResult::NotModified => Err(TransportFailure::Protocol),
        }
    }

    fn followup_url_is_admitted(&self, url: &str) -> bool {
        if same_authority(self.endpoint.url(), url) {
            return true;
        }
        let Ok(uri) = url.parse::<ureq::http::Uri>() else {
            return false;
        };
        let Some(authority) = uri.authority() else {
            return false;
        };
        let loopback = authority
            .host()
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(authority.host())
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
        if !(uri.scheme_str() == Some("https") || (uri.scheme_str() == Some("http") && loopback))
            || authority.as_str().contains('@')
            || uri.query().is_some()
            || url.contains('#')
        {
            return false;
        }
        let host = authority.host();
        let host = host.to_ascii_lowercase();
        if self.trusted_followup_hosts.contains(&host) {
            return true;
        }
        official_followup_host(self.endpoint.url(), self.ecosystem, &host)
    }

    fn admit_resource_origin(&mut self, resource: &str) -> Result<(), TransportFailure> {
        let uri = resource
            .parse::<ureq::http::Uri>()
            .map_err(|_| TransportFailure::Protocol)?;
        let Some(authority) = uri.authority() else {
            return Err(TransportFailure::Protocol);
        };
        let loopback = authority
            .host()
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(authority.host())
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
        if !(uri.scheme_str() == Some("https") || (uri.scheme_str() == Some("http") && loopback))
            || authority.as_str().contains('@')
            || uri.query().is_some()
            || resource.contains('#')
        {
            return Err(TransportFailure::Configuration);
        }
        self.trusted_followup_hosts
            .insert(authority.host().to_ascii_lowercase());
        Ok(())
    }

    fn cache_archive(
        &mut self,
        key: [u8; 32],
        artifact: ArchiveArtifact,
    ) -> Result<(), TransportFailure> {
        if self
            .archive_handoffs
            .iter()
            .any(|handoff| handoff.key == key)
        {
            return Ok(());
        }
        let length = usize::try_from(artifact.length()).map_err(|_| TransportFailure::Bounds)?;
        let next = self
            .archive_handoff_bytes
            .checked_add(length)
            .ok_or(TransportFailure::Bounds)?;
        if next > self.limits.max_page_archive_bytes {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(next).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(self.limits.max_page_archive_bytes)
                    .map_err(|_| TransportFailure::Bounds)?,
            });
        }
        self.archive_handoff_bytes = next;
        self.archive_handoffs
            .push_back(ArchiveHandoff { key, artifact });
        Ok(())
    }
}

struct ConanSourceSpec {
    urls: Vec<String>,
    checksum: RegistryChecksum,
}

fn conan_export_file(archive: &[u8], maximum: usize) -> Result<Option<Vec<u8>>, TransportFailure> {
    if !archive.starts_with(&[0x1f, 0x8b]) {
        // Deterministic transports may provide a recipe export as an opaque
        // byte payload. Keep the legacy Conan handoff path available; a real
        // gzip export is parsed for its source declaration below.
        return Ok(None);
    }
    let mut decoder = GzDecoder::new(Cursor::new(archive));
    let mut tar = Vec::new();
    decoder
        .by_ref()
        .take(
            u64::try_from(maximum)
                .map_err(|_| TransportFailure::Bounds)?
                .saturating_add(1),
        )
        .read_to_end(&mut tar)
        .map_err(|_| TransportFailure::Protocol)?;
    if tar.len() > maximum {
        return Err(TransportFailure::Overrun {
            measured: u64::try_from(tar.len()).map_err(|_| TransportFailure::Bounds)?,
            limit: u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?,
        });
    }
    let mut offset = 0usize;
    while offset.checked_add(512).is_some_and(|end| end <= tar.len()) {
        let header = &tar[offset..offset + 512];
        if header.iter().all(|byte| *byte == 0) {
            return Ok(None);
        }
        validate_tar_checksum(header)?;
        let size = tar_octal(&header[124..136])?;
        let data_start = offset.checked_add(512).ok_or(TransportFailure::Bounds)?;
        let data_end = data_start
            .checked_add(size)
            .ok_or(TransportFailure::Bounds)?;
        if data_end > tar.len() {
            return Err(TransportFailure::Protocol);
        }
        let name = trim_tar_nul(&header[..100])?;
        if name == "conandata.yml" || name.ends_with("/conandata.yml") {
            return Ok(Some(tar[data_start..data_end].to_vec()));
        }
        let padded = size.checked_add(511).ok_or(TransportFailure::Bounds)? / 512 * 512;
        offset = data_start
            .checked_add(padded)
            .ok_or(TransportFailure::Bounds)?;
    }
    Err(TransportFailure::Protocol)
}

fn conan_source_mirror_allowed(url: &str) -> bool {
    let Ok(uri) = url.parse::<ureq::http::Uri>() else {
        return false;
    };
    let Some(authority) = uri.authority() else {
        return false;
    };
    // This is deliberately an exact public mirror allowlist. Conan recipes
    // are remote input and must not be allowed to expand the local service's
    // network authority to an arbitrary host or a DNS-rebinding target.
    matches!(
        authority.host().to_ascii_lowercase().as_str(),
        "github.com" | "zlib.net"
    )
}

fn validate_tar_checksum(header: &[u8]) -> Result<(), TransportFailure> {
    let expected = tar_octal(&header[148..156])?;
    let mut actual = 0_u64;
    for (index, byte) in header.iter().copied().enumerate() {
        actual = actual
            .checked_add(u64::from(if (148..156).contains(&index) {
                b' '
            } else {
                byte
            }))
            .ok_or(TransportFailure::Bounds)?;
    }
    (actual == u64::try_from(expected).map_err(|_| TransportFailure::Bounds)?)
        .then_some(())
        .ok_or(TransportFailure::Protocol)
}

fn tar_octal(bytes: &[u8]) -> Result<usize, TransportFailure> {
    let mut value = 0usize;
    let mut found = false;
    for byte in bytes.iter().copied() {
        if byte == 0 || byte == b' ' {
            continue;
        }
        if !(b'0'..=b'7').contains(&byte) {
            return Err(TransportFailure::Protocol);
        }
        found = true;
        value = value
            .checked_mul(8)
            .and_then(|value| value.checked_add(usize::from(byte - b'0')))
            .ok_or(TransportFailure::Bounds)?;
    }
    Ok(if found { value } else { 0 })
}

fn trim_tar_nul(bytes: &[u8]) -> Result<&str, TransportFailure> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).map_err(|_| TransportFailure::Protocol)
}

fn nuget_resource<'a>(resources: &'a [serde_json::Value], prefix: &str) -> Option<&'a str> {
    resources.iter().find_map(|resource| {
        let matches = match resource.get("@type") {
            Some(serde_json::Value::String(value)) => value.starts_with(prefix),
            Some(serde_json::Value::Array(values)) => values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .any(|value| value.starts_with(prefix)),
            _ => false,
        };
        matches.then(|| resource.get("@id").and_then(serde_json::Value::as_str))?
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

fn go_checksum(bytes: &[u8], version: &str) -> Result<RegistryChecksum, TransportFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
    text.lines()
        .find_map(|line| {
            let mut fields = line.split_ascii_whitespace();
            let _module = fields.next()?;
            let candidate = fields.next()?;
            let digest = fields.next()?;
            (candidate == version).then_some(digest)
        })
        .map(RegistryChecksum::go_module_base64)
        .ok_or(TransportFailure::DownloadUnavailable)?
}

fn official_followup_host(base: &str, ecosystem: RegistryEcosystem, host: &str) -> bool {
    let Ok(base) = base.parse::<ureq::http::Uri>() else {
        return false;
    };
    let Some(base_host) = base.authority().map(|authority| authority.host()) else {
        return false;
    };
    match ecosystem {
        RegistryEcosystem::Cargo if base_host.eq_ignore_ascii_case("index.crates.io") => {
            host == "static.crates.io"
        }
        RegistryEcosystem::Pypi if base_host.eq_ignore_ascii_case("pypi.org") => {
            host == "files.pythonhosted.org"
        }
        RegistryEcosystem::Golang if base_host.eq_ignore_ascii_case("proxy.golang.org") => {
            host == "sum.golang.org"
        }
        _ => false,
    }
}

fn retry_after(response: &ureq::http::Response<ureq::Body>) -> Duration {
    const MAX_RETRY_AFTER_SECONDS: u64 = 60 * 60;
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(|seconds| Duration::from_secs(seconds.min(MAX_RETRY_AFTER_SECONDS)))
        .unwrap_or(Duration::from_secs(1))
}

impl RegistryTransport for HttpRegistryTransport {
    fn fetch_page(
        &mut self,
        request: FeedRequest,
    ) -> Result<TransportResult<FeedPage>, TransportFailure> {
        if request.cursor.registry() != self.endpoint.id()
            || request.max_items == 0
            || request.max_items > self.limits.max_items
        {
            return Err(TransportFailure::Configuration);
        }
        match &self.mode {
            HttpFeedMode::Canonical => {
                let bytes = match self.get(&self.feed_url(&request), self.limits.max_feed_bytes)? {
                    TransportResult::Available(value) => value,
                    TransportResult::Unavailable => return Ok(TransportResult::Unavailable),
                    TransportResult::RetryAfter(delay) => {
                        return Ok(TransportResult::RetryAfter(delay));
                    }
                    TransportResult::NotModified => return Ok(TransportResult::NotModified),
                };
                super::feed::admit_page(&bytes, request, self.ecosystem, self.endpoint.url())
                    .map(TransportResult::Available)
            }
            HttpFeedMode::Native(adapter) => {
                let adapter = adapter.clone();
                self.fetch_native_page(&adapter, request)
            }
        }
    }

    fn fetch_archive(
        &mut self,
        package: &RemotePackage,
    ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
        if let Some(index) = self
            .archive_handoffs
            .iter()
            .position(|handoff| handoff.key == package.integrity_version())
        {
            let handoff = self
                .archive_handoffs
                .remove(index)
                .ok_or(TransportFailure::Protocol)?;
            self.archive_handoff_bytes = self
                .archive_handoff_bytes
                .saturating_sub(usize::try_from(handoff.artifact.length()).unwrap_or(usize::MAX));
            return Ok(TransportResult::Available(handoff.artifact));
        }
        let artifact = match self
            .get_archive(&package.archive_url, self.limits.max_archive_bytes)?
        {
            TransportResult::Available(value) => value,
            TransportResult::Unavailable => return Ok(TransportResult::Unavailable),
            TransportResult::RetryAfter(delay) => return Ok(TransportResult::RetryAfter(delay)),
            TransportResult::NotModified => return Err(TransportFailure::Protocol),
        };
        Ok(TransportResult::Available(artifact))
    }
}
pub(crate) fn hex(value: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in value {
        out.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        out.push(char::from(b"0123456789abcdef"[usize::from(byte & 15)]));
    }
    out
}

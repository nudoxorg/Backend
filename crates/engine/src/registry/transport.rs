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
use crate::acquisition::{
    TransferCheckpoint, TransferResetReason, TransferTelemetry, TransferValidator,
};
use crate::capability::CapabilityArtifactId;
use backend_advisory::AdvisoryObservation;
use backend_library::DependencyFacts;

mod http;

const MAX_NUGET_REGISTRATION_PAGES: usize = 4096;
const MAX_NUGET_REGISTRATION_DEPTH: usize = 32;
const MAX_NUGET_REGISTRATION_LEAVES: usize = 100_000;
const MAX_NUGET_REGISTRATION_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;

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
    /// Versioned native metadata retained with the same source frontier.
    pub native_metadata: backend_library::RegistryNativeMetadata,
    /// Advisory observation for this exact selected version. `None` is typed absence and is
    /// fail-closed when an advisory gate is configured by the product composition.
    pub advisory: Option<AdvisoryObservation>,
    /// Native or archive dependency facts carried with the same source frontier.
    pub dependency_facts:
        backend_library::DependencyFacts<Box<[backend_library::PackageDependencyRecord]>>,
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
    /// Total authenticated representation length, including any persisted
    /// prefix that the response resumes after.
    length: u64,
    /// Byte extent carried by this response body.
    body_length: u64,
    /// Absolute byte offset represented by the first response byte.
    start_offset: u64,
    /// Validator observed for the representation, if supplied by the origin.
    validator: Option<TransferValidator>,
    /// Whether the caller must quarantine/reset the old prefix before using
    /// this full response.
    reset_reason: Option<TransferResetReason>,
}

enum ArchiveArtifactSource {
    Bytes(Vec<u8>),
    File(PathBuf),
}

impl fmt::Debug for ArchiveArtifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ArchiveArtifact")
            .field("length", &self.length)
            .field("body_length", &self.body_length)
            .field("start_offset", &self.start_offset)
            .finish_non_exhaustive()
    }
}

impl ArchiveArtifact {
    /// Constructs an in-memory artifact for deterministic transport seams.
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            length: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            body_length: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            start_offset: 0,
            validator: None,
            reset_reason: None,
            source: ArchiveArtifactSource::Bytes(bytes),
        }
    }

    fn from_range_file(
        path: PathBuf,
        start_offset: u64,
        total_length: u64,
        body_length: u64,
        validator: Option<TransferValidator>,
        reset_reason: Option<TransferResetReason>,
    ) -> Self {
        Self {
            source: ArchiveArtifactSource::File(path),
            length: total_length,
            body_length,
            start_offset,
            validator,
            reset_reason,
        }
    }

    pub(crate) fn length(&self) -> u64 {
        self.length
    }

    pub(crate) fn body_length(&self) -> u64 {
        self.body_length
    }

    pub(crate) const fn start_offset(&self) -> u64 {
        self.start_offset
    }

    pub(crate) fn validator(&self) -> Option<&TransferValidator> {
        self.validator.as_ref()
    }

    pub(crate) const fn reset_reason(&self) -> Option<TransferResetReason> {
        self.reset_reason
    }

    pub(crate) const fn telemetry(&self) -> TransferTelemetry {
        TransferTelemetry {
            resumed_bytes: self.start_offset,
            downloaded_bytes: self.body_length,
        }
    }

    pub(crate) fn into_reader(self, maximum: usize) -> Result<ArchiveReader, TransportFailure> {
        let length = usize::try_from(self.body_length).map_err(|_| TransportFailure::Bounds)?;
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
                })
            }
        }
    }

    pub(crate) fn into_bytes(self, maximum: usize) -> Result<Vec<u8>, TransportFailure> {
        let length = usize::try_from(self.body_length).map_err(|_| TransportFailure::Bounds)?;
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
                    file.take(this.body_length.saturating_add(1))
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
        if total != self.body_length {
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
}

enum ArchiveReaderSource {
    Bytes(Cursor<Vec<u8>>),
    File(File),
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

fn stage_archive_body<R: Read>(
    reader: &mut R,
    maximum: usize,
) -> Result<(PathBuf, u64), TransportFailure> {
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
    Ok((
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

    /// Fetches an archive using the durable checkpoint when the transport can
    /// authenticate a byte range.  The default keeps every deterministic and
    /// custom transport source-compatible: it returns a complete body and the
    /// staging boundary safely restarts from zero when necessary.
    fn fetch_archive_resumable(
        &mut self,
        package: &RemotePackage,
        _checkpoint: TransferCheckpoint,
    ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
        self.fetch_archive(package)
    }
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
    let mut selected: Option<((u64, u64, u64, bool), &'a str)> = None;
    for resource in resources {
        let Some(id) = resource.get("@id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let types = match resource.get("@type") {
            Some(serde_json::Value::String(value)) => std::slice::from_ref(value),
            Some(serde_json::Value::Array(values)) => {
                let mut best = None;
                for value in values.iter().filter_map(serde_json::Value::as_str) {
                    if let Some(version) = nuget_resource_version(value, prefix) {
                        best = best.max(Some(version));
                    }
                }
                let Some(version) = best else { continue };
                let candidate = (version, id);
                if selected.as_ref().is_none_or(|current| {
                    candidate.0 > current.0 || (candidate.0 == current.0 && candidate.1 < current.1)
                }) {
                    selected = Some(candidate);
                }
                continue;
            }
            _ => continue,
        };
        let Some(value) = types.first().map(String::as_str) else {
            continue;
        };
        let Some(version) = nuget_resource_version(value, prefix) else {
            continue;
        };
        if selected
            .as_ref()
            .is_none_or(|current| version > current.0 || (version == current.0 && id < current.1))
        {
            selected = Some((version, id));
        }
    }
    selected.map(|(_, id)| id)
}

fn nuget_resource_version(value: &str, prefix: &str) -> Option<(u64, u64, u64, bool)> {
    let suffix = value.strip_prefix(prefix)?;
    let suffix = suffix.strip_prefix('/').unwrap_or(suffix);
    if !suffix.is_empty() && !value.starts_with(&format!("{prefix}/")) {
        return None;
    }
    let mut parts = suffix.splitn(4, '.');
    let major = parts
        .next()
        .unwrap_or("0")
        .split('-')
        .next()?
        .parse()
        .ok()?;
    let minor = parts
        .next()
        .unwrap_or("0")
        .split('-')
        .next()?
        .parse()
        .ok()?;
    let patch_part = parts.next().unwrap_or("0");
    let patch = patch_part.split('-').next()?.parse().ok()?;
    let stable = !suffix.contains('-');
    Some((major, minor, patch, stable))
}

fn hex_digest(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

fn go_checksum(
    bytes: &[u8],
    module: &str,
    version: &str,
) -> Result<RegistryChecksum, TransportFailure> {
    let text = std::str::from_utf8(bytes).map_err(|_| TransportFailure::Protocol)?;
    let mut archive = None;
    let mut module_file = None;
    for line in text.lines() {
        let mut fields = line.split_ascii_whitespace();
        let Some(candidate_module) = fields.next() else {
            continue;
        };
        let Some(candidate_version) = fields.next() else {
            continue;
        };
        let Some(digest) = fields.next() else {
            continue;
        };
        if fields.next().is_some() {
            continue;
        }
        if candidate_module != module {
            continue;
        }
        if candidate_version == version {
            archive = Some(RegistryChecksum::go_module_base64(digest)?);
        } else if candidate_version == format!("{version}/go.mod") {
            module_file = Some(RegistryChecksum::go_module_base64(digest)?);
        }
    }
    // The tree note and signed proof are deliberately not interpreted as
    // module hashes. We require the exact module/version record and accept a
    // missing /go.mod line because proxies may synthesize a module-only file.
    let checksum = archive.ok_or(TransportFailure::DownloadUnavailable)?;
    let _module_file = module_file;
    Ok(checksum)
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

fn response_validator(response: &ureq::http::Response<ureq::Body>) -> Option<TransferValidator> {
    TransferValidator::new(
        response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok()),
        response
            .headers()
            .get("last-modified")
            .and_then(|value| value.to_str().ok()),
    )
}

fn content_length(
    response: &ureq::http::Response<ureq::Body>,
) -> Result<Option<u64>, TransportFailure> {
    response
        .headers()
        .get("content-length")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| TransportFailure::Protocol)?
                .parse::<u64>()
                .map_err(|_| TransportFailure::Protocol)
        })
        .transpose()
}

fn content_range(
    response: &ureq::http::Response<ureq::Body>,
) -> Result<Option<(u64, u64, u64)>, TransportFailure> {
    let Some(value) = response.headers().get("content-range") else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| TransportFailure::Protocol)?;
    let Some(value) = value.strip_prefix("bytes ") else {
        return Err(TransportFailure::Protocol);
    };
    let (range, total) = value.split_once('/').ok_or(TransportFailure::Protocol)?;
    let total = total
        .parse::<u64>()
        .map_err(|_| TransportFailure::Protocol)?;
    let (start, end) = range.split_once('-').ok_or(TransportFailure::Protocol)?;
    let start = start
        .parse::<u64>()
        .map_err(|_| TransportFailure::Protocol)?;
    let end = end.parse::<u64>().map_err(|_| TransportFailure::Protocol)?;
    Ok(Some((start, end, total)))
}

fn unsatisfied_range_total(
    response: &ureq::http::Response<ureq::Body>,
) -> Result<u64, TransportFailure> {
    let Some(value) = response.headers().get("content-range") else {
        return Err(TransportFailure::Protocol);
    };
    let value = value.to_str().map_err(|_| TransportFailure::Protocol)?;
    let Some(value) = value.strip_prefix("bytes */") else {
        return Err(TransportFailure::Protocol);
    };
    value.parse::<u64>().map_err(|_| TransportFailure::Protocol)
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

    fn fetch_archive_resumable(
        &mut self,
        package: &RemotePackage,
        checkpoint: TransferCheckpoint,
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
        self.get_archive_resumable(
            &package.archive_url,
            self.limits.max_archive_bytes,
            checkpoint,
        )
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

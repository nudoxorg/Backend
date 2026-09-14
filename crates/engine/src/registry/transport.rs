//! Bounded registry transport and the production HTTP adapter.

use std::{fmt, io::Read, sync::Arc, time::Duration};

use super::{
    AcquisitionError, AcquisitionLimits, AuthenticationToken, CanonicalFeedV1, EcosystemAdapter,
    FeedCursor, FeedSchema, PackageCoordinate, ProvenanceDigest, RegistryChecksum,
    RegistryEcosystem, RegistryEndpoint, RemoteRegistry,
};
use crate::capability::CapabilityArtifactId;

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
    pub(super) archive_url: Arc<str>,
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

    pub(crate) const fn canonical_digest(&self) -> Option<[u8; 32]> {
        match self.integrity {
            ArchiveIntegrity::Canonical(value) => Some(value),
            ArchiveIntegrity::Native(_) => None,
        }
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
    ) -> Result<TransportResult<Vec<u8>>, TransportFailure>;
}

/// Bounded authenticated HTTP(S) implementation of the production registry protocol.
pub struct HttpRegistryTransport {
    endpoint: RegistryEndpoint,
    ecosystem: RegistryEcosystem,
    authentication: Option<AuthenticationToken>,
    limits: AcquisitionLimits,
    agent: ureq::Agent,
    mode: HttpFeedMode,
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
            .field("authentication", &self.authentication)
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
            // Credentials and archive requests are bound to one configured
            // authority; a response cannot redirect either to another host.
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        Ok(Self {
            ecosystem: endpoint.ecosystem(),
            endpoint,
            authentication,
            limits,
            agent: config.new_agent(),
            mode: HttpFeedMode::Canonical,
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
        let maximum_u64 = u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?;
        for attempt in 1..=self.limits.attempts.get() {
            let request = self.agent.get(url);
            let request = match &self.authentication {
                Some(token) => request.header("authorization", token.expose()),
                None => request,
            };
            let result = request.call();
            match result {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if status == 429 || status == 503 {
                        if attempt == self.limits.attempts.get() {
                            return Ok(TransportResult::RetryAfter(Duration::from_secs(1)));
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

    fn feed_url<S: FeedSchema>(&self, request: &FeedRequest<S>) -> String {
        format!(
            "{}/feed?schema={}&cursor={}&limit={}",
            self.endpoint.url(),
            S::VERSION,
            hex(&request.cursor.token()),
            request.max_items
        )
    }
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
        let url = match &self.mode {
            HttpFeedMode::Canonical => self.feed_url(&request),
            HttpFeedMode::Native(adapter) => adapter.metadata_url(),
        };
        let bytes = match self.get(&url, self.limits.max_feed_bytes)? {
            TransportResult::Available(value) => value,
            TransportResult::Unavailable => return Ok(TransportResult::Unavailable),
            TransportResult::RetryAfter(delay) => return Ok(TransportResult::RetryAfter(delay)),
        };
        match &self.mode {
            HttpFeedMode::Canonical => {
                super::feed::admit_page(&bytes, request, self.ecosystem, self.endpoint.url())
            }
            HttpFeedMode::Native(adapter) => adapter.admit_page(&bytes, request),
        }
        .map(TransportResult::Available)
    }

    fn fetch_archive(
        &mut self,
        package: &RemotePackage,
    ) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
        let bytes = match self.get(&package.archive_url, self.limits.max_archive_bytes)? {
            TransportResult::Available(value) => value,
            TransportResult::Unavailable => return Ok(TransportResult::Unavailable),
            TransportResult::RetryAfter(delay) => return Ok(TransportResult::RetryAfter(delay)),
        };
        let _ = package.verify_archive(&bytes)?;
        Ok(TransportResult::Available(bytes))
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

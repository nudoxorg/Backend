//! Strong identities and resource policy for registry acquisition.

use std::{
    fmt, marker::PhantomData, net::IpAddr, num::NonZeroU8, str::FromStr, sync::Arc, time::Duration,
};

pub use backend_semantic::vocabulary::{PackageUrl as PackageCoordinate, RegistryEcosystem};

use super::AcquisitionError;
use crate::capability::CapabilityArtifactId;

/// Stable public identity of a registry source. Credentials are excluded.
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RegistryId([u8; 32]);

impl RegistryId {
    /// Returns the fixed-width identity.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
    pub(crate) const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

/// Persisted claim for a canonical capability artifact identity.
///
/// The private constructor is reachable only after hashing actual archive
/// bytes. Recovery repeats that check before returning a receipt.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublishedArtifactClaim([u8; 32]);

impl PublishedArtifactClaim {
    pub(crate) fn verified(id: CapabilityArtifactId) -> Self {
        Self(*id.as_bytes())
    }
    pub(crate) const fn from_journal(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    /// Fixed-width canonical identity claim.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
    /// Recomputes and admits the canonical artifact identity from bytes.
    ///
    /// # Errors
    /// Returns corruption when the bytes do not match the persisted claim.
    pub fn admit(self, bytes: &[u8]) -> Result<CapabilityArtifactId, AcquisitionError> {
        let id = CapabilityArtifactId::from_value(bytes);
        if id.as_bytes() == &self.0 {
            Ok(id)
        } else {
            Err(AcquisitionError::CorruptJournal)
        }
    }
}

/// Digest of provenance evidence delivered by an authenticated registry feed.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProvenanceDigest([u8; 32]);

impl ProvenanceDigest {
    pub(crate) const fn from_authenticated_feed(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub(crate) const fn from_journal(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    /// Fixed-width digest committed by the registry receipt.
    #[must_use]
    pub const fn as_bytes(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Debug for RegistryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("RegistryId").field(&self.0).finish()
    }
}

/// Validated registry endpoint paired with its stable identity.
#[derive(Clone, Eq, PartialEq)]
pub struct RegistryEndpoint {
    url: Arc<str>,
    id: RegistryId,
    ecosystem: RegistryEcosystem,
}

impl RegistryEndpoint {
    /// Admits one absolute HTTP(S) endpoint without query, fragment, or credentials.
    ///
    /// # Errors
    /// Returns invalid configuration for an unsafe or malformed endpoint.
    pub fn new(
        ecosystem: RegistryEcosystem,
        value: impl Into<String>,
    ) -> Result<Self, AcquisitionError> {
        let value = value.into();
        let normalized = value.trim_end_matches('/');
        if !endpoint_is_admitted(normalized) {
            return Err(AcquisitionError::InvalidConfiguration);
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"backend.registry.source.v1\0");
        hash.update(&[ecosystem as u8]);
        hash.update(normalized.as_bytes());
        Ok(Self {
            url: Arc::from(normalized),
            id: RegistryId(*hash.finalize().as_bytes()),
            ecosystem,
        })
    }
    /// Stable source identity.
    #[must_use]
    pub const fn id(&self) -> RegistryId {
        self.id
    }
    /// Ecosystem whose coordinates this source is allowed to publish.
    #[must_use]
    pub const fn ecosystem(&self) -> RegistryEcosystem {
        self.ecosystem
    }
    pub(crate) fn url(&self) -> &str {
        &self.url
    }
}

fn endpoint_is_admitted(endpoint: &str) -> bool {
    let Ok(uri) = ureq::http::Uri::from_str(endpoint) else {
        return false;
    };
    let (Some(scheme), Some(authority)) = (uri.scheme_str(), uri.authority()) else {
        return false;
    };
    if authority.as_str().contains('@')
        || authority.host().is_empty()
        || uri.query().is_some()
        || endpoint.contains('#')
    {
        return false;
    }
    match scheme {
        // HTTPS is the trust boundary for configured mirrors, self-hosted
        // registries, and forge-backed package origins.  The concrete source
        // policy is applied to follow-up resources below; keeping endpoint
        // admission open to HTTPS is what lets an operator configure a real
        // mirror without recompiling the client.
        "https" => true,
        "http" => loopback_host(authority.host()),
        _ => false,
    }
}

fn loopback_host(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

impl fmt::Debug for RegistryEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegistryEndpoint")
            .field("id", &self.id)
            .field("ecosystem", &self.ecosystem)
            .finish_non_exhaustive()
    }
}

/// Authentication material whose diagnostics are always redacted.
#[derive(Clone, Eq, PartialEq)]
pub struct AuthenticationToken(Arc<str>);

impl AuthenticationToken {
    /// Admits a non-empty header value without CR/LF injection.
    ///
    /// # Errors
    /// Returns invalid configuration for empty or injectable header bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, AcquisitionError> {
        let value = value.into();
        if value.is_empty() || value.contains(['\r', '\n']) {
            return Err(AcquisitionError::InvalidConfiguration);
        }
        Ok(Self(Arc::from(value)))
    }
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for AuthenticationToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AuthenticationToken([REDACTED])")
    }
}

/// Marker implemented by a canonical remote feed grammar.
pub trait FeedSchema: Copy + Clone + fmt::Debug + Eq + Send + Sync + 'static {
    /// Stable wire schema version.
    const VERSION: u16;
}

/// Canonical v1 JSON feed grammar.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CanonicalFeedV1 {}
impl FeedSchema for CanonicalFeedV1 {
    const VERSION: u16 = 1;
}

/// Registry brand used to prevent interchange with non-registry cursors.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum RemoteRegistry {}

/// Cursor bound to a registry instance and feed schema.
#[derive(Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FeedCursor<R, S> {
    registry: RegistryId,
    sequence: u64,
    token: [u8; 32],
    _brand: PhantomData<fn() -> (R, S)>,
}

impl<R, S> Copy for FeedCursor<R, S> {}
impl<R, S> Clone for FeedCursor<R, S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<R, S> fmt::Debug for FeedCursor<R, S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FeedCursor")
            .field("registry", &self.registry)
            .field("sequence", &self.sequence)
            .field("token", &self.token)
            .finish()
    }
}
impl<R, S> FeedCursor<R, S> {
    /// Genesis cursor for one exact registry.
    #[must_use]
    pub const fn genesis(registry: RegistryId) -> Self {
        Self {
            registry,
            sequence: 0,
            token: [0; 32],
            _brand: PhantomData,
        }
    }
    /// Registry source to which this cursor belongs.
    #[must_use]
    pub const fn registry(self) -> RegistryId {
        self.registry
    }
    /// Locally monotonic committed page count.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }
    /// Opaque remote continuation token digest.
    #[must_use]
    pub const fn token(self) -> [u8; 32] {
        self.token
    }
    pub(crate) const fn from_parts(registry: RegistryId, sequence: u64, token: [u8; 32]) -> Self {
        Self {
            registry,
            sequence,
            token,
            _brand: PhantomData,
        }
    }
    pub(crate) fn advance(self, token: [u8; 32]) -> Result<Self, AcquisitionError> {
        Ok(Self::from_parts(
            self.registry,
            self.sequence
                .checked_add(1)
                .ok_or(AcquisitionError::Bounds)?,
            token,
        ))
    }
}

/// Validated package name.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageName(Arc<str>);
/// Validated immutable package version.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackageVersion(Arc<str>);

fn valid_coordinate(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b'@' | b'+' | b':')
        })
}
impl PackageName {
    /// Admits a bounded ecosystem package name.
    ///
    /// # Errors
    /// Returns invalid coordinate for empty, oversized, or noncanonical text.
    pub fn new(value: impl Into<String>) -> Result<Self, AcquisitionError> {
        let value = value.into();
        if !valid_coordinate(&value, 256) {
            return Err(AcquisitionError::InvalidCoordinate);
        }
        Ok(Self(Arc::from(value)))
    }
    /// Canonical text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl PackageVersion {
    /// Admits a bounded immutable version coordinate.
    ///
    /// # Errors
    /// Returns invalid coordinate for empty, oversized, or noncanonical text.
    pub fn new(value: impl Into<String>) -> Result<Self, AcquisitionError> {
        let value = value.into();
        if !valid_coordinate(&value, 128) {
            return Err(AcquisitionError::InvalidCoordinate);
        }
        Ok(Self(Arc::from(value)))
    }
    /// Canonical text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn coordinate_from_registry_parts(
    ecosystem: RegistryEcosystem,
    name: &str,
    version: &str,
) -> Result<PackageCoordinate, AcquisitionError> {
    let path = match ecosystem {
        RegistryEcosystem::Maven | RegistryEcosystem::Cpp => {
            let (namespace, package) = name
                .rsplit_once(':')
                .ok_or(AcquisitionError::InvalidCoordinate)?;
            format!("{namespace}/{package}")
        }
        _ => name.to_owned(),
    };
    PackageCoordinate::parse(format!(
        "pkg:{}/{path}@{version}",
        ecosystem.package_type().as_str()
    ))
    .map_err(|_| AcquisitionError::InvalidCoordinate)
}

/// Registry-specific projection of one canonical package URL.
///
/// Construction proves that the URL selects a remotely resolvable package
/// type, contains no compiler-only qualifiers or subpath, and satisfies the
/// selected registry's namespace rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegistryCoordinate {
    ecosystem: RegistryEcosystem,
    namespace: Option<PackageName>,
    name: PackageName,
    qualified_name: PackageName,
    version: PackageVersion,
}

impl RegistryCoordinate {
    /// Acquisition protocol selected by the package URL type.
    #[must_use]
    pub const fn ecosystem(&self) -> RegistryEcosystem {
        self.ecosystem
    }

    /// Decoded registry namespace, when present.
    #[must_use]
    pub fn namespace(&self) -> Option<&PackageName> {
        self.namespace.as_ref()
    }

    /// Decoded package name without its namespace.
    #[must_use]
    pub const fn name(&self) -> &PackageName {
        &self.name
    }

    /// Decoded immutable version.
    #[must_use]
    pub const fn version(&self) -> &PackageVersion {
        &self.version
    }

    /// Returns the already-admitted registry-native qualified name.
    #[must_use]
    pub const fn qualified_name(&self) -> &PackageName {
        &self.qualified_name
    }
}

/// Admits a canonical package URL for one remote registry effect.
///
/// # Errors
/// Returns an acquisition error when the coordinate is not remotely
/// resolvable or any decoded registry field violates its bound.
pub fn admit_registry_coordinate(
    coordinate: &PackageCoordinate,
) -> Result<RegistryCoordinate, AcquisitionError> {
    let ecosystem = coordinate
        .package_type()
        .registry()
        .ok_or(AcquisitionError::InvalidCoordinate)?;
    if coordinate.qualifiers().is_some() || coordinate.subpath().is_some() {
        return Err(AcquisitionError::InvalidCoordinate);
    }
    let namespace = match (ecosystem, coordinate.namespace()) {
        (RegistryEcosystem::Maven | RegistryEcosystem::Cpp, Some(namespace)) => {
            Some(PackageName::new(percent_decode_coordinate(namespace)?)?)
        }
        (RegistryEcosystem::Maven | RegistryEcosystem::Cpp, None) => {
            return Err(AcquisitionError::InvalidCoordinate);
        }
        (_, Some(namespace)) => Some(PackageName::new(percent_decode_coordinate(namespace)?)?),
        (_, None) => None,
    };
    let name = PackageName::new(percent_decode_coordinate(coordinate.name())?)?;
    let qualified_name = match namespace.as_ref() {
        None => name.clone(),
        Some(namespace) => {
            let separator = match ecosystem {
                RegistryEcosystem::Maven | RegistryEcosystem::Cpp => ':',
                _ => '/',
            };
            PackageName::new(format!(
                "{}{separator}{}",
                namespace.as_str(),
                name.as_str()
            ))?
        }
    };
    Ok(RegistryCoordinate {
        ecosystem,
        namespace,
        name,
        qualified_name,
        version: PackageVersion::new(percent_decode_coordinate(coordinate.version())?)?,
    })
}

fn percent_decode_coordinate(value: &str) -> Result<String, AcquisitionError> {
    if !value.contains('%') {
        return Ok(value.to_owned());
    }
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            output.push(bytes[index]);
            index += 1;
            continue;
        }
        if index + 2 >= bytes.len() {
            return Err(AcquisitionError::InvalidCoordinate);
        }
        let high = (bytes[index + 1] as char)
            .to_digit(16)
            .ok_or(AcquisitionError::InvalidCoordinate)?;
        let low = (bytes[index + 2] as char)
            .to_digit(16)
            .ok_or(AcquisitionError::InvalidCoordinate)?;
        output.push(u8::try_from(high * 16 + low).map_err(|_| AcquisitionError::Bounds)?);
        index += 3;
    }
    String::from_utf8(output).map_err(|_| AcquisitionError::InvalidCoordinate)
}

/// Hard resource limits enforced before allocation or I/O.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcquisitionLimits {
    /// Maximum entries in one feed page.
    pub max_items: usize,
    /// Maximum feed response bytes.
    pub max_feed_bytes: usize,
    /// Maximum bytes in one archive.
    pub max_archive_bytes: usize,
    /// Maximum aggregate archive bytes in one committed page.
    pub max_page_archive_bytes: usize,
    /// Maximum distinct package coordinates retained by one registry owner.
    pub max_catalog_items: usize,
    /// TCP/TLS connection timeout.
    pub connect_timeout: Duration,
    /// Header/body read timeout.
    pub read_timeout: Duration,
    /// Bounded retry attempts for idempotent GETs.
    pub attempts: NonZeroU8,
}

impl Default for AcquisitionLimits {
    fn default() -> Self {
        Self {
            // One page admits a 200-package selection without an artificial
            // second page, while still bounding a single server reply.
            max_items: 256,
            max_feed_bytes: 8 * 1024 * 1024,
            // Source archives and sdists can exceed the 64 MiB replication
            // object cap; the default is measured and configurable, and a
            // deliberate overrun returns a typed `Overrun`.
            max_archive_bytes: 512 * 1024 * 1024,
            max_page_archive_bytes: 1024 * 1024 * 1024,
            max_catalog_items: 1_000_000,
            connect_timeout: Duration::from_secs(5),
            read_timeout: Duration::from_secs(30),
            attempts: NonZeroU8::MIN,
        }
    }
}

impl AcquisitionLimits {
    pub(crate) fn validate(self) -> Result<Self, AcquisitionError> {
        if self.max_items == 0
            || self.max_items > 4096
            || self.max_feed_bytes == 0
            || self.max_feed_bytes > 16 * 1024 * 1024
            || self.max_archive_bytes == 0
            || self.max_page_archive_bytes < self.max_archive_bytes
            || self.max_page_archive_bytes > 1024 * 1024 * 1024
            || self.max_catalog_items == 0
            || self.max_catalog_items > 10_000_000
            || self.connect_timeout.is_zero()
            || self.read_timeout.is_zero()
        {
            return Err(AcquisitionError::InvalidConfiguration);
        }
        Ok(self)
    }
}

/// Operator policy that makes offline behavior explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AcquisitionPolicy {
    /// Never access the network.
    Offline,
    /// Access the configured authenticated registry.
    Online,
}

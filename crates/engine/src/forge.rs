//! First-class acquisition of source trees from code forges.
//!
//! A forge source is a repository coordinate plus an explicit typed revision.
//! The coordinate is credential-free and immutable identities are derived only
//! after the remote owner has resolved the requested tag/branch/commit to an
//! exact commit and tree.  Source bytes are admitted by the shared
//! [`acquisition::ContentAddressedStore`], and the resulting file rows use the
//! same [`acquisition::TreeManifest`] used by registry and local sources.
//!
//! The module deliberately keeps the network seam small.  [`ForgeTransport`]
//! is suitable for loopback fixtures and remote delegation, while
//! [`HttpForgeTransport`] provides bounded HTTPS metadata/archive requests for
//! the public GitHub, GitLab, and Codeberg APIs.  A generic HTTPS Git source is
//! acquired through a supplied transport or the safe process adapter; command
//! arguments are passed directly to `git` and are never interpreted by a
//! shell.

use crate::acquisition::{
    AcquisitionDelta, ArchiveBudget, ArchiveManifestBuilder, ContentAddressedStore,
    ContentStoreError, DeltaChange, ManifestEntry, RawArchiveObjectId, SourceSnapshot,
    TreeManifest,
};
use crate::journal::{HashChainJournal, JournalCodec, JournalDomain, JournalError, JournalLimits};
use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, PackageReference, ProductText,
    RegistryEcosystem,
};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt, fs,
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::PathBuf,
    process::{Child, ChildStdout, Command, Stdio},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ID_BYTES: usize = 32;
const MAX_COORDINATE_BYTES: usize = 2_048;
const MAX_REF_BYTES: usize = 512;
const MAX_OWNER_BYTES: usize = 512;
const MAX_REPOSITORY_BYTES: usize = 256;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_README_BYTES: usize = 256 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const GIT_HASH_BYTES: usize = 20;

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn digest_fields(domain: &[u8], fields: &[&[u8]]) -> [u8; ID_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    for field in fields {
        hasher.update(&(field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    *hasher.finalize().as_bytes()
}

fn resolution_cursor(resolution: &ForgeResolution) -> [u8; ID_BYTES] {
    let commit = resolution.commit.as_hex();
    let tree = resolution.tree.as_ref().map(ForgeObjectId::as_hex);
    digest_fields(
        b"nudox.forge.cursor.v1\0",
        &[
            commit.as_bytes(),
            tree.as_deref().unwrap_or_default().as_bytes(),
            resolution
                .authority
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            resolution
                .validator
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        ],
    )
}

fn metadata_frontier(
    metadata: &ForgeRepositoryMetadata,
    manifests: &[ForgePackageManifest],
) -> [u8; ID_BYTES] {
    let metadata = serde_json::to_vec(metadata).unwrap_or_default();
    let manifests = serde_json::to_vec(manifests).unwrap_or_default();
    digest_fields(b"nudox.forge.facts.v1\0", &[&metadata, &manifests])
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        result.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    result
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() % 2 != 0 {
        return None;
    }
    let bytes = value.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() / 2);
    let nibble = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    };
    let mut index = 0;
    while index < bytes.len() {
        result.push((nibble(bytes[index])? << 4) | nibble(bytes[index + 1])?);
        index += 2;
    }
    Some(result)
}

fn text(value: &str, maximum: usize) -> Result<Arc<str>, ForgeCoordinateError> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value
            .bytes()
            .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(ForgeCoordinateError::NonCanonicalText);
    }
    Ok(Arc::from(value))
}

fn path_part(value: &str, maximum: usize) -> Result<Arc<str>, ForgeCoordinateError> {
    let value = text(value, maximum)?;
    if value
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == ".." || part.contains('\\'))
    {
        return Err(ForgeCoordinateError::UnsafePath);
    }
    Ok(value)
}

/// The forge authority inferred from a canonical HTTPS repository host.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeProvider {
    /// GitHub's repository API and archive authority.
    Github,
    /// GitLab's repository API and archive authority.
    Gitlab,
    /// Codeberg's Forgejo repository API and archive authority.
    Codeberg,
    /// A generic HTTPS Git server.
    GenericHttpsGit,
}

impl ForgeProvider {
    /// Infers the provider from a host. Ports and unknown hosts use the
    /// generic HTTPS Git protocol.
    #[must_use]
    pub fn for_host(host: &str) -> Self {
        let host = host.to_ascii_lowercase();
        if host == "github.com" {
            Self::Github
        } else if host == "gitlab.com" {
            Self::Gitlab
        } else if host == "codeberg.org" {
            Self::Codeberg
        } else {
            Self::GenericHttpsGit
        }
    }

    /// Returns the stable provider token used in identities.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Gitlab => "gitlab",
            Self::Codeberg => "codeberg",
            Self::GenericHttpsGit => "generic-https-git",
        }
    }
}

/// A Git object hash with its original algorithm and exact width retained.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ForgeObjectId {
    algorithm: ForgeHashAlgorithm,
    bytes: Box<[u8]>,
}

impl fmt::Debug for ForgeObjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForgeObjectId")
            .field("algorithm", &self.algorithm)
            .field("hex", &self.as_hex())
            .finish()
    }
}

impl ForgeObjectId {
    /// Parses a 40-character SHA-1 or 64-character SHA-256 Git object id.
    pub fn parse(value: &str) -> Result<Self, ForgeCoordinateError> {
        let bytes = decode_hex(value).ok_or(ForgeCoordinateError::InvalidObjectId)?;
        let algorithm = match bytes.len() {
            GIT_HASH_BYTES => ForgeHashAlgorithm::Sha1,
            32 => ForgeHashAlgorithm::Sha256,
            _ => return Err(ForgeCoordinateError::InvalidObjectId),
        };
        Ok(Self {
            algorithm,
            bytes: bytes.into_boxed_slice(),
        })
    }

    /// Creates an object id from an exact Git hash byte sequence.
    pub fn from_bytes(bytes: impl Into<Box<[u8]>>) -> Result<Self, ForgeCoordinateError> {
        let bytes = bytes.into();
        let algorithm = match bytes.len() {
            GIT_HASH_BYTES => ForgeHashAlgorithm::Sha1,
            32 => ForgeHashAlgorithm::Sha256,
            _ => return Err(ForgeCoordinateError::InvalidObjectId),
        };
        Ok(Self { algorithm, bytes })
    }

    /// Returns the exact object hash spelling in lowercase hexadecimal.
    #[must_use]
    pub fn as_hex(&self) -> String {
        hex(&self.bytes)
    }

    /// Returns the hash algorithm selected by its width.
    #[must_use]
    pub const fn algorithm(&self) -> ForgeHashAlgorithm {
        self.algorithm
    }

    /// Returns the raw object hash bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Git hash algorithms supported by the wire and repository adapters.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeHashAlgorithm {
    /// Traditional Git SHA-1 objects.
    Sha1,
    /// Git repositories configured for SHA-256 objects.
    Sha256,
}

/// A typed Git ref name. It remains a ref until the forge authority resolves
/// it; it is never silently treated as an immutable version.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ForgeRefName(Arc<str>);

impl ForgeRefName {
    /// Admits a bounded Git ref without path traversal or control bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ForgeCoordinateError> {
        let value = text(&value.into(), MAX_REF_BYTES)?;
        if value.starts_with('/')
            || value.ends_with('/')
            || value.contains("..")
            || value.contains('\\')
            || value.bytes().any(|byte| {
                matches!(
                    byte,
                    b'?' | b'#'
                        | b'%'
                        | b'&'
                        | b';'
                        | b':'
                        | b'^'
                        | b'~'
                        | b'*'
                        | b'['
                        | b']'
                        | b'@'
                )
            })
            || value.bytes().any(|byte| byte.is_ascii_whitespace())
            || value.split('/').any(|part| part.is_empty())
        {
            return Err(ForgeCoordinateError::UnsafeReference);
        }
        Ok(Self(value))
    }

    /// Returns the exact ref spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The requested revision kind. A tag or branch is mutable until resolved.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
pub enum ForgeRevision {
    /// An immutable commit object requested by exact hash.
    Commit(ForgeObjectId),
    /// A named tag that must be resolved by the forge authority.
    Tag(ForgeRefName),
    /// A named branch that must be resolved by the forge authority.
    Branch(ForgeRefName),
}

impl ForgeRevision {
    /// Parses an explicit revision token (`commit:`, `tag:`, or `branch:`).
    pub fn parse(value: &str) -> Result<Self, ForgeCoordinateError> {
        if let Some(commit) = value.strip_prefix("commit:") {
            return Ok(Self::Commit(ForgeObjectId::parse(commit)?));
        }
        if let Some(tag) = value.strip_prefix("tag:") {
            return Ok(Self::Tag(ForgeRefName::new(tag.to_owned())?));
        }
        if let Some(branch) = value.strip_prefix("branch:") {
            return Ok(Self::Branch(ForgeRefName::new(branch.to_owned())?));
        }
        Err(ForgeCoordinateError::RevisionKindRequired)
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        match self {
            Self::Commit(commit) => format!("commit:{}", commit.as_hex()).into_bytes(),
            Self::Tag(tag) => format!("tag:{}", tag.as_str()).into_bytes(),
            Self::Branch(branch) => format!("branch:{}", branch.as_str()).into_bytes(),
        }
    }

    /// Returns the explicit commit when this revision is already immutable.
    #[must_use]
    pub fn commit(&self) -> Option<&ForgeObjectId> {
        match self {
            Self::Commit(commit) => Some(commit),
            Self::Tag(_) | Self::Branch(_) => None,
        }
    }
}

/// A canonical repository coordinate with an explicit typed revision.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct ForgeCoordinate {
    provider: ForgeProvider,
    base_url: Arc<str>,
    owner: Arc<str>,
    repository: Arc<str>,
    revision: ForgeRevision,
    subdir: Option<Arc<str>>,
    identity: [u8; ID_BYTES],
}

impl ForgeCoordinate {
    /// Parses an HTTPS repository URL whose ref is supplied as a fragment or
    /// `@tag:...`/`@branch:...`/`@commit:...` suffix.
    pub fn parse(value: impl Into<String>) -> Result<Self, ForgeCoordinateError> {
        let value = value.into();
        if value.len() > MAX_COORDINATE_BYTES {
            return Err(ForgeCoordinateError::Bounds);
        }
        let (url, revision, subdir) = split_coordinate_suffix(&value)?;
        let (provider, base_url, owner, repository) = parse_repository_url(&url)?;
        let revision = revision.ok_or(ForgeCoordinateError::RevisionRequired)?;
        Self::new_parts(provider, base_url, owner, repository, revision, subdir)
    }

    /// Constructs a coordinate from a repository URL and an explicit revision.
    pub fn new(
        repository_url: impl Into<String>,
        revision: ForgeRevision,
        subdir: Option<impl Into<String>>,
    ) -> Result<Self, ForgeCoordinateError> {
        let (provider, base_url, owner, repository) = parse_repository_url(&repository_url.into())?;
        Self::new_parts(
            provider,
            base_url,
            owner,
            repository,
            revision,
            subdir.map(|value| value.into()),
        )
    }

    fn new_parts(
        provider: ForgeProvider,
        base_url: Arc<str>,
        owner: Arc<str>,
        repository: Arc<str>,
        revision: ForgeRevision,
        subdir: Option<String>,
    ) -> Result<Self, ForgeCoordinateError> {
        let subdir = subdir
            .map(|value| path_part(&value, MAX_COORDINATE_BYTES))
            .transpose()?;
        let mut coordinate = Self {
            provider,
            base_url,
            owner,
            repository,
            revision,
            subdir,
            identity: [0; ID_BYTES],
        };
        coordinate.identity = coordinate.derive_identity();
        Ok(coordinate)
    }

    fn derive_identity(&self) -> [u8; ID_BYTES] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.forge.coordinate.v1\0");
        hasher.update(self.provider.as_str().as_bytes());
        hasher.update(self.base_url.as_bytes());
        hasher.update(self.owner.as_bytes());
        hasher.update(self.repository.as_bytes());
        hasher.update(&self.revision.canonical_bytes());
        if let Some(subdir) = &self.subdir {
            hasher.update(b"subdir\0");
            hasher.update(subdir.as_bytes());
        }
        *hasher.finalize().as_bytes()
    }

    fn identity_is_valid(&self) -> bool {
        self.identity == self.derive_identity()
    }

    /// Returns the provider selected from the repository host.
    #[must_use]
    pub const fn provider(&self) -> ForgeProvider {
        self.provider
    }

    /// Returns the credential-free canonical repository URL.
    #[must_use]
    pub fn repository_url(&self) -> &str {
        &self.base_url
    }

    /// Returns the canonical owner/group path.
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Returns the canonical repository name without `.git`.
    #[must_use]
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Returns the exact requested revision.
    #[must_use]
    pub const fn revision(&self) -> &ForgeRevision {
        &self.revision
    }

    /// Returns the optional package-manifest subdirectory.
    #[must_use]
    pub fn subdir(&self) -> Option<&str> {
        self.subdir.as_deref()
    }

    /// Returns the credential-free coordinate identity.
    #[must_use]
    pub const fn identity(&self) -> [u8; ID_BYTES] {
        self.identity
    }

    /// Returns the canonical coordinate spelling used in journals and DTOs.
    #[must_use]
    pub fn canonical(&self) -> String {
        let mut value = self.base_url.to_string();
        value.push('@');
        value.push_str(&String::from_utf8_lossy(&self.revision.canonical_bytes()));
        if let Some(subdir) = &self.subdir {
            value.push('#');
            value.push_str(subdir);
        }
        value
    }

    /// Returns a coordinate with a different explicit revision.
    pub fn with_revision(&self, revision: ForgeRevision) -> Result<Self, ForgeCoordinateError> {
        Self::new_parts(
            self.provider,
            Arc::clone(&self.base_url),
            Arc::clone(&self.owner),
            Arc::clone(&self.repository),
            revision,
            self.subdir.as_deref().map(ToOwned::to_owned),
        )
    }
}

fn split_coordinate_suffix(
    value: &str,
) -> Result<(String, Option<ForgeRevision>, Option<String>), ForgeCoordinateError> {
    let (without_fragment, subdir) = match value.split_once('#') {
        Some((url, fragment)) => {
            if fragment.is_empty() {
                return Err(ForgeCoordinateError::UnsafePath);
            }
            let fragment = fragment.strip_prefix("subdir=").unwrap_or(fragment);
            (url, Some(fragment.to_owned()))
        }
        None => (value, None),
    };
    let (url, revision) = if let Some(at) = without_fragment.rfind('@') {
        let suffix = &without_fragment[at + 1..];
        if suffix.is_empty() || (without_fragment[..at].contains("://") && suffix.contains('/')) {
            (without_fragment, None)
        } else {
            let revision = ForgeRevision::parse(suffix)?;
            (&without_fragment[..at], Some(revision))
        }
    } else {
        (without_fragment, None)
    };
    Ok((url.to_owned(), revision, subdir))
}

fn parse_repository_url(
    value: &str,
) -> Result<(ForgeProvider, Arc<str>, Arc<str>, Arc<str>), ForgeCoordinateError> {
    let uri = ureq::http::Uri::from_str(value).map_err(|_| ForgeCoordinateError::InvalidUrl)?;
    let scheme = uri.scheme_str().ok_or(ForgeCoordinateError::InvalidUrl)?;
    let authority = uri.authority().ok_or(ForgeCoordinateError::InvalidUrl)?;
    if authority.as_str().contains('@') || uri.query().is_some() {
        return Err(ForgeCoordinateError::CredentialOrQuery);
    }
    let host = authority.host();
    if host.is_empty() || (scheme != "https" && !(scheme == "http" && is_loopback(host))) {
        return Err(ForgeCoordinateError::InvalidUrl);
    }
    let path = uri.path().trim_matches('/');
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 2 || parts.iter().any(|part| part.is_empty()) {
        return Err(ForgeCoordinateError::RepositoryPath);
    }
    let repository_raw = parts.pop().ok_or(ForgeCoordinateError::RepositoryPath)?;
    let repository_raw = repository_raw
        .strip_suffix(".git")
        .unwrap_or(repository_raw);
    let repository = path_part(repository_raw, MAX_REPOSITORY_BYTES)?;
    let owner = path_part(&parts.join("/"), MAX_OWNER_BYTES)?;
    let host = host.to_ascii_lowercase();
    let authority_text = authority.as_str();
    let host_with_port = if authority_text.eq_ignore_ascii_case(host.as_str()) {
        host.clone()
    } else {
        authority_text.to_ascii_lowercase()
    };
    let base_url = text(
        &format!(
            "{}://{}/{}/{}",
            scheme.to_ascii_lowercase(),
            host_with_port,
            owner,
            repository
        ),
        MAX_COORDINATE_BYTES,
    )?;
    Ok((
        ForgeProvider::for_host(host.as_str()),
        base_url,
        owner,
        repository,
    ))
}

fn is_loopback(host: &str) -> bool {
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// Coordinate admission failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeCoordinateError {
    /// URL or repository authority was malformed.
    InvalidUrl,
    /// URL embedded credentials or a query/fragment outside the suffix form.
    CredentialOrQuery,
    /// Repository owner/name path was malformed.
    RepositoryPath,
    /// A canonical text value was empty, oversized, or contained controls.
    NonCanonicalText,
    /// A repository or archive path could escape its root.
    UnsafePath,
    /// A Git ref contained unsafe syntax.
    UnsafeReference,
    /// The coordinate omitted its required revision.
    RevisionRequired,
    /// A revision lacked an explicit kind.
    RevisionKindRequired,
    /// A Git object id had an unsupported width or non-hex bytes.
    InvalidObjectId,
    /// The coordinate exceeded a fixed public bound.
    Bounds,
}

impl fmt::Display for ForgeCoordinateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUrl => "invalid forge repository URL",
            Self::CredentialOrQuery => "forge coordinate contains credentials or a query",
            Self::RepositoryPath => "forge repository path is invalid",
            Self::NonCanonicalText => "forge coordinate text is not canonical",
            Self::UnsafePath => "forge path is unsafe",
            Self::UnsafeReference => "forge ref name is unsafe",
            Self::RevisionRequired => "forge coordinate requires an explicit revision",
            Self::RevisionKindRequired => "forge revision requires commit:, tag:, or branch:",
            Self::InvalidObjectId => "forge object id is not a 40 or 64 digit hash",
            Self::Bounds => "forge coordinate exceeds its bound",
        })
    }
}
impl std::error::Error for ForgeCoordinateError {}

/// Exact authority resolution of one forge coordinate.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ForgeResolution {
    /// The requested typed ref.
    pub requested: ForgeRevision,
    /// The exact commit object selected by the authority.
    pub commit: ForgeObjectId,
    /// The exact Git tree object selected by the authority, when reported.
    pub tree: Option<ForgeObjectId>,
    /// Canonical repository authority that made this resolution, when the
    /// transport supplied one.  A bound resolution cannot be replayed for a
    /// different host or repository.
    #[serde(default)]
    pub authority: Option<Arc<str>>,
    /// Validator observed with the ref lookup (usually an ETag or the exact
    /// commit spelling).  Mutable refs must carry this binding so a tag or
    /// branch is never silently treated as an immutable version.
    #[serde(default)]
    pub validator: Option<Arc<str>>,
}

impl ForgeResolution {
    /// Admits a resolution and checks an immutable commit request exactly.
    pub fn new(
        requested: ForgeRevision,
        commit: ForgeObjectId,
        tree: Option<ForgeObjectId>,
    ) -> Result<Self, ForgeProtocolError> {
        if let ForgeRevision::Commit(expected) = &requested
            && expected != &commit
        {
            return Err(ForgeProtocolError::RevisionMismatch);
        }
        Ok(Self {
            requested,
            commit,
            tree,
            authority: None,
            validator: None,
        })
    }

    /// Creates a resolution explicitly bound to one coordinate authority and
    /// the validator observed while resolving its ref.
    pub fn for_coordinate(
        coordinate: &ForgeCoordinate,
        commit: ForgeObjectId,
        tree: Option<ForgeObjectId>,
        validator: impl Into<String>,
    ) -> Result<Self, ForgeProtocolError> {
        let mut resolution = Self::new(coordinate.revision().clone(), commit, tree)?;
        resolution.authority = Some(Arc::from(coordinate.repository_url()));
        let validator = validator.into();
        if validator.is_empty()
            || validator
                .bytes()
                .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
        {
            return Err(ForgeProtocolError::Malformed);
        }
        resolution.validator = Some(Arc::from(validator));
        Ok(resolution)
    }

    /// Checks the authority and validator binding supplied by a transport.
    pub fn validate_for(&self, coordinate: &ForgeCoordinate) -> Result<(), ForgeProtocolError> {
        if self.requested != coordinate.revision().clone() {
            return Err(ForgeProtocolError::RevisionMismatch);
        }
        if self
            .authority
            .as_deref()
            .is_some_and(|authority| authority != coordinate.repository_url())
        {
            return Err(ForgeProtocolError::AuthorityMismatch);
        }
        if matches!(
            coordinate.revision(),
            ForgeRevision::Tag(_) | ForgeRevision::Branch(_)
        ) && self.validator.is_none()
        {
            return Err(ForgeProtocolError::ValidatorRequired);
        }
        Ok(())
    }

    /// Returns the stable resolved revision identity.
    #[must_use]
    pub fn identity(&self) -> [u8; ID_BYTES] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.forge.resolution.v1\0");
        if let Some(authority) = &self.authority {
            hasher.update(b"authority\0");
            hasher.update(authority.as_bytes());
        }
        if let Some(validator) = &self.validator {
            hasher.update(b"validator\0");
            hasher.update(validator.as_bytes());
        }
        hasher.update(&self.requested.canonical_bytes());
        hasher.update(self.commit.as_hex().as_bytes());
        if let Some(tree) = &self.tree {
            hasher.update(tree.as_hex().as_bytes());
        }
        *hasher.finalize().as_bytes()
    }
}

/// Typed metadata availability retained from the forge authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "value", rename_all = "kebab-case")]
pub enum ForgeFact<T> {
    /// The authority reported the value.
    Recorded(T),
    /// The authority did not provide the value or it was unavailable.
    Unavailable(ForgeUnavailableReason),
}

/// Why one forge metadata value is unavailable.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeUnavailableReason {
    /// The selected authority omits this field.
    AuthorityOmitted,
    /// The metadata endpoint could not be reached.
    Unreachable,
    /// The authority does not implement this operation.
    Unsupported,
    /// Local policy is offline.
    Offline,
    /// A configured bound rejected the response.
    Bounds,
    /// The response was malformed.
    Malformed,
}

impl ForgeUnavailableReason {
    /// Stable product-facing reason spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AuthorityOmitted => "authority omitted",
            Self::Unreachable => "authority unreachable",
            Self::Unsupported => "authority unsupported",
            Self::Offline => "offline",
            Self::Bounds => "response exceeded bounds",
            Self::Malformed => "authority response malformed",
        }
    }
}

/// Repository metadata returned by a forge authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeRepositoryMetadata {
    /// Owner/group as recorded by the authority.
    pub owner: ForgeFact<ProductText>,
    /// Human description, when reported.
    pub description: ForgeFact<ProductText>,
    /// License identifier, when reported.
    pub license: ForgeFact<ProductText>,
    /// README content, bounded and retained as recorded text.
    pub readme: ForgeFact<ProductText>,
    /// Topics/tags, when the authority reports them.
    pub topics: ForgeFact<Box<[ProductText]>>,
    /// Repository stars, only when a numeric field was present.
    pub stars: ForgeFact<u64>,
    /// Repository forks, only when a numeric field was present.
    pub forks: ForgeFact<u64>,
}

impl ForgeRepositoryMetadata {
    /// Creates the typed unavailable baseline for a repository.
    #[must_use]
    pub fn unavailable(owner: &str, reason: ForgeUnavailableReason) -> Self {
        let owner = ProductText::new(owner)
            .map(ForgeFact::Recorded)
            .unwrap_or(ForgeFact::Unavailable(ForgeUnavailableReason::Malformed));
        Self {
            owner,
            description: ForgeFact::Unavailable(reason),
            license: ForgeFact::Unavailable(reason),
            readme: ForgeFact::Unavailable(reason),
            topics: ForgeFact::Unavailable(reason),
            stars: ForgeFact::Unavailable(reason),
            forks: ForgeFact::Unavailable(reason),
        }
    }

    fn from_json(owner: &str, value: &Value) -> Self {
        let recorded_text = |value: Option<&Value>| {
            value
                .and_then(Value::as_str)
                .and_then(|value| ProductText::new(value).ok())
                .map_or(
                    ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
                    ForgeFact::Recorded,
                )
        };
        let recorded_number = |value: Option<&Value>| {
            value.map_or(
                ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
                |value| {
                    value.as_u64().map_or(
                        ForgeFact::Unavailable(ForgeUnavailableReason::Malformed),
                        ForgeFact::Recorded,
                    )
                },
            )
        };
        let topics = value
            .get("topics")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(|value| ProductText::new(value).ok())
                    .take(256)
                    .collect::<Vec<_>>()
                    .into_boxed_slice()
            })
            .map_or(
                ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
                ForgeFact::Recorded,
            );
        Self {
            owner: ProductText::new(
                value
                    .get("owner")
                    .and_then(|owner| owner.get("login").or_else(|| owner.get("username")))
                    .and_then(Value::as_str)
                    .unwrap_or(owner),
            )
            .map_or(
                ForgeFact::Unavailable(ForgeUnavailableReason::Malformed),
                ForgeFact::Recorded,
            ),
            description: recorded_text(value.get("description")),
            license: recorded_text(
                value
                    .get("license")
                    .and_then(|license| license.get("spdx_id").or_else(|| license.get("name")))
                    .or_else(|| value.get("license_name")),
            ),
            readme: ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
            topics,
            stars: recorded_number(value.get("stargazers_count").or_else(|| value.get("stars"))),
            forks: recorded_number(value.get("forks_count").or_else(|| value.get("forks"))),
        }
    }
}

/// Archive encoding supplied by a forge transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForgeArchiveFormat {
    /// POSIX tar, optionally gzip-compressed by the transport.
    Tar,
    /// Gzip-compressed POSIX tar.
    TarGzip,
    /// ZIP archive.
    Zip,
}

/// Bounded source archive handoff from a transport.
pub struct ForgeArchive {
    source: Box<dyn Read>,
    /// Byte encoding of the reader's bytes.
    pub format: ForgeArchiveFormat,
    /// Optional top-level directory to strip before manifest admission.
    pub root_prefix: Option<Arc<str>>,
}

impl fmt::Debug for ForgeArchive {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForgeArchive")
            .field("format", &self.format)
            .field("root_prefix", &self.root_prefix)
            .field("streamed", &true)
            .finish()
    }
}

impl ForgeArchive {
    /// Wraps one owned archive reader. The reader is consumed exactly once by
    /// the acquisition service, which admits it directly into the shared CAS.
    pub fn from_reader<R: Read + 'static>(
        reader: R,
        format: ForgeArchiveFormat,
        root_prefix: Option<impl Into<String>>,
    ) -> Self {
        Self {
            source: Box::new(reader),
            format,
            root_prefix: root_prefix.map(|value| Arc::from(value.into())),
        }
    }

    /// Constructs a tar-gzip handoff with no implicit path stripping.
    #[must_use]
    pub fn tar_gzip(bytes: Vec<u8>) -> Self {
        Self::from_reader(
            Cursor::new(bytes),
            ForgeArchiveFormat::TarGzip,
            None::<String>,
        )
    }

    /// Constructs a plain tar handoff with an optional trusted root prefix.
    #[must_use]
    pub fn tar(bytes: Vec<u8>, root_prefix: Option<impl Into<String>>) -> Self {
        Self::from_reader(Cursor::new(bytes), ForgeArchiveFormat::Tar, root_prefix)
    }

    /// Constructs a zip handoff.
    #[must_use]
    pub fn zip(bytes: Vec<u8>) -> Self {
        Self::from_reader(Cursor::new(bytes), ForgeArchiveFormat::Zip, None::<String>)
    }

    fn into_reader(self) -> Box<dyn Read> {
        self.source
    }
}

/// A bounded, transport-independent forge request.
pub trait ForgeTransport {
    /// Resolves a typed ref to an exact commit/tree object.
    fn resolve(
        &mut self,
        coordinate: &ForgeCoordinate,
    ) -> Result<ForgeResolution, ForgeTransportError>;
    /// Fetches the exact source archive selected by `resolution`.
    fn fetch_archive(
        &mut self,
        _coordinate: &ForgeCoordinate,
        resolution: &ForgeResolution,
    ) -> Result<ForgeArchive, ForgeTransportError>;
    /// Fetches bounded repository metadata. The default preserves typed
    /// unavailable facts and does not make a second network request.
    fn fetch_metadata(
        &mut self,
        coordinate: &ForgeCoordinate,
        _resolution: &ForgeResolution,
    ) -> Result<ForgeRepositoryMetadata, ForgeTransportError> {
        Ok(ForgeRepositoryMetadata::unavailable(
            coordinate.owner(),
            ForgeUnavailableReason::Unsupported,
        ))
    }
}

/// Typed transport failures without endpoint or credential strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeTransportError {
    /// Source is unavailable or timed out.
    Unavailable,
    /// Source requested bounded retry.
    RetryAfter(u64),
    /// Remote object or repository was absent.
    NotFound,
    /// Response exceeded a configured bound.
    Bounds,
    /// Response did not satisfy the typed forge protocol.
    Protocol,
    /// Remote bytes did not match the exact selected object.
    Integrity,
    /// Credentials or local policy rejected the request.
    Policy,
}

/// Closed result algebra for forge acquisition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ForgeAcquisitionOutcome {
    /// Exact source tree, metadata, and package manifests were admitted.
    Hit(Arc<ForgeAcquisitionResult>),
    /// The source was absent from the local cache while offline.
    Offline,
    /// The source could not be reached.
    Unavailable,
    /// The source asked for a later retry.
    RetryAfter(u64),
    /// The request or response was rejected by policy/protocol.
    Rejected(ForgeRejectReason),
    /// A local cache object or journal failed integrity checks.
    Corrupt,
}

/// Why forge acquisition was rejected locally.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ForgeRejectReason {
    /// A bound was exceeded.
    Bounds,
    /// The selected revision did not match the requested identity.
    RevisionMismatch,
    /// Archive path or format was unsafe.
    Archive,
    /// A package manifest was malformed.
    Manifest,
    /// Metadata response was malformed.
    Metadata,
    /// A delegated object failed local verification.
    Integrity,
    /// Network policy denied the operation.
    Policy,
}

/// One discovered package manifest inside an admitted forge tree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgePackageManifest {
    /// Manifest path relative to the selected repository subdirectory.
    pub path: Arc<str>,
    /// Ecosystem grammar selected by the manifest filename.
    pub ecosystem: RegistryEcosystem,
    /// Package name when the manifest records one.
    pub name: ForgeFact<ProductText>,
    /// Package version when the manifest records one. It remains absent when
    /// a workspace inherits or otherwise omits its version.
    pub version: ForgeFact<ProductText>,
    /// Dependency graph facts emitted by the manifest parser.
    pub dependencies: DependencyFacts<Box<[PackageDependencyRecord]>>,
}

/// Immutable forge result consumed by package pages and compiler adapters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForgeAcquisitionResult {
    /// Canonical coordinate requested by the caller.
    pub coordinate: ForgeCoordinate,
    /// Exact remote commit/tree resolution.
    pub resolution: ForgeResolution,
    /// Content identity of the original archive bytes.
    pub archive: RawArchiveObjectId,
    /// Shared content-addressed tree manifest.
    pub tree: Arc<TreeManifest>,
    /// Repository metadata retained with typed availability.
    pub metadata: ForgeRepositoryMetadata,
    /// Package manifests discovered under the optional subdirectory.
    pub manifests: Box<[ForgePackageManifest]>,
    /// Shared canonical source snapshot for the selected tree.
    pub snapshot: Arc<SourceSnapshot>,
    /// Root-bound delta from the forge source genesis snapshot.
    pub delta: Arc<AcquisitionDelta>,
    /// Immutable receipt binding coordinate, commit, tree, and archive.
    pub receipt: ForgeReceipt,
}

impl ForgeAcquisitionResult {
    /// Projects an admitted forge result into the GUI/product package DTO.
    /// The projection preserves every recorded/unavailable metadata fact and
    /// uses the same manifest paths and dependency rows admitted by the
    /// shared package graph.
    pub fn product_record(
        &self,
    ) -> Result<backend_library::ForgePackageRecord, ForgeProtocolError> {
        fn text(value: &str) -> Result<ProductText, ForgeProtocolError> {
            ProductText::new(value).map_err(|_| ForgeProtocolError::Malformed)
        }
        fn fact<T, U, F>(
            value: &ForgeFact<T>,
            map: F,
        ) -> Result<backend_library::ForgeFact<U>, ForgeProtocolError>
        where
            F: FnOnce(&T) -> Result<U, ForgeProtocolError>,
        {
            match value {
                ForgeFact::Recorded(value) => map(value).map(backend_library::ForgeFact::Recorded),
                ForgeFact::Unavailable(reason) => Ok(backend_library::ForgeFact::Unavailable(
                    ProductText::from_static(reason.as_str()),
                )),
            }
        }
        let metadata = backend_library::ForgeRepositoryMetadataRecord {
            owner: fact(&self.metadata.owner, |value| Ok(value.clone()))?,
            description: fact(&self.metadata.description, |value| Ok(value.clone()))?,
            license: fact(&self.metadata.license, |value| Ok(value.clone()))?,
            readme: fact(&self.metadata.readme, |value| Ok(value.clone()))?,
            topics: fact(&self.metadata.topics, |value| Ok(value.clone()))?,
            stars: fact(&self.metadata.stars, |value| Ok(*value))?,
            forks: fact(&self.metadata.forks, |value| Ok(*value))?,
        };
        let manifests = self
            .manifests
            .iter()
            .map(|manifest| {
                let (name, version) = match (&manifest.name, &manifest.version) {
                    (ForgeFact::Recorded(name), ForgeFact::Recorded(version)) => {
                        (Some(name.clone()), Some(version.clone()))
                    }
                    (ForgeFact::Recorded(name), _) => (Some(name.clone()), None),
                    (_, ForgeFact::Recorded(version)) => (None, Some(version.clone())),
                    _ => (None, None),
                };
                let dependency_count = match &manifest.dependencies {
                    DependencyFacts::Known(rows) => {
                        u16::try_from(rows.len()).map_err(|_| ForgeProtocolError::Malformed)?
                    }
                    DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => 0,
                };
                Ok(backend_library::ForgeManifestRecord {
                    path: text(manifest.path.as_ref())?,
                    ecosystem: text(manifest.ecosystem.as_str())?,
                    name,
                    version,
                    dependency_count,
                })
            })
            .collect::<Result<Vec<_>, ForgeProtocolError>>()?
            .into_boxed_slice();
        let revision =
            String::from_utf8_lossy(&self.coordinate.revision().canonical_bytes()).into_owned();
        let commit = backend_library::ForgeFact::Recorded(text(&self.resolution.commit.as_hex())?);
        let tree = match &self.resolution.tree {
            Some(tree) => backend_library::ForgeFact::Recorded(text(&tree.as_hex())?),
            None => backend_library::ForgeFact::Unavailable(ProductText::from_static(
                ForgeUnavailableReason::AuthorityOmitted.as_str(),
            )),
        };
        Ok(backend_library::ForgePackageRecord {
            coordinate: text(&self.coordinate.canonical())?,
            provider: text(self.coordinate.provider().as_str())?,
            owner: text(self.coordinate.owner())?,
            repository: text(self.coordinate.repository())?,
            revision: text(&revision)?,
            subdir: self.coordinate.subdir().map(text).transpose()?,
            commit,
            tree,
            metadata,
            manifests,
            source: backend_library::ForgeFact::Recorded(text(&hex(&self.tree.id().to_bytes()))?),
        })
    }
}

/// Durable forge publication receipt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeReceipt {
    /// Canonical coordinate identity.
    pub coordinate: [u8; ID_BYTES],
    /// Exact resolved commit identity.
    pub commit: ForgeObjectId,
    /// Exact remote tree identity, if reported.
    pub remote_tree: Option<ForgeObjectId>,
    /// Shared admitted content tree identity.
    pub tree: [u8; ID_BYTES],
    /// Original archive content identity.
    pub archive: [u8; ID_BYTES],
    /// Shared source snapshot root.
    pub snapshot: [u8; ID_BYTES],
    /// Shared acquisition delta identity.
    pub delta: [u8; ID_BYTES],
    /// Diagnostic observation time; it is not part of any source or snapshot
    /// identity and never selects a journal record.
    pub observed_at_millis: u64,
}

/// Remote delegation request pinned to one exact commit object.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForgeDelegationRequest {
    /// Canonical coordinate identity without credentials.
    pub coordinate: ForgeCoordinate,
    /// Exact commit object that a remote worker must return.
    pub commit: ForgeObjectId,
}

impl ForgeDelegationRequest {
    /// Creates a request only after the caller has an exact commit resolution.
    pub fn new(coordinate: ForgeCoordinate) -> Result<Self, ForgeProtocolError> {
        let ForgeRevision::Commit(commit) = coordinate.revision().clone() else {
            return Err(ForgeProtocolError::ExactCommitRequired);
        };
        Ok(Self { coordinate, commit })
    }
}

/// Delegated exact source object returned by a remote worker.
#[derive(Debug)]
pub struct ForgeDelegatedObject {
    /// Exact commit object the worker claims to have served.
    pub commit: ForgeObjectId,
    /// Source archive selected by the worker.
    pub archive: ForgeArchive,
}

/// Typed forge protocol errors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeProtocolError {
    /// A requested tag/branch was resolved to another commit than the exact
    /// commit request permitted.
    RevisionMismatch,
    /// Remote delegation must name an exact commit.
    ExactCommitRequired,
    /// Delegated response named a different commit.
    DelegatedCommitMismatch,
    /// A resolution was returned by a different repository authority.
    AuthorityMismatch,
    /// A mutable tag or branch did not include an observed validator.
    ValidatorRequired,
    /// A canonical field was malformed.
    Malformed,
}

/// Verifies a delegated exact commit claim before local archive admission.
pub fn verify_delegated_object(
    request: &ForgeDelegationRequest,
    response: ForgeDelegatedObject,
) -> Result<ForgeArchive, ForgeProtocolError> {
    if response.commit != request.commit {
        return Err(ForgeProtocolError::DelegatedCommitMismatch);
    }
    Ok(response.archive)
}

/// Persistent local-first policy for forge acquisition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForgeAcquisitionPolicy {
    /// Resolve and fetch missing objects through the configured transport.
    Online,
    /// Read only durable records and content objects.
    Offline,
}

/// Bounds applied before an archive, metadata body, or manifest can be read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ForgeAcquisitionLimits {
    /// Maximum source archive bytes.
    pub max_archive_bytes: u64,
    /// Maximum metadata response bytes.
    pub max_metadata_bytes: usize,
    /// Maximum README bytes retained in metadata.
    pub max_readme_bytes: usize,
    /// Maximum admitted file rows/bytes in the source tree.
    pub archive_budget: ArchiveBudget,
}

impl Default for ForgeAcquisitionLimits {
    fn default() -> Self {
        Self {
            max_archive_bytes: MAX_ARCHIVE_BYTES,
            max_metadata_bytes: MAX_METADATA_BYTES,
            max_readme_bytes: MAX_README_BYTES,
            archive_budget: ArchiveBudget {
                max_entries: 100_000,
                max_bytes: 256 * 1024 * 1024,
                max_path_bytes: 4 * 1024,
                max_entry_bytes: 64 * 1024 * 1024,
            },
        }
    }
}

impl ForgeAcquisitionLimits {
    fn validate(self) -> Result<Self, ForgeRejectReason> {
        if self.max_archive_bytes == 0
            || self.max_archive_bytes > MAX_ARCHIVE_BYTES
            || self.max_metadata_bytes == 0
            || self.max_metadata_bytes > MAX_METADATA_BYTES
            || self.max_readme_bytes == 0
            || self.max_readme_bytes > MAX_README_BYTES
            || self.archive_budget.max_entries == 0
            || self.archive_budget.max_bytes == 0
            || self.archive_budget.max_bytes > MAX_MANIFEST_BYTES.saturating_mul(64)
        {
            return Err(ForgeRejectReason::Bounds);
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgeJournalRecord {
    coordinate: ForgeCoordinate,
    resolution: ForgeResolution,
    archive: [u8; ID_BYTES],
    tree: Vec<ForgeJournalEntry>,
    metadata: ForgeRepositoryMetadata,
    manifests: Vec<ForgePackageManifest>,
    source: [u8; ID_BYTES],
    cursor: [u8; ID_BYTES],
    facts_frontier: [u8; ID_BYTES],
    base_snapshot: [u8; ID_BYTES],
    target_snapshot: [u8; ID_BYTES],
    delta: [u8; ID_BYTES],
    observed_at_millis: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgeJournalEntry {
    path: Arc<str>,
    object: [u8; ID_BYTES],
    mode: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
enum ForgeJournalEvent {
    /// A durable source snapshot and its canonical acquisition delta.
    Published(ForgeJournalRecord),
    /// A durable negative observation. It remains in the append-only history
    /// until a later publication supersedes it.
    Tombstone {
        coordinate: ForgeCoordinate,
        reason: ForgeRejectReason,
    },
}

struct ForgeLog;

impl JournalDomain for ForgeLog {
    const DOMAIN: u8 = 0x92;
    const TYPE: u16 = 1;
    const VERSION: u8 = 1;
}

impl JournalCodec for ForgeLog {
    type Record = ForgeJournalEvent;

    fn encode(record: &Self::Record, output: &mut Vec<u8>) {
        // serde_json emits fields in declaration order, which is the
        // canonical payload grammar for this private typed journal.
        if let Ok(bytes) = serde_json::to_vec(record) {
            output.extend_from_slice(&bytes);
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        serde_json::from_slice(bytes).map_err(|_| JournalError::Corrupt("forge record"))
    }
}

impl ForgeJournalEvent {
    fn coordinate(&self) -> &ForgeCoordinate {
        match self {
            Self::Published(record) => &record.coordinate,
            Self::Tombstone { coordinate, .. } => coordinate,
        }
    }
}

/// Durable source acquisition owner for all forge providers.
pub struct ForgeAcquisitionService {
    store: ContentAddressedStore,
    journal: Arc<HashChainJournal<ForgeLog>>,
    catalog: Arc<Mutex<BTreeMap<[u8; ID_BYTES], ForgeJournalEvent>>>,
    policy: ForgeAcquisitionPolicy,
    limits: ForgeAcquisitionLimits,
}

impl fmt::Debug for ForgeAcquisitionService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForgeAcquisitionService")
            .field("policy", &self.policy)
            .field("limits", &self.limits)
            .field("journal_path", &self.journal.path())
            .finish_non_exhaustive()
    }
}

impl ForgeAcquisitionService {
    /// Opens a durable content store and reloads its credential-free journal.
    pub fn open(
        root: impl Into<PathBuf>,
        policy: ForgeAcquisitionPolicy,
        limits: ForgeAcquisitionLimits,
    ) -> Result<Self, ForgeAcquisitionError> {
        let limits = limits.validate().map_err(ForgeAcquisitionError::Rejected)?;
        let root = root.into();
        fs::create_dir_all(&root).map_err(ForgeAcquisitionError::Io)?;
        let store = ContentAddressedStore::open(root.join("content"))
            .map_err(ForgeAcquisitionError::Content)?;
        let catalog = Arc::new(Mutex::new(BTreeMap::new()));
        let catalog_for_recovery = Arc::clone(&catalog);
        let (journal, _) = HashChainJournal::<ForgeLog>::open_streaming_with(
            root.join("forge.journal"),
            JournalLimits::default(),
            move |frame| {
                let record = ForgeLog::decode(frame.payload)?;
                if !record.coordinate().identity_is_valid() {
                    return Err(JournalError::Corrupt("forge coordinate identity"));
                }
                catalog_for_recovery
                    .lock()
                    .map_err(|_| JournalError::Corrupt("forge catalog lock"))?
                    .insert(record.coordinate().identity(), record);
                Ok(())
            },
        )
        .map_err(ForgeAcquisitionError::Journal)?;
        Ok(Self {
            store,
            journal: Arc::new(journal),
            catalog,
            policy,
            limits,
        })
    }

    /// Returns the shared content store used by this forge owner.
    #[must_use]
    pub const fn store(&self) -> &ContentAddressedStore {
        &self.store
    }

    /// Returns one exact cached source without network I/O.
    pub fn reference(
        &self,
        coordinate: &ForgeCoordinate,
    ) -> Result<Option<Arc<ForgeAcquisitionResult>>, ForgeAcquisitionError> {
        let event = self
            .catalog
            .lock()
            .map_err(|_| ForgeAcquisitionError::Corrupt)?
            .get(&coordinate.identity())
            .cloned();
        match event {
            Some(ForgeJournalEvent::Published(record)) => {
                if !record.coordinate.identity_is_valid() || record.coordinate != *coordinate {
                    return Err(ForgeAcquisitionError::Corrupt);
                }
                self.rehydrate(record).map(Some)
            }
            Some(ForgeJournalEvent::Tombstone {
                coordinate: stored, ..
            }) => {
                if !stored.identity_is_valid() || stored != *coordinate {
                    return Err(ForgeAcquisitionError::Corrupt);
                }
                Ok(None)
            }
            None => Ok(None),
        }
    }

    /// Acquires one exact source through a typed transport and the shared CAS.
    pub fn acquire<T: ForgeTransport>(
        &self,
        coordinate: &ForgeCoordinate,
        transport: &mut T,
    ) -> ForgeAcquisitionOutcome {
        match self.reference(coordinate) {
            Ok(Some(result)) => return ForgeAcquisitionOutcome::Hit(result),
            Err(ForgeAcquisitionError::Corrupt) => return ForgeAcquisitionOutcome::Corrupt,
            Err(_) => {}
            Ok(None) => {}
        }
        if self.policy == ForgeAcquisitionPolicy::Offline {
            return ForgeAcquisitionOutcome::Offline;
        }
        let resolution = match transport.resolve(coordinate) {
            Ok(resolution) => resolution,
            Err(ForgeTransportError::Unavailable) => return ForgeAcquisitionOutcome::Unavailable,
            Err(ForgeTransportError::RetryAfter(millis)) => {
                return ForgeAcquisitionOutcome::RetryAfter(millis);
            }
            Err(ForgeTransportError::NotFound) => {
                return match self.commit_tombstone(coordinate, ForgeRejectReason::RevisionMismatch)
                {
                    Ok(()) => {
                        ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::RevisionMismatch)
                    }
                    Err(_) => ForgeAcquisitionOutcome::Corrupt,
                };
            }
            Err(ForgeTransportError::Bounds) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Bounds);
            }
            Err(ForgeTransportError::Protocol | ForgeTransportError::Policy) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Policy);
            }
            Err(ForgeTransportError::Integrity) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Integrity);
            }
        };
        if resolution.validate_for(coordinate).is_err() {
            return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::RevisionMismatch);
        }
        let archive = match transport.fetch_archive(coordinate, &resolution) {
            Ok(archive) => archive,
            Err(ForgeTransportError::Unavailable) => return ForgeAcquisitionOutcome::Unavailable,
            Err(ForgeTransportError::RetryAfter(millis)) => {
                return ForgeAcquisitionOutcome::RetryAfter(millis);
            }
            Err(ForgeTransportError::Bounds) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Bounds);
            }
            Err(ForgeTransportError::Integrity) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Integrity);
            }
            Err(
                ForgeTransportError::NotFound
                | ForgeTransportError::Protocol
                | ForgeTransportError::Policy,
            ) => return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Archive),
        };
        let metadata = match transport.fetch_metadata(coordinate, &resolution) {
            Ok(metadata) => metadata,
            Err(ForgeTransportError::Bounds) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Bounds);
            }
            Err(ForgeTransportError::Protocol) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Metadata);
            }
            Err(ForgeTransportError::Policy) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Policy);
            }
            Err(ForgeTransportError::Unavailable | ForgeTransportError::NotFound) => {
                ForgeRepositoryMetadata::unavailable(
                    coordinate.owner(),
                    ForgeUnavailableReason::Unreachable,
                )
            }
            Err(ForgeTransportError::RetryAfter(_)) => ForgeRepositoryMetadata::unavailable(
                coordinate.owner(),
                ForgeUnavailableReason::Unreachable,
            ),
            Err(ForgeTransportError::Integrity) => {
                return ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Integrity);
            }
        };
        match self.admit(coordinate, resolution, archive, metadata) {
            Ok(result) => ForgeAcquisitionOutcome::Hit(Arc::new(result)),
            Err(ForgeAcquisitionError::Rejected(reason)) => {
                ForgeAcquisitionOutcome::Rejected(reason)
            }
            Err(ForgeAcquisitionError::Content(_)) => {
                ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::Integrity)
            }
            Err(
                ForgeAcquisitionError::Io(_)
                | ForgeAcquisitionError::Journal(_)
                | ForgeAcquisitionError::Corrupt,
            ) => ForgeAcquisitionOutcome::Corrupt,
        }
    }

    fn rehydrate(
        &self,
        record: ForgeJournalRecord,
    ) -> Result<Arc<ForgeAcquisitionResult>, ForgeAcquisitionError> {
        record
            .resolution
            .validate_for(&record.coordinate)
            .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        if record.source != record.coordinate.identity()
            || record.cursor != resolution_cursor(&record.resolution)
        {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        let mut entries = Vec::with_capacity(record.tree.len());
        for entry in record.tree {
            let object = RawArchiveObjectId::from_encoded(entry.object);
            let _ = self
                .store
                .verify_object(object, self.limits.archive_budget.max_entry_bytes)
                .map_err(|_| ForgeAcquisitionError::Corrupt)?;
            entries.push(ManifestEntry {
                path: entry.path,
                object,
                mode: entry.mode,
            });
        }
        let tree =
            Arc::new(TreeManifest::new(entries).map_err(|_| ForgeAcquisitionError::Corrupt)?);
        let archive = RawArchiveObjectId::from_encoded(record.archive);
        let _ = self
            .store
            .verify_object(archive, self.limits.max_archive_bytes)
            .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        let empty =
            Arc::new(TreeManifest::new(Vec::new()).map_err(|_| ForgeAcquisitionError::Corrupt)?);
        let base = SourceSnapshot::new(record.source, [0; ID_BYTES], 0, empty, Vec::new())
            .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        if base.id().to_bytes() != record.base_snapshot {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        let target = SourceSnapshot::new_with_frontier(
            record.source,
            record.cursor,
            0,
            record.facts_frontier,
            Arc::clone(&tree),
            Vec::new(),
        )
        .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        if record.facts_frontier != metadata_frontier(&record.metadata, &record.manifests) {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        if target.id().to_bytes() != record.target_snapshot {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        let changes = tree
            .entries()
            .iter()
            .map(|entry| DeltaChange {
                path: Arc::clone(&entry.path),
                before: None,
                before_mode: None,
                after: Some(entry.object),
                after_mode: Some(entry.mode),
            })
            .collect();
        let delta = Arc::new(
            AcquisitionDelta::new(&base, Arc::new(target.clone()), changes)
                .map_err(|_| ForgeAcquisitionError::Corrupt)?,
        );
        if delta.id().as_bytes() != &record.delta {
            return Err(ForgeAcquisitionError::Corrupt);
        }
        let receipt = ForgeReceipt {
            coordinate: record.coordinate.identity(),
            commit: record.resolution.commit.clone(),
            remote_tree: record.resolution.tree.clone(),
            tree: tree.id().to_bytes(),
            archive: record.archive,
            snapshot: record.target_snapshot,
            delta: record.delta,
            observed_at_millis: record.observed_at_millis,
        };
        Ok(Arc::new(ForgeAcquisitionResult {
            coordinate: record.coordinate,
            resolution: record.resolution,
            archive,
            tree,
            metadata: record.metadata,
            manifests: record.manifests.into_boxed_slice(),
            snapshot: Arc::new(target),
            delta,
            receipt,
        }))
    }

    fn admit(
        &self,
        coordinate: &ForgeCoordinate,
        resolution: ForgeResolution,
        archive: ForgeArchive,
        metadata: ForgeRepositoryMetadata,
    ) -> Result<ForgeAcquisitionResult, ForgeAcquisitionError> {
        let format = archive.format;
        let root_prefix = archive.root_prefix.clone();
        let archive_id = self
            .store
            .admit_reader(None, archive.into_reader(), self.limits.max_archive_bytes)
            .map_err(content_error)?
            .object();
        let mut archive_object = self
            .store
            .open_object(archive_id)
            .map_err(ForgeAcquisitionError::Content)?;
        let files = extract_archive(
            &mut archive_object,
            format,
            root_prefix.as_deref(),
            &self.limits.archive_budget,
        )
        .map_err(ForgeAcquisitionError::Rejected)?;
        let manifests = discover_manifests(&files, coordinate.subdir())
            .map_err(ForgeAcquisitionError::Rejected)?;
        let mut builder = ArchiveManifestBuilder::new(self.limits.archive_budget);
        for file in files {
            let object = self
                .store
                .admit_reader(
                    None,
                    Cursor::new(file.bytes),
                    self.limits.archive_budget.max_entry_bytes,
                )
                .map_err(content_error)?
                .object();
            builder
                .push_file(Arc::clone(&file.path), object, file.bytes_len, file.mode)
                .map_err(ForgeAcquisitionError::Content)?;
        }
        let tree = Arc::new(
            builder
                .finish()
                .map_err(ForgeAcquisitionError::Content)?
                .tree()
                .clone(),
        );
        // Parse from the same bounded file rows that were admitted into the
        // shared CAS. The raw archive is never cloned or re-read into a
        // second full archive buffer.
        let empty =
            Arc::new(TreeManifest::new(Vec::new()).map_err(|_| ForgeAcquisitionError::Corrupt)?);
        let source = coordinate.identity();
        let cursor = resolution_cursor(&resolution);
        let facts_frontier = metadata_frontier(&metadata, &manifests);
        let base = SourceSnapshot::new(source, [0; ID_BYTES], 0, empty, Vec::new())
            .map_err(|_| ForgeAcquisitionError::Corrupt)?;
        let target = Arc::new(
            SourceSnapshot::new_with_frontier(
                source,
                cursor,
                0,
                facts_frontier,
                Arc::clone(&tree),
                Vec::new(),
            )
            .map_err(|_| ForgeAcquisitionError::Corrupt)?,
        );
        let changes = tree
            .entries()
            .iter()
            .map(|entry| DeltaChange {
                path: Arc::clone(&entry.path),
                before: None,
                before_mode: None,
                after: Some(entry.object),
                after_mode: Some(entry.mode),
            })
            .collect();
        let delta = Arc::new(
            AcquisitionDelta::new(&base, Arc::clone(&target), changes)
                .map_err(|_| ForgeAcquisitionError::Corrupt)?,
        );
        let receipt = ForgeReceipt {
            coordinate: coordinate.identity(),
            commit: resolution.commit.clone(),
            remote_tree: resolution.tree.clone(),
            tree: tree.id().to_bytes(),
            archive: archive_id.to_bytes(),
            snapshot: target.id().to_bytes(),
            delta: delta.id().to_bytes(),
            observed_at_millis: now_millis(),
        };
        let record = ForgeJournalRecord {
            coordinate: coordinate.clone(),
            resolution: resolution.clone(),
            archive: archive_id.to_bytes(),
            tree: tree
                .entries()
                .iter()
                .map(|entry| ForgeJournalEntry {
                    path: Arc::clone(&entry.path),
                    object: entry.object.to_bytes(),
                    mode: entry.mode,
                })
                .collect(),
            metadata: metadata.clone(),
            manifests: manifests.clone(),
            source,
            cursor,
            facts_frontier,
            base_snapshot: base.id().to_bytes(),
            target_snapshot: target.id().to_bytes(),
            delta: delta.id().to_bytes(),
            observed_at_millis: receipt.observed_at_millis,
        };
        self.commit_record(record)?;
        Ok(ForgeAcquisitionResult {
            coordinate: coordinate.clone(),
            resolution,
            archive: archive_id,
            tree,
            metadata,
            manifests: manifests.into_boxed_slice(),
            snapshot: target,
            delta,
            receipt,
        })
    }

    fn commit_record(&self, record: ForgeJournalRecord) -> Result<(), ForgeAcquisitionError> {
        self.journal
            .append(&ForgeJournalEvent::Published(record.clone()))
            .map_err(ForgeAcquisitionError::Journal)?;
        self.catalog
            .lock()
            .map_err(|_| ForgeAcquisitionError::Corrupt)?
            .insert(
                record.coordinate.identity(),
                ForgeJournalEvent::Published(record),
            );
        Ok(())
    }

    fn commit_tombstone(
        &self,
        coordinate: &ForgeCoordinate,
        reason: ForgeRejectReason,
    ) -> Result<(), ForgeAcquisitionError> {
        let event = ForgeJournalEvent::Tombstone {
            coordinate: coordinate.clone(),
            reason,
        };
        self.journal
            .append(&event)
            .map_err(ForgeAcquisitionError::Journal)?;
        self.catalog
            .lock()
            .map_err(|_| ForgeAcquisitionError::Corrupt)?
            .insert(coordinate.identity(), event);
        Ok(())
    }
}

/// Forge owner failure after transport acquisition.
#[derive(Debug)]
pub enum ForgeAcquisitionError {
    /// Durable content store failure.
    Content(ContentStoreError),
    /// Journal or filesystem failure.
    Io(io::Error),
    /// Shared hash-chain journal failure.
    Journal(JournalError),
    /// Journal or object state was corrupt.
    Corrupt,
    /// Typed protocol/policy rejection.
    Rejected(ForgeRejectReason),
}

impl fmt::Display for ForgeAcquisitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Content(error) => write!(formatter, "forge content admission failed: {error}"),
            Self::Io(error) => write!(formatter, "forge persistence failed: {error}"),
            Self::Journal(error) => write!(formatter, "forge journal failed: {error}"),
            Self::Corrupt => formatter.write_str("forge cache or journal is corrupt"),
            Self::Rejected(reason) => write!(formatter, "forge acquisition rejected: {reason:?}"),
        }
    }
}
impl std::error::Error for ForgeAcquisitionError {}

fn content_error(error: ContentStoreError) -> ForgeAcquisitionError {
    if matches!(error, ContentStoreError::Bounds { .. }) {
        ForgeAcquisitionError::Rejected(ForgeRejectReason::Bounds)
    } else {
        ForgeAcquisitionError::Content(error)
    }
}

#[derive(Clone)]
struct ArchiveFile {
    path: Arc<str>,
    bytes: Vec<u8>,
    bytes_len: u64,
    mode: u32,
}

fn extract_archive<R: Read + Seek>(
    reader: &mut R,
    format: ForgeArchiveFormat,
    root_prefix: Option<&str>,
    budget: &ArchiveBudget,
) -> Result<Vec<ArchiveFile>, ForgeRejectReason> {
    match format {
        ForgeArchiveFormat::Tar => parse_tar_stream(reader, root_prefix, budget),
        ForgeArchiveFormat::TarGzip => {
            let mut decoded = GzDecoder::new(reader);
            parse_tar_stream(&mut decoded, root_prefix, budget)
        }
        ForgeArchiveFormat::Zip => parse_zip(reader, root_prefix, budget),
    }
}

fn normalize_path(
    path: &str,
    root_prefix: Option<&str>,
) -> Result<Option<Arc<str>>, ForgeRejectReason> {
    let path = path.trim_start_matches("./").trim_end_matches('/');
    if path.is_empty()
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ForgeRejectReason::Archive);
    }
    let path = if let Some(prefix) = root_prefix {
        let prefix = prefix.trim_end_matches('/');
        if prefix.is_empty()
            || prefix.starts_with('/')
            || prefix.contains('\\')
            || prefix
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err(ForgeRejectReason::Archive);
        }
        let Some(path) = path.strip_prefix(prefix) else {
            return Ok(None);
        };
        let path = path.strip_prefix('/').ok_or(ForgeRejectReason::Archive)?;
        path
    } else {
        path
    };
    if path.is_empty() {
        return Ok(None);
    }
    Ok(Some(Arc::from(path)))
}

fn parse_tar_stream<R: Read>(
    reader: &mut R,
    root_prefix: Option<&str>,
    budget: &ArchiveBudget,
) -> Result<Vec<ArchiveFile>, ForgeRejectReason> {
    let mut files = Vec::new();
    let mut total = 0u64;
    loop {
        let mut header = [0_u8; 512];
        match reader.read_exact(&mut header) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                return Err(ForgeRejectReason::Archive);
            }
            Err(_) => return Err(ForgeRejectReason::Archive),
        }
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let name = tar_field(&header[..100])?;
        let mode = tar_octal(&header[100..108])
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        let size = tar_octal(&header[124..136]).ok_or(ForgeRejectReason::Archive)?;
        let kind = header[156];
        if size > budget.max_entry_bytes {
            return Err(ForgeRejectReason::Bounds);
        }
        let selected_path = normalize_path(&name, root_prefix)?;
        let mut content = Vec::new();
        if kind == b'0' || kind == 0 {
            if selected_path.is_some() {
                total = total.checked_add(size).ok_or(ForgeRejectReason::Bounds)?;
                if total > budget.max_bytes || files.len() >= budget.max_entries {
                    return Err(ForgeRejectReason::Bounds);
                }
                content.reserve(usize::try_from(size).map_err(|_| ForgeRejectReason::Bounds)?);
            }
        } else if !matches!(kind, b'5' | b'x' | b'g') {
            return Err(ForgeRejectReason::Archive);
        }
        if kind == b'0' || kind == 0 {
            reader
                .take(size)
                .read_to_end(&mut content)
                .map_err(|_| ForgeRejectReason::Archive)?;
            if u64::try_from(content.len()).unwrap_or(u64::MAX) != size {
                return Err(ForgeRejectReason::Archive);
            }
            if let Some(path) = selected_path {
                files.push(ArchiveFile {
                    path,
                    bytes: content,
                    bytes_len: size,
                    mode,
                });
            }
        } else {
            io::copy(&mut reader.take(size), &mut io::sink())
                .map_err(|_| ForgeRejectReason::Archive)?;
        }
        let padding = (512 - (usize::try_from(size).unwrap_or(0) % 512)) % 512;
        io::copy(
            &mut reader.take(u64::try_from(padding).unwrap_or(0)),
            &mut io::sink(),
        )
        .map_err(|_| ForgeRejectReason::Archive)?;
    }
    if files.is_empty() {
        return Err(ForgeRejectReason::Archive);
    }
    Ok(files)
}

fn tar_field(bytes: &[u8]) -> Result<String, ForgeRejectReason> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8(bytes[..end].to_vec()).map_err(|_| ForgeRejectReason::Archive)
}

fn tar_octal(bytes: &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    let mut digits = 0_u8;
    for byte in bytes.iter().copied() {
        if byte == 0 || (byte == b' ' && digits > 0) {
            break;
        }
        if byte == b' ' {
            continue;
        }
        if !(b'0'..=b'7').contains(&byte) {
            return None;
        }
        value = value.checked_mul(8)?.checked_add(u64::from(byte - b'0'))?;
        digits = digits.saturating_add(1);
    }
    if digits == 0 { Some(0) } else { Some(value) }
}

fn parse_zip<R: Read + Seek>(
    reader: &mut R,
    root_prefix: Option<&str>,
    budget: &ArchiveBudget,
) -> Result<Vec<ArchiveFile>, ForgeRejectReason> {
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| ForgeRejectReason::Archive)?;
    let mut archive = zip::ZipArchive::new(reader).map_err(|_| ForgeRejectReason::Archive)?;
    if archive.len() > budget.max_entries {
        return Err(ForgeRejectReason::Bounds);
    }
    let mut files = Vec::new();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| ForgeRejectReason::Archive)?;
        if file.is_dir() {
            continue;
        }
        let Some(path) = normalize_path(file.name(), root_prefix)? else {
            continue;
        };
        let size = file.size();
        if size > budget.max_entry_bytes {
            return Err(ForgeRejectReason::Bounds);
        }
        total = total.checked_add(size).ok_or(ForgeRejectReason::Bounds)?;
        if total > budget.max_bytes {
            return Err(ForgeRejectReason::Bounds);
        }
        let mut content = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        file.take(size.saturating_add(1))
            .read_to_end(&mut content)
            .map_err(|_| ForgeRejectReason::Archive)?;
        if u64::try_from(content.len()).unwrap_or(u64::MAX) != size {
            return Err(ForgeRejectReason::Archive);
        }
        files.push(ArchiveFile {
            path,
            bytes: content,
            bytes_len: size,
            mode: 0,
        });
    }
    if files.is_empty() {
        return Err(ForgeRejectReason::Archive);
    }
    Ok(files)
}

fn discover_manifests(
    files: &[ArchiveFile],
    subdir: Option<&str>,
) -> Result<Vec<ForgePackageManifest>, ForgeRejectReason> {
    let mut results = Vec::new();
    for file in files {
        if file.bytes_len > MAX_MANIFEST_BYTES {
            return Err(ForgeRejectReason::Bounds);
        }
        let path = match subdir {
            Some(prefix) => {
                let prefix = prefix.trim_end_matches('/');
                let Some(path) = file.path.strip_prefix(prefix) else {
                    continue;
                };
                let Some(path) = path.strip_prefix('/') else {
                    continue;
                };
                path
            }
            None => file.path.as_ref(),
        };
        let Some(kind) = manifest_kind(path) else {
            continue;
        };
        let parsed = parse_manifest(kind, path, &file.bytes)?;
        results.push(parsed);
    }
    results.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(results)
}

#[derive(Clone, Copy)]
enum ManifestKind {
    Cargo,
    Npm,
    Pypi,
    Golang,
    Maven,
}

fn manifest_kind(path: &str) -> Option<ManifestKind> {
    match path.rsplit('/').next()? {
        "Cargo.toml" => Some(ManifestKind::Cargo),
        "package.json" => Some(ManifestKind::Npm),
        "pyproject.toml" | "setup.cfg" => Some(ManifestKind::Pypi),
        "go.mod" => Some(ManifestKind::Golang),
        "pom.xml" => Some(ManifestKind::Maven),
        _ => None,
    }
}

fn parse_manifest(
    kind: ManifestKind,
    path: &str,
    bytes: &[u8],
) -> Result<ForgePackageManifest, ForgeRejectReason> {
    let name_and_version = match kind {
        ManifestKind::Cargo => parse_cargo(bytes),
        ManifestKind::Npm => parse_npm(bytes),
        ManifestKind::Pypi => parse_pyproject(bytes),
        ManifestKind::Golang => parse_go(bytes),
        ManifestKind::Maven => parse_pom(bytes),
    }?;
    Ok(ForgePackageManifest {
        path: Arc::from(path),
        ecosystem: name_and_version.0,
        name: name_and_version.1,
        version: name_and_version.2,
        dependencies: name_and_version.3,
    })
}

type ManifestParts = (
    RegistryEcosystem,
    ForgeFact<ProductText>,
    ForgeFact<ProductText>,
    DependencyFacts<Box<[PackageDependencyRecord]>>,
);

fn product(value: Option<&str>) -> ForgeFact<ProductText> {
    value.and_then(|value| ProductText::new(value).ok()).map_or(
        ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
        ForgeFact::Recorded,
    )
}

fn parse_toml(value: &[u8]) -> Result<toml::Value, ForgeRejectReason> {
    toml::from_str(std::str::from_utf8(value).map_err(|_| ForgeRejectReason::Manifest)?)
        .map_err(|_| ForgeRejectReason::Manifest)
}

fn parse_cargo(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let root = parse_toml(bytes)?;
    let package = root.get("package").and_then(toml::Value::as_table);
    let name = package
        .and_then(|table| table.get("name"))
        .and_then(toml::Value::as_str);
    let version = package
        .and_then(|table| table.get("version"))
        .and_then(toml::Value::as_str);
    let source = name.zip(version).and_then(|(name, version)| {
        PackageReference::parse(format!("pkg:cargo/{name}@{version}")).ok()
    });
    let dependencies = parse_toml_dependencies(&root, source, RegistryEcosystem::Cargo);
    Ok((
        RegistryEcosystem::Cargo,
        product(name),
        product(version),
        dependencies,
    ))
}

fn parse_toml_dependencies(
    root: &toml::Value,
    source: Option<PackageReference>,
    ecosystem: RegistryEcosystem,
) -> DependencyFacts<Box<[PackageDependencyRecord]>> {
    let Some(source) = source else {
        return DependencyFacts::Unavailable(ProductText::from_static(
            "manifest has no immutable package version",
        ));
    };
    let mut rows = Vec::new();
    for (table_name, scope, optional_default) in [
        ("dependencies", DependencyScope::Runtime, false),
        ("dev-dependencies", DependencyScope::Development, true),
        ("build-dependencies", DependencyScope::Build, false),
    ] {
        let Some(table) = root.get(table_name).and_then(toml::Value::as_table) else {
            continue;
        };
        for (name, value) in table {
            let (requirement, optional) = match value {
                toml::Value::String(value) => (value.clone(), optional_default),
                toml::Value::Table(table) => (
                    table
                        .get("version")
                        .and_then(toml::Value::as_str)
                        .unwrap_or("*")
                        .to_owned(),
                    table
                        .get("optional")
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(optional_default),
                ),
                _ => continue,
            };
            if let Ok(target) = PackageDependencyTarget::new(ecosystem, name, requirement, None) {
                rows.push(PackageDependencyRecord::new(
                    source.clone(),
                    target,
                    scope,
                    optional,
                    DependencyEvidence {
                        authority: DependencyAuthority::ForgeManifest,
                        frontier: [0; 32],
                        provenance: *blake3::hash(root.to_string().as_bytes()).as_bytes(),
                    },
                ));
            }
        }
    }
    backend_library::admit_dependency_rows(rows).map_or_else(
        |_| {
            DependencyFacts::Unavailable(ProductText::from_static(
                "manifest dependency rows exceed bounds",
            ))
        },
        DependencyFacts::Known,
    )
}

fn parse_npm(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ForgeRejectReason::Manifest)?;
    let name = value.get("name").and_then(Value::as_str);
    let version = value.get("version").and_then(Value::as_str);
    let source = name.zip(version).and_then(|(name, version)| {
        PackageReference::parse(format!("pkg:npm/{name}@{version}")).ok()
    });
    let mut rows = Vec::new();
    for (field, scope, optional) in [
        ("dependencies", DependencyScope::Runtime, false),
        ("devDependencies", DependencyScope::Development, true),
        ("peerDependencies", DependencyScope::Peer, false),
        ("optionalDependencies", DependencyScope::Optional, true),
    ] {
        if let (Some(source), Some(table)) =
            (source.clone(), value.get(field).and_then(Value::as_object))
        {
            for (name, requirement) in table {
                if let Some(requirement) = requirement
                    .as_str()
                    .and_then(|value| ProductText::new(value).ok())
                {
                    if let Ok(target) = PackageDependencyTarget::new(
                        RegistryEcosystem::Npm,
                        name,
                        requirement.as_str(),
                        None,
                    ) {
                        rows.push(PackageDependencyRecord::new(
                            source.clone(),
                            target,
                            scope,
                            optional,
                            DependencyEvidence {
                                authority: DependencyAuthority::ForgeManifest,
                                frontier: [0; 32],
                                provenance: *blake3::hash(bytes).as_bytes(),
                            },
                        ));
                    }
                }
            }
        }
    }
    let facts = source.map_or_else(
        || {
            DependencyFacts::Unavailable(ProductText::from_static(
                "manifest has no immutable package version",
            ))
        },
        |_| {
            backend_library::admit_dependency_rows(rows).map_or_else(
                |_| {
                    DependencyFacts::Unavailable(ProductText::from_static(
                        "manifest dependency rows exceed bounds",
                    ))
                },
                DependencyFacts::Known,
            )
        },
    );
    Ok((
        RegistryEcosystem::Npm,
        product(name),
        product(version),
        facts,
    ))
}

fn parse_pyproject(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let root = parse_toml(bytes)?;
    let project = root.get("project").and_then(toml::Value::as_table);
    let name = project
        .and_then(|table| table.get("name"))
        .and_then(toml::Value::as_str);
    let version = project
        .and_then(|table| table.get("version"))
        .and_then(toml::Value::as_str);
    let source = name.zip(version).and_then(|(name, version)| {
        PackageReference::parse(format!("pkg:pypi/{name}@{version}")).ok()
    });
    let mut rows = Vec::new();
    if let (Some(source), Some(requirements)) = (
        source.clone(),
        project
            .and_then(|table| table.get("dependencies"))
            .and_then(toml::Value::as_array),
    ) {
        for requirement in requirements.iter().filter_map(toml::Value::as_str) {
            let name = requirement
                .split(['=', '<', '>', '!', '~', ';'])
                .next()
                .unwrap_or(requirement)
                .trim();
            if let Ok(target) =
                PackageDependencyTarget::new(RegistryEcosystem::Pypi, name, requirement, None)
            {
                rows.push(PackageDependencyRecord::new(
                    source.clone(),
                    target,
                    DependencyScope::Runtime,
                    false,
                    DependencyEvidence {
                        authority: DependencyAuthority::ForgeManifest,
                        frontier: [0; 32],
                        provenance: *blake3::hash(bytes).as_bytes(),
                    },
                ));
            }
        }
    }
    let facts = source.map_or_else(
        || {
            DependencyFacts::Unavailable(ProductText::from_static(
                "manifest has no immutable package version",
            ))
        },
        |_| {
            backend_library::admit_dependency_rows(rows).map_or_else(
                |_| {
                    DependencyFacts::Unavailable(ProductText::from_static(
                        "manifest dependency rows exceed bounds",
                    ))
                },
                DependencyFacts::Known,
            )
        },
    );
    Ok((
        RegistryEcosystem::Pypi,
        product(name),
        product(version),
        facts,
    ))
}

fn parse_go(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let text = std::str::from_utf8(bytes).map_err(|_| ForgeRejectReason::Manifest)?;
    let module = text
        .lines()
        .find_map(|line| line.strip_prefix("module ").map(str::trim));
    let name = module;
    let facts = DependencyFacts::Unknown(ProductText::from_static(
        "go.mod dependencies retain module requirements after resolver selection",
    ));
    Ok((
        RegistryEcosystem::Golang,
        product(name),
        ForgeFact::Unavailable(ForgeUnavailableReason::AuthorityOmitted),
        facts,
    ))
}

fn parse_pom(bytes: &[u8]) -> Result<ManifestParts, ForgeRejectReason> {
    let text = std::str::from_utf8(bytes).map_err(|_| ForgeRejectReason::Manifest)?;
    let value = |tag: &str| {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        text.split_once(&open)
            .and_then(|(_, rest)| rest.split_once(&close).map(|(value, _)| value.trim()))
    };
    let group = value("groupId");
    let artifact = value("artifactId");
    let name = group
        .zip(artifact)
        .map(|(group, artifact)| format!("{group}:{artifact}"));
    let version = value("version");
    let source = name.as_deref().zip(version).and_then(|(name, version)| {
        PackageReference::parse(format!("pkg:maven/{name}@{version}")).ok()
    });
    let facts = source.map_or_else(
        || {
            DependencyFacts::Unavailable(ProductText::from_static(
                "manifest has no immutable package version",
            ))
        },
        |_| {
            DependencyFacts::Unknown(ProductText::from_static(
                "pom dependency scope is available after XML authority parsing",
            ))
        },
    );
    Ok((
        RegistryEcosystem::Maven,
        product(name.as_deref()),
        product(version),
        facts,
    ))
}

/// Safe process-backed transport for a generic HTTPS Git source.
pub struct GitCommandTransport {
    root: PathBuf,
}

struct GitArchiveReader {
    child: Child,
    stdout: ChildStdout,
    finished: bool,
}

impl Read for GitArchiveReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.stdout.read(bytes)?;
        if read == 0 && !self.finished {
            self.finished = true;
            let status = self.child.wait()?;
            if !status.success() {
                return Err(io::Error::other("git archive failed"));
            }
        }
        Ok(read)
    }
}

impl Drop for GitArchiveReader {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl GitCommandTransport {
    /// Creates a transport whose temporary repositories live below `root`.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ForgeTransportError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|_| ForgeTransportError::Unavailable)?;
        Ok(Self { root })
    }
}

impl ForgeTransport for GitCommandTransport {
    fn resolve(
        &mut self,
        coordinate: &ForgeCoordinate,
    ) -> Result<ForgeResolution, ForgeTransportError> {
        let repo = self.root.join("repo");
        if repo.exists() {
            let _ = fs::remove_dir_all(&repo);
        }
        let repo_url = coordinate.repository_url();
        let repo_url_arg = repo_url.to_owned();
        let target = repo.to_string_lossy().to_string();
        let status = Command::new("git")
            .args(["init", "--quiet", &target])
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .status()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if !status.success() {
            return Err(ForgeTransportError::Protocol);
        }
        let revision_token = coordinate_revision_token(coordinate.revision());
        let status = Command::new("git")
            .args([
                "-C",
                &target,
                "fetch",
                "--quiet",
                "--no-tags",
                &repo_url_arg,
                &revision_token,
            ])
            .stdin(Stdio::null())
            .status()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if !status.success() {
            return Err(ForgeTransportError::NotFound);
        }
        let commit = self.git_output(&target, &["rev-parse", "FETCH_HEAD"])?;
        let tree = self.git_output(&target, &["show", "-s", "--format=%T", "FETCH_HEAD"])?;
        ForgeResolution::for_coordinate(
            coordinate,
            ForgeObjectId::parse(&commit).map_err(|_| ForgeTransportError::Protocol)?,
            Some(ForgeObjectId::parse(&tree).map_err(|_| ForgeTransportError::Protocol)?),
            commit,
        )
        .map_err(|_| ForgeTransportError::Integrity)
    }

    fn fetch_archive(
        &mut self,
        _coordinate: &ForgeCoordinate,
        resolution: &ForgeResolution,
    ) -> Result<ForgeArchive, ForgeTransportError> {
        let target = self.root.join("repo").to_string_lossy().to_string();
        let commit = resolution.commit.as_hex();
        let mut child = Command::new("git")
            .args(["-C", &target, "archive", "--format=tar", &commit])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        let stdout = child.stdout.take().ok_or(ForgeTransportError::Protocol)?;
        Ok(ForgeArchive::from_reader(
            GitArchiveReader {
                child,
                stdout,
                finished: false,
            },
            ForgeArchiveFormat::Tar,
            None::<String>,
        ))
    }
}

impl GitCommandTransport {
    fn git_output(&self, target: &str, args: &[&str]) -> Result<String, ForgeTransportError> {
        let output = Command::new("git")
            .args(["-C", target])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if !output.status.success() {
            return Err(ForgeTransportError::Protocol);
        }
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_owned())
            .map_err(|_| ForgeTransportError::Protocol)
    }
}

fn coordinate_revision_token(revision: &ForgeRevision) -> String {
    match revision {
        ForgeRevision::Commit(commit) => commit.as_hex(),
        ForgeRevision::Tag(tag) => tag.as_str().to_owned(),
        ForgeRevision::Branch(branch) => branch.as_str().to_owned(),
    }
}

/// Bounded HTTP forge transport for the public provider APIs.
pub struct HttpForgeTransport {
    limits: ForgeAcquisitionLimits,
    agent: ureq::Agent,
    token: Option<ForgeAuthToken>,
}

/// Process-local forge token; it never participates in coordinate or journal identity.
#[derive(Clone, Eq, PartialEq)]
pub struct ForgeAuthToken(Arc<str>);

impl ForgeAuthToken {
    /// Admits a token without control bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ForgeTransportError> {
        let value = value.into();
        if value.is_empty() || value.contains(['\r', '\n']) {
            return Err(ForgeTransportError::Policy);
        }
        Ok(Self(Arc::from(value)))
    }
}

impl fmt::Debug for ForgeAuthToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ForgeAuthToken([REDACTED])")
    }
}

impl HttpForgeTransport {
    /// Creates a bounded provider transport. Credentials are process-local.
    pub fn new(
        limits: ForgeAcquisitionLimits,
        token: Option<ForgeAuthToken>,
    ) -> Result<Self, ForgeTransportError> {
        let limits = limits.validate().map_err(|_| ForgeTransportError::Bounds)?;
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        Ok(Self {
            limits,
            agent: config.new_agent(),
            token,
        })
    }

    fn get(&self, url: &str, maximum: usize) -> Result<Vec<u8>, ForgeTransportError> {
        let mut request = self.agent.get(url).header("accept-encoding", "identity");
        if let Some(token) = &self.token {
            request = request.header("authorization", &format!("Bearer {}", token.0));
        }
        let mut response = request
            .call()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        let status = response.status().as_u16();
        if status == 404 {
            return Err(ForgeTransportError::NotFound);
        }
        if status == 429 {
            return Err(ForgeTransportError::RetryAfter(1_000));
        }
        if !(200..300).contains(&status) {
            return Err(ForgeTransportError::Protocol);
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| ForgeTransportError::Protocol)?;
        if bytes.len() > maximum {
            return Err(ForgeTransportError::Bounds);
        }
        Ok(bytes)
    }

    fn get_reader(&self, url: &str) -> Result<Box<dyn Read>, ForgeTransportError> {
        let mut request = self.agent.get(url).header("accept-encoding", "identity");
        if let Some(token) = &self.token {
            request = request.header("authorization", &format!("Bearer {}", token.0));
        }
        let response = request
            .call()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        let status = response.status().as_u16();
        if status == 404 {
            return Err(ForgeTransportError::NotFound);
        }
        if status == 429 {
            return Err(ForgeTransportError::RetryAfter(1_000));
        }
        if !(200..300).contains(&status) {
            return Err(ForgeTransportError::Protocol);
        }
        Ok(Box::new(response.into_body().into_reader()))
    }
}

impl ForgeTransport for HttpForgeTransport {
    fn resolve(
        &mut self,
        coordinate: &ForgeCoordinate,
    ) -> Result<ForgeResolution, ForgeTransportError> {
        let (url, tree_json) = match coordinate.provider() {
            ForgeProvider::Github => (
                format!(
                    "https://api.github.com/repos/{}/{}/commits/{}",
                    coordinate.owner(),
                    coordinate.repository(),
                    revision_path(coordinate.revision())
                ),
                format!(
                    "https://api.github.com/repos/{}/{}/git/trees/{}",
                    coordinate.owner(),
                    coordinate.repository(),
                    revision_path(coordinate.revision())
                ),
            ),
            ForgeProvider::Gitlab => (
                format!(
                    "https://gitlab.com/api/v4/projects/{}/repository/commits/{}",
                    gitlab_project_path(coordinate),
                    revision_path(coordinate.revision())
                ),
                String::new(),
            ),
            ForgeProvider::Codeberg => (
                format!(
                    "{}/api/v1/repos/{}/{}/commits/{}",
                    host_root(coordinate.repository_url()),
                    coordinate.owner(),
                    coordinate.repository(),
                    revision_path(coordinate.revision())
                ),
                format!(
                    "{}/api/v1/repos/{}/{}/git/commits/{}",
                    host_root(coordinate.repository_url()),
                    coordinate.owner(),
                    coordinate.repository(),
                    revision_path(coordinate.revision())
                ),
            ),
            ForgeProvider::GenericHttpsGit => return Err(ForgeTransportError::Policy),
        };
        let bytes = self.get(&url, self.limits.max_metadata_bytes)?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| ForgeTransportError::Protocol)?;
        let commit = value
            .get("sha")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .ok_or(ForgeTransportError::Protocol)?;
        let tree = value
            .get("commit")
            .and_then(|commit| commit.get("tree"))
            .and_then(|tree| tree.get("sha"))
            .and_then(Value::as_str)
            .or_else(|| value.get("tree_id").and_then(Value::as_str));
        let tree = tree
            .map(ForgeObjectId::parse)
            .transpose()
            .map_err(|_| ForgeTransportError::Protocol)?;
        let _ = tree_json;
        ForgeResolution::for_coordinate(
            coordinate,
            ForgeObjectId::parse(commit).map_err(|_| ForgeTransportError::Protocol)?,
            tree,
            commit,
        )
        .map_err(|_| ForgeTransportError::Integrity)
    }

    fn fetch_archive(
        &mut self,
        coordinate: &ForgeCoordinate,
        resolution: &ForgeResolution,
    ) -> Result<ForgeArchive, ForgeTransportError> {
        let commit = resolution.commit.as_hex();
        let (url, root_prefix) = match coordinate.provider() {
            ForgeProvider::Github => (
                format!("{}/archive/{}.tar.gz", coordinate.repository_url(), commit),
                Some(format!("{}-{}", coordinate.repository(), commit)),
            ),
            ForgeProvider::Gitlab => (
                format!(
                    "{}/repository/archive.tar.gz?sha={}",
                    coordinate.repository_url(),
                    commit
                ),
                Some(format!("{}-{}", coordinate.repository(), commit)),
            ),
            ForgeProvider::Codeberg => (
                format!("{}/archive/{}.tar.gz", coordinate.repository_url(), commit),
                Some(format!("{}-{}", coordinate.repository(), commit)),
            ),
            ForgeProvider::GenericHttpsGit => return Err(ForgeTransportError::Policy),
        };
        Ok(ForgeArchive::from_reader(
            self.get_reader(&url)?,
            ForgeArchiveFormat::TarGzip,
            root_prefix,
        ))
    }

    fn fetch_metadata(
        &mut self,
        coordinate: &ForgeCoordinate,
        _resolution: &ForgeResolution,
    ) -> Result<ForgeRepositoryMetadata, ForgeTransportError> {
        let url = match coordinate.provider() {
            ForgeProvider::Github => format!(
                "https://api.github.com/repos/{}/{}",
                coordinate.owner(),
                coordinate.repository()
            ),
            ForgeProvider::Gitlab => format!(
                "https://gitlab.com/api/v4/projects/{}",
                gitlab_project_path(coordinate)
            ),
            ForgeProvider::Codeberg => format!(
                "{}/api/v1/repos/{}/{}",
                host_root(coordinate.repository_url()),
                coordinate.owner(),
                coordinate.repository()
            ),
            ForgeProvider::GenericHttpsGit => {
                return Ok(ForgeRepositoryMetadata::unavailable(
                    coordinate.owner(),
                    ForgeUnavailableReason::Unsupported,
                ));
            }
        };
        let bytes = self.get(&url, self.limits.max_metadata_bytes)?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| ForgeTransportError::Protocol)?;
        let mut metadata = ForgeRepositoryMetadata::from_json(coordinate.owner(), &value);
        metadata.readme = ForgeFact::Unavailable(ForgeUnavailableReason::Unsupported);
        Ok(metadata)
    }
}

fn revision_path(revision: &ForgeRevision) -> String {
    match revision {
        ForgeRevision::Commit(commit) => commit.as_hex(),
        ForgeRevision::Tag(tag) => tag.as_str().to_owned(),
        ForgeRevision::Branch(branch) => branch.as_str().to_owned(),
    }
}

fn gitlab_project_path(coordinate: &ForgeCoordinate) -> String {
    format!(
        "{}%2F{}",
        coordinate.owner().replace('/', "%2F"),
        coordinate.repository()
    )
}

fn host_root(url: &str) -> &str {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url;
    };
    rest.find('/')
        .map_or(url, |slash| &url[..scheme.len() + 3 + slash])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Write,
        net::{TcpListener, TcpStream},
        path::Path,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
    };

    fn sha(value: &str) -> ForgeObjectId {
        ForgeObjectId::parse(value).expect("hash")
    }

    fn coordinate() -> ForgeCoordinate {
        ForgeCoordinate::new(
            "https://github.com/acme/mono.git",
            ForgeRevision::Tag(ForgeRefName::new("v1.2.3").expect("tag")),
            Some("crates/widget"),
        )
        .expect("coordinate")
    }

    struct Fixture {
        calls: AtomicUsize,
        archive: Vec<u8>,
    }

    impl ForgeTransport for Fixture {
        fn resolve(
            &mut self,
            coordinate: &ForgeCoordinate,
        ) -> Result<ForgeResolution, ForgeTransportError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            ForgeResolution::for_coordinate(
                coordinate,
                sha("0123456789012345678901234567890123456789"),
                Some(sha("abcdefabcdefabcdefabcdefabcdefabcdefabcd")),
                "0123456789012345678901234567890123456789",
            )
            .map_err(|_| ForgeTransportError::Integrity)
        }
        fn fetch_archive(
            &mut self,
            _: &ForgeCoordinate,
            _: &ForgeResolution,
        ) -> Result<ForgeArchive, ForgeTransportError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(ForgeArchive::tar(self.archive.clone(), None::<String>))
        }
        fn fetch_metadata(
            &mut self,
            coordinate: &ForgeCoordinate,
            _: &ForgeResolution,
        ) -> Result<ForgeRepositoryMetadata, ForgeTransportError> {
            Ok(ForgeRepositoryMetadata::unavailable(
                coordinate.owner(),
                ForgeUnavailableReason::AuthorityOmitted,
            ))
        }
    }

    fn tar_one(path: &str, bytes: &[u8]) -> Vec<u8> {
        let mut header = [0_u8; 512];
        header[..path.len()].copy_from_slice(path.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[124..136].copy_from_slice(format!("{:011o}\0", bytes.len()).as_bytes());
        header[156] = b'0';
        header[148..156].fill(b' ');
        let checksum: u32 = header.iter().map(|byte| u32::from(*byte)).sum();
        header[148..156].copy_from_slice(format!("{:06o}\0 ", checksum).as_bytes());
        let mut result = header.to_vec();
        result.extend_from_slice(bytes);
        result.resize(result.len() + ((512 - (bytes.len() % 512)) % 512) + 1024, 0);
        result
    }

    fn serve_loopback_request(mut stream: TcpStream, root: &Path) {
        let mut request = [0_u8; 4096];
        let Ok(bytes) = stream.read(&mut request) else {
            return;
        };
        if bytes == 0 {
            return;
        }
        let request = String::from_utf8_lossy(&request[..bytes]);
        let Some(path) = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
        else {
            return;
        };
        let path = path.split('?').next().unwrap_or_default();
        let path = path.trim_start_matches('/');
        if path.is_empty() || path.split('/').any(|part| part == ".." || part.is_empty()) {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        }
        let file = root.join(path);
        let Ok(body) = fs::read(file) else {
            let _ = stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        };
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
    }

    fn start_loopback_http(
        root: &Path,
    ) -> (
        std::net::SocketAddr,
        Arc<AtomicBool>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("listener");
        listener.set_nonblocking(true).expect("nonblocking");
        let address = listener.local_addr().expect("address");
        let stopped = Arc::new(AtomicBool::new(false));
        let stopped_for_thread = Arc::clone(&stopped);
        let root = Arc::new(root.to_path_buf());
        let thread = thread::spawn(move || {
            while !stopped_for_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let root = Arc::clone(&root);
                        thread::spawn(move || serve_loopback_request(stream, &root));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::yield_now();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        thread::yield_now();
                    }
                    Err(_) => thread::yield_now(),
                }
            }
        });
        (address, stopped, thread)
    }

    #[test]
    fn coordinate_is_canonical_and_credential_free() {
        let coordinate =
            ForgeCoordinate::parse("https://GitHub.com/acme/mono.git@tag:v1#crates/widget")
                .expect("coordinate");
        assert_eq!(coordinate.repository_url(), "https://github.com/acme/mono");
        assert_eq!(coordinate.provider(), ForgeProvider::Github);
        assert_eq!(coordinate.subdir(), Some("crates/widget"));
        assert!(!coordinate.canonical().contains("token"));
        assert!(ForgeCoordinate::parse("https://u:p@github.com/acme/mono@tag:v1").is_err());
        assert!(ForgeCoordinate::parse("https://github.com/acme/mono@v1").is_err());
        assert!(ForgeCoordinate::parse("https://github.com/acme/mono@branch:main?x=1").is_err());
    }

    #[test]
    fn every_supported_provider_has_one_canonical_coordinate_shape() {
        for (url, provider) in [
            ("https://github.com/acme/mono.git", ForgeProvider::Github),
            ("https://gitlab.com/acme/mono.git", ForgeProvider::Gitlab),
            (
                "https://codeberg.org/acme/mono.git",
                ForgeProvider::Codeberg,
            ),
            (
                "https://git.example.test/acme/mono.git",
                ForgeProvider::GenericHttpsGit,
            ),
        ] {
            let coordinate = ForgeCoordinate::new(
                url,
                ForgeRevision::Branch(ForgeRefName::new("main").expect("ref")),
                None::<String>,
            )
            .expect("coordinate");
            assert_eq!(coordinate.provider(), provider);
            assert_eq!(coordinate.repository_url(), url.trim_end_matches(".git"));
            assert!(coordinate.canonical().contains("@branch:main"));
        }
    }

    #[test]
    fn immutable_commit_requests_cannot_be_rebound() {
        let coordinate = ForgeCoordinate::new(
            "https://github.com/acme/mono",
            ForgeRevision::Commit(sha("0123456789012345678901234567890123456789")),
            None::<String>,
        )
        .expect("coordinate");
        let error = ForgeResolution::new(
            coordinate.revision().clone(),
            sha("abcdefabcdefabcdefabcdefabcdefabcdefabcd"),
            None,
        )
        .expect_err("mismatch");
        assert_eq!(error, ForgeProtocolError::RevisionMismatch);
    }

    #[test]
    fn mutable_resolution_requires_authority_validator_binding() {
        struct Unbound;
        impl ForgeTransport for Unbound {
            fn resolve(
                &mut self,
                coordinate: &ForgeCoordinate,
            ) -> Result<ForgeResolution, ForgeTransportError> {
                ForgeResolution::new(
                    coordinate.revision().clone(),
                    sha("0123456789012345678901234567890123456789"),
                    None,
                )
                .map_err(|_| ForgeTransportError::Protocol)
            }

            fn fetch_archive(
                &mut self,
                _coordinate: &ForgeCoordinate,
                _: &ForgeResolution,
            ) -> Result<ForgeArchive, ForgeTransportError> {
                Ok(ForgeArchive::tar(Vec::new(), None::<String>))
            }
        }

        let root = std::env::temp_dir().join(format!("nudox-forge-binding-{}", now_millis()));
        let _ = fs::remove_dir_all(&root);
        let service = ForgeAcquisitionService::open(
            &root,
            ForgeAcquisitionPolicy::Online,
            ForgeAcquisitionLimits::default(),
        )
        .expect("service");
        assert_eq!(
            service.acquire(&coordinate(), &mut Unbound),
            ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::RevisionMismatch)
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn fixture_acquisition_reuses_content_and_restarts_offline() {
        let root = std::env::temp_dir().join(format!("nudox-forge-test-{}", now_millis()));
        let _ = fs::remove_dir_all(&root);
        let archive = tar_one(
            "crates/widget/Cargo.toml",
            b"[package]\nname=\"widget\"\nversion=\"1.0.0\"\n",
        );
        let mut fixture = Fixture {
            calls: AtomicUsize::new(0),
            archive,
        };
        let service = ForgeAcquisitionService::open(
            &root,
            ForgeAcquisitionPolicy::Online,
            ForgeAcquisitionLimits::default(),
        )
        .expect("service");
        let first = service.acquire(&coordinate(), &mut fixture);
        let ForgeAcquisitionOutcome::Hit(first) = first else {
            panic!("first acquisition")
        };
        assert_eq!(first.manifests.len(), 1);
        assert_eq!(first.manifests[0].path.as_ref(), "Cargo.toml");
        let product = first.product_record().expect("product projection");
        assert_eq!(product.manifests[0].path.as_str(), "Cargo.toml");
        assert_eq!(first.snapshot.id().to_bytes(), first.receipt.snapshot);
        assert_eq!(first.delta.id().to_bytes(), first.receipt.delta);
        let calls = fixture.calls.load(Ordering::Relaxed);
        let second = service.acquire(&coordinate(), &mut fixture);
        assert!(matches!(second, ForgeAcquisitionOutcome::Hit(_)));
        assert_eq!(fixture.calls.load(Ordering::Relaxed), calls);
        drop(service);
        let offline = ForgeAcquisitionService::open(
            &root,
            ForgeAcquisitionPolicy::Offline,
            ForgeAcquisitionLimits::default(),
        )
        .expect("reopen");
        let recovered = offline
            .reference(&coordinate())
            .expect("reference")
            .expect("recovered source");
        assert_eq!(recovered.snapshot.id(), first.snapshot.id());
        assert_eq!(recovered.delta.id(), first.delta.id());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn not_found_tombstone_survives_restart_without_becoming_identity() {
        struct Missing;
        impl ForgeTransport for Missing {
            fn resolve(
                &mut self,
                _: &ForgeCoordinate,
            ) -> Result<ForgeResolution, ForgeTransportError> {
                Err(ForgeTransportError::NotFound)
            }

            fn fetch_archive(
                &mut self,
                _: &ForgeCoordinate,
                _: &ForgeResolution,
            ) -> Result<ForgeArchive, ForgeTransportError> {
                Err(ForgeTransportError::NotFound)
            }
        }

        let root = std::env::temp_dir().join(format!("nudox-forge-tombstone-{}", now_millis()));
        let _ = fs::remove_dir_all(&root);
        let coordinate = coordinate();
        let mut missing = Missing;
        let service = ForgeAcquisitionService::open(
            &root,
            ForgeAcquisitionPolicy::Online,
            ForgeAcquisitionLimits::default(),
        )
        .expect("service");
        assert_eq!(
            service.acquire(&coordinate, &mut missing),
            ForgeAcquisitionOutcome::Rejected(ForgeRejectReason::RevisionMismatch)
        );
        assert!(
            fs::metadata(root.join("forge.journal"))
                .expect("journal")
                .len()
                > 0
        );
        drop(service);
        let offline = ForgeAcquisitionService::open(
            &root,
            ForgeAcquisitionPolicy::Offline,
            ForgeAcquisitionLimits::default(),
        )
        .expect("restart");
        assert!(offline.reference(&coordinate).expect("reference").is_none());
        assert_eq!(
            offline.acquire(&coordinate, &mut missing),
            ForgeAcquisitionOutcome::Offline
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn process_transport_streams_exact_commit_from_loopback_http() {
        let root = std::env::temp_dir().join(format!(
            "nudox-forge-process-{}-{}",
            std::process::id(),
            now_millis()
        ));
        let _ = fs::remove_dir_all(&root);
        let work = root.join("work");
        let bare = root.join("server/acme/mono");
        fs::create_dir_all(&work).expect("work directory");
        let git = |directory: &Path, args: &[&str]| {
            let output = Command::new("git")
                .current_dir(directory)
                .args(args)
                .output()
                .expect("git process");
            assert!(
                output.status.success(),
                "git failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output.stdout
        };
        git(&work, &["init", "--quiet"]);
        git(&work, &["config", "user.email", "forge@example.test"]);
        git(&work, &["config", "user.name", "Forge Fixture"]);
        fs::write(
            work.join("Cargo.toml"),
            b"[package]\nname=\"loopback\"\nversion=\"1.0.0\"\n",
        )
        .expect("manifest");
        git(&work, &["add", "Cargo.toml"]);
        git(&work, &["commit", "--quiet", "-m", "fixture"]);
        git(&work, &["branch", "-M", "main"]);
        fs::create_dir_all(bare.parent().expect("bare parent")).expect("server directory");
        let clone = Command::new("git")
            .args([
                "clone",
                "--bare",
                work.to_str().expect("work path"),
                bare.to_str().expect("bare path"),
            ])
            .output()
            .expect("bare clone");
        assert!(clone.status.success(), "bare clone failed");
        git(&bare, &["update-server-info"]);
        let commit = String::from_utf8(git(&work, &["rev-parse", "HEAD"]))
            .expect("commit text")
            .trim()
            .to_owned();
        let (address, stopped, server) = start_loopback_http(&root.join("server"));
        let coordinate = ForgeCoordinate::new(
            format!("http://{address}/acme/mono.git"),
            ForgeRevision::Branch(ForgeRefName::new("main").expect("branch")),
            None::<String>,
        )
        .expect("coordinate");
        let service = ForgeAcquisitionService::open(
            root.join("service"),
            ForgeAcquisitionPolicy::Online,
            ForgeAcquisitionLimits::default(),
        )
        .expect("service");
        let mut transport = GitCommandTransport::new(root.join("transport")).expect("transport");
        let outcome = service.acquire(&coordinate, &mut transport);
        let ForgeAcquisitionOutcome::Hit(result) = outcome else {
            panic!("loopback process acquisition failed: {outcome:?}");
        };
        assert_eq!(result.resolution.commit.as_hex(), commit);
        assert!(
            result
                .tree
                .entries()
                .iter()
                .any(|entry| entry.path.as_ref() == "Cargo.toml")
        );
        stopped.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(address);
        server.join().expect("server");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn delegated_exact_commit_is_verified_before_archive_admission() {
        let coordinate = ForgeCoordinate::new(
            "https://github.com/acme/mono",
            ForgeRevision::Commit(sha("0123456789012345678901234567890123456789")),
            None::<String>,
        )
        .expect("coordinate");
        let request = ForgeDelegationRequest::new(coordinate).expect("request");
        let response = ForgeDelegatedObject {
            commit: sha("abcdefabcdefabcdefabcdefabcdefabcdefabcd"),
            archive: ForgeArchive::tar(vec![0; 1024], None::<String>),
        };
        assert!(matches!(
            verify_delegated_object(&request, response),
            Err(ForgeProtocolError::DelegatedCommitMismatch)
        ));
    }

    #[test]
    #[ignore = "real forge network smoke; set NUDOX_FORGE_REAL_SMOKE=1 and run --ignored"]
    fn gated_real_github_smoke_uses_exact_resolved_commit() {
        if std::env::var("NUDOX_FORGE_REAL_SMOKE").as_deref() != Ok("1") {
            return;
        }
        let coordinate = ForgeCoordinate::new(
            "https://github.com/octocat/Hello-World",
            ForgeRevision::Branch(ForgeRefName::new("master").expect("ref")),
            None::<String>,
        )
        .expect("coordinate");
        let root = std::env::temp_dir().join(format!("nudox-forge-smoke-{}", now_millis()));
        let service = ForgeAcquisitionService::open(
            &root,
            ForgeAcquisitionPolicy::Online,
            ForgeAcquisitionLimits::default(),
        )
        .expect("service");
        let mut transport =
            HttpForgeTransport::new(ForgeAcquisitionLimits::default(), None).expect("transport");
        let result = service.acquire(&coordinate, &mut transport);
        assert!(matches!(result, ForgeAcquisitionOutcome::Hit(_)));
        let _ = fs::remove_dir_all(root);
    }
}

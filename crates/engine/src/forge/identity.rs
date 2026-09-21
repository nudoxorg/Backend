use super::*;

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

    pub(super) fn canonical_bytes(&self) -> Vec<u8> {
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

    pub(super) fn identity_is_valid(&self) -> bool {
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

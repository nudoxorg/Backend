//! Explicit, owner-managed trust for remote compiler peers.
//!
//! A peer identity is admitted only with one exact compiler execution scope.
//! Endpoint addresses are routing hints; authorization always uses the
//! authenticated endpoint identity and the complete typed grant.

use backend_engine::cluster_transport::EndpointId;
use backend_semantic::vocabulary::{LanguageProfile, Stage};
use std::fs::{self, File};
use std::io::Read;
use std::net::SocketAddr;
use std::path::Path;

const POLICY_MAGIC: &[u8; 8] = b"BKCTPW01";
/// Canonical owner trust policy filename under the locald durable data root.
pub const TRUSTED_COMPILER_POLICY_FILE_NAME: &str = "compiler-worker-trust.v1";
const MAX_POLICY_BYTES: usize = 512 * 1024;
const MAX_GRANTS: usize = 4096;
const MAX_ADDRESS_BYTES: usize = 128;
const MAX_SCOPE_MISMATCH_DIAGNOSTIC_GRANTS: usize = 64;
const SCOPE_MISMATCH_FIELD_COUNT: usize = 7;
const SCOPE_MISMATCH_BIN_COUNT: usize = 1 << SCOPE_MISMATCH_FIELD_COUNT;

/// Exact compiler scope projected from one captured invocation or persisted grant.
///
/// This value is only used for strict equality checks and bounded diagnostics. It carries the
/// same seven fields that the trust policy has always compared; it is not an authorization
/// claim by itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompilerTrustScope {
    namespace_id: [u8; 16],
    recipe: [u8; 32],
    profile: LanguageProfile,
    stage: Stage,
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
}

impl CompilerTrustScope {
    pub(crate) const fn new(
        namespace_id: [u8; 16],
        recipe: [u8; 32],
        profile: LanguageProfile,
        stage: Stage,
        toolchain: [u8; 32],
        environment: [u8; 32],
        target_platform: [u8; 32],
    ) -> Self {
        Self {
            namespace_id,
            recipe,
            profile,
            stage,
            toolchain,
            environment,
            target_platform,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompilerScopeField {
    Namespace,
    Recipe,
    Profile,
    Stage,
    Toolchain,
    Environment,
    TargetPlatform,
}

impl CompilerScopeField {
    const ALL: [Self; SCOPE_MISMATCH_FIELD_COUNT] = [
        Self::Namespace,
        Self::Recipe,
        Self::Profile,
        Self::Stage,
        Self::Toolchain,
        Self::Environment,
        Self::TargetPlatform,
    ];

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::Recipe => "recipe",
            Self::Profile => "profile",
            Self::Stage => "stage",
            Self::Toolchain => "toolchain",
            Self::Environment => "environment",
            Self::TargetPlatform => "target_platform",
        }
    }
}

/// Closed set of persisted-grant scope fields that differ from one captured invocation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct CompilerScopeMismatchFields(u8);

impl CompilerScopeMismatchFields {
    fn insert(&mut self, field: CompilerScopeField) {
        self.0 |= field.bit();
    }

    const fn bin(self) -> usize {
        self.0 as usize
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }

    fn write_labels(self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return formatter.write_str("exact");
        }
        let mut first = true;
        for field in CompilerScopeField::ALL {
            if self.0 & field.bit() == 0 {
                continue;
            }
            if !first {
                formatter.write_str("+")?;
            }
            formatter.write_str(field.label())?;
            first = false;
        }
        Ok(())
    }
}

/// Fixed-size histogram of scope mismatch field sets across a bounded policy prefix.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompilerScopeMismatchHistogram {
    grants_examined: u16,
    truncated: bool,
    bins: [u16; SCOPE_MISMATCH_BIN_COUNT],
}

impl CompilerScopeMismatchHistogram {
    fn from_grants(grants: &[TrustedCompilerWorkerGrant], scope: &CompilerTrustScope) -> Self {
        let mut histogram = Self {
            grants_examined: 0,
            truncated: false,
            bins: [0; SCOPE_MISMATCH_BIN_COUNT],
        };
        for grant in grants.iter().take(MAX_SCOPE_MISMATCH_DIAGNOSTIC_GRANTS) {
            histogram.grants_examined = histogram.grants_examined.saturating_add(1);
            let bin = grant.scope_mismatch_fields(scope).bin();
            histogram.bins[bin] = histogram.bins[bin].saturating_add(1);
        }
        histogram.truncated = grants.len() > usize::from(histogram.grants_examined);
        histogram
    }
}

impl std::fmt::Display for CompilerScopeMismatchHistogram {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "scope_grants_examined={} scope_grants_truncated={} scope_mismatch_bins=[",
            self.grants_examined,
            u8::from(self.truncated),
        )?;
        let mut first = true;
        for (mask, count) in (0_u8..).zip(self.bins.iter().copied()) {
            if count == 0 {
                continue;
            }
            if !first {
                formatter.write_str(",")?;
            }
            CompilerScopeMismatchFields(mask).write_labels(formatter)?;
            write!(formatter, ":{count}")?;
            first = false;
        }
        formatter.write_str("]")
    }
}

/// One owner-approved remote compiler, pinned to an authenticated peer and an
/// exact execution scope. Package lineage and target are checked against each
/// assignment separately; this grant binds the reusable namespace and recipe
/// facts required by the worker trust protocol.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrustedCompilerWorkerGrant {
    peer: EndpointId,
    address: SocketAddr,
    namespace_id: [u8; 16],
    recipe: [u8; 32],
    profile: LanguageProfile,
    stage: Stage,
    toolchain: [u8; 32],
    environment: [u8; 32],
    target_platform: [u8; 32],
}

impl TrustedCompilerWorkerGrant {
    /// Admits one exact peer and compiler execution scope.
    pub fn new(
        peer: EndpointId,
        address: SocketAddr,
        namespace_id: [u8; 16],
        recipe: [u8; 32],
        profile: LanguageProfile,
        stage: Stage,
        toolchain: [u8; 32],
        environment: [u8; 32],
        target_platform: [u8; 32],
    ) -> Result<Self, CompilerTrustError> {
        let address_valid =
            address.port() != 0 && !address.ip().is_unspecified() && !address.ip().is_multicast();
        if peer.as_bytes() == &[0; 32]
            || !address_valid
            || namespace_id == [0; 16]
            || recipe == [0; 32]
            || toolchain == [0; 32]
            || environment == [0; 32]
            || target_platform == [0; 32]
        {
            return Err(CompilerTrustError::InvalidGrant);
        }
        Ok(Self {
            peer,
            address,
            namespace_id,
            recipe,
            profile,
            stage,
            toolchain,
            environment,
            target_platform,
        })
    }

    /// Authenticated endpoint identity expected from the worker.
    #[must_use]
    pub const fn peer(&self) -> EndpointId {
        self.peer
    }

    /// Direct socket address supplied out of band for connection setup.
    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    /// Exact Turso semantic namespace allowed by this grant.
    #[must_use]
    pub const fn namespace_id(&self) -> [u8; 16] {
        self.namespace_id
    }

    /// Exact compiler recipe identity allowed by this grant.
    #[must_use]
    pub const fn recipe(&self) -> [u8; 32] {
        self.recipe
    }

    /// Exact closed profile discriminator allowed by this grant.
    #[must_use]
    pub const fn profile(&self) -> LanguageProfile {
        self.profile
    }

    /// Exact compiler stage allowed by this grant.
    #[must_use]
    pub const fn stage(&self) -> Stage {
        self.stage
    }

    /// Exact executable/toolchain identity allowed by this grant.
    #[must_use]
    pub const fn toolchain(&self) -> [u8; 32] {
        self.toolchain
    }

    /// Exact compiler environment identity allowed by this grant.
    #[must_use]
    pub const fn environment(&self) -> [u8; 32] {
        self.environment
    }

    /// Exact target platform/sysroot identity allowed by this grant.
    #[must_use]
    pub const fn target_platform(&self) -> [u8; 32] {
        self.target_platform
    }

    fn canonical_key(&self) -> Vec<u8> {
        let address = self.address.to_string();
        let mut key = Vec::with_capacity(32 + address.len() + 16 + 32 * 4 + 3);
        key.extend_from_slice(self.peer.as_bytes());
        key.extend_from_slice(&(address.len() as u16).to_be_bytes());
        key.extend_from_slice(address.as_bytes());
        key.extend_from_slice(&self.namespace_id);
        key.extend_from_slice(&self.recipe);
        key.extend_from_slice(&<[u8; 2]>::from(self.profile));
        key.push(u8::from(self.stage));
        key.extend_from_slice(&self.toolchain);
        key.extend_from_slice(&self.environment);
        key.extend_from_slice(&self.target_platform);
        key
    }

    fn scope_mismatch_fields(&self, scope: &CompilerTrustScope) -> CompilerScopeMismatchFields {
        let mut mismatches = CompilerScopeMismatchFields::default();
        if self.namespace_id != scope.namespace_id {
            mismatches.insert(CompilerScopeField::Namespace);
        }
        if self.recipe != scope.recipe {
            mismatches.insert(CompilerScopeField::Recipe);
        }
        if self.profile != scope.profile {
            mismatches.insert(CompilerScopeField::Profile);
        }
        if self.stage != scope.stage {
            mismatches.insert(CompilerScopeField::Stage);
        }
        if self.toolchain != scope.toolchain {
            mismatches.insert(CompilerScopeField::Toolchain);
        }
        if self.environment != scope.environment {
            mismatches.insert(CompilerScopeField::Environment);
        }
        if self.target_platform != scope.target_platform {
            mismatches.insert(CompilerScopeField::TargetPlatform);
        }
        mismatches
    }

    fn authorizes(&self, peer: EndpointId, scope: &CompilerTrustScope) -> bool {
        self.peer == peer && self.scope_mismatch_fields(scope).is_empty()
    }
}

/// Owner-only persisted allowlist of remote compiler execution grants.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TrustedCompilerWorkerPolicy {
    grants: Vec<TrustedCompilerWorkerGrant>,
}

impl TrustedCompilerWorkerPolicy {
    /// Loads and validates a persisted policy, or returns an empty deny-all
    /// policy when the file does not exist.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, CompilerTrustError> {
        let path = path.as_ref();
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(CompilerTrustError::Io(error.to_string())),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(CompilerTrustError::InvalidFile);
        }
        #[cfg(windows)]
        if backend_platform::win32::security::is_endpoint_metadata(&metadata) {
            return Err(CompilerTrustError::InvalidFile);
        }
        if metadata.len() > MAX_POLICY_BYTES as u64 {
            return Err(CompilerTrustError::InvalidFile);
        }
        let mut file =
            backend_platform::durable::open_private_read(path).map_err(private_policy_io_error)?;
        validate_open_policy_file(&file)?;
        let mut bytes = Vec::new();
        file.take((MAX_POLICY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| CompilerTrustError::Io(error.to_string()))?;
        if bytes.len() > MAX_POLICY_BYTES {
            return Err(CompilerTrustError::InvalidFile);
        }
        decode_policy(&bytes)
    }

    /// Exact persisted grants, sorted by their canonical scope bytes.
    #[must_use]
    pub fn grants(&self) -> &[TrustedCompilerWorkerGrant] {
        &self.grants
    }

    /// Adds one grant. A peer cannot be silently rebound to a different
    /// address; revoke it first, then explicitly enroll the replacement.
    pub fn add(&mut self, grant: TrustedCompilerWorkerGrant) -> Result<(), CompilerTrustError> {
        if self.grants.len() >= MAX_GRANTS {
            return Err(CompilerTrustError::Limit);
        }
        if self.grants.contains(&grant) {
            return Err(CompilerTrustError::DuplicateGrant);
        }
        if self
            .grants
            .iter()
            .any(|existing| existing.peer == grant.peer && existing.address != grant.address)
        {
            return Err(CompilerTrustError::PeerAddressConflict);
        }
        self.grants.push(grant);
        self.grants
            .sort_by_key(TrustedCompilerWorkerGrant::canonical_key);
        Ok(())
    }

    /// Removes every exact execution scope for one authenticated peer.
    /// Missing peers are rejected so a stale or mistyped revoke is visible.
    pub fn revoke_peer(&mut self, peer: EndpointId) -> Result<usize, CompilerTrustError> {
        let original = self.grants.len();
        self.grants.retain(|grant| grant.peer != peer);
        let removed = original - self.grants.len();
        if removed == 0 {
            return Err(CompilerTrustError::PeerNotTrusted);
        }
        Ok(removed)
    }

    /// Checks an authenticated peer against the complete persisted execution
    /// scope. A stale or revoked peer identity fails closed.
    #[must_use]
    pub fn authorizes(
        &self,
        peer: EndpointId,
        namespace_id: [u8; 16],
        recipe: [u8; 32],
        profile: LanguageProfile,
        stage: Stage,
        toolchain: [u8; 32],
        environment: [u8; 32],
        target_platform: [u8; 32],
    ) -> bool {
        let scope = CompilerTrustScope::new(
            namespace_id,
            recipe,
            profile,
            stage,
            toolchain,
            environment,
            target_platform,
        );
        self.authorizes_scope(peer, &scope)
    }

    /// Checks a peer against a previously projected scope using the same comparator that
    /// produces mismatch diagnostics.
    #[must_use]
    pub(crate) fn authorizes_scope(&self, peer: EndpointId, scope: &CompilerTrustScope) -> bool {
        self.grants
            .iter()
            .any(|grant| grant.authorizes(peer, scope))
    }

    /// Summarizes which exact trust-scope fields differ, without exposing identities or peers.
    /// The policy is fixed-size and the diagnostic scan stops after a separate small cap.
    #[must_use]
    pub(crate) fn scope_mismatch_histogram(
        &self,
        scope: &CompilerTrustScope,
    ) -> CompilerScopeMismatchHistogram {
        CompilerScopeMismatchHistogram::from_grants(&self.grants, scope)
    }

    /// Atomically writes the canonical policy with owner-only file permissions.
    pub fn save_atomic(&self, path: impl AsRef<Path>) -> Result<(), CompilerTrustError> {
        let path = path.as_ref();
        validate_private_parent(path)?;
        validate_existing_destination(path)?;
        let bytes = encode_policy(self)?;
        backend_platform::durable::write_private_atomic(path, &bytes)
            .map_err(|error| CompilerTrustError::Io(error.to_string()))
    }
}

/// Failure while admitting or persisting an owner compiler trust grant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CompilerTrustError {
    /// One public identity, endpoint, profile, or scope was invalid.
    InvalidGrant,
    /// The policy file was not a canonical bounded regular file.
    InvalidFile,
    /// The policy file or parent directory is not protected for this user.
    Permissions,
    /// An exact grant already exists.
    DuplicateGrant,
    /// One peer identity was presented with a different endpoint address.
    PeerAddressConflict,
    /// The requested peer is absent, revoked, or stale.
    PeerNotTrusted,
    /// A fixed policy resource bound was reached.
    Limit,
    /// A durable file operation failed.
    Io(String),
}

impl std::fmt::Display for CompilerTrustError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidGrant => formatter.write_str("compiler trust grant is invalid"),
            Self::InvalidFile => formatter.write_str("compiler trust file is invalid"),
            Self::Permissions => formatter.write_str("compiler trust file must be owner-only"),
            Self::DuplicateGrant => formatter.write_str("compiler trust grant already exists"),
            Self::PeerAddressConflict => {
                formatter.write_str("peer address changed; revoke the peer before re-enrollment")
            }
            Self::PeerNotTrusted => formatter.write_str("compiler peer is not trusted"),
            Self::Limit => formatter.write_str("compiler trust policy reached its fixed limit"),
            Self::Io(message) => write!(formatter, "compiler trust storage failed: {message}"),
        }
    }
}

impl std::error::Error for CompilerTrustError {}

fn encode_policy(policy: &TrustedCompilerWorkerPolicy) -> Result<Vec<u8>, CompilerTrustError> {
    validate_grants(&policy.grants)?;
    let mut bytes = Vec::with_capacity(12 + policy.grants.len() * 200);
    bytes.extend_from_slice(POLICY_MAGIC);
    bytes.extend_from_slice(
        &u32::try_from(policy.grants.len())
            .map_err(|_| CompilerTrustError::Limit)?
            .to_be_bytes(),
    );
    for grant in &policy.grants {
        bytes.extend_from_slice(grant.peer.as_bytes());
        let address = grant.address.to_string();
        if address.is_empty() || address.len() > MAX_ADDRESS_BYTES {
            return Err(CompilerTrustError::InvalidGrant);
        }
        bytes.extend_from_slice(
            &u16::try_from(address.len())
                .map_err(|_| CompilerTrustError::Limit)?
                .to_be_bytes(),
        );
        bytes.extend_from_slice(address.as_bytes());
        bytes.extend_from_slice(&grant.namespace_id);
        bytes.extend_from_slice(&grant.recipe);
        bytes.extend_from_slice(&<[u8; 2]>::from(grant.profile));
        bytes.push(u8::from(grant.stage));
        bytes.extend_from_slice(&grant.toolchain);
        bytes.extend_from_slice(&grant.environment);
        bytes.extend_from_slice(&grant.target_platform);
    }
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(CompilerTrustError::Limit);
    }
    Ok(bytes)
}

fn decode_policy(bytes: &[u8]) -> Result<TrustedCompilerWorkerPolicy, CompilerTrustError> {
    if bytes.len() < 12 || bytes.len() > MAX_POLICY_BYTES || &bytes[..8] != POLICY_MAGIC {
        return Err(CompilerTrustError::InvalidFile);
    }
    let count = usize::try_from(u32::from_be_bytes(
        bytes[8..12]
            .try_into()
            .map_err(|_| CompilerTrustError::InvalidFile)?,
    ))
    .map_err(|_| CompilerTrustError::InvalidFile)?;
    if count > MAX_GRANTS {
        return Err(CompilerTrustError::InvalidFile);
    }
    let mut reader = PolicyReader { bytes, offset: 12 };
    let mut grants = Vec::with_capacity(count);
    for _ in 0..count {
        let peer_bytes = reader.array::<32>()?;
        let peer =
            EndpointId::from_bytes(&peer_bytes).map_err(|_| CompilerTrustError::InvalidGrant)?;
        let address_length = usize::from(reader.u16()?);
        if address_length == 0 || address_length > MAX_ADDRESS_BYTES {
            return Err(CompilerTrustError::InvalidFile);
        }
        let address_text = std::str::from_utf8(reader.take(address_length)?)
            .map_err(|_| CompilerTrustError::InvalidFile)?;
        let address = address_text
            .parse::<SocketAddr>()
            .map_err(|_| CompilerTrustError::InvalidGrant)?;
        if address.to_string() != address_text {
            return Err(CompilerTrustError::InvalidFile);
        }
        let namespace_id = reader.array::<16>()?;
        let recipe = reader.array::<32>()?;
        let profile = LanguageProfile::try_from(reader.array::<2>()?)
            .map_err(|_| CompilerTrustError::InvalidGrant)?;
        let stage = Stage::try_from(reader.u8()?).map_err(|_| CompilerTrustError::InvalidGrant)?;
        let toolchain = reader.array::<32>()?;
        let environment = reader.array::<32>()?;
        let target_platform = reader.array::<32>()?;
        grants.push(TrustedCompilerWorkerGrant::new(
            peer,
            address,
            namespace_id,
            recipe,
            profile,
            stage,
            toolchain,
            environment,
            target_platform,
        )?);
    }
    if !reader.is_empty() {
        return Err(CompilerTrustError::InvalidFile);
    }
    let policy = TrustedCompilerWorkerPolicy { grants };
    validate_grants(&policy.grants).map_err(|_| CompilerTrustError::InvalidFile)?;
    Ok(policy)
}

fn validate_grants(grants: &[TrustedCompilerWorkerGrant]) -> Result<(), CompilerTrustError> {
    if grants.len() > MAX_GRANTS
        || grants
            .windows(2)
            .any(|pair| pair[0].canonical_key() >= pair[1].canonical_key())
        || grants.iter().enumerate().any(|(index, grant)| {
            grants[..index]
                .iter()
                .any(|previous| previous.peer == grant.peer && previous.address != grant.address)
        })
    {
        return Err(CompilerTrustError::InvalidFile);
    }
    Ok(())
}

fn validate_private_parent(path: &Path) -> Result<(), CompilerTrustError> {
    if !path.is_absolute() || path.file_name().is_none() {
        return Err(CompilerTrustError::InvalidFile);
    }
    let parent = path.parent().ok_or(CompilerTrustError::InvalidFile)?;
    let existed = parent.exists();
    #[cfg(windows)]
    validate_windows_path_chain(parent)?;
    fs::create_dir_all(parent).map_err(|error| CompilerTrustError::Io(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let metadata = fs::symlink_metadata(parent)
            .map_err(|error| CompilerTrustError::Io(error.to_string()))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(CompilerTrustError::InvalidFile);
        }
        if existed {
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(CompilerTrustError::Permissions);
            }
        } else {
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .map_err(|error| CompilerTrustError::Io(error.to_string()))?;
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use backend_platform::win32::security::{is_endpoint_metadata, restrict_to_current_user};
        let _ = existed;
        let metadata = fs::symlink_metadata(parent)
            .map_err(|error| CompilerTrustError::Io(error.to_string()))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || is_endpoint_metadata(&metadata)
        {
            return Err(CompilerTrustError::InvalidFile);
        }
        restrict_to_current_user(parent)
            .map_err(|error| CompilerTrustError::Io(error.to_string()))?;
        let restricted = fs::symlink_metadata(parent)
            .map_err(|error| CompilerTrustError::Io(error.to_string()))?;
        if !restricted.is_dir()
            || restricted.file_type().is_symlink()
            || is_endpoint_metadata(&restricted)
        {
            return Err(CompilerTrustError::InvalidFile);
        }
        Ok(())
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = existed;
        Err(CompilerTrustError::Permissions)
    }
}

fn validate_open_policy_file(file: &File) -> Result<(), CompilerTrustError> {
    let metadata = file
        .metadata()
        .map_err(|error| CompilerTrustError::Io(error.to_string()))?;
    if !metadata.is_file() || metadata.len() > MAX_POLICY_BYTES as u64 {
        return Err(CompilerTrustError::InvalidFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 1 {
            return Err(CompilerTrustError::Permissions);
        }
    }
    Ok(())
}

fn private_policy_io_error(error: std::io::Error) -> CompilerTrustError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        CompilerTrustError::Permissions
    } else {
        CompilerTrustError::Io(error.to_string())
    }
}

fn validate_existing_destination(path: &Path) -> Result<(), CompilerTrustError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(CompilerTrustError::Io(error.to_string())),
    };
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_POLICY_BYTES as u64
    {
        return Err(CompilerTrustError::InvalidFile);
    }
    #[cfg(windows)]
    if backend_platform::win32::security::is_endpoint_metadata(&metadata) {
        return Err(CompilerTrustError::InvalidFile);
    }
    let file =
        backend_platform::durable::open_private_read(path).map_err(private_policy_io_error)?;
    validate_open_policy_file(&file)
}

#[cfg(windows)]
fn validate_windows_path_chain(path: &Path) -> Result<(), CompilerTrustError> {
    for component in path.ancestors() {
        match fs::symlink_metadata(component) {
            Ok(metadata)
                if metadata.file_type().is_symlink()
                    || backend_platform::win32::security::is_endpoint_metadata(&metadata) =>
            {
                return Err(CompilerTrustError::InvalidFile);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CompilerTrustError::Io(error.to_string())),
        }
    }
    Ok(())
}

struct PolicyReader<'bytes> {
    bytes: &'bytes [u8],
    offset: usize,
}

impl<'bytes> PolicyReader<'bytes> {
    fn take(&mut self, length: usize) -> Result<&'bytes [u8], CompilerTrustError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(CompilerTrustError::InvalidFile)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(CompilerTrustError::InvalidFile)?;
        self.offset = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], CompilerTrustError> {
        self.take(N)?
            .try_into()
            .map_err(|_| CompilerTrustError::InvalidFile)
    }

    fn u8(&mut self) -> Result<u8, CompilerTrustError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, CompilerTrustError> {
        Ok(u16::from_be_bytes(self.array()?))
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::cluster_transport::SecretKey;
    use backend_semantic::vocabulary::RustEdition;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn path() -> std::path::PathBuf {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "backend-compiler-trust-{}-{sequence}",
            std::process::id()
        ))
    }

    fn peer(seed: u8) -> EndpointId {
        SecretKey::from_bytes(&[seed; 32]).public()
    }

    fn grant(peer: EndpointId, address: SocketAddr) -> TrustedCompilerWorkerGrant {
        TrustedCompilerWorkerGrant::new(
            peer,
            address,
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
        )
        .expect("exact peer grant")
    }

    fn scope() -> CompilerTrustScope {
        CompilerTrustScope::new(
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
        )
    }

    #[test]
    fn compiler_scope_mismatch_fields_cover_each_exact_grant_field() {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38_221);
        let grant = grant(peer(7), address);
        let baseline = scope();
        assert_eq!(
            grant.scope_mismatch_fields(&baseline),
            CompilerScopeMismatchFields::default()
        );

        let cases = [
            (
                CompilerScopeField::Namespace,
                CompilerTrustScope {
                    namespace_id: [9; 16],
                    ..baseline
                },
            ),
            (
                CompilerScopeField::Recipe,
                CompilerTrustScope {
                    recipe: [9; 32],
                    ..baseline
                },
            ),
            (
                CompilerScopeField::Profile,
                CompilerTrustScope {
                    profile: LanguageProfile::Rust(RustEdition::Rust2021),
                    ..baseline
                },
            ),
            (
                CompilerScopeField::Stage,
                CompilerTrustScope {
                    stage: Stage::Parse,
                    ..baseline
                },
            ),
            (
                CompilerScopeField::Toolchain,
                CompilerTrustScope {
                    toolchain: [9; 32],
                    ..baseline
                },
            ),
            (
                CompilerScopeField::Environment,
                CompilerTrustScope {
                    environment: [9; 32],
                    ..baseline
                },
            ),
            (
                CompilerScopeField::TargetPlatform,
                CompilerTrustScope {
                    target_platform: [9; 32],
                    ..baseline
                },
            ),
        ];

        for (field, candidate) in cases {
            assert_eq!(
                grant.scope_mismatch_fields(&candidate),
                CompilerScopeMismatchFields(field.bit()),
                "{} must be one independent mismatch bit",
                field.label(),
            );
            assert!(!grant.authorizes(peer(7), &candidate));
        }
        assert!(!grant.authorizes(peer(8), &baseline));
    }

    #[test]
    fn scope_mismatch_histogram_is_empty_without_grants_and_groups_multiple_rows() {
        let empty = TrustedCompilerWorkerPolicy::default()
            .scope_mismatch_histogram(&scope())
            .to_string();
        assert_eq!(
            empty,
            "scope_grants_examined=0 scope_grants_truncated=0 scope_mismatch_bins=[]"
        );

        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38_221);
        let exact = grant(peer(1), address);
        let mut recipe = grant(peer(2), address);
        recipe.recipe = [9; 32];
        let mut profile_environment = grant(peer(3), address);
        profile_environment.profile = LanguageProfile::Rust(RustEdition::Rust2021);
        profile_environment.environment = [9; 32];
        let mut policy = TrustedCompilerWorkerPolicy::default();
        policy
            .add(profile_environment)
            .expect("add profile/environment row");
        policy.add(recipe).expect("add recipe row");
        policy.add(exact).expect("add exact row");

        let first = policy.scope_mismatch_histogram(&scope()).to_string();
        let second = policy.scope_mismatch_histogram(&scope()).to_string();
        assert_eq!(first, second, "histogram formatting must be deterministic");
        assert_eq!(
            first,
            "scope_grants_examined=3 scope_grants_truncated=0 scope_mismatch_bins=[exact:1,recipe:1,profile+environment:1]"
        );
        assert!(!first.contains("0101010101010101"));
        assert!(!first.contains("peer"));
    }

    #[test]
    fn scope_mismatch_histogram_reports_its_scan_cap() {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38_221);
        let candidate = CompilerTrustScope {
            recipe: [9; 32],
            ..scope()
        };
        let mut policy = TrustedCompilerWorkerPolicy::default();
        for seed in 1..=67_u8 {
            policy
                .add(grant(peer(seed), address))
                .expect("add bounded test grant");
        }

        let histogram = policy.scope_mismatch_histogram(&candidate);
        assert_eq!(histogram.grants_examined, 64);
        assert!(histogram.truncated);
        assert_eq!(
            histogram.bins[usize::from(CompilerScopeField::Recipe.bit())],
            64
        );
        assert_eq!(
            histogram.to_string(),
            "scope_grants_examined=64 scope_grants_truncated=1 scope_mismatch_bins=[recipe:64]"
        );
    }

    #[test]
    fn trust_policy_roundtrips_exact_scope_and_rejects_stale_or_duplicate_peers() {
        let root = path();
        let policy_path = root.join("compiler-worker-trust.v1");
        let endpoint = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38_221);
        let grant = grant(peer(7), endpoint);
        let mut policy = TrustedCompilerWorkerPolicy::default();
        policy.add(grant.clone()).expect("add peer");
        assert_eq!(
            policy.add(grant.clone()),
            Err(CompilerTrustError::DuplicateGrant)
        );
        policy
            .save_atomic(&policy_path)
            .expect("atomic owner-only policy write");
        let mut reopened = TrustedCompilerWorkerPolicy::load(&policy_path).expect("cold reopen");
        assert_eq!(reopened.grants(), &[grant.clone()]);
        assert!(reopened.authorizes(
            peer(7),
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
        ));
        let authorizes_scope =
            |namespace_id, recipe, profile, stage, toolchain, environment, target_platform| {
                reopened.authorizes(
                    peer(7),
                    namespace_id,
                    recipe,
                    profile,
                    stage,
                    toolchain,
                    environment,
                    target_platform,
                )
            };
        assert!(!authorizes_scope(
            [9; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
        ));
        assert!(!authorizes_scope(
            [1; 16],
            [9; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
        ));
        assert!(!authorizes_scope(
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2021),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
        ));
        assert!(!authorizes_scope(
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::Parse,
            [3; 32],
            [4; 32],
            [5; 32],
        ));
        assert!(!authorizes_scope(
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [9; 32],
            [4; 32],
            [5; 32],
        ));
        assert!(!authorizes_scope(
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [9; 32],
            [5; 32],
        ));
        assert!(!authorizes_scope(
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [9; 32],
        ));
        assert!(!reopened.authorizes(
            peer(8),
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
        ));
        assert_eq!(
            reopened.revoke_peer(peer(8)),
            Err(CompilerTrustError::PeerNotTrusted)
        );
        assert_eq!(reopened.revoke_peer(peer(7)), Ok(1));
        assert!(!reopened.authorizes(
            peer(7),
            [1; 16],
            [2; 32],
            LanguageProfile::Rust(RustEdition::Rust2024),
            Stage::LowerIr,
            [3; 32],
            [4; 32],
            [5; 32],
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn zero_scope_and_rebinding_a_peer_address_are_rejected() {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38_221);
        assert_eq!(
            TrustedCompilerWorkerGrant::new(
                peer(9),
                address,
                [0; 16],
                [2; 32],
                LanguageProfile::Rust(RustEdition::Rust2024),
                Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
            ),
            Err(CompilerTrustError::InvalidGrant)
        );
        let mut policy = TrustedCompilerWorkerPolicy::default();
        policy.add(grant(peer(9), address)).expect("first endpoint");
        let changed = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38_222);
        assert_eq!(
            policy.add(grant(peer(9), changed)),
            Err(CompilerTrustError::PeerAddressConflict)
        );
        assert_eq!(
            TrustedCompilerWorkerGrant::new(
                peer(10),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 38_221),
                [1; 16],
                [2; 32],
                LanguageProfile::Rust(RustEdition::Rust2024),
                Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
            ),
            Err(CompilerTrustError::InvalidGrant)
        );
        assert_eq!(
            TrustedCompilerWorkerGrant::new(
                peer(10),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
                [1; 16],
                [2; 32],
                LanguageProfile::Rust(RustEdition::Rust2024),
                Stage::LowerIr,
                [3; 32],
                [4; 32],
                [5; 32],
            ),
            Err(CompilerTrustError::InvalidGrant)
        );
    }

    #[test]
    fn policy_file_rejects_non_owner_permissions_and_symlinks() {
        let root = path();
        let real = root.join("real");
        let link = root.join("link");
        let mut policy = TrustedCompilerWorkerPolicy::default();
        policy
            .add(grant(
                peer(11),
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 38_221),
            ))
            .expect("grant");
        policy.save_atomic(&real).expect("write policy");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&real, fs::Permissions::from_mode(0o644)).expect("chmod");
            assert_eq!(
                TrustedCompilerWorkerPolicy::load(&real),
                Err(CompilerTrustError::Permissions)
            );
            fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).expect("restore mode");
            std::os::unix::fs::symlink(&real, &link).expect("symlink");
            assert_eq!(
                TrustedCompilerWorkerPolicy::load(&link),
                Err(CompilerTrustError::InvalidFile)
            );
        }
        let _ = fs::remove_dir_all(root);
    }
}

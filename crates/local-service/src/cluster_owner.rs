//! Persisted direct-only identity and addressing for the local compiler owner.

use backend_engine::cluster_transport::{
    EndpointId, RemoteIndexCapability, RemoteIndexCapabilityClaims, RemoteIndexCapabilityError,
    RemoteIndexCapabilityIssuer, ScopedClusterInvite, SecretKey,
};
use backend_platform::directory::DirectoryCapability;
use std::fs::File;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

const OWNER_MAGIC: &[u8; 8] = b"BKCOOW01";
const MAX_OWNER_CONFIG_BYTES: usize = 320;
const MAX_ADDRESS_BYTES: usize = 128;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Durable local Iroh identity and the direct address advertised to workers.
///
/// The signing key is never printed or exposed by the public accessors. The
/// configured bind address may be a wildcard; the advertised address must be
/// a concrete direct address that workers can use.
#[derive(Clone, Eq, PartialEq)]
pub struct ClusterOwnerConfig {
    secret_key: [u8; 32],
    bind_address: SocketAddr,
    advertised_address: SocketAddr,
}

impl ClusterOwnerConfig {
    /// Creates a fresh owner identity and atomically persists it at `path`.
    pub fn create(
        path: impl AsRef<Path>,
        bind_address: SocketAddr,
        advertised_address: SocketAddr,
    ) -> Result<Self, ClusterOwnerConfigError> {
        validate_addresses(bind_address, advertised_address)?;
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        let (directory, name) = config_location(path)?;
        let config = Self {
            secret_key: SecretKey::generate().to_bytes(),
            bind_address,
            advertised_address,
        };
        write_private_new(&directory, &name, &config.encode()?)?;
        Self::load(path)
    }

    /// Opens and validates one cold-start owner configuration.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ClusterOwnerConfigError> {
        let path = path.as_ref();
        let (directory, name) = config_location(path)?;
        let mut file = directory.open_private_file(&name).map_err(io_error)?;
        if file.metadata().map_err(io_error)?.len() > MAX_OWNER_CONFIG_BYTES as u64 {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        let bytes = read_bounded(&mut file)?;
        let config = Self::decode(&bytes)?;
        if config.encode()?.as_slice() != bytes {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        Ok(config)
    }

    /// Authenticated Iroh identity announced to explicitly trusted workers.
    #[must_use]
    pub fn endpoint_id(&self) -> EndpointId {
        SecretKey::from_bytes(&self.secret_key).public()
    }

    /// Direct socket on which the local owner binds its no-relay endpoint.
    #[must_use]
    pub const fn bind_address(&self) -> SocketAddr {
        self.bind_address
    }

    /// Direct socket address carried in worker invites.
    #[must_use]
    pub const fn advertised_address(&self) -> SocketAddr {
        self.advertised_address
    }

    /// Reconstitutes the secret only for the transport endpoint constructor.
    #[must_use]
    pub(crate) fn secret_key(&self) -> SecretKey {
        SecretKey::from_bytes(&self.secret_key)
    }

    /// Mints the canonical short-lived worker invite for this owner endpoint.
    pub fn invite(
        &self,
        namespace_id: [u8; 16],
        recipe: [u8; 32],
        profile: [u8; 2],
        stage: u8,
        toolchain: [u8; 32],
        environment: [u8; 32],
        target_platform: [u8; 32],
        expires_unix_ms: u64,
    ) -> Result<ScopedClusterInvite, ClusterOwnerConfigError> {
        ScopedClusterInvite::new(
            self.endpoint_id(),
            self.advertised_address,
            namespace_id,
            recipe,
            profile,
            stage,
            toolchain,
            environment,
            target_platform,
            expires_unix_ms,
            backend_engine::cluster_transport::ClusterExecutionClass::TrustedCoordinatorHostExecution,
        )
        .map_err(|_| ClusterOwnerConfigError::InvalidInvite)
    }

    /// Issues a read-only remote index grant without exposing the signing key.
    pub fn issue_remote_index_capability(
        &self,
        claims: RemoteIndexCapabilityClaims,
        now_ms: u64,
    ) -> Result<RemoteIndexCapability, RemoteIndexCapabilityError> {
        RemoteIndexCapabilityIssuer::new(self.secret_key()).issue(claims, now_ms)
    }

    fn encode(&self) -> Result<Vec<u8>, ClusterOwnerConfigError> {
        validate_addresses(self.bind_address, self.advertised_address)?;
        let bind = self.bind_address.to_string();
        let advertised = self.advertised_address.to_string();
        if bind.len() > MAX_ADDRESS_BYTES || advertised.len() > MAX_ADDRESS_BYTES {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        let mut bytes =
            Vec::with_capacity(OWNER_MAGIC.len() + 32 + 4 + bind.len() + advertised.len());
        bytes.extend_from_slice(OWNER_MAGIC);
        bytes.extend_from_slice(&self.secret_key);
        put_address(&mut bytes, &bind)?;
        put_address(&mut bytes, &advertised)?;
        if bytes.len() > MAX_OWNER_CONFIG_BYTES {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        Ok(bytes)
    }

    fn decode(bytes: &[u8]) -> Result<Self, ClusterOwnerConfigError> {
        if bytes.len() > MAX_OWNER_CONFIG_BYTES || bytes.len() < OWNER_MAGIC.len() + 32 + 4 {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        let mut reader = Reader::new(bytes);
        if reader.take(OWNER_MAGIC.len())? != OWNER_MAGIC {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        let secret_key = reader.array()?;
        let bind_address = reader.address()?;
        let advertised_address = reader.address()?;
        reader.finish()?;
        validate_addresses(bind_address, advertised_address)?;
        let _ = SecretKey::from_bytes(&secret_key);
        Ok(Self {
            secret_key,
            bind_address,
            advertised_address,
        })
    }
}

impl std::fmt::Debug for ClusterOwnerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClusterOwnerConfig")
            .field("endpoint_id", &self.endpoint_id())
            .field("bind_address", &self.bind_address)
            .field("advertised_address", &self.advertised_address)
            .finish_non_exhaustive()
    }
}

/// Rejected owner configuration, storage, or invite claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClusterOwnerConfigError {
    /// The owner identity is already configured; it is never silently replaced.
    AlreadyExists,
    /// The owner config path is not a private regular file.
    InvalidFile,
    /// The requested bind or advertised socket is invalid.
    InvalidAddress,
    /// The scoped invite contains invalid or empty authority fields.
    InvalidInvite,
    /// The host cannot prove owner-only persistence for this platform.
    Permissions,
    /// A filesystem operation failed.
    Io(String),
}

impl std::fmt::Display for ClusterOwnerConfigError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExists => formatter.write_str("cluster owner identity already exists"),
            Self::InvalidFile => {
                formatter.write_str("cluster owner config is malformed or insecure")
            }
            Self::InvalidAddress => formatter.write_str("cluster owner address is invalid"),
            Self::InvalidInvite => formatter.write_str("cluster worker invite claims are invalid"),
            Self::Permissions => {
                formatter.write_str("owner-only cluster config storage is unavailable")
            }
            Self::Io(error) => write!(formatter, "cluster owner config I/O failed: {error}"),
        }
    }
}

impl std::error::Error for ClusterOwnerConfigError {}

fn validate_addresses(
    bind: SocketAddr,
    advertised: SocketAddr,
) -> Result<(), ClusterOwnerConfigError> {
    let bind_ip_valid = !bind.ip().is_multicast();
    let advertised_ip_valid = !advertised.ip().is_unspecified() && !advertised.ip().is_multicast();
    if bind.port() == 0
        || advertised.port() == 0
        || !bind_ip_valid
        || !advertised_ip_valid
        || bind.port() != advertised.port()
    {
        return Err(ClusterOwnerConfigError::InvalidAddress);
    }
    Ok(())
}

fn put_address(bytes: &mut Vec<u8>, address: &str) -> Result<(), ClusterOwnerConfigError> {
    let length = u16::try_from(address.len()).map_err(|_| ClusterOwnerConfigError::InvalidFile)?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(address.as_bytes());
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, cursor: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ClusterOwnerConfigError> {
        let end = self
            .cursor
            .checked_add(length)
            .ok_or(ClusterOwnerConfigError::InvalidFile)?;
        let bytes = self
            .bytes
            .get(self.cursor..end)
            .ok_or(ClusterOwnerConfigError::InvalidFile)?;
        self.cursor = end;
        Ok(bytes)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], ClusterOwnerConfigError> {
        self.take(N)?
            .try_into()
            .map_err(|_| ClusterOwnerConfigError::InvalidFile)
    }

    fn address(&mut self) -> Result<SocketAddr, ClusterOwnerConfigError> {
        let length = usize::from(u16::from_be_bytes(self.array()?));
        if length == 0 || length > MAX_ADDRESS_BYTES {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        let text = std::str::from_utf8(self.take(length)?)
            .map_err(|_| ClusterOwnerConfigError::InvalidFile)?;
        let address = text
            .parse::<SocketAddr>()
            .map_err(|_| ClusterOwnerConfigError::InvalidFile)?;
        if address.to_string() != text {
            return Err(ClusterOwnerConfigError::InvalidFile);
        }
        Ok(address)
    }

    fn finish(self) -> Result<(), ClusterOwnerConfigError> {
        if self.cursor == self.bytes.len() {
            Ok(())
        } else {
            Err(ClusterOwnerConfigError::InvalidFile)
        }
    }
}

fn config_location(path: &Path) -> Result<(DirectoryCapability, String), ClusterOwnerConfigError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .ok_or(ClusterOwnerConfigError::InvalidFile)?;
    let directory = DirectoryCapability::open(parent).map_err(io_error)?;
    directory.validate_private().map_err(io_error)?;
    Ok((directory, name.to_owned()))
}

fn read_bounded(file: &mut File) -> Result<Vec<u8>, ClusterOwnerConfigError> {
    let mut bytes = Vec::new();
    file.take((MAX_OWNER_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_OWNER_CONFIG_BYTES {
        return Err(ClusterOwnerConfigError::InvalidFile);
    }
    Ok(bytes)
}

fn write_private_new(
    directory: &DirectoryCapability,
    name: &str,
    bytes: &[u8],
) -> Result<(), ClusterOwnerConfigError> {
    if bytes.len() > MAX_OWNER_CONFIG_BYTES {
        return Err(ClusterOwnerConfigError::InvalidFile);
    }
    for _ in 0..128 {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let temporary = format!(".{name}.tmp-{}-{sequence}", std::process::id());
        let mut file = match directory.create_file_exclusive(&temporary) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(io_error(error)),
        };
        let write_result = file.write_all(bytes).and_then(|()| file.sync_all());
        drop(file);
        if let Err(error) = write_result {
            let _ = directory.remove_file(&temporary);
            return Err(io_error(error));
        }
        if let Err(error) = directory.rename(&temporary, name, false) {
            let _ = directory.remove_file(&temporary);
            return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
                ClusterOwnerConfigError::AlreadyExists
            } else {
                io_error(error)
            });
        }
        directory.sync_all().map_err(io_error)?;
        return Ok(());
    }
    Err(ClusterOwnerConfigError::Io(
        "could not allocate a unique owner-config temporary".to_owned(),
    ))
}

fn io_error(error: std::io::Error) -> ClusterOwnerConfigError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        ClusterOwnerConfigError::Permissions
    } else {
        ClusterOwnerConfigError::Io(error.to_string())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
    use std::fs;
    use std::path::PathBuf;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            use std::os::unix::fs::PermissionsExt as _;
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "backend-local-cluster-owner-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("create scratch root");
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .expect("restrict scratch root");
            Self(root)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn owner_identity_and_invite_survive_a_cold_reopen() {
        let scratch = Scratch::new();
        let path = scratch.0.join("cluster-owner.v1");
        let bind = "0.0.0.0:40123".parse().expect("bind address");
        let advertised = "192.0.2.10:40123".parse().expect("advertised address");
        let created =
            ClusterOwnerConfig::create(&path, bind, advertised).expect("create owner config");
        let reopened = ClusterOwnerConfig::load(&path).expect("cold reopen owner config");
        assert_eq!(reopened.endpoint_id(), created.endpoint_id());
        assert_eq!(reopened.bind_address(), bind);
        assert_eq!(reopened.advertised_address(), advertised);
        let profile: [u8; 2] = LanguageProfile::Rust(RustEdition::Rust2024).into();
        let stage: u8 = Stage::LowerIr.into();
        let invite = reopened
            .invite(
                [4; 16],
                [5; 32],
                profile,
                stage,
                [6; 32],
                [7; 32],
                [8; 32],
                1_800_000_000_000,
            )
            .expect("make scoped invite");
        let token = invite.encode_token().expect("encode invite");
        let decoded = ScopedClusterInvite::decode_token(&token).expect("decode invite");
        assert_eq!(decoded.coordinator(), reopened.endpoint_id());
        assert_eq!(decoded.address(), advertised);
        assert_eq!(decoded.namespace_id(), [4; 16]);
        assert_eq!(decoded.fingerprint(), invite.fingerprint());
    }

    #[test]
    fn owner_config_rejects_mismatched_ports_and_never_replaces_identity() {
        let scratch = Scratch::new();
        let path = scratch.0.join("cluster-owner.v1");
        let bind = "127.0.0.1:40123".parse().expect("bind address");
        let advertised = "127.0.0.1:40124".parse().expect("advertised address");
        assert_eq!(
            ClusterOwnerConfig::create(&path, bind, advertised),
            Err(ClusterOwnerConfigError::InvalidAddress)
        );
        let advertised = "127.0.0.1:40123".parse().expect("advertised address");
        let first =
            ClusterOwnerConfig::create(&path, bind, advertised).expect("create first identity");
        assert_eq!(
            ClusterOwnerConfig::create(&path, bind, advertised),
            Err(ClusterOwnerConfigError::AlreadyExists)
        );
        assert_eq!(
            ClusterOwnerConfig::load(&path)
                .expect("reopen original identity")
                .endpoint_id(),
            first.endpoint_id()
        );
    }

    #[test]
    fn concurrent_owner_initialization_never_replaces_the_winning_identity() {
        let scratch = Scratch::new();
        let path = scratch.0.join("cluster-owner.v1");
        let bind = "127.0.0.1:40123".parse().expect("bind address");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let path = path.clone();
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    ClusterOwnerConfig::create(&path, bind, bind)
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("join owner initialization"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| {
                    result.as_ref().err() == Some(&ClusterOwnerConfigError::AlreadyExists)
                })
                .count(),
            1
        );
        let persisted = ClusterOwnerConfig::load(&path).expect("load winning owner identity");
        assert!(
            results
                .iter()
                .filter_map(|result| result.as_ref().ok())
                .any(|created| created.endpoint_id() == persisted.endpoint_id())
        );
    }

    #[test]
    fn owner_config_rejects_symlinked_ancestor_without_writing_through_it() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let scratch = Scratch::new();
        let outside = scratch.0.join("outside");
        fs::create_dir(&outside).expect("create outside directory");
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o700))
            .expect("restrict outside directory");
        let alias = scratch.0.join("owner-alias");
        symlink(&outside, &alias).expect("create ancestor symlink");
        let path = alias.join("cluster-owner.v1");
        let bind = "127.0.0.1:40123".parse().expect("bind address");
        assert!(ClusterOwnerConfig::create(&path, bind, bind).is_err());
        assert!(!outside.join("cluster-owner.v1").exists());
        assert!(ClusterOwnerConfig::load(&path).is_err());
    }

    #[cfg(not(target_vendor = "apple"))]
    #[test]
    fn owner_config_fifo_refusal_does_not_wait_for_a_writer() {
        use rustix::fs::{CWD, Mode, mkfifoat};

        let scratch = Scratch::new();
        let path = scratch.0.join("owner-fifo");
        mkfifoat(CWD, &path, Mode::from_bits_truncate(0o600)).expect("create owner FIFO");
        assert!(ClusterOwnerConfig::load(path).is_err());
    }
}

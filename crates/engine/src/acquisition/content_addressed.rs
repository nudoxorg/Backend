//! Shared content-addressed admission primitives.
//!
//! Registry and local-project adapters deliberately stop at this boundary.  A
//! caller supplies a byte stream and an optional authenticated object identity;
//! this module owns bounded streaming, durable temporary state, first-writer
//! publication, restartable transfers, and archive-manifest admission.  The
//! format is intentionally boring: immutable objects are regular files, an
//! incomplete transfer is a private `.part` file, and a completed transfer is
//! linked into its digest path exactly once.

use super::{ManifestEntry, RawArchiveObjectId, TreeManifest};
use blake3::Hasher;
use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CHUNK_BYTES: usize = 64 * 1024;
const ID_BYTES: usize = 32;
const OBJECT_DOMAIN: &[u8] = b"backend.acquisition.archive.v1\0";
const TRANSFER_MAGIC: &[u8; 8] = b"NDOXTR02";
const TEMP_TTL: Duration = Duration::from_secs(15 * 60);
const TRANSFER_OWNER_LOCK_STRIPES: usize = 256;
const MAX_VALIDATOR_BYTES: usize = 1024;

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn digest_bytes(fields: &[&[u8]]) -> [u8; ID_BYTES] {
    let mut hasher = Hasher::new();
    hasher.update(b"backend.acquisition.transfer.v1\0");
    for field in fields {
        hasher.update(&(field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    *hasher.finalize().as_bytes()
}

fn object_from_hash(length: u64, hash: [u8; ID_BYTES]) -> RawArchiveObjectId {
    RawArchiveObjectId::from_verified_claim(length, hash)
}

fn archive_hash<R: Read>(
    reader: &mut R,
    maximum: u64,
) -> Result<(u64, [u8; ID_BYTES]), ContentStoreError> {
    let mut hasher = Hasher::new();
    hasher.update(OBJECT_DOMAIN);
    let mut buffer = [0_u8; CHUNK_BYTES];
    let mut length = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        length = length
            .checked_add(read as u64)
            .ok_or(ContentStoreError::Bounds { maximum })?;
        if length > maximum {
            return Err(ContentStoreError::Bounds { maximum });
        }
        hasher.update(&buffer[..read]);
    }
    Ok((length, *hasher.finalize().as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

/// A typed failure at the immutable object boundary.
#[derive(Debug)]
pub enum ContentStoreError {
    /// A filesystem operation failed.
    Io(io::Error),
    /// A byte or entry budget was exceeded.
    Bounds { maximum: u64 },
    /// The stream did not match the authenticated object identity.
    DigestMismatch {
        /// The expected object identity.
        expected: RawArchiveObjectId,
        /// The identity observed while streaming.
        actual: RawArchiveObjectId,
        /// The quarantined bytes, when quarantine succeeded.
        quarantine: Option<PathBuf>,
    },
    /// The stream length did not match the authenticated extent.
    LengthMismatch {
        /// The expected length.
        expected: u64,
        /// The observed length.
        actual: u64,
    },
    /// A resumable transfer has inconsistent state and bytes.
    TransferStateMismatch,
    /// A checkpoint update had an ambiguous durability result; reopen the
    /// transfer before issuing more operations.
    TransferNeedsReopen,
    /// A resumable response was authenticated by a different representation
    /// validator than the bytes already persisted for the transfer.
    TransferValidatorChanged,
    /// Another process or thread holds the transfer's exclusive owner lock.
    TransferBusy,
    /// A claimed object on disk failed an explicit integrity verification.
    CorruptObject { path: PathBuf },
    /// A caller rejected the staged bytes before immutable publication.
    ///
    /// The temporary is moved to quarantine before this error is returned so
    /// an integrity or policy rejection can never become a reachable object.
    VerificationRejected { quarantine: Option<PathBuf> },
    /// A supplied archive path is not a safe canonical relative path.
    InvalidArchivePath,
    /// An archive path was admitted more than once.
    DuplicateArchivePath,
    /// The archive entry count exceeded its budget.
    ArchiveEntryLimit,
    /// The archive byte extent exceeded its budget.
    ArchiveBytesLimit,
    /// An archive path exceeded its budget.
    ArchivePathLimit,
}

impl fmt::Display for ContentStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "content store I/O failed: {error}"),
            Self::Bounds { maximum } => write!(formatter, "content stream exceeds {maximum} bytes"),
            Self::DigestMismatch {
                expected, actual, ..
            } => {
                write!(
                    formatter,
                    "content digest mismatch: expected {expected:?}, got {actual:?}"
                )
            }
            Self::LengthMismatch { expected, actual } => {
                write!(
                    formatter,
                    "content length mismatch: expected {expected}, got {actual}"
                )
            }
            Self::TransferStateMismatch => {
                formatter.write_str("resumable transfer state is inconsistent")
            }
            Self::TransferNeedsReopen => formatter
                .write_str("resumable transfer must be reopened after an uncertain checkpoint"),
            Self::TransferValidatorChanged => {
                formatter.write_str("resumable transfer representation validator changed")
            }
            Self::TransferBusy => {
                formatter.write_str("resumable transfer is owned by another writer")
            }
            Self::CorruptObject { path } => {
                write!(formatter, "content object is corrupt: {}", path.display())
            }
            Self::VerificationRejected { .. } => {
                formatter.write_str("staged content failed adapter verification")
            }
            Self::InvalidArchivePath => {
                formatter.write_str("archive path is not canonical and relative")
            }
            Self::DuplicateArchivePath => formatter.write_str("archive path occurs more than once"),
            Self::ArchiveEntryLimit => formatter.write_str("archive entry budget exceeded"),
            Self::ArchiveBytesLimit => formatter.write_str("archive byte budget exceeded"),
            Self::ArchivePathLimit => formatter.write_str("archive path budget exceeded"),
        }
    }
}

impl std::error::Error for ContentStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ContentStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Result of first-writer-wins object publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObjectAdmission {
    /// The caller linked the private bytes into the immutable object store.
    Published {
        /// The authenticated object identity.
        object: RawArchiveObjectId,
        /// The object extent in bytes.
        bytes: u64,
    },
    /// An identical object was already present and its bytes were reused.
    Reused {
        /// The authenticated object identity.
        object: RawArchiveObjectId,
        /// The existing object extent in bytes.
        bytes: u64,
    },
}

impl ObjectAdmission {
    /// Returns the admitted immutable identity.
    #[must_use]
    pub const fn object(self) -> RawArchiveObjectId {
        match self {
            Self::Published { object, .. } | Self::Reused { object, .. } => object,
        }
    }

    /// Returns the admitted extent.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        match self {
            Self::Published { bytes, .. } | Self::Reused { bytes, .. } => bytes,
        }
    }

    /// Returns whether this call performed the publication.
    #[must_use]
    pub const fn was_published(self) -> bool {
        matches!(self, Self::Published { .. })
    }
}

/// One bounded immutable archive-manifest policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveBudget {
    /// Maximum number of file rows.
    pub max_entries: usize,
    /// Maximum sum of admitted file bytes.
    pub max_bytes: u64,
    /// Maximum UTF-8 path length.
    pub max_path_bytes: usize,
    /// Maximum bytes represented by one file row.
    pub max_entry_bytes: u64,
}

impl Default for ArchiveBudget {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_bytes: 256 * 1024 * 1024,
            max_path_bytes: 4 * 1024,
            max_entry_bytes: 64 * 1024 * 1024,
        }
    }
}

/// An immutable admitted manifest and its extent.
#[derive(Clone, Debug)]
pub struct ArchiveManifest {
    tree: Arc<TreeManifest>,
    bytes: u64,
}

impl ArchiveManifest {
    /// Returns the immutable tree identity.
    #[must_use]
    pub fn id(&self) -> super::TreeManifestId {
        self.tree.id()
    }

    /// Returns the total file bytes represented by this manifest.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Returns canonical entries in path order.
    #[must_use]
    pub fn entries(&self) -> &[ManifestEntry] {
        self.tree.entries()
    }

    /// Borrows the shared tree object used by local and registry sources.
    #[must_use]
    pub fn tree(&self) -> &TreeManifest {
        &self.tree
    }

    /// Clones the cheap immutable tree handle for a source snapshot without
    /// rebuilding or rehashing its canonical entries.
    #[must_use]
    pub fn tree_arc(&self) -> Arc<TreeManifest> {
        Arc::clone(&self.tree)
    }
}

/// Streaming-friendly archive manifest admission.
pub struct ArchiveManifestBuilder {
    budget: ArchiveBudget,
    bytes: u64,
    entries: Vec<ManifestEntry>,
}

impl fmt::Debug for ArchiveManifestBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArchiveManifestBuilder")
            .field("budget", &self.budget)
            .field("bytes", &self.bytes)
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl ArchiveManifestBuilder {
    /// Starts an empty manifest under one explicit budget.
    #[must_use]
    pub fn new(budget: ArchiveBudget) -> Self {
        Self {
            budget,
            bytes: 0,
            entries: Vec::new(),
        }
    }

    /// Admits one already content-addressed file row.
    pub fn push_file(
        &mut self,
        path: impl Into<Arc<str>>,
        object: RawArchiveObjectId,
        bytes: u64,
        mode: u32,
    ) -> Result<(), ContentStoreError> {
        let path = path.into();
        validate_archive_path(&path, self.budget.max_path_bytes)?;
        if bytes > self.budget.max_entry_bytes {
            return Err(ContentStoreError::ArchiveBytesLimit);
        }
        if self.entries.len() >= self.budget.max_entries {
            return Err(ContentStoreError::ArchiveEntryLimit);
        }
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .ok_or(ContentStoreError::ArchiveBytesLimit)?;
        if self.bytes > self.budget.max_bytes {
            self.bytes = self.bytes.saturating_sub(bytes);
            return Err(ContentStoreError::ArchiveBytesLimit);
        }
        self.entries.push(ManifestEntry { path, object, mode });
        Ok(())
    }

    /// Finalizes the sorted, duplicate-free immutable manifest.
    pub fn finish(self) -> Result<ArchiveManifest, ContentStoreError> {
        let tree = TreeManifest::new(self.entries).map_err(|error| match error {
            super::IdentityError::Duplicate => ContentStoreError::DuplicateArchivePath,
            _ => ContentStoreError::InvalidArchivePath,
        })?;
        Ok(ArchiveManifest {
            tree: Arc::new(tree),
            bytes: self.bytes,
        })
    }
}

fn validate_archive_path(path: &str, maximum: usize) -> Result<(), ContentStoreError> {
    if path.is_empty() || path.len() > maximum || path.starts_with('/') || path.contains('\\') {
        return Err(if path.len() > maximum {
            ContentStoreError::ArchivePathLimit
        } else {
            ContentStoreError::InvalidArchivePath
        });
    }
    if path
        .bytes()
        .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(ContentStoreError::InvalidArchivePath);
    }
    Ok(())
}

/// Durable content-addressed object root shared by local and registry ingest.
#[derive(Clone, Debug)]
pub struct ContentAddressedStore {
    root: Arc<PathBuf>,
    nonce: Arc<Mutex<u64>>,
    active_artifacts: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl ContentAddressedStore {
    /// Opens or creates an object root. Existing published digest directories
    /// are never removed or recreated by this call.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ContentStoreError> {
        let requested_root = root.into();
        let root_existed = requested_root.is_dir();
        fs::create_dir_all(&requested_root)?;
        // Resolve configured root aliases once. All store-owned children are
        // then checked relative to this trusted root without following links.
        let root = fs::canonicalize(&requested_root)?;
        for directory in ["objects", "temps", "transfers", "quarantine"] {
            let path = root.join(directory);
            match fs::create_dir(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            validate_store_directory(&root, &path)?;
        }
        sync_directory(&root)?;
        if !root_existed {
            let parent = root
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            sync_directory(parent)?;
        }
        Ok(Self {
            root: Arc::new(root),
            nonce: Arc::new(Mutex::new(now_millis() ^ u64::from(std::process::id()))),
            active_artifacts: Arc::new(Mutex::new(BTreeSet::new())),
        })
    }

    /// Returns the private root path.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.as_ref()
    }

    fn next_token(&self) -> [u8; ID_BYTES] {
        let mut nonce = self
            .nonce
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *nonce = nonce
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        digest_bytes(&[
            &nonce.to_be_bytes(),
            &u64::from(std::process::id()).to_be_bytes(),
            &super::TOKEN_COUNTER
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .to_be_bytes(),
        ])
    }

    fn object_path_for(&self, object: RawArchiveObjectId) -> PathBuf {
        let encoded = hex(object.as_bytes());
        self.root
            .join("objects")
            .join(&encoded[..2])
            .join(&encoded[2..])
    }

    fn ensure_object_parent(&self, target: &Path) -> Result<(), ContentStoreError> {
        let parent = target.parent().ok_or_else(|| {
            ContentStoreError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "content object path has no parent directory",
            ))
        })?;
        let objects = self.root.join("objects");
        validate_store_directory(self.root.as_ref(), &objects)?;
        let relative = parent.strip_prefix(&objects).map_err(|_| {
            ContentStoreError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "content object fanout escaped its object root",
            ))
        })?;
        let mut current = objects;
        for component in relative.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(ContentStoreError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "content object fanout has a non-normal component",
                )));
            };
            current.push(name);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_dir() => {
                    validate_store_directory(self.root.as_ref(), &current)?;
                }
                Ok(_) => {
                    return Err(ContentStoreError::CorruptObject {
                        path: current.clone(),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match fs::create_dir(&current) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error.into()),
                    }
                    validate_store_directory(self.root.as_ref(), &current)?;
                    let parent = current.parent().expect("fanout component has a parent");
                    sync_directory(parent)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    fn verify_file_identity(
        &self,
        path: &Path,
        object: RawArchiveObjectId,
        bytes: u64,
    ) -> Result<(), ContentStoreError> {
        let mut file = open_content_file(self.root.as_ref(), path)?;
        let (length, hash) = match archive_hash(&mut file, bytes) {
            Ok(identity) => identity,
            Err(ContentStoreError::Bounds { .. }) => {
                return Err(ContentStoreError::CorruptObject {
                    path: path.to_path_buf(),
                });
            }
            Err(error) => return Err(error),
        };
        if length != bytes || object_from_hash(length, hash) != object {
            return Err(ContentStoreError::CorruptObject {
                path: path.to_path_buf(),
            });
        }
        Ok(())
    }

    fn publish_file(
        &self,
        source: &Path,
        object: RawArchiveObjectId,
        bytes: u64,
    ) -> Result<ObjectAdmission, ContentStoreError> {
        self.publish_file_with_hook(source, object, bytes, false, |_| Ok(()))
    }

    fn publish_file_with_hook<F>(
        &self,
        source: &Path,
        object: RawArchiveObjectId,
        bytes: u64,
        force_staging: bool,
        mut hook: F,
    ) -> Result<ObjectAdmission, ContentStoreError>
    where
        F: FnMut(PublishPoint) -> io::Result<()>,
    {
        // Adapter verification runs while the bytes are private and receives
        // their path. Recheck the closed file before trusting that path as the
        // source of an immutable object.
        self.verify_file_identity(source, object, bytes)?;

        let target = self.object_path_for(object);
        self.ensure_object_parent(&target)?;
        let parent = target.parent().expect("object parent was ensured");
        if force_staging {
            return self.publish_staged_file_with_hook(source, &target, object, bytes, &mut hook);
        }

        let admission = match fs::hard_link(source, &target) {
            Ok(()) => {
                hook(PublishPoint::ObjectLinked)?;
                ObjectAdmission::Published { object, bytes }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.verify_file_identity(&target, object, bytes)?;
                ObjectAdmission::Reused { object, bytes }
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                return self
                    .publish_staged_file_with_hook(source, &target, object, bytes, &mut hook);
            }
            Err(error) => return Err(error.into()),
        };
        hook(PublishPoint::BeforeDirectorySync)?;
        sync_directory(parent)?;
        hook(PublishPoint::DirectorySynced)?;
        Ok(admission)
    }

    fn publish_staged_file_with_hook<F>(
        &self,
        source: &Path,
        target: &Path,
        object: RawArchiveObjectId,
        bytes: u64,
        hook: &mut F,
    ) -> Result<ObjectAdmission, ContentStoreError>
    where
        F: FnMut(PublishPoint) -> io::Result<()>,
    {
        let parent = target.parent().ok_or_else(|| {
            ContentStoreError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "content object path has no parent directory",
            ))
        })?;
        let mut stage = DestinationStage::create(self, parent, target)?;
        hook(PublishPoint::StageCreated)?;

        let mut source_file = File::open(source)?;
        let mut buffer = [0_u8; CHUNK_BYTES];
        let mut copied = 0_u64;
        loop {
            let read = source_file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            copied = copied
                .checked_add(read as u64)
                .ok_or_else(|| io::Error::other("staged content extent overflowed"))?;
            if copied > bytes {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "staged content extent exceeds its authenticated length",
                )
                .into());
            }
            stage.file_mut()?.write_all(&buffer[..read])?;
            hook(PublishPoint::StageCopyProgress)?;
        }
        if copied != bytes {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "staged content extent differs from its authenticated length",
            )
            .into());
        }
        hook(PublishPoint::StageWritten)?;
        stage.file_mut()?.sync_all()?;
        self.verify_file_identity(stage.path(), object, bytes)?;
        hook(PublishPoint::StageSynced)?;

        let admission = match fs::hard_link(stage.path(), target) {
            Ok(()) => {
                hook(PublishPoint::ObjectLinked)?;
                ObjectAdmission::Published { object, bytes }
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.verify_file_identity(target, object, bytes)?;
                ObjectAdmission::Reused { object, bytes }
            }
            Err(error) => return Err(error.into()),
        };
        drop(stage);
        hook(PublishPoint::BeforeDirectorySync)?;
        sync_directory(parent)?;
        hook(PublishPoint::DirectorySynced)?;
        Ok(admission)
    }

    /// Returns the immutable path for one content identity.
    #[must_use]
    pub fn object_path(&self, object: RawArchiveObjectId) -> PathBuf {
        self.object_path_for(object)
    }

    /// Returns the object extent when a published object is present.
    pub fn object_len(&self, object: RawArchiveObjectId) -> Result<Option<u64>, ContentStoreError> {
        let path = self.object_path_for(object);
        match validate_store_directory(
            self.root.as_ref(),
            path.parent().expect("object path has a fanout parent"),
        ) {
            Ok(()) => {}
            Err(ContentStoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() => Ok(Some(
                open_content_file(self.root.as_ref(), &path)?
                    .metadata()?
                    .len(),
            )),
            Ok(_) => Err(ContentStoreError::CorruptObject { path }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Verifies an existing object by streaming it through the same identity
    /// grammar used at admission.
    pub fn verify_object(
        &self,
        object: RawArchiveObjectId,
        maximum: u64,
    ) -> Result<u64, ContentStoreError> {
        let path = self.object_path_for(object);
        let mut file = open_content_file(self.root.as_ref(), &path)?;
        let (length, hash) = archive_hash(&mut file, maximum)?;
        if object_from_hash(length, hash) != object {
            return Err(ContentStoreError::CorruptObject { path });
        }
        Ok(length)
    }

    /// Opens an admitted object without copying it into a caller buffer.
    pub fn open_object(&self, object: RawArchiveObjectId) -> Result<File, ContentStoreError> {
        open_content_file(self.root.as_ref(), &self.object_path_for(object))
    }

    fn create_temp(&self, label: &str) -> Result<TempArtifact, ContentStoreError> {
        validate_store_directory(self.root.as_ref(), &self.root.join("temps"))?;
        let safe_label: String = label
            .bytes()
            .filter(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            .map(char::from)
            .collect();
        let label = if safe_label.is_empty() {
            "object"
        } else {
            &safe_label
        };
        let (token, path, file, lock) = loop {
            let token = self.next_token();
            let path = self
                .root
                .join("temps")
                .join(format!("{}.{}.part", hex(&token), label));
            match OpenOptions::new()
                .write(true)
                .read(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    let lock =
                        match backend_platform::durability::open_or_create_regular_file_nofollow(
                            &path,
                        ) {
                            Ok(lock) => lock,
                            Err(error) => {
                                drop(file);
                                let _ = fs::remove_file(&path);
                                return Err(error.into());
                            }
                        };
                    if let Err(error) = lock.try_lock() {
                        drop(lock);
                        drop(file);
                        let _ = fs::remove_file(&path);
                        return Err(match error {
                            std::fs::TryLockError::WouldBlock => ContentStoreError::Io(
                                io::Error::other("new private temp lock was unexpectedly busy"),
                            ),
                            std::fs::TryLockError::Error(error) => ContentStoreError::Io(error),
                        });
                    }
                    break (token, path, file, lock);
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        };
        let lease_path = path.with_extension("lease");
        if let Err(error) = write_lease_marker(
            &lease_path,
            token,
            now_millis().saturating_add(TEMP_TTL.as_millis() as u64),
        ) {
            drop(lock);
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        self.active_artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.clone());
        Ok(TempArtifact {
            store: self.clone(),
            path,
            lease_path,
            token,
            file: Some(file),
            _lock: lock,
            preserve: false,
        })
    }

    /// Streams one object into private storage and publishes it atomically.
    /// If `expected` is already present, the reader is never touched.
    pub fn admit_reader<R: Read>(
        &self,
        expected: Option<RawArchiveObjectId>,
        mut source: R,
        maximum: u64,
    ) -> Result<ObjectAdmission, ContentStoreError> {
        self.admit_reader_verified(expected, &mut source, maximum, |_path, _bytes, _object| {
            Ok(())
        })
    }

    /// Streams one object into private storage, lets an adapter verify the
    /// closed temporary file, and publishes only after that verification
    /// succeeds.
    ///
    /// The verifier receives a stable path after the temporary has been
    /// flushed and closed. This keeps adapter-specific checksum logic outside
    /// the content store while retaining one publication path for every
    /// producer. A rejected temporary is quarantined and never linked into
    /// the immutable object namespace.
    pub fn admit_reader_verified<R, V>(
        &self,
        expected: Option<RawArchiveObjectId>,
        mut source: R,
        maximum: u64,
        verifier: V,
    ) -> Result<ObjectAdmission, ContentStoreError>
    where
        R: Read,
        V: FnOnce(&Path, u64, RawArchiveObjectId) -> Result<(), ContentStoreError>,
    {
        if let Some(object) = expected
            && let Some(bytes) = self.object_len(object)?
        {
            self.verify_object(object, maximum)?;
            let parent = self
                .object_path_for(object)
                .parent()
                .expect("object path has a fanout parent");
            sync_directory(parent)?;
            return Ok(ObjectAdmission::Reused { object, bytes });
        }
        let mut temp = self.create_temp("object")?;
        let (length, hash) = stream_to_temp(&mut source, &mut temp, maximum)?;
        let actual = object_from_hash(length, hash);
        if let Some(expected) = expected
            && expected != actual
        {
            let quarantine = temp.quarantine("digest-mismatch")?;
            return Err(ContentStoreError::DigestMismatch {
                expected,
                actual,
                quarantine: Some(quarantine),
            });
        }
        temp.sync_close()?;
        if let Err(error) = verifier(temp.path(), length, actual) {
            let quarantine = temp.quarantine("verification").ok();
            if matches!(error, ContentStoreError::VerificationRejected { .. }) {
                return Err(ContentStoreError::VerificationRejected { quarantine });
            }
            return Err(error);
        }
        self.publish_temp(&mut temp, actual, length)
    }

    /// Admits every regular file below a local directory into the same
    /// content-addressed object root used by registry archives, then returns
    /// one canonical tree. Paths are slash-separated and traversal is
    /// deterministic, so an archive adapter can construct the identical tree
    /// without copying or rehashing unchanged file bytes.
    pub fn admit_directory(
        &self,
        root: impl AsRef<Path>,
        budget: ArchiveBudget,
    ) -> Result<ArchiveManifest, ContentStoreError> {
        let root = root.as_ref();
        if !root.is_dir() {
            return Err(ContentStoreError::Io(io::Error::new(
                io::ErrorKind::NotADirectory,
                "source root is not a directory",
            )));
        }
        let mut builder = ArchiveManifestBuilder::new(budget);
        admit_directory_entries(self, root, root, &mut builder, budget.max_entry_bytes)?;
        builder.finish()
    }

    fn publish_temp(
        &self,
        temp: &mut TempArtifact,
        object: RawArchiveObjectId,
        bytes: u64,
    ) -> Result<ObjectAdmission, ContentStoreError> {
        temp.sync_close()?;
        let admission = self.publish_file(temp.path(), object, bytes)?;
        temp.preserve = false;
        Ok(admission)
    }

    /// Removes only owned, expired private artifacts. A live in-process temp
    /// or destination stage is protected by the active set even if its mtime
    /// is old. Cross-process temps and destination stages also hold OS locks,
    /// so a verifier or stalled operation cannot be removed merely because
    /// its lease timestamp expired.
    pub fn scavenge_stale(&self, older_than: Duration) -> Result<usize, ContentStoreError> {
        validate_store_directory(self.root.as_ref(), &self.root.join("temps"))?;
        validate_store_directory(self.root.as_ref(), &self.root.join("objects"))?;
        let now = now_millis();
        let active = self
            .active_artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut removed = 0;
        for entry in fs::read_dir(self.root.join("temps"))? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some("part")
            {
                continue;
            }
            if active.contains(&path) {
                continue;
            }
            let marker = path.with_extension("lease");
            if read_lease_expiry(&marker).is_some_and(|expiry| expiry > now) {
                continue;
            }
            let modified = entry.metadata()?.modified().unwrap_or(UNIX_EPOCH);
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or_default();
            if age >= older_than.max(TEMP_TTL) {
                let temp_lock =
                    match backend_platform::durability::open_regular_file_readwrite_nofollow(&path)
                    {
                        Ok(file) => file,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                    };
                match temp_lock.try_lock() {
                    Ok(()) => match fs::remove_file(&path) {
                        Ok(()) => {
                            let _ = fs::remove_file(marker);
                            removed += 1;
                        }
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    },
                    Err(std::fs::TryLockError::WouldBlock) => {}
                    Err(std::fs::TryLockError::Error(error))
                        if error.kind() == io::ErrorKind::NotFound => {}
                    Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                }
            }
        }
        for bucket in fs::read_dir(self.root.join("objects"))? {
            let bucket = bucket?;
            if !bucket.file_type()?.is_dir() {
                continue;
            }
            validate_store_directory(self.root.as_ref(), &bucket.path())?;
            for entry in fs::read_dir(bucket.path())? {
                let entry = entry?;
                let path = entry.path();
                if !entry.file_type()?.is_file()
                    || !is_destination_stage(&path)
                    || active.contains(&path)
                {
                    continue;
                }
                let modified = entry.metadata()?.modified().unwrap_or(UNIX_EPOCH);
                let age = SystemTime::now()
                    .duration_since(modified)
                    .unwrap_or_default();
                if age >= older_than.max(TEMP_TTL) {
                    let removed_stage = {
                        let active_now = self
                            .active_artifacts
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if active_now.contains(&path) {
                            false
                        } else {
                            let stage_lock =
                                match backend_platform::durability::open_regular_file_readwrite_nofollow(
                                    &path,
                                ) {
                                    Ok(file) => file,
                                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                                        continue;
                                    }
                                    Err(error) => return Err(error.into()),
                                };
                            match stage_lock.try_lock() {
                                Ok(()) => match fs::remove_file(&path) {
                                    Ok(()) => true,
                                    Err(error) if error.kind() == io::ErrorKind::NotFound => false,
                                    Err(error) => return Err(error.into()),
                                },
                                Err(std::fs::TryLockError::WouldBlock) => false,
                                Err(std::fs::TryLockError::Error(error))
                                    if error.kind() == io::ErrorKind::NotFound =>
                                {
                                    false
                                }
                                Err(std::fs::TryLockError::Error(error)) => {
                                    return Err(error.into());
                                }
                            }
                        }
                    };
                    if removed_stage {
                        removed += 1;
                    }
                }
            }
        }
        Ok(removed)
    }

    /// Opens or resumes one durable range-capable transfer.
    ///
    /// A fixed set of `.owner-lock-*` markers is retained for safe
    /// interprocess locking. Transfer IDs that map to the same marker serialize
    /// while active; the marker set remains bounded and its files are never
    /// unlinked.
    pub fn resume_or_start(
        &self,
        id: TransferId,
        expected: Option<RawArchiveObjectId>,
        expected_length: Option<u64>,
    ) -> Result<ResumableTransfer, ContentStoreError> {
        validate_store_directory(self.root.as_ref(), &self.root.join("transfers"))?;
        ResumableTransfer::open(self.clone(), id, expected, expected_length)
    }
}

fn admit_directory_entries(
    store: &ContentAddressedStore,
    root: &Path,
    directory: &Path,
    builder: &mut ArchiveManifestBuilder,
    maximum_entry_bytes: u64,
) -> Result<(), ContentStoreError> {
    let mut children = fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    children.sort();
    for path in children {
        let file_type = fs::symlink_metadata(&path)?.file_type();
        if file_type.is_dir() {
            admit_directory_entries(store, root, &path, builder, maximum_entry_bytes)?;
            continue;
        }
        if !file_type.is_file() {
            return Err(ContentStoreError::InvalidArchivePath);
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| ContentStoreError::InvalidArchivePath)?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        let metadata = fs::metadata(&path)?;
        if metadata.len() > maximum_entry_bytes {
            return Err(ContentStoreError::ArchiveBytesLimit);
        }
        let mut file = File::open(&path)?;
        let admission = store.admit_reader(None, &mut file, maximum_entry_bytes)?;
        builder.push_file(relative, admission.object(), admission.bytes(), 0)?;
    }
    Ok(())
}

fn stream_to_temp<R: Read>(
    source: &mut R,
    temp: &mut TempArtifact,
    maximum: u64,
) -> Result<(u64, [u8; ID_BYTES]), ContentStoreError> {
    let mut hasher = Hasher::new();
    hasher.update(OBJECT_DOMAIN);
    let mut buffer = [0_u8; CHUNK_BYTES];
    let mut length = 0_u64;
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let next = length
            .checked_add(read as u64)
            .ok_or(ContentStoreError::Bounds { maximum })?;
        if next > maximum {
            return Err(ContentStoreError::Bounds { maximum });
        }
        temp.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        length = next;
    }
    temp.sync_data()?;
    Ok((length, *hasher.finalize().as_bytes()))
}

fn sync_directory(path: &Path) -> Result<(), ContentStoreError> {
    backend_platform::durability::open_directory_nofollow(path)
        .and_then(|directory| directory.sync_all())
        .map_err(ContentStoreError::Io)
}

fn open_content_file(root: &Path, path: &Path) -> Result<File, ContentStoreError> {
    // The metadata check gives a typed corruption error for an already
    // visible symlink/non-file. The platform opener repeats the regular-file
    // check on the opened handle and refuses link following, closing the
    // metadata/open race.
    let parent = path.parent().ok_or_else(|| {
        ContentStoreError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "content file path has no parent directory",
        ))
    })?;
    validate_store_directory(root, parent)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(ContentStoreError::CorruptObject {
                path: path.to_path_buf(),
            });
        }
        Err(error) => return Err(error.into()),
    }
    backend_platform::durability::open_regular_file_nofollow(path).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            ContentStoreError::CorruptObject {
                path: path.to_path_buf(),
            }
        } else {
            ContentStoreError::Io(error)
        }
    })
}

fn validate_store_directory(root: &Path, directory: &Path) -> Result<(), ContentStoreError> {
    // Each component is opened without following its final link, so a symlink
    // or reparse point already present is rejected. The caller's subsequent
    // mutations are still path-based; concurrent ancestor replacement would
    // require descriptor-relative operations and remains outside this helper.
    let relative = directory.strip_prefix(root).map_err(|_| {
        ContentStoreError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "content store directory escaped its root",
        ))
    })?;
    let mut current = root.to_path_buf();
    let open_checked = |path: &Path| {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(ContentStoreError::CorruptObject {
                    path: path.to_path_buf(),
                });
            }
            Err(error) => return Err(error.into()),
        }
        backend_platform::durability::open_directory_nofollow(path).map_err(|error| {
            if error.kind() == io::ErrorKind::InvalidData {
                ContentStoreError::CorruptObject {
                    path: path.to_path_buf(),
                }
            } else {
                ContentStoreError::Io(error)
            }
        })
    };
    drop(open_checked(&current)?);
    for component in relative.components() {
        match component {
            std::path::Component::Normal(name) => current.push(name),
            _ => {
                return Err(ContentStoreError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "content store directory has a non-normal component",
                )));
            }
        }
        drop(open_checked(&current)?);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublishPoint {
    StageCreated,
    StageCopyProgress,
    StageWritten,
    StageSynced,
    ObjectLinked,
    BeforeDirectorySync,
    DirectorySynced,
}

struct DestinationStage {
    store: ContentAddressedStore,
    path: PathBuf,
    file: Option<File>,
}

impl DestinationStage {
    fn create(
        store: &ContentAddressedStore,
        parent: &Path,
        target: &Path,
    ) -> Result<Self, ContentStoreError> {
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                ContentStoreError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "content object path needs a Unicode file name",
                ))
            })?;
        loop {
            let path = parent.join(format!(".{name}.{}.stage", hex(&store.next_token())));
            let inserted = store
                .active_artifacts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(path.clone());
            if !inserted {
                continue;
            }
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    if let Err(error) = file.try_lock() {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        store
                            .active_artifacts
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&path);
                        return Err(match error {
                            std::fs::TryLockError::WouldBlock => {
                                ContentStoreError::Io(io::Error::other(
                                    "new destination stage lock was unexpectedly busy",
                                ))
                            }
                            std::fs::TryLockError::Error(error) => ContentStoreError::Io(error),
                        });
                    }
                    return Ok(Self {
                        store: store.clone(),
                        path,
                        file: Some(file),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    store
                        .active_artifacts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&path);
                }
                Err(error) => {
                    store
                        .active_artifacts
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&path);
                    return Err(error.into());
                }
            }
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn file_mut(&mut self) -> Result<&mut File, ContentStoreError> {
        self.file
            .as_mut()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("stage file is closed")))
    }
}

impl Drop for DestinationStage {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
        self.store
            .active_artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
    }
}

fn is_destination_stage(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(rest) = name.strip_prefix('.') else {
        return false;
    };
    let Some((object_tail, rest)) = rest.split_once('.') else {
        return false;
    };
    let Some((token, extension)) = rest.split_once('.') else {
        return false;
    };
    extension == "stage"
        && object_tail.len() == ID_BYTES * 2 - 2
        && object_tail.bytes().all(|byte| byte.is_ascii_hexdigit())
        && token.len() == ID_BYTES * 2
        && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

struct TempArtifact {
    store: ContentAddressedStore,
    path: PathBuf,
    lease_path: PathBuf,
    token: [u8; ID_BYTES],
    file: Option<File>,
    _lock: File,
    preserve: bool,
}

impl TempArtifact {
    fn path(&self) -> &Path {
        &self.path
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), ContentStoreError> {
        self.file
            .as_mut()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("private temp is closed")))?
            .write_all(bytes)?;
        Ok(())
    }

    fn sync_data(&mut self) -> Result<(), ContentStoreError> {
        if let Some(file) = self.file.as_mut() {
            file.flush()?;
            file.sync_data()?;
        }
        Ok(())
    }

    fn sync_close(&mut self) -> Result<(), ContentStoreError> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
            file.sync_all()?;
        }
        Ok(())
    }

    fn quarantine(&mut self, reason: &str) -> Result<PathBuf, ContentStoreError> {
        self.sync_close()?;
        let destination = self.store.root.join("quarantine").join(format!(
            "{}.{}.part",
            hex(&self.token),
            reason
        ));
        fs::rename(&self.path, &destination)?;
        self.preserve = true;
        Ok(destination)
    }
}

impl Drop for TempArtifact {
    fn drop(&mut self) {
        let _ = self.file.take();
        if !self.preserve {
            let _ = fs::remove_file(&self.path);
        }
        let _ = fs::remove_file(&self.lease_path);
        self.store
            .active_artifacts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
    }
}

fn write_lease_marker(
    path: &Path,
    token: [u8; ID_BYTES],
    expires: u64,
) -> Result<(), ContentStoreError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&token)?;
    file.write_all(&expires.to_be_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn read_lease_expiry(path: &Path) -> Option<u64> {
    let mut file = backend_platform::durability::open_regular_file_nofollow(path).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    if bytes.len() < ID_BYTES + 8 {
        return None;
    }
    let mut expiry = [0_u8; 8];
    expiry.copy_from_slice(&bytes[ID_BYTES..ID_BYTES + 8]);
    Some(u64::from_be_bytes(expiry))
}

/// Stable identity for one resumable transfer.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TransferId([u8; ID_BYTES]);

impl TransferId {
    /// Derives an identity from adapter-neutral source and object fields.
    #[must_use]
    pub fn from_parts(
        source: &[u8],
        object: Option<RawArchiveObjectId>,
        extent: Option<u64>,
    ) -> Self {
        let object = object.map_or([0; ID_BYTES], RawArchiveObjectId::to_bytes);
        let extent = extent.unwrap_or(u64::MAX).to_be_bytes();
        Self(digest_bytes(&[source, &object, &extent]))
    }

    /// Returns the fixed-width transfer identity.
    #[must_use]
    pub const fn to_bytes(self) -> [u8; ID_BYTES] {
        self.0
    }
}

/// HTTP representation validators persisted alongside an incomplete transfer.
///
/// The raw header values are retained because `If-Range` has deliberately
/// different rules for strong entity tags and HTTP dates.  The transport
/// chooses the strongest usable value when it constructs a request.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct TransferValidator {
    etag: Option<Box<str>>,
    last_modified: Option<Box<str>>,
}

impl TransferValidator {
    /// Creates a validator from response header values, dropping empty values.
    #[must_use]
    pub fn new(etag: Option<&str>, last_modified: Option<&str>) -> Option<Self> {
        let etag = bounded_header(etag);
        let last_modified = bounded_http_date(last_modified);
        (etag.is_some() || last_modified.is_some()).then_some(Self {
            etag,
            last_modified,
        })
    }

    /// Returns the entity tag, if the origin supplied one.
    #[must_use]
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// Returns the last-modified date, if the origin supplied one.
    #[must_use]
    pub fn last_modified(&self) -> Option<&str> {
        self.last_modified.as_deref()
    }

    /// Returns a safe `If-Range` value. Weak entity tags cannot be used for
    /// range validation, so a last-modified date is preferred in that case.
    #[must_use]
    pub fn if_range(&self) -> Option<&str> {
        self.etag
            .as_deref()
            .filter(|value| is_strong_etag(value))
            .or(self.last_modified())
    }
}

fn bounded_header(value: Option<&str>) -> Option<Box<str>> {
    let value = value?.trim();
    (!value.is_empty()
        && value.len() <= MAX_VALIDATOR_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii() && byte >= 0x20 && byte != 0x7f))
    .then(|| value.into())
}

fn is_strong_etag(value: &str) -> bool {
    value.len() >= 2 && !value.starts_with("W/") && value.starts_with('"') && value.ends_with('"')
}

fn bounded_http_date(value: Option<&str>) -> Option<Box<str>> {
    let value = bounded_header(value)?;
    let bytes = value.as_bytes();
    if bytes.len() != 29
        || bytes[3] != b','
        || bytes[4] != b' '
        || bytes[7] != b' '
        || bytes[11] != b' '
        || bytes[16] != b' '
        || bytes[19] != b':'
        || bytes[22] != b':'
        || bytes[25] != b' '
        || &bytes[26..] != b"GMT"
        || !bytes[..3].iter().all(u8::is_ascii_alphabetic)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..11].iter().all(u8::is_ascii_alphabetic)
        || !bytes[12..16].iter().all(u8::is_ascii_digit)
        || !bytes[17..19].iter().all(u8::is_ascii_digit)
        || !bytes[20..22].iter().all(u8::is_ascii_digit)
        || !bytes[23..25].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    Some(value)
}

/// Why an in-flight response forces a transfer to restart at byte zero.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransferResetReason {
    /// The origin supplied a different representation validator.
    ValidatorChanged,
    /// The origin ignored a requested range and returned a full body.
    RangeIgnored,
}

impl TransferResetReason {
    /// Stable edge label for telemetry, diagnostics, and quarantine names.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::ValidatorChanged => "validator-change",
            Self::RangeIgnored => "range-ignored",
        }
    }

    const fn quarantine_prefix(self) -> bool {
        matches!(self, Self::ValidatorChanged)
    }
}

/// Byte accounting for one archive transfer attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransferTelemetry {
    /// Bytes already present locally when the transfer was opened.
    pub resumed_bytes: u64,
    /// Bytes read from the current upstream response.
    pub downloaded_bytes: u64,
}

/// Durable progress exposed to a range-capable transport adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferCheckpoint {
    /// Transfer identity.
    pub id: TransferId,
    /// Optional authenticated final object.
    pub expected: Option<RawArchiveObjectId>,
    /// Optional authenticated final extent.
    pub expected_length: Option<u64>,
    /// Bytes already durably appended.
    pub received: u64,
    /// Representation validator authenticated for the persisted prefix.
    pub validator: Option<TransferValidator>,
}

/// A durable append-only transfer that survives process restart.
pub struct ResumableTransfer {
    store: ContentAddressedStore,
    checkpoint: TransferCheckpoint,
    data_path: PathBuf,
    state_path: PathBuf,
    // This stable stripe marker is also the interprocess lock. Never unlink
    // it: replacing a locked pathname could split ownership across inodes.
    owner_file: File,
    owner_token: [u8; ID_BYTES],
    file: Option<File>,
    hasher: Option<Hasher>,
    telemetry: TransferTelemetry,
    needs_reopen: bool,
}

impl fmt::Debug for ResumableTransfer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResumableTransfer")
            .field("checkpoint", &self.checkpoint)
            .field("data_path", &self.data_path)
            .finish_non_exhaustive()
    }
}

impl ResumableTransfer {
    fn open(
        store: ContentAddressedStore,
        id: TransferId,
        expected: Option<RawArchiveObjectId>,
        expected_length: Option<u64>,
    ) -> Result<Self, ContentStoreError> {
        let key = hex(&id.0);
        let data_path = store.root.join("transfers").join(format!("{key}.part"));
        let state_path = store.root.join("transfers").join(format!("{key}.state"));
        let owner_path = transfer_owner_lock_path(store.root.as_ref(), id);
        let owner_token = store.next_token();
        let owner_file = acquire_transfer_owner(&owner_path, owner_token)?;
        let open_result = (|| {
            let mut file =
                backend_platform::durability::open_or_create_append_file_nofollow(&data_path)?;
            let file_length = file.metadata()?.len();
            let saved_checkpoint = read_checkpoint(&state_path)?;
            let persisted_length = saved_checkpoint
                .as_ref()
                .and_then(|saved| saved.expected_length);
            if let Some(saved) = &saved_checkpoint {
                if saved.id != id
                    || saved.expected != expected
                    || expected_length.is_some_and(|length| saved.expected_length != Some(length))
                    || saved.received > file_length
                {
                    return Err(ContentStoreError::TransferStateMismatch);
                }
                // The checkpoint is the commit record for each append.  A
                // kill between durable data and durable checkpoint leaves an
                // uncommitted tail; discard only that tail and resume from
                // the last authenticated offset.
                if file_length > saved.received {
                    file.set_len(saved.received)?;
                    file.sync_data()?;
                }
            } else if file_length != 0 {
                // Bytes without a checkpoint were never authenticated by the
                // transfer protocol. Treat them as an interrupted first
                // write instead of admitting an orphaned prefix.
                file.set_len(0)?;
                file.sync_data()?;
            }
            let received = saved_checkpoint.as_ref().map_or(0, |saved| saved.received);
            let expected_length = expected_length.or(persisted_length);
            if expected_length.is_some_and(|length| received > length) {
                return Err(ContentStoreError::LengthMismatch {
                    expected: expected_length.unwrap_or(received),
                    actual: received,
                });
            }
            let mut hasher = Hasher::new();
            hasher.update(OBJECT_DOMAIN);
            file.seek(SeekFrom::Start(0))?;
            let mut buffer = [0_u8; CHUNK_BYTES];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            file.seek(SeekFrom::End(0))?;
            let checkpoint = TransferCheckpoint {
                id,
                expected,
                expected_length,
                received,
                validator: saved_checkpoint.and_then(|saved| saved.validator),
            };
            write_checkpoint(&state_path, &checkpoint)?;
            Ok(Self {
                store: store.clone(),
                checkpoint,
                data_path,
                state_path,
                owner_file,
                owner_token,
                file: Some(file),
                hasher: Some(hasher),
                telemetry: TransferTelemetry {
                    resumed_bytes: received,
                    downloaded_bytes: 0,
                },
                needs_reopen: false,
            })
        })();
        open_result
    }

    /// Returns the range start an adapter should request next.
    #[must_use]
    pub const fn resume_offset(&self) -> u64 {
        self.checkpoint.received
    }

    /// Returns the latest durable checkpoint.
    #[must_use]
    pub fn checkpoint(&self) -> TransferCheckpoint {
        self.checkpoint.clone()
    }

    /// Returns the validator bound to the persisted prefix.
    #[must_use]
    pub fn validator(&self) -> Option<&TransferValidator> {
        self.checkpoint.validator.as_ref()
    }

    /// Returns byte accounting for this transfer handle.
    #[must_use]
    pub const fn telemetry(&self) -> TransferTelemetry {
        self.telemetry
    }

    fn check_owner(&mut self) -> Result<(), ContentStoreError> {
        if self.needs_reopen {
            return Err(ContentStoreError::TransferNeedsReopen);
        }
        validate_store_directory(self.store.root.as_ref(), &self.store.root.join("transfers"))?;
        let mut marker = [0_u8; ID_BYTES + 8];
        self.owner_file.seek(SeekFrom::Start(0))?;
        if self.owner_file.read_exact(&mut marker).is_err()
            || marker[..ID_BYTES] != self.owner_token
        {
            return Err(ContentStoreError::TransferBusy);
        }
        Ok(())
    }

    fn poison_for_reopen(&mut self) {
        self.needs_reopen = true;
        self.file.take();
        self.hasher.take();
    }

    fn persist_checkpoint(&mut self) -> Result<(), ContentStoreError> {
        if let Err(error) = write_checkpoint(&self.state_path, &self.checkpoint) {
            self.poison_for_reopen();
            return Err(error);
        }
        Ok(())
    }

    /// Binds the representation validator before appending a response body.
    pub fn set_validator(
        &mut self,
        validator: Option<TransferValidator>,
    ) -> Result<(), ContentStoreError> {
        self.check_owner()?;
        if self.checkpoint.received != 0
            && self.checkpoint.validator.is_some()
            && self.checkpoint.validator != validator
        {
            return Err(ContentStoreError::TransferValidatorChanged);
        }
        let previous = self.checkpoint.clone();
        self.checkpoint.validator = validator;
        if let Err(error) = self.persist_checkpoint() {
            self.checkpoint = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Binds the authenticated final extent once a response exposes it.
    pub fn bind_expected_length(&mut self, length: u64) -> Result<(), ContentStoreError> {
        self.check_owner()?;
        if let Some(expected) = self.checkpoint.expected_length
            && expected != length
        {
            return Err(ContentStoreError::LengthMismatch {
                expected,
                actual: length,
            });
        }
        if self.checkpoint.received > length {
            return Err(ContentStoreError::LengthMismatch {
                expected: length,
                actual: self.checkpoint.received,
            });
        }
        let previous = self.checkpoint.clone();
        self.checkpoint.expected_length = Some(length);
        if let Err(error) = self.persist_checkpoint() {
            self.checkpoint = previous;
            return Err(error);
        }
        Ok(())
    }

    /// Restarts the transfer at byte zero, preserving no unvalidated prefix.
    pub fn restart_from_zero(
        &mut self,
        reason: Option<TransferResetReason>,
    ) -> Result<(), ContentStoreError> {
        self.check_owner()?;
        let had_prefix = self.checkpoint.received != 0;
        if had_prefix {
            // Publish the reset in the checkpoint before moving or truncating
            // bytes. A kill in either window then reopens as an empty transfer
            // and can safely discard any uncommitted old tail.
            let previous = self.checkpoint.clone();
            self.checkpoint.received = 0;
            self.checkpoint.validator = None;
            if reason.is_some() {
                self.checkpoint.expected_length = None;
            }
            if let Err(error) = self.persist_checkpoint() {
                self.checkpoint = previous;
                return Err(error);
            }
            if let Some(reason) = reason
                && reason.quarantine_prefix()
            {
                self.quarantine_prefix(reason)?;
            }
        }
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer is closed")))?;
        if let Err(error) = file
            .set_len(0)
            .and_then(|()| file.sync_data())
            .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
        {
            self.poison_for_reopen();
            return Err(error.into());
        }
        self.checkpoint.received = 0;
        self.checkpoint.validator = None;
        if reason.is_some() {
            // A validator change means this is a different representation;
            // its final extent must be learned from the new response.
            self.checkpoint.expected_length = None;
        }
        self.telemetry.resumed_bytes = 0;
        self.hasher = Some({
            let mut hasher = Hasher::new();
            hasher.update(OBJECT_DOMAIN);
            hasher
        });
        self.persist_checkpoint()
    }

    /// Refreshes the owner marker while a remote range request is in flight.
    /// The exclusive OS lock, rather than the timestamp, is the ownership
    /// fence and remains held until this transfer handle is dropped.
    pub fn renew(&mut self) -> Result<(), ContentStoreError> {
        self.check_owner()?;
        let expires = now_millis().saturating_add(TEMP_TTL.as_millis() as u64);
        write_transfer_owner(&mut self.owner_file, self.owner_token, expires)
    }

    /// Appends one bounded response body and persists its new range start.
    pub fn append<R: Read>(&mut self, source: R, maximum: u64) -> Result<u64, ContentStoreError> {
        self.append_with_checkpoint_writer(source, maximum, write_checkpoint)
    }

    fn append_with_checkpoint_writer<R, W>(
        &mut self,
        mut source: R,
        maximum: u64,
        mut checkpoint_writer: W,
    ) -> Result<u64, ContentStoreError>
    where
        R: Read,
        W: FnMut(&Path, &TransferCheckpoint) -> Result<(), ContentStoreError>,
    {
        self.check_owner()?;
        let mut buffer = [0_u8; CHUNK_BYTES];
        loop {
            self.check_owner()?;
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            let next = self
                .checkpoint
                .received
                .checked_add(read as u64)
                .ok_or(ContentStoreError::Bounds { maximum })?;
            if next > maximum
                || self
                    .checkpoint
                    .expected_length
                    .is_some_and(|length| next > length)
            {
                return Err(ContentStoreError::Bounds { maximum });
            }
            let previous = self.checkpoint.received;
            let write_result = {
                let file = self
                    .file
                    .as_mut()
                    .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer is closed")))?;
                let result = file
                    .write_all(&buffer[..read])
                    .and_then(|()| file.flush())
                    .and_then(|()| file.sync_data());
                if result.is_err() {
                    let rollback = file
                        .set_len(previous)
                        .and_then(|()| file.seek(SeekFrom::End(0)).map(|_| ()));
                    if let Err(error) = rollback {
                        return Err(error.into());
                    }
                }
                result
            };
            if let Err(error) = write_result {
                return Err(error.into());
            }
            self.checkpoint.received = next;
            let previous_checkpoint = TransferCheckpoint {
                received: previous,
                ..self.checkpoint.clone()
            };
            if let Err(error) = checkpoint_writer(&self.state_path, &self.checkpoint) {
                // The state rename may already be visible even when its
                // parent-directory flush fails. Keep the synced .part extent
                // intact and require reopen: recovery accepts either the old
                // checkpoint (discarding a tail) or the new checkpoint.
                self.checkpoint = previous_checkpoint;
                self.poison_for_reopen();
                return Err(error);
            }
            self.hasher
                .as_mut()
                .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer has no digest")))?
                .update(&buffer[..read]);
            self.telemetry.downloaded_bytes =
                self.telemetry.downloaded_bytes.saturating_add(read as u64);
        }
        Ok(self.checkpoint.received)
    }

    fn quarantine_prefix(&mut self, reason: TransferResetReason) -> Result<(), ContentStoreError> {
        validate_store_directory(self.store.root.as_ref(), &self.store.root.join("transfers"))?;
        validate_store_directory(
            self.store.root.as_ref(),
            &self.store.root.join("quarantine"),
        )?;
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        let destination = self.store.root.join("quarantine").join(format!(
            "{}.{}.prefix",
            hex(&self.owner_token),
            reason.code()
        ));
        fs::rename(&self.data_path, destination)?;
        sync_directory(&self.store.root.join("transfers"))?;
        sync_directory(&self.store.root.join("quarantine"))?;
        self.file = Some(
            backend_platform::durability::open_or_create_append_file_nofollow(&self.data_path)?,
        );
        Ok(())
    }

    /// Verifies and atomically publishes the completed transfer.
    pub fn finish(self) -> Result<ObjectAdmission, ContentStoreError> {
        self.finish_verified(|_path, _bytes, _object| Ok(()))
    }

    /// Verifies a completed transfer with an adapter before publishing it.
    ///
    /// This is the durable counterpart to
    /// [`ContentAddressedStore::admit_reader_verified`]. The transfer bytes
    /// remain private until the verifier accepts them, so a process crash or
    /// a rejected checksum cannot leave a reachable untrusted object.
    pub fn finish_verified<V>(mut self, verifier: V) -> Result<ObjectAdmission, ContentStoreError>
    where
        V: FnOnce(&Path, u64, RawArchiveObjectId) -> Result<(), ContentStoreError>,
    {
        self.check_owner()?;
        let received = self.checkpoint.received;
        if let Some(expected_length) = self.checkpoint.expected_length
            && received != expected_length
        {
            return Err(ContentStoreError::LengthMismatch {
                expected: expected_length,
                actual: received,
            });
        }
        let checkpoint_hash = *self
            .hasher
            .take()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer has no digest")))?
            .finalize()
            .as_bytes();
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }

        // Never publish the transfer inode: another stale descriptor may
        // still be writable even though this handle owns the transfer lock.
        // Copy into a fresh private inode, derive identity from those exact
        // bytes, and run the adapter verifier on that sealed copy.
        let mut sealed = self.store.create_temp("sealed-transfer")?;
        let mut source = open_content_file(self.store.root.as_ref(), &self.data_path)?;
        let (sealed_length, sealed_hash) = stream_to_temp(&mut source, &mut sealed, received)?;
        if sealed_length != received {
            return Err(ContentStoreError::LengthMismatch {
                expected: received,
                actual: sealed_length,
            });
        }
        let actual = object_from_hash(sealed_length, sealed_hash);
        if object_from_hash(received, checkpoint_hash) != actual {
            return Err(ContentStoreError::CorruptObject {
                path: self.data_path.clone(),
            });
        }
        if let Some(expected) = self.checkpoint.expected
            && expected != actual
        {
            drop(sealed);
            let quarantine = self.quarantine("digest-mismatch")?;
            return Err(ContentStoreError::DigestMismatch {
                expected,
                actual,
                quarantine: Some(quarantine),
            });
        }
        sealed.sync_close()?;
        if let Err(error) = verifier(sealed.path(), sealed_length, actual) {
            drop(sealed);
            let quarantine = self.quarantine("verification").ok();
            if matches!(error, ContentStoreError::VerificationRejected { .. }) {
                return Err(ContentStoreError::VerificationRejected { quarantine });
            }
            return Err(error);
        }
        self.check_owner()?;
        let result = self
            .store
            .publish_temp(&mut sealed, actual, sealed_length)?;

        // The checkpoint is removed and made durable before its data file is
        // removed. A crash can therefore leave an orphaned .part, but cannot
        // leave an acknowledged checkpoint referring to missing bytes.
        fs::remove_file(&self.state_path)?;
        sync_directory(&self.store.root.join("transfers"))?;
        fs::remove_file(&self.data_path)?;
        sync_directory(&self.store.root.join("transfers"))?;
        Ok(result)
    }

    fn quarantine(&mut self, reason: &str) -> Result<PathBuf, ContentStoreError> {
        validate_store_directory(self.store.root.as_ref(), &self.store.root.join("transfers"))?;
        validate_store_directory(
            self.store.root.as_ref(),
            &self.store.root.join("quarantine"),
        )?;
        self.file.take();
        let destination = self.store.root.join("quarantine").join(format!(
            "{}.{}.part",
            hex(&self.owner_token),
            reason
        ));
        match fs::remove_file(&self.state_path) {
            Ok(()) => sync_directory(&self.store.root.join("transfers"))?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        fs::rename(&self.data_path, &destination)?;
        sync_directory(&self.store.root.join("transfers"))?;
        sync_directory(&self.store.root.join("quarantine"))?;
        Ok(destination)
    }
}

impl Drop for ResumableTransfer {
    fn drop(&mut self) {
        let _ = self.file.take();
    }
}

fn acquire_transfer_owner(path: &Path, token: [u8; ID_BYTES]) -> Result<File, ContentStoreError> {
    // This inode is intentionally persistent. Unlinking a locked owner file
    // would let another opener create and lock a replacement inode while the
    // old owner still holds its descriptor.
    let mut file = backend_platform::durability::open_or_create_regular_file_nofollow(path)?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(ContentStoreError::TransferBusy),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    let expires = now_millis().saturating_add(TEMP_TTL.as_millis() as u64);
    write_transfer_owner(&mut file, token, expires)?;
    Ok(file)
}

fn transfer_owner_lock_path(root: &Path, id: TransferId) -> PathBuf {
    let stripe = usize::from(id.0[0]) % TRANSFER_OWNER_LOCK_STRIPES;
    root.join("transfers")
        .join(format!(".owner-lock-{stripe:03}"))
}

fn write_transfer_owner(
    file: &mut File,
    token: [u8; ID_BYTES],
    expires: u64,
) -> Result<(), ContentStoreError> {
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(&token)?;
    file.write_all(&expires.to_be_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CheckpointPoint {
    TemporarySynced,
    Renamed,
    BeforeParentSync,
    ParentSynced,
}

fn write_checkpoint(path: &Path, checkpoint: &TransferCheckpoint) -> Result<(), ContentStoreError> {
    write_checkpoint_with_hook(path, checkpoint, |_| Ok(()))
}

fn write_checkpoint_with_hook<F>(
    path: &Path,
    checkpoint: &TransferCheckpoint,
    mut hook: F,
) -> Result<(), ContentStoreError>
where
    F: FnMut(CheckpointPoint) -> io::Result<()>,
{
    let mut bytes = Vec::with_capacity(8 + ID_BYTES + 1 + ID_BYTES + 8 + 8 + 2 + 2 + 16_384);
    bytes.extend_from_slice(TRANSFER_MAGIC);
    bytes.extend_from_slice(&checkpoint.id.0);
    match checkpoint.expected {
        Some(object) => {
            bytes.push(1);
            bytes.extend_from_slice(object.as_bytes());
        }
        None => {
            bytes.push(0);
            bytes.extend_from_slice(&[0; ID_BYTES]);
        }
    }
    bytes.extend_from_slice(&checkpoint.expected_length.unwrap_or(u64::MAX).to_be_bytes());
    bytes.extend_from_slice(&checkpoint.received.to_be_bytes());
    for value in [
        checkpoint
            .validator
            .as_ref()
            .and_then(|validator| validator.etag()),
        checkpoint
            .validator
            .as_ref()
            .and_then(|validator| validator.last_modified()),
    ] {
        let value = value.unwrap_or_default().as_bytes();
        let length =
            u16::try_from(value.len()).map_err(|_| ContentStoreError::TransferStateMismatch)?;
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(value);
    }
    let temporary = path.with_extension("state.tmp");
    {
        let mut file =
            backend_platform::durability::open_or_truncate_regular_file_nofollow(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    hook(CheckpointPoint::TemporarySynced)?;
    fs::rename(temporary, path)?;
    hook(CheckpointPoint::Renamed)?;
    hook(CheckpointPoint::BeforeParentSync)?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    hook(CheckpointPoint::ParentSynced)?;
    Ok(())
}

fn read_checkpoint(path: &Path) -> Result<Option<TransferCheckpoint>, ContentStoreError> {
    let mut file = match backend_platform::durability::open_regular_file_nofollow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    const PREFIX: usize = 8 + ID_BYTES + 1 + ID_BYTES + 8 + 8;
    if bytes.len() < PREFIX || &bytes[..8] != TRANSFER_MAGIC {
        return Err(ContentStoreError::TransferStateMismatch);
    }
    let mut id = [0; ID_BYTES];
    id.copy_from_slice(&bytes[8..8 + ID_BYTES]);
    let marker = bytes[8 + ID_BYTES];
    if marker > 1 {
        return Err(ContentStoreError::TransferStateMismatch);
    }
    let mut object_bytes = [0; ID_BYTES];
    object_bytes.copy_from_slice(&bytes[9 + ID_BYTES..9 + ID_BYTES * 2]);
    let mut extent = [0; 8];
    extent.copy_from_slice(&bytes[9 + ID_BYTES * 2..9 + ID_BYTES * 2 + 8]);
    let mut received = [0; 8];
    received.copy_from_slice(&bytes[PREFIX - 8..PREFIX]);
    let mut at = PREFIX;
    let read_value =
        |bytes: &[u8], at: &mut usize| -> Result<Option<Box<str>>, ContentStoreError> {
            if bytes.len().saturating_sub(*at) < 2 {
                return Err(ContentStoreError::TransferStateMismatch);
            }
            let mut length = [0_u8; 2];
            length.copy_from_slice(&bytes[*at..*at + 2]);
            *at += 2;
            let length = usize::from(u16::from_be_bytes(length));
            let end = (*at)
                .checked_add(length)
                .ok_or(ContentStoreError::TransferStateMismatch)?;
            if end > bytes.len() {
                return Err(ContentStoreError::TransferStateMismatch);
            }
            let value = std::str::from_utf8(&bytes[*at..end])
                .map_err(|_| ContentStoreError::TransferStateMismatch)?;
            *at = end;
            Ok((!value.is_empty()).then(|| value.into()))
        };
    let etag = read_value(&bytes, &mut at)?;
    let last_modified = read_value(&bytes, &mut at)?;
    if at != bytes.len() {
        return Err(ContentStoreError::TransferStateMismatch);
    }
    let validator = TransferValidator::new(etag.as_deref(), last_modified.as_deref());
    if etag.is_some_and(|_| {
        validator
            .as_ref()
            .and_then(TransferValidator::etag)
            .is_none()
    }) || last_modified.is_some_and(|_| {
        validator
            .as_ref()
            .and_then(TransferValidator::last_modified)
            .is_none()
    }) {
        return Err(ContentStoreError::TransferStateMismatch);
    }
    Ok(Some(TransferCheckpoint {
        id: TransferId(id),
        expected: (marker == 1).then(|| RawArchiveObjectId::from_encoded(object_bytes)),
        expected_length: (u64::from_be_bytes(extent) != u64::MAX)
            .then_some(u64::from_be_bytes(extent)),
        received: u64::from_be_bytes(received),
        validator,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acquisition::SourceSnapshot;
    use std::{
        io::Write,
        sync::{
            Arc, Barrier,
            atomic::{AtomicUsize, Ordering},
        },
    };
    #[cfg(unix)]
    use std::{
        io::{BufRead, BufReader, Read},
        process::{Command, Stdio},
    };

    fn root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "nudox-content-addressed-{label}-{}",
            std::process::id()
        ))
    }

    fn clean(path: &Path) {
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn no_op_expected_object_does_not_touch_source() {
        let path = root("reuse");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let bytes = b"immutable bytes";
        let object = store
            .admit_reader(None, &bytes[..], 1024)
            .expect("publish")
            .object();
        struct PanickingReader;
        impl Read for PanickingReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("a reused object must not read its source")
            }
        }
        let reused = store
            .admit_reader(Some(object), PanickingReader, 1024)
            .expect("reuse");
        assert_eq!(
            reused,
            ObjectAdmission::Reused {
                object,
                bytes: bytes.len() as u64
            }
        );
        let reopened = ContentAddressedStore::open(&path).expect("reopen");
        let reused_after_restart = reopened
            .admit_reader(Some(object), PanickingReader, 1024)
            .expect("reuse after restart");
        assert_eq!(reused_after_restart, reused);
        clean(&path);
    }

    #[test]
    fn thirty_two_concurrent_writers_publish_one_object() {
        let path = root("writers");
        clean(&path);
        let store = Arc::new(ContentAddressedStore::open(&path).expect("store"));
        let barrier = Arc::new(Barrier::new(32));
        let effects = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..32 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let effects = Arc::clone(&effects);
            threads.push(std::thread::spawn(move || {
                barrier.wait();
                effects.fetch_add(1, Ordering::Relaxed);
                store
                    .admit_reader(None, &b"same bytes"[..], 1024)
                    .expect("admit")
            }));
        }
        let admissions: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().expect("join"))
            .collect();
        assert_eq!(effects.load(Ordering::Relaxed), 32);
        assert_eq!(
            admissions
                .iter()
                .filter(|admission| admission.was_published())
                .count(),
            1
        );
        assert!(admissions.iter().all(|admission| admission.bytes() == 10));
        clean(&path);
    }

    #[test]
    fn staged_publication_syncs_the_file_then_links_then_syncs_its_parent() {
        let path = root("staged-publication-order");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let bytes = b"ordered staged publication";
        let source = path.join("temps/source.part");
        let mut file = File::create(&source).expect("source");
        file.write_all(bytes).expect("source bytes");
        file.sync_all().expect("sync source");
        drop(file);
        let object = RawArchiveObjectId::from_bytes(bytes);
        let mut points = Vec::new();

        let admission = store
            .publish_file_with_hook(&source, object, bytes.len() as u64, true, |point| {
                points.push(point);
                Ok(())
            })
            .expect("staged publication");

        assert_eq!(
            points,
            [
                PublishPoint::StageCreated,
                PublishPoint::StageCopyProgress,
                PublishPoint::StageWritten,
                PublishPoint::StageSynced,
                PublishPoint::ObjectLinked,
                PublishPoint::BeforeDirectorySync,
                PublishPoint::DirectorySynced,
            ]
        );
        assert_eq!(
            admission,
            ObjectAdmission::Published {
                object,
                bytes: bytes.len() as u64,
            }
        );
        let reopened = ContentAddressedStore::open(&path).expect("cold reopen");
        assert_eq!(
            reopened
                .verify_object(object, bytes.len() as u64)
                .expect("verify"),
            bytes.len() as u64
        );
        clean(&path);
    }

    #[test]
    fn direct_publication_syncs_the_parent_before_returning_winner() {
        let path = root("direct-publication-order");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let bytes = b"direct hard-link publication";
        let source = path.join("temps/source.part");
        let mut file = File::create(&source).expect("source");
        file.write_all(bytes).expect("source bytes");
        file.sync_all().expect("sync source");
        drop(file);
        let object = RawArchiveObjectId::from_bytes(bytes);
        let mut points = Vec::new();

        let admission = store
            .publish_file_with_hook(&source, object, bytes.len() as u64, false, |point| {
                points.push(point);
                Ok(())
            })
            .expect("direct publication");

        assert_eq!(
            points,
            [
                PublishPoint::ObjectLinked,
                PublishPoint::BeforeDirectorySync,
                PublishPoint::DirectorySynced,
            ]
        );
        assert!(admission.was_published());
        let reopened = ContentAddressedStore::open(&path).expect("cold reopen");
        assert_eq!(
            reopened
                .verify_object(object, bytes.len() as u64)
                .expect("verify"),
            bytes.len() as u64
        );
        clean(&path);
    }

    #[test]
    // These hook failures unwind normally and exercise RAII/retry behavior;
    // the subprocess test below covers a hard process exit.
    fn injected_staging_faults_unwind_without_publishing_partial_bytes() {
        let failure_points = [
            PublishPoint::StageCreated,
            PublishPoint::StageCopyProgress,
            PublishPoint::StageWritten,
            PublishPoint::StageSynced,
            PublishPoint::ObjectLinked,
            PublishPoint::BeforeDirectorySync,
            PublishPoint::DirectorySynced,
        ];
        let bytes = vec![0x6b; CHUNK_BYTES * 2 + 17];
        let object = RawArchiveObjectId::from_bytes(&bytes);

        for (index, fail_at) in failure_points.into_iter().enumerate() {
            let path = root(&format!("staged-interruption-{index}"));
            clean(&path);
            let store = ContentAddressedStore::open(&path).expect("store");
            let source = path.join("temps/source.part");
            let mut file = File::create(&source).expect("source");
            file.write_all(&bytes).expect("source bytes");
            file.sync_all().expect("sync source");
            drop(file);
            let mut points = Vec::new();

            let error = store
                .publish_file_with_hook(&source, object, bytes.len() as u64, true, |point| {
                    points.push(point);
                    if point == fail_at {
                        Err(io::Error::other("simulated interruption"))
                    } else {
                        Ok(())
                    }
                })
                .expect_err("injected interruption");
            assert!(matches!(error, ContentStoreError::Io(_)));
            assert_eq!(points.last(), Some(&fail_at));

            let target = store.object_path(object);
            assert!(
                fs::read_dir(target.parent().expect("target parent"))
                    .expect("read object parent")
                    .all(|entry| entry
                        .expect("object entry")
                        .path()
                        .extension()
                        .and_then(|extension| extension.to_str())
                        != Some("stage")),
                "staging file was cleaned at {fail_at:?}"
            );
            let linked_before_directory_sync = matches!(
                fail_at,
                PublishPoint::ObjectLinked
                    | PublishPoint::BeforeDirectorySync
                    | PublishPoint::DirectorySynced
            );
            assert_eq!(target.exists(), linked_before_directory_sync);

            drop(store);
            let reopened = ContentAddressedStore::open(&path).expect("cold reopen");
            if linked_before_directory_sync {
                assert_eq!(
                    reopened
                        .verify_object(object, bytes.len() as u64)
                        .expect("complete linked object survives"),
                    bytes.len() as u64
                );
                struct Unreadable;
                impl Read for Unreadable {
                    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                        panic!("an existing object must be verified and reused")
                    }
                }
                assert!(matches!(
                    reopened.admit_reader(Some(object), Unreadable, bytes.len() as u64),
                    Ok(ObjectAdmission::Reused { .. })
                ));
            } else {
                assert!(matches!(
                    reopened.admit_reader(Some(object), &bytes[..], bytes.len() as u64),
                    Ok(ObjectAdmission::Published { .. })
                ));
                assert_eq!(
                    reopened
                        .verify_object(object, bytes.len() as u64)
                        .expect("retry publishes exact bytes"),
                    bytes.len() as u64
                );
            }
            clean(&path);
        }
    }

    #[test]
    fn cold_reopen_rejects_a_partial_existing_object() {
        let path = root("partial-object");
        clean(&path);
        let bytes = b"the complete immutable object";
        let object = RawArchiveObjectId::from_bytes(bytes);
        let store = ContentAddressedStore::open(&path).expect("store");
        let target = store.object_path(object);
        fs::create_dir_all(target.parent().expect("target parent")).expect("object parent");
        fs::write(&target, &bytes[..7]).expect("partial final target");
        drop(store);

        let reopened = ContentAddressedStore::open(&path).expect("cold reopen");
        struct Unreadable;
        impl Read for Unreadable {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("a corrupt existing object must fail verification before source read")
            }
        }
        assert!(matches!(
            reopened.admit_reader(Some(object), Unreadable, bytes.len() as u64),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        clean(&path);
    }

    #[cfg(unix)]
    #[test]
    fn cas_reads_reject_symlink_targets() {
        use std::os::unix::fs::symlink;

        let path = root("symlink-object");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let bytes = b"matching external mutable bytes";
        let object = RawArchiveObjectId::from_bytes(bytes);
        let target = store.object_path(object);
        fs::create_dir_all(target.parent().expect("target parent")).expect("object parent");
        let external = path.join("outside-object");
        fs::write(&external, bytes).expect("external bytes");
        symlink(external, &target).expect("symlink object target");

        assert!(matches!(
            store.object_len(object),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        assert!(matches!(
            store.verify_object(object, 1024),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        assert!(matches!(
            store.open_object(object),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        clean(&path);
    }

    #[cfg(unix)]
    #[test]
    fn cas_reads_and_admission_reject_symlinked_fanout_parent() {
        use std::os::unix::fs::symlink;

        let path = root("symlink-fanout");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let bytes = b"bytes hidden behind a fanout link";
        let object = RawArchiveObjectId::from_bytes(bytes);
        let encoded = hex(object.as_bytes());
        let outside = path.join("outside-objects");
        let outside_bucket = outside.join(&encoded[..2]);
        fs::create_dir_all(&outside_bucket).expect("external bucket");
        fs::write(outside_bucket.join(&encoded[2..]), bytes).expect("matching external object");

        let objects = path.join("objects");
        fs::rename(&objects, path.join("objects-original")).expect("move real object tree");
        symlink(&outside, &objects).expect("link object tree outside store");

        assert!(matches!(
            store.object_len(object),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        assert!(matches!(
            store.verify_object(object, 1024),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        assert!(matches!(
            store.open_object(object),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        struct Unreadable;
        impl Read for Unreadable {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                panic!("a symlinked parent must be rejected before source reads")
            }
        }
        assert!(matches!(
            store.admit_reader(Some(object), Unreadable, 1024),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        assert!(matches!(
            store.admit_reader(None, &bytes[..], 1024),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        assert_eq!(
            fs::read(outside_bucket.join(&encoded[2..])).expect("external bytes"),
            bytes
        );
        clean(&path);
    }

    #[cfg(unix)]
    #[test]
    fn object_admission_does_not_create_through_a_symlinked_bucket() {
        use std::os::unix::fs::symlink;

        let path = root("symlink-object-bucket");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let bytes = b"never write through a fanout link";
        let object = RawArchiveObjectId::from_bytes(bytes);
        let encoded = hex(object.as_bytes());
        let bucket = path.join("objects").join(&encoded[..2]);
        let external_bucket = path.join("outside-bucket");
        fs::create_dir_all(&external_bucket).expect("external bucket");
        symlink(&external_bucket, &bucket).expect("symlink bucket");

        assert!(matches!(
            store.admit_reader(None, &bytes[..], 1024),
            Err(ContentStoreError::CorruptObject { .. })
        ));
        assert!(
            fs::read_dir(&external_bucket)
                .expect("read external bucket")
                .next()
                .is_none()
        );
        clean(&path);
    }

    #[test]
    fn transfer_owner_lock_fences_expiry_stale_renew_and_stale_part_writes() {
        let path = root("transfer-owner-fence");
        clean(&path);
        let store_a = ContentAddressedStore::open(&path).expect("store A");
        let store_b = ContentAddressedStore::open(&path).expect("store B");
        let bytes = b"hello world";
        let object = RawArchiveObjectId::from_bytes(bytes);
        let id = TransferId::from_parts(b"https://example/fenced", Some(object), Some(11));
        let mut stale = store_a
            .resume_or_start(id, Some(object), Some(11))
            .expect("first owner");
        stale.append(&b"hello"[..], 11).expect("first range");
        let owner_path = transfer_owner_lock_path(&path, id);

        // Expiry is informational; a held OS lock still blocks a second open.
        write_transfer_owner(&mut stale.owner_file, stale.owner_token, 0)
            .expect("force marker expiry");
        assert!(matches!(
            store_b.resume_or_start(id, Some(object), Some(11)),
            Err(ContentStoreError::TransferBusy)
        ));

        // Simulate a stale owner after its lock was lost. The new token must
        // fence both stale renewal and Drop, while its old writable .part fd
        // remains open across publication.
        stale
            .owner_file
            .unlock()
            .expect("simulate lost ownership lock");
        let mut current = store_b
            .resume_or_start(id, Some(object), Some(11))
            .expect("replacement owner");
        let current_token = current.owner_token;
        assert!(matches!(
            stale.renew(),
            Err(ContentStoreError::TransferBusy)
        ));
        let marker_before_stale_drop = fs::read(&owner_path).expect("current owner marker");
        assert_eq!(
            marker_before_stale_drop.get(..ID_BYTES),
            Some(current_token.as_slice())
        );

        current.append(&b" world"[..], 11).expect("complete range");
        let admission = current.finish().expect("publish sealed inode");
        assert_eq!(admission.object(), object);

        let stale_file = stale.file.as_mut().expect("stale writable part descriptor");
        stale_file.set_len(0).expect("truncate unlinked old part");
        stale_file
            .write_all(b"changed after publication")
            .expect("mutate stale part descriptor");
        stale_file.sync_all().expect("sync stale part mutation");
        drop(stale);

        assert_eq!(
            fs::read(&owner_path).expect("owner marker remains"),
            marker_before_stale_drop
        );
        assert_eq!(
            store_b
                .verify_object(object, 1024)
                .expect("published bytes remain immutable"),
            bytes.len() as u64
        );
        assert_eq!(
            store_b
                .open_object(object)
                .expect("open object")
                .metadata()
                .expect("metadata")
                .len(),
            bytes.len() as u64
        );
        clean(&path);
    }

    #[test]
    fn transfer_ids_on_one_owner_stripe_serialize_with_bounded_markers() {
        let path = root("owner-lock-stripes");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let mut first_bytes = [0_u8; ID_BYTES];
        first_bytes[0] = 19;
        let mut second_bytes = first_bytes;
        second_bytes[1] = 1;
        let first_id = TransferId(first_bytes);
        let second_id = TransferId(second_bytes);
        assert_eq!(
            transfer_owner_lock_path(&path, first_id),
            transfer_owner_lock_path(&path, second_id)
        );

        let first = store
            .resume_or_start(first_id, None, Some(0))
            .expect("first striped owner");
        assert!(matches!(
            store.resume_or_start(second_id, None, Some(0)),
            Err(ContentStoreError::TransferBusy)
        ));
        drop(first);
        let second = store
            .resume_or_start(second_id, None, Some(0))
            .expect("second striped owner after release");
        drop(second);

        let marker_count = fs::read_dir(path.join("transfers"))
            .expect("transfers")
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(".owner-lock-"))
            })
            .count();
        assert_eq!(marker_count, 1, "only one persistent stripe file is needed");
        clean(&path);
    }

    #[test]
    fn checkpoint_rename_is_followed_by_transfer_directory_sync() {
        let path = root("checkpoint-order");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let id = TransferId::from_parts(b"checkpoint-order", None, Some(0));
        let checkpoint = TransferCheckpoint {
            id,
            expected: None,
            expected_length: Some(0),
            received: 0,
            validator: None,
        };
        let state_path = store
            .root
            .join("transfers")
            .join(format!("{}.state", hex(&id.0)));
        let mut points = Vec::new();
        write_checkpoint_with_hook(&state_path, &checkpoint, |point| {
            points.push(point);
            Ok(())
        })
        .expect("write checkpoint");
        assert_eq!(
            points,
            [
                CheckpointPoint::TemporarySynced,
                CheckpointPoint::Renamed,
                CheckpointPoint::BeforeParentSync,
                CheckpointPoint::ParentSynced,
            ]
        );
        drop(store);
        assert_eq!(
            read_checkpoint(&state_path).expect("cold checkpoint"),
            Some(checkpoint)
        );
        clean(&path);
    }

    #[test]
    fn checkpoint_parent_sync_error_preserves_bytes_for_cold_retry() {
        let path = root("checkpoint-sync-error");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let bytes = b"hello world";
        let object = RawArchiveObjectId::from_bytes(bytes);
        let id = TransferId::from_parts(b"checkpoint-sync-error", Some(object), Some(11));
        let mut transfer = store
            .resume_or_start(id, Some(object), Some(11))
            .expect("start transfer");
        let part = path.join("transfers").join(format!("{}.part", hex(&id.0)));

        // This is an ordinary returned I/O failure after rename, not a
        // process/power-loss simulation. It exercises the ambiguous-commit
        // path and verifies that rollback does not truncate checkpoint bytes.
        let error = transfer
            .append_with_checkpoint_writer(&b"hello"[..], 11, |state_path, checkpoint| {
                write_checkpoint_with_hook(state_path, checkpoint, |point| {
                    if point == CheckpointPoint::BeforeParentSync {
                        Err(io::Error::other("injected parent directory sync failure"))
                    } else {
                        Ok(())
                    }
                })
            })
            .expect_err("injected checkpoint barrier failure");
        assert!(matches!(error, ContentStoreError::Io(_)));
        assert_eq!(fs::metadata(&part).expect("synced part remains").len(), 5);
        assert!(matches!(
            transfer.append(&b" world"[..], 11),
            Err(ContentStoreError::TransferNeedsReopen)
        ));
        drop(transfer);

        let reopened = ContentAddressedStore::open(&path).expect("cold reopen");
        let mut retry = reopened
            .resume_or_start(id, Some(object), Some(11))
            .expect("resume after ambiguous checkpoint");
        assert_eq!(retry.resume_offset(), 5);
        retry.append(&b" world"[..], 11).expect("retry suffix");
        assert_eq!(retry.finish().expect("publish retry").object(), object);
        assert_eq!(
            reopened.verify_object(object, 1024).expect("verify retry"),
            bytes.len() as u64
        );
        clean(&path);
    }

    #[test]
    fn digest_mismatch_is_quarantined_and_never_published() {
        let path = root("quarantine");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let expected = RawArchiveObjectId::from_bytes(b"expected");
        let error = store
            .admit_reader(Some(expected), &b"actual"[..], 1024)
            .expect_err("mismatch");
        let ContentStoreError::DigestMismatch { quarantine, .. } = error else {
            panic!("wrong error")
        };
        assert!(quarantine.is_some_and(|path| path.exists()));
        assert!(!store.object_path(expected).exists());
        clean(&path);
    }

    #[test]
    fn adapter_rejection_is_quarantined_before_publication() {
        let path = root("verified-rejection");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let error = store
            .admit_reader_verified(None, &b"untrusted"[..], 1024, |_path, _bytes, _object| {
                Err(ContentStoreError::VerificationRejected { quarantine: None })
            })
            .expect_err("adapter rejection");
        let ContentStoreError::VerificationRejected { quarantine } = error else {
            panic!("wrong error")
        };
        assert!(quarantine.is_some_and(|path| path.exists()));
        assert_eq!(
            fs::read_dir(store.root().join("objects"))
                .expect("objects")
                .count(),
            0
        );
        clean(&path);
    }

    #[test]
    fn active_temp_survives_stale_scavenge() {
        let path = root("scavenge");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let mut temp = store.create_temp("active").expect("temp");
        temp.write_all(b"active").expect("write");
        let path = temp.path().to_owned();
        assert_eq!(store.scavenge_stale(Duration::ZERO).expect("scavenge"), 0);
        assert!(path.exists());
        drop(temp);
        clean(
            &path
                .parent()
                .and_then(Path::parent)
                .unwrap_or_else(|| Path::new("/")),
        );
    }

    #[test]
    fn second_store_cannot_scavenge_a_temp_held_during_long_verification() {
        let path = root("temp-cross-store-lock");
        clean(&path);
        let store_a = ContentAddressedStore::open(&path).expect("store A");
        let store_b = ContentAddressedStore::open(&path).expect("store B");
        let mut active = store_a.create_temp("long-verifier").expect("active temp");
        active.write_all(b"sealed bytes").expect("write temp");
        active.sync_close().expect("close writer before verifier");
        File::open(active.path())
            .expect("open temp for aging")
            .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH))
            .expect("age temp");
        let mut lease = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&active.lease_path)
            .expect("open lease marker");
        lease.write_all(&active.token).expect("write lease token");
        lease.write_all(&0_u64.to_be_bytes()).expect("expire lease");
        lease.sync_all().expect("sync expired lease");

        assert_eq!(
            store_b
                .scavenge_stale(Duration::ZERO)
                .expect("live scavenge"),
            0
        );
        assert!(active.path().exists(), "cross-process lock protects temp");

        // Releasing the lock models a crashed verifier; a second store can
        // then collect the aged orphan even while this test retains metadata.
        active._lock.unlock().expect("release temp lock");
        assert_eq!(
            store_b
                .scavenge_stale(Duration::ZERO)
                .expect("orphan scavenge"),
            1
        );
        assert!(!active.path().exists());
        drop(active);
        clean(&path);
    }

    #[test]
    fn stale_destination_stages_are_scavenged_without_removing_live_stages() {
        let path = root("stage-scavenge");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let object = RawArchiveObjectId::from_bytes(b"staged object");
        let target = store.object_path(object);
        let parent = target.parent().expect("target parent");
        fs::create_dir_all(parent).expect("object parent");
        let mut active = DestinationStage::create(&store, parent, &target).expect("active stage");
        active
            .file_mut()
            .expect("active stage file")
            .write_all(b"live partial stage")
            .expect("write active stage");

        let target_name = target
            .file_name()
            .and_then(|name| name.to_str())
            .expect("target name");
        let stale_path = parent.join(format!(".{target_name}.{}.stage", "0".repeat(ID_BYTES * 2)));
        fs::write(&stale_path, b"crash-left stage").expect("stale stage");
        File::open(&stale_path)
            .expect("open stale stage")
            .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH))
            .expect("age stale stage");

        let other_store = ContentAddressedStore::open(&path).expect("second store handle");
        assert_eq!(
            other_store
                .scavenge_stale(Duration::ZERO)
                .expect("scavenge"),
            1
        );
        assert!(active.path().exists(), "live stage remains protected");
        assert!(!stale_path.exists(), "orphaned stage is removed");
        drop(active);
        clean(&path);
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_stage_child() {
        let Ok(root) = std::env::var("NUDOX_SIGKILL_STAGE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let store = ContentAddressedStore::open(&root).expect("child store");
        let bytes = b"destination stage held through verification";
        let source = root.join("temps/stage-child-source.part");
        let mut source_file = File::create(&source).expect("child source");
        source_file.write_all(bytes).expect("write child source");
        source_file.sync_all().expect("sync child source");
        drop(source_file);
        let object = RawArchiveObjectId::from_bytes(bytes);
        let _ = store.publish_file_with_hook(&source, object, bytes.len() as u64, true, |point| {
            if point == PublishPoint::StageSynced {
                println!("ready");
                io::stdout().flush().expect("flush child readiness");
                let mut input = Vec::new();
                let _ = io::stdin().read_to_end(&mut input);
            }
            Ok(())
        });
    }

    #[cfg(unix)]
    #[test]
    fn hard_exit_releases_stage_lock_for_cold_scavenging() {
        let path = root("stage-sigkill");
        clean(&path);
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "acquisition::content_addressed::tests::sigkill_stage_child",
                "--nocapture",
            ])
            .env("NUDOX_SIGKILL_STAGE_ROOT", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn stage child");
        let stdout = child.stdout.take().expect("child stdout");
        let mut lines = BufReader::new(stdout).lines();
        let mut ready = false;
        while let Some(line) = lines.next() {
            if line.expect("read child output") == "ready" {
                ready = true;
                break;
            }
        }
        assert!(ready, "child did not hold a synced stage");

        let store = ContentAddressedStore::open(&path).expect("independent store handle");
        let object = RawArchiveObjectId::from_bytes(b"destination stage held through verification");
        let target = store.object_path(object);
        let parent = target.parent().expect("object parent");
        let stage = fs::read_dir(parent)
            .expect("object entries")
            .map(|entry| entry.expect("object entry").path())
            .find(|path| is_destination_stage(path))
            .expect("child stage");
        File::open(&stage)
            .expect("open stage for aging")
            .set_times(std::fs::FileTimes::new().set_modified(UNIX_EPOCH))
            .expect("age live stage");
        assert_eq!(
            store.scavenge_stale(Duration::ZERO).expect("live scavenge"),
            0
        );
        assert!(
            stage.exists(),
            "scavenger must respect another process lock"
        );

        child.kill().expect("kill child");
        let status = child.wait().expect("wait for child");
        assert!(!status.success(), "child unexpectedly exited cleanly");
        assert_eq!(
            store
                .scavenge_stale(Duration::ZERO)
                .expect("orphan scavenge"),
            1
        );
        assert!(!stage.exists(), "hard-exit stage can be scavenged");
        clean(&path);
    }

    #[test]
    fn interrupted_transfer_reopens_at_last_checkpoint() {
        let path = root("resume");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let id = TransferId::from_parts(b"https://example/archive", None, Some(11));
        let mut transfer = store.resume_or_start(id, None, Some(11)).expect("start");
        transfer.append(&b"hello"[..], 11).expect("first range");
        assert_eq!(transfer.resume_offset(), 5);
        drop(transfer);
        let mut resumed = store.resume_or_start(id, None, Some(11)).expect("resume");
        assert_eq!(resumed.resume_offset(), 5);
        resumed.append(&b" world"[..], 11).expect("second range");
        let result = resumed.finish().expect("finish");
        assert!(result.was_published());
        assert_eq!(
            store.verify_object(result.object(), 1024).expect("verify"),
            11
        );
        clean(&path);
    }

    #[test]
    fn interrupted_transfer_discards_uncheckpointed_tail_on_reopen() {
        let path = root("resume-crash-tail");
        clean(&path);
        let store = ContentAddressedStore::open(&path).expect("store");
        let id = TransferId::from_parts(b"https://example/crash-tail", None, Some(11));
        let mut transfer = store.resume_or_start(id, None, Some(11)).expect("start");
        transfer
            .append(&b"hello"[..], 11)
            .expect("checkpoint prefix");
        drop(transfer);

        let part = path.join("transfers").join(format!("{}.part", hex(&id.0)));
        let mut file = OpenOptions::new()
            .append(true)
            .open(&part)
            .expect("open simulated crash tail");
        file.write_all(b"uncheckpointed").expect("write crash tail");
        file.sync_all().expect("sync crash tail");
        drop(file);

        let mut resumed = store.resume_or_start(id, None, Some(11)).expect("reopen");
        assert_eq!(resumed.resume_offset(), 5);
        assert_eq!(fs::metadata(&part).expect("part metadata").len(), 5);
        resumed.append(&b" world"[..], 11).expect("finish suffix");
        assert_eq!(resumed.finish().expect("finish").bytes(), 11);
        clean(&path);
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_transfer_child() {
        let Ok(root) = std::env::var("NUDOX_SIGKILL_TRANSFER_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let store = ContentAddressedStore::open(&root).expect("child store");
        let id = TransferId::from_parts(b"https://example/sigkill", None, Some(11));
        let mut transfer = store
            .resume_or_start(id, None, Some(11))
            .expect("child start");
        transfer
            .append(&b"hello"[..], 11)
            .expect("child checkpoint prefix");
        let part = root.join("transfers").join(format!("{}.part", hex(&id.0)));
        let mut file = OpenOptions::new()
            .append(true)
            .open(&part)
            .expect("child open part");
        file.write_all(b"uncheckpointed").expect("child write tail");
        file.sync_all().expect("child sync tail");
        drop(file);
        // The persistent stripe marker is intentionally left intact. A hard
        // process exit releases its OS lock and permits the parent to resume.
        println!("ready");
        io::stdout().flush().expect("flush child readiness");
        let mut input = Vec::new();
        let _ = io::stdin().read_to_end(&mut input);
    }

    #[cfg(unix)]
    #[test]
    fn sigkill_transfer_reopens_from_last_durable_checkpoint() {
        let path = root("resume-sigkill");
        clean(&path);
        let mut child = Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "acquisition::content_addressed::tests::sigkill_transfer_child",
                "--nocapture",
            ])
            .env("NUDOX_SIGKILL_TRANSFER_ROOT", &path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sigkill child");
        let stdout = child.stdout.take().expect("child stdout");
        let mut lines = BufReader::new(stdout).lines();
        let mut ready = false;
        while let Some(line) = lines.next() {
            if line.expect("read child output") == "ready" {
                ready = true;
                break;
            }
        }
        assert!(ready, "child did not reach durable interruption point");
        child.kill().expect("kill child");
        let status = child.wait().expect("wait child");
        assert!(!status.success(), "child unexpectedly exited cleanly");

        let store = ContentAddressedStore::open(&path).expect("reopen after sigkill");
        let id = TransferId::from_parts(b"https://example/sigkill", None, Some(11));
        let part = path.join("transfers").join(format!("{}.part", hex(&id.0)));
        let mut resumed = store
            .resume_or_start(id, None, Some(11))
            .expect("resume after kill");
        assert_eq!(resumed.resume_offset(), 5);
        assert_eq!(fs::metadata(part).expect("part metadata").len(), 5);
        resumed
            .append(&b" world"[..], 11)
            .expect("finish after kill");
        assert_eq!(resumed.finish().expect("publish after kill").bytes(), 11);
        clean(&path);
    }

    #[test]
    fn interrupted_transfer_reopens_with_validator_at_multiple_offsets() {
        for (index, offset) in [1_u64, 65_535, 65_536, 131_073].into_iter().enumerate() {
            let path = root(&format!("resume-validator-{index}"));
            clean(&path);
            let store = ContentAddressedStore::open(&path).expect("store");
            let bytes = vec![0x4d; 192 * 1024];
            let id = TransferId::from_parts(b"https://example/archive", None, None);
            let validator = TransferValidator::new(Some("\"stable-v1\""), None).expect("validator");
            let mut transfer = store.resume_or_start(id, None, None).expect("start");
            transfer
                .set_validator(Some(validator.clone()))
                .expect("persist validator");
            transfer
                .bind_expected_length(bytes.len() as u64)
                .expect("persist extent");
            transfer
                .append(&bytes[..offset as usize], bytes.len() as u64)
                .expect("append prefix");
            drop(transfer);

            let mut resumed = store.resume_or_start(id, None, None).expect("reopen");
            assert_eq!(resumed.resume_offset(), offset);
            assert_eq!(resumed.validator(), Some(&validator));
            assert_eq!(resumed.telemetry().resumed_bytes, offset);
            resumed
                .append(&bytes[offset as usize..], bytes.len() as u64)
                .expect("append suffix");
            assert_eq!(
                resumed.telemetry().downloaded_bytes,
                (bytes.len() - offset as usize) as u64
            );
            let result = resumed.finish().expect("finish");
            assert_eq!(result.bytes(), bytes.len() as u64);
            clean(&path);
        }
    }

    #[test]
    fn transfer_validator_persists_etag_and_last_modified_rules() {
        let strong =
            TransferValidator::new(Some("\"strong\""), Some("Wed, 21 Oct 2015 07:28:00 GMT"))
                .expect("strong validator");
        assert_eq!(strong.etag(), Some("\"strong\""));
        assert_eq!(
            strong.last_modified(),
            Some("Wed, 21 Oct 2015 07:28:00 GMT")
        );
        assert_eq!(strong.if_range(), Some("\"strong\""));

        let weak =
            TransferValidator::new(Some("W/\"weak\""), Some("Wed, 21 Oct 2015 07:28:00 GMT"))
                .expect("weak validator");
        assert_eq!(weak.if_range(), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert!(
            TransferValidator::new(Some("W/\"weak\""), None)
                .expect("weak-only validator")
                .if_range()
                .is_none()
        );
        let empty = TransferValidator::new(Some("  "), Some(""));
        assert!(empty.is_none());
        let too_long = "x".repeat(MAX_VALIDATOR_BYTES + 1);
        assert!(TransferValidator::new(Some(&too_long), None).is_none());
        let control = TransferValidator::new(Some("\"ok\""), Some("Wed\n21 Oct"))
            .expect("valid etag survives invalid date");
        assert_eq!(control.last_modified(), None);
        let malformed_date = TransferValidator::new(Some("\"ok\""), Some("not-a-date"))
            .expect("valid etag survives malformed date");
        assert_eq!(malformed_date.last_modified(), None);
    }

    #[test]
    fn manifest_rejects_traversal_and_produces_one_file_delta_shape() {
        let mut builder = ArchiveManifestBuilder::new(ArchiveBudget::default());
        assert!(matches!(
            builder.push_file("../escape", RawArchiveObjectId::from_bytes(b"x"), 1, 0),
            Err(ContentStoreError::InvalidArchivePath)
        ));
        builder
            .push_file(
                "src/lib.rs",
                RawArchiveObjectId::from_bytes(b"before"),
                6,
                0,
            )
            .expect("base row");
        let manifest = builder.finish().expect("manifest");
        assert_eq!(manifest.entries().len(), 1);
        assert_eq!(manifest.bytes(), 6);
    }

    #[test]
    fn local_directory_and_archive_rows_share_tree_and_snapshot_identity() {
        let source_root = root("tree-source");
        let store_root = root("tree-store");
        clean(&source_root);
        clean(&store_root);
        fs::create_dir_all(source_root.join("src")).expect("source directory");
        let mut main = File::create(source_root.join("src/main.rs")).expect("source file");
        main.write_all(b"fn main() {}\n").expect("source bytes");
        fs::write(source_root.join("README.md"), b"# sample\n").expect("readme");

        let store = ContentAddressedStore::open(&store_root).expect("store");
        let local = store
            .admit_directory(&source_root, ArchiveBudget::default())
            .expect("local admission");
        // The archive row extent is supplied by its archive metadata in the
        // real adapter. Rebuild the rows with the local file lengths so only
        // the canonical object identities determine the tree root.
        let mut archive = ArchiveManifestBuilder::new(ArchiveBudget::default());
        for entry in local.entries() {
            let bytes = fs::metadata(source_root.join(entry.path.as_ref()))
                .expect("source metadata")
                .len();
            archive
                .push_file(Arc::clone(&entry.path), entry.object, bytes, entry.mode)
                .expect("archive row");
        }
        let archive = archive.finish().expect("archive admission");
        assert_eq!(local.id(), archive.id());
        assert_eq!(local.entries(), archive.entries());

        let source = [7; ID_BYTES];
        let cursor = [9; ID_BYTES];
        let local_snapshot = SourceSnapshot::new(source, cursor, 1, local.tree_arc(), Vec::new())
            .expect("local snapshot");
        let archive_snapshot =
            SourceSnapshot::new(source, cursor, 1, archive.tree_arc(), Vec::new())
                .expect("archive snapshot");
        assert_eq!(local_snapshot.id(), archive_snapshot.id());
        clean(&source_root);
        clean(&store_root);
    }
}

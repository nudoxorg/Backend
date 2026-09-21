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
    /// A resumable response was authenticated by a different representation
    /// validator than the bytes already persisted for the transfer.
    TransferValidatorChanged,
    /// Another process or thread owns the transfer lease.
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
    active_temps: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl ContentAddressedStore {
    /// Opens or creates an object root. Existing published digest directories
    /// are never removed or recreated by this call.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, ContentStoreError> {
        let root = root.into();
        for directory in ["objects", "temps", "transfers", "quarantine"] {
            fs::create_dir_all(root.join(directory))?;
        }
        Ok(Self {
            root: Arc::new(root),
            nonce: Arc::new(Mutex::new(now_millis() ^ u64::from(std::process::id()))),
            active_temps: Arc::new(Mutex::new(BTreeSet::new())),
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

    /// Returns the immutable path for one content identity.
    #[must_use]
    pub fn object_path(&self, object: RawArchiveObjectId) -> PathBuf {
        self.object_path_for(object)
    }

    /// Returns the object extent when a published object is present.
    pub fn object_len(&self, object: RawArchiveObjectId) -> Result<Option<u64>, ContentStoreError> {
        let path = self.object_path_for(object);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
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
        let mut file = File::open(&path)?;
        let (length, hash) = archive_hash(&mut file, maximum)?;
        if object_from_hash(length, hash) != object {
            return Err(ContentStoreError::CorruptObject { path });
        }
        Ok(length)
    }

    /// Opens an admitted object without copying it into a caller buffer.
    pub fn open_object(&self, object: RawArchiveObjectId) -> Result<File, ContentStoreError> {
        Ok(File::open(self.object_path_for(object))?)
    }

    fn create_temp(&self, label: &str) -> Result<TempArtifact, ContentStoreError> {
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
        let (token, path, file) = loop {
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
                Ok(file) => break (token, path, file),
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
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        self.active_temps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(path.clone());
        Ok(TempArtifact {
            store: self.clone(),
            path,
            lease_path,
            token,
            file: Some(file),
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
        let target = self.object_path_for(object);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        let admission = match fs::hard_link(temp.path(), &target) {
            Ok(()) => ObjectAdmission::Published { object, bytes },
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let existing = self.verify_object(object, bytes)?;
                ObjectAdmission::Reused {
                    object,
                    bytes: existing,
                }
            }
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                let mut source = File::open(temp.path())?;
                let mut destination = match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        let existing = self.verify_object(object, bytes)?;
                        return Ok(ObjectAdmission::Reused {
                            object,
                            bytes: existing,
                        });
                    }
                    Err(error) => return Err(error.into()),
                };
                io::copy(&mut source, &mut destination)?;
                destination.sync_all()?;
                ObjectAdmission::Published { object, bytes }
            }
            Err(error) => return Err(error.into()),
        };
        temp.preserve = false;
        Ok(admission)
    }

    /// Removes only owned, expired private temps. A live in-process temp is
    /// protected by the active set even if its mtime is old; another process
    /// is protected by the expiring lease marker and must renew it.
    pub fn scavenge_stale(&self, older_than: Duration) -> Result<usize, ContentStoreError> {
        let now = now_millis();
        let active = self
            .active_temps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut removed = 0;
        for entry in fs::read_dir(self.root.join("temps"))? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("part")
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
            if age >= older_than {
                fs::remove_file(&path)?;
                let _ = fs::remove_file(marker);
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Opens or resumes one durable range-capable transfer.
    pub fn resume_or_start(
        &self,
        id: TransferId,
        expected: Option<RawArchiveObjectId>,
        expected_length: Option<u64>,
    ) -> Result<ResumableTransfer, ContentStoreError> {
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

struct TempArtifact {
    store: ContentAddressedStore,
    path: PathBuf,
    lease_path: PathBuf,
    token: [u8; ID_BYTES],
    file: Option<File>,
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
            .active_temps
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
    let bytes = fs::read(path).ok()?;
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
    owner_path: PathBuf,
    owner_token: [u8; ID_BYTES],
    file: Option<File>,
    hasher: Option<Hasher>,
    telemetry: TransferTelemetry,
    finished: bool,
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
        let owner_path = store.root.join("transfers").join(format!("{key}.owner"));
        let owner_token = store.next_token();
        acquire_transfer_owner(&owner_path, owner_token)?;

        let cleanup_owner_path = owner_path.clone();
        let open_result = (|| {
            let mut file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .append(true)
                .open(&data_path)?;
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
                owner_path,
                owner_token,
                file: Some(file),
                hasher: Some(hasher),
                telemetry: TransferTelemetry {
                    resumed_bytes: received,
                    downloaded_bytes: 0,
                },
                finished: false,
            })
        })();
        if open_result.is_err() {
            let _ = fs::remove_file(cleanup_owner_path);
        }
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

    /// Binds the representation validator before appending a response body.
    pub fn set_validator(
        &mut self,
        validator: Option<TransferValidator>,
    ) -> Result<(), ContentStoreError> {
        if self.checkpoint.received != 0
            && self.checkpoint.validator.is_some()
            && self.checkpoint.validator != validator
        {
            return Err(ContentStoreError::TransferValidatorChanged);
        }
        self.checkpoint.validator = validator;
        write_checkpoint(&self.state_path, &self.checkpoint)
    }

    /// Binds the authenticated final extent once a response exposes it.
    pub fn bind_expected_length(&mut self, length: u64) -> Result<(), ContentStoreError> {
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
        self.checkpoint.expected_length = Some(length);
        write_checkpoint(&self.state_path, &self.checkpoint)
    }

    /// Restarts the transfer at byte zero, preserving no unvalidated prefix.
    pub fn restart_from_zero(
        &mut self,
        reason: Option<TransferResetReason>,
    ) -> Result<(), ContentStoreError> {
        let had_prefix = self.checkpoint.received != 0;
        if had_prefix {
            // Publish the reset in the checkpoint before moving or truncating
            // bytes. A kill in either window then reopens as an empty transfer
            // and can safely discard any uncommitted old tail.
            self.checkpoint.received = 0;
            self.checkpoint.validator = None;
            if reason.is_some() {
                self.checkpoint.expected_length = None;
            }
            write_checkpoint(&self.state_path, &self.checkpoint)?;
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
        file.set_len(0)?;
        file.seek(SeekFrom::Start(0))?;
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
        write_checkpoint(&self.state_path, &self.checkpoint)
    }

    /// Renews the transfer lease while a remote range request is in flight.
    pub fn renew(&self) -> Result<(), ContentStoreError> {
        let expires = now_millis().saturating_add(TEMP_TTL.as_millis() as u64);
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.owner_path)?;
        file.write_all(&self.owner_token)?;
        file.write_all(&expires.to_be_bytes())?;
        file.sync_all()?;
        Ok(())
    }

    /// Appends one bounded response body and persists its new range start.
    pub fn append<R: Read>(
        &mut self,
        mut source: R,
        maximum: u64,
    ) -> Result<u64, ContentStoreError> {
        let mut buffer = [0_u8; CHUNK_BYTES];
        loop {
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
            if let Err(error) = write_checkpoint(&self.state_path, &self.checkpoint) {
                self.checkpoint.received = previous;
                self.rollback_append(previous)?;
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

    fn rollback_append(&mut self, previous: u64) -> Result<(), ContentStoreError> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer is closed")))?;
        file.set_len(previous)?;
        file.seek(SeekFrom::End(0))?;
        Ok(())
    }

    fn quarantine_prefix(&mut self, reason: TransferResetReason) -> Result<(), ContentStoreError> {
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        let destination = self.store.root.join("quarantine").join(format!(
            "{}.{}.prefix",
            hex(&self.owner_token),
            reason.code()
        ));
        fs::rename(&self.data_path, destination)?;
        self.file = Some(
            OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .append(true)
                .open(&self.data_path)?,
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
        let received = self.checkpoint.received;
        if let Some(expected_length) = self.checkpoint.expected_length
            && received != expected_length
        {
            return Err(ContentStoreError::LengthMismatch {
                expected: expected_length,
                actual: received,
            });
        }
        let hash = *self
            .hasher
            .take()
            .ok_or_else(|| ContentStoreError::Io(io::Error::other("transfer has no digest")))?
            .finalize()
            .as_bytes();
        let actual = object_from_hash(received, hash);
        if let Some(expected) = self.checkpoint.expected
            && expected != actual
        {
            let quarantine = self.quarantine("digest-mismatch")?;
            return Err(ContentStoreError::DigestMismatch {
                expected,
                actual,
                quarantine: Some(quarantine),
            });
        }
        if let Some(file) = self.file.take() {
            file.sync_all()?;
        }
        if let Err(error) = verifier(&self.data_path, received, actual) {
            let quarantine = self.quarantine("verification").ok();
            if matches!(error, ContentStoreError::VerificationRejected { .. }) {
                return Err(ContentStoreError::VerificationRejected { quarantine });
            }
            return Err(error);
        }
        let target = self.store.object_path_for(actual);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        self.file.take();
        let result = match fs::hard_link(&self.data_path, &target) {
            Ok(()) => ObjectAdmission::Published {
                object: actual,
                bytes: received,
            },
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => ObjectAdmission::Reused {
                object: actual,
                bytes: self.store.verify_object(actual, received)?,
            },
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                let mut source = File::open(&self.data_path)?;
                let mut destination = match OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&target)
                {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                        return self.finish_existing(actual);
                    }
                    Err(error) => return Err(error.into()),
                };
                io::copy(&mut source, &mut destination)?;
                destination.sync_all()?;
                ObjectAdmission::Published {
                    object: actual,
                    bytes: received,
                }
            }
            Err(error) => return Err(error.into()),
        };
        fs::remove_file(&self.data_path)?;
        let _ = fs::remove_file(&self.state_path);
        let _ = fs::remove_file(&self.owner_path);
        self.finished = true;
        Ok(result)
    }

    fn finish_existing(
        mut self,
        object: RawArchiveObjectId,
    ) -> Result<ObjectAdmission, ContentStoreError> {
        let bytes = self.store.verify_object(object, self.checkpoint.received)?;
        fs::remove_file(&self.data_path)?;
        let _ = fs::remove_file(&self.state_path);
        let _ = fs::remove_file(&self.owner_path);
        self.finished = true;
        Ok(ObjectAdmission::Reused { object, bytes })
    }

    fn quarantine(&mut self, reason: &str) -> Result<PathBuf, ContentStoreError> {
        self.file.take();
        let destination = self.store.root.join("quarantine").join(format!(
            "{}.{}.part",
            hex(&self.owner_token),
            reason
        ));
        fs::rename(&self.data_path, &destination)?;
        let _ = fs::remove_file(&self.state_path);
        let _ = fs::remove_file(&self.owner_path);
        self.finished = true;
        Ok(destination)
    }
}

impl Drop for ResumableTransfer {
    fn drop(&mut self) {
        let _ = self.file.take();
        if !self.finished {
            let _ = fs::remove_file(&self.owner_path);
        }
    }
}

fn acquire_transfer_owner(path: &Path, token: [u8; ID_BYTES]) -> Result<(), ContentStoreError> {
    let expires = now_millis().saturating_add(TEMP_TTL.as_millis() as u64);
    match write_lease_marker(path, token, expires) {
        Ok(()) => Ok(()),
        Err(ContentStoreError::Io(error)) if error.kind() == io::ErrorKind::AlreadyExists => {
            if read_lease_expiry(path).is_some_and(|expiry| expiry <= now_millis()) {
                let _ = fs::remove_file(path);
                write_lease_marker(
                    path,
                    token,
                    now_millis().saturating_add(TEMP_TTL.as_millis() as u64),
                )
            } else {
                Err(ContentStoreError::TransferBusy)
            }
        }
        Err(error) => Err(error),
    }
}

fn write_checkpoint(path: &Path, checkpoint: &TransferCheckpoint) -> Result<(), ContentStoreError> {
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
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(temporary, path)?;
    Ok(())
}

fn read_checkpoint(path: &Path) -> Result<Option<TransferCheckpoint>, ContentStoreError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
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
        // Expire only the lease so the parent can model a restarted process
        // without waiting for the production lease TTL.
        let owner = root.join("transfers").join(format!("{}.owner", hex(&id.0)));
        let mut lease = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(owner)
            .expect("child open lease");
        lease.write_all(&[0; ID_BYTES]).expect("child lease token");
        lease
            .write_all(&0_u64.to_be_bytes())
            .expect("child lease expiry");
        lease.sync_all().expect("child sync lease");
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

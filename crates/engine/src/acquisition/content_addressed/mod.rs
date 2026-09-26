//! Shared content-addressed admission primitives.
//!
//! Registry and local-project adapters deliberately stop at this boundary.  A
//! caller supplies a byte stream and an optional authenticated object identity;
//! this module owns bounded streaming, durable temporary state, first-writer
//! publication, and archive-manifest admission. Restartable range transfers
//! sit beside this object root. Immutable objects are regular files, and a
//! completed transfer is linked into its digest path exactly once.

use super::{ManifestEntry, RawArchiveObjectId, TreeManifest};
use blake3::Hasher;
use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const CHUNK_BYTES: usize = 64 * 1024;
const ID_BYTES: usize = 32;
const OBJECT_DOMAIN: &[u8] = b"backend.acquisition.archive.v1\0";
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

mod transfer;
pub use transfer::{
    ResumableTransfer, TransferCheckpoint, TransferId, TransferResetReason, TransferTelemetry,
    TransferValidator,
};

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

        let part = path.join("transfers").join(format!("{}.part", hex(&id.to_bytes())));
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
        let part = root.join("transfers").join(format!("{}.part", hex(&id.to_bytes())));
        let mut file = OpenOptions::new()
            .append(true)
            .open(&part)
            .expect("child open part");
        file.write_all(b"uncheckpointed").expect("child write tail");
        file.sync_all().expect("child sync tail");
        drop(file);
        // Expire only the lease so the parent can model a restarted process
        // without waiting for the production lease TTL.
        let owner = root.join("transfers").join(format!("{}.owner", hex(&id.to_bytes())));
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
        let part = path.join("transfers").join(format!("{}.part", hex(&id.to_bytes())));
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

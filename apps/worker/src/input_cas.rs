//! Worker input reception built on the canonical sparse transfer typestate.
//!
//! `ReceivingCas` owns all identity, range, replay, chain, coverage, and
//! final digest checks. This adapter supplies only the durable extent store;
//! it deliberately does not define a second transfer protocol.

use crate::closure_index::DurableNodeIndex;
#[path = "durable_sink.rs"]
mod durable_sink;
use backend_engine::{
    AdmittedChunk, AuthorityClaim, CheckedWorkspaceManifest, Frame, ImmutableObjectSchema,
    ObjectKey, ObjectRequest, ObjectVersion, ReceivingCas, ReceivingCheckpoint, ReplicationError,
    Schema, TransferId, TransportLimits, UntrustedWorkspaceManifest, UnverifiedObjectRequest,
    WireIdentity, WireReceivingCheckpoint, WorkspaceRoot,
};
use durable_sink::DurableSink;
#[path = "input_progress.rs"]
mod input_progress;
#[path = "input_cas/lease.rs"]
mod lease;
#[path = "input_cas/retention.rs"]
mod retention;
#[path = "input_cas/transfer.rs"]
mod transfer;
#[path = "input_cas/workspace.rs"]
mod workspace;
pub use retention::{CasGcLimits, CasGcReport, CasRootLease};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;
use std::{fs::Metadata, io};

/// Bytes admitted by file type and byte length before a durable grammar
/// decoder observes them.
pub(super) struct BoundedFileImage(Vec<u8>);

impl BoundedFileImage {
    pub(super) fn read_optional(
        path: &Path,
        maximum: usize,
    ) -> Result<Option<Self>, ReplicationError> {
        let file = match open_readonly_nofollow(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ReplicationError::CorruptFrame),
        };
        let metadata = file
            .metadata()
            .map_err(|_| ReplicationError::Disconnected)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ReplicationError::CorruptFrame);
        }
        if metadata.len() > maximum as u64 {
            return Err(ReplicationError::MessageTooLarge);
        }
        let length =
            usize::try_from(metadata.len()).map_err(|_| ReplicationError::MessageTooLarge)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| ReplicationError::Backpressure)?;
        file.take((maximum as u64).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| ReplicationError::Disconnected)?;
        if bytes.len() > maximum {
            return Err(ReplicationError::MessageTooLarge);
        }
        Ok(Some(Self(bytes)))
    }

    pub(super) fn as_slice(&self) -> &[u8] {
        &self.0
    }

    pub(super) fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

pub(super) fn checked_regular_metadata(path: &Path) -> Result<Option<Metadata>, ReplicationError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ReplicationError::Disconnected),
    };
    if metadata.file_type().is_symlink() {
        return Err(ReplicationError::CorruptFrame);
    }
    if !metadata.is_file() {
        return Ok(None);
    }
    Ok(Some(metadata))
}

pub(super) fn create_new_temp_nofollow(path: &Path) -> Result<File, ReplicationError> {
    let parent = path.parent().ok_or(ReplicationError::CorruptFrame)?;
    let name = path.file_name().ok_or(ReplicationError::CorruptFrame)?;
    let parent_metadata =
        fs::symlink_metadata(parent).map_err(|_| ReplicationError::Disconnected)?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(ReplicationError::CorruptFrame);
    }
    #[cfg(unix)]
    let file = {
        use rustix::fs::{AtFlags, Mode, OFlags, open, openat, unlinkat};
        let directory = open(
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| ReplicationError::CorruptFrame)?;
        match unlinkat(&directory, name, AtFlags::empty()) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ReplicationError::CorruptFrame),
        }
        openat(
            &directory,
            name,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NONBLOCK,
            Mode::RUSR | Mode::WUSR,
        )
        .map(File::from)
        .map_err(|_| ReplicationError::CorruptFrame)?
    };
    #[cfg(not(unix))]
    let file = {
        let directory = backend_platform::durability::open_directory(parent)
            .map_err(|_| ReplicationError::Disconnected)?;
        if !directory
            .metadata()
            .map_err(|_| ReplicationError::Disconnected)?
            .is_dir()
        {
            return Err(ReplicationError::CorruptFrame);
        }
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Err(ReplicationError::CorruptFrame),
        }
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        }
        options
            .open(path)
            .map_err(|_| ReplicationError::CorruptFrame)?
    };
    let metadata = file
        .metadata()
        .map_err(|_| ReplicationError::Disconnected)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ReplicationError::CorruptFrame);
    }
    Ok(file)
}

#[cfg(unix)]
fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, open};
    open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(io::Error::from)
}

#[cfg(windows)]
fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "symbolic link"));
    }
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_readonly_nofollow(path: &Path) -> io::Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "symbolic link"));
    }
    File::open(path)
}

const MAX_ACTIVE_SESSIONS: usize = 64;
const MAX_RETAINED_OBJECTS: usize = 8_192;
const MAX_RETAINED_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RETAINED_SIDECARS: usize = 16_384;
const MAX_RETAINED_SIDECAR_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum physical reclamation work charged to one GC call. Object files and
/// sidecars share this budget so a single root change cannot perform an
/// unbounded sweep merely because both namespaces are large.
pub(super) const MAX_GC_FILES_PER_CALL: usize = 256;
pub(super) const MAX_GC_BYTES_PER_CALL: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct CasGcBudget {
    files: usize,
    bytes: u64,
}

impl CasGcBudget {
    pub(super) fn new(limits: CasGcLimits) -> Self {
        Self {
            files: limits.max_files.min(MAX_GC_FILES_PER_CALL),
            bytes: limits.max_bytes.min(MAX_GC_BYTES_PER_CALL),
        }
    }

    pub(super) fn take(&mut self, bytes: u64) -> bool {
        if self.files == 0 || bytes > self.bytes {
            return false;
        }
        self.files -= 1;
        self.bytes -= bytes;
        true
    }
}

/// Worker-owned receiving CAS facade. Its public methods expose only typed
/// requests, checkpoints, and immutable committed bytes.
pub struct InputCas<T: Schema = ImmutableObjectSchema> {
    pub(in crate::input_cas) sink: DurableSink<T>,
    pub(in crate::input_cas) active: BTreeMap<TransferId, ReceivingCas<DurableSink<T>, T>>,
    pub(in crate::input_cas) partial: BTreeMap<TransferId, PersistedTransfer<T>>,
    pub(super) limits: TransportLimits,
    pub(super) max_extents: usize,
    pub(super) admitted_workspace: Option<WorkspaceRoot>,
    pub(super) admitted_workspace_claim: Option<[u8; 32]>,
    pub(super) workspace_manifest_bytes: Option<Vec<u8>>,
    pub(super) admitted_root: Option<backend_engine::MerkleRoot>,
    pub(super) durable_root_claim: Option<backend_engine::MerkleRootClaim>,
    pub(super) node_index: DurableNodeIndex,
    pub(super) need_memory: BTreeMap<[u8; 32], Vec<[u8; 32]>>,
    pub(super) received_memory: BTreeMap<[u8; 32], BTreeSet<[u8; 32]>>,
    pub(super) root_epoch: u64,
    pub(super) root_claims_memory: BTreeMap<[u8; 32], BTreeSet<[u8; 32]>>,
}

pub(in crate::input_cas) struct PersistedTransfer<T: Schema> {
    pub(in crate::input_cas) root: [u8; 32],
    pub(in crate::input_cas) checkpoint: WireReceivingCheckpoint<T>,
}

impl<T: Schema> fmt::Debug for InputCas<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InputCas")
            .field("sink", &self.sink.usage())
            .field("active", &self.active.len())
            .field("limits", &self.limits)
            .field("max_extents", &self.max_extents)
            .field("admitted_workspace", &self.admitted_workspace)
            .field("admitted_workspace_claim", &self.admitted_workspace_claim)
            .field(
                "workspace_manifest_bytes",
                &self.workspace_manifest_bytes.as_ref().map(Vec::len),
            )
            .field("admitted_root", &self.admitted_root)
            .field("durable_root_claim", &self.durable_root_claim)
            .field("node_index", &"durable node index")
            .field("need_memory", &self.need_memory.len())
            .field("received_memory", &self.received_memory.len())
            .field("root_epoch", &self.root_epoch)
            .field("root_claims_memory", &self.root_claims_memory.len())
            .finish()
    }
}

impl<T: Schema> Drop for InputCas<T> {
    fn drop(&mut self) {
        // Every sparse write is synced before its checkpoint sidecar is
        // atomically published. Dropping the in-memory cursor preserves that
        // durable pair so the next process can reopen it.
        self.active.clear();
    }
}

impl<T: Schema> InputCas<T> {
    /// Opens the worker's durable input namespace.
    /// # Errors
    /// Returns an error when the input is invalid or durable state cannot be accessed.
    pub fn open(path: impl AsRef<Path>, limits: TransportLimits) -> Result<Self, ReplicationError> {
        limits.validate()?;
        let path = path.as_ref();
        let admitted_workspace = None;
        let marker_root = read_root_marker(&path.join("ROOT"))?;
        // ROOT_LEASE contains the authoritative root/epoch pair. ROOT is a
        // compatibility marker and may be one atomic rename behind it after
        // a crash. Prefer the complete lease pair and repair the marker on
        // the next successful root publication.
        let (durable_root_claim, root_epoch) =
            read_root_lease(&path.join("ROOT_LEASE"), marker_root)?;
        let workspace_manifest_bytes = match BoundedFileImage::read_optional(
            &path.join("WORKSPACE_MANIFEST"),
            limits.max_frame,
        )? {
            Some(bytes) => {
                UntrustedWorkspaceManifest::decode_untrusted(bytes.as_slice())
                    .map_err(|_| ReplicationError::CorruptFrame)?;
                Some(bytes.into_vec())
            }
            None => None,
        };
        let (max_retained_objects, max_retained_bytes) = retention_budget(limits);
        let sink = DurableSink::open_with_budget(
            path,
            max_retained_objects,
            max_retained_bytes,
            limits.max_object,
        )?;
        let mut cas = Self {
            sink,
            active: BTreeMap::new(),
            partial: BTreeMap::new(),
            limits,
            max_extents: limits.max_ranges,
            admitted_workspace,
            admitted_workspace_claim: read_workspace_claim(&path.join("WORKSPACE"))?,
            workspace_manifest_bytes,
            admitted_root: None,
            durable_root_claim,
            node_index: DurableNodeIndex::open(Some(path.to_owned())),
            need_memory: BTreeMap::new(),
            received_memory: BTreeMap::new(),
            root_epoch,
            root_claims_memory: BTreeMap::new(),
        };
        cas.load_transfer_checkpoints()?;
        // A workspace manifest is published before the root lease during
        // closure completion. If the process stopped between those durable
        // boundaries, discard the manifest that belongs to a different
        // relation root and let the next offer rebuild it. Keeping it would
        // make restart reject an otherwise valid older lease.
        // The durable root is still only a wire claim at this point. It is
        // promoted to a typed root when a reconnect offer is checked against
        // the caller's expected root; opening the namespace must not forge a
        // checked root from marker bytes.
        // A process crash can leave a complete object with no live root lease,
        // or a stale proof/need journal from a prior closure.  Reclaim before
        // admitting new work so the first reconnect cannot exceed the durable
        // retention budget.
        cas.reclaim_unleased()?;
        let (objects, bytes) = cas.sink.usage();
        if objects > max_retained_objects || bytes > max_retained_bytes {
            return Err(ReplicationError::Backpressure);
        }
        if !cas.within_sidecar_budget()? {
            return Err(ReplicationError::Backpressure);
        }
        Ok(cas)
    }
}

fn read_workspace_claim(path: &Path) -> Result<Option<[u8; 32]>, ReplicationError> {
    let bytes = match BoundedFileImage::read_optional(path, 32)? {
        Some(bytes) => bytes.into_vec(),
        None => return Ok(None),
    };
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ReplicationError::CorruptFrame)?;
    Ok(Some(bytes))
}

fn read_root_marker(
    path: &Path,
) -> Result<Option<backend_engine::MerkleRootClaim>, ReplicationError> {
    let bytes = match BoundedFileImage::read_optional(path, 4 + 32)? {
        Some(bytes) => bytes.into_vec(),
        None => return Ok(None),
    };
    if bytes.len() != 4 + 32 {
        return Err(ReplicationError::CorruptFrame);
    }
    let schema: [u8; 4] = bytes[..4]
        .try_into()
        .map_err(|_| ReplicationError::CorruptFrame)?;
    let digest: [u8; 32] = bytes[4..]
        .try_into()
        .map_err(|_| ReplicationError::CorruptFrame)?;
    Ok(Some(backend_engine::MerkleRootClaim::from_wire(
        u32::from_be_bytes(schema),
        backend_engine::NodeDigest(digest),
    )))
}

fn read_root_lease(
    path: &Path,
    root: Option<backend_engine::MerkleRootClaim>,
) -> Result<(Option<backend_engine::MerkleRootClaim>, u64), ReplicationError> {
    let bytes = match BoundedFileImage::read_optional(path, 4 + 32 + 8)? {
        Some(bytes) => bytes.into_vec(),
        None => {
            return if root.is_none() {
                Ok((None, 0))
            } else {
                // A marker without a complete lease has no restart proof.
                // Fail closed rather than treating an unleased root as live.
                Err(ReplicationError::CorruptFrame)
            };
        }
    };
    if bytes.len() != 4 + 32 + 8 {
        return Err(ReplicationError::CorruptFrame);
    }
    let schema = u32::from_be_bytes(
        bytes[..4]
            .try_into()
            .map_err(|_| ReplicationError::CorruptFrame)?,
    );
    let digest: [u8; 32] = bytes[4..36]
        .try_into()
        .map_err(|_| ReplicationError::CorruptFrame)?;
    let stored =
        backend_engine::MerkleRootClaim::from_wire(schema, backend_engine::NodeDigest(digest));
    let epoch = u64::from_be_bytes(
        bytes[36..]
            .try_into()
            .map_err(|_| ReplicationError::CorruptFrame)?,
    );
    if epoch == 0 {
        return Err(ReplicationError::CorruptFrame);
    }
    Ok((Some(stored), epoch))
}

fn retention_budget(limits: TransportLimits) -> (usize, u64) {
    let objects = limits
        .max_objects
        .saturating_mul(2)
        .clamp(1, MAX_RETAINED_OBJECTS);
    let per_object = limits.max_object.min(MAX_RETAINED_BYTES);
    let bytes = per_object
        .saturating_mul(objects as u64)
        .min(MAX_RETAINED_BYTES)
        .max(per_object);
    (objects, bytes)
}

fn hex(bytes: [u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn decode_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut bytes = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes[index] = (high << 4) | low;
    }
    Some(bytes)
}

fn is_object_name(value: &str) -> bool {
    decode_hex(value).is_some()
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!("backend-worker-input-cas-{label}-{nonce}"))
    }

    fn transfer_limits() -> TransportLimits {
        TransportLimits {
            max_frame: 1024,
            max_chunk: 3,
            max_object: 1024,
            max_objects: 16,
            max_ranges: 16,
            max_capabilities: 8,
            max_key_bytes: 32,
            max_inputs: 8,
        }
    }

    fn transfer_authority() -> AuthorityClaim {
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(b"stream-authority");
        AuthorityClaim::from_typed(&key, backend_engine::AuthorityEpoch(1))
    }

    fn transfer_root() -> backend_engine::MerkleRoot {
        let root_object = ObjectVersion::<ImmutableObjectSchema>::from_value(b"stream-root");
        backend_engine::MerkleRoot::from_admitted_manifest(1, root_object)
    }

    fn chunk_frames(
        bytes: &[u8],
        transfer: TransferId,
        key: ObjectKey<ImmutableObjectSchema>,
        version: ObjectVersion<ImmutableObjectSchema>,
        authority: AuthorityClaim,
    ) -> Vec<Frame<ImmutableObjectSchema>> {
        let mut previous = backend_engine::ChunkChain([0; 32]);
        bytes
            .chunks(3)
            .enumerate()
            .map(|(sequence, payload)| {
                let frame = Frame::new(
                    transfer,
                    key,
                    version,
                    backend_engine::ChunkParts {
                        object_len: bytes.len() as u64,
                        offset: (sequence * 3) as u64,
                        sequence: sequence as u64,
                        previous_chain: previous,
                        payload: payload.to_vec(),
                    },
                    authority,
                )
                .expect("construct test chunk");
                previous = frame.chain;
                frame
            })
            .collect()
    }

    fn declare_missing(
        cas: &mut InputCas,
        root: backend_engine::MerkleRoot,
        version: ObjectVersion<ImmutableObjectSchema>,
    ) {
        cas.append_missing(root, WireIdentity::from_typed(&version))
            .expect("declare missing object");
    }

    fn open_transfer_cas(path: &Path) -> InputCas {
        InputCas::<ImmutableObjectSchema>::open(path, transfer_limits()).expect("open transfer CAS")
    }

    #[test]
    fn malformed_durable_markers_fail_closed() {
        let path = temp_directory("markers");
        fs::create_dir_all(&path).expect("create marker directory");
        fs::write(path.join("WORKSPACE"), [0_u8; 31]).expect("write workspace marker");
        assert!(matches!(
            InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default()),
            Err(ReplicationError::CorruptFrame)
        ));
        fs::remove_file(path.join("WORKSPACE")).expect("remove workspace marker");
        fs::write(path.join("ROOT"), [0_u8; 35]).expect("write root marker");
        assert!(matches!(
            InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default()),
            Err(ReplicationError::CorruptFrame)
        ));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn malformed_workspace_manifest_fails_closed() {
        let path = temp_directory("manifest");
        fs::create_dir_all(&path).expect("create manifest directory");
        fs::write(path.join("WORKSPACE_MANIFEST"), b"truncated-manifest")
            .expect("write malformed manifest");
        assert!(matches!(
            InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default()),
            Err(ReplicationError::CorruptFrame)
        ));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn oversized_workspace_manifest_is_rejected_before_body_allocation() {
        let path = temp_directory("oversized-manifest");
        fs::create_dir_all(&path).expect("create manifest directory");
        let manifest = File::create(path.join("WORKSPACE_MANIFEST")).expect("create manifest");
        manifest
            .set_len(TransportLimits::default().max_frame as u64 + 1)
            .expect("make sparse oversized manifest");
        assert!(matches!(
            InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default()),
            Err(ReplicationError::MessageTooLarge)
        ));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn oversized_durable_object_is_rejected_before_body_allocation() {
        let path = temp_directory("oversized-object");
        let limits = TransportLimits::default();
        let mut cas = InputCas::<ImmutableObjectSchema>::open(&path, limits).expect("open CAS");
        let expected = ObjectVersion::<ImmutableObjectSchema>::from_value(b"small-object");
        let object = File::create(path.join(hex(expected.to_bytes()))).expect("create object");
        object
            .set_len(limits.max_object + 1)
            .expect("make sparse oversized object");

        assert!(matches!(
            cas.get(expected),
            Err(ReplicationError::MessageTooLarge)
        ));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn oversized_node_proof_is_rejected_before_body_allocation() {
        let path = temp_directory("oversized-proof");
        let cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("open CAS");
        let digest = [4_u8; 32];
        let proof =
            File::create(path.join(format!(".proof-{}", hex(digest)))).expect("create proof");
        proof.set_len(64 * 1024 + 1).expect("make sparse proof");

        assert!(matches!(
            cas.node_proof(digest),
            Err(ReplicationError::MessageTooLarge)
        ));
        let _ = fs::remove_dir_all(path);
    }

    #[cfg(unix)]
    #[test]
    fn durable_marker_symlinks_are_never_followed() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let path = temp_directory("marker-symlink");
        fs::create_dir_all(&path).expect("create marker directory");
        let outside = path.with_extension("outside");
        fs::write(&outside, [8_u8; 32]).expect("write outside marker");
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o600))
            .expect("protect outside marker");
        symlink(&outside, path.join("WORKSPACE")).expect("link marker");
        assert!(
            InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default()).is_err()
        );
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(outside);
    }

    #[cfg(unix)]
    #[test]
    fn staging_and_canonical_object_symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;

        let path = temp_directory("transfer-symlinks");
        let outside = temp_directory("transfer-symlink-target");
        fs::create_dir_all(&path).expect("create CAS directory");
        fs::write(&outside, b"outside bytes stay unchanged").expect("write outside target");
        let bytes: &[u8] = b"abcdef";
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes);
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes);
        let transfer = TransferId::new(74).expect("transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let frames = chunk_frames(bytes, transfer, key, version, authority);
        let part = path.join(format!(".{:016x}.part", transfer.get()));
        symlink(&outside, &part).expect("link part to outside target");

        let mut cas = open_transfer_cas(&path);
        assert!(
            !part.exists(),
            "cold open safely unlinks an uncheckpointed orphan staging link"
        );
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                .expect("receive into a fresh no-follow staging file"),
            "the first partial frame must not complete the object"
        );
        assert_eq!(
            fs::read(&outside).expect("read outside target"),
            b"outside bytes stay unchanged"
        );
        let object = path.join(hex(version.to_bytes()));
        symlink(&outside, &object).expect("link canonical object to outside target");
        assert!(!cas.contains_claim(WireIdentity::from_typed(&version)));
        assert_eq!(
            fs::read(&outside).expect("read outside target after claim lookup"),
            b"outside bytes stay unchanged"
        );
        drop(cas);
        fs::remove_file(&part).expect("remove part symlink");
        assert!(matches!(
            InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default()),
            Err(ReplicationError::CorruptFrame)
        ));
        assert_eq!(
            fs::read(&outside).expect("read outside target after reopen"),
            b"outside bytes stay unchanged"
        );
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(outside);
    }

    #[cfg(unix)]
    #[test]
    fn temporary_sidecar_links_are_unlinked_without_mutating_the_root() {
        use std::fs::hard_link;
        use std::os::unix::fs::symlink;

        let path = temp_directory("sidecar-temp-symlink");
        let outside = temp_directory("sidecar-temp-target");
        fs::create_dir_all(&path).expect("create CAS directory");
        fs::write(&outside, b"outside sidecar bytes").expect("write outside target");
        let mut cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("open CAS");
        let root = transfer_root();
        cas.record_root(root).expect("admit durable root");
        let temporary = path.join(".WORKSPACE.part");
        symlink(&outside, &temporary).expect("link temporary sidecar");
        cas.record_workspace_claim([7; 32])
            .expect("replace stale symlink without following it");
        assert_eq!(cas.admitted_root(), Some(root));
        hard_link(&outside, &temporary).expect("link stale temp to outside inode");
        cas.record_workspace_claim([8; 32])
            .expect("replace stale hard link without truncating it");
        assert_eq!(cas.admitted_root(), Some(root));
        fs::create_dir(&temporary).expect("make invalid temp directory");
        assert!(cas.record_workspace_claim([9; 32]).is_err());
        assert_eq!(cas.admitted_root(), Some(root));
        assert_eq!(
            fs::read(&outside).expect("read outside target"),
            b"outside sidecar bytes"
        );
        drop(cas);
        fs::remove_dir(&temporary).expect("remove invalid temporary directory");
        let cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("reopen CAS after rejected temp path");
        assert!(cas.durable_root_matches(root));
        drop(cas);
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(outside);
    }

    #[cfg(unix)]
    #[test]
    fn persisted_part_symlink_is_rejected_on_cold_reopen() {
        use std::os::unix::fs::symlink;

        let path = temp_directory("persisted-part-symlink");
        let outside = temp_directory("persisted-part-target");
        fs::write(&outside, b"outside bytes stay unchanged").expect("write outside target");
        let bytes: &[u8] = b"abcdef";
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes);
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes);
        let transfer = TransferId::new(75).expect("transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let frames = chunk_frames(bytes, transfer, key, version, authority);
        let part = path.join(format!(".{:016x}.part", transfer.get()));

        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                .expect("persist first sparse extent")
        );
        drop(cas);
        fs::remove_file(&part).expect("remove original sparse staging file");
        symlink(&outside, &part).expect("replace staging file with link");

        assert!(matches!(
            InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default()),
            Err(ReplicationError::CorruptFrame)
        ));
        assert_eq!(
            fs::read(&outside).expect("read outside target"),
            b"outside bytes stay unchanged"
        );
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(outside);
    }

    #[test]
    fn corrupt_cached_object_is_not_admitted_as_warm() {
        let path = temp_directory("corrupt");
        let cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("open input CAS");
        let bytes: &[u8] = b"canonical-object";
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes);
        fs::write(path.join(hex(version.to_bytes())), b"corrupt").expect("write corrupt object");
        assert!(!cas.contains(version));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn root_lease_reclaims_unreachable_objects_and_survives_reopen() {
        let path = temp_directory("gc");
        let mut cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("open input CAS");
        let live_bytes: &[u8] = b"live-canonical-object";
        let stale_bytes: &[u8] = b"stale-canonical-object";
        let live = ObjectVersion::<ImmutableObjectSchema>::from_value(live_bytes);
        let stale = ObjectVersion::<ImmutableObjectSchema>::from_value(stale_bytes);
        fs::write(path.join(hex(live.to_bytes())), live_bytes).expect("write live object");
        fs::write(path.join(hex(stale.to_bytes())), stale_bytes).expect("write stale object");
        let root = backend_engine::MerkleRoot::from_admitted_manifest(
            1,
            ObjectVersion::<ImmutableObjectSchema>::from_value(&[9_u8; 32]),
        );
        let claim = WireIdentity::from_typed(&live);
        cas.record_input_claim(root, claim)
            .expect("record root input claim");
        cas.record_root(root).expect("publish root lease");
        let report = cas.reclaim_unleased().expect("repeat bounded sweep");
        assert_eq!(report.objects_removed, 0);
        assert!(!path.join(hex(stale.to_bytes())).exists());
        assert!(path.join(hex(live.to_bytes())).exists());
        assert_eq!(cas.root_lease().map(CasRootLease::epoch), Some(1));

        let reopened = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("reopen leased CAS");
        assert_eq!(
            reopened.durable_root_claim,
            Some(root.to_claim()),
            "restart keeps the lease as an untrusted claim until offer admission"
        );
        assert!(reopened.contains(live));
        assert!(
            reopened
                .input_claim(root)
                .expect("read input claim")
                .is_some()
        );
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn live_root_keeps_proofs_for_every_complete_node() {
        let path = temp_directory("gc-node-proofs");
        let mut cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("open input CAS");
        let root = backend_engine::MerkleRoot::from_admitted_manifest(
            1,
            ObjectVersion::<ImmutableObjectSchema>::from_value(b"proof-root"),
        );
        let root_digest = root.digest().as_bytes();
        let child_digest = [0x42; 32];
        let incomplete_digest = [0x43; 32];
        cas.record_node_proof(root_digest, b"root-proof")
            .expect("record root proof");
        cas.record_node_proof(child_digest, b"child-proof")
            .expect("record child proof");
        cas.record_node_proof(incomplete_digest, b"partial-proof")
            .expect("record incomplete proof");
        cas.record_node(root_digest).expect("complete root node");
        cas.record_node(child_digest).expect("complete child node");

        cas.record_root(root).expect("publish root lease");

        assert!(
            cas.node_proof(root_digest)
                .expect("read root proof")
                .is_some()
        );
        assert!(
            cas.node_proof(child_digest)
                .expect("read child proof")
                .is_some()
        );
        assert!(
            cas.node_proof(incomplete_digest)
                .expect("read incomplete proof")
                .is_none()
        );
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn active_disk_closure_can_read_its_input_claim_before_root_publication() {
        let path = temp_directory("active-input");
        let mut cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("open input CAS");
        let root = backend_engine::MerkleRoot::from_admitted_manifest(
            1,
            ObjectVersion::<ImmutableObjectSchema>::from_value(b"active-root"),
        );
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(b"active-input");
        let claim = WireIdentity::from_typed(&version);
        cas.record_input_claim(root, claim)
            .expect("record active input claim");
        assert_eq!(
            cas.input_claim(root).expect("read active input claim"),
            Some(claim)
        );
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn gc_charges_one_global_count_and_byte_budget_per_call() {
        let path = temp_directory("gc-budget");
        let mut cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("open input CAS");
        for index in 0..(MAX_GC_FILES_PER_CALL + 32) {
            let mut digest = [0_u8; 32];
            digest[..8].copy_from_slice(&(index as u64).to_be_bytes());
            fs::write(path.join(hex(digest)), [0_u8; 8]).expect("write stale object");
        }
        let report = cas.reclaim_unleased().expect("bounded GC");
        assert!(report.objects_removed <= MAX_GC_FILES_PER_CALL);
        assert!(report.bytes_removed <= MAX_GC_BYTES_PER_CALL);
        assert!(path.read_dir().expect("read CAS").count() > 0);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn restart_prefers_complete_root_lease_when_marker_rename_was_torn() {
        let path = temp_directory("lease-restart");
        fs::create_dir_all(&path).expect("create lease directory");
        let old = backend_engine::MerkleRoot::from_admitted_manifest(
            1,
            ObjectVersion::<ImmutableObjectSchema>::from_value(&[1_u8; 32]),
        );
        let current = backend_engine::MerkleRoot::from_admitted_manifest(
            1,
            ObjectVersion::<ImmutableObjectSchema>::from_value(&[2_u8; 32]),
        );
        fs::write(
            path.join("ROOT"),
            [
                old.schema().to_be_bytes().as_slice(),
                &old.digest().as_bytes(),
            ]
            .concat(),
        )
        .expect("write old marker");
        fs::write(
            path.join("ROOT_LEASE"),
            [
                current.schema().to_be_bytes().as_slice(),
                &current.digest().as_bytes(),
                &7_u64.to_be_bytes(),
            ]
            .concat(),
        )
        .expect("write current lease");
        let cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("recover leased CAS");
        assert_eq!(cas.durable_root_claim, Some(current.to_claim()));
        assert_eq!(cas.root_epoch, 7);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn restart_ignores_a_newer_compatibility_marker_without_a_new_lease() {
        let path = temp_directory("lease-marker-ahead");
        fs::create_dir_all(&path).expect("create lease directory");
        let leased = backend_engine::MerkleRoot::from_admitted_manifest(
            1,
            ObjectVersion::<ImmutableObjectSchema>::from_value(&[3_u8; 32]),
        );
        let marker = backend_engine::MerkleRoot::from_admitted_manifest(
            1,
            ObjectVersion::<ImmutableObjectSchema>::from_value(&[4_u8; 32]),
        );
        fs::write(
            path.join("ROOT"),
            [
                marker.schema().to_be_bytes().as_slice(),
                &marker.digest().as_bytes(),
            ]
            .concat(),
        )
        .expect("write ahead marker");
        fs::write(
            path.join("ROOT_LEASE"),
            [
                leased.schema().to_be_bytes().as_slice(),
                &leased.digest().as_bytes(),
                &9_u64.to_be_bytes(),
            ]
            .concat(),
        )
        .expect("write authoritative lease");
        let mut cas = InputCas::<ImmutableObjectSchema>::open(&path, TransportLimits::default())
            .expect("recover leased CAS");
        assert_eq!(cas.durable_root_claim, Some(leased.to_claim()));
        assert_eq!(cas.root_epoch, 9);
        cas.record_root(marker).expect("advance root lease");
        assert_eq!(cas.root_epoch, 10);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn sparse_shuffled_extents_resume_across_crashes_and_duplicate_retries() {
        let path = temp_directory("sparse-restart");
        let bytes: &[u8] = b"abcdefghi";
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes);
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes);
        let transfer = TransferId::new(71).expect("transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let frames = chunk_frames(bytes, transfer, key, version, authority);
        let object_path = path.join(hex(version.to_bytes()));

        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[2].clone(), authority)
                .expect("sparse final extent")
        );
        assert!(!object_path.exists(), "incomplete bytes stay unpublished");
        assert!(
            cas.sink.committed.is_empty(),
            "disk staging avoids a full Arc"
        );
        drop(cas);

        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                .expect("sparse first extent")
        );
        assert!(
            !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                .expect("idempotent duplicate retry")
        );
        assert_eq!(cas.partial.len(), 1);
        drop(cas);

        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[2].clone(), authority)
                .expect("retry retained out-of-order extent")
        );
        assert!(
            cas.ingest_authenticated_frame(root, frames[1].clone(), authority)
                .expect("complete shuffled closure object")
        );
        assert!(object_path.is_file());
        assert_eq!(
            fs::read(&object_path).expect("read committed object"),
            bytes
        );
        assert!(cas.contains_claim(WireIdentity::from_typed(&version)));
        assert!(!path.join(format!(".recv-{:016x}", transfer.get())).exists());
        assert!(!path.join(format!(".{:016x}.part", transfer.get())).exists());
        drop(cas);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn tampered_key_claim_never_publishes_complete_bytes() {
        let path = temp_directory("tampered-key");
        let bytes: &[u8] = b"abcdef";
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes);
        let wrong_key = ObjectKey::<ImmutableObjectSchema>::from_value(b"other logical key");
        let transfer = TransferId::new(72).expect("transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let frames = chunk_frames(bytes, transfer, wrong_key, version, authority);
        let object_path = path.join(hex(version.to_bytes()));
        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                .expect("stage first tampered-key extent")
        );
        assert_eq!(
            cas.ingest_authenticated_frame(root, frames[1].clone(), authority),
            Err(ReplicationError::IdentityMismatch)
        );
        assert!(!object_path.exists());
        assert!(!path.join(format!(".{:016x}.part", transfer.get())).exists());
        drop(cas);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn mismatched_retry_cannot_rebind_or_erase_a_staged_partial() {
        let path = temp_directory("mismatched-retry");
        let bytes: &[u8] = b"abcdef";
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes);
        let wrong_key = ObjectKey::<ImmutableObjectSchema>::from_value(b"another key");
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes);
        let transfer = TransferId::new(76).expect("transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let frames = chunk_frames(bytes, transfer, key, version, authority);
        let wrong_frames = chunk_frames(bytes, transfer, wrong_key, version, authority);
        let part = path.join(format!(".{:016x}.part", transfer.get()));
        let marker = path.join(format!(".recv-{:016x}", transfer.get()));
        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                .expect("stage original request")
        );
        drop(cas);

        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert_eq!(
            cas.ingest_authenticated_frame(root, wrong_frames[1].clone(), authority),
            Err(ReplicationError::ReplayConflict)
        );
        assert!(
            part.is_file(),
            "mismatched replay leaves the partial intact"
        );
        assert!(
            marker.is_file(),
            "mismatched replay leaves its checkpoint intact"
        );
        assert!(
            cas.ingest_authenticated_frame(root, frames[1].clone(), authority)
                .expect("continue original request")
        );
        assert!(path.join(hex(version.to_bytes())).is_file());
        drop(cas);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn committed_object_shortcut_cannot_erase_a_different_partial() {
        let path = temp_directory("committed-shortcut-replay");
        let partial_bytes: &[u8] = b"partial";
        let partial_key = ObjectKey::<ImmutableObjectSchema>::from_value(partial_bytes);
        let partial_version = ObjectVersion::<ImmutableObjectSchema>::from_value(partial_bytes);
        let committed_bytes: &[u8] = b"existing";
        let committed_key = ObjectKey::<ImmutableObjectSchema>::from_value(committed_bytes);
        let committed_version = ObjectVersion::<ImmutableObjectSchema>::from_value(committed_bytes);
        let transfer = TransferId::new(77).expect("partial transfer");
        let committed_transfer = TransferId::new(78).expect("committed transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let partial_frames = chunk_frames(
            partial_bytes,
            transfer,
            partial_key,
            partial_version,
            authority,
        );
        let committed_frames = chunk_frames(
            committed_bytes,
            committed_transfer,
            committed_key,
            committed_version,
            authority,
        );
        let committed_replay_frames = chunk_frames(
            committed_bytes,
            transfer,
            committed_key,
            committed_version,
            authority,
        );
        let committed_path = path.join(hex(committed_version.to_bytes()));
        let partial_path = path.join(format!(".{:016x}.part", transfer.get()));
        let marker_path = path.join(format!(".recv-{:016x}", transfer.get()));

        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, partial_version);
        declare_missing(&mut cas, root, committed_version);
        for (index, frame) in committed_frames.iter().cloned().enumerate() {
            let completed = cas
                .ingest_authenticated_frame(root, frame, authority)
                .expect("receive canonical committed object");
            assert_eq!(completed, index + 1 == committed_frames.len());
        }
        cas.record_input_claim(root, WireIdentity::from_typed(&committed_version))
            .expect("retain the committed object under the closure claim");
        cas.record_root(root)
            .expect("durably lease the input closure");
        assert!(
            !cas.ingest_authenticated_frame(root, partial_frames[0].clone(), authority)
                .expect("stage partial object")
        );
        drop(cas);

        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, partial_version);
        declare_missing(&mut cas, root, committed_version);
        assert!(matches!(
            cas.ingest_authenticated_frame(root, committed_replay_frames[0].clone(), authority),
            Err(ReplicationError::ReplayConflict)
        ));
        assert!(partial_path.is_file());
        assert!(marker_path.is_file());
        assert!(
            !cas.ingest_authenticated_frame(root, partial_frames[1].clone(), authority)
                .expect("continue original partial")
        );
        assert!(
            cas.ingest_authenticated_frame(root, partial_frames[2].clone(), authority)
                .expect("finish original partial")
        );
        assert_eq!(
            fs::read(&committed_path).expect("read committed object"),
            committed_bytes
        );
        assert_eq!(
            fs::read(path.join(hex(partial_version.to_bytes()))).expect("read completed partial"),
            partial_bytes
        );
        drop(cas);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn tampered_version_claim_never_publishes_complete_bytes() {
        let path = temp_directory("tampered-version");
        let bytes: &[u8] = b"abcdef";
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes);
        let wrong_version = ObjectVersion::<ImmutableObjectSchema>::from_value(b"other version");
        let transfer = TransferId::new(73).expect("transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let frames = chunk_frames(bytes, transfer, key, wrong_version, authority);
        let object_path = path.join(hex(wrong_version.to_bytes()));
        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, wrong_version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                .expect("stage first tampered-version extent")
        );
        assert_eq!(
            cas.ingest_authenticated_frame(root, frames[1].clone(), authority),
            Err(ReplicationError::IdentityMismatch)
        );
        assert!(!object_path.exists());
        assert!(!path.join(format!(".{:016x}.part", transfer.get())).exists());
        drop(cas);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn overlapping_extent_with_conflicting_placement_is_rejected() {
        let path = temp_directory("overlap");
        let bytes: &[u8] = b"abcdef";
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes);
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes);
        let transfer = TransferId::new(74).expect("transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let first = chunk_frames(bytes, transfer, key, version, authority)
            .into_iter()
            .next()
            .expect("first frame");
        let overlap = Frame::new(
            transfer,
            key,
            version,
            backend_engine::ChunkParts {
                object_len: bytes.len() as u64,
                offset: 2,
                sequence: 1,
                previous_chain: first.chain,
                payload: b"Xde".to_vec(),
            },
            authority,
        )
        .expect("overlap frame");
        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, first, authority)
                .expect("stage first extent")
        );
        assert_eq!(
            cas.ingest_authenticated_frame(root, overlap, authority),
            Err(ReplicationError::ReplayConflict)
        );
        assert!(!path.join(hex(version.to_bytes())).exists());
        drop(cas);
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn torn_temporary_checkpoint_is_ignored_but_torn_published_checkpoint_fails_closed() {
        let path = temp_directory("checkpoint-tear");
        let bytes: &[u8] = b"abcdef";
        let key = ObjectKey::<ImmutableObjectSchema>::from_value(bytes);
        let version = ObjectVersion::<ImmutableObjectSchema>::from_value(bytes);
        let transfer = TransferId::new(75).expect("transfer");
        let root = transfer_root();
        let authority = transfer_authority();
        let frames = chunk_frames(bytes, transfer, key, version, authority);
        let marker = path.join(format!(".recv-{:016x}", transfer.get()));
        let temporary = path.join(format!(".recv-{:016x}.tmp", transfer.get()));

        let mut cas = open_transfer_cas(&path);
        declare_missing(&mut cas, root, version);
        assert!(
            !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                .expect("stage first extent")
        );
        drop(cas);
        fs::write(&temporary, b"torn uncommitted marker").expect("write torn temp marker");
        let cas = open_transfer_cas(&path);
        assert!(!temporary.exists());
        assert_eq!(cas.partial.len(), 1);
        drop(cas);

        let mut marker_bytes = fs::read(&marker).expect("read checkpoint marker");
        let last = marker_bytes.len() - 1;
        marker_bytes[last] ^= 0x80;
        fs::write(&marker, marker_bytes).expect("tear published marker");
        assert!(matches!(
            InputCas::<ImmutableObjectSchema>::open(&path, transfer_limits()),
            Err(ReplicationError::CorruptFrame)
        ));
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn interrupted_transfers_remain_charged_to_object_and_byte_budgets() {
        let path = temp_directory("partial-budget");
        let limits = TransportLimits {
            max_object: 6,
            max_objects: 1,
            ..transfer_limits()
        };
        let root = transfer_root();
        let authority = transfer_authority();
        for index in 0..3_u64 {
            let mut cas =
                InputCas::<ImmutableObjectSchema>::open(&path, limits).expect("open budgeted CAS");
            let bytes = [b'a' + u8::try_from(index).expect("small index"); 6];
            let key = ObjectKey::<ImmutableObjectSchema>::from_value(&bytes);
            let version = ObjectVersion::<ImmutableObjectSchema>::from_value(&bytes);
            let transfer = TransferId::new(80 + index).expect("transfer");
            let frames = chunk_frames(&bytes, transfer, key, version, authority);
            declare_missing(&mut cas, root, version);
            if index < 2 {
                assert!(
                    !cas.ingest_authenticated_frame(root, frames[0].clone(), authority)
                        .expect("stage a budgeted partial object")
                );
            } else {
                assert_eq!(
                    cas.ingest_authenticated_frame(root, frames[0].clone(), authority),
                    Err(ReplicationError::Backpressure),
                    "two durable partials consume the configured two-object budget"
                );
            }
            drop(cas);
        }

        let cas = InputCas::<ImmutableObjectSchema>::open(&path, limits)
            .expect("reopen within partial retention budget");
        assert_eq!(cas.partial.len(), 2);
        for transfer in [80_u64, 81] {
            assert!(path.join(format!(".{transfer:016x}.part")).is_file());
            assert!(path.join(format!(".recv-{transfer:016x}")).is_file());
        }
        assert!(!path.join(".0000000000000052.part").exists());
        drop(cas);
        let _ = fs::remove_dir_all(path);
    }
}

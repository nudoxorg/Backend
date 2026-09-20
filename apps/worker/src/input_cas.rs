//! Worker input reception built on the canonical sparse transfer typestate.
//!
//! `ReceivingCas` owns all identity, range, replay, chain, coverage, and
//! final digest checks. This adapter supplies only the durable extent store;
//! it deliberately does not define a second transfer protocol.

use crate::closure_index::DurableNodeIndex;
#[path = "durable_sink.rs"]
mod durable_sink;
use backend_engine::{
    AdmittedChunk, AuthorityClaim, CanonicalDigest, CheckedWorkspaceManifest, Frame,
    ImmutableObjectSchema, ObjectKey, ObjectRequest, ObjectVersion, ReceivingCas,
    ReceivingCheckpoint, ReplicationError, Schema, TransferId, TransportLimits,
    UntrustedWorkspaceManifest, WireIdentity, WorkspaceRoot,
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
        let active = std::mem::take(&mut self.active);
        for (_, session) in active {
            session.abort(&mut self.sink);
        }
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
        let workspace_manifest_bytes = match fs::read(path.join("WORKSPACE_MANIFEST")) {
            Ok(bytes) => {
                UntrustedWorkspaceManifest::decode_untrusted(&bytes)
                    .map_err(|_| ReplicationError::CorruptFrame)?;
                Some(bytes)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(ReplicationError::Disconnected),
        };
        let (max_retained_objects, max_retained_bytes) = retention_budget(limits);
        let sink = DurableSink::open_with_budget(path, max_retained_objects, max_retained_bytes)?;
        let mut cas = Self {
            sink,
            active: BTreeMap::new(),
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
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ReplicationError::Disconnected),
    };
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| ReplicationError::CorruptFrame)?;
    Ok(Some(bytes))
}

fn read_root_marker(
    path: &Path,
) -> Result<Option<backend_engine::MerkleRootClaim>, ReplicationError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ReplicationError::Disconnected),
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
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if root.is_none() {
                Ok((None, 0))
            } else {
                // A marker without a complete lease has no restart proof.
                // Fail closed rather than treating an unleased root as live.
                Err(ReplicationError::CorruptFrame)
            };
        }
        Err(_) => return Err(ReplicationError::Disconnected),
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

fn digest_file<T: Schema>(path: &Path, len: u64) -> Result<[u8; 32], ReplicationError> {
    let mut file = File::open(path).map_err(|_| ReplicationError::Disconnected)?;
    let mut digest = CanonicalDigest::<T>::new(len);
    let mut offset = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| ReplicationError::Disconnected)?;
        if read == 0 {
            break;
        }
        digest.push(offset, &buffer[..read])?;
        offset = offset
            .checked_add(read as u64)
            .ok_or(ReplicationError::Overflow)?;
    }
    digest.finish()
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
}

//! Additive V3 bridge-index model. This is deliberately not wired into the
//! production history selector: it exercises a typed path-copy map, exact
//! FileStore closure, and a small single-writer crash-safe commit/ref protocol
//! in isolation.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use backend_replication::ProducedSemanticObjectKind;
use backend_store::{
    ArtifactClosureClaim, ClosureManifest, ClosureMembershipChange, FileStore, GcLimits,
    GcPinGuard, GcRoot, ObjectId, RelationAdmissionRegistry, RelationNodeRead, StoreError,
    TypedObject, UntrustedObjectId,
};
use backend_version::{
    CanonicalRelation, ClosedRelationScope, CoverageWitness, IdContext, LazyTree,
    LazyTreeMetadataShape, LazyTreeUpdateBudget, ObjectKey, Relation, RelationDecodeError,
    RelationState, Schema, SchemaIdentity, ScopeRoot, StateRoot, TreeChange, TreeNodeLoader,
    UntrustedId,
};

const STORE_MAX_BYTES: usize = 1024 * 1024;
const MAX_ROWS: usize = 20_000;
const MAX_CLOSURE_MEMBERS: usize = MAX_ROWS * 2;
const PAGE_ROWS: usize = 48;
const V3_ROOT_MAGIC: &[u8; 8] = b"V3BRIDGE";
const FLAT_V2_LOCATOR_MODEL_BYTES: usize = 18_900_000;
const FLAT_V2_LOCATOR_MODEL_ROWS: usize = 65_536;
const MAX_COLD_PAYLOAD_BYTES: u64 = 512 * 1024 * 1024;
const RELATION_DOMAIN: u8 = 0x73;
const RELATION_TYPE: u16 = 0x0301;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BridgeKey {
    family: u8,
    first_key: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BridgeValue {
    last_key: [u8; 32],
    semantic_id: [u8; 32],
    physical_id: [u8; 32],
    kind: u8,
    schema: SchemaIdentity,
    byte_length: u64,
}

#[derive(Clone, Copy)]
struct BridgeRelation;

impl Relation for BridgeRelation {
    const DOMAIN: u8 = RELATION_DOMAIN;
    const TYPE: u16 = RELATION_TYPE;
    type Key = BridgeKey;
    type Value = BridgeValue;

    fn encode_key(key: &Self::Key, out: &mut Vec<u8>) {
        out.push(key.family);
        out.extend_from_slice(&key.first_key);
    }

    fn encode_value(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(&value.last_key);
        out.extend_from_slice(&value.semantic_id);
        out.extend_from_slice(&value.physical_id);
        out.push(value.kind);
        out.push(value.schema.domain());
        out.extend_from_slice(&value.schema.ty().to_be_bytes());
        out.push(value.schema.version());
        out.extend_from_slice(&value.byte_length.to_be_bytes());
    }
}

impl CanonicalRelation for BridgeRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        if bytes.len() != 33 {
            return Err(RelationDecodeError::Malformed);
        }
        let mut first_key = [0; 32];
        first_key.copy_from_slice(&bytes[1..]);
        Ok(BridgeKey {
            family: bytes[0],
            first_key,
        })
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        if bytes.len() != 109 {
            return Err(RelationDecodeError::Malformed);
        }
        let mut last_key = [0; 32];
        last_key.copy_from_slice(&bytes[0..32]);
        let mut semantic_id = [0; 32];
        semantic_id.copy_from_slice(&bytes[32..64]);
        let mut physical_id = [0; 32];
        physical_id.copy_from_slice(&bytes[64..96]);
        let schema = SchemaIdentity::new(
            bytes[97],
            u16::from_be_bytes([bytes[98], bytes[99]]),
            bytes[100],
        );
        let byte_length = u64::from_be_bytes(
            bytes[101..109]
                .try_into()
                .map_err(|_| RelationDecodeError::Malformed)?,
        );
        Ok(BridgeValue {
            last_key,
            semantic_id,
            physical_id,
            kind: bytes[96],
            schema,
            byte_length,
        })
    }
}

struct TestSegmentPayload;

impl Schema for TestSegmentPayload {
    const DOMAIN: u8 = ProducedSemanticObjectKind::Segment
        .schema_identity()
        .domain();
    const TYPE: u16 = ProducedSemanticObjectKind::Segment.schema_identity().ty();
    const VERSION: u8 = ProducedSemanticObjectKind::Segment
        .schema_identity()
        .version();
    type Value = [u8];

    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PhysicalKind {
    Segment = 1,
    JumboLeaf = 2,
    JumboInterior = 3,
}

impl PhysicalKind {
    fn schema(self) -> SchemaIdentity {
        match self {
            Self::Segment => ProducedSemanticObjectKind::Segment.schema_identity(),
            Self::JumboLeaf => ProducedSemanticObjectKind::JumboLeaf.schema_identity(),
            Self::JumboInterior => ProducedSemanticObjectKind::JumboInterior.schema_identity(),
        }
    }

    fn from_wire(value: u8) -> Result<Self, String> {
        match value {
            1 => Ok(Self::Segment),
            2 => Ok(Self::JumboLeaf),
            3 => Ok(Self::JumboInterior),
            _ => Err("bridge row has an unknown closed object kind".to_owned()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommitBody {
    parent: Option<[u8; 32]>,
    generation_root: [u8; 32],
    bridge_root: [u8; 32],
    closure_id: [u8; 32],
}

struct VerifiedReplayToken {
    commit_id: [u8; 32],
    generation_root: [u8; 32],
    bridge_root: [u8; 32],
    closure_id: [u8; 32],
    row_count: usize,
    _gc_pin: GcPinGuard,
}

#[derive(Clone, Copy, Debug, Default)]
struct ReadMetrics {
    bridge_node_bytes: usize,
    closure_index_bytes: usize,
    payload_bytes: usize,
    bridge_node_reads: usize,
    closure_index_node_reads: usize,
    payload_reads: usize,
    rss_before_kib: Option<usize>,
    rss_after_kib: Option<usize>,
}

#[derive(Clone, Copy, Debug, Default)]
struct WriteMetrics {
    lazy_frontier_bytes: usize,
    relation_object_bytes: usize,
    relation_nodes_written: usize,
    relation_node_reads: usize,
    relation_node_read_bytes: usize,
    payload_object_bytes_written: usize,
    closure_index_bytes: u64,
    closure_verified_payload_bytes: u64,
    commit_body_bytes: usize,
    ref_bytes: usize,
    loaded_nodes: usize,
    rebuilt_nodes: usize,
    peak_metadata_bytes: usize,
    rss_before_kib: Option<usize>,
    rss_after_kib: Option<usize>,
    index_metadata_bytes_written: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CrashPoint {
    None,
    AfterNodePublish,
    AfterCommitRoot,
    AfterRefCas,
}

#[derive(Clone, Debug)]
struct CrashArtifacts {
    point: CrashPoint,
    candidate_commit: Option<[u8; 32]>,
    candidate_bridge_root: [u8; 32],
    candidate_payload_ids: Vec<[u8; 32]>,
}

struct CountedLoader<'a> {
    store: &'a FileStore,
    reads: RefCell<Vec<([u8; 32], ObjectId, usize)>>,
    loaded_nodes: RefCell<BTreeMap<[u8; 32], RelationNodeRead<BridgeRelation>>>,
}

impl<'a> CountedLoader<'a> {
    fn new(store: &'a FileStore) -> Self {
        Self {
            store,
            reads: RefCell::new(Vec::new()),
            loaded_nodes: RefCell::new(BTreeMap::new()),
        }
    }

    fn read_metrics(&self) -> (usize, usize) {
        let reads = self.reads.borrow();
        (reads.len(), reads.iter().map(|(_, _, bytes)| *bytes).sum())
    }

    fn loaded_frontier(&self) -> Vec<([u8; 32], ObjectId)> {
        let mut nodes = BTreeMap::new();
        for (version, object, _) in self.reads.borrow().iter() {
            nodes.insert(*version, *object);
        }
        nodes
            .into_iter()
            .map(|(version, object)| (version, object))
            .collect()
    }

    fn read_node(
        &self,
        claim: UntrustedId<BridgeRelation>,
    ) -> Result<RelationNodeRead<BridgeRelation>, StoreError> {
        let read = self.store.read_relation_node_with_children(claim)?;
        let version = read.node().root().to_bytes();
        let object = read.object();
        let bytes = read.node().bytes().len();
        self.reads.borrow_mut().push((version, object, bytes));
        Ok(read)
    }

    fn take_loaded_or_read(
        &self,
        version: [u8; 32],
    ) -> Result<RelationNodeRead<BridgeRelation>, StoreError> {
        if let Some(read) = self.loaded_nodes.borrow_mut().remove(&version) {
            return Ok(read);
        }
        self.read_node(relation_claim(&version))
    }
}

impl TreeNodeLoader<BridgeRelation> for CountedLoader<'_> {
    type Error = StoreError;

    fn load(
        &self,
        claim: UntrustedId<BridgeRelation>,
    ) -> Result<backend_version::CheckedCanonicalRoot<BridgeRelation>, Self::Error> {
        let read = self.read_node(claim)?;
        let version = read.node().root().to_bytes();
        self.loaded_nodes.borrow_mut().insert(version, read.clone());
        Ok(read.node().clone())
    }
}

struct TempStore {
    path: PathBuf,
}

impl TempStore {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "backend-v3-bridge-{label}-{}-{serial}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temporary V3 bridge root");
        Self { path }
    }

    fn open_store(&self, registry: RelationAdmissionRegistry) -> FileStore {
        FileStore::open_with_registry(&self.path.join("cas"), STORE_MAX_BYTES, registry)
            .expect("open V3 bridge FileStore")
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn registry() -> RelationAdmissionRegistry {
    RelationAdmissionRegistry::new()
        .with_relation::<BridgeRelation>()
        .expect("register bridge relation")
}

fn relation_shape() -> LazyTreeMetadataShape {
    LazyTreeMetadataShape::new(33, 109, 64, 256, 6, 12)
}

fn payload_bytes(family: u8, ordinal: usize, salt: u64) -> Vec<u8> {
    let mut payload = vec![0; 96];
    payload[..8].copy_from_slice(&(ordinal as u64).to_be_bytes());
    payload[8..16].copy_from_slice(&salt.to_be_bytes());
    payload[16] = family;
    for (offset, byte) in payload[17..].iter_mut().enumerate() {
        *byte = (ordinal.wrapping_mul(31).wrapping_add(offset) as u8) ^ (salt as u8);
    }
    payload
}

fn first_key(ordinal: usize) -> [u8; 32] {
    let mut key = [0; 32];
    key[24..]
        .copy_from_slice(&(u64::try_from(ordinal).expect("bounded ordinal") * 2).to_be_bytes());
    key
}

fn last_key(ordinal: usize) -> [u8; 32] {
    let mut key = first_key(ordinal);
    let tail = u64::from_be_bytes(key[24..].try_into().expect("fixed key width"));
    key[24..].copy_from_slice(&(tail + 1).to_be_bytes());
    key
}

fn make_payload(
    store: &FileStore,
    family: u8,
    ordinal: usize,
    salt: u64,
) -> (TypedObject, BridgeValue, usize) {
    let bytes = payload_bytes(family, ordinal, salt);
    let key = ObjectKey::<TestSegmentPayload>::from_value(bytes.as_slice());
    let object = TypedObject::from_value(&key, bytes.as_slice());
    assert_eq!(
        object.schema(),
        ProducedSemanticObjectKind::Segment.schema_identity(),
        "test schema must follow the producer's closed segment-kind mapping"
    );
    let receipt = store
        .write_object_with_receipt(&object)
        .expect("write immutable segment");
    let id = receipt.id();
    let written_bytes = if receipt.created() {
        usize::try_from(receipt.bytes()).expect("bounded encoded object length")
    } else {
        0
    };
    let semantic_id = semantic_segment_id(family, ordinal, &bytes);
    (
        object,
        BridgeValue {
            last_key: last_key(ordinal),
            semantic_id,
            physical_id: *id.as_bytes(),
            kind: PhysicalKind::Segment as u8,
            schema: ProducedSemanticObjectKind::Segment.schema_identity(),
            byte_length: u64::try_from(bytes.len()).expect("bounded payload length"),
        },
        written_bytes,
    )
}

fn semantic_segment_id(family: u8, ordinal: usize, bytes: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.history.v3.synthetic-segment.v1\0");
    hasher.update(&[family]);
    hasher.update(&first_key(ordinal));
    hasher.update(&last_key(ordinal));
    hasher.update(bytes);
    *hasher.finalize().as_bytes()
}

fn semantic_root(rows: &BTreeMap<BridgeKey, BridgeValue>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.history.v3.synthetic-generation.v1\0");
    for (key, value) in rows {
        hasher.update(&[key.family]);
        hasher.update(&key.first_key);
        hasher.update(&value.last_key);
        hasher.update(&value.semantic_id);
        hasher.update(&[value.kind, value.schema.domain()]);
        hasher.update(&value.schema.ty().to_be_bytes());
        hasher.update(&[value.schema.version()]);
        hasher.update(&value.byte_length.to_be_bytes());
    }
    *hasher.finalize().as_bytes()
}

fn initial_rows(
    store: &FileStore,
    count: usize,
) -> (BTreeMap<BridgeKey, BridgeValue>, Vec<TypedObject>) {
    let mut rows = BTreeMap::new();
    let mut payloads = Vec::with_capacity(count);
    for index in 0..count {
        let family = u8::try_from(index % 7).expect("seven families");
        let ordinal = index / 7;
        let (object, value, _) = make_payload(store, family, ordinal, 0);
        rows.insert(
            BridgeKey {
                family,
                first_key: first_key(ordinal),
            },
            value,
        );
        payloads.push(object);
    }
    (rows, payloads)
}

fn make_closure(
    state: &RelationState<BridgeRelation>,
    payloads: &[TypedObject],
    registry: &RelationAdmissionRegistry,
) -> ClosureManifest {
    let mut members = ClosureManifest::for_relation_state_with_registry(state, registry)
        .expect("build checked bridge relation closure")
        .objects()
        .to_vec();
    members.extend_from_slice(payloads);
    members.sort_by_key(|object| (object.schema(), *object.key(), *object.version()));
    ClosureManifest::new_with_registry(members, registry).expect("admit exact bridge closure")
}

fn initialize_history(
    store: &FileStore,
    root_dir: &Path,
    rows: BTreeMap<BridgeKey, BridgeValue>,
    payloads: &[TypedObject],
    registry: &RelationAdmissionRegistry,
) -> ([u8; 32], CommitBody, StateRoot<BridgeRelation>) {
    let state = RelationState::<BridgeRelation>::from_entries(
        rows.iter().map(|(key, value)| (*key, *value)),
        CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(
            ScopeRoot::from_u64(0),
        )),
    )
    .expect("build initial canonical bridge state");
    let bridge_root = state.root();
    let closure_manifest = make_closure(&state, payloads, registry);
    let closure_id = store
        .write_closure(&closure_manifest)
        .expect("write initial complete closure");
    let body = CommitBody {
        parent: None,
        generation_root: semantic_root(&rows),
        bridge_root: bridge_root.to_bytes(),
        closure_id: *closure_id.as_bytes(),
    };
    let commit_id = write_commit(root_dir, &body).expect("write genesis commit body");
    update_ref_cas(root_dir, None, commit_id).expect("publish genesis history ref");
    (commit_id, body, bridge_root)
}

fn commit_wire(body: &CommitBody) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8 + 1 + 32 * 4 + 32);
    bytes.extend_from_slice(V3_ROOT_MAGIC);
    bytes.push(u8::from(body.parent.is_some()));
    bytes.extend_from_slice(&body.parent.unwrap_or([0; 32]));
    bytes.extend_from_slice(&body.generation_root);
    bytes.extend_from_slice(&body.bridge_root);
    bytes.extend_from_slice(&body.closure_id);
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.history.v3.commit-body.v1\0");
    hasher.update(&bytes);
    bytes.extend_from_slice(hasher.finalize().as_bytes());
    bytes
}

fn decode_commit(bytes: &[u8]) -> Result<CommitBody, String> {
    if bytes.len() != 8 + 1 + 32 * 4 + 32 || &bytes[..8] != V3_ROOT_MAGIC {
        return Err("malformed V3 commit body".to_owned());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.history.v3.commit-body.v1\0");
    hasher.update(&bytes[..8 + 1 + 32 * 4]);
    if hasher.finalize().as_bytes() != &bytes[8 + 1 + 32 * 4..] {
        return Err("V3 commit body digest mismatch".to_owned());
    }
    let has_parent = match bytes[8] {
        0 => false,
        1 => true,
        _ => return Err("invalid V3 parent marker".to_owned()),
    };
    let parent_bytes: [u8; 32] = bytes[9..41]
        .try_into()
        .map_err(|_| "invalid V3 parent width".to_owned())?;
    if !has_parent && parent_bytes != [0; 32] {
        return Err("genesis commit has a nonzero parent claim".to_owned());
    }
    let generation_root = bytes[41..73]
        .try_into()
        .map_err(|_| "invalid V3 generation-root width".to_owned())?;
    let bridge_root = bytes[73..105]
        .try_into()
        .map_err(|_| "invalid V3 bridge-root width".to_owned())?;
    let closure_id = bytes[105..137]
        .try_into()
        .map_err(|_| "invalid V3 closure-root width".to_owned())?;
    Ok(CommitBody {
        parent: has_parent.then_some(parent_bytes),
        generation_root,
        bridge_root,
        closure_id,
    })
}

fn commit_id(wire: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.history.v3.commit-id.v1\0");
    hasher.update(wire);
    *hasher.finalize().as_bytes()
}

fn write_commit(root_dir: &Path, body: &CommitBody) -> Result<[u8; 32], String> {
    let history_dir = root_dir.join("history").join("commits");
    fs::create_dir_all(&history_dir).map_err(|error| error.to_string())?;
    let bytes = commit_wire(body);
    let id = commit_id(&bytes);
    let path = history_dir.join(hex(&id));
    write_immutable(&path, &bytes)?;
    Ok(id)
}

fn read_commit(root_dir: &Path, id: [u8; 32]) -> Result<CommitBody, String> {
    let path = root_dir.join("history").join("commits").join(hex(&id));
    let bytes = read_fixed_record(&path, 8 + 1 + 32 * 4 + 32)?
        .ok_or_else(|| "missing V3 commit body".to_owned())?;
    if commit_id(&bytes) != id {
        return Err("commit path does not match immutable body identity".to_owned());
    }
    decode_commit(&bytes)
}

fn write_immutable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(current) = read_fixed_record(path, bytes.len())? {
        return if current == bytes {
            Ok(())
        } else {
            Err("immutable V3 record collision".to_owned())
        };
    }
    let parent = path
        .parent()
        .ok_or_else(|| "record has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let temp = parent.join(format!(".tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .or_else(|_| {
            let _ = fs::remove_file(&temp);
            OpenOptions::new().write(true).create_new(true).open(&temp)
        })
        .map_err(|error| error.to_string())?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temp, path).map_err(|error| error.to_string())?;
    sync_dir(parent)
}

fn update_ref_cas(
    root_dir: &Path,
    expected: Option<[u8; 32]>,
    target: [u8; 32],
) -> Result<usize, String> {
    let refs = root_dir.join("history").join("refs");
    fs::create_dir_all(&refs).map_err(|error| error.to_string())?;
    let path = refs.join("main");
    let current = read_fixed_record(&path, 32)?
        .map(|bytes| bytes.try_into().expect("checked fixed ref width"));
    if current == Some(target) {
        return Ok(0);
    }
    if current != expected {
        return Err("V3 history ref compare-and-swap failed".to_owned());
    }
    let temp = refs.join(format!(".main-tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temp)
        .map_err(|error| error.to_string())?;
    file.write_all(&target).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())?;
    fs::rename(&temp, &path).map_err(|error| error.to_string())?;
    sync_dir(&refs)?;
    Ok(32)
}

fn current_ref(root_dir: &Path) -> Result<[u8; 32], String> {
    read_fixed_record(&root_dir.join("history").join("refs").join("main"), 32)?
        .ok_or_else(|| "missing V3 history ref".to_owned())?
        .try_into()
        .map_err(|_| "invalid V3 ref width".to_owned())
}

fn read_fixed_record(path: &Path, expected_bytes: usize) -> Result<Option<Vec<u8>>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_file()
        || metadata.len() != u64::try_from(expected_bytes).unwrap_or(u64::MAX)
    {
        return Err("fixed-size V3 record has the wrong file type or length".to_owned());
    }
    let file = File::open(path).map_err(|error| error.to_string())?;
    let opened = file.metadata().map_err(|error| error.to_string())?;
    if !opened.is_file() || opened.len() != u64::try_from(expected_bytes).unwrap_or(u64::MAX) {
        return Err("fixed-size V3 record changed while opening".to_owned());
    }
    let read_limit = u64::try_from(expected_bytes)
        .map_err(|_| "V3 record size exceeds platform bounds".to_owned())?
        .checked_add(1)
        .ok_or_else(|| "V3 record read bound overflow".to_owned())?;
    let mut bytes = Vec::with_capacity(expected_bytes);
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() != expected_bytes {
        return Err("fixed-size V3 record changed while reading".to_owned());
    }
    Ok(Some(bytes))
}

fn sync_dir(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| error.to_string())
}

fn current_rss_kib() -> Option<usize> {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
        return line.split_whitespace().nth(1)?.parse().ok();
    }
    #[cfg(not(target_os = "linux"))]
    {
        let pid = std::process::id().to_string();
        let output = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", pid.as_str()])
            .output()
            .ok()?;
        String::from_utf8(output.stdout).ok()?.trim().parse().ok()
    }
}

fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(TABLE[usize::from(byte >> 4)]));
        output.push(char::from(TABLE[usize::from(byte & 0x0f)]));
    }
    output
}

fn relation_claim(bytes: &[u8; 32]) -> UntrustedId<BridgeRelation> {
    UntrustedId::<BridgeRelation>::from_wire(bytes, IdContext::relation::<BridgeRelation>())
        .expect("decode typed bridge-root claim")
}

fn read_ref_body(root_dir: &Path) -> Result<([u8; 32], CommitBody), String> {
    let id = current_ref(root_dir)?;
    Ok((id, read_commit(root_dir, id)?))
}

fn closure_claim(body: &CommitBody) -> ArtifactClosureClaim {
    ArtifactClosureClaim::from_bytes(body.closure_id)
}

fn verify_cold(
    store: &FileStore,
    root_dir: &Path,
    commit: [u8; 32],
    body: &CommitBody,
) -> Result<
    (
        VerifiedReplayToken,
        ReadMetrics,
        BTreeMap<BridgeKey, BridgeValue>,
    ),
    String,
> {
    let rss_before_kib = current_rss_kib();
    let gc_pin = store
        .pin_garbage_collection()
        .map_err(|error| format!("pin closure during cold V3 verification: {error:?}"))?;
    if read_commit(root_dir, commit)? != *body {
        return Err("cold commit body differs from supplied root record".to_owned());
    }
    if let Some(parent) = body.parent {
        let _ = read_commit(root_dir, parent)
            .map_err(|error| format!("admit immutable parent commit body: {error}"))?;
    }
    let closure = store
        .admit_closure_claim(closure_claim(body))
        .map_err(|error| format!("admit typed bridge closure: {error:?}"))?;
    if closure.as_bytes() != &body.closure_id {
        return Err("closure descriptor differs from commit claim".to_owned());
    }
    let manifest = store
        .open_closure(closure)
        .map_err(|error| format!("open exact typed bridge closure: {error:?}"))?;
    if manifest.object_count() > u64::try_from(MAX_CLOSURE_MEMBERS).unwrap_or(u64::MAX) {
        return Err("bridge closure exceeds test replay bound".to_owned());
    }

    let loader = CountedLoader::new(store);
    let tree = LazyTree::<BridgeRelation, _>::open(&loader, relation_claim(&body.bridge_root))
        .map_err(|error| format!("admit typed bridge root: {error:?}"))?;
    if tree.root().row_count() > u64::try_from(MAX_ROWS).unwrap_or(u64::MAX) {
        return Err("typed bridge root exceeds the bounded row count".to_owned());
    }
    let root_node = loader
        .loaded_frontier()
        .into_iter()
        .find(|(version, _)| version == &body.bridge_root)
        .ok_or_else(|| "typed bridge root was not loaded".to_owned())?;
    let (root_member, root_member_stats) = manifest
        .get_with_stats(root_node.1)
        .map_err(|error| format!("check bridge root closure membership: {error:?}"))?;
    let Some(root_member) = root_member else {
        return Err("bridge root node is absent from its claimed closure".to_owned());
    };
    if root_member.version() != &body.bridge_root {
        return Err("closure root member does not match the claimed bridge root".to_owned());
    }
    let mut metrics = ReadMetrics::default();
    metrics.closure_index_node_reads = root_member_stats.nodes_read;
    metrics.closure_index_bytes = root_member_stats.bytes_read;

    let mut expected_members = BTreeSet::new();
    let mut queue = VecDeque::from([body.bridge_root]);
    let mut visited_versions = BTreeSet::new();
    let mut rows = BTreeMap::new();
    let mut charged_payload_bytes = 0_u64;
    let mut family_rows = [0_usize; 7];
    let mut prior_range: Option<(u8, [u8; 32])> = None;
    while let Some(version) = queue.pop_front() {
        if !visited_versions.insert(version) {
            continue;
        }
        let read = loader
            .take_loaded_or_read(version)
            .map_err(|error| format!("reopen reachable bridge node: {error:?}"))?;
        expected_members.insert(read.object());
        if read.node().node().level() == 0 {
            for (key, value) in read
                .node()
                .leaf_entries()
                .map_err(|error| format!("decode admitted bridge leaf: {error:?}"))?
            {
                let family_count = family_rows
                    .get_mut(usize::from(key.family))
                    .ok_or_else(|| "bridge row names a family outside the closed set".to_owned())?;
                *family_count = family_count
                    .checked_add(1)
                    .ok_or_else(|| "family row counter overflow".to_owned())?;
                validate_range(key, value, &mut prior_range)?;
                let kind = PhysicalKind::from_wire(value.kind)?;
                if value.schema != kind.schema() {
                    return Err("bridge kind is paired with the wrong physical schema".to_owned());
                }
                if kind != PhysicalKind::Segment {
                    return Err(
                        "prototype has no jumbo semantic closure verifier; reject closed jumbo kinds".to_owned(),
                    );
                }
                let ordinal = usize::try_from(
                    u64::from_be_bytes(
                        key.first_key[24..]
                            .try_into()
                            .map_err(|_| "invalid bridge stable key width".to_owned())?,
                    ) / 2,
                )
                .map_err(|_| "stable ordinal exceeds the test bound".to_owned())?;
                if key.first_key != first_key(ordinal) || value.last_key != last_key(ordinal) {
                    return Err("segment row range is not canonical for its stable key".to_owned());
                }
                let next_payload_bytes = charged_payload_bytes
                    .checked_add(value.byte_length)
                    .ok_or_else(|| "cumulative payload read budget overflow".to_owned())?;
                if next_payload_bytes > MAX_COLD_PAYLOAD_BYTES {
                    return Err("cold semantic payload budget exceeded".to_owned());
                }
                let object = store
                    .read_object_claim(UntrustedObjectId::from_bytes(value.physical_id))
                    .map_err(|error| format!("verify bridge payload envelope: {error:?}"))?;
                if object.id().as_bytes() != &value.physical_id
                    || object.schema() != value.schema
                    || u64::try_from(object.bytes().len()).ok() != Some(value.byte_length)
                {
                    return Err("bridge row physical identity/schema/length mismatch".to_owned());
                }
                if semantic_segment_id(key.family, ordinal, object.bytes()) != value.semantic_id {
                    return Err("semantic segment id does not match exact payload/range".to_owned());
                }
                if rows.insert(key, value).is_some() {
                    return Err("duplicate bridge range key".to_owned());
                }
                metrics.payload_reads += 1;
                metrics.payload_bytes = metrics
                    .payload_bytes
                    .checked_add(object.bytes().len())
                    .ok_or_else(|| "payload byte counter overflow".to_owned())?;
                charged_payload_bytes = next_payload_bytes;
                expected_members.insert(object.id());
            }
        } else {
            for child in read
                .node()
                .child_summaries()
                .map_err(|error| format!("decode authenticated bridge branch: {error:?}"))?
            {
                queue.push_back(*child.commitment.as_bytes());
            }
        }
    }
    if family_rows.contains(&0) {
        return Err("typed V3 bridge is missing a required semantic family".to_owned());
    }
    if semantic_root(&rows) != body.generation_root {
        return Err("typed bridge rows disagree with semantic generation root".to_owned());
    }

    let mut closure_members = Vec::new();
    let mut after = None;
    loop {
        let page = manifest
            .page_ids(after, PAGE_ROWS)
            .map_err(|error| format!("page exact closure ids: {error:?}"))?;
        let stats = page.stats();
        metrics.closure_index_node_reads += stats.nodes_read;
        metrics.closure_index_bytes = metrics
            .closure_index_bytes
            .checked_add(stats.bytes_read)
            .ok_or_else(|| "closure-index byte counter overflow".to_owned())?;
        closure_members.extend_from_slice(page.object_ids());
        match page.next() {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    let closure_set = closure_members.into_iter().collect::<BTreeSet<_>>();
    if closure_set != expected_members {
        return Err(
            "closure members are not the exact bridge-node/payload reachability set".to_owned(),
        );
    }
    let (bridge_node_reads, bridge_node_bytes) = loader.read_metrics();
    metrics.bridge_node_reads = bridge_node_reads;
    metrics.bridge_node_bytes = bridge_node_bytes;
    let token = VerifiedReplayToken {
        commit_id: commit,
        generation_root: body.generation_root,
        bridge_root: body.bridge_root,
        closure_id: body.closure_id,
        row_count: rows.len(),
        _gc_pin: gc_pin,
    };
    metrics.rss_before_kib = rss_before_kib;
    metrics.rss_after_kib = current_rss_kib();
    Ok((token, metrics, rows))
}

fn validate_range(
    key: BridgeKey,
    value: BridgeValue,
    prior: &mut Option<(u8, [u8; 32])>,
) -> Result<(), String> {
    if key.first_key > value.last_key {
        return Err("bridge range ends before it starts".to_owned());
    }
    if let Some((family, last)) = prior {
        if *family > key.family || (*family == key.family && key.first_key <= *last) {
            return Err("bridge ranges overlap or are not increasing".to_owned());
        }
    }
    *prior = Some((key.family, value.last_key));
    Ok(())
}

fn lookup_verified(
    store: &FileStore,
    token: &VerifiedReplayToken,
    expected_commit: [u8; 32],
    expected_generation_root: [u8; 32],
    expected_bridge_root: [u8; 32],
    keys: &[BridgeKey],
) -> Result<(Vec<Option<BridgeValue>>, usize, usize), String> {
    if token.commit_id != expected_commit
        || token.generation_root != expected_generation_root
        || token.bridge_root != expected_bridge_root
    {
        return Err("replay token does not name the requested commit/version".to_owned());
    }
    let loader = CountedLoader::new(store);
    let tree = LazyTree::<BridgeRelation, _>::open(&loader, relation_claim(&token.bridge_root))
        .map_err(|error| format!("open token bridge root: {error:?}"))?;
    if tree.root().root().to_bytes() != token.bridge_root {
        return Err("replay token root changed".to_owned());
    }
    let values = tree
        .lookup_many_sorted(keys)
        .map_err(|error| format!("bounded sorted bridge lookup: {error:?}"))?;
    let (node_reads, node_bytes) = loader.read_metrics();
    if token.commit_id == [0; 32]
        || token.generation_root == [0; 32]
        || token.closure_id == [0; 32]
        || token.row_count == 0
    {
        return Err("unverified replay token".to_owned());
    }
    Ok((values, node_reads, node_bytes))
}

fn changed_tree<'store>(
    store: &'store FileStore,
    base_root: [u8; 32],
    changes: &[(BridgeKey, Option<BridgeValue>)],
) -> Result<
    (
        backend_version::LazyPreparedUpdate<BridgeRelation>,
        CountedLoader<'store>,
        Vec<TypedObject>,
        Vec<(BridgeKey, BridgeValue)>,
    ),
    String,
> {
    let loader = CountedLoader::new(store);
    let tree = LazyTree::<BridgeRelation, _>::open(&loader, relation_claim(&base_root))
        .map_err(|error| format!("open base bridge tree: {error:?}"))?;
    if changes.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
        return Err("bridge batch must be strictly sorted".to_owned());
    }
    let keys = changes.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let prior = tree
        .lookup_many_sorted(&keys)
        .map_err(|error| format!("read bridge before values: {error:?}"))?;
    let mut tree_changes = Vec::with_capacity(changes.len());
    let mut before_values = Vec::with_capacity(changes.len());
    for ((key, after), before) in changes.iter().zip(prior) {
        let before = before
            .ok_or_else(|| "prototype updates replace existing bridge rows only".to_owned())?;
        before_values.push((*key, before));
        tree_changes.push(TreeChange {
            key: *key,
            after: *after,
        });
    }
    let budget = LazyTreeUpdateBudget::new(changes.len(), 64 * 1024 * 1024, relation_shape());
    let update = tree
        .prepare_update_bounded(&tree_changes, budget)
        .map_err(|error| format!("prepare bounded lazy bridge update: {error:?}"))?;
    let mut new_node_objects = Vec::with_capacity(update.changed_nodes().len());
    for node in update.changed_nodes() {
        new_node_objects.push(
            TypedObject::from_state_root(node.commitment(), node)
                .map_err(|error| format!("materialize typed changed bridge node: {error:?}"))?,
        );
    }
    Ok((update, loader, new_node_objects, before_values))
}

fn apply_update(
    store: &FileStore,
    root_dir: &Path,
    current_commit: [u8; 32],
    current_body: &CommitBody,
    oracle: &BTreeMap<BridgeKey, BridgeValue>,
    replacements: &[(BridgeKey, u64)],
    fault: CrashPoint,
) -> Result<
    (
        CommitBody,
        [u8; 32],
        BTreeMap<BridgeKey, BridgeValue>,
        WriteMetrics,
    ),
    CrashArtifacts,
> {
    let rss_before_kib = current_rss_kib();
    let store_gc_pin = store
        .pin_garbage_collection()
        .expect("hold GC exclusion across V3 bridge publication");
    let mut after_values = Vec::with_capacity(replacements.len());
    let mut payload_objects = Vec::with_capacity(replacements.len());
    let mut payload_object_bytes_written = 0_usize;
    let mut changes = Vec::with_capacity(replacements.len());
    for (key, salt) in replacements {
        let ordinal = usize::try_from(
            u64::from_be_bytes(key.first_key[24..].try_into().expect("fixed key tail")) / 2,
        )
        .expect("test ordinal fits usize");
        let (object, value, written_bytes) = make_payload(store, key.family, ordinal, *salt);
        payload_object_bytes_written = payload_object_bytes_written
            .checked_add(written_bytes)
            .expect("bounded payload-write counter");
        payload_objects.push(object);
        after_values.push(value);
        changes.push((*key, Some(value)));
    }
    changes.sort_by_key(|(key, _)| *key);
    let base_closure = store
        .admit_closure_claim(closure_claim(current_body))
        .expect("admit base closure before edits");
    let (update, loader, new_node_objects, before_values) =
        changed_tree(store, current_body.bridge_root, &changes)
            .expect("prepare typed V3 path-copy update");
    let target_root = update.target().root().to_bytes();
    let target_node_versions = update
        .changed_nodes()
        .iter()
        .map(|node| node.commitment().to_bytes())
        .collect::<BTreeSet<_>>();
    let mut target_child_versions = BTreeSet::new();
    for node in update.changed_nodes() {
        let checked = backend_version::admit_canonical_root::<BridgeRelation>(node.as_bytes())
            .expect("admit emitted canonical bridge node");
        for child in checked
            .child_summaries()
            .expect("decode emitted bridge child summaries")
        {
            target_child_versions.insert(*child.commitment.as_bytes());
        }
    }

    let relation_write = store
        .write_lazy_relation_update(&update)
        .expect("persist only changed bridge frontier");
    let payload_ids = payload_objects
        .iter()
        .map(|object| *object.id().as_bytes())
        .collect::<Vec<_>>();
    if fault == CrashPoint::AfterNodePublish {
        drop(store_gc_pin);
        return Err(CrashArtifacts {
            point: fault,
            candidate_commit: None,
            candidate_bridge_root: target_root,
            candidate_payload_ids: payload_ids,
        });
    }

    let base_manifest = store
        .open_closure(base_closure)
        .expect("reopen base exact closure for path copy");
    let mut desired_changes: BTreeMap<ObjectId, bool> = BTreeMap::new();
    let mut schedule = |id: ObjectId, should_exist: bool| {
        let base_has = base_manifest
            .contains_object_id(id)
            .expect("query exact base closure membership");
        if base_has != should_exist {
            desired_changes.insert(id, should_exist);
        }
    };
    for object in &new_node_objects {
        schedule(object.id(), true);
    }
    for (version, object) in loader.loaded_frontier() {
        if !target_node_versions.contains(&version) && !target_child_versions.contains(&version) {
            schedule(object, false);
        }
    }
    for (key, before) in &before_values {
        let old_object = store
            .read_object_claim(UntrustedObjectId::from_bytes(before.physical_id))
            .expect("admit old bridge physical payload");
        let after = changes
            .iter()
            .find(|(candidate, _)| candidate == key)
            .and_then(|(_, after)| *after)
            .expect("replace row after value");
        if before.physical_id != after.physical_id {
            schedule(old_object.id(), false);
        }
    }
    for object in &payload_objects {
        schedule(object.id(), true);
    }
    drop(schedule);
    let closure_changes = desired_changes
        .into_iter()
        .map(|(id, add)| {
            if add {
                ClosureMembershipChange::add(id)
            } else {
                ClosureMembershipChange::remove(id)
            }
        })
        .collect::<Vec<_>>();
    let max_changes = closure_changes.len().max(1);
    let closure_budget = backend_store::ClosureCompositionBudget::new(
        MAX_CLOSURE_MEMBERS,
        max_changes,
        512 * 1024 * 1024,
        64 * 1024 * 1024,
    );
    let pinned_closure = store
        .compose_closure_index(
            Some(ArtifactClosureClaim::from_id(base_closure)),
            &closure_changes,
            closure_budget,
        )
        .expect("path-copy exact closure index");
    let closure_receipt = pinned_closure.receipt();
    let mut target_rows = oracle.clone();
    for ((key, _), value) in replacements.iter().zip(after_values) {
        target_rows.insert(*key, value);
    }
    let oracle_state = RelationState::<BridgeRelation>::from_entries(
        target_rows.iter().map(|(key, value)| (*key, *value)),
        CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(
            ScopeRoot::from_u64(0),
        )),
    )
    .expect("build independent full-map bridge oracle");
    assert_eq!(
        oracle_state.root().to_bytes(),
        target_root,
        "path-copy root must equal independently bulk-built BTreeMap oracle"
    );
    let body = CommitBody {
        parent: Some(current_commit),
        generation_root: semantic_root(&target_rows),
        bridge_root: target_root,
        closure_id: *closure_receipt.closure().as_bytes(),
    };
    let candidate_commit = write_commit(root_dir, &body)
        .expect("persist immutable typed V3 commit root before ref CAS");
    if fault == CrashPoint::AfterCommitRoot {
        drop(pinned_closure);
        drop(store_gc_pin);
        return Err(CrashArtifacts {
            point: fault,
            candidate_commit: Some(candidate_commit),
            candidate_bridge_root: target_root,
            candidate_payload_ids: payload_ids,
        });
    }
    let ref_bytes = update_ref_cas(root_dir, Some(current_commit), candidate_commit)
        .expect("CAS selected V3 history ref");
    let body_bytes = commit_wire(&body).len();
    let closure_index_bytes =
        usize::try_from(closure_receipt.bytes_written()).expect("bounded closure metadata bytes");
    let index_metadata_bytes_written = relation_write
        .bytes_written
        .checked_add(closure_index_bytes)
        .and_then(|bytes| bytes.checked_add(body_bytes))
        .and_then(|bytes| bytes.checked_add(ref_bytes))
        .expect("bounded index metadata write bytes");
    let work = update.work();
    let (relation_node_reads, relation_node_read_bytes) = loader.read_metrics();
    let metrics = WriteMetrics {
        lazy_frontier_bytes: work.emitted_bytes,
        relation_object_bytes: relation_write.bytes_written,
        relation_nodes_written: relation_write.nodes_written,
        relation_node_reads,
        relation_node_read_bytes,
        payload_object_bytes_written,
        closure_index_bytes: closure_receipt.bytes_written(),
        closure_verified_payload_bytes: closure_receipt.bytes_verified(),
        commit_body_bytes: body_bytes,
        ref_bytes,
        loaded_nodes: work.loaded_nodes,
        rebuilt_nodes: work.rebuilt_nodes,
        peak_metadata_bytes: work.peak_metadata_bytes,
        rss_before_kib,
        rss_after_kib: current_rss_kib(),
        index_metadata_bytes_written,
    };
    if fault == CrashPoint::AfterRefCas {
        drop(pinned_closure);
        drop(store_gc_pin);
        return Err(CrashArtifacts {
            point: fault,
            candidate_commit: Some(candidate_commit),
            candidate_bridge_root: target_root,
            candidate_payload_ids: payload_ids,
        });
    }
    drop(pinned_closure);
    drop(store_gc_pin);
    Ok((body, candidate_commit, target_rows, metrics))
}

fn collect_history_roots(store: &FileStore, root_dir: &Path) {
    store
        .collect_garbage_resolving_roots(
            |roots| {
                let (mut cursor, _) = read_ref_body(root_dir).map_err(|_| StoreError::Corrupt)?;
                let mut seen = BTreeSet::new();
                loop {
                    if !seen.insert(cursor) {
                        return Err(StoreError::Corrupt);
                    }
                    let body = read_commit(root_dir, cursor).map_err(|_| StoreError::Corrupt)?;
                    let closure = store.admit_closure_claim(closure_claim(&body))?;
                    roots.add(GcRoot::Closure(closure));
                    match body.parent {
                        Some(parent) => cursor = parent,
                        None => break,
                    }
                }
                Ok(())
            },
            GcLimits::default(),
        )
        .expect("collect against retained V3 history chain");
}

fn assert_payloads_swept(store: &FileStore, artifacts: &CrashArtifacts) {
    assert!(artifacts.candidate_payload_ids.iter().all(|id| {
        store
            .read_object_claim(UntrustedObjectId::from_bytes(*id))
            .is_err()
    }));
}

#[test]
fn v3_bridge_lazy_path_copy_matches_oracle_after_cold_reopen_and_gc() {
    let temp = TempStore::new("path-copy");
    let registry = registry();
    let store = temp.open_store(registry.clone());
    let (rows, payloads) = initial_rows(&store, 2_048);
    let (mut commit, mut body, _) =
        initialize_history(&store, &temp.path, rows.clone(), &payloads, &registry);
    let mut oracle = rows;
    let (initial_token, initial_reads, initial_rows) =
        verify_cold(&store, &temp.path, commit, &body).expect("cold-verify genesis V3 root");
    assert_eq!(initial_token.row_count, oracle.len());
    assert_eq!(initial_rows, oracle);
    drop(initial_token);

    let keys = oracle.keys().copied().collect::<Vec<_>>();
    let clustered = keys[700..712]
        .iter()
        .enumerate()
        .map(|(offset, key)| (*key, u64::try_from(offset + 1).expect("small salt")))
        .collect::<Vec<_>>();
    let (next_body, next_commit, next_oracle, clustered_write) = apply_update(
        &store,
        &temp.path,
        commit,
        &body,
        &oracle,
        &clustered,
        CrashPoint::None,
    )
    .expect("publish clustered V3 bridge path copy");
    commit = next_commit;
    body = next_body;
    oracle = next_oracle;
    let (clustered_token, clustered_read, clustered_rows) =
        verify_cold(&store, &temp.path, commit, &body).expect("cold-verify clustered bridge");
    assert_eq!(clustered_token.row_count, oracle.len());
    assert_eq!(clustered_token.generation_root, semantic_root(&oracle));
    assert_eq!(clustered_rows, oracle);
    let clustered_commit = commit;

    let scattered = (0..12)
        .map(|index| {
            let position = index * (keys.len() - 1) / 11;
            (
                keys[position],
                u64::try_from(100 + index).expect("small salt"),
            )
        })
        .collect::<Vec<_>>();
    let (next_body, next_commit, next_oracle, scattered_write) = apply_update(
        &store,
        &temp.path,
        commit,
        &body,
        &oracle,
        &scattered,
        CrashPoint::None,
    )
    .expect("publish scattered V3 bridge path copy");
    commit = next_commit;
    body = next_body;
    oracle = next_oracle;
    let (scattered_token, scattered_read, scattered_rows) =
        verify_cold(&store, &temp.path, commit, &body).expect("cold-verify scattered bridge");
    assert_eq!(scattered_token.row_count, oracle.len());
    assert_eq!(scattered_token.generation_root, semantic_root(&oracle));
    assert_eq!(scattered_rows, oracle);
    assert!(
        lookup_verified(
            &store,
            &clustered_token,
            commit,
            body.generation_root,
            body.bridge_root,
            &keys[..1],
        )
        .is_err(),
        "a replay token from an older commit cannot be relabeled as the selected version"
    );
    assert_ne!(clustered_commit, commit);
    drop(clustered_token);

    let lookup_keys = keys[700..705].to_vec();
    let (lookup_values, lookup_node_reads, lookup_node_bytes) = lookup_verified(
        &store,
        &scattered_token,
        commit,
        body.generation_root,
        body.bridge_root,
        &lookup_keys,
    )
    .expect("bounded replay lookup from verified token");
    assert_eq!(
        lookup_values,
        lookup_keys
            .iter()
            .map(|key| oracle.get(key).copied())
            .collect::<Vec<_>>()
    );
    assert!(lookup_node_reads < scattered_read.bridge_node_reads);
    assert!(lookup_node_bytes < scattered_read.bridge_node_bytes);
    drop(scattered_token);

    collect_history_roots(&store, &temp.path);
    let (post_gc_token, post_gc_read, post_gc_rows) =
        verify_cold(&store, &temp.path, commit, &body).expect("cold replay after history-aware GC");
    assert_eq!(post_gc_token.generation_root, semantic_root(&oracle));
    assert_eq!(post_gc_rows, oracle);

    let body_bytes = commit_wire(&body).len();
    assert!(
        body_bytes < 256,
        "V3 commit body must remain a small root record"
    );
    let flat_v2_same_rows_bytes = FLAT_V2_LOCATOR_MODEL_BYTES
        .checked_mul(oracle.len())
        .expect("bounded same-size flat locator estimate")
        / FLAT_V2_LOCATOR_MODEL_ROWS;
    assert!(
        scattered_write.index_metadata_bytes_written < flat_v2_same_rows_bytes,
        "persisted path-copy metadata must be smaller than a flat locator of the same row count"
    );
    eprintln!(
        "V3 bridge model: genesis cold-read bridge={}B/{} nodes closure-index={}B/{} nodes payload={}B/{} reads RSS={}→{}KiB; clustered writes frontier={}B relation={}B/{} nodes bridge-read={}B/{} reads payload-write={}B closure-index={}B verified={}B loaded/rebuilt={}/{} peak-meta={}B root/ref={}/{}B metadata-total={}B RSS={}→{}KiB; scattered writes frontier={}B relation={}B/{} nodes bridge-read={}B/{} reads payload-write={}B closure-index={}B verified={}B loaded/rebuilt={}/{} peak-meta={}B root/ref={}/{}B metadata-total={}B RSS={}→{}KiB; cold scattered read bridge={}B/{} nodes closure-index={}B/{} nodes payload={}B/{} reads RSS={}→{}KiB; bounded lookup bridge={}B/{} nodes; post-GC read bridge={}B closure-index={}B payload={}B RSS={}→{}KiB; commit-root={}B; V2 flat-locator model at {} rows={}B, same-row estimate={}B",
        initial_reads.bridge_node_bytes,
        initial_reads.bridge_node_reads,
        initial_reads.closure_index_bytes,
        initial_reads.closure_index_node_reads,
        initial_reads.payload_bytes,
        initial_reads.payload_reads,
        initial_reads.rss_before_kib.unwrap_or_default(),
        initial_reads.rss_after_kib.unwrap_or_default(),
        clustered_write.lazy_frontier_bytes,
        clustered_write.relation_object_bytes,
        clustered_write.relation_nodes_written,
        clustered_write.relation_node_read_bytes,
        clustered_write.relation_node_reads,
        clustered_write.payload_object_bytes_written,
        clustered_write.closure_index_bytes,
        clustered_write.closure_verified_payload_bytes,
        clustered_write.loaded_nodes,
        clustered_write.rebuilt_nodes,
        clustered_write.peak_metadata_bytes,
        clustered_write.commit_body_bytes,
        clustered_write.ref_bytes,
        clustered_write.index_metadata_bytes_written,
        clustered_write.rss_before_kib.unwrap_or_default(),
        clustered_write.rss_after_kib.unwrap_or_default(),
        scattered_write.lazy_frontier_bytes,
        scattered_write.relation_object_bytes,
        scattered_write.relation_nodes_written,
        scattered_write.relation_node_read_bytes,
        scattered_write.relation_node_reads,
        scattered_write.payload_object_bytes_written,
        scattered_write.closure_index_bytes,
        scattered_write.closure_verified_payload_bytes,
        scattered_write.loaded_nodes,
        scattered_write.rebuilt_nodes,
        scattered_write.peak_metadata_bytes,
        scattered_write.commit_body_bytes,
        scattered_write.ref_bytes,
        scattered_write.index_metadata_bytes_written,
        scattered_write.rss_before_kib.unwrap_or_default(),
        scattered_write.rss_after_kib.unwrap_or_default(),
        scattered_read.bridge_node_bytes,
        scattered_read.bridge_node_reads,
        scattered_read.closure_index_bytes,
        scattered_read.closure_index_node_reads,
        scattered_read.payload_bytes,
        scattered_read.payload_reads,
        scattered_read.rss_before_kib.unwrap_or_default(),
        scattered_read.rss_after_kib.unwrap_or_default(),
        lookup_node_bytes,
        lookup_node_reads,
        post_gc_read.bridge_node_bytes,
        post_gc_read.closure_index_bytes,
        post_gc_read.payload_bytes,
        post_gc_read.rss_before_kib.unwrap_or_default(),
        post_gc_read.rss_after_kib.unwrap_or_default(),
        body_bytes,
        FLAT_V2_LOCATOR_MODEL_ROWS,
        FLAT_V2_LOCATOR_MODEL_BYTES,
        flat_v2_same_rows_bytes,
    );
    assert!(
        clustered_write.relation_nodes_written < scattered_write.relation_nodes_written,
        "clustered edits should rewrite fewer bridge nodes than equally sized scattered edits"
    );
    assert!(clustered_read.payload_reads == oracle.len());
    assert_eq!(post_gc_read.payload_reads, oracle.len());
}

#[test]
fn v3_bridge_crash_points_preserve_old_or_new_ref_and_retry_by_cold_admission() {
    let temp = TempStore::new("crash");
    let registry = registry();
    let store = temp.open_store(registry.clone());
    let (rows, payloads) = initial_rows(&store, 256);
    let (genesis, body, _) =
        initialize_history(&store, &temp.path, rows.clone(), &payloads, &registry);
    let base_keys = rows.keys().copied().collect::<Vec<_>>();
    let edits = vec![(base_keys[31], 7), (base_keys[219], 9)];

    let after_nodes = apply_update(
        &store,
        &temp.path,
        genesis,
        &body,
        &rows,
        &edits,
        CrashPoint::AfterNodePublish,
    )
    .expect_err("inject crash after relation node publish");
    assert_eq!(after_nodes.point, CrashPoint::AfterNodePublish);
    assert_eq!(current_ref(&temp.path).expect("old ref remains"), genesis);
    collect_history_roots(&store, &temp.path);
    assert_payloads_swept(&store, &after_nodes);
    assert!(
        LazyTree::<BridgeRelation, _>::open(
            &store,
            relation_claim(&after_nodes.candidate_bridge_root)
        )
        .is_err(),
        "GC reclaims unreferenced bridge nodes after a pre-closure crash"
    );
    let (body_after_retry, commit_after_retry, oracle_after_retry, _) = apply_update(
        &store,
        &temp.path,
        genesis,
        &body,
        &rows,
        &edits,
        CrashPoint::None,
    )
    .expect("retry after orphan node sweep");
    assert_eq!(
        current_ref(&temp.path).expect("updated ref"),
        commit_after_retry
    );
    verify_cold(&store, &temp.path, commit_after_retry, &body_after_retry)
        .expect("cold verify retry publication");

    let second_edits = vec![(base_keys[42], 17), (base_keys[201], 19)];
    let after_commit = apply_update(
        &store,
        &temp.path,
        commit_after_retry,
        &body_after_retry,
        &oracle_after_retry,
        &second_edits,
        CrashPoint::AfterCommitRoot,
    )
    .expect_err("inject crash after immutable commit-root publication");
    assert_eq!(after_commit.point, CrashPoint::AfterCommitRoot);
    let unselected_commit = after_commit
        .candidate_commit
        .expect("commit root persisted before ref CAS");
    assert_eq!(
        current_ref(&temp.path).expect("previous tip remains"),
        commit_after_retry
    );
    collect_history_roots(&store, &temp.path);
    assert_payloads_swept(&store, &after_commit);
    let unselected_body = read_commit(&temp.path, unselected_commit)
        .expect("immutable unselected commit body survives separately");
    assert!(verify_cold(&store, &temp.path, unselected_commit, &unselected_body).is_err());

    let (body_after_retry, commit_after_retry_2, oracle_after_retry_2, _) = apply_update(
        &store,
        &temp.path,
        commit_after_retry,
        &body_after_retry,
        &oracle_after_retry,
        &second_edits,
        CrashPoint::None,
    )
    .expect("cold retry rebuilds swept unselected closure then CASes ref");
    assert_eq!(commit_after_retry_2, unselected_commit);
    verify_cold(&store, &temp.path, commit_after_retry_2, &body_after_retry)
        .expect("re-admitted retry root after commit-body crash");

    let third_edits = vec![(base_keys[88], 27), (base_keys[167], 29)];
    let after_ref = apply_update(
        &store,
        &temp.path,
        commit_after_retry_2,
        &body_after_retry,
        &oracle_after_retry_2,
        &third_edits,
        CrashPoint::AfterRefCas,
    )
    .expect_err("inject crash after ref CAS durability");
    assert_eq!(after_ref.point, CrashPoint::AfterRefCas);
    let selected_after_crash = after_ref
        .candidate_commit
        .expect("selected commit id survives crash response");
    assert_eq!(
        current_ref(&temp.path).expect("new ref durable"),
        selected_after_crash
    );
    collect_history_roots(&store, &temp.path);
    let selected_body = read_commit(&temp.path, selected_after_crash)
        .expect("selected commit body after ref crash");
    let (selected_token, _, _) =
        verify_cold(&store, &temp.path, selected_after_crash, &selected_body)
            .expect("cold replay proves ref-CAS commit fully admitted");
    drop(selected_token);
    assert_eq!(
        update_ref_cas(&temp.path, Some(commit_after_retry_2), selected_after_crash)
            .expect("idempotent retry observes already-selected target"),
        0
    );
}

#[test]
fn v3_bridge_cold_admission_rejects_wrong_kind_schema_and_inexact_closures() {
    let temp = TempStore::new("negative-admission");
    let registry = registry();
    let store = temp.open_store(registry.clone());
    let (rows, payloads) = initial_rows(&store, 32);
    let (parent, genesis, _) =
        initialize_history(&store, &temp.path, rows.clone(), &payloads, &registry);
    let state = RelationState::<BridgeRelation>::from_entries(
        rows.iter().map(|(key, value)| (*key, *value)),
        CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(
            ScopeRoot::from_u64(0),
        )),
    )
    .expect("reopen canonical bridge state for closure fixtures");
    let full_closure = store
        .admit_closure_claim(closure_claim(&genesis))
        .expect("reopen genesis closure claim");

    let (extra_payload, _, _) = make_payload(&store, 0, 10_000, 91);
    let mut extra_members = payloads.clone();
    extra_members.push(extra_payload);
    let extra_closure = store
        .write_closure(&make_closure(&state, &extra_members, &registry))
        .expect("persist closure with one unreachable extra payload");
    let extra_body = CommitBody {
        closure_id: *extra_closure.as_bytes(),
        ..genesis.clone()
    };
    let extra_commit = write_commit(&temp.path, &extra_body).expect("write extra-closure root");
    assert!(verify_cold(&store, &temp.path, extra_commit, &extra_body).is_err());

    let mut wrong_kind_rows = rows.clone();
    let first_key = *wrong_kind_rows.keys().next().expect("nonempty rows");
    let wrong_kind_value = wrong_kind_rows
        .get_mut(&first_key)
        .expect("selected bridge row");
    wrong_kind_value.kind = PhysicalKind::JumboLeaf as u8;
    let wrong_state = RelationState::<BridgeRelation>::from_entries(
        wrong_kind_rows.iter().map(|(key, value)| (*key, *value)),
        CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(
            ScopeRoot::from_u64(0),
        )),
    )
    .expect("build wrong-kind test tree");
    let wrong_manifest = make_closure(&wrong_state, &payloads, &registry);
    let wrong_closure = store
        .write_closure(&wrong_manifest)
        .expect("persist malformed semantic-kind closure");
    let wrong_body = CommitBody {
        parent: Some(parent),
        generation_root: semantic_root(&wrong_kind_rows),
        bridge_root: wrong_state.root().to_bytes(),
        closure_id: *wrong_closure.as_bytes(),
    };
    let wrong_commit = write_commit(&temp.path, &wrong_body).expect("write wrong-kind root");
    assert!(verify_cold(&store, &temp.path, wrong_commit, &wrong_body).is_err());

    let mut wrong_schema_rows = rows.clone();
    wrong_schema_rows
        .get_mut(&first_key)
        .expect("selected bridge row")
        .schema = SchemaIdentity::new(0, 0, 0);
    let wrong_schema_state = RelationState::<BridgeRelation>::from_entries(
        wrong_schema_rows.iter().map(|(key, value)| (*key, *value)),
        CoverageWitness::closed_relation(ClosedRelationScope::from_scope_root(
            ScopeRoot::from_u64(0),
        )),
    )
    .expect("build wrong-schema test tree");
    let wrong_schema_closure = store
        .write_closure(&make_closure(&wrong_schema_state, &payloads, &registry))
        .expect("persist wrong-schema semantic-kind closure");
    let wrong_schema_body = CommitBody {
        generation_root: semantic_root(&wrong_schema_rows),
        bridge_root: wrong_schema_state.root().to_bytes(),
        closure_id: *wrong_schema_closure.as_bytes(),
        ..genesis.clone()
    };
    let wrong_schema_commit =
        write_commit(&temp.path, &wrong_schema_body).expect("write wrong-schema root");
    assert!(verify_cold(&store, &temp.path, wrong_schema_commit, &wrong_schema_body).is_err());

    let complete = store
        .read_closure(full_closure)
        .expect("materialize genesis closure for incomplete-closure fixture");
    let missing_payload = payloads[0].id();
    let incomplete_members = complete
        .objects()
        .iter()
        .filter(|object| object.id() != missing_payload)
        .cloned()
        .collect::<Vec<_>>();
    let incomplete_manifest = ClosureManifest::new_with_registry(incomplete_members, &registry)
        .expect("admit relation-complete closure missing one mapped value");
    let incomplete_closure = store
        .write_closure(&incomplete_manifest)
        .expect("persist incomplete semantic closure");
    let incomplete_body = CommitBody {
        closure_id: *incomplete_closure.as_bytes(),
        ..genesis
    };
    let incomplete_commit =
        write_commit(&temp.path, &incomplete_body).expect("write incomplete-closure root");
    assert!(verify_cold(&store, &temp.path, incomplete_commit, &incomplete_body).is_err());
}

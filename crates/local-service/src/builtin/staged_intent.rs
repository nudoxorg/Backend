//! Storage evidence for a future small owner commit. Nothing in this module
//! selects a workspace head or admits source/facts semantics. The owner must
//! still apply the existing relation and authenticated-base validators.
//!
//! Payloads are read and written one bounded CAS page at a time. Existing
//! FileStore immutable installation, closure composition and GC leases own
//! durability, cross-process recovery and abandoned-object reclamation.

use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ArtifactSink, ClosureCompositionBudget,
    ClosureMembershipChange, FileStore, GcPinGuard, ObjectId, PinnedStoredClosureReceipt,
    StoreError, TypedObject, UntrustedObjectId,
};
use backend_version::{ObjectKey, Schema, SchemaIdentity};
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::{Arc, Mutex};

const PAGE_BYTES: usize = 64 * 1024;
const PAGE_HEADER: usize = 4 + 8 + 32;
const MANIFEST_BYTES: usize = 4 + 8 * 2 + 32 * 4 + 3 * (32 + 8 + 8);

macro_rules! schema {
    ($name:ident, $tag:expr) => {
        #[derive(Debug)]
        struct $name;
        impl Schema for $name {
            const DOMAIN: u8 = 0x96;
            const TYPE: u16 = $tag;
            const VERSION: u8 = 1;
            type Value = [u8];
            fn encode(value: &[u8], output: &mut Vec<u8>) {
                output.extend_from_slice(value);
            }
        }
    };
}
schema!(RawSourcePage, 10);
schema!(SourceRowsPage, 11);
schema!(CompleteFactsPage, 12);
schema!(StagedManifest, 13);

pub(super) fn admit_manifest_object(object: &TypedObject) -> Result<StageBasis, StoreError> {
    if object.schema() != SchemaIdentity::new(0x96, 13, 1)
        || typed::<StagedManifest>(object.bytes()).id() != object.id()
    {
        return Err(StoreError::Corrupt);
    }
    decode_manifest(object.bytes()).map(|(basis, _)| basis)
}

pub(super) fn staged_member_count(object: &TypedObject) -> Result<u64, StoreError> {
    admit_manifest_object(object)?;
    let (_, chains) = decode_manifest(object.bytes())?;
    chains.iter().try_fold(1_u64, |count, chain| {
        count.checked_add(chain.pages).ok_or(StoreError::Bounds)
    })
}

/// Different kinds remain separated even for identical payload bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum EvidenceKind {
    RawSource,
    SourceRows,
    CompleteFacts,
}

impl EvidenceKind {
    fn slot(self) -> usize {
        match self {
            Self::RawSource => 0,
            Self::SourceRows => 1,
            Self::CompleteFacts => 2,
        }
    }
    fn schema(self) -> SchemaIdentity {
        let tag = match self {
            Self::RawSource => 10,
            Self::SourceRows => 11,
            Self::CompleteFacts => 12,
        };
        SchemaIdentity::new(0x96, tag, 1)
    }
    fn object(self, bytes: &[u8]) -> TypedObject {
        match self {
            Self::RawSource => typed::<RawSourcePage>(bytes),
            Self::SourceRows => typed::<SourceRowsPage>(bytes),
            Self::CompleteFacts => typed::<CompleteFactsPage>(bytes),
        }
    }
}

fn typed<S: Schema<Value = [u8]>>(bytes: &[u8]) -> TypedObject {
    TypedObject::from_value(&ObjectKey::<S>::from_value(bytes), bytes)
}

/// Exact owner/base/capture binding, independently checked at final commit.
/// The staging receipt is a storage capability, never an authority capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StageBasis {
    pub owner_epoch: u64,
    pub workspace_sequence: u64,
    pub owner_fence: [u8; 32],
    pub workspace_root: [u8; 32],
    pub closure_id: [u8; 32],
    pub source_capture: [u8; 32],
}

/// Shared reservations bound concurrent live stages before reading payloads.
/// This is deliberately separate from the existing waiting-command budget.
#[derive(Clone)]
pub(super) struct StagePool {
    usage: Arc<Mutex<(usize, u64)>>,
    max_live: usize,
    max_reserved_bytes: u64,
    max_pages: usize,
    max_metadata_bytes: usize,
}

impl StagePool {
    fn admit_physical_allocation(&self, store: &FileStore) -> Result<(), StoreError> {
        backend_store::PhysicalAllocationBudget::new(
            self.max_reserved_bytes,
            self.max_pages
                .checked_mul(32)
                .and_then(|n| n.checked_add(4096))
                .ok_or(StoreError::Bounds)?,
        )
        .admit(store)
        .map(|_| ())
    }
    #[cfg(unix)]
    fn write_object(
        &self,
        store: &FileStore,
        object: &TypedObject,
    ) -> Result<ObjectId, StoreError> {
        use std::{
            fs::{self, OpenOptions},
            os::unix::fs::MetadataExt,
        };
        let quota = store.root().join("staged-ingest-quota");
        let markers = quota.join("members");
        for directory in [&quota, &markers] {
            if fs::symlink_metadata(directory)
                .is_ok_and(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
            {
                return Err(StoreError::Corrupt);
            }
        }
        fs::create_dir_all(&markers).map_err(|e| StoreError::Io(e.to_string()))?;
        if fs::symlink_metadata(&quota)
            .map_err(|e| StoreError::Io(e.to_string()))?
            .file_type()
            .is_symlink()
            || fs::symlink_metadata(&markers)
                .map_err(|e| StoreError::Io(e.to_string()))?
                .file_type()
                .is_symlink()
        {
            return Err(StoreError::Corrupt);
        }
        let lock_path = quota.join("allocation.lock");
        if fs::symlink_metadata(&lock_path).is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(StoreError::Corrupt);
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|e| StoreError::Io(e.to_string()))?;
        lock.lock().map_err(|e| StoreError::Io(e.to_string()))?;
        let mut allocated = 0_u64;
        for entry in fs::read_dir(&markers).map_err(|e| StoreError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| StoreError::Io(e.to_string()))?;
            let marker_metadata =
                fs::symlink_metadata(entry.path()).map_err(|e| StoreError::Io(e.to_string()))?;
            if !marker_metadata.is_file()
                || marker_metadata.file_type().is_symlink()
                || marker_metadata.nlink() != 1
            {
                return Err(StoreError::Corrupt);
            }
            let name = entry.file_name();
            let name = name.to_str().ok_or(StoreError::Corrupt)?;
            if name.len() != 64
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(StoreError::Corrupt);
            }
            let path = store.root().join("objects").join(format!("{name}.object"));
            match fs::symlink_metadata(path) {
                Ok(metadata) => {
                    if !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || metadata.nlink() != 1
                    {
                        return Err(StoreError::Corrupt);
                    }
                    allocated = allocated
                        .checked_add(
                            metadata
                                .blocks()
                                .checked_mul(512)
                                .ok_or(StoreError::Bounds)?,
                        )
                        .ok_or(StoreError::Bounds)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    // Only accounting metadata is removed. Ordinary store GC
                    // owns all unselected CAS reclamation, including crashes.
                    fs::remove_file(entry.path()).map_err(|e| StoreError::Io(e.to_string()))?;
                }
                Err(error) => return Err(StoreError::Io(error.to_string())),
            }
        }
        let id = object.id();
        let name = id
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let marker = markers.join(&name);
        let already_counted = marker.is_file();
        let target = store.root().join("objects").join(format!("{name}.object"));
        let addition = if marker.is_file() {
            0
        } else if let Ok(metadata) = fs::symlink_metadata(&target) {
            if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.nlink() != 1 {
                return Err(StoreError::Corrupt);
            }
            metadata
                .blocks()
                .checked_mul(512)
                .ok_or(StoreError::Bounds)?
        } else {
            let bytes = u64::try_from(object.bytes().len())
                .map_err(|_| StoreError::Bounds)?
                .checked_add(4096 + 128)
                .ok_or(StoreError::Bounds)?;
            bytes
                .div_ceil(4096)
                .checked_mul(4096)
                .ok_or(StoreError::Bounds)?
        };
        if allocated.checked_add(addition).ok_or(StoreError::Bounds)? > self.max_reserved_bytes {
            return Err(StoreError::Bounds);
        }
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(file) => file.sync_all().map_err(|e| StoreError::Io(e.to_string()))?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(StoreError::Io(error.to_string())),
        }
        let written = store.write_object(object)?;
        self.admit_physical_allocation(store)?;
        let actual = fs::symlink_metadata(&target)
            .map_err(|e| StoreError::Io(e.to_string()))?
            .blocks()
            .checked_mul(512)
            .ok_or(StoreError::Bounds)?;
        if !already_counted
            && allocated.checked_add(actual).ok_or(StoreError::Bounds)? > self.max_reserved_bytes
        {
            return Err(StoreError::Bounds);
        }
        std::fs::File::open(&markers)
            .and_then(|file| file.sync_all())
            .map_err(|e| StoreError::Io(e.to_string()))?;
        Ok(written)
    }

    #[cfg(not(unix))]
    fn write_object(
        &self,
        _store: &FileStore,
        _object: &TypedObject,
    ) -> Result<ObjectId, StoreError> {
        // This policy requires real physical-allocation accounting. A platform
        // without that adapter cannot silently replace it with logical bytes.
        Err(StoreError::Bounds)
    }
    fn metadata_required(pages: usize) -> Result<usize, StoreError> {
        ClosureCompositionBudget::metadata_bytes_for(
            pages.checked_add(1).ok_or(StoreError::Bounds)?,
        )?
        .checked_add(PAGE_BYTES * 8)
        .and_then(|charge| charge.checked_add(pages.checked_mul(size_of::<ObjectId>() * 4)?))
        .ok_or(StoreError::Bounds)
    }
    pub(super) fn new(
        max_live: usize,
        max_reserved_bytes: u64,
        max_pages: usize,
        max_metadata_bytes: usize,
    ) -> Self {
        Self {
            usage: Arc::new(Mutex::new((0, 0))),
            max_live,
            max_reserved_bytes,
            max_pages,
            max_metadata_bytes,
        }
    }
    pub(super) fn begin(
        &self,
        store: &FileStore,
        basis: StageBasis,
        reserved_bytes: u64,
    ) -> Result<StageWriter, StoreError> {
        // Charge closure-composition metadata before allocating page IDs.
        if reserved_bytes == 0
            || self.max_pages == 0
            || Self::metadata_required(self.max_pages)? > self.max_metadata_bytes
        {
            return Err(StoreError::Bounds);
        }
        let mut usage = self.usage.lock().map_err(|_| StoreError::Corrupt)?;
        let count = usage.0.checked_add(1).ok_or(StoreError::Bounds)?;
        let bytes = usage
            .1
            .checked_add(reserved_bytes)
            .ok_or(StoreError::Bounds)?;
        if count > self.max_live || bytes > self.max_reserved_bytes {
            return Err(StoreError::Bounds);
        }
        usage.0 = count;
        usage.1 = bytes;
        drop(usage);
        let lease = Reservation {
            usage: self.usage.clone(),
            bytes: reserved_bytes,
        };
        let pin = store.pin_garbage_collection()?;
        self.admit_physical_allocation(store)?;
        Ok(StageWriter {
            store: store.clone(),
            pool: self.clone(),
            basis,
            chains: [Chain::default(); 3],
            pending: std::array::from_fn(|_| Vec::new()),
            ids: Vec::new(),
            payload_bytes: 0,
            poisoned: false,
            _pin: pin,
            lease,
        })
    }
}

struct Reservation {
    usage: Arc<Mutex<(usize, u64)>>,
    bytes: u64,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut usage) = self.usage.lock() {
            // Only this affine permit can release its reservation.
            usage.0 -= 1;
            usage.1 -= self.bytes;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct Chain {
    tail: [u8; 32],
    pages: u64,
    bytes: u64,
}

pub(super) struct StageWriter {
    store: FileStore,
    pool: StagePool,
    basis: StageBasis,
    chains: [Chain; 3],
    pending: [Vec<u8>; 3],
    ids: Vec<ObjectId>,
    payload_bytes: u64,
    poisoned: bool,
    _pin: GcPinGuard,
    lease: Reservation,
}

impl StageWriter {
    /// Caller supplies a finite canonical stream for exactly one kind.
    /// Repeated calls append to the same ordered stream, never publish it.
    pub(super) fn append(
        &mut self,
        kind: EvidenceKind,
        input: &mut impl Read,
    ) -> Result<(), StoreError> {
        if self.poisoned {
            return Err(StoreError::Corrupt);
        }
        let result = self.append_inner(kind, input);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn append_inner(
        &mut self,
        kind: EvidenceKind,
        input: &mut impl Read,
    ) -> Result<(), StoreError> {
        let mut buffer = vec![0; PAGE_BYTES];
        loop {
            let mut length = 0;
            while length < buffer.len() {
                let read = match input.read(&mut buffer[length..]) {
                    Ok(read) => read,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(StoreError::Io(error.to_string())),
                };
                if read == 0 {
                    break;
                }
                length += read;
            }
            if length == 0 {
                return Ok(());
            }
            let charged = self
                .payload_bytes
                .checked_add(length as u64)
                .ok_or(StoreError::Bounds)?;
            if charged > self.lease.bytes {
                return Err(StoreError::Bounds);
            }
            self.payload_bytes = charged;
            let mut at = 0;
            while at < length {
                let take = (PAGE_BYTES - self.pending[kind.slot()].len()).min(length - at);
                self.pending[kind.slot()].extend_from_slice(&buffer[at..at + take]);
                at += take;
                if self.pending[kind.slot()].len() == PAGE_BYTES {
                    let page = std::mem::take(&mut self.pending[kind.slot()]);
                    self.write_page(kind, &page)?;
                }
            }
        }
    }
    fn write_page(&mut self, kind: EvidenceKind, payload: &[u8]) -> Result<(), StoreError> {
        if self.ids.len() >= self.pool.max_pages {
            return Err(StoreError::Bounds);
        }
        let chain = &mut self.chains[kind.slot()];
        let mut bytes = Vec::with_capacity(PAGE_HEADER + payload.len());
        bytes.extend_from_slice(b"BSP1");
        bytes.extend_from_slice(&chain.pages.to_be_bytes());
        bytes.extend_from_slice(&chain.tail);
        bytes.extend_from_slice(payload);
        let id = self.pool.write_object(&self.store, &kind.object(&bytes))?;
        self.ids.push(id);
        chain.tail = *id.as_bytes();
        chain.pages += 1;
        chain.bytes += payload.len() as u64;
        Ok(())
    }
    pub(super) fn finish(mut self) -> Result<StagedEvidence, StoreError> {
        if self.poisoned {
            return Err(StoreError::Corrupt);
        }
        for kind in [
            EvidenceKind::RawSource,
            EvidenceKind::SourceRows,
            EvidenceKind::CompleteFacts,
        ] {
            let pending = std::mem::take(&mut self.pending[kind.slot()]);
            if !pending.is_empty() {
                self.write_page(kind, &pending)?;
            }
        }
        let manifest = encode_manifest(self.basis, self.chains);
        let manifest_id = self
            .pool
            .write_object(&self.store, &typed::<StagedManifest>(&manifest))?;
        let mut edits = self
            .ids
            .iter()
            .copied()
            .chain([manifest_id])
            .map(ClosureMembershipChange::add)
            .collect::<Vec<_>>();
        edits.sort_by_key(|edit| edit.object_id());
        if edits
            .windows(2)
            .any(|pair| pair[0].object_id() == pair[1].object_id())
        {
            return Err(StoreError::Corrupt);
        }
        let maximum_payload = self
            .lease
            .bytes
            .checked_add(
                (self.pool.max_pages as u64)
                    .checked_mul(PAGE_HEADER as u64)
                    .ok_or(StoreError::Bounds)?,
            )
            .and_then(|n| n.checked_add(MANIFEST_BYTES as u64))
            .ok_or(StoreError::Bounds)?;
        let receipt = self.store.compose_closure_index(
            None,
            &edits,
            ClosureCompositionBudget::new(
                self.pool.max_pages + 1,
                self.pool.max_pages + 1,
                maximum_payload,
                self.pool.max_metadata_bytes,
            ),
        )?;
        self.pool.admit_physical_allocation(&self.store)?;
        Ok(StagedEvidence {
            manifest_id,
            receipt,
            _lease: self.lease,
        })
    }
}

/// Affine pin and budget ownership survive through final selection or failure.
pub(super) struct StagedEvidence {
    pub manifest_id: ObjectId,
    pub receipt: PinnedStoredClosureReceipt,
    _lease: Reservation,
}

fn encode_manifest(basis: StageBasis, chains: [Chain; 3]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MANIFEST_BYTES);
    bytes.extend_from_slice(b"BSM1");
    bytes.extend_from_slice(&basis.owner_epoch.to_be_bytes());
    bytes.extend_from_slice(&basis.workspace_sequence.to_be_bytes());
    for id in [
        basis.owner_fence,
        basis.workspace_root,
        basis.closure_id,
        basis.source_capture,
    ] {
        bytes.extend_from_slice(&id);
    }
    for chain in chains {
        bytes.extend_from_slice(&chain.tail);
        bytes.extend_from_slice(&chain.pages.to_be_bytes());
        bytes.extend_from_slice(&chain.bytes.to_be_bytes());
    }
    bytes
}

pub(super) fn decode_manifest(bytes: &[u8]) -> Result<(StageBasis, [Chain; 3]), StoreError> {
    if bytes.len() != MANIFEST_BYTES || &bytes[..4] != b"BSM1" {
        return Err(StoreError::Corrupt);
    }
    let epoch = u64::from_be_bytes(bytes[4..12].try_into().map_err(|_| StoreError::Corrupt)?);
    let sequence = u64::from_be_bytes(bytes[12..20].try_into().map_err(|_| StoreError::Corrupt)?);
    let mut at = 20;
    let mut id = || {
        let value: [u8; 32] = bytes[at..at + 32]
            .try_into()
            .expect("fixed manifest length");
        at += 32;
        value
    };
    let basis = StageBasis {
        owner_epoch: epoch,
        workspace_sequence: sequence,
        owner_fence: id(),
        workspace_root: id(),
        closure_id: id(),
        source_capture: id(),
    };
    let mut chains = [Chain::default(); 3];
    for chain in &mut chains {
        chain.tail = bytes[at..at + 32]
            .try_into()
            .map_err(|_| StoreError::Corrupt)?;
        at += 32;
        chain.pages = u64::from_be_bytes(
            bytes[at..at + 8]
                .try_into()
                .map_err(|_| StoreError::Corrupt)?,
        );
        at += 8;
        chain.bytes = u64::from_be_bytes(
            bytes[at..at + 8]
                .try_into()
                .map_err(|_| StoreError::Corrupt)?,
        );
        at += 8;
        if (chain.pages == 0) != (chain.tail == [0; 32]) || (chain.pages == 0) != (chain.bytes == 0)
        {
            return Err(StoreError::Corrupt);
        }
    }
    Ok((basis, chains))
}

fn read_bounded(
    sink: &ArtifactSink,
    id: ObjectId,
    maximum: usize,
) -> Result<(SchemaIdentity, Vec<u8>), StoreError> {
    let mut reader = sink
        .open_object_limited(
            UntrustedObjectId::from_bytes(*id.as_bytes()),
            maximum as u64,
        )?
        .ok_or(StoreError::Corrupt)?;
    let schema = reader.schema();
    let mut bytes = vec![0; usize::try_from(reader.payload_len()).map_err(|_| StoreError::Bounds)?];
    if reader.read_payload_range(0, &mut bytes)? != bytes.len() {
        return Err(StoreError::Corrupt);
    }
    // The generic store verifies typed versions. This protocol additionally
    // requires content-derived keys, never arbitrary producer key claims.
    let canonical = match schema {
        value if value == SchemaIdentity::new(0x96, 10, 1) => typed::<RawSourcePage>(&bytes),
        value if value == SchemaIdentity::new(0x96, 11, 1) => typed::<SourceRowsPage>(&bytes),
        value if value == SchemaIdentity::new(0x96, 12, 1) => typed::<CompleteFactsPage>(&bytes),
        value if value == SchemaIdentity::new(0x96, 13, 1) => typed::<StagedManifest>(&bytes),
        _ => return Err(StoreError::Corrupt),
    };
    if canonical.id() != id {
        return Err(StoreError::Corrupt);
    }
    Ok((schema, bytes))
}

/// Reopen is fail-closed for wrong kind, partial/cyclic/reordered page chains,
/// missing members, altered pages, extra closure members and stale bases.
/// A callback borrows one page; it cannot retain the complete payload implicitly.
pub(super) fn visit_staged(
    store: &FileStore,
    manifest_id: ObjectId,
    receipt: &PinnedStoredClosureReceipt,
    expected: StageBasis,
    budget: ArtifactBudget,
    mut visit: impl FnMut(EvidenceKind, &[u8]) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    visit_staged_scope(
        store,
        manifest_id,
        receipt.receipt().closure(),
        expected,
        budget,
        Some(receipt.receipt().object_count()),
        visit,
    )
}

pub(super) fn visit_staged_scope(
    store: &FileStore,
    manifest_id: ObjectId,
    closure: backend_store::ClosureId,
    expected: StageBasis,
    budget: ArtifactBudget,
    exact_count: Option<u64>,
    mut visit: impl FnMut(EvidenceKind, &[u8]) -> Result<(), StoreError>,
) -> Result<(), StoreError> {
    // A receipt may have been produced by another FileStore containing the
    // same CAS closure. Pin this reader's store across the entire traversal.
    let _local_pin = store.pin_garbage_collection()?;
    let sink = store.artifact_sink(budget);
    let mut seen = BTreeSet::new();
    let (schema, manifest) = read_bounded(&sink, manifest_id, MANIFEST_BYTES)?;
    if schema != SchemaIdentity::new(0x96, 13, 1)
        || sink
            .verify_closure_member(
                closure,
                UntrustedObjectId::from_bytes(*manifest_id.as_bytes()),
            )?
            .is_none()
    {
        return Err(StoreError::Corrupt);
    }
    let (basis, chains) = decode_manifest(&manifest)?;
    if basis != expected {
        return Err(StoreError::WrongBase);
    }
    seen.insert(manifest_id);
    let mut total = 0u64;
    for kind in [
        EvidenceKind::RawSource,
        EvidenceKind::SourceRows,
        EvidenceKind::CompleteFacts,
    ] {
        let chain = chains[kind.slot()];
        if chain.pages > budget.max_changes as u64 {
            return Err(StoreError::Bounds);
        }
        let mut cursor = chain.tail;
        let mut pages = Vec::new();
        let mut length = 0u64;
        for ordinal in (0..chain.pages).rev() {
            let id = sink
                .verify_closure_member(closure, UntrustedObjectId::from_bytes(cursor))?
                .ok_or(StoreError::Corrupt)?
                .object_id();
            if seen.len() >= budget.max_closure_objects || !seen.insert(id) {
                return Err(StoreError::Corrupt);
            }
            let (schema, bytes) = read_bounded(&sink, id, PAGE_HEADER + PAGE_BYTES)?;
            if schema != kind.schema()
                || bytes.len() <= PAGE_HEADER
                || &bytes[..4] != b"BSP1"
                || u64::from_be_bytes(bytes[4..12].try_into().map_err(|_| StoreError::Corrupt)?)
                    != ordinal
            {
                return Err(StoreError::Corrupt);
            }
            cursor = bytes[12..44].try_into().map_err(|_| StoreError::Corrupt)?;
            length = length
                .checked_add((bytes.len() - PAGE_HEADER) as u64)
                .ok_or(StoreError::Bounds)?;
            total = total
                .checked_add((bytes.len() - PAGE_HEADER) as u64)
                .ok_or(StoreError::Bounds)?;
            if total > budget.max_payload_bytes {
                return Err(StoreError::Bounds);
            }
            pages.push(id);
        }
        if cursor != [0; 32] || length != chain.bytes {
            return Err(StoreError::Corrupt);
        }
        for id in pages.into_iter().rev() {
            let (_, bytes) = read_bounded(&sink, id, PAGE_HEADER + PAGE_BYTES)?;
            visit(kind, &bytes[PAGE_HEADER..])?;
        }
    }
    if exact_count.is_some_and(|count| seen.len() as u64 != count) {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(std::path::PathBuf);
    impl Directory {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let directory = std::env::temp_dir().join(format!(
                "nudox-staged-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            std::fs::create_dir(&directory).unwrap();
            Self(directory)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn basis() -> StageBasis {
        StageBasis {
            owner_epoch: 7,
            workspace_sequence: 11,
            owner_fence: [1; 32],
            workspace_root: [2; 32],
            closure_id: [3; 32],
            source_capture: [4; 32],
        }
    }
    fn pool() -> StagePool {
        StagePool::new(
            1,
            1024 * 1024,
            32,
            StagePool::metadata_required(32).unwrap(),
        )
    }
    fn budget() -> ArtifactBudget {
        ArtifactBudget::new(32, 33, 1024 * 1024, PAGE_BYTES, 1024)
    }
    #[test]
    fn exact_stream_domains_replay_and_cold_reopen() {
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pool = pool();
        let payload = vec![42; PAGE_BYTES * 2 + 3];
        let mut writer = pool.begin(&store, basis(), 512 * 1024).unwrap();
        writer
            .append(EvidenceKind::RawSource, &mut payload.as_slice())
            .unwrap();
        writer
            .append(EvidenceKind::CompleteFacts, &mut payload.as_slice())
            .unwrap();
        let staged = writer.finish().unwrap();
        let manifest_id = staged.manifest_id;
        let closure = staged.receipt.receipt().closure();
        let mut got = [Vec::new(), Vec::new(), Vec::new()];
        visit_staged(
            &store,
            manifest_id,
            &staged.receipt,
            basis(),
            budget(),
            |kind, bytes| {
                got[kind.slot()].extend_from_slice(bytes);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(got[0], payload);
        assert_eq!(got[2], payload);
        assert!(got[1].is_empty());
        drop(staged);
        let mut replay = pool.begin(&store, basis(), 512 * 1024).unwrap();
        // Arbitrary producer write boundaries do not alter canonical pages.
        replay
            .append(EvidenceKind::RawSource, &mut &payload[..5])
            .unwrap();
        replay
            .append(EvidenceKind::RawSource, &mut &payload[5..])
            .unwrap();
        replay
            .append(EvidenceKind::CompleteFacts, &mut payload.as_slice())
            .unwrap();
        let replay = replay.finish().unwrap();
        assert_eq!(replay.manifest_id, manifest_id);
        assert_eq!(replay.receipt.receipt().closure(), closure);
        drop(replay);
        let cold = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pin = cold
            .reopen_pinned_stored_closure(ArtifactClosureClaim::from_id(closure), budget())
            .unwrap();
        visit_staged(&cold, manifest_id, &pin, basis(), budget(), |_, _| Ok(())).unwrap();
        let mut stale = basis();
        stale.owner_epoch += 1;
        assert!(matches!(
            visit_staged(&cold, manifest_id, &pin, stale, budget(), |_, _| Ok(())),
            Err(StoreError::WrongBase)
        ));
    }
    #[test]
    fn reservations_partial_failure_and_release() {
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pool = pool();
        let mut writer = pool.begin(&store, basis(), 8).unwrap();
        assert!(matches!(
            pool.begin(&store, basis(), 8),
            Err(StoreError::Bounds)
        ));
        assert!(matches!(
            writer.append(EvidenceKind::SourceRows, &mut &[0; 9][..]),
            Err(StoreError::Bounds)
        ));
        assert!(matches!(writer.finish(), Err(StoreError::Corrupt)));
        assert!(pool.begin(&store, basis(), 8).is_ok());
        // Staging never writes FileStore's selected HEAD.
        assert!(!directory.path().join("HEAD").exists());
    }
    #[test]
    fn duplicate_wrong_kind_missing_member_and_extra_member_fail_closed() {
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pool = pool();
        let mut writer = pool.begin(&store, basis(), 100).unwrap();
        writer
            .append(EvidenceKind::RawSource, &mut &[1, 2, 3][..])
            .unwrap();
        let staged = writer.finish().unwrap();
        let manifest = store.read_object(staged.manifest_id).unwrap();
        let (_, mut chains) = decode_manifest(manifest.bytes()).unwrap();
        let page_id = store
            .artifact_sink(budget())
            .verify_closure_member(
                staged.receipt.receipt().closure(),
                UntrustedObjectId::from_bytes(chains[0].tail),
            )
            .unwrap()
            .unwrap()
            .object_id();
        // Same physical page cannot fill two evidence domains.
        chains[2] = chains[0];
        let forged_manifest = store
            .write_object(&typed::<StagedManifest>(&encode_manifest(basis(), chains)))
            .unwrap();
        let compose = |ids: &[ObjectId]| {
            let mut edits = ids
                .iter()
                .copied()
                .map(ClosureMembershipChange::add)
                .collect::<Vec<_>>();
            edits.sort_by_key(|edit| edit.object_id());
            store
                .compose_closure_index(
                    None,
                    &edits,
                    ClosureCompositionBudget::new(
                        33,
                        33,
                        1024 * 1024,
                        StagePool::metadata_required(32).unwrap(),
                    ),
                )
                .unwrap()
        };
        let forged = compose(&[forged_manifest, page_id]);
        assert!(matches!(
            visit_staged(
                &store,
                forged_manifest,
                &forged,
                basis(),
                budget(),
                |_, _| Ok(())
            ),
            Err(StoreError::Corrupt)
        ));
        let missing = compose(&[staged.manifest_id]);
        assert!(matches!(
            visit_staged(
                &store,
                staged.manifest_id,
                &missing,
                basis(),
                budget(),
                |_, _| Ok(())
            ),
            Err(StoreError::Corrupt)
        ));
        let extra = compose(&[staged.manifest_id, page_id, forged_manifest]);
        assert!(matches!(
            visit_staged(
                &store,
                staged.manifest_id,
                &extra,
                basis(),
                budget(),
                |_, _| Ok(())
            ),
            Err(StoreError::Corrupt)
        ));
        // Corrupt a real immutable CAS envelope only in this disposable
        // store, then restore read-only mode before attempting admission.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let name = page_id
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = directory
                .path()
                .join("objects")
                .join(format!("{name}.object"));
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            let mut bytes = std::fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() ^= 1;
            std::fs::write(&path, bytes).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
            assert!(
                visit_staged(
                    &store,
                    staged.manifest_id,
                    &staged.receipt,
                    basis(),
                    budget(),
                    |_, _| Ok(())
                )
                .is_err()
            );
        }
    }

    #[test]
    fn crash_after_unselected_page_is_invisible() {
        const CHILD_DIRECTORY: &str = "NUDOX_STAGED_TEST_CRASH_DIRECTORY";
        if let Some(directory) = std::env::var_os(CHILD_DIRECTORY) {
            let store = FileStore::open(directory, 1024 * 1024).unwrap();
            let mut writer = pool().begin(&store, basis(), 512 * 1024).unwrap();
            writer
                .append(EvidenceKind::RawSource, &mut &vec![17; PAGE_BYTES][..])
                .unwrap();
            // Deliberately skip Rust destructors: OS lease release and cold
            // store recovery must suffice, with no selected partial state.
            std::process::exit(77);
        }
        let directory = Directory::new();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "staged_intent::tests::crash_after_unselected_page_is_invisible",
                "--nocapture",
            ])
            .env(CHILD_DIRECTORY, directory.path())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77));
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        assert!(store.head().unwrap().is_none());
        let mut writer = pool().begin(&store, basis(), 512 * 1024).unwrap();
        writer
            .append(EvidenceKind::RawSource, &mut &vec![17; PAGE_BYTES][..])
            .unwrap();
        let staged = writer.finish().unwrap();
        visit_staged(
            &store,
            staged.manifest_id,
            &staged.receipt,
            basis(),
            budget(),
            |_, _| Ok(()),
        )
        .unwrap();
        assert!(store.head().unwrap().is_none());
    }
}

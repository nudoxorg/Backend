//! Storage evidence for a future small owner commit. Nothing in this module
//! selects a workspace head or admits source/facts semantics. The owner must
//! still apply the existing relation and authenticated-base validators.
//!
//! Payloads are read and written one bounded CAS page at a time. Existing
//! FileStore immutable installation, closure composition and GC leases own
//! durability, cross-process recovery and abandoned-object reclamation.

#[cfg(test)]
use backend_store::ArtifactClosureClaim;
use backend_store::{
    ArtifactBudget, ArtifactSink, ClosureCompositionBudget, ClosureMembershipChange, FileStore,
    GcPinGuard, ObjectId, PinnedStoredClosureReceipt, StoreError, TypedObject, UntrustedObjectId,
};
use backend_version::{ObjectKey, Schema, SchemaIdentity};
use std::collections::BTreeSet;
use std::io::Read;
use std::sync::{Arc, Mutex};

const PAGE_BYTES: usize = 64 * 1024;
const PAGE_BATCH: usize = backend_store::MAX_OBJECT_BATCH_OBJECTS;
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

pub(super) fn staged_scope_limits(object: &TypedObject) -> Result<(usize, u64), StoreError> {
    admit_manifest_object(object)?;
    let (_, chains) = decode_manifest(object.bytes())?;
    let (pages, bytes) = chains
        .iter()
        .try_fold((0_u64, 0_u64), |(pages, bytes), chain| {
            Ok::<_, StoreError>((
                pages.checked_add(chain.pages).ok_or(StoreError::Bounds)?,
                bytes.checked_add(chain.bytes).ok_or(StoreError::Bounds)?,
            ))
        })?;
    Ok((
        usize::try_from(pages).map_err(|_| StoreError::Bounds)?,
        bytes,
    ))
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
    #[cfg(unix)]
    fn write_objects(&self, store: &FileStore, objects: &[TypedObject]) -> Result<(), StoreError> {
        use std::{
            fs::{self, OpenOptions},
            os::unix::fs::MetadataExt,
        };
        if objects.is_empty() {
            return Ok(());
        }
        if objects.len() > PAGE_BATCH {
            return Err(StoreError::Bounds);
        }
        let _pin = store.pin_garbage_collection()?;
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
        let selected = store
            .head()?
            .map(|head| store.open_closure(head.descriptor().closure()))
            .transpose()?;
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
            let mut raw_id = [0_u8; 32];
            for (slot, pair) in name.as_bytes().chunks_exact(2).enumerate() {
                raw_id[slot] = u8::from_str_radix(
                    std::str::from_utf8(pair).map_err(|_| StoreError::Corrupt)?,
                    16,
                )
                .map_err(|_| StoreError::Corrupt)?;
            }
            if selected
                .as_ref()
                .map(|index| {
                    index
                        .admit_claim(UntrustedObjectId::from_bytes(raw_id))
                        .map(|admitted| admitted.is_some())
                })
                .transpose()?
                .unwrap_or(false)
            {
                // Selection transfers this allocation out of the pending
                // admission catalog. Its CAS bytes remain under HEAD and
                // retained reader closure leases, with normal GC ownership.
                fs::remove_file(entry.path()).map_err(|e| StoreError::Io(e.to_string()))?;
                continue;
            }
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
        stage_checkpoint(StagePhase::QuotaScanned)?;
        // Preflight every distinct object while holding allocation.lock.
        // A failed preflight installs no CAS bytes and creates no new marker.
        let mut identities = BTreeSet::new();
        let mut additions = 0_u64;
        let mut targets = Vec::with_capacity(objects.len());
        let mut marker_paths = Vec::with_capacity(objects.len());
        for object in objects {
            let id = object.id();
            if !identities.insert(id) {
                return Err(StoreError::Corrupt);
            }
            let name = id
                .as_bytes()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let marker = markers.join(&name);
            let already_counted = marker.is_file();
            let target = store.root().join("objects").join(format!("{name}.object"));
            let addition = if already_counted {
                0
            } else {
                match fs::symlink_metadata(&target) {
                    Ok(metadata) => {
                        if !metadata.is_file()
                            || metadata.file_type().is_symlink()
                            || metadata.nlink() != 1
                        {
                            return Err(StoreError::Corrupt);
                        }
                        metadata
                            .blocks()
                            .checked_mul(512)
                            .ok_or(StoreError::Bounds)?
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let bytes = u64::try_from(object.bytes().len())
                            .map_err(|_| StoreError::Bounds)?
                            .checked_add(4096 + 128)
                            .ok_or(StoreError::Bounds)?;
                        bytes
                            .div_ceil(4096)
                            .checked_mul(4096)
                            .ok_or(StoreError::Bounds)?
                    }
                    Err(error) => return Err(StoreError::Io(error.to_string())),
                }
            };
            additions = additions.checked_add(addition).ok_or(StoreError::Bounds)?;
            if allocated.checked_add(additions).ok_or(StoreError::Bounds)? > self.max_reserved_bytes
            {
                return Err(StoreError::Bounds);
            }
            marker_paths.push(marker);
            if !already_counted {
                targets.push(target);
            }
        }
        let mut marker_files = Vec::with_capacity(marker_paths.len());
        for marker in &marker_paths {
            let file = backend_platform::durability::open_or_create_regular_file_nofollow(marker)
                .map_err(|error| StoreError::Io(error.to_string()))?;
            let metadata = file
                .metadata()
                .map_err(|error| StoreError::Io(error.to_string()))?;
            if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() != 0 {
                return Err(StoreError::Corrupt);
            }
            marker_files.push(file);
        }
        // Empty marker inodes still require their own barrier. Flush those
        // independent files together, then durably establish the complete
        // directory hierarchy BEFORE any object can acquire its CAS name.
        backend_platform::durability::flush_concurrently(&marker_files, |file| file.sync_all())
            .map_err(|error| StoreError::Io(error.to_string()))?;
        stage_checkpoint(StagePhase::MarkerFilesFlushed(marker_files.len()))?;
        for directory in [markers.as_path(), quota.as_path(), store.root()] {
            backend_platform::durability::open_directory_nofollow(directory)
                .and_then(|file| file.sync_all())
                .map_err(|error| StoreError::Io(error.to_string()))?;
            stage_checkpoint(StagePhase::MarkerDirectoryFlushed)?;
        }
        stage_checkpoint(StagePhase::MarkersDurable)?;
        drop(marker_files);
        let durable = store.write_object_batch(objects)?;
        if durable.writes().len() != objects.len()
            || durable
                .writes()
                .iter()
                .zip(objects)
                .any(|(write, object)| write.id() != object.id())
        {
            return Err(StoreError::Corrupt);
        }
        stage_checkpoint(StagePhase::ObjectsDurable)?;
        let mut actual = 0_u64;
        for target in targets {
            let metadata =
                fs::symlink_metadata(&target).map_err(|error| StoreError::Io(error.to_string()))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.nlink() != 1 {
                return Err(StoreError::Corrupt);
            }
            actual = actual
                .checked_add(
                    metadata
                        .blocks()
                        .checked_mul(512)
                        .ok_or(StoreError::Bounds)?,
                )
                .ok_or(StoreError::Bounds)?;
        }
        // A failed post-write check retains every marker. No failed or
        // cancelled batch can release physical quota by dropping its writer.
        if allocated.checked_add(actual).ok_or(StoreError::Bounds)? > self.max_reserved_bytes {
            return Err(StoreError::Bounds);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    fn write_objects(
        &self,
        _store: &FileStore,
        _objects: &[TypedObject],
    ) -> Result<(), StoreError> {
        // Physical-allocation accounting remains unavailable on this target.
        Err(StoreError::Bounds)
    }
    pub(super) fn metadata_required(pages: usize) -> Result<usize, StoreError> {
        ClosureCompositionBudget::metadata_bytes_for(
            pages.checked_add(1).ok_or(StoreError::Bounds)?,
        )?
        .checked_add(PAGE_BYTES * 8 + PAGE_BATCH * (PAGE_BYTES + PAGE_HEADER))
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
    /// Applies a newly pinned source policy while keeping all previous
    /// affine reservations in the same process-wide admission ledger.
    pub(super) fn with_limits(
        &self,
        max_live: usize,
        max_reserved_bytes: u64,
        max_pages: usize,
        max_metadata_bytes: usize,
    ) -> Self {
        Self {
            usage: Arc::clone(&self.usage),
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
        Ok(StageWriter {
            store: store.clone(),
            pool: self.clone(),
            basis,
            chains: [Chain::default(); 3],
            pending: std::array::from_fn(|_| Vec::new()),
            ids: Vec::new(),
            queued: Vec::with_capacity(PAGE_BATCH),
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
    queued: Vec<TypedObject>,
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
        let object = kind.object(&bytes);
        let id = object.id();
        self.ids.push(id);
        chain.tail = *id.as_bytes();
        chain.pages += 1;
        chain.bytes += payload.len() as u64;
        self.queued.push(object);
        if self.queued.len() == PAGE_BATCH {
            self.flush_queued()?;
        }
        Ok(())
    }
    fn flush_queued(&mut self) -> Result<(), StoreError> {
        self.pool.write_objects(&self.store, &self.queued)?;
        self.queued.clear();
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
        let manifest = typed::<StagedManifest>(&manifest);
        let manifest_id = manifest.id();
        self.queued.push(manifest);
        // No closure membership receipt can escape until the final partial
        // batch, including the manifest, has completed every storage barrier.
        self.flush_queued()?;
        stage_checkpoint(StagePhase::BeforeClosure)?;
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

/// Affine live-work reservation after exact reachability has transferred to
/// a per-closure membership lease. This does not hold a global GC barrier.
pub(super) struct StageAdmission {
    _lease: Reservation,
}

impl StagedEvidence {
    pub(super) fn into_transport(self) -> (ObjectId, PinnedStoredClosureReceipt, StageAdmission) {
        (
            self.manifest_id,
            self.receipt,
            StageAdmission {
                _lease: self._lease,
            },
        )
    }
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
    visit: impl FnMut(EvidenceKind, &[u8]) -> Result<(), StoreError>,
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
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                builder.mode(0o700);
            }
            builder.create(&directory).unwrap();
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
    fn stage_admission_charges_the_entire_bounded_queue_before_reading() {
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pages = 64;
        let former = ClosureCompositionBudget::metadata_bytes_for(pages + 1).unwrap()
            + PAGE_BYTES * 8
            + pages * size_of::<ObjectId>() * 4;
        let required = StagePool::metadata_required(pages).unwrap();
        assert_eq!(required - former, PAGE_BATCH * (PAGE_BYTES + PAGE_HEADER));
        let pool = StagePool::new(1, 8 * 1024 * 1024, pages, former);
        assert!(matches!(
            pool.begin(&store, basis(), 4 * 1024 * 1024),
            Err(StoreError::Bounds)
        ));
        let admitted = pool.with_limits(1, 8 * 1024 * 1024, pages, required);
        assert!(admitted.begin(&store, basis(), 4 * 1024 * 1024).is_ok());
        assert!(store.head().unwrap().is_none());
    }

    fn batch_pool() -> StagePool {
        StagePool::new(
            2,
            64 * 1024 * 1024,
            128,
            StagePool::metadata_required(128).unwrap(),
        )
    }
    fn storage_pages(count: usize) -> Vec<TypedObject> {
        (0..count)
            .map(|index| {
                let mut bytes = Vec::with_capacity(PAGE_HEADER + PAGE_BYTES);
                bytes.extend_from_slice(b"BSP1");
                bytes.extend_from_slice(&0_u64.to_be_bytes());
                bytes.extend_from_slice(&[0; 32]);
                bytes.extend_from_slice(&vec![index as u8; PAGE_BYTES]);
                EvidenceKind::RawSource.object(&bytes)
            })
            .collect()
    }
    fn reset_stage(fault: Option<StagePhase>) {
        STAGE_FAULT.with(|value| value.set(fault));
        STAGE_EVENTS.with_borrow_mut(|events| *events = Some(Vec::new()));
    }
    struct ResetStage;
    impl Drop for ResetStage {
        fn drop(&mut self) {
            reset_stage(None);
            STAGE_EVENTS.with_borrow_mut(|events| *events = None);
        }
    }
    fn markers(store: &FileStore) -> usize {
        let path = store.root().join("staged-ingest-quota/members");
        if !path.exists() {
            return 0;
        }
        std::fs::read_dir(path).unwrap().count()
    }
    fn present(store: &FileStore, objects: &[TypedObject]) -> usize {
        objects
            .iter()
            .filter(|object| store.contains_object(object.id()).unwrap())
            .count()
    }

    #[test]
    fn batch_quota_scans_and_barriers_are_amortized_without_skipping_markers() {
        let individual_directory = Directory::new();
        let individual = FileStore::open(individual_directory.path(), 1024 * 1024).unwrap();
        let batched_directory = Directory::new();
        let batched = FileStore::open(batched_directory.path(), 1024 * 1024).unwrap();
        let pool = batch_pool();
        let objects = storage_pages(PAGE_BATCH);
        reset_stage(None);
        let _reset = ResetStage;
        for object in &objects {
            pool.write_objects(&individual, std::slice::from_ref(object))
                .unwrap();
        }
        let prior = STAGE_EVENTS.with_borrow(|events| events.clone().unwrap_or_default());
        reset_stage(None);
        pool.write_objects(&batched, &objects).unwrap();
        let now = STAGE_EVENTS.with_borrow(|events| events.clone().unwrap_or_default());
        let count =
            |events: &[StagePhase], phase| events.iter().filter(|value| **value == phase).count();
        assert_eq!(count(&prior, StagePhase::QuotaScanned), PAGE_BATCH);
        assert_eq!(count(&now, StagePhase::QuotaScanned), 1);
        assert_eq!(
            count(&prior, StagePhase::MarkerDirectoryFlushed),
            PAGE_BATCH * 3
        );
        assert_eq!(count(&now, StagePhase::MarkerDirectoryFlushed), 3);
        assert_eq!(
            now,
            vec![
                StagePhase::QuotaScanned,
                StagePhase::MarkerFilesFlushed(PAGE_BATCH),
                StagePhase::MarkerDirectoryFlushed,
                StagePhase::MarkerDirectoryFlushed,
                StagePhase::MarkerDirectoryFlushed,
                StagePhase::MarkersDurable,
                StagePhase::ObjectsDurable
            ]
        );
        assert_eq!(markers(&individual), PAGE_BATCH);
        assert_eq!(markers(&batched), PAGE_BATCH);
        assert_eq!(present(&batched, &objects), PAGE_BATCH);
        assert!(batched.head().unwrap().is_none());
        eprintln!(
            "staged_batch_actual_work objects={PAGE_BATCH} individual_quota_scans={PAGE_BATCH} batch_quota_scans=1 individual_marker_directory_flushes={} batch_marker_directory_flushes=3",
            PAGE_BATCH * 3
        );
    }

    #[test]
    fn failed_marker_barriers_never_name_a_cas_member() {
        for fault in [
            StagePhase::QuotaScanned,
            StagePhase::MarkerFilesFlushed(PAGE_BATCH),
            StagePhase::MarkerDirectoryFlushed,
            StagePhase::MarkersDurable,
        ] {
            let directory = Directory::new();
            let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
            let pool = batch_pool();
            let objects = storage_pages(PAGE_BATCH);
            reset_stage(Some(fault));
            let _reset = ResetStage;
            assert!(pool.write_objects(&store, &objects).is_err());
            assert_eq!(present(&store, &objects), 0);
            assert_eq!(
                markers(&store),
                if fault == StagePhase::QuotaScanned {
                    0
                } else {
                    PAGE_BATCH
                }
            );
            assert!(store.head().unwrap().is_none());
            reset_stage(None);
            pool.write_objects(&store, &objects).unwrap();
            assert_eq!(present(&store, &objects), PAGE_BATCH);
            assert_eq!(markers(&store), PAGE_BATCH);
        }
    }

    #[test]
    fn full_and_partial_batches_preserve_exact_stream_and_manifest_identity() {
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pool = batch_pool();
        let source = (0..PAGE_BYTES * (PAGE_BATCH * 2 + 5) + 17)
            .map(|n| (n % 251) as u8)
            .collect::<Vec<_>>();
        let facts = vec![37; PAGE_BYTES + 3];
        let stage = || {
            let mut writer = pool.begin(&store, basis(), 16 * 1024 * 1024).unwrap();
            writer
                .append(EvidenceKind::RawSource, &mut source.as_slice())
                .unwrap();
            writer
                .append(EvidenceKind::CompleteFacts, &mut facts.as_slice())
                .unwrap();
            writer.finish().unwrap()
        };
        reset_stage(None);
        let _reset = ResetStage;
        let staged = stage();
        let events = STAGE_EVENTS.with_borrow(|events| events.clone().unwrap_or_default());
        assert_eq!(
            events
                .iter()
                .filter(|event| **event == StagePhase::QuotaScanned)
                .count(),
            3
        );
        assert_eq!(events.last(), Some(&StagePhase::BeforeClosure));
        let mut retained = [Vec::new(), Vec::new(), Vec::new()];
        let budget =
            ArtifactBudget::new(256, 256, 128 * 1024 * 1024, PAGE_BYTES + PAGE_HEADER, 4096);
        visit_staged(
            &store,
            staged.manifest_id,
            &staged.receipt,
            basis(),
            budget,
            |kind, bytes| {
                retained[kind.slot()].extend_from_slice(bytes);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(retained[0], source);
        assert_eq!(retained[2], facts);
        assert!(retained[1].is_empty());
        assert_eq!(markers(&store), PAGE_BATCH * 2 + 9);
        let manifest = staged.manifest_id;
        let closure = staged.receipt.receipt().closure();
        drop(staged);
        let replay = stage();
        assert_eq!(replay.manifest_id, manifest);
        assert_eq!(replay.receipt.receipt().closure(), closure);
        let mut stale = basis();
        stale.source_capture[0] ^= 1;
        assert!(matches!(
            visit_staged(
                &store,
                replay.manifest_id,
                &replay.receipt,
                stale,
                budget,
                |_, _| Ok(())
            ),
            Err(StoreError::WrongBase)
        ));
        assert!(store.head().unwrap().is_none());
    }

    #[test]
    #[cfg(unix)]
    fn partial_cas_failure_retains_physical_quota_and_retry_does_not_double_charge() {
        use std::os::unix::fs::MetadataExt as _;
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pool = batch_pool();
        let objects = storage_pages(PAGE_BATCH);
        reset_stage(Some(StagePhase::MarkersDurable));
        let _reset = ResetStage;
        assert!(pool.write_objects(&store, &objects).is_err());
        assert_eq!(markers(&store), PAGE_BATCH);
        assert_eq!(present(&store, &objects), 0);
        // Model the exact interrupted state produced by the store batch's
        // first-link crash test: all markers durable, only one CAS member.
        store.write_object(&objects[0]).unwrap();
        reset_stage(None);
        let cold = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let id = objects[0]
            .id()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let allocation =
            std::fs::metadata(cold.root().join("objects").join(format!("{id}.object")))
                .unwrap()
                .blocks()
                * 512;
        let tight = StagePool::new(1, allocation, 64, StagePool::metadata_required(64).unwrap());
        tight.write_objects(&cold, &objects[..1]).unwrap();
        assert_eq!(
            markers(&cold),
            1,
            "only absent objects lose accounting markers"
        );
        assert!(matches!(
            tight.write_objects(&cold, &objects[1..2]),
            Err(StoreError::Bounds)
        ));
        assert_eq!(present(&cold, &objects), 1);
        assert_eq!(markers(&cold), 1);
        assert!(cold.head().unwrap().is_none());
    }

    #[test]
    fn a_failed_durable_batch_poison_is_affine_and_never_issues_a_closure() {
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pool = batch_pool();
        reset_stage(Some(StagePhase::ObjectsDurable));
        let _reset = ResetStage;
        let mut writer = pool.begin(&store, basis(), 4 * 1024 * 1024).unwrap();
        assert!(
            writer
                .append(
                    EvidenceKind::RawSource,
                    &mut &vec![19; PAGE_BYTES * PAGE_BATCH][..]
                )
                .is_err()
        );
        assert_eq!(markers(&store), PAGE_BATCH);
        assert!(matches!(writer.finish(), Err(StoreError::Corrupt)));
        reset_stage(None);
        let retry = pool.begin(&store, basis(), 4 * 1024 * 1024).unwrap();
        drop(retry);
        assert_eq!(
            markers(&store),
            PAGE_BATCH,
            "dropping the affine writer does not release physical quota"
        );
        assert!(store.head().unwrap().is_none());
    }

    #[test]
    fn cancelled_stream_before_or_after_a_full_batch_cannot_publish_partial_evidence() {
        struct CancelAfter {
            remaining: usize,
        }
        impl Read for CancelAfter {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.remaining == 0 {
                    return Err(std::io::Error::other("cancelled source stream"));
                }
                let length = self.remaining.min(buffer.len());
                buffer[..length].fill(31);
                self.remaining -= length;
                Ok(length)
            }
        }
        for pages in [PAGE_BATCH - 1, PAGE_BATCH + 1] {
            let directory = Directory::new();
            let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
            let pool = batch_pool();
            let mut writer = pool.begin(&store, basis(), 4 * 1024 * 1024).unwrap();
            assert!(
                writer
                    .append(
                        EvidenceKind::RawSource,
                        &mut CancelAfter {
                            remaining: pages * PAGE_BYTES
                        }
                    )
                    .is_err()
            );
            let durable = if pages < PAGE_BATCH { 0 } else { PAGE_BATCH };
            assert_eq!(markers(&store), durable);
            assert!(matches!(writer.finish(), Err(StoreError::Corrupt)));
            assert!(pool.begin(&store, basis(), 4 * 1024 * 1024).is_ok());
            assert!(store.head().unwrap().is_none());
        }
    }

    #[test]
    fn interruption_before_closure_keeps_manifest_retry_exact() {
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let pool = batch_pool();
        reset_stage(Some(StagePhase::BeforeClosure));
        let _reset = ResetStage;
        let mut writer = pool.begin(&store, basis(), 64).unwrap();
        writer
            .append(EvidenceKind::RawSource, &mut &[7, 8, 9][..])
            .unwrap();
        assert!(writer.finish().is_err());
        assert_eq!(markers(&store), 2);
        assert!(store.head().unwrap().is_none());
        reset_stage(None);
        let mut writer = pool.begin(&store, basis(), 64).unwrap();
        writer
            .append(EvidenceKind::RawSource, &mut &[7, 8, 9][..])
            .unwrap();
        let staged = writer.finish().unwrap();
        assert_eq!(markers(&store), 2);
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
    fn reconfigured_limits_preserve_existing_affine_reservations() {
        let directory = Directory::new();
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        let initial = StagePool::new(2, 100, 32, StagePool::metadata_required(32).unwrap());
        let held = initial.begin(&store, basis(), 50).unwrap();
        let changed = initial.with_limits(2, 60, 32, StagePool::metadata_required(32).unwrap());
        assert!(matches!(
            changed.begin(&store, basis(), 20),
            Err(StoreError::Bounds)
        ));
        drop(held);
        assert!(changed.begin(&store, basis(), 20).is_ok());
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
            let mut writer = StagePool::new(
                1,
                8 * 1024 * 1024,
                64,
                StagePool::metadata_required(64).unwrap(),
            )
            .begin(&store, basis(), 4 * 1024 * 1024)
            .unwrap();
            writer
                .append(
                    EvidenceKind::RawSource,
                    &mut &vec![17; PAGE_BYTES * PAGE_BATCH][..],
                )
                .unwrap();
            // Deliberately skip Rust destructors: OS lease release and cold
            // store recovery must suffice, with no selected partial state.
            std::process::exit(77);
        }
        let directory = Directory::new();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "builtin::staged_intent::tests::crash_after_unselected_page_is_invisible",
                "--nocapture",
            ])
            .env(CHILD_DIRECTORY, directory.path())
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(77));
        let store = FileStore::open(directory.path(), 1024 * 1024).unwrap();
        assert!(store.head().unwrap().is_none());
        let mut writer = StagePool::new(
            1,
            8 * 1024 * 1024,
            64,
            StagePool::metadata_required(64).unwrap(),
        )
        .begin(&store, basis(), 512 * 1024)
        .unwrap();
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StagePhase {
    QuotaScanned,
    MarkerFilesFlushed(usize),
    MarkerDirectoryFlushed,
    MarkersDurable,
    ObjectsDurable,
    BeforeClosure,
}
#[cfg(not(test))]
fn stage_checkpoint(_phase: StagePhase) -> Result<(), StoreError> {
    Ok(())
}
#[cfg(test)]
std::thread_local! { static STAGE_FAULT:std::cell::Cell<Option<StagePhase>>=const { std::cell::Cell::new(None) }; }
#[cfg(test)]
std::thread_local! { static STAGE_EVENTS:std::cell::RefCell<Option<Vec<StagePhase>>>=const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn stage_checkpoint(phase: StagePhase) -> Result<(), StoreError> {
    STAGE_EVENTS.with_borrow_mut(|events| {
        if let Some(events) = events {
            events.push(phase);
        }
    });
    if STAGE_FAULT.with(|fault| fault.get() == Some(phase)) {
        Err(StoreError::Io(format!(
            "injected stage batch failure at {phase:?}"
        )))
    } else {
        Ok(())
    }
}

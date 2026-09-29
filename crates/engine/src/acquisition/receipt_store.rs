//! Durable product receipts layered over the registry owner's journal.
//!
//! Source snapshots and deltas are immutable, content-addressed blobs. A small
//! per-request head points at the most recent complete record and is the only
//! mutable name. The record itself binds the product request, admitted owner
//! cursor, metadata evidence, raw object, and terminal outcome.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::lease::AcquisitionLease;
#[cfg(test)]
use super::lease::LeaseGuard;
use super::{
    AcquisitionDelta, AcquisitionReceipt, AcquisitionRecordId, AcquisitionRequest, DeltaChange,
    ID_BYTES, NegativeFact, NegativeFactKind, RawArchiveObjectId, SourceSnapshot, SourceSnapshotId,
};

const FORMAT_VERSION: u16 = 1;
const MAX_PRODUCT_FILE_BYTES: u64 = 512 * 1024 * 1024;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReceiptPublishPhase {
    Snapshots,
    Delta,
    Record,
    Head,
}

/// Fully staged append-only product proof. Publishing its head is deliberately
/// separate so the large immutable blobs can be written outside the paired
/// endpoint/product publication gate.
#[derive(Clone, Debug)]
pub(super) struct PreparedAcquisitionRecord {
    id: AcquisitionRecordId,
    source_intent: [u8; ID_BYTES],
}

/// The terminal source result captured by a product receipt.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum AcquisitionProductTerminal {
    /// Verified archive plus exact metadata was admitted and published.
    Published,
    /// The source returned an authoritative negative release fact.
    NegativeFact(NegativeFact),
}

/// Durable, restart-recoverable acquisition proof for one exact product intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcquisitionRecoveryRecord {
    /// Content identity of this immutable record.
    pub id: AcquisitionRecordId,
    /// Exact request key that selected this record.
    pub source_intent: [u8; ID_BYTES],
    /// Credential-free source identity.
    pub source: [u8; ID_BYTES],
    /// Canonical product coordinate.
    pub coordinate: Arc<str>,
    /// Adapter protocol schema version.
    pub schema: u16,
    /// Policy epoch bound to the record.
    pub policy_epoch: u64,
    /// Exact owner cursor admitted with the terminal result.
    pub owner_cursor: [u8; ID_BYTES],
    /// Mutable source facts root admitted with the terminal result.
    pub facts_frontier: [u8; ID_BYTES],
    /// Digest of exact source metadata evidence, when the source returned a row.
    pub metadata_digest: Option<[u8; ID_BYTES]>,
    /// Verified raw archive identity, when the source returned an archive row.
    pub raw_object: Option<RawArchiveObjectId>,
    /// Wall-clock time at which this terminal state was observed.
    pub observed_at_millis: u64,
    /// Durable terminal state.
    pub terminal: AcquisitionProductTerminal,
    /// Base catalog root for a published result.
    pub base_snapshot: Option<Arc<SourceSnapshot>>,
    /// Target catalog root for a published result.
    pub target_snapshot: Option<Arc<SourceSnapshot>>,
    /// Root-bound source delta for a published result.
    pub delta: Option<Arc<AcquisitionDelta>>,
    /// Generic versioned receipt for a published result.
    pub receipt: Option<Arc<AcquisitionReceipt>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredDelta {
    version: u16,
    base: SourceSnapshotId,
    target: SourceSnapshotId,
    changes: Vec<DeltaChange>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredRecordBody {
    version: u16,
    source_intent: [u8; ID_BYTES],
    source: [u8; ID_BYTES],
    coordinate: Arc<str>,
    schema: u16,
    policy_epoch: u64,
    owner_cursor: [u8; ID_BYTES],
    facts_frontier: [u8; ID_BYTES],
    metadata_digest: Option<[u8; ID_BYTES]>,
    raw_object: Option<RawArchiveObjectId>,
    observed_at_millis: u64,
    terminal: AcquisitionProductTerminal,
    base_snapshot: Option<SourceSnapshotId>,
    target_snapshot: Option<SourceSnapshotId>,
    delta: Option<super::AcquisitionDeltaId>,
    receipt: Option<AcquisitionReceipt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredRecord {
    id: AcquisitionRecordId,
    body: StoredRecordBody,
}

/// Filesystem-backed immutable receipt blobs and per-intent selected heads.
#[derive(Clone, Debug)]
pub(super) struct AcquisitionReceiptStore {
    root: Arc<PathBuf>,
}

impl AcquisitionReceiptStore {
    pub(super) fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        for directory in ["snapshots", "deltas", "records", "heads", "temps"] {
            fs::create_dir_all(root.join(directory))?;
        }
        for directory in ["snapshots", "deltas", "records", "heads", "temps"] {
            sync_directory(&root.join(directory))?;
        }
        sync_directory(&root)?;
        if let Some(parent) = root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            sync_directory(parent)?;
        }
        Ok(Self {
            root: Arc::new(root),
        })
    }

    #[cfg(test)]
    pub(super) fn publish(
        &self,
        record: AcquisitionRecoveryRecord,
        lease: &mut LeaseGuard,
    ) -> io::Result<Option<AcquisitionRecordId>> {
        self.publish_after(record, lease, |_| Ok(()))
    }

    pub(super) fn prepare(
        &self,
        record: AcquisitionRecoveryRecord,
    ) -> io::Result<PreparedAcquisitionRecord> {
        self.prepare_after(record, |_| Ok(()))
    }

    #[cfg(test)]
    fn publish_after(
        &self,
        record: AcquisitionRecoveryRecord,
        lease: &mut LeaseGuard,
        mut after: impl FnMut(ReceiptPublishPhase) -> io::Result<()>,
    ) -> io::Result<Option<AcquisitionRecordId>> {
        let prepared = self.prepare_after(record, &mut after)?;
        let published = lease.publish_if_current(Duration::from_secs(30), |_lease| {
            self.publish_head(&prepared)?;
            after(ReceiptPublishPhase::Head)
        })?;
        Ok(published.map(|()| prepared.id))
    }

    fn prepare_after(
        &self,
        mut record: AcquisitionRecoveryRecord,
        mut after: impl FnMut(ReceiptPublishPhase) -> io::Result<()>,
    ) -> io::Result<PreparedAcquisitionRecord> {
        self.validate_record(&record)?;
        if let Some(snapshot) = &record.base_snapshot {
            self.publish_snapshot(snapshot)?;
        }
        if let Some(snapshot) = &record.target_snapshot {
            self.publish_snapshot(snapshot)?;
        }
        after(ReceiptPublishPhase::Snapshots)?;
        if let Some(delta) = &record.delta {
            let body = StoredDelta {
                version: FORMAT_VERSION,
                base: delta.base(),
                target: delta.target(),
                changes: delta.changes().to_vec(),
            };
            let bytes = serde_json::to_vec(&body).map_err(invalid_data)?;
            let path = self.root.join("deltas").join(hex(delta.id().as_bytes()));
            publish_immutable(&path, &bytes, &self.root.join("temps"))?;
        }
        after(ReceiptPublishPhase::Delta)?;

        let body = self.body_for(&record);
        let body_bytes = serde_json::to_vec(&body).map_err(invalid_data)?;
        let id =
            AcquisitionRecordId::derive(&[b"backend.acquisition.product-record.v1\0", &body_bytes]);
        record.id = id;
        let stored = StoredRecord { id, body };
        let bytes = serde_json::to_vec(&stored).map_err(invalid_data)?;
        let path = self.root.join("records").join(hex(id.as_bytes()));
        publish_immutable(&path, &bytes, &self.root.join("temps"))?;
        after(ReceiptPublishPhase::Record)?;

        Ok(PreparedAcquisitionRecord {
            id,
            source_intent: record.source_intent,
        })
    }

    /// Performs only the short mutable-name replacement. Call this solely
    /// inside the exact endpoint/product paired-fence callback after the
    /// caller has refreshed and matched the owner cursor and facts frontier.
    pub(super) fn publish_head_fenced(
        &self,
        prepared: &PreparedAcquisitionRecord,
        _endpoint_fence: &AcquisitionLease,
        _product_fence: &AcquisitionLease,
    ) -> io::Result<AcquisitionRecordId> {
        self.publish_head(prepared)?;
        Ok(prepared.id)
    }

    fn publish_head(&self, prepared: &PreparedAcquisitionRecord) -> io::Result<()> {
        let head = self.head_path(prepared.source_intent);
        atomic_replace(&head, &prepared.id.to_bytes(), &self.root.join("temps"))
    }

    pub(super) fn recover(
        &self,
        request: &AcquisitionRequest,
        owner_cursor: [u8; ID_BYTES],
        facts_frontier: [u8; ID_BYTES],
        policy_epoch: u64,
    ) -> io::Result<Option<AcquisitionRecoveryRecord>> {
        let source_intent = request.source_intent();
        let Some((id, body)) = self.selected_body(source_intent)? else {
            return Ok(None);
        };
        if body.version != FORMAT_VERSION
            || body.source_intent != source_intent
            || body.source != request.source
            || body.coordinate.as_ref() != request.coordinate.as_ref()
            || body.schema != request.schema
            || body.policy_epoch != policy_epoch
            || body.owner_cursor != owner_cursor
            || body.facts_frontier != facts_frontier
        {
            // A well-formed older head is a cache miss. Newer policy/cursor
            // evidence must never be inferred from a previous epoch.
            return Ok(None);
        }
        self.load_record(id, body).map(Some)
    }

    pub(super) fn recover_negative(
        &self,
        request: &AcquisitionRequest,
        owner_cursor: [u8; ID_BYTES],
        facts_frontier: [u8; ID_BYTES],
        policy_epoch: u64,
    ) -> io::Result<Option<AcquisitionRecoveryRecord>> {
        let Some((id, body)) = self.selected_body(request.source_intent())? else {
            return Ok(None);
        };
        if body.version != FORMAT_VERSION
            || body.source_intent != request.source_intent()
            || body.source != request.source
            || body.coordinate.as_ref() != request.coordinate.as_ref()
            || body.schema != request.schema
            || body.policy_epoch != policy_epoch
            || body.owner_cursor != owner_cursor
            || body.facts_frontier != facts_frontier
        {
            return Ok(None);
        }
        let AcquisitionProductTerminal::NegativeFact(_) = body.terminal else {
            return Ok(None);
        };
        let record = self.load_record(id, body)?;
        if !matches!(record.terminal, AcquisitionProductTerminal::NegativeFact(_)) {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "negative record changed terminal",
            ))
        } else {
            Ok(Some(record))
        }
    }

    fn selected_body(
        &self,
        source_intent: [u8; ID_BYTES],
    ) -> io::Result<Option<(AcquisitionRecordId, StoredRecordBody)>> {
        let head = self.head_path(source_intent);
        let bytes = match fs::read(head) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if bytes.len() != ID_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid acquisition head",
            ));
        }
        let mut encoded_id = [0; ID_BYTES];
        encoded_id.copy_from_slice(&bytes);
        let id = AcquisitionRecordId::from_encoded(encoded_id);
        let record_path = self.root.join("records").join(hex(id.as_bytes()));
        let stored = read_bounded_json::<StoredRecord>(&record_path)?;
        if stored.id != id || Self::record_id(&stored.body)? != id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "acquisition record digest mismatch",
            ));
        }
        Ok(Some((id, stored.body)))
    }

    fn head_path(&self, source_intent: [u8; ID_BYTES]) -> PathBuf {
        self.root
            .join("heads")
            .join(format!("{}.head", hex(&source_intent)))
    }

    fn body_for(&self, record: &AcquisitionRecoveryRecord) -> StoredRecordBody {
        StoredRecordBody {
            version: FORMAT_VERSION,
            source_intent: record.source_intent,
            source: record.source,
            coordinate: Arc::clone(&record.coordinate),
            schema: record.schema,
            policy_epoch: record.policy_epoch,
            owner_cursor: record.owner_cursor,
            facts_frontier: record.facts_frontier,
            metadata_digest: record.metadata_digest,
            raw_object: record.raw_object,
            observed_at_millis: record.observed_at_millis,
            terminal: record.terminal,
            base_snapshot: record.base_snapshot.as_ref().map(|snapshot| snapshot.id()),
            target_snapshot: record
                .target_snapshot
                .as_ref()
                .map(|snapshot| snapshot.id()),
            delta: record.delta.as_ref().map(|delta| delta.id()),
            receipt: record.receipt.as_deref().cloned(),
        }
    }

    fn record_id(body: &StoredRecordBody) -> io::Result<AcquisitionRecordId> {
        let bytes = serde_json::to_vec(body).map_err(invalid_data)?;
        Ok(AcquisitionRecordId::derive(&[
            b"backend.acquisition.product-record.v1\0",
            &bytes,
        ]))
    }

    fn load_record(
        &self,
        id: AcquisitionRecordId,
        body: StoredRecordBody,
    ) -> io::Result<AcquisitionRecoveryRecord> {
        let load_snapshot =
            |snapshot: Option<SourceSnapshotId>| -> io::Result<Option<Arc<SourceSnapshot>>> {
                let Some(snapshot) = snapshot else {
                    return Ok(None);
                };
                let path = self.root.join("snapshots").join(hex(snapshot.as_bytes()));
                let snapshot_value = read_bounded_json::<SourceSnapshot>(&path)?;
                snapshot_value.validate().map_err(invalid_data)?;
                if snapshot_value.id() != snapshot {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "snapshot id mismatch",
                    ));
                }
                Ok(Some(Arc::new(snapshot_value)))
            };
        let base_snapshot = load_snapshot(body.base_snapshot)?;
        let target_snapshot = load_snapshot(body.target_snapshot)?;
        let delta = match body.delta {
            Some(delta_id) => {
                let base = base_snapshot.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "delta has no base snapshot")
                })?;
                let target = target_snapshot.as_ref().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "delta has no target snapshot")
                })?;
                let path = self.root.join("deltas").join(hex(delta_id.as_bytes()));
                let stored = read_bounded_json::<StoredDelta>(&path)?;
                if stored.version != FORMAT_VERSION
                    || stored.base != base.id()
                    || stored.target != target.id()
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "delta root mismatch",
                    ));
                }
                let delta = AcquisitionDelta::new(base, Arc::clone(target), stored.changes)
                    .map_err(invalid_data)?;
                if delta.id() != delta_id {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "delta id mismatch",
                    ));
                }
                Some(Arc::new(delta))
            }
            None => None,
        };
        let record = AcquisitionRecoveryRecord {
            id,
            source_intent: body.source_intent,
            source: body.source,
            coordinate: body.coordinate,
            schema: body.schema,
            policy_epoch: body.policy_epoch,
            owner_cursor: body.owner_cursor,
            facts_frontier: body.facts_frontier,
            metadata_digest: body.metadata_digest,
            raw_object: body.raw_object,
            observed_at_millis: body.observed_at_millis,
            terminal: body.terminal,
            base_snapshot,
            target_snapshot,
            delta,
            receipt: body.receipt.map(Arc::new),
        };
        self.validate_record(&record)?;
        Ok(record)
    }

    fn publish_snapshot(&self, snapshot: &SourceSnapshot) -> io::Result<()> {
        snapshot.validate().map_err(invalid_data)?;
        let bytes = serde_json::to_vec(snapshot).map_err(invalid_data)?;
        let path = self
            .root
            .join("snapshots")
            .join(hex(snapshot.id().as_bytes()));
        publish_immutable(&path, &bytes, &self.root.join("temps"))
    }

    fn validate_record(&self, record: &AcquisitionRecoveryRecord) -> io::Result<()> {
        let valid = match record.terminal {
            AcquisitionProductTerminal::Published => {
                let (Some(base), Some(target), Some(delta), Some(receipt)) = (
                    &record.base_snapshot,
                    &record.target_snapshot,
                    &record.delta,
                    &record.receipt,
                ) else {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "published receipt is incomplete",
                    ));
                };
                base.validate().map_err(invalid_data)?;
                target.validate().map_err(invalid_data)?;
                delta.validate(base).map_err(invalid_data)?;
                record
                    .metadata_digest
                    .is_some_and(|digest| digest != [0; ID_BYTES])
                    && record.raw_object == Some(receipt.raw_object)
                    && record.metadata_digest == Some(receipt.metadata_digest)
                    && record.source_intent == receipt.source_intent
                    && record.owner_cursor == receipt.owner_cursor
                    && record.policy_epoch == receipt.policy_epoch
                    && record.observed_at_millis == receipt.observed_at_millis
                    && record.owner_cursor == target.cursor()
                    && record.facts_frontier == target.facts_frontier()
                    && record.source == target.source()
                    && record.source == base.source()
                    && record.policy_epoch == base.policy_epoch()
                    && delta.base() == base.id()
                    && delta.target() == target.id()
                    && receipt.base == base.id()
                    && receipt.target == target.id()
                    && receipt.delta == delta.id()
                    && target
                        .manifest()
                        .entries()
                        .binary_search_by(|entry| {
                            entry.path.as_ref().cmp(record.coordinate.as_ref())
                        })
                        .is_ok_and(|index| {
                            target.manifest().entries()[index].object == receipt.raw_object
                        })
                    && receipt.validate(record.source)
            }
            AcquisitionProductTerminal::NegativeFact(fact) => {
                let metadata_required = matches!(
                    fact.kind,
                    NegativeFactKind::Yanked | NegativeFactKind::AdvisoryBlocked
                );
                record.base_snapshot.is_none()
                    && record.target_snapshot.is_none()
                    && record.delta.is_none()
                    && record.receipt.is_none()
                    && fact.authority == record.source
                    && fact.cursor == record.owner_cursor
                    && fact.source_proof == record.owner_cursor
                    && fact.policy_epoch == record.policy_epoch
                    && fact.observed_at_millis == record.observed_at_millis
                    && fact.expires_at_millis > fact.observed_at_millis
                    && record.metadata_digest.is_some() == metadata_required
                    && record.raw_object.is_some() == metadata_required
            }
        };
        if !valid {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "acquisition product receipt failed validation",
            ));
        }
        Ok(())
    }
}

fn read_bounded_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    let metadata = fs::metadata(path)?;
    if metadata.len() > MAX_PRODUCT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "acquisition product record exceeds bound",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::open(path)?
        .take(MAX_PRODUCT_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PRODUCT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "acquisition product record exceeds bound",
        ));
    }
    serde_json::from_slice(&bytes).map_err(invalid_data)
}

fn publish_immutable(path: &Path, bytes: &[u8], temp_root: &Path) -> io::Result<()> {
    publish_immutable_with(path, bytes, temp_root, || Ok(()), sync_directory)
}

fn publish_immutable_with(
    path: &Path,
    bytes: &[u8],
    temp_root: &Path,
    before_link: impl FnOnce() -> io::Result<()>,
    mut sync_parent: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    if bytes.len() as u64 > MAX_PRODUCT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "acquisition product record exceeds bound",
        ));
    }
    if path.exists() {
        compare_existing(path, bytes)?;
        return sync_parent(path.parent().expect("product parent"));
    }
    let temp = write_temp(temp_root, bytes)?;
    if let Err(error) = before_link() {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }
    match fs::hard_link(&temp, path) {
        Ok(()) => sync_parent(path.parent().expect("product parent"))?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            compare_existing(path, bytes)?;
            sync_parent(path.parent().expect("product parent"))?;
        }
        Err(error) => return Err(error),
    }
    let _ = fs::remove_file(temp);
    Ok(())
}

fn atomic_replace(path: &Path, bytes: &[u8], temp_root: &Path) -> io::Result<()> {
    let temp = write_temp(temp_root, bytes)?;
    match backend_platform::durable::replace_file(&temp, path) {
        Ok(()) => sync_directory(path.parent().expect("product parent")),
        Err(error) => {
            let _ = fs::remove_file(temp);
            Err(error)
        }
    }
}

fn write_temp(directory: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = directory.join(format!("{}.{}.tmp", std::process::id(), id));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(path)
}

fn compare_existing(path: &Path, expected: &[u8]) -> io::Result<()> {
    if fs::metadata(path)?.len() != expected.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed acquisition record collision",
        ));
    }
    let existing = fs::read(path)?;
    if existing == expected {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed acquisition record collision",
        ))
    }
}

fn sync_directory(path: &Path) -> io::Result<()> {
    backend_platform::durability::open_directory(path)?.sync_all()
}

fn invalid_data(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acquisition::{
        AcquisitionOutcome, AcquisitionRequest, DeltaChange, FactFreshness, ManifestEntry,
        MetadataRecord, Policy, ReleaseClaim, Resolve, TreeManifest,
    };
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "acquisition-product-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create fixture");
        path
    }

    #[test]
    fn reused_immutable_blob_flushes_its_parent_directory() {
        let root = temporary("reuse-dir-sync");
        let parent = root.join("records");
        let temps = root.join("temps");
        fs::create_dir_all(&parent).expect("records directory");
        fs::create_dir_all(&temps).expect("temps directory");
        let target = parent.join("record");
        fs::write(&target, b"identical immutable bytes").expect("seed complete record");
        let mut syncs = 0;

        publish_immutable_with(
            &target,
            b"identical immutable bytes",
            &temps,
            || Ok(()),
            |directory| {
                assert_eq!(directory, parent);
                syncs += 1;
                Ok(())
            },
        )
        .expect("verify reused record and repair its durability barrier");

        assert_eq!(syncs, 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn concurrent_immutable_winner_is_verified_and_directory_flushed() {
        let root = temporary("race-dir-sync");
        let parent = root.join("records");
        let temps = root.join("temps");
        fs::create_dir_all(&parent).expect("records directory");
        fs::create_dir_all(&temps).expect("temps directory");
        let target = parent.join("record");
        let mut syncs = 0;

        publish_immutable_with(
            &target,
            b"concurrent immutable bytes",
            &temps,
            || fs::write(&target, b"concurrent immutable bytes"),
            |directory| {
                assert_eq!(directory, parent);
                syncs += 1;
                Ok(())
            },
        )
        .expect("verify concurrent winner and flush its parent");

        assert_eq!(syncs, 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    fn published_fixture() -> (AcquisitionRecoveryRecord, AcquisitionRequest) {
        let source = [3; ID_BYTES];
        let cursor = [4; ID_BYTES];
        let facts_frontier = [5; ID_BYTES];
        let coordinate = "pkg:cargo/receipt-demo@1.0.0";
        let object = RawArchiveObjectId::from_bytes(b"admitted archive");
        let request = AcquisitionRequest::for_coordinate(source, coordinate, 1, 7)
            .expect("request")
            .with_fact_freshness(FactFreshness::max_age_millis(500));
        let base = Arc::new(
            SourceSnapshot::new_with_frontier(
                source,
                [1; ID_BYTES],
                7,
                facts_frontier,
                Arc::new(TreeManifest::new(Vec::new()).expect("empty manifest")),
                Vec::new(),
            )
            .expect("base snapshot"),
        );
        let claim = ReleaseClaim::new(source, coordinate, "1.0.0", object).expect("claim");
        let target = Arc::new(
            SourceSnapshot::new_with_frontier(
                source,
                cursor,
                7,
                facts_frontier,
                Arc::new(
                    TreeManifest::new(vec![ManifestEntry {
                        path: Arc::from(coordinate),
                        object,
                        mode: 0,
                    }])
                    .expect("manifest"),
                ),
                vec![claim.id],
            )
            .expect("target snapshot"),
        );
        let effective =
            AcquisitionRequest::new(source, coordinate, object, 1, 7).expect("effective request");
        let metadata = Resolve::new(effective)
            .metadata(MetadataRecord {
                claim,
                length: 16,
                source_proof: cursor,
                evidence_digest: [8; ID_BYTES],
                source_intent: request.source_intent(),
            })
            .expect("metadata");
        let published = metadata
            .object(object)
            .expect("object")
            .verified(object)
            .expect("verified")
            .policy(true)
            .expect("policy")
            .publish(
                &base,
                Arc::clone(&target),
                vec![DeltaChange {
                    path: Arc::from(coordinate),
                    before: None,
                    before_mode: None,
                    after: Some(object),
                    after_mode: Some(0),
                }],
            );
        let AcquisitionOutcome::Hit(published) = published else {
            panic!("publication receipt must be admitted");
        };
        let record = AcquisitionRecoveryRecord {
            id: AcquisitionRecordId::from_encoded([0; ID_BYTES]),
            source_intent: request.source_intent(),
            source,
            coordinate: Arc::from(coordinate),
            schema: 1,
            policy_epoch: 7,
            owner_cursor: cursor,
            facts_frontier,
            metadata_digest: Some([8; ID_BYTES]),
            raw_object: Some(object),
            observed_at_millis: published.receipt.observed_at_millis,
            terminal: AcquisitionProductTerminal::Published,
            base_snapshot: Some(base),
            target_snapshot: Some(target),
            delta: Some(published.delta),
            receipt: Some(published.receipt),
        };
        (record, request)
    }

    fn lease(root: &Path, request: &AcquisitionRequest) -> (LeaseStore, super::super::LeaseGuard) {
        let locks = LeaseStore::open(root.join("leases")).expect("open leases");
        let guard = locks
            .acquire(request.receipt_work_key(), Duration::from_secs(30))
            .expect("acquire product lease")
            .expect("product lease available");
        (locks, guard)
    }

    #[test]
    fn cold_reopen_recovers_receipt_delta_and_metadata_without_archive_access() {
        let root = temporary("cold-reopen");
        let (record, request) = published_fixture();
        let store = AcquisitionReceiptStore::open(root.join("receipts")).expect("store");
        let (_locks, mut lease) = lease(&root, &request);
        store
            .publish(record.clone(), &mut lease)
            .expect("publish receipt")
            .expect("lease still current");
        drop(lease);
        drop(store);

        let reopened = AcquisitionReceiptStore::open(root.join("receipts")).expect("reopen");
        let recovered = reopened
            .recover(&request, record.owner_cursor, record.facts_frontier, 7)
            .expect("read receipt")
            .expect("durable receipt");
        assert_eq!(recovered.metadata_digest, Some([8; ID_BYTES]));
        assert_eq!(recovered.raw_object, record.raw_object);
        assert_eq!(recovered.receipt, record.receipt);
        assert_eq!(
            recovered
                .delta
                .as_ref()
                .expect("recovered delta")
                .apply(recovered.base_snapshot.as_deref().expect("base"))
                .expect("apply recovered delta")
                .id(),
            recovered.target_snapshot.as_deref().expect("target").id()
        );
    }

    #[test]
    fn crashes_before_head_cas_leave_only_unreachable_append_only_blobs() {
        for stopped_at in [
            ReceiptPublishPhase::Snapshots,
            ReceiptPublishPhase::Delta,
            ReceiptPublishPhase::Record,
        ] {
            let root = temporary("crash-phase");
            let (record, request) = published_fixture();
            let store = AcquisitionReceiptStore::open(root.join("receipts")).expect("store");
            let (_locks, mut lease) = lease(&root, &request);
            let failure = store.publish_after(record.clone(), &mut lease, |phase| {
                if phase == stopped_at {
                    Err(io::Error::other("simulated process stop"))
                } else {
                    Ok(())
                }
            });
            assert!(failure.is_err());
            drop(lease);
            drop(store);
            let reopened = AcquisitionReceiptStore::open(root.join("receipts")).expect("reopen");
            assert!(
                reopened
                    .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                    .expect("recover after crash")
                    .is_none()
            );
        }

        let root = temporary("after-head");
        let (record, request) = published_fixture();
        let store = AcquisitionReceiptStore::open(root.join("receipts")).expect("store");
        let (_locks, mut lease) = lease(&root, &request);
        assert!(
            store
                .publish_after(record.clone(), &mut lease, |phase| {
                    if phase == ReceiptPublishPhase::Head {
                        Err(io::Error::other("simulated process stop after CAS"))
                    } else {
                        Ok(())
                    }
                })
                .is_err()
        );
        drop(lease);
        drop(store);
        let reopened = AcquisitionReceiptStore::open(root.join("receipts")).expect("reopen");
        assert!(
            reopened
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("recover published head")
                .is_some()
        );
    }

    #[test]
    fn a_stale_fence_cannot_publish_the_product_head() {
        let root = temporary("stale-fence");
        let (record, request) = published_fixture();
        let store = AcquisitionReceiptStore::open(root.join("receipts")).expect("store");
        let locks = LeaseStore::open(root.join("leases")).expect("lease store");
        let mut stale = locks
            .acquire(request.receipt_work_key(), Duration::ZERO)
            .expect("acquire expired lease")
            .expect("first lease");
        let mut current = locks
            .acquire(request.receipt_work_key(), Duration::from_secs(30))
            .expect("take over expired lease")
            .expect("current lease");
        assert!(
            store
                .publish(record.clone(), &mut stale)
                .expect("stale publication is rejected")
                .is_none()
        );
        assert!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("head remains absent")
                .is_none()
        );
        assert!(
            store
                .publish(record.clone(), &mut current)
                .expect("current publication")
                .is_some()
        );
        assert!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("current head recovers")
                .is_some()
        );
    }

    #[test]
    fn negative_terminal_facts_survive_reopen_and_are_policy_epoch_bound() {
        for (suffix, kind, metadata) in [
            ("missing", NegativeFactKind::NotFound, false),
            ("yanked", NegativeFactKind::Yanked, true),
            ("advisory", NegativeFactKind::AdvisoryBlocked, true),
        ] {
            let root = temporary("negative-facts");
            let request = AcquisitionRequest::for_coordinate(
                [3; ID_BYTES],
                format!("pkg:cargo/{suffix}@1.0.0"),
                1,
                7,
            )
            .expect("request");
            let cursor = [4; ID_BYTES];
            let fact = NegativeFact {
                kind,
                authority: request.source,
                source_proof: cursor,
                cursor,
                observed_at_millis: 100,
                expires_at_millis: 200,
                policy_epoch: 7,
            };
            let record = AcquisitionRecoveryRecord {
                id: AcquisitionRecordId::from_encoded([0; ID_BYTES]),
                source_intent: request.source_intent(),
                source: request.source,
                coordinate: Arc::clone(&request.coordinate),
                schema: request.schema,
                policy_epoch: 7,
                owner_cursor: cursor,
                facts_frontier: [5; ID_BYTES],
                metadata_digest: metadata.then_some([8; ID_BYTES]),
                raw_object: metadata.then_some(RawArchiveObjectId::from_bytes(b"archive")),
                observed_at_millis: fact.observed_at_millis,
                terminal: AcquisitionProductTerminal::NegativeFact(fact),
                base_snapshot: None,
                target_snapshot: None,
                delta: None,
                receipt: None,
            };
            let store = AcquisitionReceiptStore::open(root.join("receipts")).expect("store");
            let (_locks, mut lease) = lease(&root, &request);
            store
                .publish(record, &mut lease)
                .expect("publish negative fact")
                .expect("current lease");
            drop(lease);
            drop(store);

            let reopened = AcquisitionReceiptStore::open(root.join("receipts")).expect("reopen");
            let recovered = reopened
                .recover_negative(&request, cursor, [5; ID_BYTES], 7)
                .expect("recover terminal fact")
                .expect("durable negative record");
            assert_eq!(
                recovered.terminal,
                AcquisitionProductTerminal::NegativeFact(fact)
            );
            assert_eq!(recovered.metadata_digest, metadata.then_some([8; ID_BYTES]));
            assert_eq!(
                recovered.raw_object,
                metadata.then_some(RawArchiveObjectId::from_bytes(b"archive"))
            );
            assert!(
                reopened
                    .recover_negative(&request, cursor, [5; ID_BYTES], 8)
                    .expect("stale epoch is a miss")
                    .is_none()
            );
        }
    }
}

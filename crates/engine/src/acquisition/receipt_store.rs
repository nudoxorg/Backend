//! Durable product receipts layered over the registry owner's journal.
//!
//! Source snapshots and deltas are immutable, content-addressed blobs. A small
//! per-request head points at the most recent complete record and is the only
//! mutable name. The record itself binds the product request, admitted owner
//! cursor, metadata evidence, raw object, and terminal outcome.

use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
#[cfg(test)]
use std::{
    fs::{self, OpenOptions},
    time::Duration,
};

use backend_platform::directory::{DirectoryCapability, DirectoryRenameError};
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

#[derive(Serialize)]
struct BorrowedStoredDelta<'a> {
    version: u16,
    base: SourceSnapshotId,
    target: SourceSnapshotId,
    changes: &'a [DeltaChange],
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
    receipt: Option<Arc<AcquisitionReceipt>>,
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
    snapshots: DirectoryCapability,
    deltas: DirectoryCapability,
    records: DirectoryCapability,
    heads: DirectoryCapability,
}

impl AcquisitionReceiptStore {
    pub(super) fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let requested_root = root.into();
        let root_name = requested_root
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid receipt root"))?;
        let parent_path = requested_root
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // Pin the parent before creating/opening the root. All receipt I/O
        // below remains relative to these held handles, so an ancestor rename
        // or replacement cannot redirect a later path-based operation.
        let parent = DirectoryCapability::open(parent_path)?;
        let (root_directory, created_root) = match parent.open_dir(root_name) {
            Ok(directory) => {
                directory.restrict_private()?;
                directory.validate_private()?;
                (directory, false)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                (parent.create_private_dir(root_name)?, true)
            }
            Err(error) => return Err(error),
        };
        let mut children = Vec::with_capacity(5);
        for name in ["snapshots", "deltas", "records", "heads", "temps"] {
            let child = match root_directory.open_dir(name) {
                Ok(directory) => {
                    directory.restrict_private()?;
                    directory.validate_private()?;
                    directory
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    root_directory.create_private_dir(name)?
                }
                Err(error) => return Err(error),
            };
            child.sync_all()?;
            children.push(child);
        }
        root_directory.sync_all()?;
        if created_root {
            parent.sync_all()?;
        }
        let [snapshots, deltas, records, heads, _temps]: [_; 5] = children
            .try_into()
            .map_err(|_| io::Error::other("receipt directory initialization failed"))?;
        Ok(Self {
            root: Arc::new(requested_root),
            snapshots,
            deltas,
            records,
            heads,
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
        record: AcquisitionRecoveryRecord,
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
            let body = BorrowedStoredDelta {
                version: FORMAT_VERSION,
                base: delta.base(),
                target: delta.target(),
                changes: delta.changes(),
            };
            let name = hex(delta.id().as_bytes());
            publish_json_immutable_at(&self.deltas, &name, &body)?;
        }
        after(ReceiptPublishPhase::Delta)?;

        let body = self.body_for(&record);
        let id = Self::record_id(&body)?;
        let stored = StoredRecord { id, body };
        let name = hex(id.as_bytes());
        publish_json_immutable_at(&self.records, &name, &stored)?;
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
        atomic_replace_at(
            &self.heads,
            &format!("{}.head", hex(&prepared.source_intent)),
            &prepared.id.to_bytes(),
        )
    }

    #[cfg(test)]
    fn head_path(&self, source_intent: [u8; ID_BYTES]) -> PathBuf {
        self.root
            .join("heads")
            .join(format!("{}.head", hex(&source_intent)))
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
        let head = format!("{}.head", hex(&source_intent));
        let mut file = match self.heads.open_file_read(&head) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = [0_u8; ID_BYTES + 1];
        let mut read = 0;
        while read < bytes.len() {
            let count = file.read(&mut bytes[read..])?;
            if count == 0 {
                break;
            }
            read += count;
        }
        if read != ID_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid acquisition head",
            ));
        }
        let mut encoded_id = [0; ID_BYTES];
        encoded_id.copy_from_slice(&bytes[..ID_BYTES]);
        let id = AcquisitionRecordId::from_encoded(encoded_id);
        let record_name = hex(id.as_bytes());
        let stored = read_bounded_json_at::<StoredRecord>(&self.records, &record_name)?;
        if stored.id != id || Self::record_id(&stored.body)? != id {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "acquisition record digest mismatch",
            ));
        }
        Ok(Some((id, stored.body)))
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
            receipt: record.receipt.clone(),
        }
    }

    fn record_id(body: &StoredRecordBody) -> io::Result<AcquisitionRecordId> {
        let mut counter = CountingWriter {
            written: 0,
            limit: MAX_PRODUCT_FILE_BYTES,
        };
        serde_json::to_writer(&mut counter, body).map_err(json_error)?;
        AcquisitionRecordId::derive_with_streamed_field(
            &[b"backend.acquisition.product-record.v1\0"],
            counter.written,
            |writer| serde_json::to_writer(writer, body).map_err(json_error),
        )
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
                let name = hex(snapshot.as_bytes());
                let snapshot_value =
                    read_bounded_json_at::<SourceSnapshot>(&self.snapshots, &name)?;
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
                let name = hex(delta_id.as_bytes());
                let stored = read_bounded_json_at::<StoredDelta>(&self.deltas, &name)?;
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
            receipt: body.receipt,
        };
        self.validate_record(&record)?;
        Ok(record)
    }

    fn publish_snapshot(&self, snapshot: &SourceSnapshot) -> io::Result<()> {
        snapshot.validate().map_err(invalid_data)?;
        let name = hex(snapshot.id().as_bytes());
        publish_json_immutable_at(&self.snapshots, &name, snapshot)
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

fn read_bounded_json_at<T: for<'de> Deserialize<'de>>(
    directory: &DirectoryCapability,
    name: &str,
) -> io::Result<T> {
    read_bounded_json_at_with_limit(directory, name, MAX_PRODUCT_FILE_BYTES)
}

fn read_bounded_json_at_with_limit<T: for<'de> Deserialize<'de>>(
    directory: &DirectoryCapability,
    name: &str,
    limit: u64,
) -> io::Result<T> {
    let mut file = directory.open_file_read(name)?;
    let initial_length = file.metadata()?.len();
    if initial_length > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "acquisition product record exceeds bound",
        ));
    }
    let value = serde_json::from_reader(Read::take(&mut file, limit.saturating_add(1)))
        .map_err(invalid_data)?;
    if file.metadata()?.len() != initial_length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "acquisition product record changed while reading",
        ));
    }
    Ok(value)
}

fn publish_json_immutable_at<T: Serialize>(
    directory: &DirectoryCapability,
    name: &str,
    value: &T,
) -> io::Result<()> {
    let temporary = format!(
        ".{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let file = directory.create_file_exclusive(&temporary)?;
    let mut writer = BoundedJsonWriter {
        file,
        written: 0,
        limit: MAX_PRODUCT_FILE_BYTES,
    };
    let serialized = (|| {
        serde_json::to_writer(&mut writer, value).map_err(json_error)?;
        writer.file.sync_all()?;
        Ok(())
    })();
    drop(writer);
    if let Err(error) = serialized {
        let _ = directory.remove_file(&temporary);
        return Err(error);
    }
    match directory.rename_with_outcome(&temporary, name, false) {
        Ok(()) => Ok(()),
        Err(DirectoryRenameError::NotCommitted(error))
            if error.kind() == io::ErrorKind::AlreadyExists =>
        {
            let compare = compare_capability_files(directory, &temporary, name);
            let _ = directory.remove_file(&temporary);
            compare?;
            directory.sync_all()
        }
        Err(DirectoryRenameError::NotCommitted(error)) => {
            let _ = directory.remove_file(&temporary);
            Err(error)
        }
        Err(error @ DirectoryRenameError::CommittedButNotDurable(_)) => Err(error.into_io_error()),
    }
}

fn compare_capability_files(
    directory: &DirectoryCapability,
    first_name: &str,
    second_name: &str,
) -> io::Result<()> {
    let mut first = directory.open_file_read(first_name)?;
    let mut second = directory.open_file_read(second_name)?;
    let first_length = first.metadata()?.len();
    let second_length = second.metadata()?.len();
    if first_length != second_length || first_length > MAX_PRODUCT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed acquisition record collision",
        ));
    }
    let mut first_bytes = [0_u8; 64 * 1024];
    let mut second_bytes = [0_u8; 64 * 1024];
    loop {
        let first_read = first.read(&mut first_bytes)?;
        let second_read = second.read(&mut second_bytes)?;
        if first_read != second_read || first_bytes[..first_read] != second_bytes[..second_read] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "content-addressed acquisition record collision",
            ));
        }
        if first_read == 0 {
            return Ok(());
        }
    }
}

fn atomic_replace_at(directory: &DirectoryCapability, name: &str, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() as u64 > MAX_PRODUCT_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "acquisition product record exceeds bound",
        ));
    }
    let temporary = format!(
        ".{}.{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let staged = (|| {
        let mut file = directory.create_file_exclusive(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = staged {
        let _ = directory.remove_file(&temporary);
        return Err(error);
    }
    match directory.rename_with_outcome(&temporary, name, true) {
        Ok(()) => Ok(()),
        Err(DirectoryRenameError::NotCommitted(error)) => {
            let _ = directory.remove_file(&temporary);
            Err(error)
        }
        Err(error @ DirectoryRenameError::CommittedButNotDurable(_)) => Err(error.into_io_error()),
    }
}

#[cfg(test)]
fn read_bounded_json_with_limit<T: for<'de> Deserialize<'de>>(
    root: &Path,
    path: &Path,
    limit: u64,
) -> io::Result<T> {
    validate_store_directory(root, path.parent().expect("product file parent"))?;
    let mut file = backend_platform::durability::open_regular_file_nofollow(path)?;
    let metadata = file.metadata()?;
    if metadata.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "acquisition product record exceeds bound",
        ));
    }
    let value = serde_json::from_reader(Read::take(&mut file, limit.saturating_add(1)))
        .map_err(invalid_data)?;
    if file.metadata()?.len() != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "acquisition product record changed while reading",
        ));
    }
    Ok(value)
}

#[cfg(test)]
fn write_json_temp_with_limit<T: Serialize>(
    directory: &Path,
    value: &T,
    limit: u64,
) -> io::Result<(PathBuf, u64)> {
    validate_directory_chain(directory)?;
    let id = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = directory.join(format!("{}.{}.tmp", std::process::id(), id));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let mut writer = BoundedJsonWriter {
        file,
        written: 0,
        limit,
    };
    let result = (|| {
        serde_json::to_writer(&mut writer, value).map_err(json_error)?;
        writer.file.sync_all()?;
        Ok(writer.written)
    })();
    match result {
        Ok(written) => Ok((path, written)),
        Err(error) => {
            drop(writer);
            let _ = fs::remove_file(path);
            Err(error)
        }
    }
}

struct BoundedJsonWriter {
    file: File,
    written: u64,
    limit: u64,
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.limit.saturating_sub(self.written)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "acquisition product record exceeds bound",
            ));
        }
        let written = self.file.write(bytes)?;
        self.written = self
            .written
            .checked_add(u64::try_from(written).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "serialized length overflow")
            })?)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "serialized length overflow")
            })?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

struct CountingWriter {
    written: u64,
    limit: u64,
}

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "serialized length overflow")
        })?;
        if length > self.limit.saturating_sub(self.written) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "acquisition product record exceeds bound",
            ));
        }
        self.written += length;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn json_error(error: serde_json::Error) -> io::Error {
    let kind = error.io_error_kind().unwrap_or(io::ErrorKind::InvalidData);
    io::Error::new(kind, error.to_string())
}

#[cfg(test)]
fn validate_store_directory(root: &Path, directory: &Path) -> io::Result<()> {
    let relative = directory.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "acquisition receipt path escaped its root",
        )
    })?;
    validate_directory_chain(root)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid acquisition receipt directory component",
            ));
        };
        current.push(component);
        backend_platform::durability::open_directory_readonly_nofollow(&current)?;
    }
    Ok(())
}

#[cfg(test)]
fn validate_directory_chain(directory: &Path) -> io::Result<()> {
    // Reject links in every component that exists when checked. These checks
    // do not pin ancestors across a later path operation; fully race-free
    // traversal needs the platform's handle-relative directory API.
    if directory.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "acquisition receipt directory is empty",
        ));
    }
    validate_existing_directory_prefix(directory)?;
    // Only the directory whose contents we own needs a readable handle.
    // Known-path traversal through an ancestor needs search permission, not
    // enumeration permission. Opening every ancestor rejects valid scoped
    // access, including macOS grants to a folder inside protected Documents.
    backend_platform::durability::open_directory_readonly_nofollow(directory).map(|_| ())
}

#[cfg(test)]
fn validate_existing_directory_prefix(directory: &Path) -> io::Result<()> {
    let absolute = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir()?.join(directory)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                current.push(component.as_os_str());
            }
            std::path::Component::Normal(component) => {
                current.push(component);
                match fs::symlink_metadata(&current) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "acquisition receipt path contains a non-directory or link",
                        ));
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(error) => return Err(error),
                }
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "acquisition receipt path contains a parent component",
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
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
    validate_directory_chain(temp_root)?;
    validate_directory_chain(path.parent().expect("product parent"))?;
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

#[cfg(test)]
fn write_temp(directory: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    validate_directory_chain(directory)?;
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

#[cfg(test)]
fn compare_existing(path: &Path, expected: &[u8]) -> io::Result<()> {
    let mut existing = backend_platform::durability::open_regular_file_nofollow(path)?;
    if existing.metadata()?.len() != expected.len() as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "content-addressed acquisition record collision",
        ));
    }
    let mut expected_offset = 0;
    let mut buffer = [0_u8; 64 * 1024];
    while expected_offset < expected.len() {
        let amount = buffer.len().min(expected.len() - expected_offset);
        existing.read_exact(&mut buffer[..amount])?;
        if buffer[..amount] != expected[expected_offset..expected_offset + amount] {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "content-addressed acquisition record collision",
            ));
        }
        expected_offset += amount;
    }
    Ok(())
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
        AcquisitionOutcome, AcquisitionRequest, DeltaChange, FactFreshness, LeaseStore,
        ManifestEntry, MetadataRecord, Policy, ReleaseClaim, Resolve, TreeManifest,
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
        fs::canonicalize(path).expect("canonicalize fixture")
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

    #[test]
    fn json_serialization_stops_at_the_output_budget_without_a_large_buffer() {
        struct ManyStrings(usize);

        impl Serialize for ManyStrings {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                use serde::ser::SerializeSeq as _;
                let mut sequence = serializer.serialize_seq(Some(self.0))?;
                for _ in 0..self.0 {
                    sequence.serialize_element("a moderately sized value")?;
                }
                sequence.end()
            }
        }

        let root = temporary("bounded-json");
        let temps = root.join("temps");
        fs::create_dir_all(&temps).expect("temporary directory");
        let error = write_json_temp_with_limit(&temps, &ManyStrings(1_000_000), 96)
            .expect_err("serialization must stop as soon as its budget is exceeded");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            fs::read_dir(&temps).expect("read temp directory").count(),
            0
        );

        let within_limit = write_json_temp_with_limit(&temps, &ManyStrings(2), 96)
            .expect("small record fits output budget");
        assert!(within_limit.1 <= 96);
        fs::remove_file(within_limit.0).expect("remove staged JSON");
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn streamed_record_identity_matches_the_existing_canonical_identity() {
        let root = temporary("streamed-record-id");
        let (record, _) = published_fixture();
        let store = AcquisitionReceiptStore::open(root.join("receipts")).expect("store");
        let body = store.body_for(&record);
        let bytes = serde_json::to_vec(&body).expect("small fixture body");
        let expected =
            AcquisitionRecordId::derive(&[b"backend.acquisition.product-record.v1\0", &bytes]);
        assert_eq!(
            AcquisitionReceiptStore::record_id(&body).expect("stream identity"),
            expected
        );
        let delta = record.delta.as_deref().expect("fixture delta");
        let borrowed = BorrowedStoredDelta {
            version: FORMAT_VERSION,
            base: delta.base(),
            target: delta.target(),
            changes: delta.changes(),
        };
        let owned = StoredDelta {
            version: FORMAT_VERSION,
            base: delta.base(),
            target: delta.target(),
            changes: delta.changes().to_vec(),
        };
        assert_eq!(
            serde_json::to_vec(&borrowed).expect("borrowed delta JSON"),
            serde_json::to_vec(&owned).expect("owned delta JSON")
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn streamed_identity_writer_rejects_a_wrong_declared_length() {
        let too_long = AcquisitionRecordId::derive_with_streamed_field(&[], 1, |writer| {
            writer.write_all(b"two bytes")
        })
        .expect_err("stream cannot exceed the declared field length");
        assert_eq!(too_long.kind(), io::ErrorKind::InvalidData);

        let too_short = AcquisitionRecordId::derive_with_streamed_field(&[], 2, |_| Ok(()))
            .expect_err("stream must reach the declared field length");
        assert_eq!(too_short.kind(), io::ErrorKind::InvalidData);
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
            .acquire(request.receipt_work_key(), Duration::from_secs(30))
            .expect("acquire a valid lease")
            .expect("first lease");
        stale.expire_for_test().expect("persist the expired lease");
        let mut current = locks
            .acquire(request.receipt_work_key(), Duration::from_secs(30))
            .expect("take over expired lease")
            .expect("current lease");
        assert_ne!(stale.lease().token, current.lease().token);
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
    fn oversized_head_is_rejected_after_a_fixed_width_read() {
        let root = temporary("oversized-head");
        let (mut record, request) = published_fixture();
        let store = AcquisitionReceiptStore::open(root.join("receipts")).expect("store");
        let (_locks, mut lease) = lease(&root, &request);
        record.id = store
            .publish(record.clone(), &mut lease)
            .expect("publish")
            .expect("current lease");
        drop(lease);
        assert_eq!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("admit the valid receipt before tampering"),
            Some(record.clone())
        );

        let head = store.head_path(request.source_intent());
        fs::write(&head, vec![0_u8; ID_BYTES + 1]).expect("write oversized head");
        assert!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .is_err()
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn oversized_record_is_rejected_before_json_decode() {
        let root = temporary("oversized-record");
        let receipts = root.join("receipts");
        let store = AcquisitionReceiptStore::open(&receipts).expect("store");
        let record = store.root.join("records").join("oversized");
        fs::write(&record, b"{}").expect("small placeholder");
        OpenOptions::new()
            .write(true)
            .open(&record)
            .expect("open placeholder")
            .set_len(97)
            .expect("extend sparse placeholder");

        let error = read_bounded_json_with_limit::<serde_json::Value>(&store.root, &record, 96)
            .expect_err("metadata length exceeds the small test bound");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn receipt_reads_stay_on_opened_directories_when_path_is_replaced() {
        use std::os::unix::fs::symlink;

        let root = temporary("symlinked-head");
        let (mut record, request) = published_fixture();
        let store = AcquisitionReceiptStore::open(root.join("receipts")).expect("store");
        let (_locks, mut lease) = lease(&root, &request);
        record.id = store
            .publish(record.clone(), &mut lease)
            .expect("publish")
            .expect("current lease");
        drop(lease);
        assert_eq!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("admit the valid receipt before tampering"),
            Some(record.clone())
        );

        let head = store.head_path(request.source_intent());
        let external_head = root.join("external-head");
        fs::write(&external_head, record.id.to_bytes()).expect("external head bytes");
        fs::remove_file(&head).expect("remove managed head");
        symlink(&external_head, &head).expect("link head to external file");
        assert!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .is_err(),
            "leaf symlinks are refused even beneath a pinned directory"
        );

        fs::remove_file(&head).expect("remove symlink head");
        fs::write(&head, record.id.to_bytes()).expect("restore regular head");
        assert_eq!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("restoring the regular head restores admission"),
            Some(record.clone())
        );
        let record_path = store.root.join("records").join(hex(record.id.as_bytes()));
        let external_record = root.join("external-record");
        fs::copy(&record_path, &external_record).expect("copy record outside store");
        fs::remove_file(&record_path).expect("remove managed record");
        symlink(&external_record, &record_path).expect("link record to external file");
        assert!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .is_err(),
            "leaf symlinks are refused for record reads"
        );

        fs::remove_file(&record_path).expect("remove symlink record");
        fs::rename(&external_record, &record_path).expect("restore the valid managed record");
        assert_eq!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("restoring the regular record restores admission"),
            Some(record.clone())
        );
        let heads = store.root.join("heads");
        let moved_heads = store.root.join("heads-real");
        fs::rename(&heads, &moved_heads).expect("move managed heads directory");
        let external_heads = root.join("external-heads");
        fs::create_dir_all(&external_heads).expect("external heads directory");
        fs::write(
            external_heads.join(format!("{}.head", hex(&request.source_intent()))),
            record.id.to_bytes(),
        )
        .expect("external managed-looking head");
        symlink(&external_heads, &heads).expect("link heads directory externally");
        assert_eq!(
            store
                .recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("the opened heads directory remains the authority"),
            Some(record),
            "replacement parent symlinks cannot redirect a pinned receipt store"
        );
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn receipt_store_survives_restart_beneath_a_search_only_ancestor() {
        use std::os::unix::fs::PermissionsExt as _;

        if rustix::process::geteuid().is_root() {
            // Root bypasses the permission boundary this regression exercises.
            return;
        }
        struct RestorePermissions {
            path: PathBuf,
            permissions: fs::Permissions,
        }
        impl Drop for RestorePermissions {
            fn drop(&mut self) {
                let _ = fs::set_permissions(&self.path, self.permissions.clone());
            }
        }

        let root = temporary("search-only-ancestor");
        let workspace = root.join("accessible-workspace");
        fs::create_dir(&workspace).expect("create the granted workspace");
        let restore = RestorePermissions {
            path: root.clone(),
            permissions: fs::metadata(&root)
                .expect("ancestor metadata")
                .permissions(),
        };
        fs::set_permissions(&root, fs::Permissions::from_mode(0o300))
            .expect("allow known-path traversal without enumeration");
        assert_eq!(
            backend_platform::durability::open_directory_readonly_nofollow(&root)
                .expect_err("the ancestor cannot be opened for enumeration")
                .kind(),
            io::ErrorKind::PermissionDenied
        );

        let (mut record, request) = published_fixture();
        let receipts = workspace.join("receipts");
        let store = AcquisitionReceiptStore::open(&receipts).expect("open beneath scoped access");
        let (locks, mut lease) = lease(&workspace, &request);
        let published = store
            .publish(record.clone(), &mut lease)
            .expect("publish")
            .expect("current lease publishes a receipt");
        assert_ne!(
            published, record.id,
            "publication gives the placeholder its content identity"
        );
        record.id = published;
        drop(lease);
        drop(locks);
        drop(store);

        let cold =
            AcquisitionReceiptStore::open(&receipts).expect("cold reopen beneath scoped access");
        assert_eq!(
            cold.recover(&request, record.owner_cursor, record.facts_frontier, 7)
                .expect("read the exact durable receipt"),
            Some(record)
        );
        drop(cold);
        drop(restore);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[cfg(unix)]
    #[test]
    fn receipt_store_open_rejects_a_symlinked_root_ancestor() {
        use std::os::unix::fs::symlink;

        let root = temporary("symlinked-root-ancestor");
        let target = root.join("target");
        let link = root.join("link");
        fs::create_dir_all(&target).expect("target directory");
        symlink(&target, &link).expect("link root ancestor");

        assert!(AcquisitionReceiptStore::open(link.join("receipts")).is_err());
        assert!(!target.join("receipts").exists());
        fs::remove_dir_all(root).expect("cleanup");
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

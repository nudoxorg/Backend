//! Bounded durable receipts for caller-keyed index operations.

use backend_library::{
    CompileExecutionIntent, IndexOperationFailureReason, IndexOperationKey,
    IndexOperationObservation, IndexOperationPublicationReceipt, IndexOperationState,
    IndexOperationStatus, IndexOperationUnresolvedReason, PackageReference, ProductText,
    SurfaceReply, index_operation_request_digest,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

const FILE_VERSION: u8 = 1;
// First canary bound: entries are never evicted, so replayed keys cannot be
// forgotten. When either limit is reached, new starts are refused before
// acceptance. This intentionally fail-closed limit must only be replaced by a
// compact durable tombstone/history store that preserves exact receipts.
const MAX_OPERATIONS: usize = 512;
const MAX_FILE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalFile {
    version: u8,
    entries: Vec<StoredOperation>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredOperation {
    pub(super) operation_key: IndexOperationKey,
    pub(super) request_digest: [u8; 32],
    pub(super) package: PackageReference,
    pub(super) execution_intent: CompileExecutionIntent,
    pub(super) state: StoredOperationState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", content = "detail", rename_all = "kebab-case")]
pub(super) enum StoredOperationState {
    /// Acceptance was flushed before any indexing work started.
    Accepted,
    /// The exact mutation identity was flushed before crossing the workspace
    /// commit boundary. `None` records a completed no-op scan.
    Prepared {
        request_identity: Option<[u8; 32]>,
        base_workspace_root: [u8; 32],
        base_workspace_sequence: u64,
    },
    /// The selected workspace commit and its exact published view are durable.
    Published {
        receipt: IndexOperationPublicationReceipt,
        request_identity: Option<[u8; 32]>,
        base_workspace_root: [u8; 32],
        base_workspace_sequence: u64,
    },
    /// Work ended before an intent was committed.
    Failed {
        reason: IndexOperationFailureReason,
        detail: ProductText,
    },
    /// The selected owner state did not establish a terminal result.
    Unresolved {
        reason: IndexOperationUnresolvedReason,
        detail: ProductText,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Acceptance {
    New,
    Existing,
}

#[derive(Debug)]
pub(super) enum JournalError {
    Io(String),
    Corrupt(String),
    Full,
    KeyConflict,
    Missing,
    InvalidTransition,
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "index-operation journal I/O: {error}"),
            Self::Corrupt(error) => {
                write!(formatter, "index-operation journal is corrupt: {error}")
            }
            Self::Full => formatter.write_str("index-operation receipt capacity is full"),
            Self::KeyConflict => {
                formatter.write_str("index-operation key was reused with another request")
            }
            Self::Missing => formatter.write_str("index-operation key is not admitted"),
            Self::InvalidTransition => {
                formatter.write_str("invalid index-operation journal transition")
            }
        }
    }
}

pub(super) struct IndexOperationJournal {
    path: PathBuf,
    entries: BTreeMap<IndexOperationKey, StoredOperation>,
}

impl IndexOperationJournal {
    pub(super) fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref().to_owned();
        let parent = path
            .parent()
            .ok_or_else(|| JournalError::Io("journal path has no parent".to_owned()))?;
        backend_platform::durable::ensure_private_directory(parent)
            .map_err(|error| JournalError::Io(error.to_string()))?;
        let bytes = match read_private_bounded(&path, MAX_FILE_BYTES) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(JournalError::Io(error.to_string())),
        };
        let mut entries = BTreeMap::new();
        if let Some(bytes) = bytes {
            let file: JournalFile = serde_json::from_slice(&bytes)
                .map_err(|error| JournalError::Corrupt(error.to_string()))?;
            if file.version != FILE_VERSION || file.entries.len() > MAX_OPERATIONS {
                return Err(JournalError::Corrupt(
                    "version or operation count is outside the bound".to_owned(),
                ));
            }
            for entry in file.entries {
                validate_entry(&entry)?;
                let key = entry.operation_key;
                if entries.insert(key, entry).is_some() {
                    return Err(JournalError::Corrupt(
                        "operation key appears more than once".to_owned(),
                    ));
                }
            }
        }
        Ok(Self { path, entries })
    }

    pub(super) fn accept(
        &mut self,
        operation_key: IndexOperationKey,
        package: PackageReference,
        execution_intent: CompileExecutionIntent,
    ) -> Result<Acceptance, JournalError> {
        let request_digest = index_operation_request_digest(&package, execution_intent);
        if let Some(existing) = self.entries.get(&operation_key) {
            return if existing.request_digest == request_digest
                && existing.package == package
                && existing.execution_intent == execution_intent
            {
                Ok(Acceptance::Existing)
            } else {
                Err(JournalError::KeyConflict)
            };
        }
        if self.entries.len() >= MAX_OPERATIONS {
            return Err(JournalError::Full);
        }
        let entry = StoredOperation {
            operation_key,
            request_digest,
            package,
            execution_intent,
            state: StoredOperationState::Accepted,
        };
        self.entries.insert(operation_key, entry);
        if let Err(error) = self.persist() {
            self.entries.remove(&operation_key);
            return Err(error);
        }
        Ok(Acceptance::New)
    }

    pub(super) fn prepare(
        &mut self,
        operation_key: IndexOperationKey,
        request_identity: Option<[u8; 32]>,
        base_workspace_root: [u8; 32],
        base_workspace_sequence: u64,
    ) -> Result<(), JournalError> {
        if request_identity.is_some_and(|identity| identity.iter().all(|byte| *byte == 0))
            || base_workspace_root.iter().all(|byte| *byte == 0)
        {
            return Err(JournalError::InvalidTransition);
        }
        let entry = self
            .entries
            .get_mut(&operation_key)
            .ok_or(JournalError::Missing)?;
        if !matches!(entry.state, StoredOperationState::Accepted) {
            return Err(JournalError::InvalidTransition);
        }
        let previous = std::mem::replace(
            &mut entry.state,
            StoredOperationState::Prepared {
                request_identity,
                base_workspace_root,
                base_workspace_sequence,
            },
        );
        if let Err(error) = self.persist() {
            self.entries
                .get_mut(&operation_key)
                .expect("entry remains present")
                .state = previous;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn published(
        &mut self,
        operation_key: IndexOperationKey,
        receipt: IndexOperationPublicationReceipt,
    ) -> Result<(), JournalError> {
        let entry = self
            .entries
            .get_mut(&operation_key)
            .ok_or(JournalError::Missing)?;
        let prepared = match &entry.state {
            StoredOperationState::Prepared {
                request_identity,
                base_workspace_root,
                base_workspace_sequence,
            } => Some((
                *request_identity,
                *base_workspace_root,
                *base_workspace_sequence,
            )),
            _ => None,
        };
        let Some((request_identity, base_workspace_root, base_workspace_sequence)) = prepared
        else {
            return Err(JournalError::InvalidTransition);
        };
        if !published_receipt_matches(
            &receipt,
            request_identity,
            base_workspace_root,
            base_workspace_sequence,
        ) {
            return Err(JournalError::InvalidTransition);
        }
        let previous = std::mem::replace(
            &mut entry.state,
            StoredOperationState::Published {
                receipt,
                request_identity,
                base_workspace_root,
                base_workspace_sequence,
            },
        );
        if let Err(error) = self.persist() {
            self.entries
                .get_mut(&operation_key)
                .expect("entry remains present")
                .state = previous;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn failed(
        &mut self,
        operation_key: IndexOperationKey,
        reason: IndexOperationFailureReason,
        detail: ProductText,
    ) -> Result<(), JournalError> {
        let entry = self
            .entries
            .get_mut(&operation_key)
            .ok_or(JournalError::Missing)?;
        if matches!(entry.state, StoredOperationState::Published { .. }) {
            return Err(JournalError::InvalidTransition);
        }
        let previous = std::mem::replace(
            &mut entry.state,
            StoredOperationState::Failed { reason, detail },
        );
        if let Err(error) = self.persist() {
            self.entries
                .get_mut(&operation_key)
                .expect("entry remains present")
                .state = previous;
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn entry(&self, operation_key: IndexOperationKey) -> Option<&StoredOperation> {
        self.entries.get(&operation_key)
    }

    pub(super) fn first_prepared_key(&self) -> Option<IndexOperationKey> {
        self.entries.iter().find_map(|(key, entry)| {
            matches!(entry.state, StoredOperationState::Prepared { .. }).then_some(*key)
        })
    }

    pub(super) fn has_prepared(&self) -> bool {
        self.first_prepared_key().is_some()
    }

    pub(super) fn observation(
        &self,
        operation_key: IndexOperationKey,
        active: Option<(
            backend_library::IndexJobTicket,
            backend_library::IndexJobStage,
        )>,
    ) -> Option<IndexOperationObservation> {
        let entry = self.entries.get(&operation_key)?;
        let state = match &entry.state {
            StoredOperationState::Accepted => match active {
                Some((ticket, stage)) => IndexOperationState::Active { ticket, stage },
                None => IndexOperationState::Accepted,
            },
            StoredOperationState::Prepared { .. } => return None,
            StoredOperationState::Published { receipt, .. } => {
                IndexOperationState::Published(receipt.clone())
            }
            StoredOperationState::Failed { reason, detail } => IndexOperationState::Failed {
                reason: *reason,
                detail: detail.clone(),
            },
            StoredOperationState::Unresolved { reason, detail } => {
                IndexOperationState::Unresolved {
                    reason: *reason,
                    detail: detail.clone(),
                }
            }
        };
        Some(IndexOperationObservation::Known(IndexOperationStatus::new(
            entry.operation_key,
            entry.package.clone(),
            entry.execution_intent,
            state,
        )))
    }

    fn persist(&self) -> Result<(), JournalError> {
        let file = JournalFile {
            version: FILE_VERSION,
            entries: self.entries.values().cloned().collect(),
        };
        let bytes =
            serde_json::to_vec(&file).map_err(|error| JournalError::Io(error.to_string()))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(JournalError::Full);
        }
        backend_platform::durable::write_private_atomic(&self.path, &bytes)
            .map_err(|error| JournalError::Io(error.to_string()))
    }
}

fn read_private_bounded(path: &Path, maximum: usize) -> std::io::Result<Vec<u8>> {
    let file = backend_platform::durable::open_private_read(path)?;
    let length = file.metadata()?.len();
    let limit = maximum
        .checked_add(1)
        .and_then(|limit| u64::try_from(limit).ok())
        .ok_or_else(|| {
            std::io::Error::new(
                ErrorKind::InvalidInput,
                "journal size limit is not representable",
            )
        })?;
    if length >= limit {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "index-operation journal exceeds its byte limit",
        ));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length as usize)
        .map_err(|error| std::io::Error::new(ErrorKind::OutOfMemory, error))?;
    file.take(limit).read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "index-operation journal grew beyond its byte limit",
        ));
    }
    Ok(bytes)
}

fn validate_entry(entry: &StoredOperation) -> Result<(), JournalError> {
    if entry.request_digest
        != index_operation_request_digest(&entry.package, entry.execution_intent)
    {
        return Err(JournalError::Corrupt(
            "stored request digest does not match the canonical request".to_owned(),
        ));
    }
    if let StoredOperationState::Prepared {
        request_identity: Some(identity),
        base_workspace_root,
        ..
    } = &entry.state
        && (identity.iter().all(|byte| *byte == 0)
            || base_workspace_root.iter().all(|byte| *byte == 0))
    {
        return Err(JournalError::Corrupt(
            "prepared commit identity is reserved".to_owned(),
        ));
    }
    if let StoredOperationState::Prepared {
        request_identity: None,
        base_workspace_root,
        ..
    } = &entry.state
        && base_workspace_root.iter().all(|byte| *byte == 0)
    {
        return Err(JournalError::Corrupt(
            "prepared no-op workspace root is reserved".to_owned(),
        ));
    }
    if let StoredOperationState::Published {
        receipt,
        request_identity,
        base_workspace_root,
        base_workspace_sequence,
    } = &entry.state
    {
        if !published_receipt_matches(
            receipt,
            *request_identity,
            *base_workspace_root,
            *base_workspace_sequence,
        ) {
            return Err(JournalError::Corrupt(
                "published receipt does not match its prepared workspace request".to_owned(),
            ));
        }
        let status = IndexOperationStatus::new(
            entry.operation_key,
            entry.package.clone(),
            entry.execution_intent,
            IndexOperationState::Published(receipt.clone()),
        );
        let reply = SurfaceReply::IndexOperationStatus(IndexOperationObservation::Known(status));
        reply
            .admit(backend_library::CommandId::IndexProgress)
            .map_err(|error| JournalError::Corrupt(error.to_string()))?;
    }
    Ok(())
}

fn published_receipt_matches(
    receipt: &IndexOperationPublicationReceipt,
    request_identity: Option<[u8; 32]>,
    base_workspace_root: [u8; 32],
    base_workspace_sequence: u64,
) -> bool {
    match request_identity {
        Some(expected) => {
            receipt.request_identity() == Some(&expected)
                && receipt.workspace_sequence() > base_workspace_sequence
        }
        None => {
            receipt.request_identity().is_none()
                && receipt.workspace_root() == &base_workspace_root
                && receipt.workspace_sequence() == base_workspace_sequence
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{Cursor, ViewRoot};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn path() -> PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let scratch = std::env::temp_dir().join(format!(
            "backend-index-operation-journal-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir(&scratch).expect("create private journal test scratch");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700))
                .expect("protect journal test scratch");
        }
        let root = scratch.join("workspace");
        fs::create_dir(&root).expect("create private journal test workspace");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
                .expect("protect journal test workspace");
        }
        root.join("operations.json")
    }

    fn package() -> PackageReference {
        PackageReference::parse("/workspace/receipt-fixture").expect("package")
    }

    fn key(value: u8) -> IndexOperationKey {
        IndexOperationKey::from_bytes([value; 32]).expect("key")
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            if let Some(root) = parent.parent() {
                let _ = fs::remove_dir_all(root);
            }
        }
    }

    fn view() -> ViewRoot {
        let root = backend_library::view_state_root(&[]);
        let basis =
            backend_library::Basis::new(root, backend_library::object_version(b"receipt-test"));
        let frontier = backend_library::canonical::Frontier::new(
            backend_library::branch_key("main"),
            backend_library::log_key("library"),
            backend_library::cursor::CURSOR_SCHEMA,
            root,
            0,
        );
        ViewRoot::new_incomplete(
            backend_library::view_key(b"receipt-test"),
            basis,
            frontier,
            Vec::new(),
            Vec::new(),
        )
        .expect("view")
    }

    fn receipt(
        request_identity: Option<[u8; 32]>,
        sequence: u64,
    ) -> IndexOperationPublicationReceipt {
        let view = view();
        IndexOperationPublicationReceipt::from_published_view(
            request_identity,
            [8; 32],
            [9; 32],
            sequence,
            &view,
            Cursor::for_view_root(&view),
        )
        .expect("receipt")
    }

    #[test]
    fn accepted_key_replays_exactly_and_conflicting_payload_is_rejected_after_restart() {
        let path = path();
        {
            let mut journal = IndexOperationJournal::open(&path).expect("open journal");
            assert_eq!(
                journal
                    .accept(key(1), package(), CompileExecutionIntent::Interactive)
                    .expect("accept"),
                Acceptance::New
            );
        }
        let mut journal = IndexOperationJournal::open(&path).expect("reopen journal");
        assert_eq!(
            journal
                .accept(key(1), package(), CompileExecutionIntent::Interactive)
                .expect("exact replay"),
            Acceptance::Existing
        );
        assert!(matches!(
            journal.accept(key(1), package(), CompileExecutionIntent::Background),
            Err(JournalError::KeyConflict)
        ));
        assert!(matches!(
            journal.entry(key(1)).map(|entry| &entry.state),
            Some(StoredOperationState::Accepted)
        ));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn prepared_intent_survives_interruption_and_is_not_reaccepted_as_new_work() {
        let path = path();
        {
            let mut journal = IndexOperationJournal::open(&path).expect("open journal");
            journal
                .accept(key(2), package(), CompileExecutionIntent::Interactive)
                .expect("accept");
            journal
                .prepare(key(2), Some([7; 32]), [6; 32], 0)
                .expect("precommit flush");
        }
        let mut journal = IndexOperationJournal::open(&path).expect("reopen after interruption");
        assert_eq!(
            journal
                .accept(key(2), package(), CompileExecutionIntent::Interactive)
                .expect("replay does not launch"),
            Acceptance::Existing
        );
        assert!(matches!(
            journal.entry(key(2)).map(|entry| &entry.state),
            Some(StoredOperationState::Prepared {
                request_identity: Some([7; 32]),
                base_workspace_root: [6; 32],
                base_workspace_sequence: 0,
            })
        ));
        journal
            .failed(
                key(2),
                IndexOperationFailureReason::WorkerFailed,
                ProductText::from_static("owner restarted before the exact commit was selected"),
            )
            .expect("record proven precommit failure");
        drop(journal);
        let journal = IndexOperationJournal::open(&path).expect("reopen failure receipt");
        assert!(matches!(
            journal.observation(key(2), None),
            Some(IndexOperationObservation::Known(IndexOperationStatus {
                state: IndexOperationState::Failed {
                    reason: IndexOperationFailureReason::WorkerFailed,
                    ..
                },
                ..
            }))
        ));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn published_view_receipt_replays_after_terminal_write_and_restart() {
        let path = path();
        let expected = receipt(Some([7; 32]), 1);
        {
            let mut journal = IndexOperationJournal::open(&path).expect("open journal");
            journal
                .accept(key(3), package(), CompileExecutionIntent::Interactive)
                .expect("accept");
            journal
                .prepare(key(3), Some([7; 32]), [6; 32], 0)
                .expect("precommit flush");
            journal
                .published(key(3), expected.clone())
                .expect("durable terminal receipt");
        }
        let journal = IndexOperationJournal::open(&path).expect("reopen terminal receipt");
        let Some(IndexOperationObservation::Known(status)) = journal.observation(key(3), None)
        else {
            panic!("operation status should be retained");
        };
        let IndexOperationState::Published(observed) = status.state else {
            panic!("published receipt should survive restart");
        };
        assert_eq!(observed, expected);
        assert_eq!(observed.request_identity(), Some(&[7; 32]));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn no_op_receipt_binds_the_existing_head_without_claiming_a_new_intent() {
        let path = path();
        let expected = receipt(None, 4);
        let mut journal = IndexOperationJournal::open(&path).expect("open journal");
        journal
            .accept(key(4), package(), CompileExecutionIntent::Interactive)
            .expect("accept");
        journal
            .prepare(key(4), None, [9; 32], 4)
            .expect("prepare no-op");
        journal
            .published(key(4), expected.clone())
            .expect("publish no-op view");
        assert_eq!(
            journal
                .observation(key(4), None)
                .expect("status")
                .known_receipt_for_test(),
            Some(expected)
        );
        drop(journal);
        cleanup(&path);
    }
}

#[cfg(test)]
trait TestKnownReceipt {
    fn known_receipt_for_test(&self) -> Option<IndexOperationPublicationReceipt>;
}

#[cfg(test)]
impl TestKnownReceipt for IndexOperationObservation {
    fn known_receipt_for_test(&self) -> Option<IndexOperationPublicationReceipt> {
        match self {
            Self::Known(status) => match &status.state {
                IndexOperationState::Published(receipt) => Some(receipt.clone()),
                _ => None,
            },
            Self::Unknown { .. } => None,
        }
    }
}

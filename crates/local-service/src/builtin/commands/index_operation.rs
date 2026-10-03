//! Indexed durable receipts for caller-keyed index operations.
//!
//! The operation key and canonical request digest are permanent tombstones.
//! Detailed terminal payloads use a bounded receipt window; older terminal
//! rows become `OutsideReceiptWindow` and can never be accepted again.
//! The store admits at most 100,000 keys and retains at most 128 detailed
//! terminal receipts of at most 16 KiB each. Accepted and prepared rows remain
//! pinned, with at most 32 pending rows. These hard limits bound the durable
//! owner cost; a new key is refused before work once any applicable limit is
//! reached.

use backend_library::{
    CompileExecutionIntent, IndexOperationFailureReason, IndexOperationKey,
    IndexOperationObservation, IndexOperationPublicationReceipt, IndexOperationState,
    IndexOperationStatus, IndexOperationUnresolvedReason, PackageReference, ProductText,
    SurfaceReply, index_operation_request_digest,
};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::Path;
use std::time::Duration;

const SCHEMA_VERSION: i64 = 1;
const MAX_OPERATION_KEYS: i64 = 100_000;
const MAX_PENDING_OPERATIONS: i64 = 32;
const MAX_TERMINAL_RECEIPTS: i64 = 128;
const MAX_OPERATION_PAYLOAD_BYTES: usize = 16 * 1024;
const MAX_DATABASE_PAGES: i64 = 16_384;
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

const STATE_ACCEPTED: i64 = 1;
const STATE_PREPARED: i64 = 2;
const STATE_PUBLISHED: i64 = 3;
const STATE_FAILED: i64 = 4;
const STATE_OUTSIDE_RECEIPT_WINDOW: i64 = 5;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS backend_index_operation_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    key_count INTEGER NOT NULL CHECK (key_count >= 0),
    pending_count INTEGER NOT NULL CHECK (pending_count >= 0),
    prepared_count INTEGER NOT NULL CHECK (prepared_count >= 0),
    next_sequence INTEGER NOT NULL CHECK (next_sequence > 0),
    next_terminal_sequence INTEGER NOT NULL CHECK (next_terminal_sequence > 0)
);
INSERT OR IGNORE INTO backend_index_operation_meta
    (singleton, schema_version, key_count, pending_count, prepared_count, next_sequence, next_terminal_sequence)
    VALUES (1, 1, 0, 0, 0, 1, 1);
CREATE TABLE IF NOT EXISTS backend_index_operations (
    operation_key BLOB PRIMARY KEY NOT NULL CHECK (length(operation_key) = 32),
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    acceptance_sequence INTEGER NOT NULL UNIQUE CHECK (acceptance_sequence > 0),
    state INTEGER NOT NULL CHECK (state BETWEEN 1 AND 5),
    terminal_sequence INTEGER,
    payload BLOB,
    CHECK ((state IN (1, 2) AND terminal_sequence IS NULL AND payload IS NOT NULL) OR \
           (state IN (3, 4) AND terminal_sequence IS NOT NULL AND payload IS NOT NULL) OR \
           (state = 5 AND terminal_sequence IS NOT NULL AND payload IS NULL))
);
CREATE INDEX IF NOT EXISTS backend_index_operations_state_order
    ON backend_index_operations(state, acceptance_sequence);
CREATE INDEX IF NOT EXISTS backend_index_operations_terminal_order
    ON backend_index_operations(terminal_sequence)
    WHERE state IN (3, 4) AND payload IS NOT NULL;
";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredOperation {
    pub(super) operation_key: IndexOperationKey,
    pub(super) request_digest: [u8; 32],
    pub(super) package: PackageReference,
    pub(super) execution_intent: CompileExecutionIntent,
    pub(super) state: StoredOperationState,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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

#[derive(Debug, Eq, PartialEq)]
pub(super) enum JournalError {
    Database(String),
    Corrupt(String),
    KeyspaceFull,
    PendingLimit,
    KeyConflict,
    Missing,
    InvalidTransition,
    PayloadTooLarge,
    NonUtf8Path,
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "index-operation database: {error}"),
            Self::Corrupt(error) => {
                write!(formatter, "index-operation database is corrupt: {error}")
            }
            Self::KeyspaceFull => formatter.write_str(
                "durable index-operation keyspace reached its hard capacity; the request was not accepted",
            ),
            Self::PendingLimit => formatter.write_str(
                "durable index-operation pending limit is full; the request was not accepted",
            ),
            Self::KeyConflict => {
                formatter.write_str("index-operation key was reused with another request")
            }
            Self::Missing => formatter.write_str("index-operation key is not admitted"),
            Self::InvalidTransition => {
                formatter.write_str("invalid index-operation database transition")
            }
            Self::PayloadTooLarge => {
                formatter.write_str("index-operation receipt exceeds its bounded row size")
            }
            Self::NonUtf8Path => {
                formatter.write_str("index-operation database path is not valid UTF-8")
            }
        }
    }
}

pub(super) enum JournalEntry {
    Retained(StoredOperation),
    OutsideReceiptWindow { request_digest: [u8; 32] },
}

struct OperationRow {
    operation_key: IndexOperationKey,
    request_digest: [u8; 32],
    acceptance_sequence: i64,
    state: i64,
    terminal_sequence: Option<i64>,
    payload: Option<Vec<u8>>,
}

struct JournalMeta {
    key_count: i64,
    pending_count: i64,
    prepared_count: i64,
    next_sequence: i64,
    next_terminal_sequence: i64,
}

pub(super) struct IndexOperationJournal {
    _database: turso::Database,
    _database_directory: backend_platform::DirectoryCapability,
    _database_file: File,
    connection: turso::Connection,
}

impl IndexOperationJournal {
    pub(super) fn open(path: impl AsRef<Path>) -> Result<Self, JournalError> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .ok_or_else(|| JournalError::Database("database path has no parent".to_owned()))?;
        let database_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(JournalError::NonUtf8Path)?;
        let database_directory =
            backend_platform::DirectoryCapability::open_or_create_private(parent)
                .map_err(|error| JournalError::Database(error.to_string()))?;
        let database_file = preflight_sqlite_files(&database_directory, database_name)?;
        let database_path = parent
            .canonicalize()
            .map_err(|error| JournalError::Database(error.to_string()))?
            .join(database_name);
        let database_path = database_path.to_str().ok_or(JournalError::NonUtf8Path)?;
        let (database, connection) = futures_executor::block_on(async {
            let database = turso::Builder::new_local(database_path)
                .experimental_multiprocess_wal(true)
                .build()
                .await
                .map_err(database_error)?;
            let mut connection = database.connect().map_err(database_error)?;
            connection
                .busy_timeout(BUSY_TIMEOUT)
                .map_err(database_error)?;
            // These two setter PRAGMAs do not produce result rows in the
            // pinned Turso API. max_page_count is handled below via query
            // because it does return a row.
            connection
                .execute_batch("PRAGMA synchronous=FULL; PRAGMA cache_size=256;")
                .await
                .map_err(database_error)?;
            set_and_verify_database_page_limit(&connection, MAX_DATABASE_PAGES).await?;
            connection
                .execute_batch(SCHEMA)
                .await
                .map_err(database_error)?;
            database_directory
                .open_private_file_read_write(database_name, false)
                .map_err(|error| {
                    JournalError::Database(format!("database path changed during open: {error}"))
                })?;
            verify_sqlite_sidecars(&database_directory, database_name)?;
            validate_cold_snapshot(&mut connection).await?;
            Ok::<_, JournalError>((database, connection))
        })?;
        Ok(Self {
            _database: database,
            _database_directory: database_directory,
            _database_file: database_file,
            connection,
        })
    }

    pub(super) fn accept(
        &mut self,
        operation_key: IndexOperationKey,
        package: PackageReference,
        execution_intent: CompileExecutionIntent,
    ) -> Result<Acceptance, JournalError> {
        let request_digest = index_operation_request_digest(&package, execution_intent);
        let result = futures_executor::block_on(async {
            let transaction = self
                .connection
                .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
                .await
                .map_err(database_error)?;
            let result = async {
                if let Some(row) = load_row_transaction(&transaction, operation_key).await? {
                    match decode_row(row)? {
                        JournalEntry::OutsideReceiptWindow {
                            request_digest: known,
                        } if known == request_digest => Ok(Acceptance::Existing),
                        JournalEntry::Retained(entry)
                            if entry.request_digest == request_digest
                                && entry.package == package
                                && entry.execution_intent == execution_intent =>
                        {
                            Ok(Acceptance::Existing)
                        }
                        _ => Err(JournalError::KeyConflict),
                    }
                } else {
                    let meta = read_meta_transaction(&transaction).await?;
                    validate_meta(&meta)?;
                    check_acceptance_capacity(&meta)?;
                    let entry = StoredOperation {
                        operation_key,
                        request_digest,
                        package,
                        execution_intent,
                        state: StoredOperationState::Accepted,
                    };
                    validate_entry(&entry)?;
                    let payload = encode_entry(&entry)?;
                    transaction
                        .execute(
                            "INSERT INTO backend_index_operations \
                             (operation_key, request_digest, acceptance_sequence, state, \
                              terminal_sequence, payload) \
                             VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                            turso::params![
                                operation_key.to_bytes().to_vec(),
                                request_digest.to_vec(),
                                meta.next_sequence,
                                STATE_ACCEPTED,
                                payload
                            ],
                        )
                        .await
                        .map_err(database_error)?;
                    transaction
                        .execute(
                            "UPDATE backend_index_operation_meta \
                             SET key_count=key_count+1, pending_count=pending_count+1, \
                                 next_sequence=next_sequence+1 WHERE singleton=1",
                            (),
                        )
                        .await
                        .map_err(database_error)?;
                    Ok(Acceptance::New)
                }
            }
            .await;
            match result {
                Ok(Acceptance::New) => {
                    transaction.commit().await.map_err(database_error)?;
                    Ok(Acceptance::New)
                }
                Ok(Acceptance::Existing) => {
                    transaction.rollback().await.map_err(database_error)?;
                    Ok(Acceptance::Existing)
                }
                Err(error) => {
                    let _ = transaction.rollback().await;
                    Err(error)
                }
            }
        });
        result
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
        self.transition(operation_key, |mut entry| {
            if !matches!(entry.state, StoredOperationState::Accepted) {
                return Err(JournalError::InvalidTransition);
            }
            entry.state = StoredOperationState::Prepared {
                request_identity,
                base_workspace_root,
                base_workspace_sequence,
            };
            Ok(entry)
        })
    }

    pub(super) fn published(
        &mut self,
        operation_key: IndexOperationKey,
        receipt: IndexOperationPublicationReceipt,
    ) -> Result<(), JournalError> {
        self.transition(operation_key, |mut entry| {
            let StoredOperationState::Prepared {
                request_identity,
                base_workspace_root,
                base_workspace_sequence,
            } = entry.state
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
            entry.state = StoredOperationState::Published {
                receipt,
                request_identity,
                base_workspace_root,
                base_workspace_sequence,
            };
            Ok(entry)
        })
    }

    pub(super) fn failed(
        &mut self,
        operation_key: IndexOperationKey,
        reason: IndexOperationFailureReason,
        detail: ProductText,
    ) -> Result<(), JournalError> {
        self.transition(operation_key, |mut entry| {
            if !matches!(
                entry.state,
                StoredOperationState::Accepted | StoredOperationState::Prepared { .. }
            ) {
                return Err(JournalError::InvalidTransition);
            }
            entry.state = StoredOperationState::Failed { reason, detail };
            Ok(entry)
        })
    }

    pub(super) fn entry(
        &self,
        operation_key: IndexOperationKey,
    ) -> Result<Option<JournalEntry>, JournalError> {
        futures_executor::block_on(async {
            let Some(row) = load_row_connection(&self.connection, operation_key).await? else {
                return Ok(None);
            };
            decode_row(row).map(Some)
        })
    }

    pub(super) fn first_pending_key(&self) -> Result<Option<IndexOperationKey>, JournalError> {
        self.first_key("state IN (1, 2)")
    }

    pub(super) fn has_pending(&self) -> Result<bool, JournalError> {
        self.has_state("state IN (1, 2)")
    }

    fn first_key(&self, predicate: &str) -> Result<Option<IndexOperationKey>, JournalError> {
        futures_executor::block_on(async {
            let sql = format!(
                "SELECT operation_key FROM backend_index_operations \
                 WHERE {predicate} ORDER BY acceptance_sequence ASC LIMIT 1"
            );
            let mut rows = self
                .connection
                .query(sql, ())
                .await
                .map_err(database_error)?;
            let Some(row) = rows.next().await.map_err(database_error)? else {
                return Ok(None);
            };
            let bytes: Vec<u8> = row.get(0).map_err(database_error)?;
            let key = array32(bytes)
                .map_err(|_| JournalError::Corrupt("pending key is not 32 bytes".to_owned()))?;
            IndexOperationKey::from_bytes(key)
                .map(Some)
                .map_err(|error| JournalError::Corrupt(error.to_string()))
        })
    }

    pub(super) fn has_prepared(&self) -> Result<bool, JournalError> {
        self.has_state("state = 2")
    }

    fn has_state(&self, predicate: &str) -> Result<bool, JournalError> {
        futures_executor::block_on(async {
            let sql = format!("SELECT 1 FROM backend_index_operations WHERE {predicate} LIMIT 1");
            let mut rows = self
                .connection
                .query(sql, ())
                .await
                .map_err(database_error)?;
            Ok(rows.next().await.map_err(database_error)?.is_some())
        })
    }

    pub(super) fn observation(
        &self,
        operation_key: IndexOperationKey,
        active: Option<(
            backend_library::IndexJobTicket,
            backend_library::IndexJobStage,
        )>,
    ) -> Result<Option<IndexOperationObservation>, JournalError> {
        let Some(entry) = self.entry(operation_key)? else {
            return Ok(None);
        };
        let observation = match entry {
            JournalEntry::OutsideReceiptWindow { request_digest } => {
                IndexOperationObservation::OutsideReceiptWindow {
                    operation_key,
                    request_digest,
                }
            }
            JournalEntry::Retained(entry) => {
                let state = match &entry.state {
                    StoredOperationState::Accepted => match active {
                        Some((ticket, stage)) => IndexOperationState::Active { ticket, stage },
                        None => IndexOperationState::Accepted,
                    },
                    StoredOperationState::Prepared { .. } => return Ok(None),
                    StoredOperationState::Published { receipt, .. } => {
                        IndexOperationState::Published(receipt.clone())
                    }
                    StoredOperationState::Failed { reason, detail } => {
                        IndexOperationState::Failed {
                            reason: *reason,
                            detail: detail.clone(),
                        }
                    }
                    StoredOperationState::Unresolved { reason, detail } => {
                        IndexOperationState::Unresolved {
                            reason: *reason,
                            detail: detail.clone(),
                        }
                    }
                };
                IndexOperationObservation::Known(IndexOperationStatus::new(
                    entry.operation_key,
                    entry.package,
                    entry.execution_intent,
                    state,
                ))
            }
        };
        Ok(Some(observation))
    }

    fn transition(
        &mut self,
        operation_key: IndexOperationKey,
        change: impl FnOnce(StoredOperation) -> Result<StoredOperation, JournalError>,
    ) -> Result<(), JournalError> {
        let result = futures_executor::block_on(async {
            let transaction = self
                .connection
                .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
                .await
                .map_err(database_error)?;
            let result = async {
                let row = load_row_transaction(&transaction, operation_key)
                    .await?
                    .ok_or(JournalError::Missing)?;
                let previous_state = row.state;
                let previous = match decode_row(row)? {
                    JournalEntry::Retained(entry) => entry,
                    JournalEntry::OutsideReceiptWindow { .. } => {
                        return Err(JournalError::InvalidTransition);
                    }
                };
                let updated = change(previous)?;
                if updated.operation_key != operation_key {
                    return Err(JournalError::InvalidTransition);
                }
                validate_entry(&updated)?;
                let new_state =
                    stored_state_code(&updated.state).ok_or(JournalError::InvalidTransition)?;
                let payload = encode_entry(&updated)?;
                let is_terminal = matches!(new_state, STATE_PUBLISHED | STATE_FAILED);
                let terminal_sequence = if is_terminal {
                    let meta = read_meta_transaction(&transaction).await?;
                    validate_meta(&meta)?;
                    Some(meta.next_terminal_sequence)
                } else {
                    None
                };
                let changed = transaction
                    .execute(
                        "UPDATE backend_index_operations SET state=?1, payload=?2, \
                         terminal_sequence=?3 WHERE operation_key=?4 AND state=?5",
                        turso::params![
                            new_state,
                            payload,
                            terminal_sequence,
                            operation_key.to_bytes().to_vec(),
                            previous_state
                        ],
                    )
                    .await
                    .map_err(database_error)?;
                if changed != 1 {
                    return Err(JournalError::InvalidTransition);
                }
                let was_pending = matches!(previous_state, STATE_ACCEPTED | STATE_PREPARED);
                let is_pending = matches!(new_state, STATE_ACCEPTED | STATE_PREPARED);
                let was_prepared = previous_state == STATE_PREPARED;
                let is_prepared = new_state == STATE_PREPARED;
                let pending_delta = i64::from(is_pending) - i64::from(was_pending);
                let prepared_delta = i64::from(is_prepared) - i64::from(was_prepared);
                let terminal_delta = i64::from(is_terminal);
                if pending_delta != 0 || prepared_delta != 0 || terminal_delta != 0 {
                    transaction
                        .execute(
                            "UPDATE backend_index_operation_meta \
                             SET pending_count=pending_count+?1, \
                                 prepared_count=prepared_count+?2, \
                                 next_terminal_sequence=next_terminal_sequence+?3 \
                             WHERE singleton=1",
                            turso::params![pending_delta, prepared_delta, terminal_delta],
                        )
                        .await
                        .map_err(database_error)?;
                }
                if is_terminal {
                    prune_terminal_receipts(&transaction).await?;
                }
                Ok(())
            }
            .await;
            match result {
                Ok(()) => {
                    transaction.commit().await.map_err(database_error)?;
                    Ok(())
                }
                Err(error) => {
                    let _ = transaction.rollback().await;
                    Err(error)
                }
            }
        });
        result
    }
}

/// Validates the Turso database namespace through the already-held private
/// directory capability before Turso opens the pathname. The database itself
/// is owner-only; SQLite sidecars are opened without following links and stay
/// inside the owner-only directory. Turso's current Builder accepts a path,
/// not an already-open file handle, so these checks reject existing link or
/// reparse-point entries but cannot make Turso's pathname open atomic against
/// another actor with authority to mutate this private directory.
fn preflight_sqlite_files(
    directory: &backend_platform::DirectoryCapability,
    database_name: &str,
) -> Result<File, JournalError> {
    verify_sqlite_sidecars(directory, database_name)?;
    directory
        .open_private_file_read_write(database_name, true)
        .map_err(|error| JournalError::Database(format!("unsafe database file: {error}")))
}

fn verify_sqlite_sidecars(
    directory: &backend_platform::DirectoryCapability,
    database_name: &str,
) -> Result<(), JournalError> {
    for suffix in ["-wal", "-shm", "-tshm", "-journal"] {
        let sidecar_name = format!("{database_name}{suffix}");
        match directory.open_file_read_write(&sidecar_name, false) {
            Ok(file) if file.metadata().is_ok_and(|metadata| metadata.is_file()) => {}
            Ok(_) => {
                return Err(JournalError::Database(format!(
                    "unsafe non-regular SQLite sidecar {sidecar_name}"
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(JournalError::Database(format!(
                    "unsafe SQLite sidecar {sidecar_name}: {error}"
                )));
            }
        }
    }
    Ok(())
}

async fn prune_terminal_receipts(
    transaction: &turso::transaction::Transaction<'_>,
) -> Result<(), JournalError> {
    // Before this statement, at most 128 detailed terminal rows existed.
    // The just-written terminal can make 129. The partial index means this
    // bounded update never scans compact tombstones or pending operations.
    transaction
        .execute(
            "UPDATE backend_index_operations SET state=5, payload=NULL \
             WHERE state IN (3, 4) AND payload IS NOT NULL \
               AND terminal_sequence NOT IN (\
                   SELECT terminal_sequence FROM backend_index_operations \
                   WHERE state IN (3, 4) AND payload IS NOT NULL \
                   ORDER BY terminal_sequence DESC LIMIT ?1\
               )",
            [MAX_TERMINAL_RECEIPTS],
        )
        .await
        .map_err(database_error)?;
    Ok(())
}

async fn validate_cold_snapshot(connection: &mut turso::Connection) -> Result<(), JournalError> {
    let transaction = connection
        .transaction_with_behavior(turso::transaction::TransactionBehavior::Deferred)
        .await
        .map_err(database_error)?;
    let validation = async {
        let meta = read_validated_cold_meta(&transaction).await?;
        validate_cold_aggregates(&transaction, &meta).await
    }
    .await;
    if let Err(error) = validation {
        let _ = transaction.rollback().await;
        return Err(error);
    }
    transaction.commit().await.map_err(database_error)
}

/// Reads quota, metadata, and aggregate facts from one deferred read snapshot.
/// The first query establishes the WAL snapshot; later cold checks must use
/// this transaction so another process cannot make counters look corrupt
/// between otherwise-valid reads.
async fn read_validated_cold_meta(
    transaction: &turso::transaction::Transaction<'_>,
) -> Result<JournalMeta, JournalError> {
    // Read the durable metadata row first so the deferred transaction pins its
    // WAL snapshot before checking the page quota and bounded aggregates.
    let meta = read_meta_transaction(transaction).await?;
    let mut rows = transaction
        .query("PRAGMA page_count", ())
        .await
        .map_err(database_error)?;
    let row = rows
        .next()
        .await
        .map_err(database_error)?
        .ok_or_else(|| JournalError::Corrupt("database page count is missing".to_owned()))?;
    let page_count: i64 = row.get(0).map_err(database_error)?;
    if !(0..=MAX_DATABASE_PAGES).contains(&page_count) {
        return Err(JournalError::Corrupt(
            "database page count exceeds its configured bound".to_owned(),
        ));
    }
    validate_meta(&meta)?;
    Ok(meta)
}

async fn read_meta_transaction(
    transaction: &turso::transaction::Transaction<'_>,
) -> Result<JournalMeta, JournalError> {
    let mut rows = transaction
        .query(
            "SELECT schema_version, key_count, pending_count, prepared_count, \
                    next_sequence, next_terminal_sequence \
             FROM backend_index_operation_meta WHERE singleton=1",
            (),
        )
        .await
        .map_err(database_error)?;
    read_meta_row(&mut rows).await
}

async fn read_meta_row(rows: &mut turso::Rows) -> Result<JournalMeta, JournalError> {
    let row = rows
        .next()
        .await
        .map_err(database_error)?
        .ok_or_else(|| JournalError::Corrupt("singleton metadata row is missing".to_owned()))?;
    let version: i64 = row.get(0).map_err(database_error)?;
    if version != SCHEMA_VERSION {
        return Err(JournalError::Corrupt(format!(
            "schema version {version} is unsupported"
        )));
    }
    Ok(JournalMeta {
        key_count: row.get(1).map_err(database_error)?,
        pending_count: row.get(2).map_err(database_error)?,
        prepared_count: row.get(3).map_err(database_error)?,
        next_sequence: row.get(4).map_err(database_error)?,
        next_terminal_sequence: row.get(5).map_err(database_error)?,
    })
}

fn validate_meta(meta: &JournalMeta) -> Result<(), JournalError> {
    if meta.key_count < 0
        || meta.key_count > MAX_OPERATION_KEYS
        || meta.pending_count < 0
        || meta.pending_count > MAX_PENDING_OPERATIONS
        || meta.prepared_count < 0
        || meta.prepared_count > meta.pending_count
        || meta.next_sequence != meta.key_count.saturating_add(1)
        || meta.next_terminal_sequence < 1
        || meta.next_terminal_sequence > meta.key_count.saturating_add(1)
    {
        return Err(JournalError::Corrupt(
            "metadata counters are outside the configured bounds".to_owned(),
        ));
    }
    Ok(())
}

fn check_acceptance_capacity(meta: &JournalMeta) -> Result<(), JournalError> {
    if meta.key_count >= MAX_OPERATION_KEYS {
        return Err(JournalError::KeyspaceFull);
    }
    if meta.pending_count >= MAX_PENDING_OPERATIONS {
        return Err(JournalError::PendingLimit);
    }
    Ok(())
}

/// Installs the bounded database size and verifies the value Turso actually
/// accepted. `max_page_count` is a row-producing PRAGMA, so it must be queried
/// rather than sent through `execute_batch` (which rejects result rows).
async fn set_and_verify_database_page_limit(
    connection: &turso::Connection,
    max_pages: i64,
) -> Result<(), JournalError> {
    if !(1..=MAX_DATABASE_PAGES).contains(&max_pages) {
        return Err(JournalError::Corrupt(
            "configured database page bound is outside its allowed range".to_owned(),
        ));
    }

    let mut rows = connection
        .query(format!("PRAGMA max_page_count={max_pages}"), ())
        .await
        .map_err(database_error)?;
    let row = rows.next().await.map_err(database_error)?.ok_or_else(|| {
        JournalError::Corrupt("database maximum page count is missing".to_owned())
    })?;
    let actual_max_pages: i64 = row.get(0).map_err(database_error)?;
    if rows.next().await.map_err(database_error)?.is_some() {
        return Err(JournalError::Corrupt(
            "database maximum page count returned multiple rows".to_owned(),
        ));
    }
    if actual_max_pages > max_pages {
        return Err(JournalError::Corrupt(
            "database page count exceeds its configured bound".to_owned(),
        ));
    }
    if !(1..=MAX_DATABASE_PAGES).contains(&actual_max_pages) {
        return Err(JournalError::Corrupt(
            "database maximum page count is outside its configured bounds".to_owned(),
        ));
    }
    if actual_max_pages != max_pages {
        return Err(JournalError::Corrupt(
            "database maximum page count does not match its configured bound".to_owned(),
        ));
    }
    Ok(())
}

/// Cross-checks persisted high-water counters against bounded table aggregates
/// once at cold open. The table is capped by `MAX_OPERATION_KEYS` and the
/// database itself is capped by `MAX_DATABASE_PAGES`, so this verification has
/// a fixed maximum scan instead of trusting counters to hide durable rows.
async fn validate_cold_aggregates(
    transaction: &turso::transaction::Transaction<'_>,
    meta: &JournalMeta,
) -> Result<(), JournalError> {
    let mut rows = transaction
        .query(
            "SELECT COUNT(*), \
                    COALESCE(SUM(CASE WHEN state IN (1, 2) THEN 1 ELSE 0 END), 0), \
                    COALESCE(SUM(CASE WHEN state = 2 THEN 1 ELSE 0 END), 0), \
                    COALESCE(MIN(acceptance_sequence), 0), \
                    COALESCE(MAX(acceptance_sequence), 0), \
                    COALESCE(SUM(CASE WHEN terminal_sequence IS NOT NULL THEN 1 ELSE 0 END), 0), \
                    COALESCE(MIN(terminal_sequence), 0), \
                    COALESCE(MAX(terminal_sequence), 0), \
                    COUNT(DISTINCT acceptance_sequence), \
                    COUNT(DISTINCT terminal_sequence), \
                    COALESCE(SUM(CASE WHEN \
                        typeof(operation_key) != 'blob' OR length(operation_key) != 32 OR \
                        operation_key = zeroblob(32) OR \
                        typeof(request_digest) != 'blob' OR length(request_digest) != 32 OR \
                        acceptance_sequence <= 0 OR state NOT BETWEEN 1 AND 5 OR \
                        (state IN (1, 2) AND (terminal_sequence IS NOT NULL OR \
                            typeof(payload) != 'blob' OR \
                            length(payload) NOT BETWEEN 1 AND ?1)) OR \
                        (state IN (3, 4) AND (terminal_sequence IS NULL OR terminal_sequence <= 0 OR \
                            typeof(payload) != 'blob' OR \
                            length(payload) NOT BETWEEN 1 AND ?1)) OR \
                        (state = 5 AND (terminal_sequence IS NULL OR terminal_sequence <= 0 OR \
                            payload IS NOT NULL)) \
                    THEN 1 ELSE 0 END), 0) \
             FROM backend_index_operations",
            [MAX_OPERATION_PAYLOAD_BYTES as i64],
        )
        .await
        .map_err(database_error)?;
    let row =
        rows.next().await.map_err(database_error)?.ok_or_else(|| {
            JournalError::Corrupt("operation aggregate row is missing".to_owned())
        })?;
    let key_count: i64 = row.get(0).map_err(database_error)?;
    let pending_count: i64 = row.get(1).map_err(database_error)?;
    let prepared_count: i64 = row.get(2).map_err(database_error)?;
    let min_sequence: i64 = row.get(3).map_err(database_error)?;
    let max_sequence: i64 = row.get(4).map_err(database_error)?;
    let terminal_count: i64 = row.get(5).map_err(database_error)?;
    let min_terminal_sequence: i64 = row.get(6).map_err(database_error)?;
    let max_terminal_sequence: i64 = row.get(7).map_err(database_error)?;
    let unique_sequences: i64 = row.get(8).map_err(database_error)?;
    let unique_terminal_sequences: i64 = row.get(9).map_err(database_error)?;
    let malformed_rows: i64 = row.get(10).map_err(database_error)?;
    if malformed_rows != 0
        || key_count != meta.key_count
        || pending_count != meta.pending_count
        || prepared_count != meta.prepared_count
        || max_sequence != meta.key_count
        || unique_sequences != key_count
        || (key_count == 0 && min_sequence != 0)
        || (key_count > 0 && min_sequence != 1)
        || max_terminal_sequence.saturating_add(1) != meta.next_terminal_sequence
        || terminal_count != max_terminal_sequence
        || unique_terminal_sequences != terminal_count
        || (terminal_count == 0 && min_terminal_sequence != 0)
        || (terminal_count > 0 && min_terminal_sequence != 1)
    {
        return Err(JournalError::Corrupt(
            "metadata counters or indexed row shapes disagree with durable aggregates".to_owned(),
        ));
    }
    Ok(())
}

async fn load_row_connection(
    connection: &turso::Connection,
    operation_key: IndexOperationKey,
) -> Result<Option<OperationRow>, JournalError> {
    let rows = connection
        .query(
            "SELECT operation_key, request_digest, acceptance_sequence, state, \
                    terminal_sequence, length(payload), typeof(payload), \
                    CASE WHEN typeof(payload)='blob' \
                              AND length(payload) <= ?2 \
                         THEN payload ELSE NULL END \
             FROM backend_index_operations WHERE operation_key=?1",
            turso::params![
                operation_key.to_bytes().to_vec(),
                MAX_OPERATION_PAYLOAD_BYTES as i64
            ],
        )
        .await
        .map_err(database_error)?;
    read_operation_row(rows).await
}

async fn load_row_transaction(
    transaction: &turso::transaction::Transaction<'_>,
    operation_key: IndexOperationKey,
) -> Result<Option<OperationRow>, JournalError> {
    let rows = transaction
        .query(
            "SELECT operation_key, request_digest, acceptance_sequence, state, \
                    terminal_sequence, length(payload), typeof(payload), \
                    CASE WHEN typeof(payload)='blob' \
                              AND length(payload) <= ?2 \
                         THEN payload ELSE NULL END \
             FROM backend_index_operations WHERE operation_key=?1",
            turso::params![
                operation_key.to_bytes().to_vec(),
                MAX_OPERATION_PAYLOAD_BYTES as i64
            ],
        )
        .await
        .map_err(database_error)?;
    read_operation_row(rows).await
}

async fn read_operation_row(mut rows: turso::Rows) -> Result<Option<OperationRow>, JournalError> {
    let Some(row) = rows.next().await.map_err(database_error)? else {
        return Ok(None);
    };
    let operation_key =
        IndexOperationKey::from_bytes(array32(row.get::<Vec<u8>>(0).map_err(database_error)?)?)
            .map_err(|error| JournalError::Corrupt(error.to_string()))?;
    let request_digest = array32(row.get::<Vec<u8>>(1).map_err(database_error)?)?;
    let acceptance_sequence: i64 = row.get(2).map_err(database_error)?;
    let state: i64 = row.get(3).map_err(database_error)?;
    let terminal_sequence = match row.get_value(4).map_err(database_error)? {
        turso::Value::Null => None,
        turso::Value::Integer(value) => Some(value),
        _ => {
            return Err(JournalError::Corrupt(
                "terminal sequence is not an integer".to_owned(),
            ));
        }
    };
    let payload_length = match row.get_value(5).map_err(database_error)? {
        turso::Value::Null => None,
        turso::Value::Integer(value) => Some(value),
        _ => {
            return Err(JournalError::Corrupt(
                "operation payload length is not an integer".to_owned(),
            ));
        }
    };
    let payload_type: String = row.get(6).map_err(database_error)?;
    if acceptance_sequence <= 0 {
        return Err(JournalError::Corrupt(
            "acceptance sequence is reserved".to_owned(),
        ));
    }
    match state {
        STATE_ACCEPTED | STATE_PREPARED
            if terminal_sequence.is_none() && payload_length.is_some() => {}
        STATE_PUBLISHED | STATE_FAILED
            if terminal_sequence.is_some_and(|sequence| sequence > 0)
                && payload_length.is_some() => {}
        STATE_OUTSIDE_RECEIPT_WINDOW
            if terminal_sequence.is_some_and(|sequence| sequence > 0)
                && payload_length.is_none() => {}
        _ => {
            return Err(JournalError::Corrupt(
                "stored operation state, terminal sequence, and payload length disagree".to_owned(),
            ));
        }
    }
    let payload = match payload_length {
        None if payload_type == "null" => None,
        Some(length) if length > MAX_OPERATION_PAYLOAD_BYTES as i64 => {
            return Err(JournalError::Corrupt(
                "stored operation payload exceeds its row bound".to_owned(),
            ));
        }
        Some(length) if length > 0 && payload_type == "blob" => {
            match row.get_value(7).map_err(database_error)? {
                turso::Value::Blob(bytes) if bytes.len() == length as usize => Some(bytes),
                _ => {
                    return Err(JournalError::Corrupt(
                        "bounded operation payload is unavailable or malformed".to_owned(),
                    ));
                }
            }
        }
        _ => {
            return Err(JournalError::Corrupt(
                "operation payload is not a bounded blob".to_owned(),
            ));
        }
    };
    Ok(Some(OperationRow {
        operation_key,
        request_digest,
        acceptance_sequence,
        state,
        terminal_sequence,
        payload,
    }))
}

fn decode_row(row: OperationRow) -> Result<JournalEntry, JournalError> {
    if row.state == STATE_OUTSIDE_RECEIPT_WINDOW {
        if row.payload.is_some() || row.terminal_sequence.is_none_or(|sequence| sequence <= 0) {
            return Err(JournalError::Corrupt(
                "outside-window tombstone retained a full payload".to_owned(),
            ));
        }
        return Ok(JournalEntry::OutsideReceiptWindow {
            request_digest: row.request_digest,
        });
    }
    let payload = row.payload.ok_or_else(|| {
        JournalError::Corrupt("retained operation is missing its payload".to_owned())
    })?;
    let is_terminal = matches!(row.state, STATE_PUBLISHED | STATE_FAILED);
    if (is_terminal && row.terminal_sequence.is_none_or(|sequence| sequence <= 0))
        || (!is_terminal && row.terminal_sequence.is_some())
    {
        return Err(JournalError::Corrupt(
            "terminal sequence does not match the retained state".to_owned(),
        ));
    }
    if payload.len() > MAX_OPERATION_PAYLOAD_BYTES {
        return Err(JournalError::Corrupt(
            "stored operation payload exceeds its row bound".to_owned(),
        ));
    }
    let entry: StoredOperation = serde_json::from_slice(&payload)
        .map_err(|error| JournalError::Corrupt(error.to_string()))?;
    if entry.operation_key != row.operation_key
        || entry.request_digest != row.request_digest
        || stored_state_code(&entry.state) != Some(row.state)
    {
        return Err(JournalError::Corrupt(
            "indexed key, digest, state, and operation payload disagree".to_owned(),
        ));
    }
    validate_entry(&entry)?;
    Ok(JournalEntry::Retained(entry))
}

fn encode_entry(entry: &StoredOperation) -> Result<Vec<u8>, JournalError> {
    let bytes =
        serde_json::to_vec(entry).map_err(|error| JournalError::Corrupt(error.to_string()))?;
    if bytes.len() > MAX_OPERATION_PAYLOAD_BYTES {
        return Err(JournalError::PayloadTooLarge);
    }
    Ok(bytes)
}

fn stored_state_code(state: &StoredOperationState) -> Option<i64> {
    match state {
        StoredOperationState::Accepted => Some(STATE_ACCEPTED),
        StoredOperationState::Prepared { .. } => Some(STATE_PREPARED),
        StoredOperationState::Published { .. } => Some(STATE_PUBLISHED),
        StoredOperationState::Failed { .. } => Some(STATE_FAILED),
        StoredOperationState::Unresolved { .. } => None,
    }
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
        request_identity,
        base_workspace_root,
        ..
    } = &entry.state
        && (request_identity.is_some_and(|identity| identity.iter().all(|byte| *byte == 0))
            || base_workspace_root.iter().all(|byte| *byte == 0))
    {
        return Err(JournalError::Corrupt(
            "prepared workspace identity is reserved".to_owned(),
        ));
    }
    if let StoredOperationState::Published {
        receipt,
        request_identity,
        base_workspace_root,
        base_workspace_sequence,
    } = &entry.state
    {
        if base_workspace_root.iter().all(|byte| *byte == 0)
            || request_identity.is_some_and(|identity| identity.iter().all(|byte| *byte == 0))
            || !published_receipt_matches(
                receipt,
                *request_identity,
                *base_workspace_root,
                *base_workspace_sequence,
            )
        {
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

fn array32(bytes: Vec<u8>) -> Result<[u8; 32], JournalError> {
    bytes
        .try_into()
        .map_err(|_| JournalError::Corrupt("stored operation identity is not 32 bytes".to_owned()))
}

fn database_error(error: turso::Error) -> JournalError {
    JournalError::Database(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{Cursor, ViewRoot};
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    const MULTIPROCESS_WRITER_PATH: &str = "BACKEND_INDEX_OPERATION_WRITER_PATH";

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn path() -> std::path::PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let scratch = std::env::temp_dir().join(format!(
            "backend-index-operation-db-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&scratch);
        fs::create_dir(&scratch).expect("create private database scratch");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&scratch, fs::Permissions::from_mode(0o700))
                .expect("protect database scratch");
        }
        scratch.join("workspace").join("operations.turso")
    }

    fn package() -> PackageReference {
        PackageReference::parse("/workspace/receipt-fixture").expect("package")
    }

    fn key(value: u64) -> IndexOperationKey {
        let mut bytes = [0; 32];
        bytes[24..].copy_from_slice(&value.to_be_bytes());
        IndexOperationKey::from_bytes(bytes).expect("key")
    }

    fn open(path: &std::path::Path) -> IndexOperationJournal {
        IndexOperationJournal::open(path).expect("open index-operation database")
    }

    fn read_single_integer_pragma(journal: &IndexOperationJournal, sql: &str) -> i64 {
        futures_executor::block_on(async {
            let mut rows = journal
                .connection
                .query(sql, ())
                .await
                .expect("query pragma");
            let row = rows
                .next()
                .await
                .expect("read pragma row")
                .expect("pragma row exists");
            let value: i64 = row.get(0).expect("pragma returns an integer");
            assert!(rows.next().await.expect("check pragma row count").is_none());
            value
        })
    }

    fn operation_receipt_view() -> ViewRoot {
        let root = backend_library::view_state_root(&[]);
        let basis = backend_library::Basis::new(
            root,
            backend_library::object_version(b"index-operation-source"),
        );
        let frontier = Cursor::at(root, 0).frontier();
        ViewRoot::new_incomplete(
            backend_library::view_key(b"index-operation-test-view"),
            basis,
            frontier,
            Vec::new(),
            Vec::new(),
        )
        .expect("incomplete test view")
    }

    fn receipt(
        request_identity: Option<[u8; 32]>,
        workspace_root: [u8; 32],
        sequence: u64,
    ) -> IndexOperationPublicationReceipt {
        let view = operation_receipt_view();
        IndexOperationPublicationReceipt::from_published_view(
            request_identity,
            [2; 32],
            workspace_root,
            sequence,
            &view,
            Cursor::for_view_root(&view),
        )
        .expect("checked receipt")
    }

    fn cleanup(path: &std::path::Path) {
        if let Some(parent) = path.parent()
            && let Some(root) = parent.parent()
        {
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn cold_open_installs_the_exact_database_page_limit() {
        let path = path();
        let journal = open(&path);
        assert_eq!(
            read_single_integer_pragma(&journal, "PRAGMA max_page_count"),
            MAX_DATABASE_PAGES
        );
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn database_page_limit_rejects_a_database_above_the_cap() {
        let path = path();
        let journal = open(&path);
        let page_count = read_single_integer_pragma(&journal, "PRAGMA page_count");
        assert!(
            page_count > 2,
            "fixture must exceed the requested page limit"
        );

        let result =
            futures_executor::block_on(set_and_verify_database_page_limit(&journal.connection, 2));
        assert_eq!(
            result,
            Err(JournalError::Corrupt(
                "database page count exceeds its configured bound".to_owned()
            ))
        );
        assert_eq!(
            read_single_integer_pragma(&journal, "PRAGMA max_page_count"),
            page_count,
            "Turso must report the actual clamped maximum, not the requested value"
        );
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn accepted_key_and_request_conflict_survive_cold_reopen() {
        let path = path();
        let mut journal = open(&path);
        assert_eq!(
            journal.accept(key(1), package(), CompileExecutionIntent::Interactive),
            Ok(Acceptance::New)
        );
        drop(journal);
        let mut journal = open(&path);
        assert_eq!(
            journal.accept(key(1), package(), CompileExecutionIntent::Interactive),
            Ok(Acceptance::Existing)
        );
        assert_eq!(
            journal.accept(key(1), package(), CompileExecutionIntent::Background),
            Err(JournalError::KeyConflict)
        );
        let Some(IndexOperationObservation::Known(status)) = journal
            .observation(key(1), None)
            .expect("lookup accepted operation")
        else {
            panic!("accepted row should survive the reopen")
        };
        assert!(matches!(status.state, IndexOperationState::Accepted));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn prepared_commit_survives_cold_reopen_with_exact_mutation_identity() {
        let path = path();
        let mut journal = open(&path);
        let operation = key(2);
        journal
            .accept(operation, package(), CompileExecutionIntent::Interactive)
            .expect("accept operation");
        journal
            .prepare(operation, Some([4; 32]), [5; 32], 9)
            .expect("persist precommit binding");
        drop(journal);
        let journal = open(&path);
        let Some(JournalEntry::Retained(entry)) = journal.entry(operation).expect("read row")
        else {
            panic!("prepared row should not be compacted")
        };
        assert!(matches!(
            entry.state,
            StoredOperationState::Prepared {
                request_identity,
                base_workspace_root,
                base_workspace_sequence: 9
            } if request_identity == Some([4; 32]) && base_workspace_root == [5; 32]
        ));
        assert!(journal.has_prepared().expect("prepared state probe"));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn published_receipt_survives_cold_reopen_and_is_not_reconstructed_from_names() {
        let path = path();
        let mut journal = open(&path);
        let operation = key(3);
        journal
            .accept(operation, package(), CompileExecutionIntent::Interactive)
            .expect("accept operation");
        journal
            .prepare(operation, Some([4; 32]), [5; 32], 9)
            .expect("persist precommit binding");
        let receipt = receipt(Some([4; 32]), [6; 32], 10);
        journal
            .published(operation, receipt.clone())
            .expect("persist exact publication");
        drop(journal);
        let journal = open(&path);
        let Some(IndexOperationObservation::Known(status)) = journal
            .observation(operation, None)
            .expect("load published receipt")
        else {
            panic!("published row should retain its exact receipt")
        };
        let IndexOperationState::Published(observed) = status.state else {
            panic!("published status expected")
        };
        assert_eq!(observed, receipt);
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn oversized_payload_is_rejected_before_the_blob_projection_is_read() {
        let path = path();
        let mut journal = open(&path);
        let operation = key(30);
        journal
            .accept(operation, package(), CompileExecutionIntent::Interactive)
            .expect("accept operation");
        futures_executor::block_on(journal.connection.execute(
            "UPDATE backend_index_operations SET payload=zeroblob(?1) WHERE operation_key=?2",
            turso::params![
                (MAX_OPERATION_PAYLOAD_BYTES as i64) * 64,
                operation.to_bytes().to_vec()
            ],
        ))
        .expect("write oversized corruption fixture");

        assert!(matches!(
            journal.entry(operation),
            Err(JournalError::Corrupt(message))
                if message == "stored operation payload exceeds its row bound"
        ));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn no_op_at_genesis_is_a_checked_receipt() {
        let path = path();
        let mut journal = open(&path);
        let operation = key(4);
        journal
            .accept(operation, package(), CompileExecutionIntent::Interactive)
            .expect("accept operation");
        journal
            .prepare(operation, None, [5; 32], 0)
            .expect("record no-op base");
        let receipt = receipt(None, [5; 32], 0);
        journal
            .published(operation, receipt.clone())
            .expect("persist no-op receipt");
        let Some(IndexOperationObservation::Known(status)) = journal
            .observation(operation, None)
            .expect("read no-op receipt")
        else {
            panic!("no-op should be known")
        };
        assert_eq!(status.state, IndexOperationState::Published(receipt));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn terminal_detail_window_archives_receipts_without_reusing_their_keys() {
        let path = path();
        let mut journal = open(&path);
        let oldest_accepted = key(1);
        journal
            .accept(
                oldest_accepted,
                package(),
                CompileExecutionIntent::Interactive,
            )
            .expect("accept long-running operation");
        for number in 2..=(MAX_TERMINAL_RECEIPTS as u64 + 1) {
            let operation = key(number);
            journal
                .accept(operation, package(), CompileExecutionIntent::Interactive)
                .expect("accept operation");
            journal
                .failed(
                    operation,
                    IndexOperationFailureReason::WorkerFailed,
                    ProductText::from_static("failed before publication"),
                )
                .expect("record failure");
        }
        journal
            .failed(
                oldest_accepted,
                IndexOperationFailureReason::WorkerFailed,
                ProductText::from_static("failed after the newer operations"),
            )
            .expect("complete the long-running operation last");
        let oldest_terminal = key(2);
        assert!(matches!(
            journal
                .observation(oldest_terminal, None)
                .expect("read tombstone"),
            Some(IndexOperationObservation::OutsideReceiptWindow {
                operation_key,
                ..
            }) if operation_key == oldest_terminal
        ));
        assert_eq!(
            journal.accept(
                oldest_terminal,
                package(),
                CompileExecutionIntent::Interactive
            ),
            Ok(Acceptance::Existing)
        );
        assert_eq!(
            journal.accept(
                oldest_terminal,
                package(),
                CompileExecutionIntent::Background
            ),
            Err(JournalError::KeyConflict)
        );
        assert!(matches!(
            journal
                .observation(oldest_accepted, None)
                .expect("read last-completed receipt"),
            Some(IndexOperationObservation::Known(_))
        ));
        drop(journal);
        let mut journal = open(&path);
        assert!(matches!(
            journal
                .observation(oldest_terminal, None)
                .expect("reopen tombstone"),
            Some(IndexOperationObservation::OutsideReceiptWindow { .. })
        ));
        assert_eq!(
            journal.accept(
                oldest_terminal,
                package(),
                CompileExecutionIntent::Interactive
            ),
            Ok(Acceptance::Existing)
        );
        assert!(matches!(
            journal
                .observation(oldest_accepted, None)
                .expect("reopen last-completed receipt"),
            Some(IndexOperationObservation::Known(_))
        ));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn cold_open_rejects_key_pending_and_terminal_counter_mismatches() {
        let first_path = path();
        let mut journal = open(&first_path);
        journal
            .accept(key(60), package(), CompileExecutionIntent::Interactive)
            .expect("accept operation");
        futures_executor::block_on(journal.connection.execute(
            "UPDATE backend_index_operation_meta \
             SET key_count=0, pending_count=0, next_sequence=1 WHERE singleton=1",
            (),
        ))
        .expect("corrupt key and pending counters");
        drop(journal);
        assert!(matches!(
            IndexOperationJournal::open(&first_path),
            Err(JournalError::Corrupt(message))
                if message == "metadata counters or indexed row shapes disagree with durable aggregates"
        ));
        cleanup(&first_path);

        let path = path();
        let mut journal = open(&path);
        let operation = key(61);
        journal
            .accept(operation, package(), CompileExecutionIntent::Interactive)
            .expect("accept operation");
        journal
            .failed(
                operation,
                IndexOperationFailureReason::WorkerFailed,
                ProductText::from_static("failed before publication"),
            )
            .expect("persist terminal receipt");
        futures_executor::block_on(journal.connection.execute(
            "UPDATE backend_index_operation_meta \
             SET next_terminal_sequence=1 WHERE singleton=1",
            (),
        ))
        .expect("corrupt terminal counter");
        drop(journal);
        assert!(matches!(
            IndexOperationJournal::open(&path),
            Err(JournalError::Corrupt(message))
                if message == "metadata counters or indexed row shapes disagree with durable aggregates"
        ));
        cleanup(&path);
    }

    #[test]
    fn pending_rows_are_pinned_while_terminal_receipts_roll_over() {
        let path = path();
        let mut journal = open(&path);
        let pending = key(1);
        journal
            .accept(pending, package(), CompileExecutionIntent::Interactive)
            .expect("accept pending operation");
        journal
            .prepare(pending, Some([4; 32]), [5; 32], 9)
            .expect("prepare pending operation");
        for number in 2..=(MAX_TERMINAL_RECEIPTS as u64 + 2) {
            let operation = key(number);
            journal
                .accept(operation, package(), CompileExecutionIntent::Interactive)
                .expect("accept terminal operation");
            journal
                .failed(
                    operation,
                    IndexOperationFailureReason::WorkerFailed,
                    ProductText::from_static("failed before publication"),
                )
                .expect("record terminal failure");
        }
        assert!(matches!(
            journal.entry(pending).expect("read pinned row"),
            Some(JournalEntry::Retained(StoredOperation {
                state: StoredOperationState::Prepared { .. },
                ..
            }))
        ));
        drop(journal);
        let journal = open(&path);
        assert!(matches!(
            journal.entry(pending).expect("reopen pinned row"),
            Some(JournalEntry::Retained(StoredOperation {
                state: StoredOperationState::Prepared { .. },
                ..
            }))
        ));
        assert!(journal.has_prepared().expect("prepared state probe"));
        drop(journal);
        cleanup(&path);
    }

    #[test]
    fn state_probes_observe_commits_from_another_open_journal() {
        let path = path();
        let mut writer = open(&path);
        let observer = open(&path);
        assert!(!observer.has_pending().expect("empty pending probe"));
        assert!(!observer.has_prepared().expect("empty prepared probe"));

        let operation = key(70);
        writer
            .accept(operation, package(), CompileExecutionIntent::Interactive)
            .expect("accept operation through writer");
        assert!(observer.has_pending().expect("observe accepted row"));
        assert!(!observer.has_prepared().expect("accepted is not prepared"));

        writer
            .prepare(operation, Some([4; 32]), [5; 32], 9)
            .expect("prepare through writer");
        assert!(observer.has_prepared().expect("observe prepared row"));

        writer
            .failed(
                operation,
                IndexOperationFailureReason::WorkerFailed,
                ProductText::from_static("failed before publication"),
            )
            .expect("finish through writer");
        assert!(!observer.has_pending().expect("observe terminal row"));
        assert!(!observer.has_prepared().expect("terminal is not prepared"));
        drop(observer);
        drop(writer);
        cleanup(&path);
    }

    #[test]
    fn state_probe_multiprocess_writer_child() {
        let Some(path) = std::env::var_os(MULTIPROCESS_WRITER_PATH) else {
            return;
        };
        let mut journal = open(std::path::Path::new(&path));
        journal
            .accept(key(71), package(), CompileExecutionIntent::Interactive)
            .expect("accept from independent process");
    }

    #[test]
    fn cold_validation_keeps_one_snapshot_across_independent_process_commit() {
        let path = path();
        let mut observer = open(&path);
        assert!(!observer.has_pending().expect("empty pending probe"));

        futures_executor::block_on(async {
            // This is the first cold-validation read and fixes the parent's
            // deferred WAL snapshot before the independent process commits.
            let transaction = observer
                .connection
                .transaction_with_behavior(turso::transaction::TransactionBehavior::Deferred)
                .await
                .expect("begin cold-validation read transaction");
            let meta = read_validated_cold_meta(&transaction)
                .await
                .expect("read metadata in cold snapshot");
            assert_eq!(meta.key_count, 0);

            // Launch only after metadata has been read, then wait for the
            // child's durable accept before running the aggregate query. This
            // makes the cross-process commit land exactly between the two
            // cold-validation checks without timing-dependent sleeps.
            let output = Command::new(std::env::current_exe().expect("test executable"))
                .arg("state_probe_multiprocess_writer_child")
                .arg("--nocapture")
                .env(MULTIPROCESS_WRITER_PATH, &path)
                .output()
                .expect("spawn independent journal writer");
            assert!(
                output.status.success(),
                "independent journal writer failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            validate_cold_aggregates(&transaction, &meta)
                .await
                .expect("aggregate still matches the metadata snapshot");
            transaction
                .commit()
                .await
                .expect("release cold-validation snapshot");
        });

        // The same connection must leave its old read snapshot and observe
        // the child's accepted row through the live indexed state probes.
        assert!(
            observer
                .has_pending()
                .expect("observe other process commit")
        );
        assert_eq!(
            observer
                .first_pending_key()
                .expect("select cross-process pending row"),
            Some(key(71))
        );
        drop(observer);
        cleanup(&path);
    }

    #[test]
    fn capacity_policy_refuses_at_hard_limits_without_fabricating_database_metadata() {
        let keyspace_full = JournalMeta {
            key_count: MAX_OPERATION_KEYS,
            pending_count: 0,
            prepared_count: 0,
            next_sequence: MAX_OPERATION_KEYS + 1,
            next_terminal_sequence: 1,
        };
        validate_meta(&keyspace_full).expect("valid keyspace high-water mark");
        assert_eq!(
            check_acceptance_capacity(&keyspace_full),
            Err(JournalError::KeyspaceFull)
        );

        let pending_full = JournalMeta {
            key_count: 0,
            pending_count: MAX_PENDING_OPERATIONS,
            prepared_count: 0,
            next_sequence: 1,
            next_terminal_sequence: 1,
        };
        validate_meta(&pending_full).expect("valid pending high-water mark");
        assert_eq!(
            check_acceptance_capacity(&pending_full),
            Err(JournalError::PendingLimit)
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_and_wal_symlinks_are_rejected_before_turso_open() {
        use std::os::unix::fs::symlink;

        let path = path();
        let parent = path.parent().expect("database parent");
        let directory = backend_platform::DirectoryCapability::open_or_create_private(parent)
            .expect("create protected database directory");
        drop(directory);
        let target = parent.join("outside-state");
        fs::write(&target, b"outside bytes").expect("write sentinel");
        symlink(&target, &path).expect("link database path");
        assert!(IndexOperationJournal::open(&path).is_err());
        assert_eq!(fs::read(&target).expect("read sentinel"), b"outside bytes");
        fs::remove_file(&path).expect("remove database symlink");

        let sidecar = parent.join("operations.turso-wal");
        symlink(&target, &sidecar).expect("link WAL path");
        assert!(IndexOperationJournal::open(&path).is_err());
        assert!(!path.exists(), "sidecar refusal precedes database creation");
        assert_eq!(fs::read(&target).expect("read sentinel"), b"outside bytes");
        cleanup(&path);
    }
}

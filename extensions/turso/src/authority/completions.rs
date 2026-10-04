//! Append-only receipts for completed remote compiler-result ACKs.
//!
//! This domain is independent of the global authority schema version. The
//! owner must preflight its marker and all table/index/trigger shapes before
//! executing this DDL. A stored row is written only after the caller completes
//! the worker retirement handshake; this module independently rejoins the
//! receipt to the exact immutable compiler-selected generation and reopens
//! its compiler metadata before it can persist or return a positive record.

use super::types::{i64_to_u64, u64_to_i64};
use super::{
    AuthorityError, AuthorityNamespace, SelectedGeneration, SelectionOrigin, TursoAuthority,
};
use backend_library::{
    CompilerResultCompletionKey, CompilerResultCompletionReceipt,
    MAX_COMPILER_RESULT_COMPLETION_BYTES,
};
use backend_store::FileStore;

/// Independent schema version for the immutable completion-receipt domain.
pub(super) const COMPLETION_SCHEMA_VERSION: i64 = 1;

/// DDL for an absent completion domain. Call only after read-only preflight;
/// initialize it and its marker together inside the authority's immediate
/// initialization transaction.
pub(super) const COMPLETION_SCHEMA_V1: &str = r"
CREATE TABLE backend_compiler_result_completion_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL CHECK (schema_version = 1)
);

INSERT INTO backend_compiler_result_completion_meta(singleton, schema_version)
VALUES (1, 1);

CREATE TABLE backend_compiler_result_completions (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT CHECK (sequence > 0),
    completion_key BLOB NOT NULL UNIQUE CHECK (
        typeof(completion_key) = 'blob' AND length(completion_key) = 32
    ),
    namespace_id BLOB NOT NULL CHECK (
        typeof(namespace_id) = 'blob' AND length(namespace_id) = 16
    ),
    package TEXT NOT NULL CHECK (
        typeof(package) = 'text' AND length(CAST(package AS BLOB)) BETWEEN 1 AND 4096
        AND instr(package, char(0)) = 0
    ),
    source TEXT NOT NULL CHECK (
        typeof(source) = 'text' AND length(CAST(source AS BLOB)) BETWEEN 1 AND 4096
        AND instr(source, char(0)) = 0
    ),
    branch TEXT NOT NULL CHECK (
        typeof(branch) = 'text' AND length(CAST(branch AS BLOB)) BETWEEN 1 AND 4096
        AND instr(branch, char(0)) = 0
    ),
    environment TEXT NOT NULL CHECK (
        typeof(environment) = 'text' AND length(CAST(environment AS BLOB)) BETWEEN 1 AND 4096
        AND instr(environment, char(0)) = 0
    ),
    plane_kind INTEGER NOT NULL CHECK (plane_kind IN (0, 1)),
    profile TEXT NOT NULL CHECK (
        typeof(profile) = 'text' AND length(CAST(profile AS BLOB)) <= 256
        AND instr(profile, char(0)) = 0
    ),
    generation INTEGER NOT NULL CHECK (generation > 0),
    receipt BLOB NOT NULL CHECK (
        typeof(receipt) = 'blob' AND length(receipt) > 0 AND length(receipt) <= 1024
    ),
    CHECK ((plane_kind = 0 AND length(profile) = 0)
        OR (plane_kind = 1 AND length(profile) > 0))
);

CREATE INDEX backend_compiler_result_completions_generation
ON backend_compiler_result_completions(
    package, source, branch, environment, plane_kind, profile, generation, sequence
);

CREATE TRIGGER backend_compiler_result_completions_immutable_update
BEFORE UPDATE ON backend_compiler_result_completions
BEGIN
    SELECT RAISE(ABORT, 'compiler result completion receipt is immutable');
END;

CREATE TRIGGER backend_compiler_result_completions_immutable_delete
BEFORE DELETE ON backend_compiler_result_completions
BEGIN
    SELECT RAISE(ABORT, 'compiler result completion receipt is immutable');
END;
";

/// Maximum number of durable completion receipts returned by one query page.
pub const MAX_COMPILER_RESULT_COMPLETION_PAGE: usize = 128;

/// Durable owner completion record. Its fields are private so callers cannot
/// manufacture a persisted-receipt capability from an ACK claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedCompilerResultCompletion {
    sequence: u64,
    key: CompilerResultCompletionKey,
    namespace: AuthorityNamespace,
    receipt: CompilerResultCompletionReceipt,
}

impl RecordedCompilerResultCompletion {
    /// Monotonic owner-ledger sequence assigned when this exact receipt was
    /// first committed.
    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Stable exact-result identity key used for idempotent replay.
    #[must_use]
    pub const fn key(&self) -> CompilerResultCompletionKey {
        self.key
    }

    /// Full structural Turso namespace checked against the receipt's compact
    /// namespace identifier and immutable generation history.
    #[must_use]
    pub const fn namespace(&self) -> &AuthorityNamespace {
        &self.namespace
    }

    /// Exact canonical receipt payload reopened from the append-only row.
    #[must_use]
    pub const fn receipt(&self) -> &CompilerResultCompletionReceipt {
        &self.receipt
    }
}

impl TursoAuthority {
    /// Rejoins and durably records an owner-verified completed Stored ACK.
    ///
    /// The receipt's generation is an exact historical join key, not the
    /// current-head precondition. Newer selected heads do not invalidate an
    /// older, retained compiler-result completion. The caller must call this
    /// only after receiving the sealed completed worker-retirement result;
    /// the persisted row itself is still checked against full namespace,
    /// selected-history, and reopened compiler metadata here.
    pub async fn record_compiler_result_completion(
        &mut self,
        store: &FileStore,
        namespace: &AuthorityNamespace,
        receipt: CompilerResultCompletionReceipt,
    ) -> Result<RecordedCompilerResultCompletion, AuthorityError> {
        receipt
            .validate_shape()
            .map_err(|_| AuthorityError::ClosureMismatch)?;
        if namespace.namespace_id() != receipt.assignment.namespace_id {
            return Err(AuthorityError::ClosureMismatch);
        }

        let canonical_receipt = receipt.canonical_bytes();
        if canonical_receipt.is_empty()
            || canonical_receipt.len() > MAX_COMPILER_RESULT_COMPLETION_BYTES
        {
            return Err(AuthorityError::ClosureMismatch);
        }
        let key = receipt.completion_key();
        let generation = u64_to_i64(receipt.published.turso_generation)?;
        // Reopen content-addressed metadata before taking the writer slot. The
        // immutable-history point read is repeated under BEGIN IMMEDIATE
        // below; this is an optimistic cross-resource fence, not a transaction
        // spanning Turso and the CAS.
        let selected = self
            .selected_generation(namespace, receipt.published.turso_generation)
            .await?
            .ok_or(AuthorityError::GenerationNotFound)?;
        verify_completion_generation(store, namespace, &receipt, &selected)?;

        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;

        let current_selected = super::read_selected_generation(&tx, namespace, generation)
            .await?
            .ok_or(AuthorityError::GenerationNotFound)?;
        if current_selected != selected {
            tx.rollback().await?;
            return Err(AuthorityError::ClosureMismatch);
        }

        let mut rows = tx
            .query(
                "SELECT sequence, \
                        CASE WHEN typeof(namespace_id)='blob' AND length(namespace_id)=16 \
                             THEN namespace_id ELSE NULL END, \
                        CASE WHEN typeof(package)='text' AND length(CAST(package AS BLOB))<=4096 \
                                   AND instr(package, char(0))=0 THEN package ELSE '' END, \
                        CASE WHEN typeof(source)='text' AND length(CAST(source AS BLOB))<=4096 \
                                   AND instr(source, char(0))=0 THEN source ELSE '' END, \
                        CASE WHEN typeof(branch)='text' AND length(CAST(branch AS BLOB))<=4096 \
                                   AND instr(branch, char(0))=0 THEN branch ELSE '' END, \
                        CASE WHEN typeof(environment)='text' AND length(CAST(environment AS BLOB))<=4096 \
                                   AND instr(environment, char(0))=0 THEN environment ELSE '' END, \
                        plane_kind, \
                        CASE WHEN typeof(profile)='text' AND length(CAST(profile AS BLOB))<=256 \
                                   AND instr(profile, char(0))=0 THEN profile ELSE '' END, \
                        generation, \
                        CASE WHEN typeof(receipt)='blob' AND length(receipt)<=1024 \
                             THEN receipt ELSE NULL END \
                 FROM backend_compiler_result_completions WHERE completion_key=?1",
                [key.as_bytes().as_slice()],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let sequence: i64 = row.get(0)?;
            let namespace_id: Option<Vec<u8>> = row.get(1)?;
            let package: String = row.get(2)?;
            let source: String = row.get(3)?;
            let branch: String = row.get(4)?;
            let environment: String = row.get(5)?;
            let plane_kind: i64 = row.get(6)?;
            let profile: String = row.get(7)?;
            let stored_generation: i64 = row.get(8)?;
            let stored_receipt: Option<Vec<u8>> = row.get(9)?;
            drop(rows);
            let sequence = i64_to_u64(sequence, "completion_sequence")?;
            if sequence == 0 {
                tx.rollback().await?;
                return Err(AuthorityError::CorruptRecord("completion_sequence"));
            }
            let namespace_id =
                namespace_id.ok_or(AuthorityError::CorruptRecord("completion_namespace_id"))?;
            let stored_receipt =
                stored_receipt.ok_or(AuthorityError::CorruptRecord("completion_receipt"))?;
            let expected_namespace_id = namespace.namespace_id();
            if namespace_id.as_slice() != expected_namespace_id.as_slice()
                || !same_namespace_columns(
                    namespace,
                    &package,
                    &source,
                    &branch,
                    &environment,
                    plane_kind,
                    &profile,
                )
                || stored_generation != generation
            {
                tx.rollback().await?;
                return Err(AuthorityError::CorruptRecord("completion_scope"));
            }
            if stored_receipt != canonical_receipt {
                tx.rollback().await?;
                return Err(AuthorityError::ClosureMismatch);
            }
            let stored = CompilerResultCompletionReceipt::from_canonical_bytes(&stored_receipt)
                .map_err(|_| AuthorityError::CorruptRecord("completion_receipt"))?;
            if stored.completion_key() != key || stored != receipt {
                tx.rollback().await?;
                return Err(AuthorityError::CorruptRecord("completion_receipt"));
            }
            tx.commit().await?;
            return Ok(RecordedCompilerResultCompletion {
                sequence,
                key,
                namespace: namespace.clone(),
                receipt: stored,
            });
        }
        drop(rows);

        let (plane_kind, profile) = namespace.plane.sql_parts();
        let namespace_id = namespace.namespace_id();
        let changed = tx
            .execute(
                "INSERT INTO backend_compiler_result_completions(\
                    completion_key, namespace_id, package, source, branch, environment, \
                    plane_kind, profile, generation, receipt\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                turso::params![
                    key.as_bytes().as_slice(),
                    namespace_id.as_slice(),
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    plane_kind,
                    profile,
                    generation,
                    canonical_receipt.as_slice()
                ],
            )
            .await?;
        if changed != 1 {
            tx.rollback().await?;
            return Err(AuthorityError::CorruptRecord("completion_insert"));
        }
        let sequence = tx.last_insert_rowid();
        let sequence = i64_to_u64(sequence, "completion_sequence")?;
        tx.commit().await?;
        Ok(RecordedCompilerResultCompletion {
            sequence,
            key,
            namespace: namespace.clone(),
            receipt,
        })
    }

    /// Reads one bounded ascending page of immutable completion receipts for
    /// the exact structural namespace and selected generation.
    pub async fn compiler_result_completion_page(
        &self,
        store: &FileStore,
        namespace: &AuthorityNamespace,
        generation: u64,
        after_sequence: Option<u64>,
        limit: usize,
    ) -> Result<Box<[RecordedCompilerResultCompletion]>, AuthorityError> {
        if generation == 0 {
            return Err(AuthorityError::GenerationNotFound);
        }
        if !(1..=MAX_COMPILER_RESULT_COMPLETION_PAGE).contains(&limit) {
            return Err(AuthorityError::InvalidCompletionPageLimit);
        }
        let generation = u64_to_i64(generation)?;
        let after_sequence = u64_to_i64(after_sequence.unwrap_or(0))?;
        let page_capacity = limit;
        let limit = i64::try_from(limit).map_err(|_| AuthorityError::IntegerOverflow)?;
        let tx = self.connection.unchecked_transaction().await?;
        let (plane_kind, profile) = namespace.plane.sql_parts();
        let mut rows = tx
            .query(
                "SELECT sequence, \
                        CASE WHEN typeof(completion_key)='blob' AND length(completion_key)=32 \
                             THEN completion_key ELSE NULL END, \
                        CASE WHEN typeof(namespace_id)='blob' AND length(namespace_id)=16 \
                             THEN namespace_id ELSE NULL END, \
                        CASE WHEN typeof(package)='text' AND length(CAST(package AS BLOB))<=4096 \
                                   AND instr(package, char(0))=0 THEN package ELSE '' END, \
                        CASE WHEN typeof(source)='text' AND length(CAST(source AS BLOB))<=4096 \
                                   AND instr(source, char(0))=0 THEN source ELSE '' END, \
                        CASE WHEN typeof(branch)='text' AND length(CAST(branch AS BLOB))<=4096 \
                                   AND instr(branch, char(0))=0 THEN branch ELSE '' END, \
                        CASE WHEN typeof(environment)='text' AND length(CAST(environment AS BLOB))<=4096 \
                                   AND instr(environment, char(0))=0 THEN environment ELSE '' END, \
                        plane_kind, \
                        CASE WHEN typeof(profile)='text' AND length(CAST(profile AS BLOB))<=256 \
                                   AND instr(profile, char(0))=0 THEN profile ELSE '' END, \
                        generation, \
                        CASE WHEN typeof(receipt)='blob' AND length(receipt)<=1024 \
                             THEN receipt ELSE NULL END \
                 FROM backend_compiler_result_completions \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND generation=?7 AND sequence>?8 \
                 ORDER BY sequence ASC LIMIT ?9",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    plane_kind,
                    profile,
                    generation,
                    after_sequence,
                    limit
                ],
            )
            .await?;
        let mut stored_rows = Vec::with_capacity(page_capacity);
        while let Some(row) = rows.next().await? {
            let sequence: i64 = row.get(0)?;
            let key_bytes: Option<Vec<u8>> = row.get(1)?;
            let namespace_id: Option<Vec<u8>> = row.get(2)?;
            let package: String = row.get(3)?;
            let source: String = row.get(4)?;
            let branch: String = row.get(5)?;
            let environment: String = row.get(6)?;
            let row_plane_kind: i64 = row.get(7)?;
            let row_profile: String = row.get(8)?;
            let row_generation: i64 = row.get(9)?;
            let canonical_receipt: Option<Vec<u8>> = row.get(10)?;
            stored_rows.push((
                sequence,
                key_bytes,
                namespace_id,
                package,
                source,
                branch,
                environment,
                row_plane_kind,
                row_profile,
                row_generation,
                canonical_receipt,
            ));
        }
        drop(rows);
        tx.rollback().await?;

        let expected_namespace_id = namespace.namespace_id();
        let selected = self
            .selected_generation(namespace, i64_to_u64(generation, "generation")?)
            .await?
            .ok_or(AuthorityError::GenerationNotFound)?;
        let reopened = super::reopen_selected_compiler_metadata(store, &selected)
            .map_err(AuthorityError::ClosureVerification)?;
        let mut records = Vec::with_capacity(stored_rows.len());
        for (
            sequence,
            key_bytes,
            namespace_id,
            package,
            source,
            branch,
            environment,
            row_plane_kind,
            row_profile,
            row_generation,
            canonical_receipt,
        ) in stored_rows
        {
            let key_bytes = key_bytes.ok_or(AuthorityError::CorruptRecord("completion_key"))?;
            let namespace_id =
                namespace_id.ok_or(AuthorityError::CorruptRecord("completion_namespace_id"))?;
            let canonical_receipt =
                canonical_receipt.ok_or(AuthorityError::CorruptRecord("completion_receipt"))?;
            let sequence = i64_to_u64(sequence, "completion_sequence")?;
            if sequence == 0
                || !same_namespace_columns(
                    namespace,
                    &package,
                    &source,
                    &branch,
                    &environment,
                    row_plane_kind,
                    &row_profile,
                )
                || namespace_id.as_slice() != expected_namespace_id.as_slice()
                || row_generation != generation
            {
                return Err(AuthorityError::CorruptRecord("completion_scope"));
            }
            let key_bytes: [u8; 32] = key_bytes
                .as_slice()
                .try_into()
                .map_err(|_| AuthorityError::CorruptRecord("completion_key"))?;
            let receipt = CompilerResultCompletionReceipt::from_canonical_bytes(&canonical_receipt)
                .map_err(|_| AuthorityError::CorruptRecord("completion_receipt"))?;
            let key = receipt.completion_key();
            if key.as_bytes() != &key_bytes
                || receipt.assignment.namespace_id != namespace.namespace_id()
                || receipt.published.turso_generation != i64_to_u64(generation, "generation")?
            {
                return Err(AuthorityError::CorruptRecord("completion_identity"));
            }
            verify_completion_facts(namespace, &receipt, &selected, &reopened)?;
            records.push(RecordedCompilerResultCompletion {
                sequence,
                key,
                namespace: namespace.clone(),
                receipt,
            });
        }
        Ok(records.into_boxed_slice())
    }
}

fn verify_completion_generation(
    store: &FileStore,
    namespace: &AuthorityNamespace,
    receipt: &CompilerResultCompletionReceipt,
    selected: &SelectedGeneration,
) -> Result<(), AuthorityError> {
    let reopened = super::reopen_selected_compiler_metadata(store, selected)
        .map_err(AuthorityError::ClosureVerification)?;
    verify_completion_facts(namespace, receipt, selected, &reopened)
}

fn verify_completion_facts(
    namespace: &AuthorityNamespace,
    receipt: &CompilerResultCompletionReceipt,
    selected: &SelectedGeneration,
    reopened: &super::ReopenedCompilerMetadata,
) -> Result<(), AuthorityError> {
    let assignment = receipt.assignment;
    let published = receipt.published;
    let input = receipt.input;
    let (attempt_id, attempt_epoch) = selected.attempt();
    let (scheduler_epoch, scheduler_fence) = selected.scheduler_fence();
    if selected.namespace() != namespace
        || namespace.namespace_id() != assignment.namespace_id
        || selected.generation() != published.turso_generation
        || selected.selection_origin() != SelectionOrigin::CompilerAttempt
        || attempt_id != &assignment.turso_attempt_id
        || attempt_epoch != assignment.turso_attempt_epoch
        || scheduler_epoch != assignment.attempt
        || scheduler_fence != assignment.fence
        || selected.input_digest() != &published.input_digest
        || selected.input_digest() != &input.input_root
        || selected.candidate_id() != &published.candidate_id
        || selected.target_root() != &published.target_root
        || selected.closure_id() != &published.selected_closure_id
        || selected.semantic_catalog_root().copied() != published.semantic_plane_manifest_root
    {
        return Err(AuthorityError::ClosureMismatch);
    }
    let profile = namespace
        .plane()
        .profile()
        .and_then(|name| name.strip_suffix("/lower-ir"))
        .filter(|code| {
            code.len() == 4
                && code
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .and_then(|code| {
            Some([
                u8::from_str_radix(code.get(..2)?, 16).ok()?,
                u8::from_str_radix(code.get(2..4)?, 16).ok()?,
            ])
        })
        .ok_or(AuthorityError::InvalidNamespace)?;
    if profile != input.profile.to_bytes() {
        return Err(AuthorityError::ClosureMismatch);
    }
    let envelope = reopened.envelope();
    let metadata = reopened.metadata();
    if envelope.namespace() != namespace
        || envelope.profile() != namespace.plane().profile().unwrap_or_default()
        || metadata.binding_identity() != &published.semantic_generation
        || metadata.pinned_root() != &published.generation_root
        || metadata.dependency_set() != &published.dependency_set
        || metadata.manifest_identity() != &published.manifest
    {
        return Err(AuthorityError::ClosureMismatch);
    }
    Ok(())
}

fn same_namespace_columns(
    namespace: &AuthorityNamespace,
    package: &str,
    source: &str,
    branch: &str,
    environment: &str,
    plane_kind: i64,
    profile: &str,
) -> bool {
    let (expected_kind, expected_profile) = namespace.plane.sql_parts();
    package == namespace.package.as_ref()
        && source == namespace.source.as_ref()
        && branch == namespace.branch.as_ref()
        && environment == namespace.environment.as_ref()
        && plane_kind == expected_kind
        && profile == expected_profile
}

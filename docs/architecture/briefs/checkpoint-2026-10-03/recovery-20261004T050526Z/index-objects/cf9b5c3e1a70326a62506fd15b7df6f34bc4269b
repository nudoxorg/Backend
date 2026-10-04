//! Durable selection authority for package-index generations.
//!
//! Immutable packs and complete closures remain in content-addressed storage.
//! This module stores only their identities, exact source/input fences, and
//! mutable package/source/branch/environment selection. Projection readiness
//! is committed as an explicit lag state in the same selection transaction.

mod envelope;
mod error;
mod schema;
mod types;
mod versioned;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use envelope::FileStoreCompilerPublicationVerifier;
pub use envelope::{
    COMPILER_PUBLICATION_ENVELOPE_SCHEMA, COMPILER_PUBLICATION_METADATA_SCHEMA,
    COMPILER_SEMANTIC_IMAGE_SCHEMA, CompilerEnvelopeError, CompilerImageMember,
    CompilerPublicationEnvelope, CompilerPublicationMetadata, ReopenedCompilerImage,
    ReopenedCompilerMetadata, ReopenedCompilerPublication, reopen_selected_compiler_metadata,
    reopen_selected_compiler_publication,
};
pub use error::AuthorityError;
use types::DurableClosureVerifier;
pub use types::{
    AttemptInvalidatedByObservationProof, AuthorityHash, AuthorityNamespace, AuthorityPlane,
    CandidateAttempt, CandidateAttemptRecoveryClaim, CandidateAttemptRetirementReason,
    CandidateGeneration, ClosureClaim, ClosureReceipt, ExistingGenerationSelection,
    NoResultRetirementBarrier, ProjectionKind, ProjectionWatermark, SelectedFrontier,
    SelectedGeneration, SelectionOrigin, SourceObservation, SourceObservationReceipt,
    SourceObservationValue, SupersededAttemptProof,
};
pub use versioned::{
    VERSIONED_PLANE_MANIFEST_SCHEMA, VERSIONED_PLANE_SEGMENT_SCHEMA,
    VersionedPlaneArtifactMetadata, VersionedPlaneError, VersionedPlaneManifestSchema,
    VersionedPlaneMember, VersionedPlaneMetadata, VersionedPlanePublication,
    VersionedPlaneSegmentSchema,
};

use crate::connection::BUSY_TIMEOUT;
use backend_store::{ArtifactBudget, FileStore};
use schema::{AUTHORITY_SCHEMA, AUTHORITY_SCHEMA_VERSION};
use std::{fmt, path::Path};
use types::{i64_to_u64, u64_to_i64};

/// One process-owned handle to package-index selection authority.
///
/// It may be opened concurrently by daemon, UI, and worker processes. Each
/// mutating operation takes Turso's serialized immediate-writer lane and
/// validates its exact attempt, input, observation, and selected-head fence
/// before it updates anything.
pub struct TursoAuthority {
    _database: turso::Database,
    connection: turso::Connection,
}

impl fmt::Debug for TursoAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TursoAuthority")
            .finish_non_exhaustive()
    }
}

impl TursoAuthority {
    /// Opens or creates the greenfield authority tables at `path`.
    ///
    /// Keep this file separate from [`crate::TursoProjection`]: the latter is
    /// disposable and may be removed when its projection schema changes.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, AuthorityError> {
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| AuthorityError::NonUtf8Path(path.to_path_buf()))?;
        let database = turso::Builder::new_local(text)
            .experimental_multiprocess_wal(true)
            .experimental_index_method(true)
            .build()
            .await?;
        let mut connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.execute_batch(AUTHORITY_SCHEMA).await?;
        let tx = connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        tx.execute(
            "INSERT OR IGNORE INTO backend_index_authority_meta(singleton, schema_version) VALUES (1, ?1)",
            [AUTHORITY_SCHEMA_VERSION],
        )
        .await?;
        let mut rows = tx
            .query(
                "SELECT schema_version FROM backend_index_authority_meta WHERE singleton=1",
                (),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(AuthorityError::CorruptRecord("schema_version"))?;
        let found: i64 = row.get(0)?;
        drop(rows);
        if found != AUTHORITY_SCHEMA_VERSION {
            tx.rollback().await?;
            return Err(AuthorityError::Schema { found });
        }
        tx.commit().await?;
        Ok(Self {
            _database: database,
            connection,
        })
    }

    /// Persists the next mutable source observation for its exact namespace.
    ///
    /// A known count of zero is stored as `KnownCount(0)`; it is never encoded
    /// as absence or `Unknown`. The returned receipt can seed an exact attempt.
    pub async fn record_source_observation(
        &mut self,
        observation: SourceObservation,
    ) -> Result<SourceObservationReceipt, AuthorityError> {
        let namespace = &observation.namespace;
        let observed_at = u64_to_i64(observation.observed_at_ms)?;
        let (kind, count, reason) = encode_observation_value(&observation.value)?;
        let revision = observation.revision.as_ref().map(|value| value.as_slice());
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        ensure_scope(&tx, namespace).await?;
        let mut rows = tx
            .query(
                "SELECT latest_observation FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(AuthorityError::CorruptRecord("scope"))?;
        let previous: i64 = row.get(0)?;
        drop(rows);
        let sequence = previous
            .checked_add(1)
            .ok_or(AuthorityError::IntegerOverflow)?;
        tx.execute(
            "INSERT INTO backend_index_authority_observations(\
                package, source, branch, environment, plane_kind, profile, sequence, revision, \
                observed_at_ms, value_kind, known_count, unavailable_reason\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                sequence,
                revision,
                observed_at,
                kind,
                count,
                reason
            ],
        )
        .await?;
        tx.execute(
            "UPDATE backend_index_authority_scopes SET latest_observation=?1 \
             WHERE package=?2 AND source=?3 AND branch=?4 AND environment=?5 \
               AND plane_kind=?6 AND profile=?7",
            turso::params![
                sequence,
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1
            ],
        )
        .await?;
        tx.commit().await?;
        Ok(SourceObservationReceipt::new(
            observation,
            i64_to_u64(sequence, "observation_sequence")?,
        ))
    }

    /// Reopens the latest persisted source observation for a namespace.
    pub async fn latest_source_observation(
        &self,
        namespace: &AuthorityNamespace,
    ) -> Result<Option<SourceObservationReceipt>, AuthorityError> {
        let tx = self.connection.unchecked_transaction().await?;
        let mut scope_rows = tx
            .query(
                "SELECT latest_observation FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let sequence = match scope_rows.next().await? {
            Some(row) => row.get::<i64>(0)?,
            None => 0,
        };
        drop(scope_rows);
        let receipt = if sequence == 0 {
            None
        } else {
            Some(read_observation(&tx, namespace, sequence).await?)
        };
        tx.rollback().await?;
        Ok(receipt)
    }

    /// Returns the input digest from the latest acquired compiler attempt, if any.
    ///
    /// Callers should pair this with [`Self::latest_source_observation`]: the digest
    /// alone does not prove that it was computed after the newest source observation.
    pub async fn latest_input_digest(
        &self,
        namespace: &AuthorityNamespace,
    ) -> Result<Option<AuthorityHash>, AuthorityError> {
        let tx = self.connection.unchecked_transaction().await?;
        let mut rows = tx
            .query(
                "SELECT s.latest_attempt, a.input_digest FROM backend_index_authority_scopes AS s \
                 LEFT JOIN backend_index_authority_attempts AS a \
                   ON a.package=s.package AND a.source=s.source AND a.branch=s.branch \
                  AND a.environment=s.environment AND a.plane_kind=s.plane_kind \
                  AND a.profile=s.profile AND a.attempt_id=s.latest_attempt \
                 WHERE s.package=?1 AND s.source=?2 AND s.branch=?3 AND s.environment=?4 \
                   AND s.plane_kind=?5 AND s.profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let digest = match rows.next().await? {
            Some(row) => {
                let latest_attempt: Option<Vec<u8>> = row.get(0)?;
                match latest_attempt {
                    None => None,
                    Some(_) => {
                        let digest: Option<Vec<u8>> = row.get(1)?;
                        Some(decode_hash(
                            digest.ok_or(AuthorityError::CorruptRecord("latest_attempt"))?,
                            "input_digest",
                        )?)
                    }
                }
            }
            None => None,
        };
        drop(rows);
        tx.rollback().await?;
        Ok(digest)
    }

    /// Reopens the canonical typed namespace for one transport namespace identifier.
    ///
    /// The 128-bit transport value is only an index; callers must retain and
    /// compare the full Turso namespace before using it as maintenance debt.
    pub async fn authority_namespace_for_id(
        &self,
        namespace_id: [u8; 16],
    ) -> Result<Option<AuthorityNamespace>, AuthorityError> {
        if namespace_id == [0; 16] {
            return Ok(None);
        }
        let tx = self.connection.unchecked_transaction().await?;
        let mut rows = tx
            .query(
                "SELECT DISTINCT package, source, branch, environment, plane_kind, profile \
                 FROM backend_index_authority_scopes",
                (),
            )
            .await?;
        let mut found = None;
        while let Some(row) = rows.next().await? {
            let package: String = row.get(0)?;
            let source: String = row.get(1)?;
            let branch: String = row.get(2)?;
            let environment: String = row.get(3)?;
            let plane_kind: i64 = row.get(4)?;
            let profile: String = row.get(5)?;
            let plane = match plane_kind {
                0 if profile.is_empty() => AuthorityPlane::PackageMetadata,
                1 if !profile.is_empty() => AuthorityPlane::semantic_profile(profile)?,
                _ => return Err(AuthorityError::CorruptRecord("namespace_plane")),
            };
            let namespace =
                AuthorityNamespace::with_plane(package, source, branch, environment, plane)?;
            if namespace.namespace_id() == namespace_id {
                if found.replace(namespace).is_some() {
                    return Err(AuthorityError::CorruptRecord("namespace_id_collision"));
                }
            }
        }
        drop(rows);
        tx.rollback().await?;
        Ok(found)
    }

    /// Mints an idempotent cleanup barrier for one exact, unselected worker `NoResult` scope.
    ///
    /// This operation shares Turso's immediate-writer lane with `begin_attempt`. The barrier has
    /// its own durable identity and never fabricates a [`CandidateAttempt`]. It consumes an
    /// epoch above every candidate and prior barrier, while retiring only the terminal attempt's
    /// exact epoch. A newer active candidate therefore stays current and may still publish.
    pub async fn mint_no_result_retirement_barrier(
        &mut self,
        namespace: &AuthorityNamespace,
        terminal_work_id: [u8; 16],
        terminal_epoch: u64,
        terminal_fence: AuthorityHash,
    ) -> Result<NoResultRetirementBarrier, AuthorityError> {
        if terminal_work_id == [0; 16] || terminal_fence == [0; 32] || terminal_epoch == 0 {
            return Err(AuthorityError::StaleAttempt);
        }
        let terminal_epoch_sql = u64_to_i64(terminal_epoch)?;
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;

        let mut existing_rows = tx
            .query(
                "SELECT terminal_work_id, barrier_work_id, barrier_epoch, barrier_fence, retired_through_epoch \
                 FROM backend_index_authority_no_result_barriers \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 \
                   AND terminal_epoch=?7 AND terminal_fence=?8",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    terminal_epoch_sql,
                    terminal_fence.as_slice()
                ],
            )
            .await?;
        let existing = if let Some(row) = existing_rows.next().await? {
            let stored_terminal_work_id = decode_array::<16>(row.get(0)?, "terminal_work_id")?;
            if stored_terminal_work_id != terminal_work_id {
                tx.rollback().await?;
                return Err(AuthorityError::StaleAttempt);
            }
            let barrier_work_id = decode_array::<16>(row.get(1)?, "barrier_work_id")?;
            let barrier_epoch = i64_to_u64(row.get(2)?, "barrier_epoch")?;
            let barrier_fence = decode_hash(row.get(3)?, "barrier_fence")?;
            let retired_through_epoch = i64_to_u64(row.get(4)?, "retired_through_epoch")?;
            if retired_through_epoch != terminal_epoch {
                tx.rollback().await?;
                return Err(AuthorityError::CorruptRecord("retired_through_epoch"));
            }
            Some((
                barrier_work_id,
                barrier_epoch,
                barrier_fence,
                retired_through_epoch,
            ))
        } else {
            None
        };
        drop(existing_rows);

        let mut active_rows = tx
            .query(
                "SELECT attempt_id, state FROM backend_index_authority_attempts \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND epoch=?7 AND attempt_fence=?8",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    terminal_epoch_sql,
                    terminal_fence.as_slice()
                ],
            )
            .await?;
        let active_attempt = if let Some(row) = active_rows.next().await? {
            let attempt_id = decode_array::<16>(row.get(0)?, "attempt_id")?;
            let state: i64 = row.get(1)?;
            match state {
                0 => Some(attempt_id),
                1 => {
                    tx.rollback().await?;
                    return Err(AuthorityError::StaleAttempt);
                }
                _ => return Err(AuthorityError::CorruptRecord("attempt_state")),
            }
        } else {
            None
        };
        drop(active_rows);

        let attempt_id = if let Some(attempt_id) = active_attempt {
            attempt_id
        } else {
            // fd1 terminalizes non-published attempts into an immutable table.
            // A durable NoResult debt may outlive that move, so recover only
            // the unique epoch/fence tuple and never recreate a CandidateAttempt.
            let mut terminal_rows = tx
                .query(
                    "SELECT attempt_id, terminal_reason \
                     FROM backend_index_authority_attempt_terminals \
                     WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                       AND plane_kind=?5 AND profile=?6 AND epoch=?7 AND attempt_fence=?8",
                    turso::params![
                        namespace.package.as_ref(),
                        namespace.source.as_ref(),
                        namespace.branch.as_ref(),
                        namespace.environment.as_ref(),
                        namespace.plane.sql_parts().0,
                        namespace.plane.sql_parts().1,
                        terminal_epoch_sql,
                        terminal_fence.as_slice()
                    ],
                )
                .await?;
            let terminal = if let Some(row) = terminal_rows.next().await? {
                let attempt_id = decode_array::<16>(row.get(0)?, "attempt_id")?;
                let terminal_reason: i64 = row.get(1)?;
                if !matches!(terminal_reason, 1..=4) {
                    tx.rollback().await?;
                    return Err(AuthorityError::CorruptRecord("terminal_reason"));
                }
                Some(attempt_id)
            } else {
                None
            };
            drop(terminal_rows);
            let Some(attempt_id) = terminal else {
                tx.rollback().await?;
                return Err(AuthorityError::StaleAttempt);
            };
            attempt_id
        };

        // Terminal status alone is not enough if authority is corrupt or a
        // selected generation ever names this attempt. Never mint cleanup
        // authority for a published candidate.
        let mut selected_rows = tx
            .query(
                "SELECT EXISTS(\
                    SELECT 1 FROM backend_index_authority_frontiers \
                    WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                      AND plane_kind=?5 AND profile=?6 AND attempt_id=?7\
                 ) OR EXISTS(\
                    SELECT 1 FROM backend_index_authority_generation_history \
                    WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                      AND plane_kind=?5 AND profile=?6 AND attempt_id=?7\
                 )",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    attempt_id.as_slice()
                ],
            )
            .await?;
        let selected_row = selected_rows
            .next()
            .await?
            .ok_or(AuthorityError::CorruptRecord("selected_attempt"))?;
        let selected: i64 = selected_row.get(0)?;
        drop(selected_rows);
        if selected != 0 {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        }

        let mut scope_rows = tx
            .query(
                "SELECT attempt_epoch FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let Some(scope_row) = scope_rows.next().await? else {
            drop(scope_rows);
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        };
        let candidate_epoch: i64 = scope_row.get(0)?;
        drop(scope_rows);

        if let Some((barrier_work_id, barrier_epoch, barrier_fence, retired_through_epoch)) =
            existing
            && candidate_epoch < u64_to_i64(barrier_epoch)?
        {
            // Retries at the same or older candidate epoch stay byte-for-byte
            // idempotent. Once begin_attempt reaches this control epoch, the
            // debt must receive a newer barrier so its ACK can retire only the
            // original terminal epoch without consuming the new candidate.
            tx.commit().await?;
            return Ok(NoResultRetirementBarrier {
                namespace: namespace.clone(),
                terminal_work_id,
                terminal_epoch,
                terminal_fence,
                barrier_work_id,
                barrier_epoch,
                barrier_fence,
                retired_through_epoch,
            });
        }

        let mut barrier_rows = tx
            .query(
                "SELECT COALESCE(MAX(barrier_epoch), 0) \
                 FROM backend_index_authority_no_result_barriers \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let barrier_row = barrier_rows
            .next()
            .await?
            .ok_or(AuthorityError::CorruptRecord("barrier_epoch"))?;
        let prior_barrier_epoch: i64 = barrier_row.get(0)?;
        drop(barrier_rows);
        let barrier_epoch_sql = candidate_epoch
            .max(prior_barrier_epoch)
            .checked_add(1)
            .ok_or(AuthorityError::IntegerOverflow)?;
        let barrier_epoch = i64_to_u64(barrier_epoch_sql, "barrier_epoch")?;
        if barrier_epoch <= terminal_epoch {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        }
        let barrier_work_id = derive_no_result_barrier_work_id(
            namespace,
            terminal_work_id,
            terminal_epoch,
            terminal_fence,
            barrier_epoch,
        );
        let barrier_fence = derive_no_result_barrier_fence(
            namespace,
            terminal_work_id,
            terminal_epoch,
            terminal_fence,
            barrier_epoch,
        );
        if existing.is_some() {
            let affected = tx
                .execute(
                    "UPDATE backend_index_authority_no_result_barriers \
                     SET barrier_work_id=?1, barrier_epoch=?2, barrier_fence=?3, \
                         retired_through_epoch=?4 \
                     WHERE package=?5 AND source=?6 AND branch=?7 AND environment=?8 \
                       AND plane_kind=?9 AND profile=?10 AND terminal_epoch=?11 \
                       AND terminal_fence=?12 AND terminal_work_id=?13",
                    turso::params![
                        barrier_work_id.as_slice(),
                        barrier_epoch_sql,
                        barrier_fence.as_slice(),
                        terminal_epoch_sql,
                        namespace.package.as_ref(),
                        namespace.source.as_ref(),
                        namespace.branch.as_ref(),
                        namespace.environment.as_ref(),
                        namespace.plane.sql_parts().0,
                        namespace.plane.sql_parts().1,
                        terminal_epoch_sql,
                        terminal_fence.as_slice(),
                        terminal_work_id.as_slice()
                    ],
                )
                .await?;
            if affected != 1 {
                tx.rollback().await?;
                return Err(AuthorityError::StaleAttempt);
            }
        } else {
            tx.execute(
                "INSERT INTO backend_index_authority_no_result_barriers(\
                    package, source, branch, environment, plane_kind, profile, terminal_work_id, \
                    terminal_epoch, terminal_fence, barrier_work_id, barrier_epoch, barrier_fence, \
                    retired_through_epoch\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    terminal_work_id.as_slice(),
                    terminal_epoch_sql,
                    terminal_fence.as_slice(),
                    barrier_work_id.as_slice(),
                    barrier_epoch_sql,
                    barrier_fence.as_slice(),
                    terminal_epoch_sql
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(NoResultRetirementBarrier {
            namespace: namespace.clone(),
            terminal_work_id,
            terminal_epoch,
            terminal_fence,
            barrier_work_id,
            barrier_epoch,
            barrier_fence,
            retired_through_epoch: terminal_epoch,
        })
    }

    /// Returns an opaque durable proof when this exact persisted attempt has
    /// been superseded by a newer attempt in the same namespace.
    ///
    /// Unknown attempts, attempts with a mismatched fence, and the current
    /// attempt return `None`. A caller may bind the proof to its arriving
    /// worker result's peer and closure before acknowledging that result as
    /// stale.
    pub async fn superseded_attempt_proof(
        &self,
        namespace: &AuthorityNamespace,
        attempt_id: [u8; 16],
        epoch: u64,
        fence: AuthorityHash,
    ) -> Result<Option<SupersededAttemptProof>, AuthorityError> {
        self.read_superseded_attempt_proof(namespace, Some(attempt_id), epoch, fence)
            .await
    }

    /// Cold-restart variant for scheduler tokens that retain the exact epoch
    /// and fence but not the random Turso attempt nonce. The nonce is resolved
    /// only from the matching immutable authority row.
    pub async fn superseded_attempt_proof_for_fence(
        &self,
        namespace: &AuthorityNamespace,
        epoch: u64,
        fence: AuthorityHash,
    ) -> Result<Option<SupersededAttemptProof>, AuthorityError> {
        self.read_superseded_attempt_proof(namespace, None, epoch, fence)
            .await
    }

    async fn read_superseded_attempt_proof(
        &self,
        namespace: &AuthorityNamespace,
        expected_attempt_id: Option<[u8; 16]>,
        epoch: u64,
        fence: AuthorityHash,
    ) -> Result<Option<SupersededAttemptProof>, AuthorityError> {
        let epoch_sql = u64_to_i64(epoch)?;
        let tx = self.connection.unchecked_transaction().await?;
        let active_attempt_record = {
            let mut attempt_rows = tx
                .query(
                    "SELECT attempt_id, input_digest FROM backend_index_authority_attempts \
                     WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                       AND plane_kind=?5 AND profile=?6 AND epoch=?7 AND attempt_fence=?8",
                    turso::params![
                        namespace.package.as_ref(),
                        namespace.source.as_ref(),
                        namespace.branch.as_ref(),
                        namespace.environment.as_ref(),
                        namespace.plane.sql_parts().0,
                        namespace.plane.sql_parts().1,
                        epoch_sql,
                        fence.as_slice()
                    ],
                )
                .await?;
            if let Some(row) = attempt_rows.next().await? {
                Some((row.get::<Vec<u8>>(0)?, row.get::<Vec<u8>>(1)?))
            } else {
                None
            }
        };
        let attempt_record = if let Some(active) = active_attempt_record {
            Some(active)
        } else {
            let mut terminal_rows = tx
                .query(
                    "SELECT attempt_id, input_digest FROM backend_index_authority_attempt_terminals \
                     WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                       AND plane_kind=?5 AND profile=?6 AND epoch=?7 AND attempt_fence=?8",
                    turso::params![
                        namespace.package.as_ref(),
                        namespace.source.as_ref(),
                        namespace.branch.as_ref(),
                        namespace.environment.as_ref(),
                        namespace.plane.sql_parts().0,
                        namespace.plane.sql_parts().1,
                        epoch_sql,
                        fence.as_slice()
                    ],
                )
                .await?;
            let terminal = if let Some(row) = terminal_rows.next().await? {
                Some((row.get::<Vec<u8>>(0)?, row.get::<Vec<u8>>(1)?))
            } else {
                None
            };
            drop(terminal_rows);
            terminal
        };
        let Some((attempt_id, input_digest)) = attempt_record else {
            tx.rollback().await?;
            return Ok(None);
        };
        let attempt_id = decode_array::<16>(attempt_id, "attempt_id")?;
        let input_digest = decode_hash(input_digest, "input_digest")?;
        if expected_attempt_id.is_some_and(|expected| expected != attempt_id) {
            tx.rollback().await?;
            return Ok(None);
        }

        let mut scope_rows = tx
            .query(
                "SELECT attempt_epoch, latest_attempt FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let current_scope = scope_rows.next().await?;
        let Some(current_scope) = current_scope else {
            drop(scope_rows);
            tx.rollback().await?;
            return Ok(None);
        };
        let current_epoch_sql: i64 = current_scope.get(0)?;
        let current_attempt_id: Option<Vec<u8>> = current_scope.get(1)?;
        drop(scope_rows);
        let current_epoch = i64_to_u64(current_epoch_sql, "attempt_epoch")?;
        let Some(current_attempt_id) = current_attempt_id else {
            tx.rollback().await?;
            return Err(AuthorityError::CorruptRecord("latest_attempt"));
        };
        let current_attempt_id = decode_array::<16>(current_attempt_id, "latest_attempt")?;
        tx.rollback().await?;
        if current_epoch <= epoch || current_attempt_id == attempt_id {
            return Ok(None);
        }
        Ok(Some(SupersededAttemptProof::new(
            namespace.clone(),
            attempt_id,
            epoch,
            fence,
            input_digest,
            current_attempt_id,
            current_epoch,
        )))
    }

    /// Acquires a newer exact input attempt and fences any earlier completion.
    ///
    /// The attempt token is the only constructor path to a
    /// [`CandidateGeneration`]. Starting a later attempt increments the
    /// namespace epoch in the database, so two workers that started from one
    /// old head cannot both select their completion.
    pub async fn begin_attempt(
        &mut self,
        namespace: &AuthorityNamespace,
        input_digest: AuthorityHash,
        observation: &SourceObservationReceipt,
    ) -> Result<CandidateAttempt, AuthorityError> {
        if observation.observation.namespace != *namespace {
            return Err(AuthorityError::StaleObservation);
        }
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        ensure_scope(&tx, namespace).await?;
        let mut rows = tx
            .query(
                "SELECT attempt_epoch, latest_observation FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let row = rows
            .next()
            .await?
            .ok_or(AuthorityError::CorruptRecord("scope"))?;
        let epoch: i64 = row.get(0)?;
        let latest_observation: i64 = row.get(1)?;
        drop(rows);
        let expected_observation = u64_to_i64(observation.sequence)?;
        if expected_observation != latest_observation {
            tx.rollback().await?;
            return Err(AuthorityError::StaleObservation);
        }
        let mut barrier_rows = tx
            .query(
                "SELECT COALESCE(MAX(barrier_epoch), 0) \
                 FROM backend_index_authority_no_result_barriers \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let barrier_row = barrier_rows
            .next()
            .await?
            .ok_or(AuthorityError::CorruptRecord("barrier_epoch"))?;
        let barrier_epoch: i64 = barrier_row.get(0)?;
        drop(barrier_rows);
        let next_epoch = epoch
            .max(barrier_epoch)
            .checked_add(1)
            .ok_or(AuthorityError::IntegerOverflow)?;
        let (base_generation, base_root) = read_head_token(&tx, namespace).await?;
        let attempt_id = random_attempt_id(&tx).await?;
        let attempt_fence = derive_attempt_fence(
            namespace,
            next_epoch,
            attempt_id,
            input_digest,
            base_generation,
            base_root,
            expected_observation,
        );
        tx.execute(
            "INSERT INTO backend_index_authority_attempts(\
                package, source, branch, environment, plane_kind, profile, attempt_id, epoch, \
                attempt_fence, input_digest, base_generation, base_root, observation_sequence, state\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 0)",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                attempt_id.as_slice(),
                next_epoch,
                attempt_fence.as_slice(),
                input_digest.as_slice(),
                base_generation,
                base_root.as_ref().map(|value| value.as_slice()),
                expected_observation
            ],
        )
        .await?;
        tx.execute(
            "UPDATE backend_index_authority_scopes SET attempt_epoch=?1, latest_attempt=?2 \
             WHERE package=?3 AND source=?4 AND branch=?5 AND environment=?6 \
               AND plane_kind=?7 AND profile=?8 AND latest_observation=?9",
            turso::params![
                next_epoch,
                attempt_id.as_slice(),
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                expected_observation
            ],
        )
        .await?;
        tx.commit().await?;
        Ok(CandidateAttempt::new(
            namespace.clone(),
            i64_to_u64(next_epoch, "attempt_epoch")?,
            attempt_id,
            attempt_fence,
            input_digest,
            i64_to_u64(base_generation, "base_generation")?,
            base_root,
            observation.clone(),
        ))
    }

    /// Reopens a pending attempt after an owner restart without minting a new
    /// epoch or weakening its original source and selected-head fences.
    ///
    /// The caller supplies only a persisted lookup claim. This method reads a
    /// single Turso snapshot and returns a sealed [`CandidateAttempt`] only
    /// when the scope still names this attempt, its observation is current,
    /// the selected head is still its base, and every stored attempt field
    /// matches the claim. A selected or superseded attempt cannot be revived.
    pub async fn recover_candidate_attempt(
        &self,
        claim: &CandidateAttemptRecoveryClaim,
    ) -> Result<CandidateAttempt, AuthorityError> {
        if claim.epoch == 0
            || claim.attempt_id == [0; 16]
            || claim.fence == [0; 32]
            || claim.input_digest == [0; 32]
            || claim.observation_sequence == 0
            || (claim.base_generation == 0) != claim.base_root.is_none()
        {
            return Err(AuthorityError::StaleAttempt);
        }
        let namespace = &claim.namespace;
        let tx = self.connection.unchecked_transaction().await?;
        let mut scope_rows = tx
            .query(
                "SELECT attempt_epoch, latest_attempt, latest_observation \
                 FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let scope = scope_rows.next().await?;
        let Some(scope) = scope else {
            return Err(AuthorityError::StaleAttempt);
        };
        let current_epoch: i64 = scope.get(0)?;
        let latest_attempt: Option<Vec<u8>> = scope.get(1)?;
        let latest_observation: i64 = scope.get(2)?;
        drop(scope_rows);
        if latest_observation != u64_to_i64(claim.observation_sequence)? {
            return Err(AuthorityError::StaleObservation);
        }
        if current_epoch != u64_to_i64(claim.epoch)?
            || latest_attempt.as_deref() != Some(claim.attempt_id.as_slice())
        {
            return Err(AuthorityError::StaleAttempt);
        }
        if read_head_token(&tx, namespace).await?
            != (u64_to_i64(claim.base_generation)?, claim.base_root)
        {
            return Err(AuthorityError::StaleFrontier);
        }
        let mut attempt_rows = tx
            .query(
                "SELECT epoch, attempt_fence, input_digest, base_generation, base_root, \
                        observation_sequence, state \
                 FROM backend_index_authority_attempts \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    claim.attempt_id.as_slice()
                ],
            )
            .await?;
        let row = attempt_rows.next().await?;
        let Some(row) = row else {
            return Err(AuthorityError::StaleAttempt);
        };
        let epoch: i64 = row.get(0)?;
        let fence: Vec<u8> = row.get(1)?;
        let input_digest: Vec<u8> = row.get(2)?;
        let base_generation: i64 = row.get(3)?;
        let base_root: Option<Vec<u8>> = row.get(4)?;
        let observation_sequence: i64 = row.get(5)?;
        let state: i64 = row.get(6)?;
        drop(attempt_rows);
        if epoch != u64_to_i64(claim.epoch)?
            || fence.as_slice() != claim.fence.as_slice()
            || input_digest.as_slice() != claim.input_digest.as_slice()
            || base_generation != u64_to_i64(claim.base_generation)?
            || decode_optional_hash(base_root, "base_root")? != claim.base_root
            || observation_sequence != u64_to_i64(claim.observation_sequence)?
            || state != 0
        {
            return Err(AuthorityError::StaleAttempt);
        }
        let observation = read_observation(&tx, namespace, observation_sequence).await?;
        tx.rollback().await?;
        Ok(CandidateAttempt::new(
            namespace.clone(),
            claim.epoch,
            claim.attempt_id,
            claim.fence,
            claim.input_digest,
            claim.base_generation,
            claim.base_root,
            observation,
        ))
    }

    /// Closes an exact unselected attempt after its owner job reaches a
    /// terminal non-publication result. The operation never changes the
    /// selected head and is safe to repeat with the same or a later terminal
    /// reason. Selected attempts are an idempotent no-op and never have their
    /// selected head or terminal state changed.
    pub async fn retire_attempt(
        &mut self,
        attempt: &CandidateAttempt,
        reason: CandidateAttemptRetirementReason,
    ) -> Result<(), AuthorityError> {
        let namespace = &attempt.namespace;
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let mut rows = tx
            .query(
                "SELECT epoch, attempt_fence, input_digest, base_generation, base_root, \
                        observation_sequence, state \
                 FROM backend_index_authority_attempts \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    attempt.attempt_id.as_slice()
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            drop(rows);
            let mut terminal_rows = tx
                .query(
                    "SELECT epoch, attempt_fence, input_digest, base_generation, base_root, \
                            observation_sequence \
                     FROM backend_index_authority_attempt_terminals \
                     WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                       AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                    turso::params![
                        namespace.package.as_ref(),
                        namespace.source.as_ref(),
                        namespace.branch.as_ref(),
                        namespace.environment.as_ref(),
                        namespace.plane.sql_parts().0,
                        namespace.plane.sql_parts().1,
                        attempt.attempt_id.as_slice()
                    ],
                )
                .await?;
            let terminal = terminal_rows.next().await?;
            let Some(terminal) = terminal else {
                drop(terminal_rows);
                tx.rollback().await?;
                return Err(AuthorityError::StaleAttempt);
            };
            let terminal_epoch: i64 = terminal.get(0)?;
            let terminal_fence: Vec<u8> = terminal.get(1)?;
            let terminal_input: Vec<u8> = terminal.get(2)?;
            let terminal_base_generation: i64 = terminal.get(3)?;
            let terminal_base_root: Option<Vec<u8>> = terminal.get(4)?;
            let terminal_observation: i64 = terminal.get(5)?;
            drop(terminal_rows);
            if terminal_epoch != u64_to_i64(attempt.epoch)?
                || terminal_fence.as_slice() != attempt.fence.as_slice()
                || terminal_input.as_slice() != attempt.input_digest.as_slice()
                || terminal_base_generation != u64_to_i64(attempt.base_generation)?
                || decode_optional_hash(terminal_base_root, "base_root")? != attempt.base_root
                || terminal_observation != u64_to_i64(attempt.observation.sequence)?
            {
                tx.rollback().await?;
                return Err(AuthorityError::StaleAttempt);
            }
            tx.rollback().await?;
            return Ok(());
        };
        let epoch: i64 = row.get(0)?;
        let fence: Vec<u8> = row.get(1)?;
        let input: Vec<u8> = row.get(2)?;
        let base_generation: i64 = row.get(3)?;
        let base_root: Option<Vec<u8>> = row.get(4)?;
        let observation: i64 = row.get(5)?;
        let state: i64 = row.get(6)?;
        drop(rows);
        if epoch != u64_to_i64(attempt.epoch)?
            || fence.as_slice() != attempt.fence.as_slice()
            || input.as_slice() != attempt.input_digest.as_slice()
            || base_generation != u64_to_i64(attempt.base_generation)?
            || decode_optional_hash(base_root, "base_root")? != attempt.base_root
            || observation != u64_to_i64(attempt.observation.sequence)?
        {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        }
        match state {
            0 => {}
            1 => {
                tx.rollback().await?;
                return Ok(());
            }
            _ => {
                tx.rollback().await?;
                return Err(AuthorityError::CorruptRecord("attempt_state"));
            }
        }
        // A stale current-input fence overrides the caller's requested
        // disposition: the durable cause is supersession, regardless of
        // whether cancellation or a compiler refusal arrived first.
        let mut scope_rows = tx
            .query(
                "SELECT attempt_epoch, latest_attempt, latest_observation \
                 FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let scope = scope_rows.next().await?;
        let Some(scope) = scope else {
            drop(scope_rows);
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        };
        let current_epoch: i64 = scope.get(0)?;
        let current_attempt: Option<Vec<u8>> = scope.get(1)?;
        let current_observation: i64 = scope.get(2)?;
        drop(scope_rows);
        let terminal_reason = if current_epoch != epoch
            || current_attempt.as_deref() != Some(attempt.attempt_id.as_slice())
            || current_observation != observation
        {
            CandidateAttemptRetirementReason::Superseded
        } else {
            reason
        };
        tx.execute(
            "INSERT INTO backend_index_authority_attempt_terminals(\
                package, source, branch, environment, plane_kind, profile, attempt_id, epoch, \
                attempt_fence, input_digest, base_generation, base_root, observation_sequence, \
                terminal_reason\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                attempt.attempt_id.as_slice(),
                epoch,
                fence.as_slice(),
                input.as_slice(),
                base_generation,
                attempt.base_root.as_ref().map(|root| root.as_slice()),
                observation,
                terminal_reason.sql_code()
            ],
        )
        .await?;
        let affected = tx
            .execute(
                "DELETE FROM backend_index_authority_attempts \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7 AND epoch=?8 \
                   AND attempt_fence=?9 AND state=0",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    attempt.attempt_id.as_slice(),
                    u64_to_i64(attempt.epoch)?,
                    attempt.fence.as_slice()
                ],
            )
            .await?;
        if affected != 1 {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        }
        tx.commit().await?;
        Ok(())
    }

    /// Proves that a still-latest, unselected attempt was invalidated solely
    /// by a newer source observation before another attempt was acquired.
    ///
    /// Every fact is read from one Turso snapshot. An authority I/O error is
    /// never collapsed into `None`, and a selected attempt cannot produce this
    /// rejection-only proof even if its source observation subsequently moves.
    pub async fn attempt_invalidated_by_observation_proof(
        &self,
        claim: &CandidateAttemptRecoveryClaim,
    ) -> Result<Option<AttemptInvalidatedByObservationProof>, AuthorityError> {
        if claim.epoch == 0
            || claim.attempt_id == [0; 16]
            || claim.fence == [0; 32]
            || claim.input_digest == [0; 32]
            || claim.observation_sequence == 0
            || (claim.base_generation == 0) != claim.base_root.is_none()
        {
            return Ok(None);
        }
        let namespace = &claim.namespace;
        let tx = self.connection.unchecked_transaction().await?;
        let mut scope_rows = tx
            .query(
                "SELECT attempt_epoch, latest_attempt, latest_observation \
             FROM backend_index_authority_scopes \
             WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
               AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let Some(scope) = scope_rows.next().await? else {
            return Ok(None);
        };
        let current_epoch: i64 = scope.get(0)?;
        let current_attempt: Option<Vec<u8>> = scope.get(1)?;
        let current_observation: i64 = scope.get(2)?;
        drop(scope_rows);
        if current_epoch != u64_to_i64(claim.epoch)?
            || current_attempt.as_deref() != Some(claim.attempt_id.as_slice())
            || current_observation <= u64_to_i64(claim.observation_sequence)?
        {
            return Ok(None);
        }
        let active_attempt = {
            let mut rows = tx
                .query(
                    "SELECT epoch, attempt_fence, input_digest, base_generation, base_root, \
                            observation_sequence \
                     FROM backend_index_authority_attempts \
                     WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                       AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                    turso::params![
                        namespace.package.as_ref(),
                        namespace.source.as_ref(),
                        namespace.branch.as_ref(),
                        namespace.environment.as_ref(),
                        namespace.plane.sql_parts().0,
                        namespace.plane.sql_parts().1,
                        claim.attempt_id.as_slice()
                    ],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                Some((
                    row.get::<i64>(0)?,
                    row.get::<Vec<u8>>(1)?,
                    row.get::<Vec<u8>>(2)?,
                    row.get::<i64>(3)?,
                    row.get::<Option<Vec<u8>>>(4)?,
                    row.get::<i64>(5)?,
                    true,
                ))
            } else {
                None
            }
        };
        let attempt_facts = if let Some(active) = active_attempt {
            Some(active)
        } else {
            let mut rows = tx
                .query(
                    "SELECT epoch, attempt_fence, input_digest, base_generation, base_root, \
                            observation_sequence \
                     FROM backend_index_authority_attempt_terminals \
                     WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                       AND plane_kind=?5 AND profile=?6 AND attempt_id=?7 \
                       AND terminal_reason=?8",
                    turso::params![
                        namespace.package.as_ref(),
                        namespace.source.as_ref(),
                        namespace.branch.as_ref(),
                        namespace.environment.as_ref(),
                        namespace.plane.sql_parts().0,
                        namespace.plane.sql_parts().1,
                        claim.attempt_id.as_slice(),
                        CandidateAttemptRetirementReason::Superseded.sql_code()
                    ],
                )
                .await?;
            if let Some(row) = rows.next().await? {
                Some((
                    row.get::<i64>(0)?,
                    row.get::<Vec<u8>>(1)?,
                    row.get::<Vec<u8>>(2)?,
                    row.get::<i64>(3)?,
                    row.get::<Option<Vec<u8>>>(4)?,
                    row.get::<i64>(5)?,
                    false,
                ))
            } else {
                None
            }
        };
        let Some((epoch, fence, input, base_generation, base_root, observation, is_active)) =
            attempt_facts
        else {
            return Ok(None);
        };
        if epoch != u64_to_i64(claim.epoch)?
            || fence.as_slice() != claim.fence.as_slice()
            || input.as_slice() != claim.input_digest.as_slice()
            || base_generation != u64_to_i64(claim.base_generation)?
            || decode_optional_hash(base_root, "base_root")? != claim.base_root
            || observation != u64_to_i64(claim.observation_sequence)?
            || (is_active && current_observation <= observation)
        {
            return Ok(None);
        }
        let mut history_rows = tx
            .query(
                "SELECT 1 FROM backend_index_authority_generation_history \
             WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
               AND plane_kind=?5 AND profile=?6 AND attempt_id=?7 LIMIT 1",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    claim.attempt_id.as_slice()
                ],
            )
            .await?;
        let was_selected = history_rows.next().await?.is_some();
        drop(history_rows);
        if was_selected {
            return Ok(None);
        }
        let current = read_observation(&tx, namespace, current_observation).await?;
        tx.rollback().await?;
        Ok(Some(AttemptInvalidatedByObservationProof::new(
            claim,
            current.sequence(),
            current.observation().revision(),
        )))
    }

    /// Returns a closure receipt only after an injected verifier accepts every
    /// member named by the candidate's exact claim.
    fn verify_closure<V: DurableClosureVerifier>(
        &self,
        candidate: &CandidateGeneration,
        verifier: &V,
    ) -> Result<ClosureReceipt, AuthorityError> {
        let claim = ClosureClaim::for_candidate(candidate);
        verifier
            .verify_closure(&claim)
            .map_err(|error| AuthorityError::ClosureVerification(error.to_string()))?;
        Ok(ClosureReceipt { claim })
    }

    /// Verifies a local compiler publication envelope and every admitted member
    /// in the exact outer FileStore closure before minting its selection receipt.
    pub fn verify_compiler_publication(
        &self,
        candidate: &CandidateGeneration,
        admitted_attempt: &CandidateAttempt,
        store: &FileStore,
        budget: ArtifactBudget,
        envelope: &CompilerPublicationEnvelope,
        metadata: &CompilerPublicationMetadata,
    ) -> Result<ClosureReceipt, AuthorityError> {
        if candidate.attempt() != admitted_attempt {
            return Err(AuthorityError::AdmittedInputMismatch);
        }
        let verifier = FileStoreCompilerPublicationVerifier::new(store, budget, envelope, metadata);
        self.verify_closure(candidate, &verifier)
    }

    /// Atomically selects a fully verified candidate against its exact attempt,
    /// input digest, source observation, and base frontier.
    ///
    /// Catalog, graph, and lexical projection watermarks are set to lagging in
    /// the same transaction. A crash after selection therefore reopens to the
    /// exact new head plus explicit projection lag.
    pub async fn compare_and_select(
        &mut self,
        candidate: CandidateGeneration,
        receipt: ClosureReceipt,
    ) -> Result<SelectedFrontier, AuthorityError> {
        let expected_claim = ClosureClaim::for_candidate(&candidate);
        if receipt.claim != expected_claim {
            return Err(AuthorityError::ClosureMismatch);
        }
        let attempt = &candidate.attempt;
        let namespace = &attempt.namespace;
        let attempt_epoch = u64_to_i64(attempt.epoch)?;
        let observation_sequence = u64_to_i64(attempt.observation.sequence)?;
        let base_generation = u64_to_i64(attempt.base_generation)?;
        let next_generation = base_generation
            .checked_add(1)
            .ok_or(AuthorityError::IntegerOverflow)?;
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let mut scope_rows = tx
            .query(
                "SELECT attempt_epoch, latest_attempt, latest_observation \
                 FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let scope = scope_rows.next().await?;
        let Some(scope) = scope else {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        };
        let persisted_epoch: i64 = scope.get(0)?;
        let latest_attempt: Option<Vec<u8>> = scope.get(1)?;
        let latest_observation: i64 = scope.get(2)?;
        drop(scope_rows);
        if persisted_epoch != attempt_epoch
            || latest_attempt.as_deref() != Some(attempt.attempt_id.as_slice())
            || latest_observation != observation_sequence
        {
            tx.rollback().await?;
            return Err(if latest_observation != observation_sequence {
                AuthorityError::StaleObservation
            } else {
                AuthorityError::StaleAttempt
            });
        }
        let persisted_base = read_head_token(&tx, namespace).await?;
        if persisted_base != (base_generation, attempt.base_root) {
            tx.rollback().await?;
            return Err(AuthorityError::StaleFrontier);
        }
        let mut attempt_rows = tx
            .query(
                "SELECT epoch, attempt_fence, input_digest, base_generation, base_root, observation_sequence, state \
                 FROM backend_index_authority_attempts \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    attempt.attempt_id.as_slice()
                ],
            )
            .await?;
        let attempt_row = attempt_rows.next().await?;
        let Some(attempt_row) = attempt_row else {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        };
        let stored_epoch: i64 = attempt_row.get(0)?;
        let stored_fence: Vec<u8> = attempt_row.get(1)?;
        let stored_input: Vec<u8> = attempt_row.get(2)?;
        let stored_base_generation: i64 = attempt_row.get(3)?;
        let stored_base_root: Option<Vec<u8>> = attempt_row.get(4)?;
        let stored_observation: i64 = attempt_row.get(5)?;
        let state: i64 = attempt_row.get(6)?;
        drop(attempt_rows);
        if stored_epoch != attempt_epoch
            || stored_fence.as_slice() != attempt.fence.as_slice()
            || stored_input.as_slice() != attempt.input_digest.as_slice()
            || stored_base_generation != base_generation
            || decode_optional_hash(stored_base_root, "base_root")? != attempt.base_root
            || stored_observation != observation_sequence
            || state != 0
        {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        }
        let mut retirement_rows = tx
            .query(
                "SELECT 1 FROM backend_index_authority_no_result_barriers \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND terminal_epoch=?7 \
                   AND terminal_fence=?8 LIMIT 1",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    attempt_epoch,
                    attempt.fence.as_slice()
                ],
            )
            .await?;
        let retired_by_barrier = retirement_rows.next().await?.is_some();
        drop(retirement_rows);
        if retired_by_barrier {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        }
        let mut observation_rows = tx
            .query(
                "SELECT 1 FROM backend_index_authority_observations \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND sequence=?7",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    observation_sequence
                ],
            )
            .await?;
        if observation_rows.next().await?.is_none() {
            drop(observation_rows);
            tx.rollback().await?;
            return Err(AuthorityError::StaleObservation);
        }
        drop(observation_rows);
        let root = candidate.target_root;
        tx.execute(
            "INSERT INTO backend_index_authority_generation_history(\
                package, source, branch, environment, plane_kind, profile, generation, candidate_id, \
                attempt_id, attempt_epoch, attempt_fence, input_digest, target_root, pack_id, closure_id, \
                semantic_manifest_root, observation_sequence, selection_origin\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                next_generation,
                candidate.candidate_id.as_slice(),
                attempt.attempt_id.as_slice(),
                attempt_epoch,
                attempt.fence.as_slice(),
                attempt.input_digest.as_slice(),
                root.as_slice(),
                candidate.pack_id.as_slice(),
                candidate.closure_id.as_slice(),
                candidate.semantic_manifest_root.as_ref().map(|root| root.as_slice()),
                observation_sequence,
                SelectionOrigin::CompilerAttempt.as_sql()
            ],
        )
        .await?;
        tx.execute(
            "INSERT INTO backend_index_authority_frontiers(\
                package, source, branch, environment, plane_kind, profile, generation, candidate_id, \
                attempt_id, attempt_epoch, attempt_fence, input_digest, target_root, pack_id, closure_id, \
                semantic_manifest_root, observation_sequence, selection_origin\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18) \
             ON CONFLICT(package, source, branch, environment, plane_kind, profile) DO UPDATE SET \
                generation=excluded.generation, candidate_id=excluded.candidate_id, \
                attempt_id=excluded.attempt_id, attempt_epoch=excluded.attempt_epoch, \
                attempt_fence=excluded.attempt_fence, \
                input_digest=excluded.input_digest, target_root=excluded.target_root, \
                pack_id=excluded.pack_id, closure_id=excluded.closure_id, \
                semantic_manifest_root=excluded.semantic_manifest_root, \
                observation_sequence=excluded.observation_sequence, \
                selection_origin=excluded.selection_origin",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                next_generation,
                candidate.candidate_id.as_slice(),
                attempt.attempt_id.as_slice(),
                attempt_epoch,
                attempt.fence.as_slice(),
                attempt.input_digest.as_slice(),
                root.as_slice(),
                candidate.pack_id.as_slice(),
                candidate.closure_id.as_slice(),
                candidate.semantic_manifest_root.as_ref().map(|root| root.as_slice()),
                observation_sequence,
                SelectionOrigin::CompilerAttempt.as_sql()
            ],
        )
        .await?;
        for projector in [
            ProjectionKind::Catalog,
            ProjectionKind::Graph,
            ProjectionKind::Lexical,
        ] {
            tx.execute(
                "INSERT INTO backend_index_authority_projection_watermarks(\
                    package, source, branch, environment, plane_kind, profile, projector, \
                    selected_generation, selected_root, projected_generation, projected_root, state\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL, 0) \
                 ON CONFLICT(package, source, branch, environment, plane_kind, profile, projector) DO UPDATE SET \
                    selected_generation=excluded.selected_generation, \
                    selected_root=excluded.selected_root, projected_generation=NULL, \
                    projected_root=NULL, state=0",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    projector.as_sql(),
                    next_generation,
                    root.as_slice()
                ],
            )
            .await?;
        }
        let affected = tx
            .execute(
                "UPDATE backend_index_authority_attempts SET state=1 \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7 AND epoch=?8 AND state=0",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    attempt.attempt_id.as_slice(),
                    attempt_epoch
                ],
            )
            .await?;
        if affected != 1 {
            tx.rollback().await?;
            return Err(AuthorityError::StaleAttempt);
        }
        let watermarks = read_projection_watermarks(&tx, namespace).await?;
        let selected = SelectedFrontier::new(
            namespace.clone(),
            i64_to_u64(next_generation, "generation")?,
            candidate.candidate_id,
            attempt.attempt_id,
            attempt.epoch,
            attempt.fence,
            attempt.input_digest,
            candidate.target_root,
            candidate.pack_id,
            candidate.closure_id,
            candidate.semantic_manifest_root,
            attempt.observation.clone(),
            watermarks.into_boxed_slice(),
            SelectionOrigin::CompilerAttempt,
        );
        tx.commit().await?;
        drop(receipt);
        Ok(selected)
    }

    /// Selects an immutable retained generation as a new monotonic head event.
    ///
    /// The exact stored candidate closure is independently re-verified first.
    /// The immediate writer transaction then compares the caller's selected
    /// generation/root token with the live head and checks the retained
    /// generation against the latest source observation and acquired input.
    /// Historical selection requires explicit caller acknowledgement. The
    /// transaction appends a new history record and marks every projection
    /// lagging. Advancing the generation token fences attempts acquired from
    /// the former head.
    async fn select_existing_generation<V: DurableClosureVerifier>(
        &mut self,
        namespace: &AuthorityNamespace,
        expected_frontier: Option<&SelectedFrontier>,
        retained_generation: u64,
        intent: ExistingGenerationSelection,
        verifier: &V,
    ) -> Result<SelectedFrontier, AuthorityError> {
        let expected_token = match expected_frontier {
            Some(frontier) if frontier.namespace != *namespace => {
                return Err(AuthorityError::StaleFrontier);
            }
            Some(frontier) => (u64_to_i64(frontier.generation)?, Some(frontier.target_root)),
            None => (0, None),
        };
        let retained = self
            .selected_generation(namespace, retained_generation)
            .await?
            .ok_or(AuthorityError::GenerationNotFound)?;
        let claim = ClosureClaim::for_selected_generation(&retained);
        verifier
            .verify_closure(&claim)
            .map_err(|error| AuthorityError::ClosureVerification(error.to_string()))?;

        let next_generation = expected_token
            .0
            .checked_add(1)
            .ok_or(AuthorityError::IntegerOverflow)?;
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        if read_head_token(&tx, namespace).await? != expected_token {
            tx.rollback().await?;
            return Err(AuthorityError::StaleFrontier);
        }
        let mut scope_rows = tx
            .query(
                "SELECT latest_observation, latest_attempt \
                 FROM backend_index_authority_scopes \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6",
                namespace_params(namespace),
            )
            .await?;
        let scope = scope_rows
            .next()
            .await?
            .ok_or(AuthorityError::CorruptRecord("scope"))?;
        let latest_observation: i64 = scope.get(0)?;
        let latest_attempt: Option<Vec<u8>> = scope.get(1)?;
        drop(scope_rows);
        let latest_attempt =
            latest_attempt.ok_or(AuthorityError::CorruptRecord("latest_attempt"))?;
        let mut attempt_rows = tx
            .query(
                "SELECT input_digest FROM backend_index_authority_attempts \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND attempt_id=?7",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    latest_attempt
                ],
            )
            .await?;
        let latest_attempt_row = attempt_rows
            .next()
            .await?
            .ok_or(AuthorityError::CorruptRecord("latest_attempt"))?;
        let latest_input_digest: Vec<u8> = latest_attempt_row.get(0)?;
        drop(attempt_rows);
        let historical = latest_observation != u64_to_i64(retained.observation.sequence)?
            || latest_input_digest.as_slice() != retained.input_digest.as_slice();
        let selection_origin = match (intent, historical) {
            (ExistingGenerationSelection::RequireCurrentObservation, true) => {
                tx.rollback().await?;
                return Err(AuthorityError::HistoricalSelectionRequiresAcknowledgement);
            }
            (_, false) => SelectionOrigin::RetainedCurrentObservation,
            (ExistingGenerationSelection::AcknowledgeHistorical, true) => {
                SelectionOrigin::RetainedHistoricalObservation
            }
        };
        let observation_sequence = u64_to_i64(retained.observation.sequence)?;
        tx.execute(
            "INSERT INTO backend_index_authority_generation_history(\
                package, source, branch, environment, plane_kind, profile, generation, candidate_id, \
                attempt_id, attempt_epoch, attempt_fence, input_digest, target_root, pack_id, closure_id, \
                semantic_manifest_root, observation_sequence, selection_origin\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                next_generation,
                retained.candidate_id.as_slice(),
                retained.attempt_id.as_slice(),
                u64_to_i64(retained.attempt_epoch)?,
                retained.attempt_fence.as_slice(),
                retained.input_digest.as_slice(),
                retained.target_root.as_slice(),
                retained.pack_id.as_slice(),
                retained.closure_id.as_slice(),
                retained.semantic_manifest_root.as_ref().map(|root| root.as_slice()),
                observation_sequence,
                selection_origin.as_sql()
            ],
        )
        .await?;
        tx.execute(
            "INSERT INTO backend_index_authority_frontiers(\
                package, source, branch, environment, plane_kind, profile, generation, candidate_id, \
                attempt_id, attempt_epoch, attempt_fence, input_digest, target_root, pack_id, closure_id, \
                semantic_manifest_root, observation_sequence, selection_origin\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18) \
             ON CONFLICT(package, source, branch, environment, plane_kind, profile) DO UPDATE SET \
                generation=excluded.generation, candidate_id=excluded.candidate_id, \
                attempt_id=excluded.attempt_id, attempt_epoch=excluded.attempt_epoch, \
                attempt_fence=excluded.attempt_fence, input_digest=excluded.input_digest, \
                target_root=excluded.target_root, pack_id=excluded.pack_id, closure_id=excluded.closure_id, \
                semantic_manifest_root=excluded.semantic_manifest_root, \
                observation_sequence=excluded.observation_sequence, \
                selection_origin=excluded.selection_origin",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                next_generation,
                retained.candidate_id.as_slice(),
                retained.attempt_id.as_slice(),
                u64_to_i64(retained.attempt_epoch)?,
                retained.attempt_fence.as_slice(),
                retained.input_digest.as_slice(),
                retained.target_root.as_slice(),
                retained.pack_id.as_slice(),
                retained.closure_id.as_slice(),
                retained.semantic_manifest_root.as_ref().map(|root| root.as_slice()),
                observation_sequence,
                selection_origin.as_sql()
            ],
        )
        .await?;
        for projector in [
            ProjectionKind::Catalog,
            ProjectionKind::Graph,
            ProjectionKind::Lexical,
        ] {
            tx.execute(
                "INSERT INTO backend_index_authority_projection_watermarks(\
                    package, source, branch, environment, plane_kind, profile, projector, \
                    selected_generation, selected_root, projected_generation, projected_root, state\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL, 0) \
                 ON CONFLICT(package, source, branch, environment, plane_kind, profile, projector) DO UPDATE SET \
                    selected_generation=excluded.selected_generation, selected_root=excluded.selected_root, \
                    projected_generation=NULL, projected_root=NULL, state=0",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    projector.as_sql(),
                    next_generation,
                    retained.target_root.as_slice()
                ],
            )
            .await?;
        }
        let selected = read_selected_frontier(&tx, namespace)
            .await?
            .ok_or(AuthorityError::CorruptRecord("selected_frontier"))?;
        tx.commit().await?;
        Ok(selected)
    }

    /// Re-selects one retained compiler generation after reopening its exact
    /// envelope, metadata, and closure membership from durable storage.
    ///
    /// This typed path only traverses the closure index and small metadata;
    /// immutable output objects were already admitted before their original
    /// selection and are read lazily by their consumers.
    pub async fn select_existing_compiler_generation(
        &mut self,
        namespace: &AuthorityNamespace,
        expected_frontier: Option<&SelectedFrontier>,
        retained_generation: u64,
        intent: ExistingGenerationSelection,
        store: &FileStore,
    ) -> Result<SelectedFrontier, AuthorityError> {
        let retained = self
            .selected_generation(namespace, retained_generation)
            .await?
            .ok_or(AuthorityError::GenerationNotFound)?;
        let reopened = reopen_selected_compiler_metadata(store, &retained)
            .map_err(AuthorityError::ClosureVerification)?;
        let verifier = SelectedCompilerMetadataVerifier {
            store,
            selected: retained,
            expected: reopened,
        };
        self.select_existing_generation(
            namespace,
            expected_frontier,
            retained_generation,
            intent,
            &verifier,
        )
        .await
    }

    /// Cold-reopens the complete selected answer and projection watermarks.
    pub async fn selected_frontier(
        &self,
        namespace: &AuthorityNamespace,
    ) -> Result<Option<SelectedFrontier>, AuthorityError> {
        let tx = self.connection.unchecked_transaction().await?;
        let selected = read_selected_frontier(&tx, namespace).await?;
        tx.rollback().await?;
        Ok(selected)
    }

    /// Reads a bounded lexicographic page of selected namespace heads.
    ///
    /// This enumeration is independent of workspace package relations so cold
    /// startup can discover and repair a head committed just before a crash.
    pub async fn selected_frontiers(
        &self,
        after: Option<&AuthorityNamespace>,
        limit: usize,
    ) -> Result<Box<[SelectedFrontier]>, AuthorityError> {
        const MAX_PAGE: usize = 512;
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AuthorityError::InvalidHistoryPageLimit);
        }
        let limit = i64::try_from(limit).map_err(|_| AuthorityError::IntegerOverflow)?;
        let tx = self.connection.unchecked_transaction().await?;
        let mut namespaces = Vec::new();
        if let Some(after) = after {
            let mut rows = tx
                .query(
                    "SELECT package, source, branch, environment, plane_kind, profile \
                     FROM backend_index_authority_frontiers \
                     WHERE package>?1 \
                        OR (package=?1 AND source>?2) \
                        OR (package=?1 AND source=?2 AND branch>?3) \
                        OR (package=?1 AND source=?2 AND branch=?3 AND environment>?4) \
                        OR (package=?1 AND source=?2 AND branch=?3 AND environment=?4 AND plane_kind>?5) \
                        OR (package=?1 AND source=?2 AND branch=?3 AND environment=?4 AND plane_kind=?5 AND profile>?6) \
                     ORDER BY package, source, branch, environment, plane_kind, profile LIMIT ?7",
                    turso::params![
                        after.package.as_ref(),
                        after.source.as_ref(),
                        after.branch.as_ref(),
                        after.environment.as_ref(),
                        after.plane.sql_parts().0,
                        after.plane.sql_parts().1,
                        limit
                    ],
                )
                .await?;
            while let Some(row) = rows.next().await? {
                namespaces.push((
                    row.get::<String>(0)?,
                    row.get::<String>(1)?,
                    row.get::<String>(2)?,
                    row.get::<String>(3)?,
                    row.get::<i64>(4)?,
                    row.get::<String>(5)?,
                ));
            }
            drop(rows);
        } else {
            let mut rows = tx
                .query(
                    "SELECT package, source, branch, environment, plane_kind, profile \
                     FROM backend_index_authority_frontiers \
                     ORDER BY package, source, branch, environment, plane_kind, profile LIMIT ?1",
                    [limit],
                )
                .await?;
            while let Some(row) = rows.next().await? {
                namespaces.push((
                    row.get::<String>(0)?,
                    row.get::<String>(1)?,
                    row.get::<String>(2)?,
                    row.get::<String>(3)?,
                    row.get::<i64>(4)?,
                    row.get::<String>(5)?,
                ));
            }
            drop(rows);
        }
        let mut selected = Vec::with_capacity(namespaces.len());
        for (package, source, branch, environment, plane_kind, profile) in namespaces {
            let plane = match plane_kind {
                0 if profile.is_empty() => AuthorityPlane::PackageMetadata,
                1 => AuthorityPlane::semantic_profile(profile)
                    .map_err(|_| AuthorityError::CorruptRecord("plane"))?,
                _ => return Err(AuthorityError::CorruptRecord("plane")),
            };
            let namespace =
                AuthorityNamespace::with_plane(package, source, branch, environment, plane)
                    .map_err(|_| AuthorityError::CorruptRecord("namespace"))?;
            selected.push(
                read_selected_frontier(&tx, &namespace)
                    .await?
                    .ok_or(AuthorityError::CorruptRecord("selected_frontier"))?,
            );
        }
        tx.rollback().await?;
        Ok(selected.into_boxed_slice())
    }

    /// Cold-reopens one immutable generation from the append-only history.
    pub async fn selected_generation(
        &self,
        namespace: &AuthorityNamespace,
        generation: u64,
    ) -> Result<Option<SelectedGeneration>, AuthorityError> {
        let generation = u64_to_i64(generation)?;
        let tx = self.connection.unchecked_transaction().await?;
        let selected = read_selected_generation(&tx, namespace, generation).await?;
        tx.rollback().await?;
        Ok(selected)
    }

    /// Reads a bounded ascending page of immutable selected-history events.
    pub async fn selected_generations(
        &self,
        namespace: &AuthorityNamespace,
        after_generation: Option<u64>,
        limit: usize,
    ) -> Result<Box<[SelectedGeneration]>, AuthorityError> {
        const MAX_PAGE: usize = 512;
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(AuthorityError::InvalidHistoryPageLimit);
        }
        let after_generation = u64_to_i64(after_generation.unwrap_or(0))?;
        let limit = i64::try_from(limit).map_err(|_| AuthorityError::IntegerOverflow)?;
        let tx = self.connection.unchecked_transaction().await?;
        let mut rows = tx
            .query(
                "SELECT generation FROM backend_index_authority_generation_history \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND generation>?7 \
                 ORDER BY generation ASC LIMIT ?8",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    after_generation,
                    limit
                ],
            )
            .await?;
        let mut generations = Vec::new();
        while let Some(row) = rows.next().await? {
            generations.push(row.get::<i64>(0)?);
        }
        drop(rows);
        let mut selected = Vec::with_capacity(generations.len());
        for generation in generations {
            selected.push(
                read_selected_generation(&tx, namespace, generation)
                    .await?
                    .ok_or(AuthorityError::CorruptRecord("generation_history"))?,
            );
        }
        tx.rollback().await?;
        Ok(selected.into_boxed_slice())
    }

    /// Marks one projection current only for the exact selected generation/root.
    ///
    /// Repeating the exact current mark is idempotent. A mark for an older root
    /// fails so a delayed projection worker cannot clear newer lag state.
    pub async fn mark_projection_current(
        &mut self,
        namespace: &AuthorityNamespace,
        projector: ProjectionKind,
        generation: u64,
        root: AuthorityHash,
    ) -> Result<ProjectionWatermark, AuthorityError> {
        let generation = u64_to_i64(generation)?;
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let changed = tx
            .execute(
                "UPDATE backend_index_authority_projection_watermarks SET \
                    projected_generation=selected_generation, projected_root=selected_root, state=1 \
                 WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
                   AND plane_kind=?5 AND profile=?6 AND projector=?7 \
                   AND selected_generation=?8 AND selected_root=?9 AND state=0",
                turso::params![
                    namespace.package.as_ref(),
                    namespace.source.as_ref(),
                    namespace.branch.as_ref(),
                    namespace.environment.as_ref(),
                    namespace.plane.sql_parts().0,
                    namespace.plane.sql_parts().1,
                    projector.as_sql(),
                    generation,
                    root.as_slice()
                ],
            )
            .await?;
        let watermark = read_projection_watermark(&tx, namespace, projector).await?;
        if changed != 1 && !watermark.is_current() {
            tx.rollback().await?;
            return Err(AuthorityError::StaleFrontier);
        }
        if watermark.selected_generation != i64_to_u64(generation, "generation")?
            || watermark.selected_root != root
        {
            tx.rollback().await?;
            return Err(AuthorityError::StaleFrontier);
        }
        tx.commit().await?;
        Ok(watermark)
    }
}

struct SelectedCompilerMetadataVerifier<'store> {
    store: &'store FileStore,
    selected: SelectedGeneration,
    expected: ReopenedCompilerMetadata,
}

impl DurableClosureVerifier for SelectedCompilerMetadataVerifier<'_> {
    type Error = String;

    fn verify_closure(&self, claim: &ClosureClaim) -> Result<(), Self::Error> {
        if *claim != ClosureClaim::for_selected_generation(&self.selected) {
            return Err("retained compiler claim differs from selected history".to_owned());
        }
        let observed = reopen_selected_compiler_metadata(self.store, &self.selected)?;
        if observed != self.expected {
            return Err("retained compiler metadata changed after reopen".to_owned());
        }
        Ok(())
    }
}

async fn ensure_scope(
    connection: &turso::Connection,
    namespace: &AuthorityNamespace,
) -> Result<(), AuthorityError> {
    connection
        .execute(
            "INSERT OR IGNORE INTO backend_index_authority_scopes(\
                package, source, branch, environment, plane_kind, profile, attempt_epoch, latest_observation\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0)",
            namespace_params(namespace),
        )
        .await?;
    Ok(())
}

fn namespace_params(namespace: &AuthorityNamespace) -> [turso::Value; 6] {
    let (plane_kind, profile) = namespace.plane.sql_parts();
    [
        namespace.package.as_ref().into(),
        namespace.source.as_ref().into(),
        namespace.branch.as_ref().into(),
        namespace.environment.as_ref().into(),
        plane_kind.into(),
        profile.into(),
    ]
}

async fn random_attempt_id(connection: &turso::Connection) -> Result<[u8; 16], AuthorityError> {
    let mut rows = connection.query("SELECT randomblob(16)", ()).await?;
    let row = rows
        .next()
        .await?
        .ok_or(AuthorityError::CorruptRecord("attempt_id"))?;
    let bytes: Vec<u8> = row.get(0)?;
    bytes
        .try_into()
        .map_err(|_| AuthorityError::CorruptRecord("attempt_id"))
}

fn derive_attempt_fence(
    namespace: &AuthorityNamespace,
    epoch: i64,
    attempt_id: [u8; 16],
    input_digest: AuthorityHash,
    base_generation: i64,
    base_root: Option<AuthorityHash>,
    observation_sequence: i64,
) -> AuthorityHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.turso.index-attempt-fence.v1\0");
    hasher.update(&namespace.namespace_digest());
    hasher.update(&epoch.to_le_bytes());
    hasher.update(&attempt_id);
    hasher.update(&input_digest);
    hasher.update(&base_generation.to_le_bytes());
    match base_root {
        Some(root) => {
            hasher.update(&[1]);
            hasher.update(&root);
        }
        None => {
            hasher.update(&[0]);
        }
    };
    hasher.update(&observation_sequence.to_le_bytes());
    let mut fence = *hasher.finalize().as_bytes();
    if fence == [0; 32] {
        fence[0] = 1;
    }
    fence
}

fn derive_no_result_barrier_work_id(
    namespace: &AuthorityNamespace,
    terminal_work_id: [u8; 16],
    terminal_epoch: u64,
    terminal_fence: AuthorityHash,
    barrier_epoch: u64,
) -> [u8; 16] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.turso.no-result-barrier-work.v1\0");
    hasher.update(&namespace.namespace_digest());
    hasher.update(&terminal_work_id);
    hasher.update(&terminal_epoch.to_le_bytes());
    hasher.update(&terminal_fence);
    hasher.update(&barrier_epoch.to_le_bytes());
    let digest = hasher.finalize();
    let mut work_id = [0; 16];
    work_id.copy_from_slice(&digest.as_bytes()[..16]);
    if work_id == [0; 16] {
        work_id[0] = 1;
    }
    work_id
}

fn derive_no_result_barrier_fence(
    namespace: &AuthorityNamespace,
    terminal_work_id: [u8; 16],
    terminal_epoch: u64,
    terminal_fence: AuthorityHash,
    barrier_epoch: u64,
) -> AuthorityHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.turso.no-result-barrier-fence.v1\0");
    hasher.update(&namespace.namespace_digest());
    hasher.update(&terminal_work_id);
    hasher.update(&terminal_epoch.to_le_bytes());
    hasher.update(&terminal_fence);
    hasher.update(&barrier_epoch.to_le_bytes());
    let mut fence = *hasher.finalize().as_bytes();
    if fence == [0; 32] {
        fence[0] = 1;
    }
    fence
}

async fn read_head_token(
    connection: &turso::Connection,
    namespace: &AuthorityNamespace,
) -> Result<(i64, Option<AuthorityHash>), AuthorityError> {
    let mut rows = connection
        .query(
            "SELECT generation, target_root FROM backend_index_authority_frontiers \
             WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
               AND plane_kind=?5 AND profile=?6",
            namespace_params(namespace),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok((0, None));
    };
    let generation: i64 = row.get(0)?;
    let root: Vec<u8> = row.get(1)?;
    Ok((generation, Some(decode_hash(root, "target_root")?)))
}

async fn read_selected_frontier(
    connection: &turso::Connection,
    namespace: &AuthorityNamespace,
) -> Result<Option<SelectedFrontier>, AuthorityError> {
    let mut rows = connection
        .query(
            "SELECT generation, candidate_id, attempt_id, attempt_epoch, attempt_fence, input_digest, \
                    target_root, pack_id, closure_id, semantic_manifest_root, observation_sequence, selection_origin \
             FROM backend_index_authority_frontiers \
             WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
               AND plane_kind=?5 AND profile=?6",
            namespace_params(namespace),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let generation: i64 = row.get(0)?;
    let candidate_id: Vec<u8> = row.get(1)?;
    let attempt_id: Vec<u8> = row.get(2)?;
    let attempt_epoch: i64 = row.get(3)?;
    let attempt_fence: Vec<u8> = row.get(4)?;
    let input_digest: Vec<u8> = row.get(5)?;
    let target_root: Vec<u8> = row.get(6)?;
    let pack_id: Vec<u8> = row.get(7)?;
    let closure_id: Vec<u8> = row.get(8)?;
    let semantic_manifest_root: Option<Vec<u8>> = row.get(9)?;
    let observation_sequence: i64 = row.get(10)?;
    let selection_origin = SelectionOrigin::from_sql(row.get(11)?)?;
    drop(rows);
    let observation = read_observation(connection, namespace, observation_sequence).await?;
    let projections = read_projection_watermarks(connection, namespace).await?;
    Ok(Some(SelectedFrontier::new(
        namespace.clone(),
        i64_to_u64(generation, "generation")?,
        decode_hash(candidate_id, "candidate_id")?,
        decode_array::<16>(attempt_id, "attempt_id")?,
        i64_to_u64(attempt_epoch, "attempt_epoch")?,
        decode_hash(attempt_fence, "attempt_fence")?,
        decode_hash(input_digest, "input_digest")?,
        decode_hash(target_root, "target_root")?,
        decode_hash(pack_id, "pack_id")?,
        decode_hash(closure_id, "closure_id")?,
        decode_optional_hash(semantic_manifest_root, "semantic_manifest_root")?,
        observation,
        projections.into_boxed_slice(),
        selection_origin,
    )))
}

async fn read_selected_generation(
    connection: &turso::Connection,
    namespace: &AuthorityNamespace,
    generation: i64,
) -> Result<Option<SelectedGeneration>, AuthorityError> {
    let mut rows = connection
        .query(
            "SELECT candidate_id, attempt_id, attempt_epoch, attempt_fence, input_digest, \
                    target_root, pack_id, closure_id, semantic_manifest_root, observation_sequence, selection_origin \
             FROM backend_index_authority_generation_history \
             WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
               AND plane_kind=?5 AND profile=?6 AND generation=?7",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                generation
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let candidate_id: Vec<u8> = row.get(0)?;
    let attempt_id: Vec<u8> = row.get(1)?;
    let attempt_epoch: i64 = row.get(2)?;
    let attempt_fence: Vec<u8> = row.get(3)?;
    let input_digest: Vec<u8> = row.get(4)?;
    let target_root: Vec<u8> = row.get(5)?;
    let pack_id: Vec<u8> = row.get(6)?;
    let closure_id: Vec<u8> = row.get(7)?;
    let semantic_manifest_root: Option<Vec<u8>> = row.get(8)?;
    let observation_sequence: i64 = row.get(9)?;
    let selection_origin = SelectionOrigin::from_sql(row.get(10)?)?;
    drop(rows);
    let observation = read_observation(connection, namespace, observation_sequence).await?;
    Ok(Some(SelectedGeneration::new(
        namespace.clone(),
        i64_to_u64(generation, "generation")?,
        decode_hash(candidate_id, "candidate_id")?,
        decode_array::<16>(attempt_id, "attempt_id")?,
        i64_to_u64(attempt_epoch, "attempt_epoch")?,
        decode_hash(attempt_fence, "attempt_fence")?,
        decode_hash(input_digest, "input_digest")?,
        decode_hash(target_root, "target_root")?,
        decode_hash(pack_id, "pack_id")?,
        decode_hash(closure_id, "closure_id")?,
        decode_optional_hash(semantic_manifest_root, "semantic_manifest_root")?,
        observation,
        selection_origin,
    )))
}

async fn read_observation(
    connection: &turso::Connection,
    namespace: &AuthorityNamespace,
    sequence: i64,
) -> Result<SourceObservationReceipt, AuthorityError> {
    let mut rows = connection
        .query(
            "SELECT revision, observed_at_ms, value_kind, known_count, unavailable_reason \
             FROM backend_index_authority_observations \
             WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
               AND plane_kind=?5 AND profile=?6 AND sequence=?7",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                sequence
            ],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(AuthorityError::CorruptRecord("source_observation"))?;
    let revision: Option<Vec<u8>> = row.get(0)?;
    let observed_at_ms: i64 = row.get(1)?;
    let kind: i64 = row.get(2)?;
    let count: Option<i64> = row.get(3)?;
    let reason: Option<String> = row.get(4)?;
    let value = match (kind, count, reason) {
        (1, Some(count), None) => {
            SourceObservationValue::KnownCount(i64_to_u64(count, "known_count")?)
        }
        (2, None, None) => SourceObservationValue::Unknown,
        (3, None, Some(reason)) => SourceObservationValue::Unavailable(reason.into_boxed_str()),
        _ => return Err(AuthorityError::CorruptRecord("source_observation_value")),
    };
    let observation = SourceObservation::new(
        namespace.clone(),
        revision
            .map(|revision| decode_hash(revision, "revision"))
            .transpose()?,
        i64_to_u64(observed_at_ms, "observed_at_ms")?,
        value,
    )?;
    Ok(SourceObservationReceipt::new(
        observation,
        i64_to_u64(sequence, "observation_sequence")?,
    ))
}

async fn read_projection_watermarks(
    connection: &turso::Connection,
    namespace: &AuthorityNamespace,
) -> Result<Vec<ProjectionWatermark>, AuthorityError> {
    let mut rows = connection
        .query(
            "SELECT projector, selected_generation, selected_root, projected_generation, \
                    projected_root, state \
             FROM backend_index_authority_projection_watermarks \
             WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
               AND plane_kind=?5 AND profile=?6 ORDER BY projector",
            namespace_params(namespace),
        )
        .await?;
    let mut output = Vec::with_capacity(3);
    while let Some(row) = rows.next().await? {
        let projector: String = row.get(0)?;
        let selected_generation: i64 = row.get(1)?;
        let selected_root: Vec<u8> = row.get(2)?;
        let projected_generation: Option<i64> = row.get(3)?;
        let projected_root: Option<Vec<u8>> = row.get(4)?;
        let state: i64 = row.get(5)?;
        let watermark = ProjectionWatermark::new(
            ProjectionKind::from_sql(&projector)?,
            i64_to_u64(selected_generation, "selected_generation")?,
            decode_hash(selected_root, "selected_root")?,
            projected_generation
                .map(|generation| i64_to_u64(generation, "projected_generation"))
                .transpose()?,
            projected_root
                .map(|root| decode_hash(root, "projected_root"))
                .transpose()?,
        );
        if (state == 1) != watermark.is_current() || !matches!(state, 0 | 1) {
            return Err(AuthorityError::CorruptRecord("projection_state"));
        }
        output.push(watermark);
    }
    if output.len() != 3 {
        return Err(AuthorityError::CorruptRecord("projection_watermarks"));
    }
    Ok(output)
}

async fn read_projection_watermark(
    connection: &turso::Connection,
    namespace: &AuthorityNamespace,
    projector: ProjectionKind,
) -> Result<ProjectionWatermark, AuthorityError> {
    let mut rows = connection
        .query(
            "SELECT selected_generation, selected_root, projected_generation, projected_root, state \
             FROM backend_index_authority_projection_watermarks \
             WHERE package=?1 AND source=?2 AND branch=?3 AND environment=?4 \
               AND plane_kind=?5 AND profile=?6 AND projector=?7",
            turso::params![
                namespace.package.as_ref(),
                namespace.source.as_ref(),
                namespace.branch.as_ref(),
                namespace.environment.as_ref(),
                namespace.plane.sql_parts().0,
                namespace.plane.sql_parts().1,
                projector.as_sql()
            ],
        )
        .await?;
    let row = rows.next().await?.ok_or(AuthorityError::StaleFrontier)?;
    let selected_generation: i64 = row.get(0)?;
    let selected_root: Vec<u8> = row.get(1)?;
    let projected_generation: Option<i64> = row.get(2)?;
    let projected_root: Option<Vec<u8>> = row.get(3)?;
    let state: i64 = row.get(4)?;
    let watermark = ProjectionWatermark::new(
        projector,
        i64_to_u64(selected_generation, "selected_generation")?,
        decode_hash(selected_root, "selected_root")?,
        projected_generation
            .map(|generation| i64_to_u64(generation, "projected_generation"))
            .transpose()?,
        projected_root
            .map(|root| decode_hash(root, "projected_root"))
            .transpose()?,
    );
    if (state == 1) != watermark.is_current() || !matches!(state, 0 | 1) {
        return Err(AuthorityError::CorruptRecord("projection_state"));
    }
    Ok(watermark)
}

fn encode_observation_value(
    value: &SourceObservationValue,
) -> Result<(i64, Option<i64>, Option<&str>), AuthorityError> {
    match value {
        SourceObservationValue::KnownCount(count) => Ok((1, Some(u64_to_i64(*count)?), None)),
        SourceObservationValue::Unknown => Ok((2, None, None)),
        SourceObservationValue::Unavailable(reason) => Ok((3, None, Some(reason))),
    }
}

fn decode_optional_hash(
    value: Option<Vec<u8>>,
    field: &'static str,
) -> Result<Option<AuthorityHash>, AuthorityError> {
    value.map(|bytes| decode_hash(bytes, field)).transpose()
}

fn decode_hash(bytes: Vec<u8>, field: &'static str) -> Result<AuthorityHash, AuthorityError> {
    bytes
        .try_into()
        .map_err(|_| AuthorityError::CorruptRecord(field))
}

fn decode_array<const N: usize>(
    bytes: Vec<u8>,
    field: &'static str,
) -> Result<[u8; N], AuthorityError> {
    bytes
        .try_into()
        .map_err(|_| AuthorityError::CorruptRecord(field))
}

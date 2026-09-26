//! Publication transactions for aligning the projection with immutable roots.

use super::*;

impl TursoProjection {
    /// Aligns the database with a complete immutable root.
    ///
    /// The exact-root fast path performs one metadata read and no writes.
    /// A different root keeps every stored row whose content hash still
    /// matches and writes only the identities that appeared, changed, or
    /// disappeared. Rebuild is reserved for first boot, recovery, or a
    /// missed transition.
    ///
    /// # Errors
    ///
    /// Returns an error when Turso rejects the atomic rebuild or the row count
    /// cannot be represented by the projection schema.
    pub async fn synchronize(
        &mut self,
        view: &ViewRoot,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let expected = self.metadata().await?;
        if let Some(metadata) = expected.as_ref()
            && metadata.root.as_slice() == view.root().as_bytes()
            && metadata.view_version.as_slice() == view.version().as_bytes()
        {
            return Ok(ProjectionUpdate::Reused {
                rows: metadata.row_count.cast_unsigned(),
            });
        }

        let row_count =
            i64::try_from(view.row_count()).map_err(|_| ProjectionError::RowCountOverflow)?;
        // Acquire the one Turso writer lane before touching rows. The
        // metadata check is repeated inside this transaction so a writer that
        // waited behind another publisher returns a typed stale transition and
        // performs zero row work.
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let observed = metadata_from(&tx).await?;
        if let Some(current) = observed.as_ref()
            && current.root.as_slice() == view.root().as_bytes()
            && current.view_version.as_slice() == view.version().as_bytes()
        {
            let rows = current.row_count.cast_unsigned();
            tx.rollback().await?;
            return Ok(ProjectionUpdate::Reused { rows });
        }
        // Any other change since the pre-transaction read means a peer
        // published a different root while this rebuild waited. Publishing
        // now could overwrite a newer root with a stale complete one.
        if observed != expected {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        }
        let changed_rows = if view.rows().is_empty() {
            tx.execute("DELETE FROM backend_projection_rows", ())
                .await?
        } else {
            let stored = stored_row_hashes(&tx).await?;
            let (desired, due, changed_rows) = projection_mutations(&stored, view.rows());
            for rows in due.chunks(REBUILD_BATCH_ROWS) {
                upsert_rows(&tx, rows).await?;
            }
            delete_absent_rows(&tx, &desired).await?;
            changed_rows
        };
        // Each writing statement can create one immutable FTS segment. Compact
        // a large rebuild once before publishing the root fence. Unchanged
        // hashes never enter that write, and hot one-row deltas stay append-only.
        if view.row_count() > REBUILD_BATCH_ROWS as u64 {
            tx.execute("OPTIMIZE INDEX backend_projection_rows_fts", ())
                .await?;
        }
        tx.execute(
            "INSERT INTO backend_projection_meta \
             (singleton, schema_version, root, view_version, row_count) \
             VALUES (1, ?1, ?2, ?3, ?4) \
             ON CONFLICT(singleton) DO UPDATE SET \
             schema_version=excluded.schema_version, root=excluded.root, \
             view_version=excluded.view_version, row_count=excluded.row_count",
            turso::params![
                SCHEMA_VERSION,
                view.root().as_bytes().as_slice(),
                view.version().as_bytes().as_slice(),
                row_count
            ],
        )
        .await?;
        let changed_rows =
            i64::try_from(changed_rows).map_err(|_| ProjectionError::RowCountOverflow)?;
        record_commit(&tx, view.root().as_bytes(), None, None, changed_rows).await?;
        prune_commits(&tx).await?;
        tx.commit().await?;
        Ok(ProjectionUpdate::Rebuilt {
            rows: view.row_count(),
        })
    }

    /// Applies one checked view transition against the exact cached base root.
    ///
    /// Reset events intentionally use [`Self::synchronize`]. All ordinary row
    /// changes update one row and the root fence in the same transaction.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::StaleTransition`] when the database does not
    /// name the transition's exact base, or a database error if the atomic
    /// update fails.
    pub async fn apply(
        &mut self,
        delta: &CommittedViewDelta,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        self.apply_all(std::slice::from_ref(delta)).await
    }

    /// Applies a contiguous checked transition chain in one SQL transaction.
    ///
    /// The first base and final target form the database fence. Intermediate
    /// roots remain in the canonical view journal, while SQL avoids a commit
    /// and metadata rewrite per changed row.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::StaleTransition`] when an empty chain has no
    /// initialized projection, when a nonempty chain is discontinuous, or when
    /// it starts at a different cached root. A chain whose target fence is
    /// already committed is replay-safe and returns
    /// [`ProjectionUpdate::Reused`].
    pub async fn apply_all(
        &mut self,
        deltas: &[CommittedViewDelta],
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let Some(first) = deltas.first() else {
            let metadata = self
                .metadata()
                .await?
                .ok_or(ProjectionError::StaleTransition)?;
            return Ok(ProjectionUpdate::Reused {
                rows: metadata.row_count.cast_unsigned(),
            });
        };
        let last = deltas.last().ok_or(ProjectionError::StaleTransition)?;
        if deltas.windows(2).any(|pair| {
            pair[0].target_root() != pair[1].base_root()
                || pair[0].target_version() != pair[1].base_version()
        }) {
            return Err(ProjectionError::StaleTransition);
        }
        if deltas
            .iter()
            .any(|delta| matches!(delta.delta(), ViewDelta::Reset { .. }))
        {
            return self.synchronize(last.target_view()).await;
        }
        let Some(metadata) = self.metadata().await? else {
            return Err(ProjectionError::StaleTransition);
        };
        // A retried receipt whose target fence already committed is a no-op,
        // not a stale base: replaying it must not mutate rows or force a
        // rebuild.
        if metadata.root.as_slice() == last.target_root().as_bytes()
            && metadata.view_version.as_slice() == last.target_version().as_bytes()
        {
            return Ok(ProjectionUpdate::Reused {
                rows: metadata.row_count.cast_unsigned(),
            });
        }
        if metadata.root.as_slice() != first.base_root().as_bytes()
            || metadata.view_version.as_slice() != first.base_version().as_bytes()
        {
            return Err(ProjectionError::StaleTransition);
        }

        let target = last.target_view();
        let target_count =
            i64::try_from(target.row_count()).map_err(|_| ProjectionError::RowCountOverflow)?;
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let Some(current) = metadata_from(&tx).await? else {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        };
        if current.root.as_slice() != first.base_root().as_bytes()
            || current.view_version.as_slice() != first.base_version().as_bytes()
        {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        }
        let mut changed_rows = 0_u64;
        for delta in deltas {
            let affected = apply_delta(&tx, delta.delta()).await?;
            changed_rows = changed_rows.saturating_add(affected);
        }
        let fenced = tx
            .execute(
                "UPDATE backend_projection_meta SET root=?1, view_version=?2, row_count=?3 \
                 WHERE singleton=1 AND schema_version=?4 AND root=?5 AND view_version=?6",
                turso::params![
                    target.root().as_bytes().as_slice(),
                    target.version().as_bytes().as_slice(),
                    target_count,
                    SCHEMA_VERSION,
                    first.base_root().as_bytes().as_slice(),
                    first.base_version().as_bytes().as_slice()
                ],
            )
            .await?;
        if fenced != 1 {
            return Err(ProjectionError::StaleTransition);
        }
        let changed_rows_i64 =
            i64::try_from(changed_rows).map_err(|_| ProjectionError::RowCountOverflow)?;
        let delta_id = (deltas.len() == 1).then(|| first.id());
        record_commit(
            &tx,
            target.root().as_bytes(),
            Some(first.base_root().as_bytes()),
            delta_id
                .as_ref()
                .map(backend_library::ViewDeltaId::as_bytes),
            changed_rows_i64,
        )
        .await?;
        prune_commits(&tx).await?;
        tx.commit().await?;
        Ok(ProjectionUpdate::Advanced { changed_rows })
    }
}

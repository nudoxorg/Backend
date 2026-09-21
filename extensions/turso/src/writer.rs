//! Version-fenced publication transactions and row encoding.

use crate::read::metadata_from;
use crate::schema::{MAX_AUDIT_ROOTS, SCHEMA_VERSION, UPSERT_ROW};
use crate::{ProjectionError, ProjectionUpdate, TursoProjection};
use backend_library::{
    CommittedViewDelta, Fragment, Row, RowChange, RowId, RowState, ViewDelta, ViewRoot,
};

impl TursoProjection {
    /// Aligns the database with a complete immutable root.
    ///
    /// The exact-root fast path performs one metadata read and no writes.
    /// Rebuild is reserved for first boot, recovery, or a missed transition.
    ///
    /// # Errors
    ///
    /// Returns an error when Turso rejects the atomic rebuild or the row count
    /// cannot be represented by the projection schema.
    pub async fn synchronize(
        &mut self,
        view: &ViewRoot,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        if let Some(metadata) = self.metadata().await?
            && metadata.root.as_slice() == view.root().as_bytes()
            && metadata.view_version.as_slice() == view.version().as_bytes()
        {
            return Ok(ProjectionUpdate::Reused {
                rows: u64::try_from(metadata.row_count).unwrap_or(0),
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
        if let Some(current) = metadata_from(&tx).await?
            && current.root.as_slice() == view.root().as_bytes()
            && current.view_version.as_slice() == view.version().as_bytes()
        {
            tx.rollback().await?;
            return Ok(ProjectionUpdate::Reused {
                rows: u64::try_from(current.row_count).unwrap_or(0),
            });
        }
        tx.execute("DELETE FROM backend_projection_rows", ())
            .await?;
        for row in view.rows() {
            upsert_row(&tx, row).await?;
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
        record_commit(&tx, view.root().as_bytes(), None, None, row_count).await?;
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
    /// Returns [`ProjectionError::StaleTransition`] when the chain is empty,
    /// discontinuous, or starts at a different cached root.
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
                rows: u64::try_from(metadata.row_count).unwrap_or(0),
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

async fn apply_delta(
    connection: &turso::Connection,
    delta: &ViewDelta,
) -> Result<u64, ProjectionError> {
    match delta {
        ViewDelta::Upsert { row } => Ok(upsert_row(connection, row).await?.min(1)),
        ViewDelta::Remove { id } => Ok(connection
            .execute(
                "DELETE FROM backend_projection_rows WHERE row_id = ?1",
                [id.stable_key()],
            )
            .await?
            .min(1)),
        ViewDelta::Coverage { .. } => Ok(0),
        ViewDelta::Patch { changes } => {
            let mut affected = 0_u64;
            for change in changes.iter() {
                let changed = match change {
                    RowChange::Upsert(row) => upsert_row(connection, row).await?.min(1),
                    RowChange::Remove(id) => connection
                        .execute(
                            "DELETE FROM backend_projection_rows WHERE row_id = ?1",
                            [id.stable_key()],
                        )
                        .await?
                        .min(1),
                };
                affected = affected.saturating_add(changed);
            }
            Ok(affected)
        }
        ViewDelta::Reset { .. } => Err(ProjectionError::StaleTransition),
    }
}

async fn upsert_row(connection: &turso::Connection, row: &Row) -> turso::Result<u64> {
    let (kind, id) = row_identity(row.id);
    let state = match row.state {
        RowState::Ready => 0_i64,
        RowState::Loading => 1_i64,
        RowState::Failed => 2_i64,
    };
    let score = row
        .score
        .map(i64::from)
        .map_or(turso::Value::Null, turso::Value::Integer);
    let package = row.package.map_or(turso::Value::Null, |value| {
        turso::Value::Text(backend_library::encode_id(value.as_bytes()))
    });
    let parent = row.parent.map_or(turso::Value::Null, |value| {
        turso::Value::Text(backend_library::encode_id(value.as_bytes()))
    });
    let signature = row.signature.as_ref().map_or(turso::Value::Null, |value| {
        turso::Value::Text(value.clone())
    });
    connection
        .execute(
            UPSERT_ROW,
            turso::params![
                id,
                kind,
                row_hash(row).as_bytes().as_slice(),
                state,
                row.label.as_str(),
                score,
                package,
                parent,
                signature,
                render_document(&row.document)
            ],
        )
        .await
}

async fn record_commit(
    connection: &turso::Connection,
    root: &[u8; 32],
    parent: Option<&[u8; 32]>,
    delta: Option<&[u8; 32]>,
    changed_rows: i64,
) -> turso::Result<()> {
    let parent = parent.map_or(turso::Value::Null, |value| {
        turso::Value::Blob(value.to_vec())
    });
    let delta = delta.map_or(turso::Value::Null, |value| {
        turso::Value::Blob(value.to_vec())
    });
    connection
        .execute(
            "INSERT OR IGNORE INTO backend_projection_commits \
             (root, parent_root, delta_id, changed_rows) VALUES (?1, ?2, ?3, ?4)",
            turso::params![root.as_slice(), parent, delta, changed_rows],
        )
        .await?;
    Ok(())
}

async fn prune_commits(connection: &turso::Connection) -> turso::Result<()> {
    connection
        .execute(
            "DELETE FROM backend_projection_commits WHERE sequence <= \
             (SELECT COALESCE(MAX(sequence), 0) - ?1 FROM backend_projection_commits)",
            [MAX_AUDIT_ROOTS],
        )
        .await?;
    Ok(())
}

fn row_identity(id: RowId) -> (i64, String) {
    let kind = match id {
        RowId::Package(_) => 0,
        RowId::Symbol(_) => 1,
        RowId::Object(_) => 2,
    };
    (kind, id.stable_key())
}

fn row_hash(row: &Row) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.turso.row.v1\0");
    hash_field(&mut hasher, row.id.stable_key().as_bytes());
    hash_field(&mut hasher, row.basis.root.as_bytes());
    hash_field(&mut hasher, row.basis.object.as_bytes());
    hash_field(&mut hasher, row.basis.branch.as_bytes());
    hash_field(&mut hasher, row.basis.log.as_bytes());
    hash_field(&mut hasher, &row.basis.schema.to_be_bytes());
    hash_field(
        &mut hasher,
        &[match row.state {
            RowState::Ready => 0,
            RowState::Loading => 1,
            RowState::Failed => 2,
        }],
    );
    hash_field(&mut hasher, row.label.as_bytes());
    let score = row.score.map(u32::to_be_bytes);
    hash_option(&mut hasher, score.as_ref().map(<[u8; 4]>::as_slice));
    hash_option(
        &mut hasher,
        row.package
            .as_ref()
            .map(|value| value.as_bytes().as_slice()),
    );
    hash_option(
        &mut hasher,
        row.parent.as_ref().map(|value| value.as_bytes().as_slice()),
    );
    hash_option(&mut hasher, row.signature.as_deref().map(str::as_bytes));
    for fragment in &row.document {
        match fragment {
            Fragment::Text(value) => {
                hash_field(&mut hasher, &[0]);
                hash_field(&mut hasher, value.as_bytes());
            }
            Fragment::Code(value) => {
                hash_field(&mut hasher, &[1]);
                hash_field(&mut hasher, value.as_bytes());
            }
            Fragment::Link { label, target } => {
                hash_field(&mut hasher, &[2]);
                hash_field(&mut hasher, label.as_bytes());
                hash_field(&mut hasher, target.as_bytes());
            }
            Fragment::Break => hash_field(&mut hasher, &[3]),
        }
    }
    hasher.finalize()
}

fn hash_option(hasher: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_field(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn render_document(fragments: &[Fragment]) -> String {
    let mut output = String::new();
    for fragment in fragments {
        match fragment {
            Fragment::Text(value) | Fragment::Code(value) => output.push_str(value),
            Fragment::Link { label, .. } => output.push_str(label),
            Fragment::Break => output.push('\n'),
        }
    }
    output
}

//! Version-fenced publication transactions and row encoding.

use crate::read::metadata_from;
use crate::schema::{
    MAX_AUDIT_ROOTS, REBUILD_BATCH_ROWS, SCHEMA_VERSION, UPSERT_COLUMNS, UPSERT_CONFLICT,
    UPSERT_ROW,
};
use crate::{ProjectionError, ProjectionUpdate, TursoProjection};
use backend_library::{
    CommittedViewDelta, Fragment, Row, RowChange, RowId, RowState, ViewDelta, ViewRoot,
};
use std::collections::{BTreeMap, BTreeSet};

impl TursoProjection {
    /// Aligns the database with a complete immutable root.
    ///
    /// The exact-root fast path performs one metadata read and no writes.
    /// A different frontier with the same row-set digest updates the fence
    /// and does not read row hashes. A hot delta clears that digest, so the
    /// next synchronize compares borrowed rows and writes only the identities
    /// that appeared, changed, or disappeared. Rebuild is reserved for first
    /// boot, recovery, or a missed transition.
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
        let row_digest = row_set_digest(view.row_refs());
        let digest_matches = expected
            .as_ref()
            .is_some_and(|metadata| metadata.row_digest.as_deref() == Some(row_digest.as_slice()));
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
        let changed_rows = if digest_matches {
            0
        } else if view.row_count() == 0 {
            tx.execute("DELETE FROM backend_projection_rows", ())
                .await?
        } else {
            let stored = stored_row_hashes(&tx).await?;
            let (desired, due, changed_rows) = projection_mutations(&stored, view.row_refs());
            for rows in due.chunks(REBUILD_BATCH_ROWS) {
                upsert_rows(&tx, rows).await?;
            }
            delete_absent_rows(&tx, &stored, &desired).await?;
            changed_rows
        };
        // Each writing statement can create one immutable FTS segment. Compact
        // a large rebuild once before publishing the root fence. Unchanged
        // hashes never enter that write, and hot one-row deltas stay append-only.
        if changed_rows > 0 && view.row_count() > REBUILD_BATCH_ROWS as u64 {
            tx.execute("OPTIMIZE INDEX backend_projection_rows_fts", ())
                .await?;
        }
        tx.execute(
            "INSERT INTO backend_projection_meta \
             (singleton, schema_version, root, view_version, row_count, row_digest) \
             VALUES (1, ?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(singleton) DO UPDATE SET \
             schema_version=excluded.schema_version, root=excluded.root, \
             view_version=excluded.view_version, row_count=excluded.row_count, \
             row_digest=excluded.row_digest",
            turso::params![
                SCHEMA_VERSION,
                view.root().as_bytes().as_slice(),
                view.version().as_bytes().as_slice(),
                row_count,
                row_digest.as_slice()
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
                "UPDATE backend_projection_meta SET root=?1, view_version=?2, row_count=?3, \
                 row_digest=NULL \
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

async fn stored_row_hashes(
    connection: &turso::Connection,
) -> Result<BTreeMap<String, [u8; 32]>, ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT row_id, content_hash FROM backend_projection_rows",
            (),
        )
        .await?;
    let mut stored = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let hash: Vec<u8> = row.get(1)?;
        let hash: [u8; 32] = hash
            .try_into()
            .map_err(|_| ProjectionError::CorruptMetadata {
                field: "content_hash",
            })?;
        stored.insert(id, hash);
    }
    Ok(stored)
}

fn projection_mutations<'row>(
    stored: &BTreeMap<String, [u8; 32]>,
    rows: impl IntoIterator<Item = &'row Row>,
) -> (BTreeSet<String>, Vec<&'row Row>, u64) {
    let mut desired = BTreeSet::new();
    let mut due = Vec::new();
    let mut changed = 0_u64;
    let mut scratch = Vec::new();
    for row in rows {
        let id = row.id.stable_key();
        let hash = *row_hash_into(row, &mut scratch).as_bytes();
        if stored.get(&id) != Some(&hash) {
            changed = changed.saturating_add(1);
            due.push(row);
        }
        desired.insert(id);
    }
    for id in stored.keys() {
        if !desired.contains(id) {
            changed = changed.saturating_add(1);
        }
    }
    (desired, due, changed)
}

async fn delete_absent_rows(
    connection: &turso::Connection,
    stored: &BTreeMap<String, [u8; 32]>,
    desired: &BTreeSet<String>,
) -> Result<(), ProjectionError> {
    let stale = stored
        .keys()
        .filter(|id| !desired.contains(id.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    for batch in stale.chunks(REBUILD_BATCH_ROWS) {
        let mut sql = String::from("DELETE FROM backend_projection_rows WHERE row_id IN (");
        for index in 0..batch.len() {
            if index != 0 {
                sql.push(',');
            }
            sql.push('?');
        }
        sql.push(')');
        connection.execute(&sql, batch.to_vec()).await?;
    }
    Ok(())
}

async fn upsert_row(connection: &turso::Connection, row: &Row) -> turso::Result<u64> {
    connection.execute(UPSERT_ROW, row_values(row)).await
}

async fn upsert_rows(connection: &turso::Connection, rows: &[&Row]) -> turso::Result<u64> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut sql = String::with_capacity(
        UPSERT_COLUMNS.len() + UPSERT_CONFLICT.len() + rows.len().saturating_mul(23),
    );
    sql.push_str(UPSERT_COLUMNS);
    for index in 0..rows.len() {
        if index != 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?,?,?,?,?,?,?)");
    }
    sql.push_str(UPSERT_CONFLICT);
    let values = rows
        .iter()
        .flat_map(|row| row_values(row))
        .collect::<Vec<_>>();
    connection
        .execute(&sql, turso::params_from_iter(values))
        .await
}

fn row_values(row: &Row) -> [turso::Value; 10] {
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
    [
        turso::Value::Text(id),
        turso::Value::Integer(kind),
        turso::Value::Blob(row_hash(row).as_bytes().to_vec()),
        turso::Value::Integer(state),
        turso::Value::Text(row.label.clone()),
        score,
        package,
        parent,
        signature,
        turso::Value::Text(render_document(&row.document)),
    ]
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

fn row_set_digest<'row>(rows: impl IntoIterator<Item = &'row Row>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    let mut scratch = Vec::new();
    hasher.update(b"backend.turso.row-set.v1\0");
    for row in rows {
        hasher.update(row_hash_into(row, &mut scratch).as_bytes());
    }
    *hasher.finalize().as_bytes()
}

fn row_hash(row: &Row) -> blake3::Hash {
    row_hash_into(row, &mut Vec::new())
}

fn row_hash_into(row: &Row, bytes: &mut Vec<u8>) -> blake3::Hash {
    bytes.clear();
    bytes.extend_from_slice(b"backend.turso.row.v1\0");
    write_field(bytes, row.id.stable_key().as_bytes());
    write_field(bytes, row.basis.root.as_bytes());
    write_field(bytes, row.basis.object.as_bytes());
    write_field(bytes, row.basis.branch.as_bytes());
    write_field(bytes, row.basis.log.as_bytes());
    write_field(bytes, &row.basis.schema.to_be_bytes());
    write_field(
        bytes,
        &[match row.state {
            RowState::Ready => 0,
            RowState::Loading => 1,
            RowState::Failed => 2,
        }],
    );
    write_field(bytes, row.label.as_bytes());
    let score = row.score.map(u32::to_be_bytes);
    write_option(bytes, score.as_ref().map(<[u8; 4]>::as_slice));
    write_option(
        bytes,
        row.package
            .as_ref()
            .map(|value| value.as_bytes().as_slice()),
    );
    write_option(
        bytes,
        row.parent.as_ref().map(|value| value.as_bytes().as_slice()),
    );
    write_option(bytes, row.signature.as_deref().map(str::as_bytes));
    for fragment in &row.document {
        match fragment {
            Fragment::Text(value) => {
                write_field(bytes, &[0]);
                write_field(bytes, value.as_bytes());
            }
            Fragment::Code(value) => {
                write_field(bytes, &[1]);
                write_field(bytes, value.as_bytes());
            }
            Fragment::Link { label, target } => {
                write_field(bytes, &[2]);
                write_field(bytes, label.as_bytes());
                write_field(bytes, target.as_bytes());
            }
            Fragment::Break => write_field(bytes, &[3]),
        }
    }
    blake3::hash(bytes)
}

fn write_option(bytes: &mut Vec<u8>, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            bytes.push(1);
            write_field(bytes, value);
        }
        None => bytes.push(0),
    }
}

fn write_field(bytes: &mut Vec<u8>, value: &[u8]) {
    bytes.extend_from_slice(&(value.len() as u64).to_be_bytes());
    bytes.extend_from_slice(value);
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

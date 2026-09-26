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

mod publish;

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
    rows: &'row [Row],
) -> (BTreeSet<String>, Vec<&'row Row>, u64) {
    let mut last_index = BTreeMap::<String, usize>::new();
    for (index, row) in rows.iter().enumerate() {
        last_index.insert(row.id.stable_key(), index);
    }
    let mut desired = BTreeSet::new();
    let mut due = Vec::new();
    let mut changed = 0_u64;
    for (id, index) in &last_index {
        let row = &rows[*index];
        let hash = *row_hash(row).as_bytes();
        desired.insert(id.clone());
        if stored.get(id) != Some(&hash) {
            changed = changed.saturating_add(1);
            due.push(row);
        }
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
    desired: &BTreeSet<String>,
) -> Result<(), ProjectionError> {
    let mut rows = connection
        .query("SELECT row_id FROM backend_projection_rows", ())
        .await?;
    let mut stale = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        if !desired.contains(&id) {
            stale.push(id);
        }
    }
    drop(rows);
    for id in stale {
        connection
            .execute(
                "DELETE FROM backend_projection_rows WHERE row_id = ?1",
                [id],
            )
            .await?;
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

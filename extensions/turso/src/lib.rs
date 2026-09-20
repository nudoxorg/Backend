//! Root-bound Turso projection for local query acceleration.
//!
//! The versioned workspace remains the authority. This crate owns a disposable
//! SQL projection whose sole mutable row set is bound to one immutable
//! [`ViewRoot`]. Hot transitions update only changed rows in one transaction;
//! cold recovery can rebuild the projection from a certified root. A failed or
//! stale projection therefore cannot advance canonical workspace state.

#![deny(unsafe_code)]

use backend_library::{
    CommittedViewDelta, Fragment, Row, RowChange, RowId, RowState, ViewDelta, ViewRoot,
};
use std::fmt;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i64 = 2;
const MAX_AUDIT_ROOTS: i64 = 128;
const REBUILD_BATCH_ROWS: usize = 512;

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS backend_projection_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    root BLOB NOT NULL,
    view_version BLOB NOT NULL,
    row_count INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS backend_projection_rows (
    row_id TEXT PRIMARY KEY,
    row_kind INTEGER NOT NULL,
    content_hash BLOB NOT NULL,
    state INTEGER NOT NULL,
    label TEXT NOT NULL,
    score INTEGER,
    package_id TEXT,
    parent_id TEXT,
    signature TEXT,
    document TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS backend_projection_rows_label
    ON backend_projection_rows(label);
CREATE INDEX IF NOT EXISTS backend_projection_rows_package
    ON backend_projection_rows(package_id, parent_id, row_id);
CREATE INDEX IF NOT EXISTS backend_projection_rows_fts
    ON backend_projection_rows USING fts (label, signature, document);
CREATE TABLE IF NOT EXISTS backend_projection_commits (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    root BLOB NOT NULL UNIQUE,
    parent_root BLOB,
    delta_id BLOB,
    changed_rows INTEGER NOT NULL
);
";

const UPSERT_ROW: &str = r"
INSERT INTO backend_projection_rows (
    row_id, row_kind, content_hash, state, label, score,
    package_id, parent_id, signature, document
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
ON CONFLICT(row_id) DO UPDATE SET
    row_kind = excluded.row_kind,
    content_hash = excluded.content_hash,
    state = excluded.state,
    label = excluded.label,
    score = excluded.score,
    package_id = excluded.package_id,
    parent_id = excluded.parent_id,
    signature = excluded.signature,
    document = excluded.document
WHERE backend_projection_rows.content_hash != excluded.content_hash
";

const UPSERT_COLUMNS: &str = "INSERT INTO backend_projection_rows (\
    row_id, row_kind, content_hash, state, label, score, \
    package_id, parent_id, signature, document) VALUES ";
const UPSERT_CONFLICT: &str = " ON CONFLICT(row_id) DO UPDATE SET \
    row_kind=excluded.row_kind, content_hash=excluded.content_hash, \
    state=excluded.state, label=excluded.label, score=excluded.score, \
    package_id=excluded.package_id, parent_id=excluded.parent_id, \
    signature=excluded.signature, document=excluded.document \
    WHERE backend_projection_rows.content_hash != excluded.content_hash";

/// Durable path used by the product composition.
pub const FILE_NAME: &str = "projection.turso";

/// Result of aligning the projection to an immutable view root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionUpdate {
    /// The database already named this exact root; no SQL mutation ran.
    Reused {
        /// Number of rows retained without work.
        rows: u64,
    },
    /// A cold or stale database was rebuilt from the supplied root.
    Rebuilt {
        /// Rows materialized from the root.
        rows: u64,
    },
    /// One checked transition advanced the existing projection.
    Advanced {
        /// Rows inserted, replaced, or removed by the transition.
        changed_rows: u64,
    },
}

/// Failure to open, align, or query the derived projection.
#[derive(Debug)]
pub enum ProjectionError {
    /// A database path was not representable as UTF-8.
    NonUtf8Path(PathBuf),
    /// A database operation failed.
    Database(turso::Error),
    /// Persisted projection metadata has an unknown schema.
    Schema {
        /// Schema number read from the projection metadata row.
        found: i64,
    },
    /// A hot transition was offered against a different cached root.
    StaleTransition,
    /// A view size exceeded the SQL integer domain.
    RowCountOverflow,
    /// Persisted projection metadata is structurally invalid.
    CorruptMetadata {
        /// Name of the malformed metadata field.
        field: &'static str,
    },
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonUtf8Path(path) => {
                write!(
                    formatter,
                    "Turso projection path is not UTF-8: {}",
                    path.display()
                )
            }
            Self::Database(error) => error.fmt(formatter),
            Self::Schema { found } => {
                write!(formatter, "unsupported Turso projection schema {found}")
            }
            Self::StaleTransition => {
                formatter.write_str("Turso projection transition has the wrong base root")
            }
            Self::RowCountOverflow => formatter.write_str("projection row count overflows i64"),
            Self::CorruptMetadata { field } => {
                write!(formatter, "projection metadata field {field} is invalid")
            }
        }
    }
}

impl std::error::Error for ProjectionError {}

impl From<turso::Error> for ProjectionError {
    fn from(error: turso::Error) -> Self {
        Self::Database(error)
    }
}

/// One process-owned Turso accelerator.
///
/// `Connection::transaction(&mut self)` makes overlapping writers impossible
/// through this API: a projection update exclusively borrows the connection
/// until its root fence commits.
pub struct TursoProjection {
    _database: turso::Database,
    connection: turso::Connection,
}

impl fmt::Debug for TursoProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TursoProjection")
            .finish_non_exhaustive()
    }
}

impl TursoProjection {
    /// Opens or creates a local projection database and validates its schema.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid path, database failure, or incompatible
    /// on-disk projection schema.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, ProjectionError> {
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| ProjectionError::NonUtf8Path(path.to_path_buf()))?;
        let database = turso::Builder::new_local(text)
            .experimental_index_method(true)
            .build()
            .await?;
        let connection = database.connect()?;
        connection.execute_batch(SCHEMA).await?;
        let projection = Self {
            _database: database,
            connection,
        };
        let _ = projection.metadata().await?;
        Ok(projection)
    }

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
        let expected_metadata = self.metadata().await?;
        if let Some(metadata) = expected_metadata.as_ref()
            && metadata.root.as_slice() == view.root().as_bytes()
            && metadata.view_version.as_slice() == view.version().as_bytes()
        {
            return Ok(ProjectionUpdate::Reused {
                rows: metadata.row_count.cast_unsigned(),
            });
        }

        let row_count =
            i64::try_from(view.row_count()).map_err(|_| ProjectionError::RowCountOverflow)?;
        let tx = self.connection.transaction().await?;
        // `metadata` was read before opening the transaction. Re-check it from
        // the transaction snapshot before deleting rows so a second process
        // cannot publish a stale complete root after the first read raced a
        // writer. A failed fence rolls the whole rebuild back.
        let observed = read_metadata(&tx).await?;
        if !metadata_matches(observed.as_ref(), expected_metadata.as_ref()) {
            return Err(ProjectionError::StaleTransition);
        }
        tx.execute("DELETE FROM backend_projection_rows", ())
            .await?;
        for rows in view.rows().chunks(REBUILD_BATCH_ROWS) {
            upsert_rows(&tx, rows).await?;
        }
        // Each SQL statement creates one immutable Tantivy segment. Compact
        // the bounded rebuild batches once before publishing the root fence;
        // hot one-row deltas remain append-only and avoid global maintenance.
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
        let tx = self.connection.transaction().await?;
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

    /// Searches the materialized rows and returns stable row IDs.
    ///
    /// Results are always accompanied by the exact root that fenced the SQL
    /// snapshot, so callers cannot mistake cache output for another version.
    ///
    /// # Errors
    ///
    /// Returns an error when query execution or metadata decoding fails.
    pub async fn search(&self, text: &str, limit: u32) -> Result<RootedRows, ProjectionError> {
        // Metadata and rows must come from one SQLite snapshot. Reading the
        // fence first and the rows second lets another process commit between
        // the two queries, yielding rows labelled with the wrong root.
        let tx = self.connection.unchecked_transaction().await?;
        let metadata = read_metadata(&tx)
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        let query = fts_query(text);
        let mut rows = match query {
            Some(query) => {
                tx.query(
                    "SELECT row_id FROM backend_projection_rows \
                     WHERE (label, signature, document) MATCH ?1 \
                     ORDER BY row_id LIMIT ?2",
                    turso::params![query, i64::from(limit)],
                )
                .await?
            }
            None => {
                tx.query(
                    "SELECT row_id FROM backend_projection_rows ORDER BY row_id LIMIT ?1",
                    [i64::from(limit)],
                )
                .await?
            }
        };
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get::<String>(0)?);
        }
        let root = metadata.root.into_boxed_slice();
        tx.commit().await?;
        Ok(RootedRows {
            root,
            ids: ids.into_boxed_slice(),
        })
    }

    async fn metadata(&self) -> Result<Option<Metadata>, ProjectionError> {
        read_metadata(&self.connection).await
    }
}

/// Query output fenced by one immutable projection root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootedRows {
    /// Exact projected root.
    pub root: Box<[u8]>,
    /// Stable matching row identities.
    pub ids: Box<[String]>,
}

#[derive(Debug, Eq, PartialEq)]
struct Metadata {
    schema_version: i64,
    root: Vec<u8>,
    view_version: Vec<u8>,
    row_count: i64,
}

async fn read_metadata(
    connection: &turso::Connection,
) -> Result<Option<Metadata>, ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT schema_version, root, view_version, row_count \
             FROM backend_projection_meta WHERE singleton=1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let metadata = Metadata {
        schema_version: row.get(0)?,
        root: row.get(1)?,
        view_version: row.get(2)?,
        row_count: row.get(3)?,
    };
    if metadata.schema_version != SCHEMA_VERSION {
        return Err(ProjectionError::Schema {
            found: metadata.schema_version,
        });
    }
    validate_metadata(&metadata)?;
    Ok(Some(metadata))
}

fn validate_metadata(metadata: &Metadata) -> Result<(), ProjectionError> {
    if metadata.root.len() != 32 {
        return Err(ProjectionError::CorruptMetadata { field: "root" });
    }
    if metadata.view_version.len() != 32 {
        return Err(ProjectionError::CorruptMetadata {
            field: "view_version",
        });
    }
    if metadata.row_count < 0 {
        return Err(ProjectionError::CorruptMetadata { field: "row_count" });
    }
    Ok(())
}

fn metadata_matches(left: Option<&Metadata>, right: Option<&Metadata>) -> bool {
    left == right
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
    connection.execute(UPSERT_ROW, row_values(row)).await
}

async fn upsert_rows(connection: &turso::Connection, rows: &[Row]) -> turso::Result<u64> {
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
    let values = rows.iter().flat_map(row_values).collect::<Vec<_>>();
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

fn fts_query(text: &str) -> Option<String> {
    let tokens = text
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!tokens.is_empty()).then(|| tokens.join(" AND "))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use backend_library::{AuthorityScopeClaim, ProducerObservationVerifier};
    use backend_library::{
        Basis, Coverage, CoverageCapability, Frontier, RowId, ViewDelta, ViewRoot,
        admit_complete_scope, admit_producer_observation, branch_key, log_key, object_version,
        package_key, view_key, view_state_root,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Instant;

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

    struct ExactObservation(backend_library::UntrustedProducerObservation);

    impl ProducerObservationVerifier for ExactObservation {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &backend_library::UntrustedProducerObservation,
        ) -> Result<(), Self::Error> {
            (observation == &self.0).then_some(()).ok_or("mismatch")
        }
    }

    fn capability(object: backend_library::SemanticObject) -> CoverageCapability {
        let declaration = AuthorityScopeClaim::from_object_version(object);
        let raw = backend_library::UntrustedProducerObservation::new(
            [7; 32],
            declaration.scope_root(),
            [8; 32],
            b"projection-test".to_vec(),
        );
        let observation = admit_producer_observation(raw.clone(), &ExactObservation(raw))
            .unwrap_or_else(|error| panic!("observation: {error}"));
        let coverage = admit_complete_scope(declaration, observation)
            .unwrap_or_else(|error| panic!("coverage: {error}"));
        CoverageCapability::from_authorized_with_evidence(coverage, b"projection-test".to_vec())
            .unwrap_or_else(|error| panic!("capability: {error}"))
    }

    fn root(rows: Vec<Row>) -> ViewRoot {
        let source = view_state_root(&[]);
        let object = object_version(b"projection-test");
        let basis = Basis::with_context(source, object, branch_key("main"), log_key("library"), 1);
        ViewRoot::new_checked(
            view_key(b"projection-test"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
            rows,
            vec![Coverage::Complete],
            capability(object),
        )
        .unwrap_or_else(|error| panic!("root: {error:?}"))
    }

    fn path() -> PathBuf {
        let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "backend-turso-projection-{}-{serial}.db",
            std::process::id()
        ))
    }

    #[test]
    fn exact_root_reuses_database_and_hot_delta_changes_one_row() {
        futures_executor::block_on(async {
            let path = path();
            let empty = root(Vec::new());
            let package = package_key("workspace");
            let row = Row::new(RowId::Package(package), empty.basis(), "workspace");
            let prepared = empty
                .prepare(ViewDelta::Upsert { row }, capability(empty.basis().object))
                .unwrap_or_else(|error| panic!("prepare: {error:?}"));
            let (next, committed) = empty
                .clone()
                .commit(prepared)
                .unwrap_or_else(|error| panic!("commit: {error:?}"));

            let mut projection = TursoProjection::open(&path)
                .await
                .unwrap_or_else(|error| panic!("open: {error}"));
            assert_eq!(
                projection
                    .synchronize(&empty)
                    .await
                    .unwrap_or_else(|error| panic!("initial synchronize: {error}")),
                ProjectionUpdate::Rebuilt { rows: 0 }
            );
            assert_eq!(
                projection
                    .synchronize(&empty)
                    .await
                    .unwrap_or_else(|error| panic!("reuse synchronize: {error}")),
                ProjectionUpdate::Reused { rows: 0 }
            );
            assert_eq!(
                projection
                    .apply(&committed)
                    .await
                    .unwrap_or_else(|error| panic!("apply: {error}")),
                ProjectionUpdate::Advanced { changed_rows: 1 }
            );
            // A retried transport receipt is safe to replay after the target
            // fence has committed. The projection must not re-run its row
            // mutation or force a rebuild just because the base is now old.
            assert_eq!(
                projection
                    .apply(&committed)
                    .await
                    .unwrap_or_else(|error| panic!("replay apply: {error}")),
                ProjectionUpdate::Reused { rows: 1 }
            );
            let found = projection
                .search("workspace", 10)
                .await
                .unwrap_or_else(|error| panic!("search: {error}"));
            assert_eq!(found.root.as_ref(), next.root().as_bytes());
            assert_eq!(found.ids.as_ref(), &[RowId::Package(package).stable_key()]);
            drop(projection);

            let mut reopened = TursoProjection::open(&path)
                .await
                .unwrap_or_else(|error| panic!("reopen: {error}"));
            assert_eq!(
                reopened
                    .synchronize(&next)
                    .await
                    .unwrap_or_else(|error| panic!("reopen synchronize: {error}")),
                ProjectionUpdate::Reused { rows: 1 }
            );
            std::fs::remove_file(&path)
                .unwrap_or_else(|error| panic!("remove projection: {error}"));
        });
    }

    #[test]
    #[ignore = "bounded Turso/Tantivy projection stress probe"]
    fn stress_fts_projection_reports_build_query_and_delta_costs() {
        futures_executor::block_on(async {
            const ROWS: usize = 20_000;
            let path = path();
            let empty = root(Vec::new());
            let package = package_key("stress");
            let rows = (0..ROWS)
                .map(|index| {
                    let marker = if index == ROWS - 1 {
                        " singular-needle"
                    } else {
                        ""
                    };
                    Row::in_package(
                        RowId::Symbol(backend_library::symbol_key(&format!(
                            "stress::symbol_{index:05}"
                        ))),
                        empty.basis(),
                        package,
                        format!("symbol_{index:05}"),
                    )
                    .with_signature(format!("fn symbol_{index:05}()"))
                    .with_document(vec![Fragment::Text(format!(
                        "indexed package documentation common-token{marker}"
                    ))])
                })
                .collect::<Vec<_>>();
            let view = root(rows);
            let mut projection = TursoProjection::open(&path)
                .await
                .unwrap_or_else(|error| panic!("open: {error}"));
            let started = Instant::now();
            let update = projection
                .synchronize(&view)
                .await
                .unwrap_or_else(|error| panic!("synchronize: {error}"));
            let build_ms = started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(update, ProjectionUpdate::Rebuilt { rows: ROWS as u64 });

            let started = Instant::now();
            let rare = projection
                .search("singular needle", 10)
                .await
                .unwrap_or_else(|error| panic!("rare search: {error}"));
            let rare_ms = started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(rare.ids.len(), 1);

            let started = Instant::now();
            let common = projection
                .search("common token", 25)
                .await
                .unwrap_or_else(|error| panic!("common search: {error}"));
            let common_ms = started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(common.ids.len(), 25);

            let replacement = Row::in_package(
                RowId::Symbol(backend_library::symbol_key("stress::symbol_10000")),
                view.basis(),
                package,
                "symbol_10000",
            )
            .with_document(vec![Fragment::Text("changed edge".to_owned())]);
            let prepared = view
                .prepare(
                    ViewDelta::Upsert { row: replacement },
                    capability(view.basis().object),
                )
                .unwrap_or_else(|error| panic!("prepare: {error:?}"));
            let (_, committed) = view
                .commit(prepared)
                .unwrap_or_else(|error| panic!("commit: {error:?}"));
            let started = Instant::now();
            let update = projection
                .apply(&committed)
                .await
                .unwrap_or_else(|error| panic!("apply: {error}"));
            let delta_ms = started.elapsed().as_secs_f64() * 1_000.0;
            assert_eq!(update, ProjectionUpdate::Advanced { changed_rows: 1 });

            let bytes = std::fs::metadata(&path)
                .unwrap_or_else(|error| panic!("metadata: {error}"))
                .len();
            eprintln!(
                "turso_fts_stress rows={ROWS} build_ms={build_ms:.2} rare_ms={rare_ms:.3} common_ms={common_ms:.3} one_row_delta_ms={delta_ms:.3} database_bytes={bytes}"
            );
            drop(projection);
            std::fs::remove_file(&path)
                .unwrap_or_else(|error| panic!("remove projection: {error}"));
        });
    }
}

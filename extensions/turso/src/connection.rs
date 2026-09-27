//! Database opening and process-shared connection policy.

use crate::{ProjectionError, TursoProjection, schema};
use std::path::Path;
use std::time::Duration;

/// Bounded wait for another process-owned writer lane.
///
/// A finite busy handler allows another GUI/MCP/CLI writer to finish while
/// still surfacing a wedged peer as an error instead of hanging local work.
pub(crate) const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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
        // Every projection handle may be opened by a different process (the
        // GUI, MCP daemon, and CLI can all observe the same workspace). The
        // default in-process WAL rejects that topology and turns an otherwise
        // safe reader/writer overlap into `database is locked`. Turso's
        // multiprocess WAL keeps immutable read snapshots independent from the
        // single serialized writer lane and persists the coordination state
        // next to the database.
        let database = turso::Builder::new_local(text)
            .experimental_multiprocess_wal(true)
            .experimental_index_method(true)
            .build()
            .await?;
        let connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.execute_batch(schema::SCHEMA).await?;
        // CREATE TABLE IF NOT EXISTS does not add columns to a projection
        // written by an older binary. Read the version from the columns that
        // already existed so an older file is discarded instead of failing as
        // a missing-column query.
        if !meta_has_row_digest(&connection).await? {
            let found = legacy_schema_version(&connection).await?;
            return Err(ProjectionError::Schema { found });
        }
        let projection = Self {
            _database: database,
            connection,
        };
        if let Some(metadata) = projection.metadata().await?
            && metadata.schema_version != schema::SCHEMA_VERSION
        {
            return Err(ProjectionError::Schema {
                found: metadata.schema_version,
            });
        }
        Ok(projection)
    }

    /// Opens the projection, rebuilding one written under an older schema.
    ///
    /// The projection is derived entirely from the authoritative workspace
    /// journal, so an older on-disk schema is discarded rather than migrated
    /// and the caller's next `synchronize` repopulates it. A newer schema is
    /// still refused: an older binary must never destroy a newer file.
    ///
    /// # Errors
    ///
    /// Returns every [`Self::open`] error except an older schema, and
    /// [`ProjectionError::Discard`] when the outdated files cannot be removed.
    pub async fn open_or_rebuild(path: impl AsRef<Path>) -> Result<Self, ProjectionError> {
        let path = path.as_ref();
        match Self::open(path).await {
            Err(ProjectionError::Schema { found }) if found < schema::SCHEMA_VERSION => {
                discard(path)?;
                Self::open(path).await
            }
            opened => opened,
        }
    }
}

async fn meta_has_row_digest(connection: &turso::Connection) -> Result<bool, ProjectionError> {
    let mut rows = connection
        .query("PRAGMA table_info(backend_projection_meta)", ())
        .await?;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if name == "row_digest" {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn legacy_schema_version(connection: &turso::Connection) -> Result<i64, ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT schema_version FROM backend_projection_meta WHERE singleton=1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(0);
    };
    row.get(0).map_err(ProjectionError::from)
}

/// Removes a projection database and the sidecars Turso keeps beside it.
fn discard(path: &Path) -> Result<(), ProjectionError> {
    let mut targets = vec![path.to_path_buf()];
    for suffix in ["-wal", "-shm", "-tshm"] {
        let mut sidecar = path.as_os_str().to_owned();
        sidecar.push(suffix);
        targets.push(sidecar.into());
    }
    for target in targets {
        match std::fs::remove_file(&target) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(ProjectionError::Discard(error));
            }
            _ => {}
        }
    }
    Ok(())
}

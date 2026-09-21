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
            .build()
            .await?;
        let connection = database.connect()?;
        connection.busy_timeout(BUSY_TIMEOUT)?;
        connection.execute_batch(schema::SCHEMA).await?;
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
}

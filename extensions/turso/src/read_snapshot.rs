//! Read-only SQL state captured before the unique projection writer moves.
//!
//! A generation pin is not a snapshot: view deltas mutate that generation's
//! WAL. This capability holds an actual SQLite read transaction, established
//! by decoding its metadata while the ordinary selected-generation guard is
//! held. Later reads use that transaction and never consult the latest
//! namespace selector. They still need the caller's admitted view/facts proof.

use crate::{PackageGraphReadError, PackageGraphRevision, ProjectionError, TursoProjection};
use backend_library::{PackageGraphPage, PackageGraphPageRequest};

/// One exact SQL read snapshot, with no mutation or selector-admission API.
/// The read transaction lives until its connection is retired by the worker.
#[must_use = "retain the captured SQL read state through owner installation"]
pub struct TursoProjectionReadSnapshot {
    _database: turso::Database,
    connection: turso::Connection,
    _generation_directory: backend_platform::DirectoryCapability,
    _generation_pin: std::fs::File,
    revision: PackageGraphRevision,
}

impl TursoProjection {
    /// Opens a separate read-only connection to this already-admitted
    /// database and fixes its read snapshot before returning. No path is
    /// reopened, selector changed, schema written, or writer cloned.
    pub async fn capture_read_snapshot(
        &self,
    ) -> Result<TursoProjectionReadSnapshot, ProjectionError> {
        if self.staging {
            return Err(ProjectionError::StaleTransition);
        }
        let _guard = self.operation_guard()?;
        let connection = self._database.connect()?;
        connection.busy_timeout(crate::connection::BUSY_TIMEOUT)?;
        connection
            .execute_batch("PRAGMA query_only=1; PRAGMA cache_size=256; BEGIN DEFERRED;")
            .await?;
        // BEGIN alone does not acquire a SQLite snapshot. This first actual
        // metadata read does, and validates both row and graph metadata.
        let revision = crate::graph::package_graph_revision_from(
            &connection,
            self.generation,
            self.marker_identity,
        )
        .await?;
        let generation_pin = self
            .generation_pin
            .try_clone()
            .map_err(ProjectionError::Filesystem)?;
        Ok(TursoProjectionReadSnapshot {
            _database: self._database.clone(),
            connection,
            _generation_directory: self.generation_directory.clone(),
            _generation_pin: generation_pin,
            revision,
        })
    }
}

impl TursoProjectionReadSnapshot {
    #[cfg(test)]
    pub(crate) fn connection_for_test(&self) -> &turso::Connection {
        &self.connection
    }

    /// Returns the exact SQL revision observed when the transaction started.
    #[must_use]
    pub const fn package_graph_revision(&self) -> PackageGraphRevision {
        self.revision
    }

    /// Uses the same bounded page decoder as an ordinary selected read, with
    /// the already-held transaction as its fixed SQL source. The product
    /// adapter must still authenticate the result against selected facts.
    pub async fn read_package_graph_page(
        &self,
        request: &PackageGraphPageRequest,
    ) -> Result<PackageGraphPage, PackageGraphReadError> {
        crate::package_graph_read::read_page_snapshot(&self.connection, request).await
    }
}

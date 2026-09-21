//! Consistent read snapshots and query decoding.

use crate::schema::Metadata;
use crate::{ProjectionError, TursoProjection};

/// Query output fenced by one immutable projection root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootedRows {
    /// Exact projected root.
    pub root: Box<[u8]>,
    /// Stable matching row identities.
    pub ids: Box<[String]>,
}

impl TursoProjection {
    /// Searches the materialized rows and returns stable row IDs.
    ///
    /// Results are always accompanied by the exact root that fenced the SQL
    /// snapshot, so callers cannot mistake cache output for another version.
    ///
    /// # Errors
    ///
    /// Returns an error when query execution or metadata decoding fails.
    pub async fn search(&self, text: &str, limit: u32) -> Result<RootedRows, ProjectionError> {
        // Keep the root fence and the rows in one read transaction. A pair of
        // independent statements can otherwise observe `base` for the fence
        // and `target` for the rows when a writer commits between them.
        let tx = self.connection.unchecked_transaction().await?;
        let metadata = metadata_from(&tx)
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        let pattern = format!("%{}%", escape_like(text));
        let mut rows = tx
            .query(
                "SELECT row_id FROM backend_projection_rows \
                 WHERE label LIKE ?1 ESCAPE '\\' OR signature LIKE ?1 ESCAPE '\\' \
                 OR document LIKE ?1 ESCAPE '\\' ORDER BY row_id LIMIT ?2",
                turso::params![pattern, i64::from(limit)],
            )
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get::<String>(0)?);
        }
        drop(rows);
        tx.rollback().await?;
        Ok(RootedRows {
            root: metadata.root.into_boxed_slice(),
            ids: ids.into_boxed_slice(),
        })
    }

    pub(crate) async fn metadata(&self) -> Result<Option<Metadata>, ProjectionError> {
        metadata_from(&self.connection).await
    }
}

pub(crate) async fn metadata_from(
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
    Ok(Some(Metadata {
        schema_version: row.get(0)?,
        root: row.get(1)?,
        view_version: row.get(2)?,
        row_count: row.get(3)?,
    }))
}

pub(crate) fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

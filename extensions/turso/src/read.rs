//! Consistent read snapshots and query decoding.

use crate::schema::{Metadata, SCHEMA_VERSION};
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
        let mut rows = match fts_query(text) {
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

/// Builds an FTS conjunction from alphanumeric/underscore tokens.
///
/// Returns `None` when the text carries no searchable token, in which case the
/// caller lists the first rows in stable order instead.
fn fts_query(text: &str) -> Option<String> {
    let tokens = text
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    (!tokens.is_empty()).then(|| tokens.join(" AND "))
}

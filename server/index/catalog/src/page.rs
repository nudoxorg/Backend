//! Checkpoint advancement and page-operation projection.

use core::convert::Infallible;

use server_index_ingest::{Checkpoint, PendingCheckpoint};
use server_index_vocabulary::PackageCoordinate;

use super::{CatalogError, TursoCatalog, codec::integer};
use super::{CatalogPageError, CatalogPageOperation};

impl TursoCatalog {
    /// Applies bounded mutations and a strictly newer checkpoint in one transaction.
    pub async fn apply_page(
        &mut self,
        current: Checkpoint,
        next: Checkpoint,
        operations: &[CatalogPageOperation<'_>],
    ) -> Result<Checkpoint, CatalogPageError> {
        let pending = current
            .prepare_next(next)
            .map_err(CatalogPageError::Candidate)?;
        durable_cells(current)?;
        let next_cells = durable_cells(next)?;
        if operations.len() > server_index_ingest::MAX_RECONCILIATION_ROWS {
            return Err(CatalogPageError::TooManyOperations);
        }
        for (index, operation) in operations.iter().enumerate() {
            let selected_coordinate = coordinate(operation);
            for prior in &operations[..index] {
                if coordinate(prior) == selected_coordinate {
                    return Err(CatalogPageError::DuplicateCoordinate);
                }
            }
        }
        let transaction = self.connection.transaction().await?;
        let stored = read_checkpoint(&transaction).await?;
        if stored != current {
            return Err(CatalogPageError::Stale { stored, current });
        }
        for operation in operations {
            match operation {
                CatalogPageOperation::Publish(publication) => {
                    Self::publish_in(&transaction, *publication).await?;
                }
                CatalogPageOperation::Remove(coordinate) => {
                    transaction.execute("DELETE FROM catalog_current WHERE ecosystem=?1 AND package=?2 AND version=?3", (coordinate.lineage.ecosystem, coordinate.lineage.name, coordinate.version.as_str())).await?;
                }
            }
        }
        transaction
            .execute(
                "UPDATE catalog_checkpoint SET sequence=?1,page=?2 WHERE singleton=1",
                next_cells,
            )
            .await?;
        transaction.commit().await?;
        advance_after_commit(pending)
    }

    /// Returns the checkpoint committed with the most recent successful page.
    pub async fn checkpoint(&self) -> Result<Checkpoint, CatalogError> {
        read_checkpoint(&self.connection).await
    }
}

fn durable_cells(checkpoint: Checkpoint) -> Result<(i64, i64), CatalogPageError> {
    Ok((
        i64::try_from(checkpoint.sequence).map_err(|_| CatalogPageError::CheckpointWidth)?,
        i64::try_from(checkpoint.page).map_err(|_| CatalogPageError::CheckpointWidth)?,
    ))
}

async fn read_checkpoint(connection: &turso::Connection) -> Result<Checkpoint, CatalogError> {
    let mut rows = connection
        .query(
            "SELECT sequence,page FROM catalog_checkpoint WHERE singleton=1",
            (),
        )
        .await?;
    let row = rows.next().await?.ok_or(CatalogError::CorruptScalar)?;
    Ok(Checkpoint {
        sequence: u64::try_from(integer(row.get_value(0)?)?)
            .map_err(|_| CatalogError::CorruptScalar)?,
        page: u64::try_from(integer(row.get_value(1)?)?)
            .map_err(|_| CatalogError::CorruptScalar)?,
    })
}

pub(super) fn advance_after_commit(
    pending: PendingCheckpoint,
) -> Result<Checkpoint, CatalogPageError> {
    match pending.advance_after_durable_apply(|| Ok::<(), Infallible>(())) {
        Ok(checkpoint) => Ok(checkpoint),
        Err(fault) => match fault.error {},
    }
}

pub(super) const fn coordinate<'coordinate>(
    operation: &CatalogPageOperation<'coordinate>,
) -> PackageCoordinate<'coordinate> {
    match operation {
        CatalogPageOperation::Publish(publication) => publication.package,
        CatalogPageOperation::Remove(coordinate) => *coordinate,
    }
}

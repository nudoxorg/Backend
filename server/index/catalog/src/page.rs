//! Checkpoint advancement and page-operation projection.

use core::convert::Infallible;

use server_index_ingest::{Checkpoint, PendingCheckpoint};
use server_index_vocabulary::PackageCoordinate;

use super::{
    CatalogError, FeedCheckpoint, FeedContentChecksum, FeedCursor, FeedIdentity, FeedObservation,
    FeedObservationState, FeedSnapshotId, FeedValidator, TursoCatalog,
    codec::{blob, integer},
    feed::{validate_non_snapshot_transition, validate_snapshot_transition},
};
use super::{CatalogPageError, CatalogPageOperation};

impl TursoCatalog {
    /// Applies one feed's bounded mutations and successor source state in one transaction.
    ///
    /// Unlike the historical singleton cursor, every registry identity owns an independent
    /// checkpoint, opaque continuation cursor, and conditional validator.
    pub async fn apply_feed_page(
        &mut self,
        current: &FeedCheckpoint,
        next: &FeedCheckpoint,
        operations: &[CatalogPageOperation<'_>],
    ) -> Result<FeedCheckpoint, CatalogPageError> {
        if current.feed != next.feed {
            return Err(CatalogPageError::FeedMismatch);
        }
        validate_non_snapshot_transition(current, next)
            .map_err(CatalogPageError::FeedTransition)?;
        let pending = current
            .checkpoint
            .prepare_next(next.checkpoint)
            .map_err(CatalogPageError::Candidate)?;
        durable_cells(current.checkpoint)?;
        let next_cells = durable_cells(next.checkpoint)?;
        preflight(operations)?;
        let transaction = self.connection.transaction().await?;
        let stored = read_feed_checkpoint(&transaction, &current.feed).await?;
        if stored != *current {
            return Err(CatalogPageError::Stale {
                stored: stored.checkpoint,
                current: current.checkpoint,
            });
        }
        apply_operations(&transaction, operations).await?;
        write_feed_state(&transaction, next, next_cells).await?;
        transaction.commit().await?;
        advance_after_commit(pending)?;
        Ok(next.clone())
    }

    /// Reads one feed's independent durable source state, returning its typed initial state
    /// before the first successful page has been applied.
    pub async fn feed_checkpoint(
        &self,
        feed: FeedIdentity,
    ) -> Result<FeedCheckpoint, CatalogError> {
        read_feed_checkpoint(&self.connection, &feed).await
    }

    /// Reads exact source checksums separately from compiler semantic publication identities.
    pub async fn feed_observations(
        &self,
        feed: &FeedIdentity,
    ) -> Result<Vec<FeedObservationState>, CatalogError> {
        let mut rows = self.connection.query("SELECT ecosystem,package,version,checksum,active,cycle FROM catalog_feed_observation WHERE feed=?1 ORDER BY ecosystem,package,version", (feed.as_str(),)).await?;
        let mut result = Vec::new();
        while let Some(row) = rows.next().await? {
            let text = |index| match row.get_value(index)? {
                turso::Value::Text(value) => Ok(value),
                _ => Err(CatalogError::CorruptScalar),
            };
            let checksum: [u8; 32] = blob(row.get_value(3)?, 32)?
                .try_into()
                .map_err(|_| CatalogError::CorruptBlob)?;
            let active = match integer(row.get_value(4)?)? {
                0 => false,
                1 => true,
                _ => return Err(CatalogError::CorruptScalar),
            };
            result.push(FeedObservationState {
                ecosystem: text(0)?,
                package: text(1)?,
                version: text(2)?,
                checksum: FeedContentChecksum::from_bytes(checksum),
                active,
                cycle: u64::try_from(integer(row.get_value(5)?)?)
                    .map_err(|_| CatalogError::CorruptScalar)?,
            });
        }
        Ok(result)
    }

    /// Commits one bounded stable-snapshot chunk with observations and delayed final removals.
    pub async fn apply_feed_snapshot_page(
        &mut self,
        current: &FeedCheckpoint,
        next: &FeedCheckpoint,
        operations: &[CatalogPageOperation<'_>],
        observations: &[FeedObservation<'_>],
        complete: bool,
    ) -> Result<FeedCheckpoint, CatalogPageError> {
        if current.feed != next.feed {
            return Err(CatalogPageError::FeedMismatch);
        }
        validate_snapshot_transition(current, next, observations.len(), complete)
            .map_err(CatalogPageError::FeedTransition)?;
        let pending = current
            .checkpoint
            .prepare_next(next.checkpoint)
            .map_err(CatalogPageError::Candidate)?;
        durable_cells(current.checkpoint)?;
        let cells = durable_cells(next.checkpoint)?;
        preflight(operations)?;
        if observations.len() > server_index_ingest::MAX_RECONCILIATION_ROWS {
            return Err(CatalogPageError::TooManyOperations);
        }
        let transaction = self.connection.transaction().await?;
        let stored = read_feed_checkpoint(&transaction, &current.feed).await?;
        if stored != *current {
            return Err(CatalogPageError::Stale {
                stored: stored.checkpoint,
                current: current.checkpoint,
            });
        }
        write_feed_state(&transaction, next, cells).await?;
        apply_operations(&transaction, operations).await?;
        let cycle = i64::try_from(next.cycle).map_err(|_| CatalogPageError::CheckpointWidth)?;
        for observation in observations {
            transaction.execute("INSERT INTO catalog_feed_observation(feed,ecosystem,package,version,checksum,active,cycle) VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(feed,ecosystem,package,version) DO UPDATE SET checksum=excluded.checksum,active=excluded.active,cycle=excluded.cycle", (next.feed.as_str(), observation.coordinate.lineage.ecosystem, observation.coordinate.lineage.name, observation.coordinate.version.as_str(), turso::Value::Blob(observation.checksum.as_bytes().to_vec()), i64::from(observation.active), cycle)).await?;
        }
        if complete {
            transaction
                .execute(
                    "UPDATE catalog_feed_observation SET active=0 WHERE feed=?1 AND cycle<>?2",
                    (next.feed.as_str(), cycle),
                )
                .await?;
            transaction.execute("DELETE FROM catalog_current WHERE EXISTS (SELECT 1 FROM catalog_feed_observation target WHERE target.feed=?1 AND target.ecosystem=catalog_current.ecosystem AND target.package=catalog_current.package AND target.version=catalog_current.version AND target.active=0) AND NOT EXISTS (SELECT 1 FROM catalog_feed_observation live WHERE live.ecosystem=catalog_current.ecosystem AND live.package=catalog_current.package AND live.version=catalog_current.version AND live.active=1)", (next.feed.as_str(),)).await?;
        }
        transaction.commit().await?;
        advance_after_commit(pending)?;
        Ok(next.clone())
    }

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
        preflight(operations)?;
        let transaction = self.connection.transaction().await?;
        let stored = read_checkpoint(&transaction).await?;
        if stored != current {
            return Err(CatalogPageError::Stale { stored, current });
        }
        apply_operations(&transaction, operations).await?;
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

fn preflight(operations: &[CatalogPageOperation<'_>]) -> Result<(), CatalogPageError> {
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
    Ok(())
}

async fn apply_operations(
    transaction: &turso::transaction::Transaction<'_>,
    operations: &[CatalogPageOperation<'_>],
) -> Result<(), CatalogError> {
    for operation in operations {
        match operation {
            CatalogPageOperation::Publish(publication) => {
                TursoCatalog::publish_in(transaction, *publication).await?;
            }
            CatalogPageOperation::Remove(coordinate) => {
                transaction.execute("DELETE FROM catalog_current WHERE ecosystem=?1 AND package=?2 AND version=?3", (coordinate.lineage.ecosystem, coordinate.lineage.name, coordinate.version.as_str())).await?;
            }
        }
    }
    Ok(())
}

async fn write_feed_state(
    transaction: &turso::transaction::Transaction<'_>,
    next: &FeedCheckpoint,
    cells: (i64, i64),
) -> Result<(), CatalogPageError> {
    let cycle = i64::try_from(next.cycle).map_err(|_| CatalogPageError::CheckpointWidth)?;
    let offset = i64::try_from(next.offset).map_err(|_| CatalogPageError::CheckpointWidth)?;
    transaction.execute("INSERT INTO catalog_feed_checkpoint(feed,sequence,page,cursor,validator) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(feed) DO UPDATE SET sequence=excluded.sequence,page=excluded.page,cursor=excluded.cursor,validator=excluded.validator", (next.feed.as_str(), cells.0, cells.1, next.cursor.as_ref().map(FeedCursor::as_str), next.validator.as_ref().map(FeedValidator::as_str))).await?;
    transaction.execute("INSERT INTO catalog_feed_cycle(feed,cycle,snapshot,offset) VALUES (?1,?2,?3,?4) ON CONFLICT(feed) DO UPDATE SET cycle=excluded.cycle,snapshot=excluded.snapshot,offset=excluded.offset", (next.feed.as_str(), cycle, next.snapshot.map(|snapshot| turso::Value::Blob(snapshot.as_bytes().to_vec())), offset)).await?;
    Ok(())
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

async fn read_feed_checkpoint(
    connection: &turso::Connection,
    feed: &FeedIdentity,
) -> Result<FeedCheckpoint, CatalogError> {
    let mut rows = connection
        .query(
            "SELECT c.sequence,c.page,c.cursor,c.validator,s.cycle,s.snapshot,s.offset FROM catalog_feed_checkpoint c LEFT JOIN catalog_feed_cycle s ON s.feed=c.feed WHERE c.feed=?1",
            (feed.as_str(),),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(FeedCheckpoint::initial(feed.clone()));
    };
    let text = |value: turso::Value| match value {
        turso::Value::Text(value) => {
            FeedCursor::new(value).map_err(|_| CatalogError::CorruptScalar)
        }
        _ => Err(CatalogError::CorruptScalar),
    };
    let optional = |value: turso::Value| match value {
        turso::Value::Null => Ok(None),
        value => text(value).map(Some),
    };
    Ok(FeedCheckpoint {
        feed: feed.clone(),
        checkpoint: Checkpoint {
            sequence: u64::try_from(integer(row.get_value(0)?)?)
                .map_err(|_| CatalogError::CorruptScalar)?,
            page: u64::try_from(integer(row.get_value(1)?)?)
                .map_err(|_| CatalogError::CorruptScalar)?,
        },
        cursor: optional(row.get_value(2)?)?,
        validator: optional_validator(row.get_value(3)?)?,
        cycle: match row.get_value(4)? {
            turso::Value::Null => 0,
            value => u64::try_from(integer(value)?).map_err(|_| CatalogError::CorruptScalar)?,
        },
        snapshot: match row.get_value(5)? {
            turso::Value::Null => None,
            value => {
                let bytes: [u8; 32] = blob(value, 32)?
                    .try_into()
                    .map_err(|_| CatalogError::CorruptBlob)?;
                Some(FeedSnapshotId::from_sha256(bytes))
            }
        },
        offset: match row.get_value(6)? {
            turso::Value::Null => 0,
            value => u64::try_from(integer(value)?).map_err(|_| CatalogError::CorruptScalar)?,
        },
    })
}

fn optional_validator(value: turso::Value) -> Result<Option<FeedValidator>, CatalogError> {
    match value {
        turso::Value::Null => Ok(None),
        turso::Value::Text(value) => FeedValidator::new(value)
            .map(Some)
            .map_err(|_| CatalogError::CorruptScalar),
        _ => Err(CatalogError::CorruptScalar),
    }
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

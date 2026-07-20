//! The catalog read surface (INDEX-PLAN §8.1): `get_package`, `changed_since`
//! cursor pagination, `outbox_claim`, and the bitemporal `at(AsOf)` view.
//!
//! Read functions are free functions generic over the engine trait so both the
//! writer and any read-only replica handle can call them.

use heart::query::{AsOf, CatalogCommitHash};

use crate::engine::{CatalogEngine, Value, VersioningEngine};
use crate::enums::{SinkKind, TextEnum};
use crate::ids::PackageStemId;
use crate::tables::outbox::OutboxRow;
use crate::tables::packages::{self, PackageRow};
use crate::tables::sink_watermarks;

use super::{CatalogCursor, ChangedPage, MetaError};

/// A read view pinned to a point in catalog history (INDEX-PLAN §9).
///
/// The commit is already resolved (an `AsOf::Time` was turned into a concrete
/// commit against the engine's history graph). Downstream reads that want the
/// snapshot semantics run their SQL against a checkout of `commit`; this type is
/// the resolved handle those reads carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogAsOf {
    /// The concrete commit this view is pinned to.
    pub commit: CatalogCommitHash,
}

/// Fetch a package stem row by id.
pub fn get_package<E: CatalogEngine>(
    engine: &E,
    stem: PackageStemId,
) -> Result<Option<PackageRow>, MetaError> {
    let sql = format!(
        "SELECT {columns} FROM {table} WHERE {stem_id} = ?1",
        columns = packages::PackageRow::INSERT_COLUMNS.join(", "),
        table = packages::TABLE,
        stem_id = packages::columns::STEM_ID,
    );
    let mut rows = engine.query_rows(
        &sql,
        &[Value::Blob(stem.to_blob().to_vec())],
        &mut |row| PackageRow::from_row(row).map_err(Into::into),
    )?;
    Ok(rows.pop())
}

/// Page the catalog change stream (the outbox) after `cursor`, in `seq` order.
/// Stable under interleaved writes: new writes only ever receive higher `seq`,
/// so a cursor never skips or repeats a row.
pub fn changed_since<E: CatalogEngine>(
    engine: &E,
    cursor: CatalogCursor,
) -> Result<ChangedPage, MetaError> {
    // A generous default page; callers page until an empty result.
    const PAGE_LIMIT: i64 = 512;
    let sql = format!(
        "SELECT {columns} FROM {table} WHERE seq > ?1 ORDER BY seq ASC LIMIT ?2",
        columns = OutboxRow::SELECT_COLUMNS.join(", "),
        table = crate::tables::outbox::TABLE,
    );
    let rows = engine.query_rows(
        &sql,
        &[Value::Integer(cursor.after_seq), Value::Integer(PAGE_LIMIT)],
        &mut |row| OutboxRow::from_row(row).map_err(Into::into),
    )?;
    let next = match rows.last() {
        Some(last) => CatalogCursor { after_seq: last.seq },
        None => cursor,
    };
    Ok(ChangedPage { rows, next })
}

/// Claim up to `limit` outbox rows for `sink`, strictly after its recorded
/// watermark, in `seq` order. Does **not** advance the watermark.
pub fn outbox_claim<E: CatalogEngine>(
    engine: &E,
    sink: SinkKind,
    limit: usize,
) -> Result<Vec<OutboxRow>, MetaError> {
    let watermark = current_watermark(engine, sink)?;
    outbox_read_since(engine, sink, watermark, limit)
}

/// Read up to `limit` outbox rows for `sink` with `seq > after`, ascending.
/// The primitive under [`outbox_claim`]; also serves explicit-cursor pollers.
pub fn outbox_read_since<E: CatalogEngine>(
    engine: &E,
    sink: SinkKind,
    after: i64,
    limit: usize,
) -> Result<Vec<OutboxRow>, MetaError> {
    let sql = format!(
        "SELECT {columns} FROM {table} WHERE seq > ?1 AND sink_kind = ?2 \
         ORDER BY seq ASC LIMIT ?3",
        columns = OutboxRow::SELECT_COLUMNS.join(", "),
        table = crate::tables::outbox::TABLE,
    );
    let rows = engine.query_rows(
        &sql,
        &[
            Value::Integer(after),
            Value::Text(crate::enums::TextEnum::as_token(&sink).to_owned()),
            Value::Integer(limit as i64),
        ],
        &mut |row| OutboxRow::from_row(row).map_err(Into::into),
    )?;
    Ok(rows)
}

/// Delete outbox rows already consumed by **every** sink (seq at or below the
/// minimum watermark across all sinks). Returns rows removed.
pub fn outbox_gc<E: CatalogEngine>(engine: &E) -> Result<u64, MetaError> {
    let minimum = [SinkKind::Text, SinkKind::Vector, SinkKind::UsageIndex]
        .into_iter()
        .map(|sink| current_watermark(engine, sink))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min()
        .unwrap_or(0);
    let removed = engine.execute(
        &format!("DELETE FROM {table} WHERE seq <= ?1", table = crate::tables::outbox::TABLE),
        &[Value::Integer(minimum)],
    )?;
    Ok(removed as u64)
}

/// The current watermark `last_seq` for a sink, or `0` when unseen.
pub fn current_watermark<E: CatalogEngine>(
    engine: &E,
    sink: SinkKind,
) -> Result<i64, MetaError> {
    let sql = format!(
        "SELECT {last_seq} FROM {table} WHERE {sink_kind} = ?1",
        last_seq = sink_watermarks::columns::LAST_SEQ,
        table = sink_watermarks::TABLE,
        sink_kind = sink_watermarks::columns::SINK_KIND,
    );
    let mut values = engine.query_rows(
        &sql,
        &[Value::Text(sink.as_token().to_owned())],
        &mut |row| row.get_integer(0),
    )?;
    Ok(values.pop().unwrap_or(0))
}

/// The highest `seq` present in the outbox for `sink` (the drain head), or `0`
/// when the sink has no rows. The follower lag is `outbox_head - watermark`.
pub fn outbox_head<E: CatalogEngine>(engine: &E, sink: SinkKind) -> Result<i64, MetaError> {
    let sql = format!(
        "SELECT COALESCE(MAX(seq), 0) FROM {table} WHERE {sink_kind} = ?1",
        table = crate::tables::outbox::TABLE,
        sink_kind = crate::tables::outbox::columns::SINK_KIND,
    );
    let mut values = engine.query_rows(
        &sql,
        &[Value::Text(sink.as_token().to_owned())],
        &mut |row| row.get_integer(0),
    )?;
    Ok(values.pop().unwrap_or(0))
}

/// Resolve a bitemporal read view (INDEX-PLAN §9). `AsOf::Commit` is taken as
/// given; `AsOf::Time` is resolved to the newest commit at or before the instant
/// against the engine's history graph, erroring if none exists.
pub fn at<E: VersioningEngine>(
    engine: &E,
    as_of: &AsOf,
) -> Result<CatalogAsOf, MetaError> {
    match as_of {
        AsOf::Commit(commit) => Ok(CatalogAsOf {
            commit: commit.clone(),
        }),
        AsOf::Time(instant) => {
            let resolved = engine.resolve_as_of_time(instant.0)?;
            match resolved {
                Some(commit) => Ok(CatalogAsOf {
                    commit: CatalogCommitHash(commit.0),
                }),
                None => Err(MetaError::NoCommitAtInstant),
            }
        }
    }
}

//! Catalog read surface (INDEX-PLAN §8.1): `get_package`, `changed_since`,
//! `outbox_claim`, and the bitemporal `at(AsOf)` view.
//!
//! Selects are SeaORM `Entity::find` queries; rows map into entity models.

use sea_orm::{
    ColumnTrait, Condition, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    QueryTrait,
};

use heart::query::{AsOf, CatalogCommitHash};

use crate::{
    codec::CodecError,
    engine::{self, CatalogEngine, Row, VersioningEngine},
    entity::{git_watermarks, outbox, packages, sink_watermarks, versions},
    enums::{OutboxOperation, SinkKind, TextEnum},
    ids::{GenerationStamp, PackageId, PackageStemId, version_id},
    tables::{outbox::OutboxRow, packages::PackageRow},
};

use super::{CatalogCursor, ChangedPage, MetaError, VersionSnapshot};

/// A read view pinned to a point in catalog history (INDEX-PLAN §9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogAsOf {
    /// The concrete commit this view is pinned to.
    pub commit: CatalogCommitHash,
}

/// The catalog's last seen ref digest for `stem`, when a poll has recorded one.
pub fn git_last_rev<E: CatalogEngine>(
    engine: &E,
    stem: PackageStemId,
) -> Result<Option<String>, MetaError> {
    let stmt = git_watermarks::Entity::find()
        .filter(git_watermarks::Column::StemId.eq(stem))
        .select_only()
        .column(git_watermarks::Column::LastRev)
        .build(DbBackend::Sqlite);
    let mut rows = engine::query(engine, stmt, &mut |row| row.get_optional_text(0))?;
    Ok(rows.pop().flatten())
}

/// One C++ package the git monitor should poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPollTarget {
    /// Catalog stem.
    pub stem_id: PackageStemId,
    /// Canonical name, the repo slug.
    pub name: String,
    /// Fetch URL.
    pub repo_url: String,
}

/// C++ packages that named a repository URL.
///
/// Other ecosystems stay on their registry followers. An empty URL is omitted.
pub fn git_poll_targets<E: CatalogEngine>(engine: &E) -> Result<Vec<GitPollTarget>, MetaError> {
    let stmt = packages::Entity::find()
        .filter(packages::Column::Ecosystem.eq("cpp"))
        .build(DbBackend::Sqlite);
    let rows = engine::query(engine, stmt, &mut |row| {
        package_from_row(row).map_err(Into::into)
    })?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let repo_url = row.repo_url.filter(|url| !url.is_empty())?;
            Some(GitPollTarget {
                stem_id: row.stem_id,
                name: row.name_canonical,
                repo_url,
            })
        })
        .collect())
}

/// Fetch a package stem row by id.
pub fn get_package<E: CatalogEngine>(
    engine: &E,
    stem: PackageStemId,
) -> Result<Option<PackageRow>, MetaError> {
    let stmt = packages::Entity::find()
        .filter(packages::Column::StemId.eq(stem))
        .build(DbBackend::Sqlite);
    let mut rows = engine::query(engine, stmt, &mut |row| {
        package_from_row(row).map_err(Into::into)
    })?;
    Ok(rows.pop())
}

fn package_from_row(row: &dyn Row) -> Result<PackageRow, CodecError> {
    // Column order follows entity field declaration / SELECT * order from find().
    Ok(PackageRow {
        stem_id: PackageStemId::from_blob(&row.get_blob(0)?)?,
        ecosystem: row.get_text(1)?,
        name_struct: row.get_text(2)?,
        name_canonical: row.get_text(3)?,
        name_original: row.get_text(4)?,
        repo_url: row.get_optional_text(5)?,
        created_at: row.get_integer(6)?,
    })
}

/// Page the catalog change stream (the outbox) after `cursor`, in `seq` order.
pub fn changed_since<E: CatalogEngine>(
    engine: &E,
    cursor: CatalogCursor,
) -> Result<ChangedPage, MetaError> {
    const PAGE_LIMIT: u64 = 512;
    let stmt = outbox::Entity::find()
        .filter(outbox::Column::Seq.gt(cursor.after_seq))
        .order_by_asc(outbox::Column::Seq)
        .limit(PAGE_LIMIT)
        .build(DbBackend::Sqlite);
    let rows = engine::query(engine, stmt, &mut |row| {
        outbox_from_row(row).map_err(Into::into)
    })?;
    let next = rows.last().map_or(cursor, |last| CatalogCursor {
        after_seq: last.seq,
    });
    Ok(ChangedPage { rows, next })
}

/// Read the durable version set for one stem without loading unrelated
/// packages.
pub fn version_snapshots<E: CatalogEngine>(
    engine: &E,
    stem: PackageStemId,
) -> Result<Vec<VersionSnapshot>, MetaError> {
    let stmt = versions::Entity::find()
        .filter(versions::Column::StemId.eq(stem))
        .select_only()
        .column(versions::Column::Id)
        .column(versions::Column::VersionCanonical)
        .column(versions::Column::SourceRev)
        .order_by_asc(versions::Column::VersionCanonical)
        .build(DbBackend::Sqlite);
    engine::query(engine, stmt, &mut |row| {
        Ok(VersionSnapshot {
            version_id: PackageId::from_uuid(
                *version_id::from_blob(&row.get_blob(0)?)
                    .map_err(crate::codec::CodecError::from)?
                    .as_uuid(),
            ),
            version_canonical: row.get_text(1)?,
            source_rev: row.get_optional_text(2)?,
        })
    })
    .map_err(Into::into)
}

fn outbox_from_row(row: &dyn Row) -> Result<OutboxRow, CodecError> {
    Ok(OutboxRow {
        seq: row.get_integer(0)?,
        version_id: match row.get_optional_blob(1)? {
            Some(bytes) => Some(*version_id::from_blob(&bytes)?.as_uuid()),
            None => None,
        },
        gen_stamp: match row.get_optional_blob(2)? {
            Some(bytes) => Some(GenerationStamp::from_blob(&bytes)?),
            None => None,
        },
        sink_kind: SinkKind::from_token(&row.get_text(3)?)?,
        op: OutboxOperation::from_token(&row.get_text(4)?)?,
        created_at: row.get_integer(5)?,
    })
}

/// Claim up to `limit` outbox rows for `sink`, strictly after its watermark.
pub fn outbox_claim<E: CatalogEngine>(
    engine: &E,
    sink: SinkKind,
    limit: usize,
) -> Result<Vec<OutboxRow>, MetaError> {
    let watermark = current_watermark(engine, sink)?;
    outbox_read_since(engine, sink, watermark, limit)
}

/// Read up to `limit` outbox rows for `sink` with `seq > after`, ascending.
pub fn outbox_read_since<E: CatalogEngine>(
    engine: &E,
    sink: SinkKind,
    after: i64,
    limit: usize,
) -> Result<Vec<OutboxRow>, MetaError> {
    let stmt = outbox::Entity::find()
        .filter(
            Condition::all()
                .add(outbox::Column::Seq.gt(after))
                .add(outbox::Column::SinkKind.eq(sink)),
        )
        .order_by_asc(outbox::Column::Seq)
        .limit(limit as u64)
        .build(DbBackend::Sqlite);
    let rows = engine::query(engine, stmt, &mut |row| {
        outbox_from_row(row).map_err(Into::into)
    })?;
    Ok(rows)
}

/// Delete outbox rows already consumed by every sink.
pub fn outbox_gc<E: CatalogEngine>(engine: &E) -> Result<u64, MetaError> {
    let minimum = [SinkKind::Text, SinkKind::Vector, SinkKind::UsageIndex]
        .into_iter()
        .map(|sink| current_watermark(engine, sink))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .min()
        .unwrap_or(0);
    let stmt = outbox::Entity::delete_many()
        .filter(outbox::Column::Seq.lte(minimum))
        .build(DbBackend::Sqlite);
    let removed = engine::exec(engine, stmt)?;
    Ok(removed as u64)
}

/// The current watermark `last_seq` for a sink, or `0` when unseen.
pub fn current_watermark<E: CatalogEngine>(engine: &E, sink: SinkKind) -> Result<i64, MetaError> {
    let stmt = sink_watermarks::Entity::find()
        .filter(sink_watermarks::Column::SinkKind.eq(sink))
        .select_only()
        .column(sink_watermarks::Column::LastSeq)
        .build(DbBackend::Sqlite);
    let mut values = engine::query(engine, stmt, &mut |row| row.get_integer(0))?;
    Ok(values.pop().unwrap_or(0))
}

/// The highest `seq` present in the outbox for `sink`, or `0` when empty.
pub fn outbox_head<E: CatalogEngine>(engine: &E, sink: SinkKind) -> Result<i64, MetaError> {
    use sea_orm::sea_query::{Expr, Func, SimpleExpr};
    let stmt = outbox::Entity::find()
        .filter(outbox::Column::SinkKind.eq(sink))
        .select_only()
        .column_as(
            SimpleExpr::FunctionCall(Func::coalesce([
                Expr::col(outbox::Column::Seq).max(),
                Expr::value(0i64),
            ])),
            "head",
        )
        .build(DbBackend::Sqlite);
    let mut values = engine::query(engine, stmt, &mut |row| row.get_integer(0))?;
    Ok(values.pop().unwrap_or(0))
}

/// Resolve a bitemporal read view (INDEX-PLAN §9).
pub fn at<E: VersioningEngine>(engine: &E, as_of: &AsOf) -> Result<CatalogAsOf, MetaError> {
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

//! Version lifecycle reads/writes for the serving plane (INDEX-PLAN §8).
//!
//! Built with SeaORM `Entity` queries / `update_many` / `ActiveModel` upserts.

use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, DbBackend, EntityTrait, JoinType, QueryFilter, QueryOrder,
    QuerySelect, QueryTrait, RelationTrait,
};

use crate::engine::{self, CatalogEngine};
use crate::entity::{
    edges, facets, generations, listing_events, packages, symbols_proj, versions,
};
use crate::enums::{IrStatus, OutboxOperation, ParseState, SinkKind, TextEnum};
use crate::ids::{GenerationStamp, IntroIdHash, PackageId};
use crate::store::MetaError;

/// The lifecycle columns of one `versions` row, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionLifecycle {
    pub parse_state: ParseState,
    pub parse_phase: Option<String>,
    pub attempts: i64,
    pub failure: Option<String>,
}

/// Write a version's lifecycle columns (and only those columns).
///
/// # Does not fan out to the Text sink
///
/// This used to unconditionally `emit_outbox_row(.., SinkKind::Text, Upsert)`
/// after every write — i.e. on *every* pipeline phase transition
/// (`Acquiring`/`Extracting`/`Compiling`/`Emitting`), not just the terminal
/// one. One parse job therefore left five-plus Text intents behind it (one
/// per `set_state` call, via `GlobalStore::write_state`), all functionally
/// identical from the Text/package-search sink's point of view — its
/// consumer (`materialize`'s `SinkKind::Text` arm) is a no-op; the real work
/// happens in the separate `package_index_poller`, which reads the *current*
/// catalog row whenever it processes an intent, not a per-emission snapshot.
/// Bookkeeping transitions through the pipeline's phases carry no
/// search-relevant change, so they need no Text signal at all.
///
/// Two other places *do* carry a real Text-relevant change and each emits its
/// own dedicated signal: metadata becoming known/updated, at
/// `store::apply::apply_run`'s `CatalogOp::UpsertVersion` arm (see
/// `tests/store_apply.rs` / `tests/dolt_engine.rs`), and a generation
/// reaching the terminal `Stored` state — once symbols actually exist to
/// project — at `coordination::Outbox::record_stored` (see that function's
/// doc comment for why Text is emitted there too, symmetric with
/// Vector/Graph, and `tests/text_sink_recovers_after_stored.rs` for the
/// regression it fixes). Emitting from *this* function in between would
/// still be pure duplication, not a second necessary signal.
pub fn set_version_lifecycle<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    parse_state: ParseState,
    parse_phase: Option<&str>,
    failure: Option<&str>,
    bump_attempts: bool,
) -> Result<(), MetaError> {
    let stmt = versions::Entity::update_many()
        .col_expr(versions::Column::ParseState, Expr::value(parse_state))
        .col_expr(
            versions::Column::ParsePhase,
            Expr::value(parse_phase.map(std::borrow::ToOwned::to_owned)),
        )
        .col_expr(
            versions::Column::Failure,
            Expr::value(failure.map(std::borrow::ToOwned::to_owned)),
        )
        .col_expr(
            versions::Column::Attempts,
            Expr::col(versions::Column::Attempts).add(i64::from(bump_attempts)),
        )
        .filter(versions::Column::Id.eq(*version.as_uuid()))
        .build(DbBackend::Sqlite);
    let changed = engine::exec(engine, stmt)?;
    if changed == 0 {
        return Err(MetaError::MissingRow {
            what: "versions row for lifecycle write",
        });
    }
    Ok(())
}

/// Read a version's lifecycle columns. `Ok(None)` when the row is absent.
pub fn version_lifecycle<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<VersionLifecycle>, MetaError> {
    let stmt = versions::Entity::find()
        .filter(versions::Column::Id.eq(*version.as_uuid()))
        .select_only()
        .column(versions::Column::ParseState)
        .column(versions::Column::ParsePhase)
        .column(versions::Column::Attempts)
        .column(versions::Column::Failure)
        .build(DbBackend::Sqlite);
    let mut rows = engine::query(engine, stmt, &mut |row| {
        Ok((
            row.get_text(0)?,
            row.get_optional_text(1)?,
            row.get_integer(2)?,
            row.get_optional_text(3)?,
        ))
    })?;
    let Some((state_token, parse_phase, attempts, failure)) = rows.pop() else {
        return Ok(None);
    };
    Ok(Some(VersionLifecycle {
        parse_state: ParseState::from_token(&state_token)
            .map_err(crate::codec::CodecError::from)?,
        parse_phase,
        attempts,
        failure,
    }))
}

/// Record the `Stored` terminal generation.
pub fn record_stored_generation<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    content_hash: &[u8; 32],
    sealed_at_unix_milliseconds: i64,
) -> Result<(), MetaError> {
    let stamp = GenerationStamp::from_bytes(*content_hash);
    let am = generations::ActiveModel {
        gen_stamp: Set(stamp),
        version_id: Set(*version.as_uuid()),
        channel_tip: Set(None),
        job_key: Set(None),
        producer_toolchain: Set(None),
        sealed_at: Set(Some(sealed_at_unix_milliseconds)),
        ir_status: Set(IrStatus::Sealed),
        resolution_stats: Set(None),
    };
    let stmt = generations::Entity::insert(am)
        .on_conflict(
            OnConflict::column(generations::Column::GenStamp)
                .update_columns([generations::Column::SealedAt, generations::Column::IrStatus])
                .to_owned(),
        )
        .build(DbBackend::Sqlite);
    engine::exec(engine, stmt)?;
    Ok(())
}

/// The newest sealed generation stamp for a version, if any.
pub fn latest_generation<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<GenerationStamp>, MetaError> {
    let stmt = generations::Entity::find()
        .filter(generations::Column::VersionId.eq(*version.as_uuid()))
        .order_by_desc(generations::Column::SealedAt)
        .select_only()
        .column(generations::Column::GenStamp)
        .limit(1)
        .build(DbBackend::Sqlite);
    let mut rows = engine::query(engine, stmt, &mut |row| row.get_blob(0))?;
    let Some(blob) = rows.pop() else {
        return Ok(None);
    };
    let stamp = GenerationStamp::from_blob(&blob).map_err(crate::codec::CodecError::from)?;
    Ok(Some(stamp))
}

/// Upsert one serving-projection symbol row (`symbols_proj`).
pub fn upsert_symbol_projection<E: CatalogEngine>(
    engine: &E,
    intro_id: &[u8; 32],
    version: PackageId,
    gen_stamp: &[u8; 32],
    moniker: &str,
    kind: &str,
) -> Result<(), MetaError> {
    let am = symbols_proj::ActiveModel {
        gen_stamp: Set(GenerationStamp::from_bytes(*gen_stamp)),
        intro_id: Set(IntroIdHash::from_bytes(*intro_id)),
        version_id: Set(*version.as_uuid()),
        moniker: Set(moniker.to_owned()),
        kind: Set(kind.to_owned()),
    };
    let stmt = symbols_proj::Entity::insert(am)
        .on_conflict(
            OnConflict::columns([
                symbols_proj::Column::GenStamp,
                symbols_proj::Column::IntroId,
            ])
            .update_columns([symbols_proj::Column::Moniker, symbols_proj::Column::Kind])
            .to_owned(),
        )
        .build(DbBackend::Sqlite);
    engine::exec(engine, stmt)?;
    Ok(())
}

/// All projection symbols for a version: `(intro_id, moniker, kind)` rows.
pub fn symbols_for_version<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Vec<(Vec<u8>, String, String)>, MetaError> {
    let stmt = symbols_proj::Entity::find()
        .filter(symbols_proj::Column::VersionId.eq(*version.as_uuid()))
        .order_by_asc(symbols_proj::Column::Moniker)
        .select_only()
        .column(symbols_proj::Column::IntroId)
        .column(symbols_proj::Column::Moniker)
        .column(symbols_proj::Column::Kind)
        .build(DbBackend::Sqlite);
    engine::query(engine, stmt, &mut |row| {
        Ok((row.get_blob(0)?, row.get_text(1)?, row.get_text(2)?))
    })
    .map_err(MetaError::from)
}

/// Look up one projection symbol row by its packed `intro_id` slot, across
/// every generation and version (the caller — `GlobalStore::symbol_by_id`
/// — has only the durable `SymbolId`, not the version it belongs to; that is
/// exactly what this recovers). Returns `(version_id, moniker, kind)` for the
/// most recently-written matching row.
///
/// `intro_id` is not unique across generations of the *same* symbol by
/// design (`upsert_symbol_projection`'s conflict key is `(gen_stamp,
/// intro_id)`, so an unchanged symbol re-upserted under a new generation adds
/// a row rather than replacing one) — `order_by_desc(GenStamp)` picks the
/// newest write deterministically rather than an arbitrary row.
pub fn symbol_by_intro_id<E: CatalogEngine>(
    engine: &E,
    intro_id: &[u8; 32],
) -> Result<Option<(uuid::Uuid, String, String)>, MetaError> {
    let stmt = symbols_proj::Entity::find()
        .filter(symbols_proj::Column::IntroId.eq(intro_id.as_slice()))
        .order_by_desc(symbols_proj::Column::GenStamp)
        .select_only()
        .column(symbols_proj::Column::VersionId)
        .column(symbols_proj::Column::Moniker)
        .column(symbols_proj::Column::Kind)
        .limit(1)
        .build(DbBackend::Sqlite);
    // `version_id` is a sea-orm `Uuid` column, stored as a 16-byte BLOB on the
    // SQLite backend (not TEXT) — `get_blob` + `Uuid::from_slice`, matching
    // how every other UUID-typed id in this crate is decoded off the wire
    // (see `symbol_from_slot`/`PackageStemId::from_uuid` callers); `get_text`
    // here would (and did, before this was caught against a live server)
    // fail with "non-UTF-8 bytes in Row::get_text" on the raw bytes.
    let rows = engine::query(engine, stmt, &mut |row| {
        Ok((row.get_blob(0)?, row.get_text(1)?, row.get_text(2)?))
    })?;
    Ok(rows.into_iter().find_map(|(version_id, moniker, kind)| {
        uuid::Uuid::from_slice(&version_id)
            .ok()
            .map(|id| (id, moniker, kind))
    }))
}

// ── Archive conditional-GET cache (W4b) ────────────────────────────────────
//
// Durable substrate for "respect the source": persist the `ETag`/
// `Last-Modified` observed on a version's last successful archive fetch so a
// re-fetch can send `If-None-Match`/`If-Modified-Since` and treat a `304 Not
// Modified` as "reuse the content we already have" instead of paying for
// (and making the origin pay for) a full re-download.
//
// This lives on the `versions` row (one row per (stem, version) — the exact
// granularity a source-archive fetch is keyed at) rather than a new sibling
// table: `migrations::ddl::schema_v4_statements` derives each table's CREATE
// TABLE straight from its SeaORM entity (`Schema::create_table_from_entity`),
// so adding nullable columns to `entity::versions::Model` is picked up here
// with no separate DDL registration. Nullable + defaulting to `NULL` also
// means this never gates or fails a read/write path that predates it.
//
// No new trait was introduced for this: `CatalogEngine` (what every function
// in this file is already generic over) *is* this codebase's persistence
// abstraction — a bespoke `ArchiveCachePersistence` trait on top of it would
// just be a second name for the same seam. `ArchiveCacheMeta` plus these two
// functions are the "real catalog-backed impl" the durable substrate calls
// for.

/// Cached HTTP conditional-GET metadata for a version's upstream source
/// archive fetch. Both fields are independently optional because an origin
/// may send neither, either, or both validators.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArchiveCacheMeta {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl ArchiveCacheMeta {
    /// `true` when there is no cached validator to send as a conditional-GET
    /// precondition (never fetched, or the origin sent neither header).
    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }
}

/// Read the cached archive conditional-GET metadata for `version`.
///
/// Returns `ArchiveCacheMeta::default()` (both fields `None`) both when the
/// version has never had a successful fetch recorded and when the origin
/// never sent either header — the caller does not need to distinguish those
/// cases, since both mean "nothing to send as a precondition".
pub fn get_archive_cache_meta<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<ArchiveCacheMeta, MetaError> {
    let stmt = versions::Entity::find()
        .filter(versions::Column::Id.eq(*version.as_uuid()))
        .select_only()
        .column(versions::Column::ArchiveEtag)
        .column(versions::Column::ArchiveLastModified)
        .build(DbBackend::Sqlite);
    let mut rows = engine::query(engine, stmt, &mut |row| {
        Ok(ArchiveCacheMeta {
            etag: row.get_optional_text(0)?,
            last_modified: row.get_optional_text(1)?,
        })
    })?;
    Ok(rows.pop().unwrap_or_default())
}

/// Persist the archive conditional-GET metadata observed on the most recent
/// successful (non-304) fetch for `version`. Overwrites unconditionally —
/// callers pass `ArchiveCacheMeta::default()` to clear a validator the
/// origin stopped sending.
pub fn set_archive_cache_meta<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    meta: &ArchiveCacheMeta,
) -> Result<(), MetaError> {
    let stmt = versions::Entity::update_many()
        .col_expr(
            versions::Column::ArchiveEtag,
            Expr::value(meta.etag.clone()),
        )
        .col_expr(
            versions::Column::ArchiveLastModified,
            Expr::value(meta.last_modified.clone()),
        )
        .filter(versions::Column::Id.eq(*version.as_uuid()))
        .build(DbBackend::Sqlite);
    let changed = engine::exec(engine, stmt)?;
    if changed == 0 {
        return Err(MetaError::MissingRow {
            what: "versions row for archive cache metadata",
        });
    }
    Ok(())
}

/// Overwrite a version's facet row (write only — does not fan out).
///
/// Split out of [`set_facets`] for callers that already emit their own Text
/// signal for the very same event they're calling this from — most notably
/// `coordination::Outbox::record_stored`, which just transitioned the
/// version's lifecycle to `Stored` and is about to notify Vector/Graph
/// explicitly. `record_stored`'s facets write used to *also* call the
/// emitting [`set_facets`] immediately after, so one `Stored` transition left
/// two Text intents where the metadata-upsert intent (`store::apply`'s
/// `CatalogOp::UpsertVersion` arm, fired at `ensure_initialized` track time)
/// had already left the one that matters (see
/// `tests/pipeline_end_to_end.rs::package_is_parsed_once_and_fanned_out`).
fn set_facets_row<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    keywords: Option<&str>,
    quality_ppm: Option<i64>,
    extras_json: Option<&str>,
) -> Result<(), MetaError> {
    let am = facets::ActiveModel {
        version_id: Set(*version.as_uuid()),
        keywords: Set(keywords.map(std::borrow::ToOwned::to_owned)),
        quality_ppm: Set(quality_ppm),
        extras: Set(extras_json.map(std::borrow::ToOwned::to_owned)),
    };
    let stmt = facets::Entity::insert(am)
        .on_conflict(
            OnConflict::column(facets::Column::VersionId)
                .update_columns([
                    facets::Column::Keywords,
                    facets::Column::QualityPpm,
                    facets::Column::Extras,
                ])
                .to_owned(),
        )
        .build(DbBackend::Sqlite);
    engine::exec(engine, stmt)?;
    Ok(())
}

/// Overwrite a version's facet row and emit a Text outbox row.
pub fn set_facets<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    keywords: Option<&str>,
    quality_ppm: Option<i64>,
    extras_json: Option<&str>,
) -> Result<(), MetaError> {
    set_facets_row(engine, version, keywords, quality_ppm, extras_json)?;
    super::apply::emit_outbox_row(
        engine,
        Some(version),
        None,
        SinkKind::Text,
        OutboxOperation::Upsert,
    )
}

/// Overwrite a version's facet row without emitting an outbox row. See
/// [`set_facets_row`] (which this wraps) for why this variant exists.
pub fn set_facets_quiet<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    keywords: Option<&str>,
    quality_ppm: Option<i64>,
    extras_json: Option<&str>,
) -> Result<(), MetaError> {
    set_facets_row(engine, version, keywords, quality_ppm, extras_json)
}

/// Batched form of [`set_facets`]: upsert many versions' facets and emit
/// their outbox rows in **one** transaction — one `INSERT … ON CONFLICT` for
/// the facets rows plus one `INSERT` for the outbox rows, instead of two
/// engine round-trips (each its own SQLite autocommit outside an explicit
/// transaction) per version. Used by `Catalog::refresh_dependents` /
/// `refresh_popularity_percentiles`, which can touch the whole corpus.
///
/// Each row is `(version, keywords, quality_ppm, extras_json)`, matching
/// `set_facets`'s parameters. A no-op for an empty slice.
pub fn set_facets_batch<E: CatalogEngine>(
    engine: &E,
    rows: &[(PackageId, Option<String>, Option<i64>, Option<String>)],
) -> Result<(), MetaError> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut deferred: Option<MetaError> = None;
    let transaction_result = engine.transaction(&mut |tx| {
        let models =
            rows.iter().map(
                |(version, keywords, quality_ppm, extras_json)| facets::ActiveModel {
                    version_id: Set(*version.as_uuid()),
                    keywords: Set(keywords.clone()),
                    quality_ppm: Set(*quality_ppm),
                    extras: Set(extras_json.clone()),
                },
            );
        let stmt = facets::Entity::insert_many(models)
            .on_conflict(
                OnConflict::column(facets::Column::VersionId)
                    .update_columns([
                        facets::Column::Keywords,
                        facets::Column::QualityPpm,
                        facets::Column::Extras,
                    ])
                    .to_owned(),
            )
            .build(DbBackend::Sqlite);
        engine::exec(tx, stmt)?;

        if let Err(error) = super::apply::emit_outbox_rows(
            tx,
            rows.iter().map(|(version, ..)| *version),
            SinkKind::Text,
            OutboxOperation::Upsert,
        ) {
            let engine_error = match error {
                MetaError::Engine(inner) => inner,
                other => {
                    let message = other.to_string();
                    deferred = Some(other);
                    crate::engine::EngineError::Statement(message)
                }
            };
            return Err(engine_error);
        }
        Ok(())
    });
    match transaction_result {
        Ok(()) => Ok(()),
        Err(engine_error) => Err(deferred.unwrap_or(MetaError::Engine(engine_error))),
    }
}

/// A facet row: `(keywords, quality_ppm, extras_json)`.
pub fn facets_for<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<(Option<String>, Option<i64>, Option<String>)>, MetaError> {
    let stmt = facets::Entity::find()
        .filter(facets::Column::VersionId.eq(*version.as_uuid()))
        .select_only()
        .column(facets::Column::Keywords)
        .column(facets::Column::QualityPpm)
        .column(facets::Column::Extras)
        .build(DbBackend::Sqlite);
    let mut rows = engine::query(engine, stmt, &mut |row| {
        Ok((
            row.get_optional_text(0)?,
            row.get_optional_integer(1)?,
            row.get_optional_text(2)?,
        ))
    })?;
    Ok(rows.pop())
}

/// One page of the corpus-wide facet scan.
pub fn scan_version_facets<E: CatalogEngine>(
    engine: &E,
    after: Option<PackageId>,
    limit: u64,
) -> Result<Vec<(PackageId, String, String, Option<String>)>, MetaError> {
    use sea_orm::sea_query::Expr;

    let mut select = versions::Entity::find()
        .join(JoinType::InnerJoin, versions::Relation::Package.def())
        .join(JoinType::LeftJoin, versions::Relation::Facets.def())
        .select_only()
        .expr(Expr::col((versions::Entity, versions::Column::Id)))
        .expr(Expr::col((packages::Entity, packages::Column::Ecosystem)))
        .expr(Expr::col((
            packages::Entity,
            packages::Column::NameCanonical,
        )))
        .expr(Expr::col((facets::Entity, facets::Column::Extras)))
        .order_by_asc(versions::Column::Id)
        .limit(limit);

    if let Some(id) = after {
        select = select.filter(versions::Column::Id.gt(*id.as_uuid()));
    }

    let stmt = select.build(DbBackend::Sqlite);
    engine::query(engine, stmt, &mut |row| {
        let id = version_id_from_blob(&row.get_blob(0)?)?;
        Ok((
            id,
            row.get_text(1)?,
            row.get_text(2)?,
            row.get_optional_text(3)?,
        ))
    })
    .map_err(MetaError::from)
}

/// Drop every edge of one version so the next upsert is the full set.
pub fn delete_version_edges<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<(), MetaError> {
    engine
        .execute(
            "DELETE FROM edges WHERE dependent_version = ?",
            &[crate::engine::Value::Blob(
                version.as_uuid().as_bytes().to_vec(),
            )],
        )
        .map_err(MetaError::from)?;
    Ok(())
}

/// Runtime edges whose `dep_ecosystem` is the dependent package's ecosystem.
///
/// Cross-ecosystem names and non-runtime kinds stay out: the sweep keys
/// targets by the depender's language, so those rows must not increment it.
pub fn scan_runtime_edges<E: CatalogEngine>(
    engine: &E,
) -> Result<Vec<(PackageId, String)>, MetaError> {
    use sea_orm::sea_query::Query;

    let mut select = Query::select();
    select
        .column((edges::Entity, edges::Column::DependentVersion))
        .column((edges::Entity, edges::Column::DepNameCanonical))
        .from(edges::Entity)
        .inner_join(
            versions::Entity,
            Expr::col((edges::Entity, edges::Column::DependentVersion))
                .equals((versions::Entity, versions::Column::Id)),
        )
        .inner_join(
            packages::Entity,
            Expr::col((versions::Entity, versions::Column::StemId))
                .equals((packages::Entity, packages::Column::StemId)),
        )
        .and_where(Expr::col((edges::Entity, edges::Column::Kind)).eq("runtime"))
        .and_where(
            Expr::col((edges::Entity, edges::Column::DepEcosystem))
                .equals((packages::Entity, packages::Column::Ecosystem)),
        );
    crate::engine::stmt::query_select(engine, select, &mut |row| {
        let id = version_id_from_blob(&row.get_blob(0)?)?;
        Ok((id, row.get_text(1)?))
    })
    .map_err(MetaError::from)
}

fn version_id_from_blob(blob: &[u8]) -> Result<PackageId, crate::engine::EngineError> {
    crate::ids::version_id::from_blob(blob)
        .map_err(|e| crate::engine::EngineError::Statement(e.to_string()))
}

/// The newest listing event for a version: `(status_token, reason)`.
pub fn latest_listing<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<(String, Option<String>)>, MetaError> {
    let stmt = listing_events::Entity::find()
        .filter(listing_events::Column::VersionId.eq(*version.as_uuid()))
        .order_by_desc(listing_events::Column::Seq)
        .select_only()
        .column(listing_events::Column::Status)
        .column(listing_events::Column::Reason)
        .limit(1)
        .build(DbBackend::Sqlite);
    let mut rows = engine::query(engine, stmt, &mut |row| {
        Ok((row.get_text(0)?, row.get_optional_text(1)?))
    })?;
    Ok(rows.pop())
}

/// Version row joined with its stem for record reconstruction.
#[allow(clippy::type_complexity)]
pub fn version_record<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<
    Option<(
        String,
        String,
        Option<String>,
        String,
        String,
        String,
        String,
    )>,
    MetaError,
> {
    use sea_orm::sea_query::Expr;

    let stmt = versions::Entity::find()
        .join(JoinType::InnerJoin, versions::Relation::Package.def())
        .filter(versions::Column::Id.eq(*version.as_uuid()))
        .select_only()
        .expr(Expr::col((
            versions::Entity,
            versions::Column::VersionCanonical,
        )))
        .expr(Expr::col((
            versions::Entity,
            versions::Column::VersionOriginal,
        )))
        .expr(Expr::col((versions::Entity, versions::Column::Toolchain)))
        .expr(Expr::col((packages::Entity, packages::Column::Ecosystem)))
        .expr(Expr::col((
            packages::Entity,
            packages::Column::NameCanonical,
        )))
        .expr(Expr::col((
            packages::Entity,
            packages::Column::NameOriginal,
        )))
        .expr(Expr::col((packages::Entity, packages::Column::NameStruct)))
        .build(DbBackend::Sqlite);
    let mut rows = engine::query(engine, stmt, &mut |row| {
        Ok((
            row.get_text(0)?,
            row.get_text(1)?,
            row.get_optional_text(2)?,
            row.get_text(3)?,
            row.get_text(4)?,
            row.get_text(5)?,
            row.get_text(6)?,
        ))
    })?;
    Ok(rows.pop())
}

/// Every version currently in `state`, ascending id order.
pub fn versions_in_state<E: CatalogEngine>(
    engine: &E,
    state: ParseState,
) -> Result<Vec<PackageId>, MetaError> {
    let stmt = versions::Entity::find()
        .filter(versions::Column::ParseState.eq(state))
        .order_by_asc(versions::Column::Id)
        .select_only()
        .column(versions::Column::Id)
        .build(DbBackend::Sqlite);
    engine::query(engine, stmt, &mut |row| row.get_blob(0))?
        .into_iter()
        .map(|blob| {
            crate::ids::version_id::from_blob(&blob)
                .map_err(|e| crate::codec::CodecError::from(e).into())
        })
        .collect()
}

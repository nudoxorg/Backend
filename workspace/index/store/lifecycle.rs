//! Version lifecycle reads/writes for the serving plane (INDEX-PLAN §8).
//!
//! The registry's pipeline state machine (`heart::ResolutionState`) lives in
//! the catalog as the `versions` lifecycle columns (`parse_state`,
//! `parse_phase`, `attempts`, `failure`) plus — for the `Stored` terminal —
//! a `generations` row whose `gen_stamp` **is** the stored content hash
//! (BLOB32). This module is the focused API those consumers drive; the
//! metadata upsert path ([`super::apply`]) deliberately never touches these
//! columns (a feed refresh must not reset a version mid-compile).

use crate::engine::{CatalogEngine, Value};
use crate::enums::TextEnum;
use crate::enums::{IrStatus, OutboxOperation, ParseState, SinkKind};
use crate::ids::{GenerationStamp, PackageId, version_id};
use crate::store::MetaError;
use crate::tables::{facets, generations, symbols_proj, versions};

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
/// Emits a same-transaction `Text` outbox row so search projections observe
/// state flips (ID-3). `bump_attempts` increments `attempts` atomically.
pub fn set_version_lifecycle<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    parse_state: ParseState,
    parse_phase: Option<&str>,
    failure: Option<&str>,
    bump_attempts: bool,
) -> Result<(), MetaError> {
    let sql = format!(
        "UPDATE {table} SET {ps}=?1, {ph}=?2, {f}=?3, {a}={a}+?4 WHERE {id}=?5",
        table = versions::TABLE,
        ps = versions::columns::PARSE_STATE,
        ph = versions::columns::PARSE_PHASE,
        f = versions::columns::FAILURE,
        a = versions::columns::ATTEMPTS,
        id = versions::columns::ID,
    );
    let changed = engine.execute(
        &sql,
        &[
            Value::Text(parse_state.as_token().to_owned()),
            match parse_phase {
                Some(phase) => Value::Text(phase.to_owned()),
                None => Value::Null,
            },
            match failure {
                Some(json) => Value::Text(json.to_owned()),
                None => Value::Null,
            },
            Value::Integer(i64::from(bump_attempts)),
            Value::Blob(version_id::to_blob(&version).to_vec()),
        ],
    )?;
    if changed == 0 {
        return Err(MetaError::MissingRow {
            what: "versions row for lifecycle write",
        });
    }
    super::apply::emit_outbox_row(
        engine,
        Some(version_id::to_blob(&version).to_vec()),
        None,
        SinkKind::Text,
        OutboxOperation::Upsert,
    )
}

/// Read a version's lifecycle columns. `Ok(None)` when the row is absent.
pub fn version_lifecycle<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<VersionLifecycle>, MetaError> {
    let sql = format!(
        "SELECT {ps},{ph},{a},{f} FROM {table} WHERE {id}=?1",
        table = versions::TABLE,
        ps = versions::columns::PARSE_STATE,
        ph = versions::columns::PARSE_PHASE,
        a = versions::columns::ATTEMPTS,
        f = versions::columns::FAILURE,
        id = versions::columns::ID,
    );
    let mut rows = engine.query_rows(
        &sql,
        &[Value::Blob(version_id::to_blob(&version).to_vec())],
        &mut |row| {
            Ok((
                row.get_text(0)?,
                row.get_optional_text(1)?,
                row.get_integer(2)?,
                row.get_optional_text(3)?,
            ))
        },
    )?;
    let Some((state_token, parse_phase, attempts, failure)) = rows.pop() else {
        return Ok(None);
    };
    Ok(Some(VersionLifecycle {
        parse_state: ParseState::from_token(&state_token).map_err(crate::codec::CodecError::from)?,
        parse_phase,
        attempts,
        failure,
    }))
}

/// Record the `Stored` terminal: a `generations` row whose `gen_stamp` is the
/// 32-byte stored content hash, upserted idempotently.
pub fn record_stored_generation<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    content_hash: &[u8; 32],
    sealed_at_unix_milliseconds: i64,
) -> Result<(), MetaError> {
    let sql = format!(
        "INSERT INTO {table} ({gs},{v},{ir},{sa}) VALUES (?1,?2,?3,?4) \
         ON CONFLICT({gs}) DO UPDATE SET {sa}=excluded.{sa}, {ir}=excluded.{ir}",
        table = generations::TABLE,
        gs = generations::columns::GEN_STAMP,
        v = generations::columns::VERSION_ID,
        ir = generations::columns::IR_STATUS,
        sa = generations::columns::SEALED_AT,
    );
    engine.execute(
        &sql,
        &[
            Value::Blob(content_hash.to_vec()),
            Value::Blob(version_id::to_blob(&version).to_vec()),
            Value::Text(IrStatus::Sealed.as_token().to_owned()),
            Value::Integer(sealed_at_unix_milliseconds),
        ],
    )?;
    Ok(())
}

/// The newest sealed generation stamp for a version, if any.
pub fn latest_generation<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<GenerationStamp>, MetaError> {
    let sql = format!(
        "SELECT {gs} FROM {table} WHERE {v}=?1 ORDER BY {sa} DESC LIMIT 1",
        table = generations::TABLE,
        gs = generations::columns::GEN_STAMP,
        v = generations::columns::VERSION_ID,
        sa = generations::columns::SEALED_AT,
    );
    let mut rows = engine.query_rows(
        &sql,
        &[Value::Blob(version_id::to_blob(&version).to_vec())],
        &mut |row| row.get_blob(0),
    )?;
    let Some(blob) = rows.pop() else {
        return Ok(None);
    };
    let stamp = GenerationStamp::from_blob(&blob).map_err(crate::codec::CodecError::from)?;
    Ok(Some(stamp))
}

/// Upsert one serving-projection symbol row (`symbols_proj`).
///
/// The projection is keyed `(gen_stamp, intro_id)`; identity truth stays in
/// the IR plane — this table is wiped with tantivy (INDEX-PLAN §8).
pub fn upsert_symbol_projection<E: CatalogEngine>(
    engine: &E,
    intro_id: &[u8; 32],
    version: PackageId,
    gen_stamp: &[u8; 32],
    moniker: &str,
    kind: &str,
) -> Result<(), MetaError> {
    let sql = format!(
        "INSERT INTO {table} ({ii},{v},{gs},{m},{k}) VALUES (?1,?2,?3,?4,?5) \
         ON CONFLICT({gs},{ii}) DO UPDATE SET {m}=excluded.{m}, {k}=excluded.{k}",
        table = symbols_proj::TABLE,
        ii = symbols_proj::columns::INTRO_ID,
        v = symbols_proj::columns::VERSION_ID,
        gs = symbols_proj::columns::GEN_STAMP,
        m = symbols_proj::columns::MONIKER,
        k = symbols_proj::columns::KIND,
    );
    engine.execute(
        &sql,
        &[
            Value::Blob(intro_id.to_vec()),
            Value::Blob(version_id::to_blob(&version).to_vec()),
            Value::Blob(gen_stamp.to_vec()),
            Value::Text(moniker.to_owned()),
            Value::Text(kind.to_owned()),
        ],
    )?;
    Ok(())
}

/// All projection symbols for a version: `(intro_id, moniker, kind)` rows.
pub fn symbols_for_version<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Vec<(Vec<u8>, String, String)>, MetaError> {
    let sql = format!(
        "SELECT {ii},{m},{k} FROM {table} WHERE {v}=?1 ORDER BY {m}",
        table = symbols_proj::TABLE,
        ii = symbols_proj::columns::INTRO_ID,
        m = symbols_proj::columns::MONIKER,
        k = symbols_proj::columns::KIND,
        v = symbols_proj::columns::VERSION_ID,
    );
    engine
        .query_rows(
            &sql,
            &[Value::Blob(version_id::to_blob(&version).to_vec())],
            &mut |row| Ok((row.get_blob(0)?, row.get_text(1)?, row.get_text(2)?)),
        )
        .map_err(MetaError::from)
}

/// Overwrite a version's facet row (keywords + quality + extras JSON) and
/// emit a same-transaction `Text` outbox row — a facet change is
/// search-relevant, so projections must observe it (ID-3).
pub fn set_facets<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
    keywords: Option<&str>,
    quality_ppm: Option<i64>,
    extras_json: Option<&str>,
) -> Result<(), MetaError> {
    let sql = format!(
        "INSERT INTO {table} ({v},{kw},{q},{ex}) VALUES (?1,?2,?3,?4) \
         ON CONFLICT({v}) DO UPDATE SET {kw}=excluded.{kw}, {q}=excluded.{q}, {ex}=excluded.{ex}",
        table = facets::TABLE,
        v = facets::columns::VERSION_ID,
        kw = facets::columns::KEYWORDS,
        q = facets::columns::QUALITY_PPM,
        ex = facets::columns::EXTRAS,
    );
    engine.execute(
        &sql,
        &[
            Value::Blob(version_id::to_blob(&version).to_vec()),
            match keywords {
                Some(text) => Value::Text(text.to_owned()),
                None => Value::Null,
            },
            match quality_ppm {
                Some(ppm) => Value::Integer(ppm),
                None => Value::Null,
            },
            match extras_json {
                Some(json) => Value::Text(json.to_owned()),
                None => Value::Null,
            },
        ],
    )?;
    super::apply::emit_outbox_row(
        engine,
        Some(version_id::to_blob(&version).to_vec()),
        None,
        SinkKind::Text,
        OutboxOperation::Upsert,
    )
}

/// A facet row read back: `(keywords, quality_ppm, extras_json)`.
pub fn facets_for<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<(Option<String>, Option<i64>, Option<String>)>, MetaError> {
    let sql = format!(
        "SELECT {kw},{q},{ex} FROM {table} WHERE {v}=?1",
        table = facets::TABLE,
        kw = facets::columns::KEYWORDS,
        q = facets::columns::QUALITY_PPM,
        ex = facets::columns::EXTRAS,
        v = facets::columns::VERSION_ID,
    );
    let mut rows = engine.query_rows(
        &sql,
        &[Value::Blob(version_id::to_blob(&version).to_vec())],
        &mut |row| {
            Ok((
                row.get_optional_text(0)?,
                row.get_optional_integer(1)?,
                row.get_optional_text(2)?,
            ))
        },
    )?;
    Ok(rows.pop())
}

/// One page of the corpus-wide facet scan (dependents / popularity sweeps).
///
/// Rows are `(version_id, ecosystem_token, name_canonical, extras_json)` in
/// ascending `version_id` order; pass the last id back as `after` to resume.
pub fn scan_version_facets<E: CatalogEngine>(
    engine: &E,
    after: Option<PackageId>,
    limit: u64,
) -> Result<Vec<(PackageId, String, String, Option<String>)>, MetaError> {
    let sql = format!(
        "SELECT v.{vid}, p.{eco}, p.{name}, f.{ex} FROM {versions} v \
         JOIN {packages} p ON p.{pid} = v.{stem} \
         LEFT JOIN {facets} f ON f.{fv} = v.{vid} \
         WHERE (?1 IS NULL OR v.{vid} > ?1) ORDER BY v.{vid} LIMIT ?2",
        vid = versions::columns::ID,
        eco = crate::tables::packages::columns::ECOSYSTEM,
        name = crate::tables::packages::columns::NAME_CANONICAL,
        ex = facets::columns::EXTRAS,
        versions = versions::TABLE,
        packages = crate::tables::packages::TABLE,
        pid = crate::tables::packages::columns::STEM_ID,
        stem = versions::columns::STEM_ID,
        facets = facets::TABLE,
        fv = facets::columns::VERSION_ID,
    );
    let after_value = match after {
        Some(id) => Value::Blob(version_id::to_blob(&id).to_vec()),
        None => Value::Null,
    };
    engine
        .query_rows(
            &sql,
            &[after_value, Value::Integer(limit as i64)],
            &mut |row| {
                Ok((
                    row.get_blob(0)?,
                    row.get_text(1)?,
                    row.get_text(2)?,
                    row.get_optional_text(3)?,
                ))
            },
        )?
        .into_iter()
        .map(|(id_blob, eco, name, extras)| {
            let id = version_id::from_blob(&id_blob).map_err(crate::codec::CodecError::from)?;
            Ok((id, eco, name, extras))
        })
        .collect()
}

/// The newest listing event for a version: `(status_token, reason)`.
pub fn latest_listing<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<(String, Option<String>)>, MetaError> {
    let sql = format!(
        "SELECT {s},{r} FROM {table} WHERE {v}=?1 ORDER BY {seq} DESC LIMIT 1",
        table = crate::tables::listing::TABLE,
        s = crate::tables::listing::columns::STATUS,
        r = crate::tables::listing::columns::REASON,
        v = crate::tables::listing::columns::VERSION_ID,
        seq = crate::tables::listing::columns::SEQ,
    );
    let mut rows = engine.query_rows(
        &sql,
        &[Value::Blob(version_id::to_blob(&version).to_vec())],
        &mut |row| Ok((row.get_text(0)?, row.get_optional_text(1)?)),
    )?;
    Ok(rows.pop())
}

/// A version row joined with its stem, for record reconstruction:
/// `(version_canonical, version_original, toolchain_json, ecosystem_token,
/// name_canonical, name_original, name_struct)`.
#[allow(clippy::type_complexity)]
pub fn version_record<E: CatalogEngine>(
    engine: &E,
    version: PackageId,
) -> Result<Option<(String, String, Option<String>, String, String, String, String)>, MetaError> {
    let sql = format!(
        "SELECT v.{vc}, v.{vo}, v.{tc}, p.{eco}, p.{nc}, p.{no}, p.{ns} \
         FROM {versions} v JOIN {packages} p ON p.{pid} = v.{stem} WHERE v.{vid}=?1",
        vc = versions::columns::VERSION_CANONICAL,
        vo = versions::columns::VERSION_ORIGINAL,
        tc = versions::columns::TOOLCHAIN,
        eco = crate::tables::packages::columns::ECOSYSTEM,
        nc = crate::tables::packages::columns::NAME_CANONICAL,
        no = crate::tables::packages::columns::NAME_ORIGINAL,
        ns = crate::tables::packages::columns::NAME_STRUCT,
        versions = versions::TABLE,
        packages = crate::tables::packages::TABLE,
        pid = crate::tables::packages::columns::STEM_ID,
        stem = versions::columns::STEM_ID,
        vid = versions::columns::ID,
    );
    let mut rows = engine.query_rows(
        &sql,
        &[Value::Blob(version_id::to_blob(&version).to_vec())],
        &mut |row| {
            Ok((
                row.get_text(0)?,
                row.get_text(1)?,
                row.get_optional_text(2)?,
                row.get_text(3)?,
                row.get_text(4)?,
                row.get_text(5)?,
                row.get_text(6)?,
            ))
        },
    )?;
    Ok(rows.pop())
}

/// Every version currently in `state`, ascending id order.
pub fn versions_in_state<E: CatalogEngine>(
    engine: &E,
    state: ParseState,
) -> Result<Vec<PackageId>, MetaError> {
    let sql = format!(
        "SELECT {id} FROM {table} WHERE {ps}=?1 ORDER BY {id}",
        table = versions::TABLE,
        id = versions::columns::ID,
        ps = versions::columns::PARSE_STATE,
    );
    engine
        .query_rows(
            &sql,
            &[Value::Text(state.as_token().to_owned())],
            &mut |row| row.get_blob(0),
        )?
        .into_iter()
        .map(|blob| version_id::from_blob(&blob).map_err(|e| crate::codec::CodecError::from(e).into()))
        .collect()
}

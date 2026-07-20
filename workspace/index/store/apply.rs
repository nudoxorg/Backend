//! Applying [`CatalogOp`]s (INDEX-PLAN ID-3) and generation registration
//! (ID-15), plus the monotonic sink-watermark advance.
//!
//! [`apply_ops`] runs the **whole batch in one transaction**: every business
//! mutation and the outbox rows it fans out are written together, so a failing
//! op leaves no partial rows and no orphan outbox rows (the transaction rolls
//! back). Statement text is centralized here; table row structs supply column
//! names and value binders.

use crate::engine::{CatalogEngine, Value};
use crate::enums::{OutboxOperation, SinkKind, TextEnum};
use crate::ids::version_id;
use crate::protocol::CatalogOp;
use crate::tables::{
    advisories, aliases, edges, facets, generations, git_watermarks, lineage, listing, outbox,
    packages, repo_facts, sink_watermarks, versions,
};

use super::read::current_watermark;
use super::{ApplyReport, GenerationRegistration, MetaError};

/// Apply a batch of ops atomically, emitting outbox fan-out rows in the same
/// transaction (ID-3). Returns counts for observability.
pub fn apply_ops<E: CatalogEngine>(
    engine: &E,
    ops: &[CatalogOp],
) -> Result<ApplyReport, MetaError> {
    // The transaction closure cannot return our rich MetaError (the facade
    // speaks EngineError), so we stash any typed codec/logic error out-of-band
    // and surface it after the rollback.
    let mut report = ApplyReport::default();
    let mut deferred: Option<MetaError> = None;

    let transaction_result = engine.transaction(&mut |tx| {
        report = ApplyReport::default();
        for op in ops {
            match apply_one(tx, op) {
                Ok(outbox_rows) => {
                    report.applied += 1;
                    report.outbox_rows += outbox_rows;
                }
                Err(error) => {
                    // Convert the failure into an EngineError so the facade rolls
                    // back, remembering the typed error for the caller.
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
            }
        }
        Ok(())
    });

    match transaction_result {
        Ok(()) => Ok(report),
        Err(engine_error) => Err(deferred.unwrap_or(MetaError::Engine(engine_error))),
    }
}

/// Apply one op against the transaction handle, returning how many outbox rows
/// it emitted.
fn apply_one(tx: &dyn CatalogEngine, op: &CatalogOp) -> Result<usize, MetaError> {
    match op {
        CatalogOp::UpsertPackage { stem, repo_url } => {
            let sql = format!(
                "INSERT INTO {table} ({cols}) VALUES (?1,?2,?3,?4,?5,?6,?7) \
                 ON CONFLICT({stem_id}) DO UPDATE SET \
                 {ecosystem}=excluded.{ecosystem}, {name_struct}=excluded.{name_struct}, \
                 {name_canonical}=excluded.{name_canonical}, {name_original}=excluded.{name_original}, \
                 {repo_url}=excluded.{repo_url}",
                table = packages::TABLE,
                cols = packages::PackageRow::INSERT_COLUMNS.join(","),
                stem_id = packages::columns::STEM_ID,
                ecosystem = packages::columns::ECOSYSTEM,
                name_struct = packages::columns::NAME_STRUCT,
                name_canonical = packages::columns::NAME_CANONICAL,
                name_original = packages::columns::NAME_ORIGINAL,
                repo_url = packages::columns::REPO_URL,
            );
            let row = packages::PackageRow {
                stem_id: stem.stem_id,
                ecosystem: stem.ecosystem,
                name_struct: stem.name_struct.clone(),
                name_canonical: stem.name_canonical.clone(),
                name_original: stem.name_original.clone(),
                repo_url: repo_url.clone(),
                created_at: now_placeholder(),
            };
            tx.execute(&sql, &row.bind())?;
            Ok(0)
        }

        CatalogOp::UpsertVersion {
            coordinates,
            published_at,
            toolchain,
            license,
            edges: edge_wires,
            facets: facet_wire,
            source,
        } => {
            upsert_version(
                tx,
                coordinates,
                *published_at,
                toolchain.as_ref(),
                license.as_deref(),
                edge_wires,
                facet_wire,
                source.as_ref(),
            )?;
            // Fan out to the text sink: a new/updated version is searchable.
            emit_outbox_row(
                tx,
                Some(version_id::to_blob(&coordinates.version_id).to_vec()),
                None,
                SinkKind::Text,
                OutboxOperation::Upsert,
            )?;
            Ok(1)
        }

        CatalogOp::SetRepoFacts { stem, facts } => {
            let sql = format!(
                "INSERT INTO {table} ({s},{stars},{last},{arch},{branch},{fetched}) \
                 VALUES (?1,?2,?3,?4,?5,?6) \
                 ON CONFLICT({s}) DO UPDATE SET \
                 {stars}=excluded.{stars}, {last}=excluded.{last}, {arch}=excluded.{arch}, \
                 {branch}=excluded.{branch}, {fetched}=excluded.{fetched}",
                table = repo_facts::TABLE,
                s = repo_facts::columns::STEM_ID,
                stars = repo_facts::columns::STARS,
                last = repo_facts::columns::LAST_ACTIVITY_AT,
                arch = repo_facts::columns::ARCHIVED,
                branch = repo_facts::columns::DEFAULT_BRANCH,
                fetched = repo_facts::columns::FETCHED_AT,
            );
            tx.execute(
                &sql,
                &[
                    Value::Blob(stem.to_blob().to_vec()),
                    crate::codec::bind_optional_integer(facts.stars),
                    crate::codec::bind_optional_integer(facts.last_activity_at),
                    crate::codec::bind_bool(facts.archived),
                    crate::codec::bind_optional_text(facts.default_branch.clone()),
                    Value::Integer(facts.fetched_at),
                ],
            )?;
            Ok(0)
        }

        CatalogOp::SetListing {
            version,
            status,
            valid_from,
            reason,
        } => {
            let sql = format!(
                "INSERT INTO {table} ({v},{s},{r},{vf},{vt},{rec}) VALUES (?1,?2,?3,?4,?5,?6)",
                table = listing::TABLE,
                v = listing::columns::VERSION_ID,
                s = listing::columns::STATUS,
                r = listing::columns::REASON,
                vf = listing::columns::VALID_FROM,
                vt = listing::columns::VALID_TO,
                rec = listing::columns::RECORDED_AT,
            );
            tx.execute(
                &sql,
                &[
                    Value::Blob(version_id::to_blob(version).to_vec()),
                    Value::Text(status.as_token().to_owned()),
                    crate::codec::bind_optional_text(reason.clone()),
                    Value::Integer(*valid_from),
                    Value::Null,
                    Value::Integer(now_placeholder()),
                ],
            )?;
            Ok(0)
        }

        CatalogOp::UpsertAdvisory { advisory } => {
            let sql = format!(
                "INSERT INTO {table} ({id},{s},{vr},{sev},{sum},{url},{vf},{vt},{rec}) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) \
                 ON CONFLICT({id}) DO UPDATE SET \
                 {s}=excluded.{s}, {vr}=excluded.{vr}, {sev}=excluded.{sev}, \
                 {sum}=excluded.{sum}, {url}=excluded.{url}, {vf}=excluded.{vf}, \
                 {vt}=excluded.{vt}, {rec}=excluded.{rec}",
                table = advisories::TABLE,
                id = advisories::columns::ID,
                s = advisories::columns::STEM_ID,
                vr = advisories::columns::VERSION_RANGE,
                sev = advisories::columns::SEVERITY,
                sum = advisories::columns::SUMMARY,
                url = advisories::columns::URL,
                vf = advisories::columns::VALID_FROM,
                vt = advisories::columns::VALID_TO,
                rec = advisories::columns::RECORDED_AT,
            );
            tx.execute(
                &sql,
                &[
                    Value::Blob(advisory.id.to_blob().to_vec()),
                    match advisory.stem_id {
                        Some(stem) => Value::Blob(stem.to_blob().to_vec()),
                        None => Value::Null,
                    },
                    crate::codec::bind_optional_text(advisory.version_range.clone()),
                    crate::codec::bind_optional_text(advisory.severity.clone()),
                    crate::codec::bind_optional_text(advisory.summary.clone()),
                    crate::codec::bind_optional_text(advisory.url.clone()),
                    Value::Integer(advisory.valid_from),
                    crate::codec::bind_optional_integer(advisory.valid_to),
                    Value::Integer(advisory.recorded_at),
                ],
            )?;
            Ok(0)
        }

        CatalogOp::SourceMoved {
            stem,
            rev,
            checked_at,
        } => {
            let sql = format!(
                "INSERT INTO {table} ({s},{lr},{lc},{le}) VALUES (?1,?2,?3,NULL) \
                 ON CONFLICT({s}) DO UPDATE SET {lr}=excluded.{lr}, {lc}=excluded.{lc}",
                table = git_watermarks::TABLE,
                s = git_watermarks::columns::STEM_ID,
                lr = git_watermarks::columns::LAST_REV,
                lc = git_watermarks::columns::LAST_CHECKED_AT,
                le = git_watermarks::columns::LAST_ERROR,
            );
            tx.execute(
                &sql,
                &[
                    Value::Blob(stem.to_blob().to_vec()),
                    Value::Text(rev.0.to_string()),
                    Value::Integer(*checked_at),
                ],
            )?;
            Ok(0)
        }

        CatalogOp::SetIrStatus {
            version,
            status,
            generation,
        } => {
            // Update the version's generation IR status; if a gen stamp is
            // supplied, upsert the generation row's status too.
            if let Some(gen_wire) = generation {
                let sql = format!(
                    "INSERT INTO {table} ({gs},{vid},{ct},{irs}) VALUES (?1,?2,?3,?4) \
                     ON CONFLICT({gs}) DO UPDATE SET {ct}=excluded.{ct}, {irs}=excluded.{irs}",
                    table = generations::TABLE,
                    gs = generations::columns::GEN_STAMP,
                    vid = generations::columns::VERSION_ID,
                    ct = generations::columns::CHANNEL_TIP,
                    irs = generations::columns::IR_STATUS,
                );
                tx.execute(
                    &sql,
                    &[
                        Value::Blob(gen_wire.gen_stamp.to_blob().to_vec()),
                        Value::Blob(version_id::to_blob(version).to_vec()),
                        match gen_wire.channel_tip {
                            Some(tip) => Value::Blob(tip.to_blob().to_vec()),
                            None => Value::Null,
                        },
                        Value::Text(status.as_token().to_owned()),
                    ],
                )?;
            }
            Ok(0)
        }

        CatalogOp::Refresh { stem } => {
            // A refresh only bumps the git watermark's checked time so the
            // monitor re-enumerates; no business rows change.
            let sql = format!(
                "INSERT INTO {table} ({s},{lc}) VALUES (?1,?2) \
                 ON CONFLICT({s}) DO UPDATE SET {lc}=excluded.{lc}",
                table = git_watermarks::TABLE,
                s = git_watermarks::columns::STEM_ID,
                lc = git_watermarks::columns::LAST_CHECKED_AT,
            );
            tx.execute(
                &sql,
                &[
                    Value::Blob(stem.to_blob().to_vec()),
                    Value::Integer(now_placeholder()),
                ],
            )?;
            Ok(0)
        }

        CatalogOp::UpsertAlias {
            ecosystem,
            kind,
            alias,
            stem,
            confidence,
        } => {
            let sql = format!(
                "INSERT INTO {table} ({eco},{ak},{al},{s},{c},{rec}) VALUES (?1,?2,?3,?4,?5,?6) \
                 ON CONFLICT({eco},{ak},{al}) DO UPDATE SET \
                 {s}=excluded.{s}, {c}=excluded.{c}, {rec}=excluded.{rec}",
                table = aliases::TABLE,
                eco = aliases::columns::ECOSYSTEM,
                ak = aliases::columns::ALIAS_KIND,
                al = aliases::columns::ALIAS,
                s = aliases::columns::STEM_ID,
                c = aliases::columns::CONFIDENCE,
                rec = aliases::columns::RECORDED_AT,
            );
            tx.execute(
                &sql,
                &[
                    Value::Text(ecosystem.as_token().to_owned()),
                    Value::Text(kind.to_string()),
                    Value::Text(alias.to_string()),
                    Value::Blob(stem.to_blob().to_vec()),
                    Value::Text(confidence.as_token().to_owned()),
                    Value::Integer(now_placeholder()),
                ],
            )?;
            Ok(0)
        }

        CatalogOp::UpsertLineage {
            stem,
            relation,
            target,
            evidence,
            fork_point_rev,
            overlap_ratio,
            confidence,
        } => {
            let sql = format!(
                "INSERT INTO {table} ({s},{rel},{tgt},{ev},{fpr},{ovr},{c},{rec}) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8) \
                 ON CONFLICT({s},{rel},{tgt}) DO UPDATE SET \
                 {ev}=excluded.{ev}, {fpr}=excluded.{fpr}, {ovr}=excluded.{ovr}, \
                 {c}=excluded.{c}, {rec}=excluded.{rec}",
                table = lineage::TABLE,
                s = lineage::columns::STEM_ID,
                rel = lineage::columns::RELATION,
                tgt = lineage::columns::TARGET_STEM,
                ev = lineage::columns::EVIDENCE,
                fpr = lineage::columns::FORK_POINT_REV,
                ovr = lineage::columns::OVERLAP_RATIO,
                c = lineage::columns::CONFIDENCE,
                rec = lineage::columns::RECORDED_AT,
            );
            tx.execute(
                &sql,
                &[
                    Value::Blob(stem.to_blob().to_vec()),
                    Value::Text(relation.as_token().to_owned()),
                    Value::Blob(target.to_blob().to_vec()),
                    Value::Text(evidence.as_token().to_owned()),
                    match fork_point_rev {
                        Some(rev) => Value::Text(rev.0.to_string()),
                        None => Value::Null,
                    },
                    match overlap_ratio {
                        Some(ratio) => Value::Real(f64::from(*ratio)),
                        None => Value::Null,
                    },
                    Value::Text(confidence.as_token().to_owned()),
                    Value::Integer(now_placeholder()),
                ],
            )?;
            Ok(0)
        }
    }
}

/// Insert the version row plus its edges and facets.
#[allow(clippy::too_many_arguments)]
fn upsert_version(
    tx: &dyn CatalogEngine,
    coordinates: &crate::protocol::VersionCoordinates,
    published_at: Option<i64>,
    toolchain: Option<&crate::protocol::ToolchainRef>,
    license: Option<&str>,
    edge_wires: &[crate::protocol::EdgeWire],
    facet_wire: &crate::protocol::FacetWire,
    source: Option<&crate::protocol::SourceAcquisitionWire>,
) -> Result<(), MetaError> {
    let row = versions::VersionRow {
        id: coordinates.version_id,
        stem_id: coordinates.stem_id,
        version_canonical: coordinates.version_canonical.clone(),
        version_original: coordinates.version_original.clone(),
        published_at,
        toolchain: toolchain.map(|t| t.0.to_string()),
        license_spdx: license.map(|s| s.to_owned()),
        yanked_upstream: false,
        parse_state: crate::enums::ParseState::Pending,
        parse_phase: None,
        attempts: 0,
        failure: None,
        source_kind: source
            .map(|s| s.source_kind)
            .unwrap_or(crate::enums::SourceKind::Unknown),
        source_pack: source.and_then(|s| s.source_pack),
        source_rev: source.and_then(|s| s.source_rev.clone()),
        registry_checksum: source.and_then(|s| s.registry_checksum.clone()),
        registry_package_uri: source.and_then(|s| s.registry_package_uri.clone()),
    };
    let placeholders = (1..=versions::VersionRow::INSERT_COLUMNS.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    // Upsert on the primary key `id`, refreshing every *metadata* column from
    // the incoming row (a bare `stem=excluded.stem` would silently drop all
    // other updated fields on a re-ingest of the same version) — but never the
    // lifecycle columns: a feed refresh must not reset a version that is
    // mid-compile or `Stored` back to `Pending` (see `store::lifecycle`, which
    // is the only writer of those columns after insert).
    const LIFECYCLE_COLUMNS: [&str; 4] = [
        versions::columns::PARSE_STATE,
        versions::columns::PARSE_PHASE,
        versions::columns::ATTEMPTS,
        versions::columns::FAILURE,
    ];
    let update_assignments = versions::VersionRow::INSERT_COLUMNS
        .iter()
        .filter(|column| **column != versions::columns::ID)
        .filter(|column| !LIFECYCLE_COLUMNS.contains(*column))
        .map(|column| format!("{column}=excluded.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {table} ({cols}) VALUES ({placeholders}) \
         ON CONFLICT({id}) DO UPDATE SET {update_assignments}",
        table = versions::TABLE,
        cols = versions::VersionRow::INSERT_COLUMNS.join(","),
        id = versions::columns::ID,
    );
    tx.execute(&sql, &row.bind())?;

    for edge in edge_wires {
        let sql = format!(
            "INSERT INTO {table} ({dv},{de},{dn},{req},{rs},{k},{src}) \
             VALUES (?1,?2,?3,?4,?5,?6,?7) \
             ON CONFLICT({dv},{de},{dn},{k}) DO UPDATE SET \
             {req}=excluded.{req}, {rs}=excluded.{rs}, {src}=excluded.{src}",
            table = edges::TABLE,
            dv = edges::columns::DEPENDENT_VERSION,
            de = edges::columns::DEP_ECOSYSTEM,
            dn = edges::columns::DEP_NAME_CANONICAL,
            req = edges::columns::REQUIREMENT,
            rs = edges::columns::RESOLVED_STEM,
            k = edges::columns::KIND,
            src = edges::columns::SOURCE,
        );
        tx.execute(
            &sql,
            &[
                Value::Blob(version_id::to_blob(&coordinates.version_id).to_vec()),
                Value::Text(edge.dep_ecosystem.as_token().to_owned()),
                Value::Text(edge.dep_name_canonical.clone()),
                Value::Text(edge.requirement.clone()),
                match edge.resolved_stem {
                    Some(stem) => Value::Blob(stem.to_blob().to_vec()),
                    None => Value::Null,
                },
                Value::Text(edge.kind.as_token().to_owned()),
                Value::Text(edge.source.as_token().to_owned()),
            ],
        )?;
    }

    let sql = format!(
        "INSERT INTO {table} ({v},{kw},{q},{ex}) VALUES (?1,?2,?3,?4) \
         ON CONFLICT({v}) DO UPDATE SET {kw}=excluded.{kw}, {q}=excluded.{q}, {ex}=excluded.{ex}",
        table = facets::TABLE,
        v = facets::columns::VERSION_ID,
        kw = facets::columns::KEYWORDS,
        q = facets::columns::QUALITY_PPM,
        ex = facets::columns::EXTRAS,
    );
    tx.execute(
        &sql,
        &[
            Value::Blob(version_id::to_blob(&coordinates.version_id).to_vec()),
            crate::codec::bind_optional_text(facet_wire.keywords.clone()),
            crate::codec::bind_optional_integer(facet_wire.quality_ppm),
            crate::codec::bind_optional_text(facet_wire.extras.clone()),
        ],
    )?;

    let _ = published_at;
    Ok(())
}

/// Insert one outbox row (version- or generation-scoped).
/// Emit one outbox row in the caller's transaction (ID-3). Shared with the
/// lifecycle write path (`store::lifecycle`) and the registry's explicit
/// fan-out intents, which flip state outside `apply_ops` but must fan out
/// identically.
pub fn emit_outbox_row(
    tx: &dyn CatalogEngine,
    version_blob: Option<Vec<u8>>,
    gen_blob: Option<Vec<u8>>,
    sink: SinkKind,
    op: OutboxOperation,
) -> Result<(), MetaError> {
    let sql = format!(
        "INSERT INTO {table} ({cols}) VALUES (?1,?2,?3,?4,?5)",
        table = outbox::TABLE,
        cols = outbox::OutboxRow::INSERT_COLUMNS.join(","),
    );
    tx.execute(
        &sql,
        &[
            match version_blob {
                Some(bytes) => Value::Blob(bytes),
                None => Value::Null,
            },
            match gen_blob {
                Some(bytes) => Value::Blob(bytes),
                None => Value::Null,
            },
            Value::Text(sink.as_token().to_owned()),
            Value::Text(op.as_token().to_owned()),
            Value::Integer(now_placeholder()),
        ],
    )?;
    Ok(())
}

/// Register (or update) a generation row (ID-15). Standalone (not part of an op
/// batch), but still a single statement.
pub fn register_generation<E: CatalogEngine>(
    engine: &E,
    registration: GenerationRegistration,
) -> Result<(), MetaError> {
    let placeholders = (1..=8).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    let sql = format!(
        "INSERT INTO {table} ({gs},{vid},{ct},{jk},{pt},{sa},{irs},{stats}) VALUES ({placeholders}) \
         ON CONFLICT({gs}) DO UPDATE SET \
         {ct}=excluded.{ct}, {jk}=excluded.{jk}, {pt}=excluded.{pt}, \
         {sa}=excluded.{sa}, {irs}=excluded.{irs}, {stats}=excluded.{stats}",
        table = generations::TABLE,
        gs = generations::columns::GEN_STAMP,
        vid = generations::columns::VERSION_ID,
        ct = generations::columns::CHANNEL_TIP,
        jk = generations::columns::JOB_KEY,
        pt = generations::columns::PRODUCER_TOOLCHAIN,
        sa = generations::columns::SEALED_AT,
        irs = generations::columns::IR_STATUS,
        stats = generations::columns::RESOLUTION_STATS,
    );
    engine.execute(
        &sql,
        &[
            Value::Blob(registration.gen_stamp.to_blob().to_vec()),
            Value::Blob(version_id::to_blob(&registration.version_id).to_vec()),
            match registration.channel_tip {
                Some(tip) => Value::Blob(tip.to_blob().to_vec()),
                None => Value::Null,
            },
            match registration.job_key {
                Some(key) => Value::Blob(key.to_blob().to_vec()),
                None => Value::Null,
            },
            crate::codec::bind_optional_text(registration.producer_toolchain),
            crate::codec::bind_optional_integer(registration.sealed_at),
            Value::Text(registration.ir_status.as_token().to_owned()),
            crate::codec::bind_optional_text(registration.resolution_stats),
        ],
    )?;
    Ok(())
}

/// Advance a sink watermark, rejecting a regression (monotonicity guard).
pub fn advance_sink_watermark<E: CatalogEngine>(
    engine: &E,
    sink: SinkKind,
    last_seq: i64,
    updated_at: i64,
) -> Result<(), MetaError> {
    let current = current_watermark(engine, sink)?;
    if last_seq < current {
        return Err(MetaError::WatermarkRegression {
            sink,
            current,
            requested: last_seq,
        });
    }
    let sql = format!(
        "INSERT INTO {table} ({sk},{ls},{ua}) VALUES (?1,?2,?3) \
         ON CONFLICT({sk}) DO UPDATE SET {ls}=excluded.{ls}, {ua}=excluded.{ua}",
        table = sink_watermarks::TABLE,
        sk = sink_watermarks::columns::SINK_KIND,
        ls = sink_watermarks::columns::LAST_SEQ,
        ua = sink_watermarks::columns::UPDATED_AT,
    );
    engine.execute(
        &sql,
        &[
            Value::Text(sink.as_token().to_owned()),
            Value::Integer(last_seq),
            Value::Integer(updated_at),
        ],
    )?;
    Ok(())
}

/// A placeholder "now" for fields the wire op did not carry. The catalog is
/// commit-timestamped by DoltLite; row-level `created_at`/`recorded_at` here are
/// best-effort and monotone within a process. Kept as one function so a real
/// clock injection is a one-line change.
fn now_placeholder() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static CLOCK: AtomicI64 = AtomicI64::new(1);
    CLOCK.fetch_add(1, Ordering::SeqCst)
}

//! Applying [`CatalogOp`]s (INDEX-PLAN ID-3) and generation registration
//! (ID-15), plus the monotonic sink-watermark advance.
//!
//! Writes are SeaORM `ActiveModel` inserts with `OnConflict`; the catalog
//! engine only sees the rendered [`Statement`].

use sea_orm::sea_query::OnConflict;
use sea_orm::{ActiveValue::Set, DbBackend, EntityTrait, QueryTrait};

use crate::engine::{self, CatalogEngine};
use crate::entity::{
    advisories, edges, facets, generations, git_watermarks, listing_events, outbox, packages,
    package_aliases, repo_facts, repo_lineage, sink_watermarks, versions,
};
use crate::enums::{OutboxOperation, ParseState, SinkKind, SourceKind};
use crate::ids::PackageId;
use crate::protocol::CatalogOp;

use super::read::current_watermark;
use super::{ApplyReport, GenerationRegistration, MetaError};

/// Apply a batch of ops atomically, emitting outbox fan-out rows in the same
/// transaction (ID-3).
pub fn apply_ops<E: CatalogEngine>(
    engine: &E,
    ops: &[CatalogOp],
) -> Result<ApplyReport, MetaError> {
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

fn apply_one(tx: &dyn CatalogEngine, op: &CatalogOp) -> Result<usize, MetaError> {
    match op {
        CatalogOp::UpsertPackage { stem, repo_url } => {
            let am = packages::ActiveModel {
                stem_id: Set(stem.stem_id),
                ecosystem: Set(stem.ecosystem.as_token().to_owned()),
                name_struct: Set(stem.name_struct.clone()),
                name_canonical: Set(stem.name_canonical.clone()),
                name_original: Set(stem.name_original.clone()),
                repo_url: Set(repo_url.clone()),
                created_at: Set(now_placeholder()),
            };
            let stmt = packages::Entity::insert(am)
                .on_conflict(
                    OnConflict::column(packages::Column::StemId)
                        .update_columns([
                            packages::Column::Ecosystem,
                            packages::Column::NameStruct,
                            packages::Column::NameCanonical,
                            packages::Column::NameOriginal,
                            packages::Column::RepoUrl,
                        ])
                        .to_owned(),
                )
                .build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
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
            emit_outbox_row(
                tx,
                Some(coordinates.version_id),
                None,
                SinkKind::Text,
                OutboxOperation::Upsert,
            )?;
            Ok(1)
        }

        CatalogOp::SetRepoFacts { stem, facts } => {
            let am = repo_facts::ActiveModel {
                stem_id: Set(*stem),
                stars: Set(facts.stars),
                last_activity_at: Set(facts.last_activity_at),
                archived: Set(facts.archived),
                default_branch: Set(facts.default_branch.clone()),
                fetched_at: Set(facts.fetched_at),
            };
            let stmt = repo_facts::Entity::insert(am)
                .on_conflict(
                    OnConflict::column(repo_facts::Column::StemId)
                        .update_columns([
                            repo_facts::Column::Stars,
                            repo_facts::Column::LastActivityAt,
                            repo_facts::Column::Archived,
                            repo_facts::Column::DefaultBranch,
                            repo_facts::Column::FetchedAt,
                        ])
                        .to_owned(),
                )
                .build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
            Ok(0)
        }

        CatalogOp::SetListing {
            version,
            status,
            valid_from,
            reason,
        } => {
            let am = listing_events::ActiveModel {
                seq: sea_orm::ActiveValue::NotSet,
                version_id: Set(*version.as_uuid()),
                status: Set(*status),
                reason: Set(reason.clone()),
                valid_from: Set(*valid_from),
                valid_to: Set(None),
                recorded_at: Set(now_placeholder()),
            };
            let stmt = listing_events::Entity::insert(am).build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
            Ok(0)
        }

        CatalogOp::UpsertAdvisory { advisory } => {
            let am = advisories::ActiveModel {
                id: Set(advisory.id),
                stem_id: Set(advisory.stem_id),
                version_range: Set(advisory.version_range.clone()),
                severity: Set(advisory.severity.clone()),
                summary: Set(advisory.summary.clone()),
                url: Set(advisory.url.clone()),
                valid_from: Set(advisory.valid_from),
                valid_to: Set(advisory.valid_to),
                recorded_at: Set(advisory.recorded_at),
            };
            let stmt = advisories::Entity::insert(am)
                .on_conflict(
                    OnConflict::column(advisories::Column::Id)
                        .update_columns([
                            advisories::Column::StemId,
                            advisories::Column::VersionRange,
                            advisories::Column::Severity,
                            advisories::Column::Summary,
                            advisories::Column::Url,
                            advisories::Column::ValidFrom,
                            advisories::Column::ValidTo,
                            advisories::Column::RecordedAt,
                        ])
                        .to_owned(),
                )
                .build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
            Ok(0)
        }

        CatalogOp::SourceMoved {
            stem,
            rev,
            checked_at,
        } => {
            let am = git_watermarks::ActiveModel {
                stem_id: Set(*stem),
                last_rev: Set(Some(rev.0.to_string())),
                last_checked_at: Set(*checked_at),
                last_error: Set(None),
            };
            let stmt = git_watermarks::Entity::insert(am)
                .on_conflict(
                    OnConflict::column(git_watermarks::Column::StemId)
                        .update_columns([
                            git_watermarks::Column::LastRev,
                            git_watermarks::Column::LastCheckedAt,
                        ])
                        .to_owned(),
                )
                .build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
            Ok(0)
        }

        CatalogOp::SetIrStatus {
            version,
            status,
            generation,
        } => {
            if let Some(gen_wire) = generation {
                let am = generations::ActiveModel {
                    gen_stamp: Set(gen_wire.gen_stamp),
                    version_id: Set(*version.as_uuid()),
                    channel_tip: Set(gen_wire.channel_tip),
                    job_key: Set(None),
                    producer_toolchain: Set(None),
                    sealed_at: Set(None),
                    ir_status: Set(*status),
                    resolution_stats: Set(None),
                };
                let stmt = generations::Entity::insert(am)
                    .on_conflict(
                        OnConflict::column(generations::Column::GenStamp)
                            .update_columns([
                                generations::Column::ChannelTip,
                                generations::Column::IrStatus,
                            ])
                            .to_owned(),
                    )
                    .build(DbBackend::Sqlite);
                engine::exec(tx, stmt)?;
            }
            Ok(0)
        }

        CatalogOp::Refresh { stem } => {
            let am = git_watermarks::ActiveModel {
                stem_id: Set(*stem),
                last_rev: Set(None),
                last_checked_at: Set(now_placeholder()),
                last_error: Set(None),
            };
            let stmt = git_watermarks::Entity::insert(am)
                .on_conflict(
                    OnConflict::column(git_watermarks::Column::StemId)
                        .update_columns([git_watermarks::Column::LastCheckedAt])
                        .to_owned(),
                )
                .build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
            Ok(0)
        }

        CatalogOp::UpsertAlias {
            ecosystem,
            kind,
            alias,
            stem,
            confidence,
        } => {
            let am = package_aliases::ActiveModel {
                ecosystem: Set(ecosystem.as_token().to_owned()),
                alias_kind: Set(kind.to_string()),
                alias: Set(alias.to_string()),
                stem_id: Set(*stem),
                confidence: Set(*confidence),
                recorded_at: Set(now_placeholder()),
            };
            let stmt = package_aliases::Entity::insert(am)
                .on_conflict(
                    OnConflict::columns([
                        package_aliases::Column::Ecosystem,
                        package_aliases::Column::AliasKind,
                        package_aliases::Column::Alias,
                    ])
                    .update_columns([
                        package_aliases::Column::StemId,
                        package_aliases::Column::Confidence,
                        package_aliases::Column::RecordedAt,
                    ])
                    .to_owned(),
                )
                .build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
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
            let am = repo_lineage::ActiveModel {
                stem_id: Set(*stem),
                relation: Set(*relation),
                target_stem: Set(*target),
                evidence: Set(*evidence),
                fork_point_rev: Set(fork_point_rev.as_ref().map(|r| r.0.to_string())),
                overlap_ratio: Set(overlap_ratio.map(f64::from)),
                confidence: Set(*confidence),
                recorded_at: Set(now_placeholder()),
            };
            let stmt = repo_lineage::Entity::insert(am)
                .on_conflict(
                    OnConflict::columns([
                        repo_lineage::Column::StemId,
                        repo_lineage::Column::Relation,
                        repo_lineage::Column::TargetStem,
                    ])
                    .update_columns([
                        repo_lineage::Column::Evidence,
                        repo_lineage::Column::ForkPointRev,
                        repo_lineage::Column::OverlapRatio,
                        repo_lineage::Column::Confidence,
                        repo_lineage::Column::RecordedAt,
                    ])
                    .to_owned(),
                )
                .build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
            Ok(0)
        }
    }
}

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
    // Metadata upsert only — never touch lifecycle columns on conflict.
    let source_kind = source.map_or(SourceKind::Unknown, |s| s.source_kind);
    let am = versions::ActiveModel {
        id: Set(*coordinates.version_id.as_uuid()),
        stem_id: Set(coordinates.stem_id),
        version_canonical: Set(coordinates.version_canonical.clone()),
        version_original: Set(coordinates.version_original.clone()),
        published_at: Set(published_at),
        toolchain: Set(toolchain.map(|t| t.0.to_string())),
        license_spdx: Set(license.map(std::borrow::ToOwned::to_owned)),
        yanked_upstream: Set(false),
        parse_state: Set(ParseState::Pending),
        parse_phase: Set(None),
        attempts: Set(0),
        failure: Set(None),
        source_kind: Set(source_kind),
        source_pack: Set(source.and_then(|s| s.source_pack)),
        source_rev: Set(source.and_then(|s| s.source_rev.clone())),
        registry_checksum: Set(source.and_then(|s| s.registry_checksum.clone())),
        registry_package_uri: Set(source.and_then(|s| s.registry_package_uri.clone())),
        // W4b conditional-GET cache columns: not part of this metadata-only
        // upsert's contract (see `store::lifecycle::set_archive_cache_meta`
        // for the dedicated writer). `None` only matters for the INSERT arm
        // (a fresh version starts with no cached validators); the UPDATE arm
        // below deliberately omits both columns from `update_columns` so an
        // existing row's cache is never clobbered by a metadata upsert.
        archive_etag: Set(None),
        archive_last_modified: Set(None),
    };
    let stmt = versions::Entity::insert(am)
        .on_conflict(
            OnConflict::column(versions::Column::Id)
                .update_columns([
                    versions::Column::StemId,
                    versions::Column::VersionCanonical,
                    versions::Column::VersionOriginal,
                    versions::Column::PublishedAt,
                    versions::Column::Toolchain,
                    versions::Column::LicenseSpdx,
                    versions::Column::YankedUpstream,
                    versions::Column::SourceKind,
                    versions::Column::SourcePack,
                    versions::Column::SourceRev,
                    versions::Column::RegistryChecksum,
                    versions::Column::RegistryPackageUri,
                ])
                .to_owned(),
        )
        .build(DbBackend::Sqlite);
    engine::exec(tx, stmt)?;

    for edge in edge_wires {
        let am = edges::ActiveModel {
            dependent_version: Set(*coordinates.version_id.as_uuid()),
            dep_ecosystem: Set(edge.dep_ecosystem.as_token().to_owned()),
            dep_name_canonical: Set(edge.dep_name_canonical.clone()),
            kind: Set(edge.kind),
            requirement: Set(edge.requirement.clone()),
            resolved_stem: Set(edge.resolved_stem),
            source: Set(edge.source),
        };
        let stmt = edges::Entity::insert(am)
            .on_conflict(
                OnConflict::columns([
                    edges::Column::DependentVersion,
                    edges::Column::DepEcosystem,
                    edges::Column::DepNameCanonical,
                    edges::Column::Kind,
                ])
                .update_columns([
                    edges::Column::Requirement,
                    edges::Column::ResolvedStem,
                    edges::Column::Source,
                ])
                .to_owned(),
            )
            .build(DbBackend::Sqlite);
        engine::exec(tx, stmt)?;
    }

    let am = facets::ActiveModel {
        version_id: Set(*coordinates.version_id.as_uuid()),
        keywords: Set(facet_wire.keywords.clone()),
        quality_ppm: Set(facet_wire.quality_ppm),
        extras: Set(facet_wire.extras.clone()),
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
    engine::exec(tx, stmt)?;

    Ok(())
}

/// Emit one outbox row in the caller's transaction (ID-3).
pub fn emit_outbox_row(
    tx: &dyn CatalogEngine,
    version: Option<PackageId>,
    gen_stamp: Option<crate::ids::GenerationStamp>,
    sink: SinkKind,
    op: OutboxOperation,
) -> Result<(), MetaError> {
    let am = outbox::ActiveModel {
        seq: sea_orm::ActiveValue::NotSet,
        version_id: Set(version.map(|id| *id.as_uuid())),
        gen_stamp: Set(gen_stamp),
        sink_kind: Set(sink),
        op: Set(op),
        created_at: Set(now_placeholder()),
    };
    let stmt = outbox::Entity::insert(am).build(DbBackend::Sqlite);
    engine::exec(tx, stmt)?;
    Ok(())
}

/// Register (or update) a generation row (ID-15).
pub fn register_generation<E: CatalogEngine>(
    engine: &E,
    registration: GenerationRegistration,
) -> Result<(), MetaError> {
    let am = generations::ActiveModel {
        gen_stamp: Set(registration.gen_stamp),
        version_id: Set(*registration.version_id.as_uuid()),
        channel_tip: Set(registration.channel_tip),
        job_key: Set(registration.job_key),
        producer_toolchain: Set(registration.producer_toolchain),
        sealed_at: Set(registration.sealed_at),
        ir_status: Set(registration.ir_status),
        resolution_stats: Set(registration.resolution_stats),
    };
    let stmt = generations::Entity::insert(am)
        .on_conflict(
            OnConflict::column(generations::Column::GenStamp)
                .update_columns([
                    generations::Column::ChannelTip,
                    generations::Column::JobKey,
                    generations::Column::ProducerToolchain,
                    generations::Column::SealedAt,
                    generations::Column::IrStatus,
                    generations::Column::ResolutionStats,
                ])
                .to_owned(),
        )
        .build(DbBackend::Sqlite);
    engine::exec(engine, stmt)?;
    Ok(())
}

/// Advance a sink watermark, rejecting a regression.
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
    let am = sink_watermarks::ActiveModel {
        sink_kind: Set(sink),
        last_seq: Set(last_seq),
        updated_at: Set(updated_at),
    };
    let stmt = sink_watermarks::Entity::insert(am)
        .on_conflict(
            OnConflict::column(sink_watermarks::Column::SinkKind)
                .update_columns([
                    sink_watermarks::Column::LastSeq,
                    sink_watermarks::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .build(DbBackend::Sqlite);
    engine::exec(engine, stmt)?;
    Ok(())
}

fn now_placeholder() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static CLOCK: AtomicI64 = AtomicI64::new(1);
    CLOCK.fetch_add(1, Ordering::SeqCst)
}

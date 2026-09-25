//! Applying [`CatalogOp`]s (INDEX-PLAN ID-3) and generation registration
//! (ID-15), plus the monotonic sink-watermark advance.
//!
//! Writes are SeaORM `ActiveModel` inserts with `OnConflict`; the catalog
//! engine only sees the rendered [`Statement`].

use sea_orm::{
    ActiveValue::Set,
    DbBackend, EntityTrait, QueryTrait,
    sea_query::{Alias, BinOper, Expr, OnConflict},
};

use crate::{
    engine::{self, CatalogEngine, Value},
    entity::{
        advisories, edges, feed_watermarks, generations, git_watermarks, listing_events, outbox,
        package_aliases, packages, repo_facts, repo_lineage, sink_watermarks,
    },
    enums::{OutboxOperation, SinkKind, SourceKind, TextEnum},
    ids::PackageId,
    protocol::{CatalogOp, VersionDelta},
};

use super::{ApplyReport, GenerationRegistration, MetaError, read::current_watermark};

/// Apply a batch of ops atomically, emitting outbox fan-out rows in the same
/// transaction (ID-3).
///
/// Ops are dispatched in **runs**: maximal stretches of consecutive ops that
/// share a [`CatalogOp`] variant (same variant ⇒ same target table(s) and the
/// same `OnConflict` clause). Each run is written with one `insert_many` per
/// table instead of one `insert` per op, which is what made a 20-op ingest
/// batch cost 7.59ms of `apply_and_commit` — every op was its own prepared
/// statement + round trip through the engine's locked connection, even
/// though all 20 were already inside one transaction.
///
/// Runs are never merged across a variant boundary, so op order (and hence
/// final state, when two ops in a batch touch the same row) is unchanged
/// from the fully sequential form: within a run, `insert_many(..).on_conflict`
/// applies its rows in the given order, so an earlier row's conflict-update
/// is still overwritten by a later row's, exactly as issuing them as
/// separate statements would. Cross-table ordering *within* a run (e.g.
/// `UpsertVersion`'s versions → edges → facets → outbox) is preserved too:
/// each table still gets its own statement, issued in the same relative
/// order as before, just once per run instead of once per op.
pub fn apply_ops<E: CatalogEngine>(
    engine: &E,
    ops: &[CatalogOp],
) -> Result<ApplyReport, MetaError> {
    let mut report = ApplyReport::default();
    let mut deferred: Option<MetaError> = None;

    let transaction_result = engine.transaction(&mut |tx| {
        report = ApplyReport::default();
        let mut start = 0;
        while start < ops.len() {
            let variant = std::mem::discriminant(&ops[start]);
            let mut end = start + 1;
            while end < ops.len() && std::mem::discriminant(&ops[end]) == variant {
                end += 1;
            }
            let run = &ops[start..end];
            match apply_run(tx, run) {
                Ok(outbox_rows) => {
                    report.applied += run.len();
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
            start = end;
        }
        Ok(())
    });

    match transaction_result {
        Ok(()) => Ok(report),
        Err(engine_error) => Err(deferred.unwrap_or(MetaError::Engine(engine_error))),
    }
}

/// Apply one run of same-variant ops, returning the outbox rows fanned out.
/// `run` is guaranteed non-empty and every element shares `run[0]`'s variant
/// (enforced by `apply_ops`'s grouping loop), so each arm can destructure
/// every element with the same pattern.
fn apply_run(tx: &dyn CatalogEngine, run: &[CatalogOp]) -> Result<usize, MetaError> {
    match &run[0] {
        CatalogOp::UpsertPackage { .. } => {
            let models = run.iter().map(|op| {
                let CatalogOp::UpsertPackage { stem, repo_url } = op else {
                    unreachable!("run is homogeneous by construction")
                };
                packages::ActiveModel {
                    stem_id: Set(stem.stem_id),
                    ecosystem: Set(stem.ecosystem.as_token().to_owned()),
                    name_struct: Set(stem.name_struct.clone()),
                    name_canonical: Set(stem.name_canonical.clone()),
                    name_original: Set(stem.name_original.clone()),
                    repo_url: Set(repo_url.clone()),
                    created_at: Set(now_placeholder()),
                }
            });
            let stmt = packages::Entity::insert_many(models)
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

        CatalogOp::UpsertVersion { .. } => {
            let changed = upsert_versions_batch(tx, run)?;
            emit_outbox_rows(
                tx,
                changed.iter().copied(),
                SinkKind::Text,
                OutboxOperation::Upsert,
            )?;
            Ok(changed.len())
        }

        CatalogOp::VersionDelta { .. } => {
            let mut outbox_rows = 0;
            for op in run {
                let CatalogOp::VersionDelta { delta } = op else {
                    unreachable!("run is homogeneous by construction")
                };
                match delta {
                    VersionDelta::Added { version } | VersionDelta::Changed { version } => {
                        let upsert = CatalogOp::UpsertVersion {
                            coordinates: version.coordinates.clone(),
                            published_at: version.published_at,
                            toolchain: version.toolchain.clone(),
                            license: version.license.clone(),
                            edges: version.edges.clone(),
                            facets: version.facets.clone(),
                            source: version.source.clone(),
                        };
                        let changed = upsert_versions_batch(tx, &[upsert])?;
                        emit_outbox_rows(
                            tx,
                            changed.iter().copied(),
                            SinkKind::Text,
                            OutboxOperation::Upsert,
                        )?;
                        outbox_rows += changed.len();
                    }
                    VersionDelta::Removed { version_id, .. } => {
                        let version_blob = crate::ids::version_id::to_blob(version_id).to_vec();
                        let uuid_blob = version_id.as_uuid().as_bytes().to_vec();
                        tx.execute("DELETE FROM edges WHERE dependent_version = ?", &[
                            Value::Blob(uuid_blob.clone()),
                        ])?;
                        tx.execute("DELETE FROM facets WHERE version_id = ?", &[Value::Blob(
                            uuid_blob,
                        )])?;
                        tx.execute("DELETE FROM versions WHERE id = ?", &[Value::Blob(
                            version_blob,
                        )])?;
                        emit_outbox_row(
                            tx,
                            Some(*version_id),
                            None,
                            SinkKind::Text,
                            OutboxOperation::Delete,
                        )?;
                        outbox_rows += 1;
                    }
                }
            }
            Ok(outbox_rows)
        }

        CatalogOp::SetRepoFacts { .. } => {
            let models = run.iter().map(|op| {
                let CatalogOp::SetRepoFacts { stem, facts } = op else {
                    unreachable!("run is homogeneous by construction")
                };
                repo_facts::ActiveModel {
                    stem_id: Set(*stem),
                    stars: Set(facts.stars),
                    last_activity_at: Set(facts.last_activity_at),
                    archived: Set(facts.archived),
                    default_branch: Set(facts.default_branch.clone()),
                    fetched_at: Set(facts.fetched_at),
                }
            });
            let stmt = repo_facts::Entity::insert_many(models)
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

        CatalogOp::SetListing { .. } => {
            let models = run.iter().map(|op| {
                let CatalogOp::SetListing {
                    version,
                    status,
                    valid_from,
                    reason,
                } = op
                else {
                    unreachable!("run is homogeneous by construction")
                };
                listing_events::ActiveModel {
                    seq: sea_orm::ActiveValue::NotSet,
                    version_id: Set(*version.as_uuid()),
                    status: Set(*status),
                    reason: Set(reason.clone()),
                    valid_from: Set(*valid_from),
                    valid_to: Set(None),
                    recorded_at: Set(now_placeholder()),
                }
            });
            // No `OnConflict`: every listing event is an append (bitemporal
            // log), matching the original single-row `insert`.
            let stmt = listing_events::Entity::insert_many(models).build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
            Ok(0)
        }

        CatalogOp::UpsertAdvisory { .. } => {
            let models = run.iter().map(|op| {
                let CatalogOp::UpsertAdvisory { advisory } = op else {
                    unreachable!("run is homogeneous by construction")
                };
                advisories::ActiveModel {
                    id: Set(advisory.id),
                    stem_id: Set(advisory.stem_id),
                    version_range: Set(advisory.version_range.clone()),
                    severity: Set(advisory.severity.clone()),
                    summary: Set(advisory.summary.clone()),
                    url: Set(advisory.url.clone()),
                    valid_from: Set(advisory.valid_from),
                    valid_to: Set(advisory.valid_to),
                    recorded_at: Set(advisory.recorded_at),
                }
            });
            let stmt = advisories::Entity::insert_many(models)
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

        CatalogOp::SourceMoved { .. } => {
            let models = run.iter().map(|op| {
                let CatalogOp::SourceMoved {
                    stem,
                    rev,
                    checked_at,
                } = op
                else {
                    unreachable!("run is homogeneous by construction")
                };
                git_watermarks::ActiveModel {
                    stem_id: Set(*stem),
                    last_rev: Set(Some(rev.0.to_string())),
                    last_checked_at: Set(*checked_at),
                    last_error: Set(None),
                }
            });
            let stmt = git_watermarks::Entity::insert_many(models)
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

        CatalogOp::SetIrStatus { .. } => {
            // Only ops carrying a generation write anything (see `apply_one`'s
            // original `if let Some(gen_wire) = generation`); ops without one
            // are dropped from the batch, same as before.
            let models: Vec<_> = run
                .iter()
                .filter_map(|op| {
                    let CatalogOp::SetIrStatus {
                        version,
                        status,
                        generation,
                    } = op
                    else {
                        unreachable!("run is homogeneous by construction")
                    };
                    let gen_wire = generation.as_ref()?;
                    Some(generations::ActiveModel {
                        gen_stamp: Set(gen_wire.gen_stamp),
                        version_id: Set(*version.as_uuid()),
                        channel_tip: Set(gen_wire.channel_tip),
                        job_key: Set(None),
                        producer_toolchain: Set(None),
                        sealed_at: Set(None),
                        ir_status: Set(*status),
                        resolution_stats: Set(None),
                    })
                })
                .collect();
            if !models.is_empty() {
                let stmt = generations::Entity::insert_many(models)
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

        CatalogOp::Refresh { .. } => {
            let models = run.iter().map(|op| {
                let CatalogOp::Refresh { stem } = op else {
                    unreachable!("run is homogeneous by construction")
                };
                git_watermarks::ActiveModel {
                    stem_id: Set(*stem),
                    last_rev: Set(None),
                    last_checked_at: Set(now_placeholder()),
                    last_error: Set(None),
                }
            });
            let stmt = git_watermarks::Entity::insert_many(models)
                .on_conflict(
                    OnConflict::column(git_watermarks::Column::StemId)
                        .update_columns([git_watermarks::Column::LastCheckedAt])
                        .to_owned(),
                )
                .build(DbBackend::Sqlite);
            engine::exec(tx, stmt)?;
            Ok(0)
        }

        CatalogOp::UpsertAlias { .. } => {
            let models = run.iter().map(|op| {
                let CatalogOp::UpsertAlias {
                    ecosystem,
                    kind,
                    alias,
                    stem,
                    confidence,
                } = op
                else {
                    unreachable!("run is homogeneous by construction")
                };
                package_aliases::ActiveModel {
                    ecosystem: Set(ecosystem.as_token().to_owned()),
                    alias_kind: Set(kind.to_string()),
                    alias: Set(alias.to_string()),
                    stem_id: Set(*stem),
                    confidence: Set(*confidence),
                    recorded_at: Set(now_placeholder()),
                }
            });
            let stmt = package_aliases::Entity::insert_many(models)
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

        CatalogOp::UpsertLineage { .. } => {
            let models = run.iter().map(|op| {
                let CatalogOp::UpsertLineage {
                    stem,
                    relation,
                    target,
                    evidence,
                    fork_point_rev,
                    overlap_ratio,
                    confidence,
                } = op
                else {
                    unreachable!("run is homogeneous by construction")
                };
                repo_lineage::ActiveModel {
                    stem_id: Set(*stem),
                    relation: Set(*relation),
                    target_stem: Set(*target),
                    evidence: Set(*evidence),
                    fork_point_rev: Set(fork_point_rev.as_ref().map(|r| r.0.to_string())),
                    overlap_ratio: Set(overlap_ratio.map(f64::from)),
                    confidence: Set(*confidence),
                    recorded_at: Set(now_placeholder()),
                }
            });
            let stmt = repo_lineage::Entity::insert_many(models)
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

/// Apply one [`EdgeSnapshot`](crate::protocol::EdgeSnapshot) to the SQL edge
/// table.
///
/// [`EdgeSnapshot::Unobserved`](crate::protocol::EdgeSnapshot::Unobserved)
/// writes nothing. A replace deletes kinds the snapshot owns and upserts its
/// wires. Unchanged requirement, stem, and source rows stay put.
pub(crate) fn write_edge_snapshot(
    tx: &dyn CatalogEngine,
    version: PackageId,
    snapshot: &crate::protocol::EdgeSnapshot,
) -> Result<bool, MetaError> {
    let crate::protocol::EdgeSnapshot::Replace { kinds, wires } = snapshot else {
        return Ok(false);
    };
    let removed = delete_replaced_edges(tx, version, kinds, wires)?;
    let dependent_version = *version.as_uuid();
    let models: Vec<_> = wires
        .iter()
        .filter(|edge| kinds.contains(&edge.kind))
        .map(|edge| edges::ActiveModel {
            dependent_version: Set(dependent_version),
            dep_ecosystem: Set(edge.dep_ecosystem.as_token().to_owned()),
            dep_name_canonical: Set(edge.dep_name_canonical.clone()),
            kind: Set(edge.kind),
            requirement: Set(edge.requirement.clone()),
            resolved_stem: Set(edge.resolved_stem),
            source: Set(edge.source),
        })
        .collect();
    if models.is_empty() {
        return Ok(removed != 0);
    }
    let stmt = edges::Entity::insert_many(models)
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
            .action_and_where(edge_payload_differs())
            .to_owned(),
        )
        .build(DbBackend::Sqlite);
    let affected = engine::exec(tx, stmt)?;
    Ok(affected != 0 || removed != 0)
}

/// Delete stored edges of `kinds` that `wires` no longer name.
///
/// Kinds outside the snapshot stay. An empty wire list clears `kinds`.
fn delete_replaced_edges(
    tx: &dyn CatalogEngine,
    version: PackageId,
    kinds: &[crate::enums::EdgeKind],
    wires: &[crate::protocol::EdgeWire],
) -> Result<usize, MetaError> {
    if kinds.is_empty() {
        return Ok(0);
    }
    let mut sql = String::from("DELETE FROM edges WHERE dependent_version = ? AND kind IN (");
    let mut params = vec![Value::Blob(version.as_uuid().as_bytes().to_vec())];
    for (index, kind) in kinds.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
        params.push(Value::text(kind.as_token()));
    }
    sql.push(')');
    let kept: Vec<_> = wires
        .iter()
        .filter(|wire| kinds.contains(&wire.kind))
        .collect();
    if !kept.is_empty() {
        sql.push_str(" AND NOT (");
        for (index, wire) in kept.iter().enumerate() {
            if index > 0 {
                sql.push_str(" OR ");
            }
            sql.push_str("(dep_ecosystem = ? AND dep_name_canonical = ? AND kind = ?)");
            params.push(Value::text(wire.dep_ecosystem.as_token()));
            params.push(Value::text(&wire.dep_name_canonical));
            params.push(Value::text(wire.kind.as_token()));
        }
        sql.push(')');
    }
    tx.execute(&sql, &params).map_err(MetaError::from)
}

fn edge_payload_differs() -> sea_orm::sea_query::SimpleExpr {
    let excluded = |column| Expr::col((Alias::new("excluded"), column));
    let differs = |column| Expr::col(column).binary(BinOper::IsNot, excluded(column));
    differs(edges::Column::Requirement)
        .or(differs(edges::Column::ResolvedStem))
        .or(differs(edges::Column::Source))
}

/// Batch-write the `versions`, `edges`, and `facets` rows for a run of
/// `UpsertVersion` ops: one `insert_many` per table (edges flattened across
/// every op in the run) instead of `1 + edges.len() + 1` statements per op.
/// `run` must be non-empty and every element `UpsertVersion` (enforced by
/// `apply_run`'s caller).
fn upsert_versions_batch(
    tx: &dyn CatalogEngine,
    run: &[CatalogOp],
) -> Result<Vec<PackageId>, MetaError> {
    // Use a conditional upsert so SQLite reports zero affected rows when the
    // re-enumerated metadata is byte-for-byte unchanged. This is the delta
    // boundary for the outbox: only versions whose catalog metadata changed
    // need another projection upsert.
    let mut changed = Vec::with_capacity(run.len());
    for op in run {
        let CatalogOp::UpsertVersion {
            coordinates,
            published_at,
            toolchain,
            license,
            source,
            ..
        } = op
        else {
            unreachable!("run is homogeneous by construction")
        };
        let source_kind = source
            .as_ref()
            .map_or(SourceKind::Unknown, |s| s.source_kind);
        let source_pack = source.as_ref().and_then(|s| s.source_pack);
        let source_rev = source.as_ref().and_then(|s| s.source_rev.as_deref());
        let registry_checksum = source.as_ref().and_then(|s| s.registry_checksum.as_deref());
        let registry_package_uri = source
            .as_ref()
            .and_then(|s| s.registry_package_uri.as_deref());
        let params = vec![
            Value::Blob(crate::ids::version_id::to_blob(&coordinates.version_id).to_vec()),
            Value::Blob(coordinates.stem_id.to_blob().to_vec()),
            Value::text(&coordinates.version_canonical),
            Value::text(&coordinates.version_original),
            published_at.map_or(Value::Null, Value::Integer),
            Value::from_optional(toolchain.as_ref(), |t| Value::text(t.0.to_string())),
            Value::from_optional(license.as_ref(), |v| Value::text(v.clone())),
            Value::Integer(0),
            Value::text(source_kind.as_token()),
            Value::from_optional(source_pack, |v| Value::Blob(v.to_blob().to_vec())),
            Value::from_optional(source_rev, Value::text),
            Value::from_optional(registry_checksum, Value::text),
            Value::from_optional(registry_package_uri, Value::text),
        ];
        let affected = tx.execute(
            "INSERT INTO versions (
                id, stem_id, version_canonical, version_original, published_at,
                toolchain, license_spdx, yanked_upstream, source_kind,
                source_pack, source_rev, registry_checksum, registry_package_uri,
                parse_state, parse_phase, attempts, failure
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', NULL, 0, NULL)
            ON CONFLICT(id) DO UPDATE SET
                stem_id = excluded.stem_id,
                version_canonical = excluded.version_canonical,
                version_original = excluded.version_original,
                published_at = excluded.published_at,
                toolchain = excluded.toolchain,
                license_spdx = excluded.license_spdx,
                yanked_upstream = excluded.yanked_upstream,
                source_kind = excluded.source_kind,
                source_pack = excluded.source_pack,
                source_rev = excluded.source_rev,
                registry_checksum = excluded.registry_checksum,
                registry_package_uri = excluded.registry_package_uri
            WHERE versions.stem_id IS NOT excluded.stem_id
               OR versions.version_canonical IS NOT excluded.version_canonical
               OR versions.version_original IS NOT excluded.version_original
               OR versions.published_at IS NOT excluded.published_at
               OR versions.toolchain IS NOT excluded.toolchain
               OR versions.license_spdx IS NOT excluded.license_spdx
               OR versions.yanked_upstream IS NOT excluded.yanked_upstream
               OR versions.source_kind IS NOT excluded.source_kind
               OR versions.source_pack IS NOT excluded.source_pack
               OR versions.source_rev IS NOT excluded.source_rev
               OR versions.registry_checksum IS NOT excluded.registry_checksum
               OR versions.registry_package_uri IS NOT excluded.registry_package_uri",
            &params,
        )?;
        if affected != 0 {
            changed.push(coordinates.version_id);
        }
    }

    // One statement per version. [`EdgeSnapshot::Unobserved`] leaves stored
    // edges alone. [`EdgeSnapshot::Replace`] is the complete set for its
    // kinds, so an empty wire list clears those kinds and no others.
    let mut edges_by_version: Vec<(PackageId, crate::protocol::EdgeSnapshot)> = Vec::new();
    for op in run {
        let CatalogOp::UpsertVersion {
            coordinates,
            edges: snapshot,
            ..
        } = op
        else {
            unreachable!("run is homogeneous by construction")
        };
        if matches!(snapshot, crate::protocol::EdgeSnapshot::Unobserved) {
            continue;
        }
        if let Some((_, existing)) = edges_by_version
            .iter_mut()
            .find(|(id, _)| *id == coordinates.version_id)
        {
            *existing = snapshot.clone();
        } else {
            edges_by_version.push((coordinates.version_id, snapshot.clone()));
        }
    }
    for (version, snapshot) in edges_by_version {
        if write_edge_snapshot(tx, version, &snapshot)? && !changed.contains(&version) {
            changed.push(version);
        }
    }

    // Facets are part of the version's search projection too. A plain
    // `ON CONFLICT DO UPDATE` would overwrite them correctly but would not
    // tell the delta boundary that a facet-only change occurred. Keep the
    // conditional write inside the caller's transaction and merge its
    // affected-row signal with the version metadata signal above.
    for op in run {
        let CatalogOp::UpsertVersion {
            coordinates,
            facets: facet_wire,
            ..
        } = op
        else {
            unreachable!("run is homogeneous by construction")
        };
        let facet_params = vec![
            Value::Blob(coordinates.version_id.as_uuid().as_bytes().to_vec()),
            Value::from_optional(facet_wire.keywords.as_deref(), Value::text),
            Value::from_optional(facet_wire.quality_ppm, Value::Integer),
            Value::from_optional(facet_wire.extras.as_deref(), Value::text),
        ];
        let affected = tx.execute(
            "INSERT INTO facets (version_id, keywords, quality_ppm, extras)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(version_id) DO UPDATE SET
                 keywords = excluded.keywords,
                 quality_ppm = excluded.quality_ppm,
                 extras = excluded.extras
             WHERE facets.keywords IS NOT excluded.keywords
                OR facets.quality_ppm IS NOT excluded.quality_ppm
                OR facets.extras IS NOT excluded.extras",
            &facet_params,
        )?;
        if affected != 0 && !changed.contains(&coordinates.version_id) {
            changed.push(coordinates.version_id);
        }
    }

    Ok(changed)
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

/// Batched form of [`emit_outbox_row`]: emit one outbox row per `version` in
/// a single `INSERT` instead of one round-trip (and, outside an explicit
/// transaction, one autocommit) per row. All rows share `sink`/`op` and carry
/// no `gen_stamp`, matching every current facet-refresh caller; extend the
/// signature if a caller ever needs per-row sink/op/gen_stamp.
pub fn emit_outbox_rows(
    tx: &dyn CatalogEngine,
    versions: impl IntoIterator<Item = PackageId>,
    sink: SinkKind,
    op: OutboxOperation,
) -> Result<(), MetaError> {
    let models: Vec<outbox::ActiveModel> = versions
        .into_iter()
        .map(|version| outbox::ActiveModel {
            seq: sea_orm::ActiveValue::NotSet,
            version_id: Set(Some(*version.as_uuid())),
            gen_stamp: Set(None),
            sink_kind: Set(sink),
            op: Set(op),
            created_at: Set(now_placeholder()),
        })
        .collect();
    if models.is_empty() {
        return Ok(());
    }
    let stmt = outbox::Entity::insert_many(models).build(DbBackend::Sqlite);
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

/// Record a git poll that saw the same ref digest.
///
/// This is the catalog clock for an unchanged tick. It does not emit a version
/// row or an outbox intent. The caller commits the batch after this returns.
pub fn record_git_checked<E: CatalogEngine>(
    engine: &E,
    stem: crate::ids::PackageStemId,
    rev: &str,
    checked_at: i64,
) -> Result<(), MetaError> {
    let model = git_watermarks::ActiveModel {
        stem_id: Set(stem),
        last_rev: Set(Some(rev.to_owned())),
        last_checked_at: Set(checked_at),
        last_error: Set(None),
    };
    let stmt = git_watermarks::Entity::insert(model)
        .on_conflict(
            OnConflict::column(git_watermarks::Column::StemId)
                .update_columns([
                    git_watermarks::Column::LastRev,
                    git_watermarks::Column::LastCheckedAt,
                ])
                .to_owned(),
        )
        .build(DbBackend::Sqlite);
    engine::exec(engine, stmt)?;
    Ok(())
}

/// Persist a feed crawl cursor. No outbox row. The caller commits the batch.
pub fn record_feed_cursor<E: CatalogEngine>(
    engine: &E,
    watermark: &crate::ingest::watermark::FeedWatermark,
) -> Result<(), MetaError> {
    let model = feed_watermarks::ActiveModel {
        feed: Set(watermark.feed.clone()),
        last_ref: Set(watermark.last_ref.clone()),
        last_checked_at: Set(watermark.last_checked_at),
        last_error: Set(watermark.last_error.clone()),
    };
    let stmt = feed_watermarks::Entity::insert(model)
        .on_conflict(
            OnConflict::column(feed_watermarks::Column::Feed)
                .update_columns([
                    feed_watermarks::Column::LastRef,
                    feed_watermarks::Column::LastCheckedAt,
                    feed_watermarks::Column::LastError,
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

//! Snapshot-bound, keyset-paged reads over the durable package graph.
//!
//! This lives separately from `graph.rs` so the writer and its transactional
//! projection contract can evolve independently from package page reads.

use crate::schema::PackageGraphMetadata;
use crate::{ProjectionError, TursoProjection};
use backend_library::{
    CheckedPackageGraphFacts, DependencyAuthority, DependencyEvidence, DependencyFacts,
    DependencyScope, MAX_PACKAGE_GRAPH_AUTHORITIES, PackageDependencyRecord,
    PackageDependencyTarget, PackageGraphControl, PackageGraphCursor, PackageGraphDirection,
    PackageGraphKnowledge, PackageGraphPage, PackageGraphPageError, PackageGraphPageRequest,
    PackageGraphPageTerminal, PackageGraphSourceAuthority, PackageGraphSourceKey, PackageReference,
    ProductText,
};

/// Failure while reading one bounded package graph page.
#[derive(Debug)]
pub enum PackageGraphReadError {
    /// The local SQL projection was unavailable or corrupt.
    Projection(ProjectionError),
    /// The request or its opaque cursor failed semantic admission.
    Request(PackageGraphPageError),
    /// A cursor names an older selected graph snapshot.
    StaleCursor,
}

impl std::fmt::Display for PackageGraphReadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Projection(error) => write!(formatter, "{error}"),
            Self::Request(error) => write!(formatter, "{error}"),
            Self::StaleCursor => formatter
                .write_str("package graph changed since this cursor was issued; restart the read"),
        }
    }
}

impl std::error::Error for PackageGraphReadError {}

impl From<ProjectionError> for PackageGraphReadError {
    fn from(value: ProjectionError) -> Self {
        Self::Projection(value)
    }
}

impl From<turso::Error> for PackageGraphReadError {
    fn from(value: turso::Error) -> Self {
        Self::Projection(ProjectionError::from(value))
    }
}

impl From<PackageGraphPageError> for PackageGraphReadError {
    fn from(value: PackageGraphPageError) -> Self {
        Self::Request(value)
    }
}

impl TursoProjection {
    /// Reads one dependency or dependent page in a single SQLite read
    /// transaction, bound to the selected view root and dependency witness.
    ///
    /// The SQL query fetches at most `limit + 1` edge keys. It never decodes a
    /// complete neighbor set before slicing, and the continuation uses the
    /// last returned edge id as its keyset boundary.
    ///
    /// A standalone Turso read is a local-cache read. It checks row identities
    /// and bounded source state, but its metadata witness alone cannot prove
    /// that no valid row was inserted or removed. Command-path authentication
    /// depends on the production adapter comparing the returned page with the
    /// resident `CheckedPackageGraphFacts` snapshot before exposing it to
    /// clients.
    pub async fn read_package_graph_page(
        &self,
        request: &PackageGraphPageRequest,
    ) -> Result<PackageGraphPage, PackageGraphReadError> {
        request.admit()?;
        let tx = self.connection.unchecked_transaction().await?;
        let Some(metadata) = crate::graph::package_graph_metadata_from(&tx).await? else {
            tx.rollback().await?;
            return if request.cursor.is_some() {
                Err(PackageGraphReadError::StaleCursor)
            } else {
                Err(ProjectionError::StaleTransition.into())
            };
        };
        let view_root = fixed_root(&metadata)?;
        if let Some(cursor) = &request.cursor
            && (cursor.schema != backend_library::PACKAGE_GRAPH_PAGE_SCHEMA
                || cursor.view_root != view_root
                || cursor.facts_witness != metadata.facts_witness
                || cursor.catalog_snapshot != request.catalog_snapshot)
        {
            tx.rollback().await?;
            return Err(PackageGraphReadError::StaleCursor);
        }

        let selection = match request.direction {
            PackageGraphDirection::Dependencies => {
                select_source(&tx, request, request.cursor.as_ref()).await?
            }
            PackageGraphDirection::Dependents => SourceSelection::NotApplicable,
        };
        if request.cursor.is_some() && matches!(&selection, SourceSelection::Missing) {
            tx.rollback().await?;
            return Err(PackageGraphReadError::StaleCursor);
        }

        let selected_source = match &selection {
            SourceSelection::Exact(source) => Some(source),
            SourceSelection::NotApplicable
            | SourceSelection::Missing
            | SourceSelection::Ambiguous(_) => None,
        };
        if let Some(cursor) = &request.cursor {
            let recipe = request.recipe(selected_source);
            if cursor.recipe != recipe || cursor.source.as_ref() != selected_source {
                tx.rollback().await?;
                return Err(PackageGraphPageError::CursorMismatch.into());
            }
        }

        if request.control == PackageGraphControl::Cancel {
            let page = empty_page(
                view_root,
                metadata.facts_witness,
                request.catalog_snapshot,
                request,
                selected_source.cloned(),
                PackageGraphKnowledge::Unknown { reason: None },
                PackageGraphPageTerminal::Cancelled,
            );
            page.admit_for(request)?;
            tx.rollback().await?;
            return Ok(page);
        }

        let (knowledge, source, rows, more) = match request.direction {
            PackageGraphDirection::Dependencies => match selection {
                SourceSelection::Missing => (
                    PackageGraphKnowledge::Unknown {
                        reason: Some(product_text(
                            "no dependency source is recorded for this package",
                        )),
                    },
                    None,
                    Vec::new(),
                    false,
                ),
                SourceSelection::Ambiguous(sources) => (
                    PackageGraphKnowledge::Ambiguous {
                        sources: sources.into_boxed_slice(),
                    },
                    None,
                    Vec::new(),
                    false,
                ),
                SourceSelection::NotApplicable => {
                    tx.rollback().await?;
                    return Err(PackageGraphPageError::PageShape.into());
                }
                SourceSelection::Exact(selected) => {
                    let state = package_state(&tx, &selected).await?;
                    match state {
                        Some((1, reason)) => (
                            PackageGraphKnowledge::Unknown {
                                reason: Some(product_text(&reason)),
                            },
                            Some(selected.clone()),
                            Vec::new(),
                            false,
                        ),
                        Some((2, reason)) => (
                            PackageGraphKnowledge::Unavailable {
                                reason: product_text(&reason),
                            },
                            Some(selected.clone()),
                            Vec::new(),
                            false,
                        ),
                        Some((_, _)) => {
                            tx.rollback().await?;
                            return Err(ProjectionError::Database(turso::Error::Misuse(
                                "invalid package graph state kind".to_owned(),
                            ))
                            .into());
                        }
                        None => {
                            let (rows, more) = forward_edges(
                                &tx,
                                &selected,
                                request.cursor.as_ref().map(|value| value.after_edge_id),
                                request.limit,
                            )
                            .await?;
                            (PackageGraphKnowledge::Known, Some(selected), rows, more)
                        }
                    }
                }
            },
            PackageGraphDirection::Dependents => {
                let (rows, more) = match &request.package {
                    PackageReference::Purl(_) => {
                        reverse_edges(
                            &tx,
                            &request.package,
                            request.cursor.as_ref().map(|value| value.after_edge_id),
                            request.limit,
                        )
                        .await?
                    }
                    PackageReference::Local(_) => (Vec::new(), false),
                };
                let knowledge = if matches!(&request.package, PackageReference::Local(_)) {
                    PackageGraphKnowledge::Unknown {
                        reason: Some(product_text(
                            "dependent lookup requires a versioned registry package coordinate",
                        )),
                    }
                } else {
                    let coverage = reverse_knowledge(&tx).await?;
                    match coverage {
                        PackageGraphKnowledge::Unknown {
                            reason: Some(reason),
                        } if !rows.is_empty() || more => PackageGraphKnowledge::Partial {
                            reason,
                            unavailable: false,
                        },
                        PackageGraphKnowledge::Unavailable { reason }
                            if !rows.is_empty() || more =>
                        {
                            PackageGraphKnowledge::Partial {
                                reason,
                                unavailable: true,
                            }
                        }
                        other => other,
                    }
                };
                (knowledge, None, rows, more)
            }
        };

        let terminal = if more {
            let Some(after_edge_id) = rows.last().map(|row| row.facts_version) else {
                tx.rollback().await?;
                return Err(ProjectionError::Database(turso::Error::Misuse(
                    "package graph page has a continuation without a row".to_owned(),
                ))
                .into());
            };
            PackageGraphPageTerminal::More(PackageGraphCursor {
                schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
                view_root,
                facts_witness: metadata.facts_witness,
                catalog_snapshot: request.catalog_snapshot,
                recipe: request.recipe(source.as_ref()),
                source: source.clone(),
                after_edge_id,
            })
        } else {
            PackageGraphPageTerminal::Complete
        };
        let page = PackageGraphPage {
            schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
            view_root,
            facts_witness: metadata.facts_witness,
            catalog_snapshot: request.catalog_snapshot,
            package: request.package.clone(),
            direction: request.direction,
            source,
            knowledge,
            rows: rows.into_boxed_slice(),
            terminal,
        };
        page.admit_for(request)?;
        tx.rollback().await?;
        Ok(page)
    }
}

#[derive(Clone)]
enum SourceSelection {
    NotApplicable,
    Missing,
    Exact(PackageGraphSourceKey),
    Ambiguous(Vec<PackageGraphSourceKey>),
}

async fn select_source(
    connection: &turso::Connection,
    request: &PackageGraphPageRequest,
    cursor: Option<&PackageGraphCursor>,
) -> Result<SourceSelection, PackageGraphReadError> {
    if let Some(cursor) = cursor
        && let Some(source) = &cursor.source
    {
        if source.coordinate != request.package || !exact_source_present(connection, source).await?
        {
            return Ok(SourceSelection::Missing);
        }
        return Ok(SourceSelection::Exact(source.clone()));
    }
    if let Some(authority) = request.authority {
        let source = PackageGraphSourceKey::new(request.package.clone(), authority);
        return Ok(if exact_source_present(connection, &source).await? {
            SourceSelection::Exact(source)
        } else {
            SourceSelection::Missing
        });
    }
    let mut sources = source_inventory(connection, &request.package).await?;
    Ok(match sources.len() {
        0 => SourceSelection::Missing,
        1 => SourceSelection::Exact(sources.remove(0)),
        _ => SourceSelection::Ambiguous(sources),
    })
}

async fn exact_source_present(
    connection: &turso::Connection,
    source: &PackageGraphSourceKey,
) -> Result<bool, PackageGraphReadError> {
    let selected = source_present(connection, "backend_projection_package_sources", source).await?;
    let witnessed = source_present(
        connection,
        "backend_projection_package_source_witnesses",
        source,
    )
    .await?;
    if selected != witnessed {
        return Err(misuse("package graph source inventory differs from its witness index").into());
    }
    Ok(selected)
}

async fn source_present(
    connection: &turso::Connection,
    table: &'static str,
    source: &PackageGraphSourceKey,
) -> Result<bool, ProjectionError> {
    let sql = format!(
        "SELECT 1 FROM {table} WHERE source=?1 AND source_authority_kind=?2 \
         AND source_authority_id=?3 LIMIT 1"
    );
    let mut rows = connection
        .query(
            &sql,
            turso::params![
                source.coordinate.as_str(),
                source.authority.kind_tag(),
                source.authority.id_bytes().as_slice()
            ],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Confirms that the lookup inventory and its checked per-source witness
/// index contain the same bounded set of authorities for this coordinate.
/// This catches a deleted source-presence row before it can turn an existing
/// source into a misleading Missing result.
async fn source_inventory(
    connection: &turso::Connection,
    coordinate: &PackageReference,
) -> Result<Vec<PackageGraphSourceKey>, PackageGraphReadError> {
    let source_keys =
        source_authorities_for_table(connection, "backend_projection_package_sources", coordinate)
            .await?;
    let witness_keys = source_authorities_for_table(
        connection,
        "backend_projection_package_source_witnesses",
        coordinate,
    )
    .await?;
    if source_keys != witness_keys {
        return Err(misuse("package graph source inventory differs from its witness index").into());
    }
    Ok(source_keys)
}

async fn source_authorities_for_table(
    connection: &turso::Connection,
    table: &'static str,
    coordinate: &PackageReference,
) -> Result<Vec<PackageGraphSourceKey>, PackageGraphReadError> {
    // `table` is selected only from the two constant names above. Both tables
    // have the same primary-key order, so the per-coordinate query stays
    // bounded and index-backed.
    let sql = format!(
        "SELECT source_authority_kind, source_authority_id FROM {table} \
         WHERE source=?1 ORDER BY source_authority_kind, source_authority_id LIMIT ?2"
    );
    let mut rows = connection
        .query(
            &sql,
            turso::params![
                coordinate.as_str(),
                i64::try_from(MAX_PACKAGE_GRAPH_AUTHORITIES + 1).unwrap_or(i64::MAX)
            ],
        )
        .await?;
    let mut sources = Vec::new();
    while let Some(row) = rows.next().await? {
        if sources.len() == MAX_PACKAGE_GRAPH_AUTHORITIES {
            drop(rows);
            return Err(PackageGraphPageError::AuthorityFanout.into());
        }
        sources.push(PackageGraphSourceKey::new(
            coordinate.clone(),
            decode_authority(row.get(0)?, row.get(1)?)?,
        ));
    }
    drop(rows);
    Ok(sources)
}

async fn forward_edges(
    connection: &turso::Connection,
    source: &PackageGraphSourceKey,
    after: Option<[u8; 32]>,
    limit: u16,
) -> Result<(Vec<PackageDependencyRecord>, bool), ProjectionError> {
    let mut rows = if let Some(after) = after {
        connection
            .query(
                "SELECT edge_id, source, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE source=?1 \
                 AND source_authority_kind=?2 AND source_authority_id=?3 AND edge_id>?4 \
                 ORDER BY edge_id LIMIT ?5",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice(),
                    after.as_slice(),
                    i64::from(limit) + 1
                ],
            )
            .await?
    } else {
        connection
            .query(
                "SELECT edge_id, source, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE source=?1 \
                 AND source_authority_kind=?2 AND source_authority_id=?3 \
                 ORDER BY edge_id LIMIT ?4",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice(),
                    i64::from(limit) + 1
                ],
            )
            .await?
    };
    read_edge_page(&mut rows, limit).await
}

async fn reverse_edges(
    connection: &turso::Connection,
    target: &PackageReference,
    after: Option<[u8; 32]>,
    limit: u16,
) -> Result<(Vec<PackageDependencyRecord>, bool), ProjectionError> {
    let PackageReference::Purl(target_url) = target else {
        return Ok((Vec::new(), false));
    };
    let ecosystem = target_url
        .package_type()
        .registry()
        .map_or(0, |value| value as u8);
    let mut rows = if let Some(after) = after {
        connection
            .query(
                "SELECT edge_id, source, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE target_ecosystem=?1 \
                 AND target_name=?2 AND (resolved IS NULL OR resolved=?3) \
                 AND scope IN (0,1) AND edge_id>?4 ORDER BY edge_id LIMIT ?5",
                turso::params![
                    i64::from(ecosystem),
                    target_url.lineage_name(),
                    target.as_str(),
                    after.as_slice(),
                    i64::from(limit) + 1
                ],
            )
            .await?
    } else {
        connection
            .query(
                "SELECT edge_id, source, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE target_ecosystem=?1 \
                 AND target_name=?2 AND (resolved IS NULL OR resolved=?3) \
                 AND scope IN (0,1) ORDER BY edge_id LIMIT ?4",
                turso::params![
                    i64::from(ecosystem),
                    target_url.lineage_name(),
                    target.as_str(),
                    i64::from(limit) + 1
                ],
            )
            .await?
    };
    read_edge_page(&mut rows, limit).await
}

async fn read_edge_page(
    rows: &mut turso::Rows,
    limit: u16,
) -> Result<(Vec<PackageDependencyRecord>, bool), ProjectionError> {
    let mut edges = Vec::with_capacity(usize::from(limit));
    let mut more = false;
    while let Some(row) = rows.next().await? {
        if edges.len() == usize::from(limit) {
            more = true;
            break;
        }
        edges.push(decode_edge(&row).await?);
    }
    Ok((edges, more))
}

async fn package_state(
    connection: &turso::Connection,
    source: &PackageGraphSourceKey,
) -> Result<Option<(i64, String)>, ProjectionError> {
    let mut witness_rows = connection
        .query(
            "SELECT facts_witness FROM backend_projection_package_source_witnesses \
             WHERE source=?1 AND source_authority_kind=?2 AND source_authority_id=?3",
            turso::params![
                source.coordinate.as_str(),
                source.authority.kind_tag(),
                source.authority.id_bytes().as_slice()
            ],
        )
        .await?;
    let Some(witness_row) = witness_rows.next().await? else {
        return Err(misuse("package graph source witness is missing"));
    };
    let witness = fixed_32(witness_row.get(0)?, "graph_source_facts_witness")?;
    drop(witness_rows);

    let mut rows = connection
        .query(
            "SELECT state, reason FROM backend_projection_package_states \
             WHERE source=?1 AND source_authority_kind=?2 AND source_authority_id=?3",
            turso::params![
                source.coordinate.as_str(),
                source.authority.kind_tag(),
                source.authority.id_bytes().as_slice()
            ],
        )
        .await?;
    let row = rows.next().await?;
    drop(rows);
    let Some(row) = row else {
        // A missing state row means Known. Accept that interpretation for an
        // empty source only when its checked witness is exactly the canonical
        // Known-empty witness. For a nonempty source, one indexed edge probe
        // is enough to distinguish it from a deleted Unknown/Unavailable
        // state row. Edge membership beyond this probe remains uncommitted by
        // a Merkle index and cannot be proven by a bounded page read.
        let known_empty = checked_source_witness(source, DependencyFacts::Known(Box::new([])))?;
        if witness == known_empty {
            return Ok(None);
        }
        let mut edge_rows = connection
            .query(
                "SELECT 1 FROM backend_projection_package_edges WHERE source=?1 \
                 AND source_authority_kind=?2 AND source_authority_id=?3 LIMIT 1",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice()
                ],
            )
            .await?;
        let has_edge = edge_rows.next().await?.is_some();
        drop(edge_rows);
        if has_edge {
            return Ok(None);
        }
        return Err(misuse(
            "package graph state is missing and its witness is not Known-empty",
        ));
    };
    let state = row.get::<i64>(0)?;
    let reason = row.get::<String>(1)?;
    let facts = match state {
        1 => DependencyFacts::Unknown(
            ProductText::new(reason.clone())
                .map_err(|_| misuse("invalid package graph state reason"))?,
        ),
        2 => DependencyFacts::Unavailable(
            ProductText::new(reason.clone())
                .map_err(|_| misuse("invalid package graph state reason"))?,
        ),
        _ => return Err(misuse("invalid package graph state kind")),
    };
    if checked_source_witness(source, facts)? != witness {
        return Err(misuse(
            "package graph state differs from its checked source witness",
        ));
    }
    Ok(Some((state, reason)))
}

fn checked_source_witness(
    source: &PackageGraphSourceKey,
    facts: DependencyFacts<Box<[PackageDependencyRecord]>>,
) -> Result<[u8; 32], ProjectionError> {
    let checked = CheckedPackageGraphFacts::new(vec![(source.clone(), facts)])
        .map_err(|_| misuse("invalid package graph source witness input"))?;
    checked
        .source_witnesses()
        .first()
        .copied()
        .ok_or_else(|| misuse("package graph source witness input is empty"))
}

async fn reverse_knowledge(
    connection: &turso::Connection,
) -> Result<PackageGraphKnowledge, ProjectionError> {
    // A negative dependent answer is exhaustive only when every source has an
    // answer. Probe the bounded state index instead of decoding all sources.
    let mut gaps = connection
        .query(
            "SELECT state, reason FROM backend_projection_package_states \
             WHERE state IN (1,2) ORDER BY source, source_authority_kind, source_authority_id LIMIT 1",
            (),
        )
        .await?;
    if let Some(row) = gaps.next().await? {
        let state = row.get::<i64>(0)?;
        let reason = product_text(&row.get::<String>(1)?);
        return Ok(if state == 1 {
            PackageGraphKnowledge::Unknown {
                reason: Some(reason),
            }
        } else {
            PackageGraphKnowledge::Unavailable { reason }
        });
    }
    Ok(PackageGraphKnowledge::Known)
}

async fn decode_edge(row: &turso::Row) -> Result<PackageDependencyRecord, ProjectionError> {
    let source = PackageReference::parse(row.get::<String>(1)?)
        .map_err(|_| misuse("invalid graph source"))?;
    let source_authority = decode_authority(row.get(2)?, row.get(3)?)?;
    let ecosystem =
        backend_library::RegistryEcosystem::parse_canonical(match row.get::<i64>(4)? {
            1 => "cargo",
            2 => "npm",
            3 => "pypi",
            4 => "maven",
            5 => "nuget",
            6 => "golang",
            7 => "cpp",
            _ => return Err(misuse("invalid graph ecosystem")),
        })
        .map_err(|_| misuse("invalid graph ecosystem"))?;
    let name = product_text(&row.get::<String>(5)?);
    let requirement = product_text(&row.get::<String>(6)?);
    let resolved = row
        .get::<Option<String>>(7)?
        .map(PackageReference::parse)
        .transpose()
        .map_err(|_| misuse("invalid graph resolution"))?;
    let target =
        PackageDependencyTarget::new(ecosystem, name.as_str(), requirement.as_str(), resolved)
            .map_err(|_| misuse("invalid graph target"))?;
    let scope = match row.get::<i64>(8)? {
        0 => DependencyScope::Runtime,
        1 => DependencyScope::Optional,
        2 => DependencyScope::Development,
        3 => DependencyScope::Build,
        4 => DependencyScope::Peer,
        _ => return Err(misuse("invalid graph scope")),
    };
    let optional = match row.get::<i64>(9)? {
        0 => false,
        1 => true,
        _ => return Err(misuse("invalid graph optional flag")),
    };
    let authority = match row.get::<i64>(10)? {
        0 => DependencyAuthority::RegistryMetadata,
        1 => DependencyAuthority::ArchiveManifest,
        2 => DependencyAuthority::ForgeManifest,
        3 => DependencyAuthority::LocalManifest,
        _ => return Err(misuse("invalid graph evidence authority")),
    };
    let frontier = fixed_32(row.get(11)?, "graph frontier")?;
    let provenance = fixed_32(row.get(12)?, "graph provenance")?;
    let facts_version = fixed_32(row.get(13)?, "graph facts version")?;
    let edge_id = fixed_32(row.get(0)?, "graph edge id")?;
    if edge_id != facts_version {
        return Err(misuse("graph edge id differs from its facts identity"));
    }
    if !source_authority.matches_evidence(authority) {
        return Err(misuse("graph evidence does not match its source authority"));
    }
    let record = PackageDependencyRecord {
        source,
        source_authority,
        target,
        scope,
        optional,
        evidence: DependencyEvidence {
            authority,
            frontier,
            provenance,
        },
        facts_version,
    };
    if record.recomputed_version() != facts_version {
        return Err(misuse("graph edge payload differs from its facts identity"));
    }
    Ok(record)
}

fn decode_authority(
    kind: i64,
    id: Vec<u8>,
) -> Result<PackageGraphSourceAuthority, ProjectionError> {
    let bytes = fixed_32(id, "graph source authority")?;
    match kind {
        0 if bytes == [0; 32] => Ok(PackageGraphSourceAuthority::Unattributed),
        1 => Ok(PackageGraphSourceAuthority::Registry(
            backend_library::RegistryAuthorityId::from_configured_source(bytes),
        )),
        2 => Ok(PackageGraphSourceAuthority::Forge(bytes)),
        3 => Ok(PackageGraphSourceAuthority::Archive(bytes)),
        4 => Ok(PackageGraphSourceAuthority::Local(bytes)),
        _ => Err(misuse("invalid package graph source authority")),
    }
}

fn fixed_root(metadata: &PackageGraphMetadata) -> Result<[u8; 32], ProjectionError> {
    fixed_32(metadata.root.clone(), "graph root")
}

fn fixed_32(value: Vec<u8>, field: &'static str) -> Result<[u8; 32], ProjectionError> {
    value
        .try_into()
        .map_err(|_| ProjectionError::CorruptMetadata { field })
}

fn product_text(value: &str) -> ProductText {
    match ProductText::new(value) {
        Ok(value) => value,
        Err(_) => ProductText::from_static("no reason was recorded"),
    }
}

fn misuse(message: &'static str) -> ProjectionError {
    ProjectionError::Database(turso::Error::Misuse(message.to_owned()))
}

fn empty_page(
    view_root: [u8; 32],
    facts_witness: [u8; 32],
    catalog_snapshot: Option<[u8; 32]>,
    request: &PackageGraphPageRequest,
    source: Option<PackageGraphSourceKey>,
    knowledge: PackageGraphKnowledge,
    terminal: PackageGraphPageTerminal,
) -> PackageGraphPage {
    PackageGraphPage {
        schema: backend_library::PACKAGE_GRAPH_PAGE_SCHEMA,
        view_root,
        facts_witness,
        catalog_snapshot,
        package: request.package.clone(),
        direction: request.direction,
        source,
        knowledge,
        rows: Box::new([]),
        terminal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_library::{
        DependencyFacts, PackageDependencySourceFacts, RegistryAuthorityId, RegistryEcosystem,
        view_state_root,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_GRAPH_PAGE_PATH: AtomicU64 = AtomicU64::new(0);

    fn database_path() -> std::path::PathBuf {
        let serial = NEXT_GRAPH_PAGE_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "backend-turso-package-graph-page-{}-{serial}.db",
            std::process::id()
        ))
    }

    fn remove_database(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(std::path::PathBuf::from(format!(
                "{}{suffix}",
                path.display()
            )));
        }
    }

    async fn assert_query_plan(
        connection: &turso::Connection,
        label: &str,
        sql: &str,
        params: impl turso::IntoParams,
        expected_index: &str,
    ) {
        let mut rows = connection
            .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
            .await
            .unwrap_or_else(|error| panic!("{label}: {error}"));
        let mut details = Vec::new();
        while let Some(row) = rows.next().await.expect("plan row") {
            details.push(row.get::<String>(3).expect("plan detail"));
        }
        assert!(
            details.iter().any(|detail| detail.contains(expected_index)),
            "{label} is index-backed: {details:#?}"
        );
        assert!(
            details.iter().all(|detail| !detail.contains("TEMP B-TREE")),
            "{label} preserves key order without a temporary sort: {details:#?}"
        );
    }

    #[test]
    fn first_and_keyset_pages_use_turso_edge_indexes_without_sorting() {
        futures_executor::block_on(async {
            let path = database_path();
            let projection = TursoProjection::open(&path).await.expect("projection");
            let coordinate = "pkg:cargo/toml@0.8.23";
            let authority = [0x31; 32];
            let after = [0x72; 32];
            assert_query_plan(
                &projection.connection,
                "forward first page",
                "SELECT edge_id FROM backend_projection_package_edges \
                 WHERE source=?1 AND source_authority_kind=?2 AND source_authority_id=?3 \
                 ORDER BY edge_id LIMIT ?4",
                turso::params![coordinate, 1_i64, authority.as_slice(), 65_i64],
                "backend_projection_package_edges_source",
            )
            .await;
            assert_query_plan(
                &projection.connection,
                "forward continuation",
                "SELECT edge_id FROM backend_projection_package_edges \
                 WHERE source=?1 AND source_authority_kind=?2 AND source_authority_id=?3 \
                 AND edge_id>?4 ORDER BY edge_id LIMIT ?5",
                turso::params![
                    coordinate,
                    1_i64,
                    authority.as_slice(),
                    after.as_slice(),
                    65_i64
                ],
                "backend_projection_package_edges_source",
            )
            .await;
            assert_query_plan(
                &projection.connection,
                "reverse first page",
                "SELECT edge_id FROM backend_projection_package_edges \
                 WHERE target_ecosystem=?1 AND target_name=?2 \
                 AND (resolved IS NULL OR resolved=?3) AND scope IN (0,1) \
                 ORDER BY edge_id LIMIT ?4",
                turso::params![1_i64, "toml", coordinate, 65_i64],
                "backend_projection_package_edges_target_page",
            )
            .await;
            assert_query_plan(
                &projection.connection,
                "reverse continuation",
                "SELECT edge_id FROM backend_projection_package_edges \
                 WHERE target_ecosystem=?1 AND target_name=?2 \
                 AND (resolved IS NULL OR resolved=?3) AND scope IN (0,1) \
                 AND edge_id>?4 ORDER BY edge_id LIMIT ?5",
                turso::params![1_i64, "toml", coordinate, after.as_slice(), 65_i64],
                "backend_projection_package_edges_target_page",
            )
            .await;
            drop(projection);
            remove_database(&path);
        });
    }

    fn registry_authority(byte: u8) -> PackageGraphSourceAuthority {
        PackageGraphSourceAuthority::Registry(RegistryAuthorityId::from_configured_source(
            [byte; 32],
        ))
    }

    fn package(value: &str) -> PackageReference {
        PackageReference::parse(value).expect("valid package coordinate")
    }

    fn edge(
        source: &PackageReference,
        authority: PackageGraphSourceAuthority,
        name: &str,
        requirement: &str,
        resolved: Option<&str>,
        scope: DependencyScope,
        frontier: u8,
    ) -> PackageDependencyRecord {
        PackageDependencyRecord::new_with_source_authority(
            source.clone(),
            authority,
            PackageDependencyTarget::new(
                RegistryEcosystem::Cargo,
                name,
                requirement,
                resolved.map(package),
            )
            .expect("valid edge target"),
            scope,
            false,
            DependencyEvidence {
                authority: DependencyAuthority::RegistryMetadata,
                frontier: [frontier; 32],
                provenance: [frontier.wrapping_add(1); 32],
            },
        )
    }

    async fn projection_with_facts(
        facts: &[PackageDependencySourceFacts],
        root_byte: u8,
    ) -> (std::path::PathBuf, TursoProjection) {
        let path = database_path();
        let mut projection = TursoProjection::open(&path).await.expect("open projection");
        projection
            .synchronize_package_graph(
                view_state_root(&[("package-graph".to_owned(), root_byte.to_string())]),
                facts,
            )
            .await
            .expect("project package graph snapshot");
        (path, projection)
    }

    #[test]
    fn keyset_pages_are_bounded_stable_and_do_not_repeat_edges() {
        futures_executor::block_on(async {
            let source = package("pkg:cargo/demo@1.0.0");
            let authority = registry_authority(0x31);
            let source_key = PackageGraphSourceKey::new(source.clone(), authority);
            let records = [
                edge(
                    &source,
                    authority,
                    "alpha",
                    "*",
                    None,
                    DependencyScope::Runtime,
                    1,
                ),
                edge(
                    &source,
                    authority,
                    "beta",
                    "*",
                    None,
                    DependencyScope::Runtime,
                    3,
                ),
                edge(
                    &source,
                    authority,
                    "gamma",
                    "*",
                    None,
                    DependencyScope::Optional,
                    5,
                ),
            ];
            let facts: [PackageDependencySourceFacts; 1] = [(
                source_key.clone(),
                DependencyFacts::Known(records.to_vec().into_boxed_slice()),
            )];
            let (path, mut projection) = projection_with_facts(&facts, 1).await;
            let request = PackageGraphPageRequest::new(
                source.clone(),
                PackageGraphDirection::Dependencies,
                None,
                1,
            )
            .expect("bounded request");
            let first = projection
                .read_package_graph_page(&request)
                .await
                .expect("first page");
            assert_eq!(first.rows.len(), 1);
            assert_eq!(first.source, Some(source_key.clone()));
            let PackageGraphPageTerminal::More(cursor) = &first.terminal else {
                panic!("first page should continue")
            };
            let second = projection
                .read_package_graph_page(&request.clone().with_cursor(cursor.clone()))
                .await
                .expect("second page");
            assert_eq!(second.rows.len(), 1);
            let PackageGraphPageTerminal::More(cursor) = &second.terminal else {
                panic!("second page should continue")
            };
            let third = projection
                .read_package_graph_page(&request.with_cursor(cursor.clone()))
                .await
                .expect("third page");
            assert_eq!(third.rows.len(), 1);
            assert_eq!(third.terminal, PackageGraphPageTerminal::Complete);
            let ids = [
                first.rows[0].facts_version,
                second.rows[0].facts_version,
                third.rows[0].facts_version,
            ];
            assert!(ids[0] < ids[1] && ids[1] < ids[2]);
            assert!(first.admit().is_ok() && second.admit().is_ok() && third.admit().is_ok());
            drop(projection);
            projection = TursoProjection::open(&path)
                .await
                .expect("reopen durable projection");
            let reopened = projection
                .read_package_graph_page(
                    &PackageGraphPageRequest::new(
                        source,
                        PackageGraphDirection::Dependencies,
                        Some(authority),
                        8,
                    )
                    .expect("reopen request"),
                )
                .await
                .expect("read after cold reopen");
            assert_eq!(reopened.knowledge, PackageGraphKnowledge::Known);
            assert_eq!(reopened.rows.len(), 3);
            drop(projection);
            remove_database(&path);
        });
    }

    #[test]
    fn durable_edge_payload_must_match_its_content_identity() {
        futures_executor::block_on(async {
            let source = package("pkg:cargo/demo@1.0.0");
            let authority = registry_authority(0x31);
            let facts = [(
                PackageGraphSourceKey::new(source.clone(), authority),
                DependencyFacts::Known(
                    vec![edge(
                        &source,
                        authority,
                        "serde",
                        "^1",
                        None,
                        DependencyScope::Runtime,
                        1,
                    )]
                    .into_boxed_slice(),
                ),
            )];
            let (path, projection) = projection_with_facts(&facts, 9).await;
            let request = PackageGraphPageRequest::new(
                source,
                PackageGraphDirection::Dependencies,
                Some(authority),
                8,
            )
            .expect("request");
            assert_eq!(
                projection
                    .read_package_graph_page(&request)
                    .await
                    .expect("original page")
                    .rows
                    .len(),
                1
            );
            projection
                .connection
                .execute(
                    "UPDATE backend_projection_package_edges SET requirement='^9'",
                    (),
                )
                .await
                .expect("alter one durable payload without its identity");
            assert!(projection.read_package_graph_page(&request).await.is_err());
            drop(projection);
            remove_database(&path);
        });
    }

    #[test]
    fn state_value_mutation_is_rejected_against_source_witness() {
        futures_executor::block_on(async {
            let source = package("pkg:cargo/stateful@1.0.0");
            let authority = registry_authority(0x35);
            let facts = [(
                PackageGraphSourceKey::new(source.clone(), authority),
                DependencyFacts::Unknown(
                    ProductText::new("manifest has no dependency metadata").expect("state reason"),
                ),
            )];
            let (path, projection) = projection_with_facts(&facts, 10).await;
            projection
                .connection
                .execute(
                    "UPDATE backend_projection_package_states SET reason='changed reason' \
                     WHERE source=?1 AND source_authority_kind=?2 AND source_authority_id=?3",
                    turso::params![
                        source.as_str(),
                        authority.kind_tag(),
                        authority.id_bytes().as_slice()
                    ],
                )
                .await
                .expect("mutate durable state value");
            let request = PackageGraphPageRequest::new(
                source,
                PackageGraphDirection::Dependencies,
                Some(authority),
                8,
            )
            .expect("request");
            assert!(projection.read_package_graph_page(&request).await.is_err());
            drop(projection);
            remove_database(&path);
        });
    }

    #[test]
    fn deleted_state_row_is_not_misread_as_known_empty() {
        futures_executor::block_on(async {
            let source = package("pkg:cargo/state-deleted@1.0.0");
            let authority = registry_authority(0x36);
            let facts = [(
                PackageGraphSourceKey::new(source.clone(), authority),
                DependencyFacts::Unavailable(
                    ProductText::new("registry unavailable").expect("state reason"),
                ),
            )];
            let (path, projection) = projection_with_facts(&facts, 11).await;
            projection
                .connection
                .execute(
                    "DELETE FROM backend_projection_package_states WHERE source=?1 \
                     AND source_authority_kind=?2 AND source_authority_id=?3",
                    turso::params![
                        source.as_str(),
                        authority.kind_tag(),
                        authority.id_bytes().as_slice()
                    ],
                )
                .await
                .expect("delete durable state row");
            let request = PackageGraphPageRequest::new(
                source,
                PackageGraphDirection::Dependencies,
                Some(authority),
                8,
            )
            .expect("request");
            assert!(projection.read_package_graph_page(&request).await.is_err());
            drop(projection);
            remove_database(&path);
        });
    }

    #[test]
    fn deleted_source_inventory_row_is_not_misread_as_missing() {
        futures_executor::block_on(async {
            let source = package("pkg:cargo/inventory-deleted@1.0.0");
            let authority = registry_authority(0x37);
            let facts: [PackageDependencySourceFacts; 1] = [(
                PackageGraphSourceKey::new(source.clone(), authority),
                DependencyFacts::Known(Box::new([])),
            )];
            let (path, projection) = projection_with_facts(&facts, 12).await;
            projection
                .connection
                .execute(
                    "DELETE FROM backend_projection_package_sources WHERE source=?1 \
                     AND source_authority_kind=?2 AND source_authority_id=?3",
                    turso::params![
                        source.as_str(),
                        authority.kind_tag(),
                        authority.id_bytes().as_slice()
                    ],
                )
                .await
                .expect("delete source inventory row");
            let request =
                PackageGraphPageRequest::new(source, PackageGraphDirection::Dependencies, None, 8)
                    .expect("request");
            assert!(projection.read_package_graph_page(&request).await.is_err());
            drop(projection);
            remove_database(&path);
        });
    }

    #[test]
    fn ambiguity_requires_exact_source_and_exposes_bounded_choices() {
        futures_executor::block_on(async {
            let source = package("pkg:cargo/shared@1.0.0");
            let authority_a = registry_authority(0x31);
            let authority_b = registry_authority(0x52);
            let facts: [PackageDependencySourceFacts; 2] = [
                (
                    PackageGraphSourceKey::new(source.clone(), authority_a),
                    DependencyFacts::Known(Box::new([])),
                ),
                (
                    PackageGraphSourceKey::new(source.clone(), authority_b),
                    DependencyFacts::Known(Box::new([])),
                ),
            ];
            let (path, projection) = projection_with_facts(&facts, 2).await;
            let request = PackageGraphPageRequest::new(
                source.clone(),
                PackageGraphDirection::Dependencies,
                None,
                8,
            )
            .expect("request");
            let ambiguous = projection
                .read_package_graph_page(&request)
                .await
                .expect("ambiguity response");
            assert!(ambiguous.rows.is_empty());
            assert!(matches!(
                &ambiguous.knowledge,
                PackageGraphKnowledge::Ambiguous { sources }
                    if sources.iter().map(|source| source.authority).collect::<Vec<_>>()
                        == [authority_a, authority_b]
            ));

            let exact = PackageGraphPageRequest::new(
                source,
                PackageGraphDirection::Dependencies,
                Some(authority_b),
                8,
            )
            .expect("exact authority request");
            let selected = projection
                .read_package_graph_page(&exact)
                .await
                .expect("exact authority response");
            assert_eq!(
                selected.source.as_ref().map(|key| key.authority),
                Some(authority_b)
            );
            assert_eq!(selected.knowledge, PackageGraphKnowledge::Known);
            drop(projection);
            remove_database(&path);
        });
    }

    #[test]
    fn reverse_pages_match_exact_versions_and_unresolved_runtime_edges() {
        futures_executor::block_on(async {
            let first = package("pkg:cargo/first@1.0.0");
            let second = package("pkg:cargo/second@1.0.0");
            let authority_a = registry_authority(0x41);
            let authority_b = registry_authority(0x42);
            let facts: [PackageDependencySourceFacts; 2] = [
                (
                    PackageGraphSourceKey::new(first.clone(), authority_a),
                    DependencyFacts::Known(
                        vec![
                            edge(
                                &first,
                                authority_a,
                                "demo",
                                "=1.0.0",
                                Some("pkg:cargo/demo@1.0.0"),
                                DependencyScope::Runtime,
                                1,
                            ),
                            edge(
                                &first,
                                authority_a,
                                "demo",
                                "=2.0.0",
                                Some("pkg:cargo/demo@2.0.0"),
                                DependencyScope::Runtime,
                                3,
                            ),
                            edge(
                                &first,
                                authority_a,
                                "demo",
                                "*",
                                None,
                                DependencyScope::Runtime,
                                5,
                            ),
                            edge(
                                &first,
                                authority_a,
                                "demo",
                                "*",
                                None,
                                DependencyScope::Development,
                                7,
                            ),
                        ]
                        .into_boxed_slice(),
                    ),
                ),
                (
                    PackageGraphSourceKey::new(second.clone(), authority_b),
                    DependencyFacts::Known(
                        vec![edge(
                            &second,
                            authority_b,
                            "demo",
                            "*",
                            None,
                            DependencyScope::Optional,
                            9,
                        )]
                        .into_boxed_slice(),
                    ),
                ),
            ];
            let (path, projection) = projection_with_facts(&facts, 3).await;
            let page = projection
                .read_package_graph_page(
                    &PackageGraphPageRequest::new(
                        package("pkg:cargo/demo@1.0.0"),
                        PackageGraphDirection::Dependents,
                        None,
                        16,
                    )
                    .expect("reverse request"),
                )
                .await
                .expect("reverse result");
            assert_eq!(page.rows.len(), 3);
            assert!(page.rows.iter().all(|row| {
                matches!(
                    row.scope,
                    DependencyScope::Runtime | DependencyScope::Optional
                ) && row
                    .target
                    .resolved
                    .as_ref()
                    .is_none_or(|resolved| resolved.as_str() == "pkg:cargo/demo@1.0.0")
            }));
            drop(projection);
            remove_database(&path);
        });
    }

    #[test]
    fn stale_cursor_states_and_cancellation_are_explicit() {
        futures_executor::block_on(async {
            let source = package("pkg:cargo/demo@1.0.0");
            let authority = registry_authority(0x71);
            let source_key = PackageGraphSourceKey::new(source.clone(), authority);
            let records = [
                edge(
                    &source,
                    authority,
                    "alpha",
                    "*",
                    None,
                    DependencyScope::Runtime,
                    1,
                ),
                edge(
                    &source,
                    authority,
                    "beta",
                    "*",
                    None,
                    DependencyScope::Runtime,
                    3,
                ),
            ];
            let facts = [(
                source_key.clone(),
                DependencyFacts::Known(records.to_vec().into_boxed_slice()),
            )];
            let (path, mut projection) = projection_with_facts(&facts, 4).await;
            let request = PackageGraphPageRequest::new(
                source.clone(),
                PackageGraphDirection::Dependencies,
                None,
                1,
            )
            .expect("request");
            let first = projection
                .read_package_graph_page(&request)
                .await
                .expect("first page");
            let PackageGraphPageTerminal::More(cursor) = first.terminal else {
                panic!("first page should continue")
            };
            let empty = projection
                .read_package_graph_page(
                    &PackageGraphPageRequest::new(
                        source.clone(),
                        PackageGraphDirection::Dependencies,
                        Some(authority),
                        4,
                    )
                    .expect("explicit authority request"),
                )
                .await
                .expect("known page");
            assert_eq!(empty.knowledge, PackageGraphKnowledge::Known);
            assert_eq!(
                projection
                    .read_package_graph_page(&request.clone().cancelled())
                    .await
                    .expect("cancel response")
                    .terminal,
                PackageGraphPageTerminal::Cancelled
            );

            let unknown_key =
                PackageGraphSourceKey::new(package("pkg:cargo/unknown@1.0.0"), authority);
            let unavailable_key =
                PackageGraphSourceKey::new(package("pkg:cargo/unavailable@1.0.0"), authority);
            let states = [
                (
                    unknown_key.clone(),
                    DependencyFacts::Unknown(ProductText::new("metadata omitted").expect("reason")),
                ),
                (
                    unavailable_key.clone(),
                    DependencyFacts::Unavailable(
                        ProductText::new("registry unavailable").expect("reason"),
                    ),
                ),
            ];
            projection
                .synchronize_package_graph(
                    view_state_root(&[("package-graph".to_owned(), "5".to_owned())]),
                    &states,
                )
                .await
                .expect("advance graph snapshot");
            let stale = projection
                .read_package_graph_page(&request.with_cursor(cursor))
                .await;
            assert!(matches!(stale, Err(PackageGraphReadError::StaleCursor)));

            for (coordinate, expected) in
                [(unknown_key, "unknown"), (unavailable_key, "unavailable")]
            {
                let page = projection
                    .read_package_graph_page(
                        &PackageGraphPageRequest::new(
                            coordinate.coordinate,
                            PackageGraphDirection::Dependencies,
                            Some(authority),
                            4,
                        )
                        .expect("state request"),
                    )
                    .await
                    .expect("state page");
                assert!(match (expected, page.knowledge) {
                    ("unknown", PackageGraphKnowledge::Unknown { .. }) => true,
                    ("unavailable", PackageGraphKnowledge::Unavailable { .. }) => true,
                    _ => false,
                });
            }
            drop(projection);
            remove_database(&path);
        });
    }
}

//! Package dependency projection and graph query decoding.

use crate::schema::PackageGraphMetadata;
use crate::{ProjectionError, ProjectionUpdate, TursoProjection};
use backend_library::{
    DependencyAuthority, DependencyFacts, DependencyScope, PackageDependencyRecord,
    PackageReference,
};

/// A graph query result fenced to the immutable root that supplied its facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootedPackageGraph {
    /// Exact projected root.
    pub root: Box<[u8]>,
    /// Matching canonical dependency edges.
    pub edges: Box<[PackageDependencyRecord]>,
    /// Unknown or unavailable source state, when no edge rows were possible.
    pub state: Option<PackageGraphState>,
}

/// A typed explanation for a package with no usable dependency edges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageGraphState {
    /// Canonical source spelling.
    pub source: String,
    /// `1` is unknown and `2` is unavailable.
    pub kind: i64,
    /// Bounded reason supplied by the authority.
    pub reason: String,
}

impl TursoProjection {
    /// Replaces the package graph projection for one immutable view root.
    ///
    /// The graph is deliberately fenced independently from the UI row
    /// projection: package metadata can arrive in a different ingest batch,
    /// while every query still returns the exact root that supplied its facts.
    /// An identical root performs no writes, and the single transaction clears
    /// stale edges and typed unknown/unavailable states together.
    pub async fn synchronize_package_graph(
        &mut self,
        root: backend_library::ViewStateRoot,
        facts: &[(
            PackageReference,
            DependencyFacts<Box<[PackageDependencyRecord]>>,
        )],
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let root_bytes = root.as_bytes();
        if let Some(metadata) = self.package_graph_metadata().await?
            && metadata.root.as_slice() == root_bytes
        {
            return Ok(ProjectionUpdate::Reused {
                rows: u64::try_from(metadata.edge_count).unwrap_or(0),
            });
        }
        let edge_count = facts
            .iter()
            .map(|(_, state)| match state {
                DependencyFacts::Known(rows) => rows.len(),
                DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_) => 0,
            })
            .sum::<usize>();
        let edge_count_i64 =
            i64::try_from(edge_count).map_err(|_| ProjectionError::GraphRowCountOverflow)?;
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        if let Some(current) = package_graph_metadata_from(&tx).await?
            && current.root.as_slice() == root_bytes
        {
            tx.rollback().await?;
            return Ok(ProjectionUpdate::Reused {
                rows: u64::try_from(current.edge_count).unwrap_or(0),
            });
        }
        tx.execute("DELETE FROM backend_projection_package_edges", ())
            .await?;
        tx.execute("DELETE FROM backend_projection_package_states", ())
            .await?;
        for (source, state) in facts {
            match state {
                DependencyFacts::Known(rows) => {
                    for record in rows.iter() {
                        upsert_package_edge(&tx, root_bytes, record).await?;
                    }
                }
                DependencyFacts::Unknown(reason) => {
                    put_package_state(&tx, root_bytes, source, 1, reason.as_str()).await?;
                }
                DependencyFacts::Unavailable(reason) => {
                    put_package_state(&tx, root_bytes, source, 2, reason.as_str()).await?;
                }
            }
        }
        tx.execute(
            "INSERT INTO backend_projection_package_graph_meta (singleton, root, edge_count) \
             VALUES (1, ?1, ?2) ON CONFLICT(singleton) DO UPDATE SET \
             root=excluded.root, edge_count=excluded.edge_count",
            turso::params![root_bytes.as_slice(), edge_count_i64],
        )
        .await?;
        tx.commit().await?;
        Ok(ProjectionUpdate::Rebuilt {
            rows: u64::try_from(edge_count).unwrap_or(u64::MAX),
        })
    }

    /// Returns the forward edges for `source`, fenced to one graph root.
    pub async fn package_dependencies(
        &self,
        source: &PackageReference,
    ) -> Result<RootedPackageGraph, ProjectionError> {
        let tx = self.connection.unchecked_transaction().await?;
        let metadata = package_graph_metadata_from(&tx)
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        let mut rows = tx
            .query(
                "SELECT edge_id, source, target_ecosystem, target_name, requirement, resolved, \
                 scope, optional, authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE root=?1 AND source=?2 \
                 ORDER BY edge_id",
                turso::params![metadata.root.as_slice(), source.as_str()],
            )
            .await?;
        let mut edges = Vec::new();
        while let Some(row) = rows.next().await? {
            edges.push(decode_package_edge(&row)?);
        }
        drop(rows);
        let root = metadata.root.clone().into_boxed_slice();
        let state = package_state_from(&tx, &metadata.root, source.as_str()).await?;
        tx.rollback().await?;
        Ok(RootedPackageGraph {
            root,
            edges: edges.into_boxed_slice(),
            state,
        })
    }

    /// Returns reverse edges whose target name and ecosystem match a package.
    ///
    /// Resolution is retained in the predicate: a resolved exact edge matches
    /// only that version, while an unresolved requirement remains visible for
    /// all versions of the target package.
    pub async fn package_dependents(
        &self,
        target: &PackageReference,
    ) -> Result<RootedPackageGraph, ProjectionError> {
        let tx = self.connection.unchecked_transaction().await?;
        let metadata = package_graph_metadata_from(&tx)
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        let PackageReference::Purl(target_url) = target else {
            let root = metadata.root.into_boxed_slice();
            tx.rollback().await?;
            return Ok(RootedPackageGraph {
                root,
                edges: Box::new([]),
                state: None,
            });
        };
        let mut rows = tx
            .query(
                "SELECT edge_id, source, target_ecosystem, target_name, requirement, resolved, \
                 scope, optional, authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE root=?1 AND target_ecosystem=?2 \
                 AND target_name=?3 AND (resolved IS NULL OR resolved=?4) ORDER BY edge_id",
                turso::params![
                    metadata.root.as_slice(),
                    i64::from(
                        target_url
                            .package_type()
                            .registry()
                            .map_or(0, |value| value as u8)
                    ),
                    target_url.lineage_name(),
                    target.as_str()
                ],
            )
            .await?;
        let mut edges = Vec::new();
        while let Some(row) = rows.next().await? {
            edges.push(decode_package_edge(&row)?);
        }
        drop(rows);
        let root = metadata.root.into_boxed_slice();
        tx.rollback().await?;
        Ok(RootedPackageGraph {
            root,
            edges: edges.into_boxed_slice(),
            state: None,
        })
    }
}

impl TursoProjection {
    async fn package_graph_metadata(
        &self,
    ) -> Result<Option<PackageGraphMetadata>, ProjectionError> {
        package_graph_metadata_from(&self.connection).await
    }
}

pub(crate) async fn package_graph_metadata_from(
    connection: &turso::Connection,
) -> Result<Option<PackageGraphMetadata>, ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT root, edge_count FROM backend_projection_package_graph_meta \
             WHERE singleton=1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(PackageGraphMetadata {
        root: row.get(0)?,
        edge_count: row.get(1)?,
    }))
}

async fn package_state_from(
    connection: &turso::Connection,
    root: &[u8],
    source: &str,
) -> Result<Option<PackageGraphState>, ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT state, reason FROM backend_projection_package_states \
             WHERE root=?1 AND source=?2",
            turso::params![root, source],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(PackageGraphState {
        source: source.to_owned(),
        kind: row.get(0)?,
        reason: row.get(1)?,
    }))
}

async fn upsert_package_edge(
    connection: &turso::Connection,
    root: &[u8; 32],
    record: &PackageDependencyRecord,
) -> turso::Result<u64> {
    let edge_id = record.facts_version;
    let resolved = record.target.resolved.as_ref().map(|value| value.as_str());
    connection
        .execute(
            "INSERT INTO backend_projection_package_edges (\
             edge_id, root, source, target_ecosystem, target_name, requirement, resolved, \
             scope, optional, authority, frontier, provenance, facts_version) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
             ON CONFLICT(edge_id) DO UPDATE SET root=excluded.root, source=excluded.source, \
             target_ecosystem=excluded.target_ecosystem, target_name=excluded.target_name, \
             requirement=excluded.requirement, resolved=excluded.resolved, scope=excluded.scope, \
             optional=excluded.optional, authority=excluded.authority, frontier=excluded.frontier, \
             provenance=excluded.provenance, facts_version=excluded.facts_version",
            turso::params![
                edge_id.as_slice(),
                root.as_slice(),
                record.source.as_str(),
                i64::from(record.target.ecosystem as u8),
                record.target.name.as_str(),
                record.target.requirement.as_str(),
                resolved,
                dependency_scope_code(record.scope),
                i64::from(record.optional),
                dependency_authority_code(record.evidence.authority),
                record.evidence.frontier.as_slice(),
                record.evidence.provenance.as_slice(),
                record.facts_version.as_slice(),
            ],
        )
        .await
}

async fn put_package_state(
    connection: &turso::Connection,
    root: &[u8; 32],
    source: &PackageReference,
    state: i64,
    reason: &str,
) -> turso::Result<u64> {
    connection
        .execute(
            "INSERT INTO backend_projection_package_states (root, source, state, reason) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(root, source) DO UPDATE SET \
             state=excluded.state, reason=excluded.reason",
            turso::params![root.as_slice(), source.as_str(), state, reason],
        )
        .await
}

fn dependency_scope_code(scope: DependencyScope) -> i64 {
    match scope {
        DependencyScope::Runtime => 0,
        DependencyScope::Optional => 1,
        DependencyScope::Development => 2,
        DependencyScope::Build => 3,
        DependencyScope::Peer => 4,
    }
}

fn dependency_authority_code(authority: DependencyAuthority) -> i64 {
    match authority {
        DependencyAuthority::RegistryMetadata => 0,
        DependencyAuthority::ArchiveManifest => 1,
        DependencyAuthority::ForgeManifest => 2,
        DependencyAuthority::LocalManifest => 3,
    }
}

fn decode_package_edge(row: &turso::Row) -> Result<PackageDependencyRecord, ProjectionError> {
    let source = PackageReference::parse(row.get::<String>(1)?).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse("invalid graph source".to_owned()))
    })?;
    let target_ecosystem =
        backend_library::RegistryEcosystem::parse_canonical(match row.get::<i64>(2)? {
            1 => "cargo",
            2 => "npm",
            3 => "pypi",
            4 => "maven",
            5 => "nuget",
            6 => "golang",
            7 => "cpp",
            _ => {
                return Err(ProjectionError::Database(turso::Error::Misuse(
                    "invalid graph ecosystem".to_owned(),
                )));
            }
        })
        .map_err(|_| {
            ProjectionError::Database(turso::Error::Misuse("invalid graph ecosystem".to_owned()))
        })?;
    let name = backend_library::ProductText::new(row.get::<String>(3)?).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse("invalid graph target name".to_owned()))
    })?;
    let requirement = backend_library::ProductText::new(row.get::<String>(4)?).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse("invalid graph requirement".to_owned()))
    })?;
    let resolved = row
        .get::<Option<String>>(5)?
        .map(|value| PackageReference::parse(value))
        .transpose()
        .map_err(|_| {
            ProjectionError::Database(turso::Error::Misuse("invalid graph resolution".to_owned()))
        })?;
    let scope = match row.get::<i64>(6)? {
        0 => DependencyScope::Runtime,
        1 => DependencyScope::Optional,
        2 => DependencyScope::Development,
        3 => DependencyScope::Build,
        4 => DependencyScope::Peer,
        _ => {
            return Err(ProjectionError::Database(turso::Error::Misuse(
                "invalid graph scope".to_owned(),
            )));
        }
    };
    let authority = match row.get::<i64>(8)? {
        0 => DependencyAuthority::RegistryMetadata,
        1 => DependencyAuthority::ArchiveManifest,
        2 => DependencyAuthority::ForgeManifest,
        3 => DependencyAuthority::LocalManifest,
        _ => {
            return Err(ProjectionError::Database(turso::Error::Misuse(
                "invalid graph authority".to_owned(),
            )));
        }
    };
    let frontier: Vec<u8> = row.get(9)?;
    let provenance: Vec<u8> = row.get(10)?;
    let facts_version: Vec<u8> = row.get(11)?;
    let frontier: [u8; 32] = frontier.try_into().map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse("invalid graph frontier".to_owned()))
    })?;
    let provenance: [u8; 32] = provenance.try_into().map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse("invalid graph provenance".to_owned()))
    })?;
    let facts_version: [u8; 32] = facts_version.try_into().map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse(
            "invalid graph facts version".to_owned(),
        ))
    })?;
    let record = PackageDependencyRecord::new(
        source,
        backend_library::PackageDependencyTarget {
            ecosystem: target_ecosystem,
            name,
            requirement,
            resolved,
        },
        scope,
        row.get::<i64>(7)? != 0,
        backend_library::DependencyEvidence {
            authority,
            frontier,
            provenance,
        },
    );
    if record.facts_version != facts_version {
        return Err(ProjectionError::Database(turso::Error::Misuse(
            "graph facts version mismatch".to_owned(),
        )));
    }
    Ok(record)
}

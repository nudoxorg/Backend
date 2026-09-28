//! Package dependency projection and graph query decoding.

use crate::schema::PackageGraphMetadata;
use crate::{ProjectionError, ProjectionUpdate, TursoProjection};
use backend_library::{
    CheckedPackageGraphFacts, DependencyAuthority, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencySourceFacts, PackageGraphSourceAuthority,
    PackageGraphSourceKey, PackageReference,
};
use std::collections::{BTreeMap, BTreeSet};

/// A graph query result fenced to the immutable root that supplied its facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootedPackageGraph {
    /// Exact projected root.
    pub root: Box<[u8]>,
    /// Canonical identity of the dependency facts projected at `root`.
    pub facts_witness: [u8; 32],
    /// Matching canonical dependency edges.
    pub edges: Box<[PackageDependencyRecord]>,
    /// Authority resolution for coordinate-only forward lookups. Reverse
    /// lookups use `NotApplicable` because their returned edges each retain
    /// their own source key.
    pub source_selection: PackageGraphSourceSelection,
    /// Unknown or unavailable source state, when no edge rows were possible.
    pub state: Option<PackageGraphState>,
}

/// A typed explanation for a package with no usable dependency edges.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageGraphState {
    /// Canonical source spelling.
    pub source: String,
    /// Exact authority whose source state was recorded.
    pub authority: PackageGraphSourceAuthority,
    /// `1` is unknown and `2` is unavailable.
    pub kind: i64,
    /// Bounded reason supplied by the authority.
    pub reason: String,
}

/// Authority selection outcome for a coordinate-only graph query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageGraphSourceSelection {
    /// Reverse lookup does not choose one source package.
    NotApplicable,
    /// No graph source has the requested coordinate.
    Missing,
    /// The coordinate names exactly one source authority.
    Exact(PackageGraphSourceKey),
    /// Several source authorities publish that coordinate; callers must select
    /// one instead of silently merging facts.
    Ambiguous(Box<[PackageGraphSourceKey]>),
}

impl TursoProjection {
    /// Replaces the package graph projection for one immutable view root.
    ///
    /// The graph is deliberately fenced independently from the UI row
    /// projection: package metadata can arrive in a different ingest batch,
    /// while every query still returns the exact root that supplied its facts.
    /// An identical root and facts witness perform no writes. Otherwise the
    /// projection reconciles edge identities and state rows transactionally,
    /// then updates the selected root and witness in one metadata row. This
    /// also handles changed facts whose selected view root did not change.
    pub async fn synchronize_package_graph(
        &mut self,
        root: backend_library::ViewStateRoot,
        facts: &[PackageDependencySourceFacts],
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let checked = CheckedPackageGraphFacts::new(facts.to_vec()).map_err(|_| {
            ProjectionError::Database(turso::Error::Misuse(
                "invalid or duplicate package graph source facts".to_owned(),
            ))
        })?;
        self.synchronize_checked_package_graph(root, &checked).await
    }

    /// Synchronizes a previously checked immutable graph snapshot without
    /// recomputing its canonical facts witness.
    pub async fn synchronize_checked_package_graph(
        &mut self,
        root: backend_library::ViewStateRoot,
        facts: &CheckedPackageGraphFacts,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        self.synchronize_package_graph_witness(root, facts.facts(), facts.witness())
            .await
    }

    async fn synchronize_package_graph_witness(
        &mut self,
        root: backend_library::ViewStateRoot,
        facts: &[PackageDependencySourceFacts],
        facts_witness: [u8; 32],
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let root_bytes = root.as_bytes();
        // Read the selected metadata only after taking the writer transaction:
        // otherwise a concurrent publisher can change the selected projection
        // between the reuse check and reconciliation.
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        if let Some(current) = package_graph_metadata_from(&tx).await?
            && current.facts_witness == facts_witness
        {
            let rows = u64::try_from(current.edge_count)
                .map_err(|_| ProjectionError::GraphRowCountOverflow)?;
            if current.root.as_slice() == root_bytes {
                tx.rollback().await?;
                return Ok(ProjectionUpdate::Reused { rows });
            }
            // The complete facts witness already names every edge and state.
            // Moving only the selected view root must not scan the edge table.
            tx.execute(
                "UPDATE backend_projection_package_graph_meta SET root=?1 WHERE singleton=1",
                [root_bytes.as_slice()],
            )
            .await?;
            tx.commit().await?;
            return Ok(ProjectionUpdate::Rebuilt { rows });
        }
        let stored_edges = stored_edge_ids(&tx).await?;
        let mut desired_edges = BTreeSet::new();
        let mut due_edges = Vec::new();
        let mut desired_states: BTreeMap<PackageGraphSourceKey, (i64, String)> = BTreeMap::new();
        let desired_sources = facts
            .iter()
            .map(|(source, _)| source.clone())
            .collect::<BTreeSet<_>>();
        for (source, state) in facts {
            match state {
                DependencyFacts::Known(rows) => {
                    for record in rows.iter() {
                        if desired_edges.insert(record.facts_version)
                            && !stored_edges.contains(&record.facts_version)
                        {
                            due_edges.push(record);
                        }
                    }
                }
                DependencyFacts::Unknown(reason) => {
                    desired_states.insert(source.clone(), (1_i64, reason.as_str().to_owned()));
                }
                DependencyFacts::Unavailable(reason) => {
                    desired_states.insert(source.clone(), (2_i64, reason.as_str().to_owned()));
                }
            }
        }
        for batch in due_edges.chunks(EDGE_WRITE_BATCH) {
            insert_package_edges(&tx, batch).await?;
        }
        let stale_edges = stored_edges
            .into_iter()
            .filter(|edge_id| !desired_edges.contains(edge_id))
            .collect::<Vec<_>>();
        delete_edge_ids(&tx, &stale_edges).await?;
        reconcile_package_sources(&tx, &desired_sources).await?;
        reconcile_package_states(&tx, &desired_states).await?;
        let edge_count = desired_edges.len();
        let edge_count_i64 =
            i64::try_from(edge_count).map_err(|_| ProjectionError::GraphRowCountOverflow)?;
        tx.execute(
            "INSERT INTO backend_projection_package_graph_meta \
             (singleton, root, edge_count, facts_witness) VALUES (1, ?1, ?2, ?3) \
             ON CONFLICT(singleton) DO UPDATE SET root=excluded.root, \
             edge_count=excluded.edge_count, facts_witness=excluded.facts_witness",
            turso::params![
                root_bytes.as_slice(),
                edge_count_i64,
                facts_witness.as_slice()
            ],
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
        let mut source_rows = tx
            .query(
                "SELECT source_authority_kind, source_authority_id \
                 FROM backend_projection_package_sources WHERE source=?1 \
                 ORDER BY source_authority_kind, source_authority_id",
                [source.as_str()],
            )
            .await?;
        let mut keys = Vec::new();
        while let Some(row) = source_rows.next().await? {
            keys.push(PackageGraphSourceKey::new(
                source.clone(),
                decode_source_authority(row.get(0)?, row.get(1)?)?,
            ));
        }
        drop(source_rows);
        if keys.is_empty() {
            let root = metadata.root.into_boxed_slice();
            tx.rollback().await?;
            return Ok(RootedPackageGraph {
                root,
                facts_witness: metadata.facts_witness,
                edges: Box::new([]),
                source_selection: PackageGraphSourceSelection::Missing,
                state: None,
            });
        }
        if keys.len() > 1 {
            let root = metadata.root.into_boxed_slice();
            tx.rollback().await?;
            return Ok(RootedPackageGraph {
                root,
                facts_witness: metadata.facts_witness,
                edges: Box::new([]),
                source_selection: PackageGraphSourceSelection::Ambiguous(keys.into_boxed_slice()),
                state: None,
            });
        }
        let selected = keys.remove(0);
        let mut rows = tx
            .query(
                "SELECT edge_id, source, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE source=?1 \
                 AND source_authority_kind=?2 AND source_authority_id=?3 \
                 ORDER BY edge_id",
                turso::params![
                    selected.coordinate.as_str(),
                    selected.authority.kind_tag(),
                    selected.authority.id_bytes().as_slice()
                ],
            )
            .await?;
        let mut edges = Vec::new();
        while let Some(row) = rows.next().await? {
            edges.push(decode_package_edge(&row)?);
        }
        drop(rows);
        let root = metadata.root.clone().into_boxed_slice();
        let state = package_state_from(&tx, &selected).await?;
        tx.rollback().await?;
        Ok(RootedPackageGraph {
            root,
            facts_witness: metadata.facts_witness,
            edges: edges.into_boxed_slice(),
            source_selection: PackageGraphSourceSelection::Exact(selected),
            state,
        })
    }

    /// Returns edges for one exact coordinate/authority pair.
    pub async fn package_dependencies_for_source(
        &self,
        source: &PackageGraphSourceKey,
    ) -> Result<RootedPackageGraph, ProjectionError> {
        let tx = self.connection.unchecked_transaction().await?;
        let metadata = package_graph_metadata_from(&tx)
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        let mut source_rows = tx
            .query(
                "SELECT 1 FROM backend_projection_package_sources WHERE source=?1 \
                 AND source_authority_kind=?2 AND source_authority_id=?3",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice()
                ],
            )
            .await?;
        let exists = source_rows.next().await?.is_some();
        drop(source_rows);
        if !exists {
            let root = metadata.root.into_boxed_slice();
            tx.rollback().await?;
            return Ok(RootedPackageGraph {
                root,
                facts_witness: metadata.facts_witness,
                edges: Box::new([]),
                source_selection: PackageGraphSourceSelection::Missing,
                state: None,
            });
        }
        let mut rows = tx
            .query(
                "SELECT edge_id, source, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE source=?1 \
                 AND source_authority_kind=?2 AND source_authority_id=?3 ORDER BY edge_id",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice()
                ],
            )
            .await?;
        let mut edges = Vec::new();
        while let Some(row) = rows.next().await? {
            edges.push(decode_package_edge(&row)?);
        }
        drop(rows);
        let state = package_state_from(&tx, source).await?;
        let root = metadata.root.into_boxed_slice();
        tx.rollback().await?;
        Ok(RootedPackageGraph {
            root,
            facts_witness: metadata.facts_witness,
            edges: edges.into_boxed_slice(),
            source_selection: PackageGraphSourceSelection::Exact(source.clone()),
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
                facts_witness: metadata.facts_witness,
                edges: Box::new([]),
                source_selection: PackageGraphSourceSelection::NotApplicable,
                state: None,
            });
        };
        let mut rows = tx
            .query(
                "SELECT edge_id, source, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE target_ecosystem=?1 \
                 AND target_name=?2 AND (resolved IS NULL OR resolved=?3) ORDER BY edge_id",
                turso::params![
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
            facts_witness: metadata.facts_witness,
            edges: edges.into_boxed_slice(),
            source_selection: PackageGraphSourceSelection::NotApplicable,
            state: None,
        })
    }
}

pub(crate) async fn package_graph_metadata_from(
    connection: &turso::Connection,
) -> Result<Option<PackageGraphMetadata>, ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT root, edge_count, facts_witness FROM backend_projection_package_graph_meta \
             WHERE singleton=1",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let root: Vec<u8> = row.get(0)?;
    if root.len() != 32 {
        return Err(ProjectionError::CorruptMetadata {
            field: "graph_root",
        });
    }
    let facts_witness: Vec<u8> = row.get(2)?;
    let facts_witness: [u8; 32] =
        facts_witness
            .try_into()
            .map_err(|_| ProjectionError::CorruptMetadata {
                field: "graph_facts_witness",
            })?;
    Ok(Some(PackageGraphMetadata {
        root,
        edge_count: row.get(1)?,
        facts_witness,
    }))
}

async fn package_state_from(
    connection: &turso::Connection,
    source: &PackageGraphSourceKey,
) -> Result<Option<PackageGraphState>, ProjectionError> {
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
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    Ok(Some(PackageGraphState {
        source: source.coordinate.as_str().to_owned(),
        authority: source.authority,
        kind: row.get(0)?,
        reason: row.get(1)?,
    }))
}

const EDGE_WRITE_BATCH: usize = 128;

async fn stored_edge_ids(
    connection: &turso::Connection,
) -> Result<BTreeSet<[u8; 32]>, ProjectionError> {
    let mut rows = connection
        .query("SELECT edge_id FROM backend_projection_package_edges", ())
        .await?;
    let mut ids = BTreeSet::new();
    while let Some(row) = rows.next().await? {
        let id: Vec<u8> = row.get(0)?;
        let id: [u8; 32] = id
            .try_into()
            .map_err(|_| ProjectionError::CorruptMetadata { field: "edge_id" })?;
        ids.insert(id);
    }
    Ok(ids)
}

async fn insert_package_edges(
    connection: &turso::Connection,
    records: &[&PackageDependencyRecord],
) -> Result<(), ProjectionError> {
    if records.is_empty() {
        return Ok(());
    }
    let mut sql = String::from(
        "INSERT INTO backend_projection_package_edges (\
         edge_id, source, source_authority_kind, source_authority_id, target_ecosystem, \
         target_name, requirement, resolved, scope, optional, authority, frontier, provenance, \
         facts_version) VALUES ",
    );
    for index in 0..records.len() {
        if index != 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?,?,?,?,?,?,?,?,?,?,?)");
    }
    let mut values = Vec::with_capacity(records.len().saturating_mul(14));
    for record in records {
        values.extend(edge_values(record));
    }
    connection
        .execute(&sql, turso::params_from_iter(values))
        .await?;
    Ok(())
}

fn edge_values(record: &PackageDependencyRecord) -> [turso::Value; 14] {
    let resolved = record
        .target
        .resolved
        .as_ref()
        .map_or(turso::Value::Null, |value| {
            turso::Value::Text(value.as_str().to_owned())
        });
    [
        turso::Value::Blob(record.facts_version.to_vec()),
        turso::Value::Text(record.source.as_str().to_owned()),
        turso::Value::Integer(record.source_authority.kind_tag()),
        turso::Value::Blob(record.source_authority.id_bytes().to_vec()),
        turso::Value::Integer(i64::from(record.target.ecosystem as u8)),
        turso::Value::Text(record.target.name.as_str().to_owned()),
        turso::Value::Text(record.target.requirement.as_str().to_owned()),
        resolved,
        turso::Value::Integer(dependency_scope_code(record.scope)),
        turso::Value::Integer(i64::from(record.optional)),
        turso::Value::Integer(dependency_authority_code(record.evidence.authority)),
        turso::Value::Blob(record.evidence.frontier.to_vec()),
        turso::Value::Blob(record.evidence.provenance.to_vec()),
        turso::Value::Blob(record.facts_version.to_vec()),
    ]
}

async fn delete_edge_ids(
    connection: &turso::Connection,
    stale: &[[u8; 32]],
) -> Result<(), ProjectionError> {
    for batch in stale.chunks(EDGE_WRITE_BATCH) {
        let mut sql =
            String::from("DELETE FROM backend_projection_package_edges WHERE edge_id IN (");
        for index in 0..batch.len() {
            if index != 0 {
                sql.push(',');
            }
            sql.push('?');
        }
        sql.push(')');
        let values = batch
            .iter()
            .map(|id| turso::Value::Blob(id.to_vec()))
            .collect::<Vec<_>>();
        connection
            .execute(&sql, turso::params_from_iter(values))
            .await?;
    }
    Ok(())
}

async fn reconcile_package_states(
    connection: &turso::Connection,
    desired: &BTreeMap<PackageGraphSourceKey, (i64, String)>,
) -> Result<(), ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT source, source_authority_kind, source_authority_id, state, reason \
             FROM backend_projection_package_states \
             ORDER BY source, source_authority_kind, source_authority_id",
            (),
        )
        .await?;
    let mut current = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let source: String = row.get(0)?;
        let coordinate = PackageReference::parse(source).map_err(|_| {
            ProjectionError::Database(turso::Error::Misuse(
                "invalid package graph state source".to_owned(),
            ))
        })?;
        let authority = decode_source_authority(row.get(1)?, row.get(2)?)?;
        let state: i64 = row.get(3)?;
        let reason: String = row.get(4)?;
        current.insert(
            PackageGraphSourceKey::new(coordinate, authority),
            (state, reason),
        );
    }
    drop(rows);

    for source in current
        .keys()
        .filter(|source| !desired.contains_key(*source))
    {
        connection
            .execute(
                "DELETE FROM backend_projection_package_states WHERE source=?1 \
                 AND source_authority_kind=?2 AND source_authority_id=?3",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice()
                ],
            )
            .await?;
    }
    for (source, (state, reason)) in desired {
        if current
            .get(source)
            .is_some_and(|(current_state, current_reason)| {
                *current_state == *state && current_reason == reason
            })
        {
            continue;
        }
        connection
            .execute(
                "INSERT INTO backend_projection_package_states (source, source_authority_kind, \
                 source_authority_id, state, reason) VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(source, source_authority_kind, source_authority_id) DO UPDATE SET \
                 state=excluded.state, reason=excluded.reason",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice(),
                    *state,
                    reason.as_str()
                ],
            )
            .await?;
    }
    Ok(())
}

async fn reconcile_package_sources(
    connection: &turso::Connection,
    desired: &BTreeSet<PackageGraphSourceKey>,
) -> Result<(), ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT source, source_authority_kind, source_authority_id \
             FROM backend_projection_package_sources \
             ORDER BY source, source_authority_kind, source_authority_id",
            (),
        )
        .await?;
    let mut current = BTreeSet::new();
    while let Some(row) = rows.next().await? {
        let source: String = row.get(0)?;
        let coordinate = PackageReference::parse(source).map_err(|_| {
            ProjectionError::Database(turso::Error::Misuse(
                "invalid package graph source".to_owned(),
            ))
        })?;
        current.insert(PackageGraphSourceKey::new(
            coordinate,
            decode_source_authority(row.get(1)?, row.get(2)?)?,
        ));
    }
    drop(rows);
    for source in current.difference(desired) {
        connection
            .execute(
                "DELETE FROM backend_projection_package_sources WHERE source=?1 \
                 AND source_authority_kind=?2 AND source_authority_id=?3",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice()
                ],
            )
            .await?;
    }
    for source in desired.difference(&current) {
        connection
            .execute(
                "INSERT INTO backend_projection_package_sources \
                 (source, source_authority_kind, source_authority_id) VALUES (?1, ?2, ?3)",
                turso::params![
                    source.coordinate.as_str(),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice()
                ],
            )
            .await?;
    }
    Ok(())
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

fn decode_source_authority(
    kind: i64,
    id: Vec<u8>,
) -> Result<PackageGraphSourceAuthority, ProjectionError> {
    let bytes: [u8; 32] = id.try_into().map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse(
            "invalid package graph source authority ID".to_owned(),
        ))
    })?;
    match kind {
        0 if bytes == [0; 32] => Ok(PackageGraphSourceAuthority::Unattributed),
        1 => Ok(PackageGraphSourceAuthority::Registry(
            backend_library::RegistryAuthorityId::from_configured_source(bytes),
        )),
        2 => Ok(PackageGraphSourceAuthority::Forge(bytes)),
        3 => Ok(PackageGraphSourceAuthority::Archive(bytes)),
        4 => Ok(PackageGraphSourceAuthority::Local(bytes)),
        _ => Err(ProjectionError::Database(turso::Error::Misuse(
            "invalid package graph source authority kind".to_owned(),
        ))),
    }
}

fn decode_package_edge(row: &turso::Row) -> Result<PackageDependencyRecord, ProjectionError> {
    let source = PackageReference::parse(row.get::<String>(1)?).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse("invalid graph source".to_owned()))
    })?;
    let source_authority = decode_source_authority(row.get(2)?, row.get(3)?)?;
    let target_ecosystem =
        backend_library::RegistryEcosystem::parse_canonical(match row.get::<i64>(4)? {
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
    let name = backend_library::ProductText::new(row.get::<String>(5)?).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse("invalid graph target name".to_owned()))
    })?;
    let requirement = backend_library::ProductText::new(row.get::<String>(6)?).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse("invalid graph requirement".to_owned()))
    })?;
    let resolved = row
        .get::<Option<String>>(7)?
        .map(|value| PackageReference::parse(value))
        .transpose()
        .map_err(|_| {
            ProjectionError::Database(turso::Error::Misuse("invalid graph resolution".to_owned()))
        })?;
    let scope = match row.get::<i64>(8)? {
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
    let authority = match row.get::<i64>(10)? {
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
    let frontier: Vec<u8> = row.get(11)?;
    let provenance: Vec<u8> = row.get(12)?;
    let facts_version: Vec<u8> = row.get(13)?;
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
    let record = PackageDependencyRecord::new_with_source_authority(
        source,
        source_authority,
        backend_library::PackageDependencyTarget {
            ecosystem: target_ecosystem,
            name,
            requirement,
            resolved,
        },
        scope,
        row.get::<i64>(9)? != 0,
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

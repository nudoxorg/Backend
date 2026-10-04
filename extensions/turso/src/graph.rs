//! Package dependency projection and graph query decoding.

use crate::projection_namespace::ProjectionGenerationId;
use crate::read::metadata_from;
use crate::schema::PackageGraphMetadata;
use crate::{ProjectionError, ProjectionUpdate, TursoProjection};
use backend_library::{
    CheckedPackageGraphFacts, DependencyAuthority, DependencyFacts, DependencyScope,
    MAX_PACKAGE_GRAPH_AUTHORITIES, MAX_PACKAGE_GRAPH_ROWS, PackageDependencyRecord,
    PackageDependencySourceFacts, PackageGraphSourceAuthority, PackageGraphSourceKey,
    PackageReference, PackageReferenceKind, ProductText,
};
use backend_platform::FileIdentity;
use std::cmp::Ordering;
use std::collections::BTreeSet;

const MAX_UNPAGED_GRAPH_EDGE_READ_LIMIT: i64 = MAX_PACKAGE_GRAPH_ROWS as i64 + 1;
const MAX_GRAPH_SOURCE_KEYS_FOR_SPELLING: usize = MAX_PACKAGE_GRAPH_AUTHORITIES.saturating_mul(2);
const MAX_GRAPH_SOURCE_KEY_READ_LIMIT: i64 =
    MAX_GRAPH_SOURCE_KEYS_FOR_SPELLING.saturating_add(1) as i64;

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
    /// Exact typed source coordinate whose state was recorded.
    pub source: PackageReference,
    /// Exact authority whose source state was recorded.
    pub authority: PackageGraphSourceAuthority,
    /// `1` is unknown and `2` is unavailable.
    pub kind: i64,
    /// Bounded reason supplied by the authority.
    pub reason: String,
}

/// A source's state after its typed key and state witness have been checked.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PackageGraphSourceState {
    /// Known source with at least one edge, or the canonical Known-empty witness.
    Known,
    /// The authority did not publish dependency metadata.
    Unknown(ProductText),
    /// The authority could not provide dependency metadata.
    Unavailable(ProductText),
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

/// Opaque graph compare-and-swap token captured before a caller reads source
/// facts. A graph replacement must present this exact generation, selected
/// view root, and previous graph witness so delayed facts cannot replace a
/// graph commit made after capture. The owner must obtain the graph root, facts
/// and their authority provenance from one exact current source observation.
/// This token proves only the cache-side base; it does not prove that supplied
/// facts are the source authority's newest snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackageGraphRevision {
    generation: ProjectionGenerationId,
    marker_identity: FileIdentity,
    view_root: [u8; 32],
    view_version: [u8; 32],
    graph_root: Option<[u8; 32]>,
    facts_witness: Option<[u8; 32]>,
}

impl PackageGraphRevision {
    /// Exact selected generation whose graph state was captured.
    #[must_use]
    pub const fn generation(self) -> ProjectionGenerationId {
        self.generation
    }

    /// Exact selected view root when this graph revision was captured.
    #[must_use]
    pub const fn view_root(self) -> [u8; 32] {
        self.view_root
    }

    /// Exact selected view version when this graph revision was captured.
    #[must_use]
    pub const fn view_version(self) -> [u8; 32] {
        self.view_version
    }

    /// Previous graph facts witness, or `None` when no graph snapshot was
    /// published at capture time.
    #[must_use]
    pub const fn facts_witness(self) -> Option<[u8; 32]> {
        self.facts_witness
    }
}

impl TursoProjection {
    /// Captures the selected row root and current graph witness before the
    /// caller reads or admits replacement facts.
    ///
    /// The owning source must then read its authoritative graph root and facts
    /// as one current observation and pass that exact result to a `_from`
    /// method. The returned token cannot certify freshness if the caller
    /// already holds stale facts, and it carries no registry/source proof.
    pub async fn package_graph_revision(&self) -> Result<PackageGraphRevision, ProjectionError> {
        if self.staging {
            return Err(ProjectionError::StaleTransition);
        }
        let _operation_guard = self.operation_guard()?;
        let tx = self.connection.unchecked_transaction().await?;
        let Some(view) = metadata_from(&tx).await? else {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        };
        let graph = package_graph_metadata_from(&tx).await?;
        tx.rollback().await?;
        let view_root = view
            .root
            .as_slice()
            .try_into()
            .map_err(|_| ProjectionError::CorruptMetadata { field: "root" })?;
        let view_version = view.view_version.as_slice().try_into().map_err(|_| {
            ProjectionError::CorruptMetadata {
                field: "view_version",
            }
        })?;
        let (graph_root, facts_witness) = match graph {
            Some(graph) => (
                Some(graph.root.as_slice().try_into().map_err(|_| {
                    ProjectionError::CorruptMetadata {
                        field: "graph_root",
                    }
                })?),
                Some(graph.facts_witness),
            ),
            None => (None, None),
        };
        Ok(PackageGraphRevision {
            generation: self.generation,
            marker_identity: self.marker_identity,
            view_root,
            view_version,
            graph_root,
            facts_witness,
        })
    }

    /// Reuses an already selected, exact graph snapshot.
    ///
    /// This compatibility method does not replace facts in a selected
    /// generation. Use [`Self::synchronize_package_graph_from`] with a graph
    /// revision captured before source facts are read. Private staging
    /// generations may be initialized here because they are not yet visible.
    /// `root` must equal the selected row view root.
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
        self.synchronize_package_graph_witness(None, root, &checked)
            .await
    }

    /// Reconciles graph facts only if the selected root and previous graph
    /// witness still equal `expected`, captured before source fact admission.
    /// `root` must equal the selected row view root, and `facts` must come from
    /// the same authoritative source observation that the caller reads after
    /// capturing `expected`.
    pub async fn synchronize_package_graph_from(
        &mut self,
        expected: PackageGraphRevision,
        root: backend_library::ViewStateRoot,
        facts: &[PackageDependencySourceFacts],
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let checked = CheckedPackageGraphFacts::new(facts.to_vec()).map_err(|_| {
            ProjectionError::Database(turso::Error::Misuse(
                "invalid or duplicate package graph source facts".to_owned(),
            ))
        })?;
        self.synchronize_package_graph_witness(Some(expected), root, &checked)
            .await
    }

    /// Reuses a previously checked graph snapshot without recomputing its
    /// canonical facts witness. Replacement requires
    /// [`Self::synchronize_checked_package_graph_from`]. `root` must equal the
    /// selected row view root.
    pub async fn synchronize_checked_package_graph(
        &mut self,
        root: backend_library::ViewStateRoot,
        facts: &CheckedPackageGraphFacts,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        self.synchronize_package_graph_witness(None, root, facts)
            .await
    }

    /// Reconciles a checked graph snapshot against an exact prior view root and
    /// graph witness captured before source fact admission. The checked facts
    /// validate canonical shape and content identity, not currentness; the
    /// caller must obtain `root` and `facts` from one authoritative current
    /// source observation after capturing `expected`. `root` must equal the
    /// selected row view root.
    pub async fn synchronize_checked_package_graph_from(
        &mut self,
        expected: PackageGraphRevision,
        root: backend_library::ViewStateRoot,
        facts: &CheckedPackageGraphFacts,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        self.synchronize_package_graph_witness(Some(expected), root, facts)
            .await
    }

    async fn synchronize_package_graph_witness(
        &mut self,
        expected: Option<PackageGraphRevision>,
        root: backend_library::ViewStateRoot,
        checked: &CheckedPackageGraphFacts,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        if let Some(expected) = expected
            && (self.staging
                || expected.generation != self.generation
                || expected.marker_identity != self.marker_identity)
        {
            return Err(ProjectionError::StaleTransition);
        }
        let _operation_guard = self.operation_guard()?;
        let facts = checked.facts();
        let facts_witness = checked.witness();
        let root_bytes = root.as_bytes();
        // Read the selected metadata only after taking the writer transaction:
        // otherwise a concurrent publisher can change the selected projection
        // between the reuse check and reconciliation.
        #[cfg(test)]
        crate::writer::run_before_projection_writer_tx_test_hook();
        let tx = self
            .connection
            .transaction_with_behavior(turso::transaction::TransactionBehavior::Immediate)
            .await?;
        let Some(view) = metadata_from(&tx).await? else {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        };
        if view.root.as_slice() != root_bytes {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        }
        let current = package_graph_metadata_from(&tx).await?;
        let target_is_current = current.as_ref().is_some_and(|current| {
            current.facts_witness == facts_witness && current.root.as_slice() == root_bytes
        });
        if let Some(expected) = expected {
            let graph_matches = match (
                current.as_ref(),
                expected.graph_root,
                expected.facts_witness,
            ) {
                (None, None, None) => true,
                (Some(current), Some(root), Some(witness)) => {
                    current.root.as_slice() == root && current.facts_witness == witness
                }
                _ => false,
            };
            if view.root.as_slice() != expected.view_root
                || view.view_version.as_slice() != expected.view_version
                || !graph_matches
            {
                tx.rollback().await?;
                return Err(ProjectionError::StaleTransition);
            }
        } else if !self.staging && !target_is_current {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        }
        if target_is_current {
            let current = current.as_ref().ok_or(ProjectionError::StaleTransition)?;
            let rows = u64::try_from(current.edge_count)
                .map_err(|_| ProjectionError::GraphRowCountOverflow)?;
            tx.rollback().await?;
            return Ok(ProjectionUpdate::Reused { rows });
        }
        if let Some(current) = current.as_ref()
            && current.facts_witness == facts_witness
        {
            let rows = u64::try_from(current.edge_count)
                .map_err(|_| ProjectionError::GraphRowCountOverflow)?;
            // The complete facts witness already names every edge and state.
            // Moving only the graph root is permitted after the expected
            // graph revision and selected row root both pass their fences.
            tx.execute(
                "UPDATE backend_projection_package_graph_meta SET root=?1 WHERE singleton=1",
                [root_bytes.as_slice()],
            )
            .await?;
            tx.commit().await?;
            return Ok(ProjectionUpdate::Rebuilt { rows });
        }
        let source_witnesses = checked.source_witnesses();

        // Merge the canonical incoming source keys with the persisted source
        // witness index. This visits source metadata once, while edge rows are
        // fetched only for changed or removed keys below.
        let force_reconcile = current.is_none();
        let mut changed = Vec::new();
        let mut removed = Vec::new();
        let mut desired_index = 0_usize;
        let mut stored = tx
            .query(
                "SELECT source, coordinate_kind, source_authority_kind, source_authority_id, facts_witness \
                 FROM backend_projection_package_source_witnesses \
                 ORDER BY source, coordinate_kind, source_authority_kind, source_authority_id",
                (),
            )
            .await?;
        while let Some(row) = stored.next().await? {
            let source_text: String = row.get(0)?;
            let coordinate_kind: i64 = row.get(1)?;
            let authority_kind: i64 = row.get(2)?;
            let authority_id = graph_digest(row.get(3)?, "graph_source_authority_id")?;
            let witness = graph_digest(row.get(4)?, "graph_source_facts_witness")?;
            loop {
                let Some((incoming_source, _)) = facts.get(desired_index) else {
                    removed.push(decode_source_key(
                        source_text,
                        coordinate_kind,
                        authority_kind,
                        authority_id,
                    )?);
                    break;
                };
                match compare_stored_source(
                    &source_text,
                    coordinate_kind,
                    authority_kind,
                    &authority_id,
                    incoming_source,
                ) {
                    Ordering::Less => {
                        removed.push(decode_source_key(
                            source_text,
                            coordinate_kind,
                            authority_kind,
                            authority_id,
                        )?);
                        break;
                    }
                    Ordering::Greater => {
                        changed.push(desired_index);
                        desired_index += 1;
                    }
                    Ordering::Equal => {
                        if force_reconcile || witness != source_witnesses[desired_index] {
                            changed.push(desired_index);
                        }
                        desired_index += 1;
                        break;
                    }
                }
            }
        }
        drop(stored);
        if force_reconcile {
            // A graph metadata row and its source witness rows are committed
            // together. Reaching this branch means there was no published
            // generation, so every supplied source must seed that generation.
            changed = (0..facts.len()).collect();
        } else {
            changed.extend(desired_index..facts.len());
        }

        let mut affected_sources = changed
            .iter()
            .map(|index| facts[*index].0.clone())
            .chain(removed.iter().cloned())
            .collect::<Vec<_>>();
        affected_sources.sort_unstable_by(compare_source_keys);
        affected_sources.dedup();
        let stored_edges = stored_edge_ids_for_sources(&tx, &affected_sources).await?;
        let mut desired_edges = BTreeSet::new();
        let mut due_edges = Vec::new();
        for index in &changed {
            if let DependencyFacts::Known(rows) = &facts[*index].1 {
                for record in rows.iter() {
                    if desired_edges.insert(record.facts_version)
                        && !stored_edges.contains(&record.facts_version)
                    {
                        due_edges.push(record);
                    }
                }
            }
        }
        let stale_edges = stored_edges
            .iter()
            .filter(|edge_id| !desired_edges.contains(*edge_id))
            .copied()
            .collect::<Vec<_>>();
        delete_edge_ids(&tx, &stale_edges).await?;
        for batch in due_edges.chunks(EDGE_WRITE_BATCH) {
            insert_package_edges(&tx, batch).await?;
        }

        let changed_sources = changed
            .iter()
            .map(|index| &facts[*index].0)
            .collect::<Vec<_>>();
        let mut cleared_state_sources = removed.clone();
        cleared_state_sources.extend(changed.iter().filter_map(|index| {
            matches!(&facts[*index].1, DependencyFacts::Known(_)).then(|| facts[*index].0.clone())
        }));
        cleared_state_sources.sort_unstable_by(compare_source_keys);
        cleared_state_sources.dedup();
        delete_package_source_rows(
            &tx,
            "backend_projection_package_states",
            &cleared_state_sources,
        )
        .await?;
        delete_package_source_rows(&tx, "backend_projection_package_sources", &removed).await?;
        delete_package_source_rows(&tx, "backend_projection_package_source_witnesses", &removed)
            .await?;
        insert_package_sources(&tx, &changed_sources).await?;
        upsert_package_source_witnesses(&tx, facts, source_witnesses, &changed).await?;
        upsert_package_states(&tx, facts, &changed).await?;

        let previous_edge_count = match current.as_ref() {
            Some(metadata) => u64::try_from(metadata.edge_count).map_err(|_| {
                ProjectionError::CorruptMetadata {
                    field: "graph_edge_count",
                }
            })?,
            None => u64::try_from(stored_edges.len())
                .map_err(|_| ProjectionError::GraphRowCountOverflow)?,
        };
        let removed_count =
            u64::try_from(stale_edges.len()).map_err(|_| ProjectionError::GraphRowCountOverflow)?;
        let inserted_count =
            u64::try_from(due_edges.len()).map_err(|_| ProjectionError::GraphRowCountOverflow)?;
        let edge_count = previous_edge_count
            .checked_sub(removed_count)
            .and_then(|count| count.checked_add(inserted_count))
            .ok_or(ProjectionError::GraphRowCountOverflow)?;
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
        Ok(ProjectionUpdate::Rebuilt { rows: edge_count })
    }

    /// Returns the forward edges for `source`, fenced to one graph root.
    ///
    /// This compatibility read is capped at the checked per-source row limit.
    /// Use [`Self::read_package_graph_page`] when the result spans multiple
    /// pages.
    pub async fn package_dependencies(
        &self,
        source: &PackageReference,
    ) -> Result<RootedPackageGraph, ProjectionError> {
        let _operation_guard = self.operation_guard()?;
        let tx = self.connection.unchecked_transaction().await?;
        let metadata = package_graph_metadata_from(&tx)
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        if !graph_root_matches_selected_view(&tx, &metadata).await? {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        }
        let mut keys = package_source_inventory(&tx, source).await?;
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
                "SELECT edge_id, source, coordinate_kind, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE source=?1 \
                 AND coordinate_kind=?2 AND source_authority_kind=?3 AND source_authority_id=?4 \
                 ORDER BY edge_id LIMIT ?5",
                turso::params![
                    selected.coordinate.as_str(),
                    i64::from(selected.coordinate.kind().tag()),
                    selected.authority.kind_tag(),
                    selected.authority.id_bytes().as_slice(),
                    MAX_UNPAGED_GRAPH_EDGE_READ_LIMIT
                ],
            )
            .await?;
        let edges = read_unpaged_edges(&mut rows).await?;
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

    /// Returns edges for one exact coordinate/authority pair, capped at the
    /// checked per-source row limit. Use [`Self::read_package_graph_page`] for
    /// a larger paged result.
    pub async fn package_dependencies_for_source(
        &self,
        source: &PackageGraphSourceKey,
    ) -> Result<RootedPackageGraph, ProjectionError> {
        let _operation_guard = self.operation_guard()?;
        let tx = self.connection.unchecked_transaction().await?;
        let metadata = package_graph_metadata_from(&tx)
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        if !graph_root_matches_selected_view(&tx, &metadata).await? {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        }
        let exists = package_source_inventory(&tx, &source.coordinate)
            .await?
            .iter()
            .any(|candidate| candidate == source);
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
                "SELECT edge_id, source, coordinate_kind, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE source=?1 \
                 AND coordinate_kind=?2 AND source_authority_kind=?3 AND source_authority_id=?4 \
                 ORDER BY edge_id LIMIT ?5",
                turso::params![
                    source.coordinate.as_str(),
                    i64::from(source.coordinate.kind().tag()),
                    source.authority.kind_tag(),
                    source.authority.id_bytes().as_slice(),
                    MAX_UNPAGED_GRAPH_EDGE_READ_LIMIT
                ],
            )
            .await?;
        let edges = read_unpaged_edges(&mut rows).await?;
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
    /// all versions of the target package. Results are capped at the checked
    /// per-source row limit; use [`Self::read_package_graph_page`] when a
    /// reverse lookup spans multiple pages.
    pub async fn package_dependents(
        &self,
        target: &PackageReference,
    ) -> Result<RootedPackageGraph, ProjectionError> {
        let _operation_guard = self.operation_guard()?;
        let tx = self.connection.unchecked_transaction().await?;
        let metadata = package_graph_metadata_from(&tx)
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        if !graph_root_matches_selected_view(&tx, &metadata).await? {
            tx.rollback().await?;
            return Err(ProjectionError::StaleTransition);
        }
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
                "SELECT edge_id, source, coordinate_kind, source_authority_kind, source_authority_id, \
                 target_ecosystem, target_name, requirement, resolved, scope, optional, \
                 authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE target_ecosystem=?1 \
                 AND target_name=?2 AND (resolved IS NULL OR resolved=?3) \
                 ORDER BY edge_id LIMIT ?4",
                turso::params![
                    i64::from(
                        target_url
                            .package_type()
                            .registry()
                            .map_or(0, |value| value as u8)
                    ),
                    target_url.lineage_name(),
                    target.as_str(),
                    MAX_UNPAGED_GRAPH_EDGE_READ_LIMIT
                ],
            )
            .await?;
        let edges = read_unpaged_edges(&mut rows).await?;
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

async fn read_unpaged_edges(
    rows: &mut turso::Rows,
) -> Result<Vec<PackageDependencyRecord>, ProjectionError> {
    let mut edges = Vec::with_capacity(MAX_PACKAGE_GRAPH_ROWS.min(128));
    while let Some(row) = rows.next().await? {
        if edges.len() == MAX_PACKAGE_GRAPH_ROWS {
            return Err(ProjectionError::ReadLimitExceeded {
                maximum: MAX_PACKAGE_GRAPH_ROWS,
            });
        }
        edges.push(decode_package_edge(&row)?);
    }
    Ok(edges)
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

/// Checks that one graph read names the currently selected view root in the
/// same SQL snapshot. The graph witness identifies dependency content; its
/// root is the view root that admitted that content.
pub(crate) async fn graph_root_matches_selected_view(
    connection: &turso::Connection,
    graph: &PackageGraphMetadata,
) -> Result<bool, ProjectionError> {
    let Some(view) = metadata_from(connection).await? else {
        return Ok(false);
    };
    Ok(graph.root.as_slice() == view.root.as_slice())
}

/// Checks both source inventories for one spelling, validates every stored
/// coordinate tag, then returns the exact requested typed coordinate. Scanning
/// both tags prevents an invalid persisted kind from being reported as a
/// misleading Missing result for a PURL or Local query.
pub(crate) async fn package_source_inventory(
    connection: &turso::Connection,
    coordinate: &PackageReference,
) -> Result<Vec<PackageGraphSourceKey>, ProjectionError> {
    let sources = source_keys_for_table(
        connection,
        "backend_projection_package_sources",
        coordinate.as_str(),
    )
    .await?;
    let witnesses = source_keys_for_table(
        connection,
        "backend_projection_package_source_witnesses",
        coordinate.as_str(),
    )
    .await?;
    if sources != witnesses {
        return Err(ProjectionError::Database(turso::Error::Misuse(
            "package graph source inventory differs from its witness index".to_owned(),
        )));
    }
    Ok(sources
        .into_iter()
        .filter(|source| &source.coordinate == coordinate)
        .collect())
}

async fn source_keys_for_table(
    connection: &turso::Connection,
    table: &'static str,
    spelling: &str,
) -> Result<Vec<PackageGraphSourceKey>, ProjectionError> {
    let sql = format!(
        "SELECT source, coordinate_kind, source_authority_kind, source_authority_id \
         FROM {table} WHERE source=?1 \
         ORDER BY coordinate_kind, source_authority_kind, source_authority_id LIMIT ?2"
    );
    let mut rows = connection
        .query(
            &sql,
            turso::params![spelling, MAX_GRAPH_SOURCE_KEY_READ_LIMIT],
        )
        .await?;
    let mut keys = Vec::with_capacity(MAX_GRAPH_SOURCE_KEYS_FOR_SPELLING);
    let mut purl_count = 0;
    let mut local_count = 0;
    while let Some(row) = rows.next().await? {
        if keys.len() == MAX_GRAPH_SOURCE_KEYS_FOR_SPELLING {
            drop(rows);
            return Err(ProjectionError::ReadLimitExceeded {
                maximum: MAX_PACKAGE_GRAPH_AUTHORITIES,
            });
        }
        let coordinate = decode_package_reference(row.get(1)?, row.get(0)?)?;
        let count = match coordinate.kind() {
            PackageReferenceKind::Purl => &mut purl_count,
            PackageReferenceKind::Local => &mut local_count,
        };
        *count += 1;
        if *count > MAX_PACKAGE_GRAPH_AUTHORITIES {
            drop(rows);
            return Err(ProjectionError::ReadLimitExceeded {
                maximum: MAX_PACKAGE_GRAPH_AUTHORITIES,
            });
        }
        keys.push(PackageGraphSourceKey::new(
            coordinate,
            decode_source_authority(row.get(2)?, row.get(3)?)?,
        ));
    }
    drop(rows);
    Ok(keys)
}

pub(crate) async fn package_source_state(
    connection: &turso::Connection,
    source: &PackageGraphSourceKey,
) -> Result<PackageGraphSourceState, ProjectionError> {
    let mut rows = connection
        .query(
            "SELECT s.source, s.coordinate_kind, s.source_authority_kind, \
             s.source_authority_id, w.facts_witness, st.state, st.reason, \
             EXISTS(SELECT 1 FROM backend_projection_package_edges AS e \
                    WHERE e.source=s.source AND e.coordinate_kind=s.coordinate_kind \
                    AND e.source_authority_kind=s.source_authority_kind \
                    AND e.source_authority_id=s.source_authority_id LIMIT 1) \
             FROM backend_projection_package_sources AS s \
             JOIN backend_projection_package_source_witnesses AS w \
             ON s.source=w.source AND s.coordinate_kind=w.coordinate_kind \
             AND s.source_authority_kind=w.source_authority_kind \
             AND s.source_authority_id=w.source_authority_id \
             LEFT JOIN backend_projection_package_states AS st \
             ON s.source=st.source AND s.coordinate_kind=st.coordinate_kind \
             AND s.source_authority_kind=st.source_authority_kind \
             AND s.source_authority_id=st.source_authority_id \
             WHERE s.source=?1 AND s.coordinate_kind=?2 \
             AND s.source_authority_kind=?3 AND s.source_authority_id=?4 LIMIT 2",
            turso::params![
                source.coordinate.as_str(),
                i64::from(source.coordinate.kind().tag()),
                source.authority.kind_tag(),
                source.authority.id_bytes().as_slice()
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(misuse_graph_edge(
            "package graph source is missing from its source/witness inventory",
        ));
    };
    let stored_source = decode_source_key(
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        graph_digest(row.get(3)?, "graph source authority id")?,
    )?;
    if stored_source != *source {
        return Err(misuse_graph_edge(
            "package graph source witness has a different typed identity",
        ));
    }
    let witness = graph_digest(row.get(4)?, "graph source facts witness")?;
    let state = row.get::<Option<i64>>(5)?;
    let reason = row.get::<Option<String>>(6)?;
    let has_edge = match row.get::<i64>(7)? {
        0 => false,
        1 => true,
        _ => return Err(misuse_graph_edge("invalid package graph edge-existence flag")),
    };
    if rows.next().await?.is_some() {
        return Err(misuse_graph_edge("package graph source witness is duplicated"));
    }
    drop(rows);
    let Some(state) = state else {
        if reason.is_some() {
            return Err(misuse_graph_edge(
                "package graph state reason exists without a state kind",
            ));
        }
        let known_empty = checked_source_witness(
            source,
            DependencyFacts::Known(Vec::<PackageDependencyRecord>::new().into_boxed_slice()),
        )?;
        // This indexed probe distinguishes a deleted Unknown/Unavailable row
        // from Known facts. It does not prove every edge against the graph
        // witness; bounded pages have no Merkle membership proof.
        let is_known_empty = witness == known_empty;
        if (is_known_empty && !has_edge) || (!is_known_empty && has_edge) {
            return Ok(PackageGraphSourceState::Known);
        }
        return Err(misuse_graph_edge(
            "package graph state is missing and its witness does not match Known-empty or indexed edges",
        ));
    };

    if has_edge {
        return Err(misuse_graph_edge(
            "package graph gap state coexists with indexed dependency edges",
        ));
    }
    let reason = decode_exact_product_text(
        reason.ok_or_else(|| misuse_graph_edge("package graph state reason is missing"))?,
        "invalid graph state reason",
    )?;
    let facts = match state {
        1 => DependencyFacts::Unknown(reason.clone()),
        2 => DependencyFacts::Unavailable(reason.clone()),
        _ => return Err(misuse_graph_edge("invalid graph state kind")),
    };
    if checked_source_witness(source, facts)? != witness {
        return Err(misuse_graph_edge(
            "package graph state differs from its checked source witness",
        ));
    }
    Ok(if state == 1 {
        PackageGraphSourceState::Unknown(reason)
    } else {
        PackageGraphSourceState::Unavailable(reason)
    })
}

async fn package_state_from(
    connection: &turso::Connection,
    source: &PackageGraphSourceKey,
) -> Result<Option<PackageGraphState>, ProjectionError> {
    let state = package_source_state(connection, source).await?;
    let (kind, reason) = match state {
        PackageGraphSourceState::Known => return Ok(None),
        PackageGraphSourceState::Unknown(reason) => (1, reason),
        PackageGraphSourceState::Unavailable(reason) => (2, reason),
    };
    Ok(Some(PackageGraphState {
        source: source.coordinate.clone(),
        authority: source.authority,
        kind,
        reason: reason.as_str().to_owned(),
    }))
}

pub(crate) fn checked_source_witness(
    source: &PackageGraphSourceKey,
    facts: DependencyFacts<Box<[PackageDependencyRecord]>>,
) -> Result<[u8; 32], ProjectionError> {
    let checked = CheckedPackageGraphFacts::new(vec![(source.clone(), facts)])
        .map_err(|_| misuse_graph_edge("invalid package graph source witness input"))?;
    checked
        .source_witnesses()
        .first()
        .copied()
        .ok_or_else(|| misuse_graph_edge("package graph source witness input is empty"))
}

const EDGE_WRITE_BATCH: usize = 64;
const SOURCE_KEY_BATCH: usize = 128;

fn compare_source_keys(left: &PackageGraphSourceKey, right: &PackageGraphSourceKey) -> Ordering {
    left.canonical_cmp(right)
}

fn compare_stored_source(
    source: &str,
    coordinate_kind: i64,
    authority_kind: i64,
    authority_id: &[u8; 32],
    desired: &PackageGraphSourceKey,
) -> Ordering {
    source
        .cmp(desired.coordinate.as_str())
        .then_with(|| coordinate_kind.cmp(&i64::from(desired.coordinate.kind().tag())))
        .then_with(|| authority_kind.cmp(&desired.authority.kind_tag()))
        .then_with(|| authority_id.cmp(&desired.authority.id_bytes()))
}

fn graph_digest(value: Vec<u8>, field: &'static str) -> Result<[u8; 32], ProjectionError> {
    value
        .try_into()
        .map_err(|_| ProjectionError::CorruptMetadata { field })
}

fn decode_source_key(
    source: String,
    coordinate_kind: i64,
    authority_kind: i64,
    authority_id: [u8; 32],
) -> Result<PackageGraphSourceKey, ProjectionError> {
    let coordinate = decode_package_reference(coordinate_kind, source)?;
    Ok(PackageGraphSourceKey::new(
        coordinate,
        decode_source_authority(authority_kind, authority_id.to_vec())?,
    ))
}

pub(crate) fn decode_package_reference(
    coordinate_kind: i64,
    coordinate: String,
) -> Result<PackageReference, ProjectionError> {
    let tag = u8::try_from(coordinate_kind).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse(
            "invalid package graph coordinate kind".to_owned(),
        ))
    })?;
    let kind = PackageReferenceKind::try_from(tag).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse(
            "invalid package graph coordinate kind".to_owned(),
        ))
    })?;
    if matches!(kind, PackageReferenceKind::Local) && coordinate.trim() != coordinate.as_str() {
        return Err(ProjectionError::Database(turso::Error::Misuse(
            "noncanonical package graph source coordinate".to_owned(),
        )));
    }
    PackageReference::from_kind(kind, coordinate).map_err(|_| {
        ProjectionError::Database(turso::Error::Misuse(
            "invalid package graph source coordinate".to_owned(),
        ))
    })
}

async fn stored_edge_ids_for_sources(
    connection: &turso::Connection,
    sources: &[PackageGraphSourceKey],
) -> Result<BTreeSet<[u8; 32]>, ProjectionError> {
    let mut ids = BTreeSet::new();
    for batch in sources.chunks(SOURCE_KEY_BATCH) {
        if batch.is_empty() {
            continue;
        }
        let mut sql = String::from("SELECT edge_id FROM backend_projection_package_edges WHERE ");
        for index in 0..batch.len() {
            if index != 0 {
                sql.push_str(" OR ");
            }
            sql.push_str("(source=? AND coordinate_kind=? AND source_authority_kind=? AND source_authority_id=?)");
        }
        let values = batch.iter().flat_map(source_key_values).collect::<Vec<_>>();
        let mut rows = connection
            .query(&sql, turso::params_from_iter(values))
            .await?;
        while let Some(row) = rows.next().await? {
            ids.insert(graph_digest(row.get(0)?, "edge_id")?);
        }
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
         edge_id, source, coordinate_kind, source_authority_kind, source_authority_id, target_ecosystem, \
         target_name, requirement, resolved, scope, optional, authority, frontier, provenance, \
         facts_version) VALUES ",
    );
    for index in 0..records.len() {
        if index != 0 {
            sql.push(',');
        }
        sql.push_str("(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)");
    }
    let mut values = Vec::with_capacity(records.len().saturating_mul(15));
    for record in records {
        values.extend(edge_values(record));
    }
    connection
        .execute(&sql, turso::params_from_iter(values))
        .await?;
    Ok(())
}

fn edge_values(record: &PackageDependencyRecord) -> [turso::Value; 15] {
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
        turso::Value::Integer(i64::from(record.source.kind().tag())),
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

fn source_key_values(source: &PackageGraphSourceKey) -> [turso::Value; 4] {
    [
        turso::Value::Text(source.coordinate.as_str().to_owned()),
        turso::Value::Integer(i64::from(source.coordinate.kind().tag())),
        turso::Value::Integer(source.authority.kind_tag()),
        turso::Value::Blob(source.authority.id_bytes().to_vec()),
    ]
}

async fn delete_package_source_rows(
    connection: &turso::Connection,
    table: &str,
    sources: &[PackageGraphSourceKey],
) -> Result<(), ProjectionError> {
    for batch in sources.chunks(SOURCE_KEY_BATCH) {
        if batch.is_empty() {
            continue;
        }
        let mut sql = format!("DELETE FROM {table} WHERE ");
        for index in 0..batch.len() {
            if index != 0 {
                sql.push_str(" OR ");
            }
            sql.push_str("(source=? AND coordinate_kind=? AND source_authority_kind=? AND source_authority_id=?)");
        }
        let values = batch.iter().flat_map(source_key_values).collect::<Vec<_>>();
        connection
            .execute(&sql, turso::params_from_iter(values))
            .await?;
    }
    Ok(())
}

async fn insert_package_sources(
    connection: &turso::Connection,
    sources: &[&PackageGraphSourceKey],
) -> Result<(), ProjectionError> {
    for batch in sources.chunks(SOURCE_KEY_BATCH) {
        if batch.is_empty() {
            continue;
        }
        let mut sql = String::from(
            "INSERT OR IGNORE INTO backend_projection_package_sources \
             (source, coordinate_kind, source_authority_kind, source_authority_id) VALUES ",
        );
        append_value_rows(&mut sql, batch.len(), 4);
        let values = batch
            .iter()
            .flat_map(|source| source_key_values(source))
            .collect::<Vec<_>>();
        connection
            .execute(&sql, turso::params_from_iter(values))
            .await?;
    }
    Ok(())
}

async fn upsert_package_source_witnesses(
    connection: &turso::Connection,
    facts: &[PackageDependencySourceFacts],
    source_witnesses: &[[u8; 32]],
    changed: &[usize],
) -> Result<(), ProjectionError> {
    for batch in changed.chunks(SOURCE_KEY_BATCH) {
        if batch.is_empty() {
            continue;
        }
        let mut sql = String::from(
            "INSERT INTO backend_projection_package_source_witnesses \
             (source, coordinate_kind, source_authority_kind, source_authority_id, facts_witness) VALUES ",
        );
        append_value_rows(&mut sql, batch.len(), 5);
        sql.push_str(
            " ON CONFLICT(source, coordinate_kind, source_authority_kind, source_authority_id) \
             DO UPDATE SET facts_witness=excluded.facts_witness \
             WHERE backend_projection_package_source_witnesses.facts_witness != excluded.facts_witness",
        );
        let values = batch
            .iter()
            .flat_map(|index| {
                let source = &facts[*index].0;
                let mut values = source_key_values(source).to_vec();
                values.push(turso::Value::Blob(source_witnesses[*index].to_vec()));
                values
            })
            .collect::<Vec<_>>();
        connection
            .execute(&sql, turso::params_from_iter(values))
            .await?;
    }
    Ok(())
}

async fn upsert_package_states(
    connection: &turso::Connection,
    facts: &[PackageDependencySourceFacts],
    changed: &[usize],
) -> Result<(), ProjectionError> {
    let states = changed
        .iter()
        .filter_map(|index| {
            let (state, reason) = match &facts[*index].1 {
                DependencyFacts::Known(_) => return None,
                DependencyFacts::Unknown(reason) => (1_i64, reason.as_str()),
                DependencyFacts::Unavailable(reason) => (2_i64, reason.as_str()),
            };
            Some((&facts[*index].0, state, reason))
        })
        .collect::<Vec<_>>();
    for batch in states.chunks(SOURCE_KEY_BATCH) {
        if batch.is_empty() {
            continue;
        }
        let mut sql = String::from(
            "INSERT INTO backend_projection_package_states \
             (source, coordinate_kind, source_authority_kind, source_authority_id, state, reason) VALUES ",
        );
        append_value_rows(&mut sql, batch.len(), 6);
        sql.push_str(
            " ON CONFLICT(source, coordinate_kind, source_authority_kind, source_authority_id) \
             DO UPDATE SET state=excluded.state, reason=excluded.reason",
        );
        let values = batch
            .iter()
            .flat_map(|(source, state, reason)| {
                let mut values = source_key_values(source).to_vec();
                values.push(turso::Value::Integer(*state));
                values.push(turso::Value::Text((*reason).to_owned()));
                values
            })
            .collect::<Vec<_>>();
        connection
            .execute(&sql, turso::params_from_iter(values))
            .await?;
    }
    Ok(())
}

fn append_value_rows(sql: &mut String, rows: usize, columns: usize) {
    for row in 0..rows {
        if row != 0 {
            sql.push(',');
        }
        sql.push('(');
        for column in 0..columns {
            if column != 0 {
                sql.push(',');
            }
            sql.push('?');
        }
        sql.push(')');
    }
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

pub(crate) fn decode_source_authority(
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

pub(crate) fn decode_package_edge(
    row: &turso::Row,
) -> Result<PackageDependencyRecord, ProjectionError> {
    let edge_id = graph_digest(row.get(0)?, "graph_edge_id")?;
    let source = decode_package_reference(row.get(2)?, row.get::<String>(1)?)?;
    let source_authority = decode_source_authority(row.get(3)?, row.get(4)?)?;
    let target_ecosystem =
        backend_library::RegistryEcosystem::parse_canonical(match row.get::<i64>(5)? {
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
    let name = decode_exact_product_text(row.get::<String>(6)?, "graph target name")?;
    let requirement = decode_exact_product_text(row.get::<String>(7)?, "graph requirement")?;
    let resolved = row
        .get::<Option<String>>(8)?
        .map(|value| {
            PackageReference::from_kind(PackageReferenceKind::Purl, value)
                .map_err(|_| misuse_graph_edge("invalid graph resolution"))
        })
        .transpose()?;
    let scope = match row.get::<i64>(9)? {
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
    let authority = match row.get::<i64>(11)? {
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
    let frontier: Vec<u8> = row.get(12)?;
    let provenance: Vec<u8> = row.get(13)?;
    let facts_version: Vec<u8> = row.get(14)?;
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
    let target = backend_library::PackageDependencyTarget {
        ecosystem: target_ecosystem,
        name,
        requirement,
        resolved,
    };
    target
        .admit()
        .map_err(|_| misuse_graph_edge("invalid graph target"))?;
    if !source_authority.matches_evidence(authority) {
        return Err(misuse_graph_edge(
            "graph evidence does not match its source authority",
        ));
    }
    let record = PackageDependencyRecord::new_with_source_authority(
        source,
        source_authority,
        target,
        scope,
        match row.get::<i64>(10)? {
            0 => false,
            1 => true,
            _ => return Err(misuse_graph_edge("invalid graph optional flag")),
        },
        backend_library::DependencyEvidence {
            authority,
            frontier,
            provenance,
        },
    );
    if edge_id != facts_version || record.facts_version != facts_version {
        return Err(misuse_graph_edge("graph edge identity mismatch"));
    }
    Ok(record)
}

fn decode_exact_product_text(
    value: String,
    field: &'static str,
) -> Result<backend_library::ProductText, ProjectionError> {
    if value.trim() != value.as_str() {
        return Err(misuse_graph_edge(field));
    }
    backend_library::ProductText::new(value).map_err(|_| misuse_graph_edge(field))
}

fn misuse_graph_edge(message: &'static str) -> ProjectionError {
    ProjectionError::Database(turso::Error::Misuse(message.to_owned()))
}

//! Root-bound Turso projection for local query acceleration.
//!
//! The versioned workspace remains the authority. This crate owns a disposable
//! SQL projection whose sole mutable row set is bound to one immutable
//! [`ViewRoot`]. Hot transitions update only changed rows in one transaction;
//! cold recovery can rebuild the projection from a certified root. A failed or
//! stale projection therefore cannot advance canonical workspace state.

#![deny(unsafe_code)]

use backend_library::{
    CommittedViewDelta, DependencyAuthority, DependencyFacts, DependencyScope, Fragment,
    PackageDependencyRecord, PackageReference, Row, RowChange, RowId, RowState, ViewDelta,
    ViewRoot,
};
use std::fmt;
use std::path::{Path, PathBuf};

const SCHEMA_VERSION: i64 = 1;
const MAX_AUDIT_ROOTS: i64 = 128;

const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS backend_projection_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    schema_version INTEGER NOT NULL,
    root BLOB NOT NULL,
    view_version BLOB NOT NULL,
    row_count INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS backend_projection_rows (
    row_id TEXT PRIMARY KEY,
    row_kind INTEGER NOT NULL,
    content_hash BLOB NOT NULL,
    state INTEGER NOT NULL,
    label TEXT NOT NULL,
    score INTEGER,
    package_id TEXT,
    parent_id TEXT,
    signature TEXT,
    document TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS backend_projection_rows_label
    ON backend_projection_rows(label);
CREATE INDEX IF NOT EXISTS backend_projection_rows_package
    ON backend_projection_rows(package_id, parent_id, row_id);
CREATE TABLE IF NOT EXISTS backend_projection_commits (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    root BLOB NOT NULL UNIQUE,
    parent_root BLOB,
    delta_id BLOB,
    changed_rows INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS backend_projection_package_graph_meta (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    root BLOB NOT NULL,
    edge_count INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS backend_projection_package_edges (
    edge_id BLOB PRIMARY KEY,
    root BLOB NOT NULL,
    source TEXT NOT NULL,
    target_ecosystem INTEGER NOT NULL,
    target_name TEXT NOT NULL,
    requirement TEXT NOT NULL,
    resolved TEXT,
    scope INTEGER NOT NULL,
    optional INTEGER NOT NULL,
    authority INTEGER NOT NULL,
    frontier BLOB NOT NULL,
    provenance BLOB NOT NULL,
    facts_version BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS backend_projection_package_edges_source
    ON backend_projection_package_edges(root, source, target_name);
CREATE INDEX IF NOT EXISTS backend_projection_package_edges_target
    ON backend_projection_package_edges(root, target_ecosystem, target_name, resolved);
CREATE TABLE IF NOT EXISTS backend_projection_package_states (
    root BLOB NOT NULL,
    source TEXT NOT NULL,
    state INTEGER NOT NULL,
    reason TEXT NOT NULL,
    PRIMARY KEY(root, source)
);
CREATE INDEX IF NOT EXISTS backend_projection_package_states_source
    ON backend_projection_package_states(root, source);
";

const UPSERT_ROW: &str = r"
INSERT INTO backend_projection_rows (
    row_id, row_kind, content_hash, state, label, score,
    package_id, parent_id, signature, document
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
ON CONFLICT(row_id) DO UPDATE SET
    row_kind = excluded.row_kind,
    content_hash = excluded.content_hash,
    state = excluded.state,
    label = excluded.label,
    score = excluded.score,
    package_id = excluded.package_id,
    parent_id = excluded.parent_id,
    signature = excluded.signature,
    document = excluded.document
WHERE backend_projection_rows.content_hash != excluded.content_hash
";

/// Durable path used by the product composition.
pub const FILE_NAME: &str = "projection.turso";

/// Result of aligning the projection to an immutable view root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionUpdate {
    /// The database already named this exact root; no SQL mutation ran.
    Reused {
        /// Number of rows retained without work.
        rows: u64,
    },
    /// A cold or stale database was rebuilt from the supplied root.
    Rebuilt {
        /// Rows materialized from the root.
        rows: u64,
    },
    /// One checked transition advanced the existing projection.
    Advanced {
        /// Rows inserted, replaced, or removed by the transition.
        changed_rows: u64,
    },
}

/// Failure to open, align, or query the derived projection.
#[derive(Debug)]
pub enum ProjectionError {
    /// A database path was not representable as UTF-8.
    NonUtf8Path(PathBuf),
    /// A database operation failed.
    Database(turso::Error),
    /// Persisted projection metadata has an unknown schema.
    Schema {
        /// Schema number read from the projection metadata row.
        found: i64,
    },
    /// A hot transition was offered against a different cached root.
    StaleTransition,
    /// A view size exceeded the SQL integer domain.
    RowCountOverflow,
    /// A package graph exceeded the bounded SQL graph projection size.
    GraphRowCountOverflow,
}

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonUtf8Path(path) => {
                write!(
                    formatter,
                    "Turso projection path is not UTF-8: {}",
                    path.display()
                )
            }
            Self::Database(error) => error.fmt(formatter),
            Self::Schema { found } => {
                write!(formatter, "unsupported Turso projection schema {found}")
            }
            Self::StaleTransition => {
                formatter.write_str("Turso projection transition has the wrong base root")
            }
            Self::RowCountOverflow => formatter.write_str("projection row count overflows i64"),
            Self::GraphRowCountOverflow => {
                formatter.write_str("package graph row count overflows i64")
            }
        }
    }
}

impl std::error::Error for ProjectionError {}

impl From<turso::Error> for ProjectionError {
    fn from(error: turso::Error) -> Self {
        Self::Database(error)
    }
}

/// One process-owned Turso accelerator.
///
/// `Connection::transaction(&mut self)` makes overlapping writers impossible
/// through this API: a projection update exclusively borrows the connection
/// until its root fence commits.
pub struct TursoProjection {
    _database: turso::Database,
    connection: turso::Connection,
}

impl fmt::Debug for TursoProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TursoProjection")
            .finish_non_exhaustive()
    }
}

impl TursoProjection {
    /// Opens or creates a local projection database and validates its schema.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid path, database failure, or incompatible
    /// on-disk projection schema.
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, ProjectionError> {
        let path = path.as_ref();
        let text = path
            .to_str()
            .ok_or_else(|| ProjectionError::NonUtf8Path(path.to_path_buf()))?;
        let database = turso::Builder::new_local(text).build().await?;
        let connection = database.connect()?;
        connection.execute_batch(SCHEMA).await?;
        let projection = Self {
            _database: database,
            connection,
        };
        if let Some(metadata) = projection.metadata().await?
            && metadata.schema_version != SCHEMA_VERSION
        {
            return Err(ProjectionError::Schema {
                found: metadata.schema_version,
            });
        }
        Ok(projection)
    }

    /// Aligns the database with a complete immutable root.
    ///
    /// The exact-root fast path performs one metadata read and no writes.
    /// Rebuild is reserved for first boot, recovery, or a missed transition.
    ///
    /// # Errors
    ///
    /// Returns an error when Turso rejects the atomic rebuild or the row count
    /// cannot be represented by the projection schema.
    pub async fn synchronize(
        &mut self,
        view: &ViewRoot,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        if let Some(metadata) = self.metadata().await?
            && metadata.root.as_slice() == view.root().as_bytes()
            && metadata.view_version.as_slice() == view.version().as_bytes()
        {
            return Ok(ProjectionUpdate::Reused {
                rows: u64::try_from(metadata.row_count).unwrap_or(0),
            });
        }

        let row_count =
            i64::try_from(view.row_count()).map_err(|_| ProjectionError::RowCountOverflow)?;
        let tx = self.connection.transaction().await?;
        tx.execute("DELETE FROM backend_projection_rows", ())
            .await?;
        for row in view.rows() {
            upsert_row(&tx, row).await?;
        }
        tx.execute(
            "INSERT INTO backend_projection_meta \
             (singleton, schema_version, root, view_version, row_count) \
             VALUES (1, ?1, ?2, ?3, ?4) \
             ON CONFLICT(singleton) DO UPDATE SET \
             schema_version=excluded.schema_version, root=excluded.root, \
             view_version=excluded.view_version, row_count=excluded.row_count",
            turso::params![
                SCHEMA_VERSION,
                view.root().as_bytes().as_slice(),
                view.version().as_bytes().as_slice(),
                row_count
            ],
        )
        .await?;
        record_commit(&tx, view.root().as_bytes(), None, None, row_count).await?;
        prune_commits(&tx).await?;
        tx.commit().await?;
        Ok(ProjectionUpdate::Rebuilt {
            rows: view.row_count(),
        })
    }

    /// Applies one checked view transition against the exact cached base root.
    ///
    /// Reset events intentionally use [`Self::synchronize`]. All ordinary row
    /// changes update one row and the root fence in the same transaction.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::StaleTransition`] when the database does not
    /// name the transition's exact base, or a database error if the atomic
    /// update fails.
    pub async fn apply(
        &mut self,
        delta: &CommittedViewDelta,
    ) -> Result<ProjectionUpdate, ProjectionError> {
        self.apply_all(std::slice::from_ref(delta)).await
    }

    /// Applies a contiguous checked transition chain in one SQL transaction.
    ///
    /// The first base and final target form the database fence. Intermediate
    /// roots remain in the canonical view journal, while SQL avoids a commit
    /// and metadata rewrite per changed row.
    ///
    /// # Errors
    ///
    /// Returns [`ProjectionError::StaleTransition`] when the chain is empty,
    /// discontinuous, or starts at a different cached root.
    pub async fn apply_all(
        &mut self,
        deltas: &[CommittedViewDelta],
    ) -> Result<ProjectionUpdate, ProjectionError> {
        let Some(first) = deltas.first() else {
            let metadata = self
                .metadata()
                .await?
                .ok_or(ProjectionError::StaleTransition)?;
            return Ok(ProjectionUpdate::Reused {
                rows: u64::try_from(metadata.row_count).unwrap_or(0),
            });
        };
        let last = deltas.last().ok_or(ProjectionError::StaleTransition)?;
        if deltas.windows(2).any(|pair| {
            pair[0].target_root() != pair[1].base_root()
                || pair[0].target_version() != pair[1].base_version()
        }) {
            return Err(ProjectionError::StaleTransition);
        }
        if deltas
            .iter()
            .any(|delta| matches!(delta.delta(), ViewDelta::Reset { .. }))
        {
            return self.synchronize(last.target_view()).await;
        }
        let Some(metadata) = self.metadata().await? else {
            return Err(ProjectionError::StaleTransition);
        };
        if metadata.root.as_slice() != first.base_root().as_bytes()
            || metadata.view_version.as_slice() != first.base_version().as_bytes()
        {
            return Err(ProjectionError::StaleTransition);
        }

        let target = last.target_view();
        let target_count =
            i64::try_from(target.row_count()).map_err(|_| ProjectionError::RowCountOverflow)?;
        let tx = self.connection.transaction().await?;
        let mut changed_rows = 0_u64;
        for delta in deltas {
            let affected = apply_delta(&tx, delta.delta()).await?;
            changed_rows = changed_rows.saturating_add(affected);
        }
        let fenced = tx
            .execute(
                "UPDATE backend_projection_meta SET root=?1, view_version=?2, row_count=?3 \
                 WHERE singleton=1 AND schema_version=?4 AND root=?5 AND view_version=?6",
                turso::params![
                    target.root().as_bytes().as_slice(),
                    target.version().as_bytes().as_slice(),
                    target_count,
                    SCHEMA_VERSION,
                    first.base_root().as_bytes().as_slice(),
                    first.base_version().as_bytes().as_slice()
                ],
            )
            .await?;
        if fenced != 1 {
            return Err(ProjectionError::StaleTransition);
        }
        let changed_rows_i64 =
            i64::try_from(changed_rows).map_err(|_| ProjectionError::RowCountOverflow)?;
        let delta_id = (deltas.len() == 1).then(|| first.id());
        record_commit(
            &tx,
            target.root().as_bytes(),
            Some(first.base_root().as_bytes()),
            delta_id
                .as_ref()
                .map(backend_library::ViewDeltaId::as_bytes),
            changed_rows_i64,
        )
        .await?;
        prune_commits(&tx).await?;
        tx.commit().await?;
        Ok(ProjectionUpdate::Advanced { changed_rows })
    }

    /// Searches the materialized rows and returns stable row IDs.
    ///
    /// Results are always accompanied by the exact root that fenced the SQL
    /// snapshot, so callers cannot mistake cache output for another version.
    ///
    /// # Errors
    ///
    /// Returns an error when query execution or metadata decoding fails.
    pub async fn search(&self, text: &str, limit: u32) -> Result<RootedRows, ProjectionError> {
        let metadata = self
            .metadata()
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        let pattern = format!("%{}%", escape_like(text));
        let mut rows = self
            .connection
            .query(
                "SELECT row_id FROM backend_projection_rows \
                 WHERE label LIKE ?1 ESCAPE '\\' OR signature LIKE ?1 ESCAPE '\\' \
                 OR document LIKE ?1 ESCAPE '\\' ORDER BY row_id LIMIT ?2",
                turso::params![pattern, i64::from(limit)],
            )
            .await?;
        let mut ids = Vec::new();
        while let Some(row) = rows.next().await? {
            ids.push(row.get::<String>(0)?);
        }
        Ok(RootedRows {
            root: metadata.root.into_boxed_slice(),
            ids: ids.into_boxed_slice(),
        })
    }

    async fn metadata(&self) -> Result<Option<Metadata>, ProjectionError> {
        let mut rows = self
            .connection
            .query(
                "SELECT schema_version, root, view_version, row_count \
                 FROM backend_projection_meta WHERE singleton=1",
                (),
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(Metadata {
            schema_version: row.get(0)?,
            root: row.get(1)?,
            view_version: row.get(2)?,
            row_count: row.get(3)?,
        }))
    }

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
        facts: &[(PackageReference, DependencyFacts<Box<[PackageDependencyRecord]>>)],
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
        let edge_count_i64 = i64::try_from(edge_count)
            .map_err(|_| ProjectionError::GraphRowCountOverflow)?;
        let tx = self.connection.transaction().await?;
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
        let metadata = self
            .package_graph_metadata()
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        let mut rows = self
            .connection
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
        let state = self.package_state(&metadata.root, source.as_str()).await?;
        Ok(RootedPackageGraph {
            root: metadata.root.into_boxed_slice(),
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
        let metadata = self
            .package_graph_metadata()
            .await?
            .ok_or(ProjectionError::StaleTransition)?;
        let PackageReference::Purl(target_url) = target else {
            return Ok(RootedPackageGraph {
                root: metadata.root.into_boxed_slice(),
                edges: Box::new([]),
                state: None,
            });
        };
        let mut rows = self
            .connection
            .query(
                "SELECT edge_id, source, target_ecosystem, target_name, requirement, resolved, \
                 scope, optional, authority, frontier, provenance, facts_version \
                 FROM backend_projection_package_edges WHERE root=?1 AND target_ecosystem=?2 \
                 AND target_name=?3 AND (resolved IS NULL OR resolved=?4) ORDER BY edge_id",
                turso::params![
                    metadata.root.as_slice(),
                    i64::from(target_url.package_type().registry().map_or(0, |value| value as u8)),
                    target_url.lineage_name(),
                    target.as_str()
                ],
            )
            .await?;
        let mut edges = Vec::new();
        while let Some(row) = rows.next().await? {
            edges.push(decode_package_edge(&row)?);
        }
        Ok(RootedPackageGraph {
            root: metadata.root.into_boxed_slice(),
            edges: edges.into_boxed_slice(),
            state: None,
        })
    }

    async fn package_graph_metadata(&self) -> Result<Option<PackageGraphMetadata>, ProjectionError> {
        let mut rows = self
            .connection
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

    async fn package_state(
        &self,
        root: &[u8],
        source: &str,
    ) -> Result<Option<PackageGraphState>, ProjectionError> {
        let mut rows = self
            .connection
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
}

/// Query output fenced by one immutable projection root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootedRows {
    /// Exact projected root.
    pub root: Box<[u8]>,
    /// Stable matching row identities.
    pub ids: Box<[String]>,
}

/// A graph query result fenced to the immutable root that supplied it.
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

struct Metadata {
    schema_version: i64,
    root: Vec<u8>,
    view_version: Vec<u8>,
    row_count: i64,
}

struct PackageGraphMetadata {
    root: Vec<u8>,
    edge_count: i64,
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
    let target_ecosystem = backend_library::RegistryEcosystem::parse_canonical(
        match row.get::<i64>(2)? {
            1 => "cargo",
            2 => "npm",
            3 => "pypi",
            4 => "maven",
            5 => "nuget",
            6 => "golang",
            7 => "cpp",
            _ => return Err(ProjectionError::Database(turso::Error::Misuse("invalid graph ecosystem".to_owned()))),
        },
    )
    .map_err(|_| ProjectionError::Database(turso::Error::Misuse("invalid graph ecosystem".to_owned())))?;
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
        .map_err(|_| ProjectionError::Database(turso::Error::Misuse("invalid graph resolution".to_owned())))?;
    let scope = match row.get::<i64>(6)? {
        0 => DependencyScope::Runtime,
        1 => DependencyScope::Optional,
        2 => DependencyScope::Development,
        3 => DependencyScope::Build,
        4 => DependencyScope::Peer,
        _ => return Err(ProjectionError::Database(turso::Error::Misuse("invalid graph scope".to_owned()))),
    };
    let authority = match row.get::<i64>(8)? {
        0 => DependencyAuthority::RegistryMetadata,
        1 => DependencyAuthority::ArchiveManifest,
        2 => DependencyAuthority::ForgeManifest,
        3 => DependencyAuthority::LocalManifest,
        _ => return Err(ProjectionError::Database(turso::Error::Misuse("invalid graph authority".to_owned()))),
    };
    let frontier: Vec<u8> = row.get(9)?;
    let provenance: Vec<u8> = row.get(10)?;
    let facts_version: Vec<u8> = row.get(11)?;
    let frontier: [u8; 32] = frontier.try_into().map_err(|_| ProjectionError::Database(turso::Error::Misuse("invalid graph frontier".to_owned())))?;
    let provenance: [u8; 32] = provenance.try_into().map_err(|_| ProjectionError::Database(turso::Error::Misuse("invalid graph provenance".to_owned())))?;
    let facts_version: [u8; 32] = facts_version.try_into().map_err(|_| ProjectionError::Database(turso::Error::Misuse("invalid graph facts version".to_owned())))?;
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
        backend_library::DependencyEvidence { authority, frontier, provenance },
    );
    if record.facts_version != facts_version {
        return Err(ProjectionError::Database(turso::Error::Misuse(
            "graph facts version mismatch".to_owned(),
        )));
    }
    Ok(record)
}

async fn apply_delta(
    connection: &turso::Connection,
    delta: &ViewDelta,
) -> Result<u64, ProjectionError> {
    match delta {
        ViewDelta::Upsert { row } => Ok(upsert_row(connection, row).await?.min(1)),
        ViewDelta::Remove { id } => Ok(connection
            .execute(
                "DELETE FROM backend_projection_rows WHERE row_id = ?1",
                [id.stable_key()],
            )
            .await?
            .min(1)),
        ViewDelta::Coverage { .. } => Ok(0),
        ViewDelta::Patch { changes } => {
            let mut affected = 0_u64;
            for change in changes.iter() {
                let changed = match change {
                    RowChange::Upsert(row) => upsert_row(connection, row).await?.min(1),
                    RowChange::Remove(id) => connection
                        .execute(
                            "DELETE FROM backend_projection_rows WHERE row_id = ?1",
                            [id.stable_key()],
                        )
                        .await?
                        .min(1),
                };
                affected = affected.saturating_add(changed);
            }
            Ok(affected)
        }
        ViewDelta::Reset { .. } => Err(ProjectionError::StaleTransition),
    }
}

async fn upsert_row(connection: &turso::Connection, row: &Row) -> turso::Result<u64> {
    let (kind, id) = row_identity(row.id);
    let state = match row.state {
        RowState::Ready => 0_i64,
        RowState::Loading => 1_i64,
        RowState::Failed => 2_i64,
    };
    let score = row
        .score
        .map(i64::from)
        .map_or(turso::Value::Null, turso::Value::Integer);
    let package = row.package.map_or(turso::Value::Null, |value| {
        turso::Value::Text(backend_library::encode_id(value.as_bytes()))
    });
    let parent = row.parent.map_or(turso::Value::Null, |value| {
        turso::Value::Text(backend_library::encode_id(value.as_bytes()))
    });
    let signature = row.signature.as_ref().map_or(turso::Value::Null, |value| {
        turso::Value::Text(value.clone())
    });
    connection
        .execute(
            UPSERT_ROW,
            turso::params![
                id,
                kind,
                row_hash(row).as_bytes().as_slice(),
                state,
                row.label.as_str(),
                score,
                package,
                parent,
                signature,
                render_document(&row.document)
            ],
        )
        .await
}

async fn record_commit(
    connection: &turso::Connection,
    root: &[u8; 32],
    parent: Option<&[u8; 32]>,
    delta: Option<&[u8; 32]>,
    changed_rows: i64,
) -> turso::Result<()> {
    let parent = parent.map_or(turso::Value::Null, |value| {
        turso::Value::Blob(value.to_vec())
    });
    let delta = delta.map_or(turso::Value::Null, |value| {
        turso::Value::Blob(value.to_vec())
    });
    connection
        .execute(
            "INSERT OR IGNORE INTO backend_projection_commits \
             (root, parent_root, delta_id, changed_rows) VALUES (?1, ?2, ?3, ?4)",
            turso::params![root.as_slice(), parent, delta, changed_rows],
        )
        .await?;
    Ok(())
}

async fn prune_commits(connection: &turso::Connection) -> turso::Result<()> {
    connection
        .execute(
            "DELETE FROM backend_projection_commits WHERE sequence <= \
             (SELECT COALESCE(MAX(sequence), 0) - ?1 FROM backend_projection_commits)",
            [MAX_AUDIT_ROOTS],
        )
        .await?;
    Ok(())
}

fn row_identity(id: RowId) -> (i64, String) {
    let kind = match id {
        RowId::Package(_) => 0,
        RowId::Symbol(_) => 1,
        RowId::Object(_) => 2,
    };
    (kind, id.stable_key())
}

fn row_hash(row: &Row) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.turso.row.v1\0");
    hash_field(&mut hasher, row.id.stable_key().as_bytes());
    hash_field(&mut hasher, row.basis.root.as_bytes());
    hash_field(&mut hasher, row.basis.object.as_bytes());
    hash_field(&mut hasher, row.basis.branch.as_bytes());
    hash_field(&mut hasher, row.basis.log.as_bytes());
    hash_field(&mut hasher, &row.basis.schema.to_be_bytes());
    hash_field(
        &mut hasher,
        &[match row.state {
            RowState::Ready => 0,
            RowState::Loading => 1,
            RowState::Failed => 2,
        }],
    );
    hash_field(&mut hasher, row.label.as_bytes());
    let score = row.score.map(u32::to_be_bytes);
    hash_option(&mut hasher, score.as_ref().map(<[u8; 4]>::as_slice));
    hash_option(
        &mut hasher,
        row.package
            .as_ref()
            .map(|value| value.as_bytes().as_slice()),
    );
    hash_option(
        &mut hasher,
        row.parent.as_ref().map(|value| value.as_bytes().as_slice()),
    );
    hash_option(&mut hasher, row.signature.as_deref().map(str::as_bytes));
    for fragment in &row.document {
        match fragment {
            Fragment::Text(value) => {
                hash_field(&mut hasher, &[0]);
                hash_field(&mut hasher, value.as_bytes());
            }
            Fragment::Code(value) => {
                hash_field(&mut hasher, &[1]);
                hash_field(&mut hasher, value.as_bytes());
            }
            Fragment::Link { label, target } => {
                hash_field(&mut hasher, &[2]);
                hash_field(&mut hasher, label.as_bytes());
                hash_field(&mut hasher, target.as_bytes());
            }
            Fragment::Break => hash_field(&mut hasher, &[3]),
        }
    }
    hasher.finalize()
}

fn hash_option(hasher: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hash_field(hasher, value);
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_field(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_be_bytes());
    hasher.update(value);
}

fn render_document(fragments: &[Fragment]) -> String {
    let mut output = String::new();
    for fragment in fragments {
        match fragment {
            Fragment::Text(value) | Fragment::Code(value) => output.push_str(value),
            Fragment::Link { label, .. } => output.push_str(label),
            Fragment::Break => output.push('\n'),
        }
    }
    output
}

fn escape_like(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use backend_library::{
        AuthorityScopeClaim, ProducerObservationClaims, ProducerObservationVerifier,
    };
    use backend_library::{
        Basis, Coverage, CoverageCapability, Frontier, RowId, ViewDelta, ViewRoot,
        admit_complete_scope, admit_producer_observation, branch_key, log_key, object_version,
        package_key, view_key, view_state_root,
    };
    use backend_library::{
        DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
        PackageDependencyRecord, PackageDependencyTarget, PackageReference, ProductText,
        RegistryEcosystem,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

    struct ExactObservation(backend_library::UntrustedProducerObservation);

    impl ProducerObservationVerifier for ExactObservation {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &backend_library::UntrustedProducerObservation,
        ) -> Result<ProducerObservationClaims, Self::Error> {
            if observation != &self.0 {
                return Err("mismatch");
            }
            Ok(ProducerObservationClaims::new(
                self.0.producer_identity(),
                self.0.scope_root(),
                self.0.context(),
                *blake3::hash(self.0.evidence()).as_bytes(),
            ))
        }
    }

    fn capability(object: backend_library::SemanticObject) -> CoverageCapability {
        let declaration = AuthorityScopeClaim::from_object_version(object);
        let raw = backend_library::UntrustedProducerObservation::new(
            [7; 32],
            declaration.scope_root(),
            [8; 32],
            b"projection-test".to_vec(),
        );
        let observation = admit_producer_observation(raw.clone(), &ExactObservation(raw))
            .unwrap_or_else(|error| panic!("observation: {error}"));
        let coverage = admit_complete_scope(declaration, observation)
            .unwrap_or_else(|error| panic!("coverage: {error}"));
        CoverageCapability::from_authorized_with_evidence(coverage, b"projection-test".to_vec())
            .unwrap_or_else(|error| panic!("capability: {error}"))
    }

    fn root(rows: Vec<Row>) -> ViewRoot {
        let source = view_state_root(&[]);
        let object = object_version(b"projection-test");
        let basis = Basis::with_context(source, object, branch_key("main"), log_key("library"), 1);
        ViewRoot::new_checked(
            view_key(b"projection-test"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
            rows,
            vec![Coverage::Complete],
            capability(object),
        )
        .unwrap_or_else(|error| panic!("root: {error:?}"))
    }

    fn path() -> PathBuf {
        let serial = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "backend-turso-projection-{}-{serial}.db",
            std::process::id()
        ))
    }

    #[test]
    fn exact_root_reuses_database_and_hot_delta_changes_one_row() {
        futures_executor::block_on(async {
            let path = path();
            let empty = root(Vec::new());
            let package = package_key("workspace");
            let row = Row::new(RowId::Package(package), empty.basis(), "workspace");
            let prepared = empty
                .prepare(ViewDelta::Upsert { row }, capability(empty.basis().object))
                .unwrap_or_else(|error| panic!("prepare: {error:?}"));
            let (next, committed) = empty
                .clone()
                .commit(prepared)
                .unwrap_or_else(|error| panic!("commit: {error:?}"));

            let mut projection = TursoProjection::open(&path)
                .await
                .unwrap_or_else(|error| panic!("open: {error}"));
            assert_eq!(
                projection
                    .synchronize(&empty)
                    .await
                    .unwrap_or_else(|error| panic!("initial synchronize: {error}")),
                ProjectionUpdate::Rebuilt { rows: 0 }
            );
            assert_eq!(
                projection
                    .synchronize(&empty)
                    .await
                    .unwrap_or_else(|error| panic!("reuse synchronize: {error}")),
                ProjectionUpdate::Reused { rows: 0 }
            );
            assert_eq!(
                projection
                    .apply(&committed)
                    .await
                    .unwrap_or_else(|error| panic!("apply: {error}")),
                ProjectionUpdate::Advanced { changed_rows: 1 }
            );
            let found = projection
                .search("work", 10)
                .await
                .unwrap_or_else(|error| panic!("search: {error}"));
            assert_eq!(found.root.as_ref(), next.root().as_bytes());
            assert_eq!(found.ids.as_ref(), &[RowId::Package(package).stable_key()]);
            drop(projection);

            let mut reopened = TursoProjection::open(&path)
                .await
                .unwrap_or_else(|error| panic!("reopen: {error}"));
            assert_eq!(
                reopened
                    .synchronize(&next)
                    .await
                    .unwrap_or_else(|error| panic!("reopen synchronize: {error}")),
                ProjectionUpdate::Reused { rows: 1 }
            );
            std::fs::remove_file(&path)
                .unwrap_or_else(|error| panic!("remove projection: {error}"));
        });
    }

    #[test]
    fn package_graph_reuses_root_and_answers_forward_and_reverse_edges() {
        futures_executor::block_on(async {
            let path = path();
            let view = root(Vec::new());
            let source = PackageReference::parse("pkg:cargo/app@1.0.0").expect("source");
            let target = PackageReference::parse("pkg:cargo/serde@1.0.0").expect("target");
            let edge = PackageDependencyRecord::new(
                source.clone(),
                PackageDependencyTarget::new(
                    RegistryEcosystem::Cargo,
                    "serde",
                    "^1",
                    None,
                )
                .expect("target facts"),
                DependencyScope::Runtime,
                false,
                DependencyEvidence {
                    authority: DependencyAuthority::RegistryMetadata,
                    frontier: [1; 32],
                    provenance: [2; 32],
                },
            );
            let facts = vec![(
                source.clone(),
                DependencyFacts::Known(vec![edge.clone()].into_boxed_slice()),
            )];
            let mut projection = TursoProjection::open(&path).await.expect("open");
            assert_eq!(
                projection
                    .synchronize_package_graph(view.root(), &facts)
                    .await
                    .expect("project graph"),
                ProjectionUpdate::Rebuilt { rows: 1 }
            );
            assert_eq!(
                projection
                    .synchronize_package_graph(view.root(), &facts)
                    .await
                    .expect("reuse graph"),
                ProjectionUpdate::Reused { rows: 1 }
            );
            let forward = projection.package_dependencies(&source).await.expect("forward");
            assert_eq!(forward.edges.as_ref(), &[edge]);
            let reverse = projection.package_dependents(&target).await.expect("reverse");
            assert_eq!(reverse.edges.len(), 1);
            assert_eq!(reverse.edges[0].source, source);
            let unknown = vec![(
                target.clone(),
                DependencyFacts::Unavailable(
                    ProductText::new("metadata timeout").expect("reason"),
                ),
            )];
            let next = view_state_root(&[("graph".to_owned(), "next".to_owned())]);
            projection
                .synchronize_package_graph(next, &unknown)
                .await
                .expect("project unavailable");
            let state = projection
                .package_dependencies(&target)
                .await
                .expect("unavailable state")
                .state
                .expect("state row");
            assert_eq!(state.kind, 2);
            std::fs::remove_file(&path).expect("remove projection");
        });
    }
}

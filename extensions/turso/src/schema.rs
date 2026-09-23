//! SQL schema and stable statement text for the projection.

pub(crate) const SCHEMA_VERSION: i64 = 2;
pub(crate) const MAX_AUDIT_ROOTS: i64 = 128;
/// Rows per multi-row rebuild statement. Each statement becomes one immutable
/// FTS segment, so batching bounds both statement size and segment count.
pub(crate) const REBUILD_BATCH_ROWS: usize = 512;

pub(crate) const SCHEMA: &str = r"
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
CREATE INDEX IF NOT EXISTS backend_projection_rows_fts
    ON backend_projection_rows USING fts (label, signature, document);
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

pub(crate) const UPSERT_ROW: &str = r"
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

pub(crate) const UPSERT_COLUMNS: &str = "INSERT INTO backend_projection_rows (\
    row_id, row_kind, content_hash, state, label, score, \
    package_id, parent_id, signature, document) VALUES ";
pub(crate) const UPSERT_CONFLICT: &str = " ON CONFLICT(row_id) DO UPDATE SET \
    row_kind=excluded.row_kind, content_hash=excluded.content_hash, \
    state=excluded.state, label=excluded.label, score=excluded.score, \
    package_id=excluded.package_id, parent_id=excluded.parent_id, \
    signature=excluded.signature, document=excluded.document \
    WHERE backend_projection_rows.content_hash != excluded.content_hash";

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Metadata {
    pub(crate) schema_version: i64,
    pub(crate) root: Vec<u8>,
    pub(crate) view_version: Vec<u8>,
    pub(crate) row_count: i64,
}

pub(crate) struct PackageGraphMetadata {
    pub(crate) root: Vec<u8>,
    pub(crate) edge_count: i64,
}

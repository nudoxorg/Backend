//! SQL capture for the index route.
//!
//! Mirrors the native Stylo lane: one same-scan capture per project, bound to
//! the scan's revision fence, produces a private proof that the intent builder
//! attaches to every shape of intent. Nothing here publishes; the owner grant
//! barrier recomputes the whole delta from the live source before anything
//! becomes visible.
use super::{
    BuiltinModelError,
    ingest::{self, SqlCaptureFailure},
    sql_admission::SqlLiveProof,
    sql_capture::{PostgresWorkerPermit, SqlProjectProjection, SqliteWorkerPermit, project_sql_project},
};
use backend_engine::{
    ProductSourceRecord, WorkspaceSnapshot,
    builtin::{SqlUnavailable, sql_analysis_relation, sql_selection_key},
};
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
};

/// Owner-admitted native SQL workers. Either may be absent: a host whose
/// dialect has no admitted worker is recorded as unavailable, never as empty.
#[derive(Clone, Default)]
pub(super) struct SqlWorkers {
    pub(super) sqlite: Option<Arc<SqliteWorkerPermit>>,
    pub(super) postgres: Option<Arc<PostgresWorkerPermit>>,
}

/// Whether the base snapshot already holds a selection for this project.
pub(super) fn sql_prior(snapshot: &WorkspaceSnapshot, project: [u8; 32]) -> Result<bool, BuiltinModelError> {
    let Some(relation) = sql_analysis_relation(snapshot)
        .map_err(|e| BuiltinModelError(format!("open selected SQL analysis: {e}")))?
    else {
        return Ok(false);
    };
    Ok(relation
        .lookup(&sql_selection_key(project))
        .map_err(|e| BuiltinModelError(format!("read selected SQL selection: {e}")))?
        .is_some())
}

/// Capture and project the project's SQL, or `None` when the project has no
/// SQL and never had any (so unrelated projects gain no SQL rows).
pub(super) fn sql_projection(
    walk: &ingest::SqlWalkEvidence,
    fence: &ingest::CompilerRevisionFence,
    source_root: &Path,
    prior: bool,
    workers: &SqlWorkers,
    cancellation: &Arc<AtomicBool>,
) -> Result<Option<SqlProjectProjection>, BuiltinModelError> {
    trace(format_args!(
        "scan saw {} sql file(s), inventory complete {}, prior selection {}, sqlite worker {}, postgres worker {}",
        walk.file_count(), walk.inventory_complete(), prior, workers.sqlite.is_some(), workers.postgres.is_some()
    ));
    if walk.file_count() == 0 && walk.inventory_complete() && !prior {
        return Ok(None);
    }
    let capture = match ingest::capture_sql_project(walk, fence, source_root, cancellation) {
        Ok(capture) => Arc::new(capture),
        Err(SqlCaptureFailure::Cancelled) => {
            return Err(BuiltinModelError(ingest::INDEX_SCAN_CANCELLED.to_owned()));
        }
        Err(error) => {
            return Err(BuiltinModelError(format!("SQL source capture: {error:?}")));
        }
    };
    project_sql_project(
        capture,
        walk.clone(),
        fence.clone(),
        source_root.to_path_buf(),
        workers.sqlite.as_deref(),
        workers.postgres.as_deref(),
        cancellation,
    )
    .map(Some)
    .map_err(|reason| match reason {
        SqlUnavailable::Cancelled => BuiltinModelError(ingest::INDEX_SCAN_CANCELLED.to_owned()),
        other => BuiltinModelError(format!("SQL projection: {other:?}")),
    })
}

/// Bind a projection to the exact Project after-image it will be published with.
pub(super) fn sql_proof(
    projection: SqlProjectProjection,
    project: &ProductSourceRecord,
    cancellation: &Arc<AtomicBool>,
) -> Result<Arc<SqlLiveProof>, BuiltinModelError> {
    SqlLiveProof::new(Arc::new(projection), project, Arc::clone(cancellation))
        .map(Arc::new)
        .map_err(|e| BuiltinModelError(format!("SQL proof: {e}")))
}

/// Diagnostic line for the SQL lane, shown only when `NUDOX_TRACE` is set.
pub(super) fn trace(args: std::fmt::Arguments<'_>) {
    if std::env::var_os("NUDOX_TRACE").is_some() {
        eprintln!("backend-local-service: sql: {args}");
    }
}

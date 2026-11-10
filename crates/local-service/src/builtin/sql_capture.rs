//! Supervised SQL host projection. The caller must separately admit the
//! project walk, immutable configuration, artifact permit and owner transition.
use super::ingest::SqlCapturedHost;
use backend_compile::{CancellationObserver, ProcessEnvironment, ProcessLimits, ToolchainArtifact};
use backend_engine::builtin::{
    SqlAnalysisRecord, SqlHostTerminal, SqlUnavailable, sql_fact_key, sql_host_fact_digest,
    sql_host_key,
};
use backend_semantic::sql_analysis::{
    SqlByteRange, SqlCoverage, SqlDiagnostic, SqlDialect, SqlFact, SqliteColumnOrigin,
    SqliteLineageCoverage, SqliteRelationKind,
};
use backend_sql_contract::{
    Binding, CheckedLaunch, Dialect, Engine, Request, SemanticContext, Settings, Syntax,
};
use backend_version::Relation;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Private capability constructed only by an owner that already admitted the
/// exact worker artifact and native support fingerprint. Merely hashing an
/// arbitrary path does not establish this capability.
pub(super) struct SqliteWorkerPermit {
    program: PathBuf,
    artifact: ToolchainArtifact,
    basis: backend_sql_contract::ProducerBasis,
}
impl SqliteWorkerPermit {
    pub(super) fn from_admitted(
        program: PathBuf,
        artifact: ToolchainArtifact,
        basis: backend_sql_contract::ProducerBasis,
    ) -> Result<Self, &'static str> {
        if !program.is_absolute()
            || !artifact.dependencies().is_empty()
            || basis.engine != Engine::Syntaqlite4e4de76
            || basis.support_sha256.is_none_or(|v| v == [0; 32])
        {
            return Err("SQL worker artifact not admitted");
        }
        Ok(Self {
            program,
            artifact,
            basis,
        })
    }
}
/// Projection still requires exact-root owner admission and source revalidation.
pub(super) struct SqlHostProjection {
    pub(super) host_key: [u8; 32],
    pub(super) host: SqlAnalysisRecord,
    pub(super) facts: Vec<([u8; 32], SqlAnalysisRecord)>,
}
#[derive(Serialize)]
struct ProposedSqliteContext<'a> {
    epoch: u64,
    tables: &'a [backend_library::sql_workspace::SqliteProjectTable],
}
fn digest(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(&(bytes.len() as u64).to_be_bytes());
    h.update(bytes);
    *h.finalize().as_bytes()
}
/// Identity of the exact native-to-canonical projection and its vocabulary.
/// The worker artifact and native support fingerprint are bound separately.
fn projection_support() -> [u8; 32] {
    let sources: &[&[u8]] = &[
        include_bytes!("sql_capture.rs"),
        include_bytes!("../../../semantic/src/sql_analysis.rs"),
        include_bytes!("../../../engine/src/builtin/sql_relation.rs"),
        include_bytes!("../../../library/sql_workspace.rs"),
        include_bytes!("../../../../frontends/sql-contract/src/lib.rs"),
    ];
    projection_support_for(sources)
}
fn projection_support_for(sources: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"nudox.sql.owner-projection-support.v1\0");
    for source in sources {
        hasher.update(&(source.len() as u64).to_be_bytes());
        hasher.update(source);
    }
    *hasher.finalize().as_bytes()
}
/// Run one selected SQLite host with an already-admitted artifact. No source
/// occurrence is inferred from native relation or lineage display names.
pub(super) fn capture_sqlite_host(
    project: [u8; 32],
    root: &Path,
    host: &SqlCapturedHost,
    permit: &SqliteWorkerPermit,
    cancellation: &dyn CancellationObserver,
) -> Result<SqlHostProjection, SqlUnavailable> {
    use backend_library::sql_workspace::{SqlDialect as SelectedDialect, SqlProjectContext};
    if cancellation.is_cancelled() {
        return Err(SqlUnavailable::Cancelled);
    }
    if host.selected().dialect != SelectedDialect::Sqlite {
        return Err(SqlUnavailable::UnsupportedDialect);
    }
    let host_key = sql_host_key(project, &host.selected().path)
        .map_err(|_| SqlUnavailable::SelectionUnavailable)?;
    let (context, context_id) = match &host.selected().context {
        SqlProjectContext::Unavailable => (None, None),
        SqlProjectContext::SqliteProposed { epoch, tables } => {
            let payload = serde_json::to_vec(&ProposedSqliteContext {
                epoch: *epoch,
                tables,
            })
            .map_err(|_| SqlUnavailable::Context)?;
            if payload.len() > backend_sql_contract::MAX_CONTEXT_BYTES {
                return Err(SqlUnavailable::Limit);
            }
            let identity: [u8; 32] = Sha256::digest(&payload).into();
            (
                Some(SemanticContext::Proposed {
                    dialect: Dialect::Sqlite,
                    payload,
                }),
                Some(identity),
            )
        }
    };
    let request = Request {
        version: backend_sql_contract::PROTOCOL_VERSION,
        expected_basis: Some(permit.basis.clone()),
        source: host.bytes().to_vec(),
        settings: Settings::ProcessDefaults,
        context,
    };
    let wall = Duration::from_secs(10);
    let limits = ProcessLimits::new(8 * 1024 * 1024, 128 * 1024, wall, 9 * 1024 * 1024)
        .and_then(|value| value.with_input_bytes_limit(4 * 1024 * 1024))
        .map_err(|_| SqlUnavailable::Limit)?;
    let launch = CheckedLaunch {
        program: &permit.program,
        workspace: root,
        artifact: permit.artifact.clone(),
        environment: ProcessEnvironment::new(vec![]).map_err(|_| SqlUnavailable::WorkerFailure)?,
        limits,
        cancellation,
    };
    let response = backend_sql_contract::analyze_checked(
        &request,
        host.bytes(),
        host.input(),
        host.source(),
        launch,
        Dialect::Sqlite,
        Engine::Syntaqlite4e4de76,
    )
    .map_err(|error| match error {
        backend_sql_contract::CheckedError::Cancelled => SqlUnavailable::Cancelled,
        backend_sql_contract::CheckedError::InputLimit => SqlUnavailable::Limit,
        backend_sql_contract::CheckedError::UnsupportedSettings => SqlUnavailable::Context,
        backend_sql_contract::CheckedError::Process(backend_compile::ProcessError::Deadline)
        | backend_sql_contract::CheckedError::Terminal(
            backend_compile::ProcessTerminal::Deadline,
        ) => SqlUnavailable::Deadline,
        backend_sql_contract::CheckedError::Process(backend_compile::ProcessError::OutputLimit)
        | backend_sql_contract::CheckedError::Terminal(
            backend_compile::ProcessTerminal::OutputLimit,
        ) => SqlUnavailable::Limit,
        backend_sql_contract::CheckedError::Terminal(
            backend_compile::ProcessTerminal::Cancelled,
        ) => SqlUnavailable::Cancelled,
        _ => SqlUnavailable::WorkerFailure,
    })?;
    let observed = response.response();
    let mut facts = Vec::new();
    let terminal = match &observed.syntax {
        Syntax::Parsed { .. } => {
            let (coverage, lineage) = match &observed.binding {
                Binding::ProposedAnalysis {
                    diagnostics,
                    relations,
                    lineage,
                    native_lineage,
                    ..
                } => {
                    if diagnostics
                        .len()
                        .saturating_add(relations.len())
                        .saturating_add(lineage.len())
                        > 4096
                    {
                        return Err(SqlUnavailable::Limit);
                    }
                    for relation in relations {
                        facts.push(SqlFact::SqliteRelation {
                            name: relation.name.clone(),
                            relation_kind: match relation.kind {
                                backend_sql_contract::SqliteRelationKind::Table => {
                                    SqliteRelationKind::Table
                                }
                                backend_sql_contract::SqliteRelationKind::View => {
                                    SqliteRelationKind::View
                                }
                            },
                        });
                    }
                    for column in lineage {
                        facts.push(SqlFact::SqliteOutputLineage {
                            output_name: column.output_name.clone(),
                            ordinal: column.ordinal,
                            origin: column.origin.as_ref().map(|origin| SqliteColumnOrigin {
                                relation: origin.relation.clone(),
                                column: origin.column.clone(),
                            }),
                        });
                    }
                    for diagnostic in diagnostics {
                        facts.push(SqlFact::Diagnostic {
                            code: match diagnostic.kind {
                                backend_sql_contract::DiagnosticKind::UnknownColumn => {
                                    SqlDiagnostic::UnknownColumn
                                }
                                backend_sql_contract::DiagnosticKind::Other => SqlDiagnostic::Other,
                            },
                            extent: diagnostic.extent.as_ref().map(|extent| SqlByteRange {
                                start: extent.start as u32,
                                end: extent.end as u32,
                            }),
                        });
                    }
                    let state = match native_lineage {
                        backend_sql_contract::SqliteLineageCoverage::Unavailable => {
                            SqliteLineageCoverage::Unavailable
                        }
                        backend_sql_contract::SqliteLineageCoverage::NativeComplete => {
                            SqliteLineageCoverage::NativeComplete
                        }
                        backend_sql_contract::SqliteLineageCoverage::NativePartial => {
                            SqliteLineageCoverage::NativePartial
                        }
                    };
                    (SqlCoverage::Partial, Some(state))
                }
                Binding::MissingContext
                | Binding::UnsupportedContext
                | Binding::ContextUnavailable
                | Binding::NotAttempted => (SqlCoverage::Unavailable, None),
                _ => return Err(SqlUnavailable::WorkerFailure),
            };
            SqlHostTerminal::Parsed {
                source: SqlCoverage::Unavailable,
                contextual: coverage,
                sqlite_lineage: lineage,
            }
        }
        Syntax::NativeError => SqlHostTerminal::NativeError,
        Syntax::ResourceLimit => return Err(SqlUnavailable::Limit),
        Syntax::TimedOut => return Err(SqlUnavailable::Deadline),
        _ => return Err(SqlUnavailable::WorkerFailure),
    };
    let basis_bytes =
        serde_json::to_vec(&permit.basis).map_err(|_| SqlUnavailable::WorkerFailure)?;
    let mut producer_bytes = permit.artifact.identity().to_bytes().to_vec();
    producer_bytes.extend_from_slice(&basis_bytes);
    producer_bytes.extend_from_slice(&projection_support());
    let producer = digest(b"nudox.sql.sqlite.producer.v1\0", &producer_bytes);
    let mut settings_bytes = basis_bytes;
    settings_bytes.extend_from_slice(&10u64.to_be_bytes());
    settings_bytes.extend_from_slice(&(8u64 * 1024 * 1024).to_be_bytes());
    settings_bytes.extend_from_slice(&(128u64 * 1024).to_be_bytes());
    settings_bytes.extend_from_slice(&(9u64 * 1024 * 1024).to_be_bytes());
    settings_bytes.extend_from_slice(&(4u64 * 1024 * 1024).to_be_bytes());
    // ProcessSupervisor does not promise a hard memory bound on every host.
    settings_bytes.extend_from_slice(b"memory-limit-unavailable\0");
    let settings = digest(b"nudox.sql.sqlite.settings.v1\0", &settings_bytes);
    let mut rows = Vec::with_capacity(facts.len());
    for (i, fact) in facts.into_iter().enumerate() {
        let ordinal = u32::try_from(i).map_err(|_| SqlUnavailable::Limit)?;
        let key = sql_fact_key(
            host_key,
            SqlDialect::Sqlite,
            host.input().to_bytes(),
            *host.source().as_ref(),
            producer,
            settings,
            context_id,
            ordinal,
        );
        let row = SqlAnalysisRecord::Fact {
            host: host_key,
            ordinal,
            fact,
        };
        row.validate().map_err(|_| SqlUnavailable::Limit)?;
        rows.push((key, row));
    }
    let fact_count = u32::try_from(rows.len()).map_err(|_| SqlUnavailable::Limit)?;
    let fact_digest = sql_host_fact_digest(
        &rows
            .iter()
            .map(|(key, row)| (*key, row))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| SqlUnavailable::Limit)?;
    let row = SqlAnalysisRecord::Host {
        project,
        path: host.selected().path.clone(),
        dialect: SqlDialect::Sqlite,
        input: Some(host.input().to_bytes()),
        source: Some(*host.source().as_ref()),
        length: Some(host.bytes().len() as u64),
        producer: Some(producer),
        settings: Some(settings),
        context: context_id,
        terminal,
        fact_count,
        fact_digest,
    };
    row.validate().map_err(|_| SqlUnavailable::WorkerFailure)?;
    Ok(SqlHostProjection {
        host_key,
        host: row,
        facts: rows,
    })
}

/// Same-scan SQL rows and their private source witness. These rows remain
/// claims until exact selected predecessor/target and closure admission.
pub(super) struct SqlProjectProjection {
    capture: std::sync::Arc<super::ingest::SqlProjectCapture>,
    walk: super::ingest::SqlWalkEvidence,
    fence: super::ingest::CompilerRevisionFence,
    requested_root: PathBuf,
    rows: Vec<([u8; 32], SqlAnalysisRecord)>,
}

/// Charge the encoded row, key and framing before retaining it in the project.
struct ProjectRowBudget {
    bytes: usize,
    max_bytes: usize,
    max_rows: usize,
}
fn exact_root_match(requested: &Path, selected: &Path) -> bool {
    requested.as_os_str() == selected.as_os_str()
}
impl ProjectRowBudget {
    fn push(
        &mut self,
        rows: &mut Vec<([u8; 32], SqlAnalysisRecord)>,
        key: [u8; 32],
        row: SqlAnalysisRecord,
    ) -> Result<(), SqlUnavailable> {
        row.validate().map_err(|_| SqlUnavailable::WorkerFailure)?;
        let mut encoded = Vec::new();
        backend_engine::builtin::SqlAnalysisRelation::encode_value(&row, &mut encoded);
        let next = self
            .bytes
            .checked_add(encoded.len())
            .and_then(|n| n.checked_add(40))
            .ok_or(SqlUnavailable::Limit)?;
        if rows.len() >= self.max_rows || next > self.max_bytes {
            return Err(SqlUnavailable::Limit);
        }
        rows.try_reserve(1).map_err(|_| SqlUnavailable::Limit)?;
        self.bytes = next;
        rows.push((key, row));
        Ok(())
    }
}
impl SqlProjectProjection {
    pub(super) fn rows(&self) -> &[([u8; 32], SqlAnalysisRecord)] {
        &self.rows
    }
    pub(super) fn project(&self) -> [u8; 32] {
        self.capture.project()
    }
    /// Reread both host identity domains and actual configuration bytes.
    pub(super) fn revalidate(
        &self,
        cancellation: &std::sync::atomic::AtomicBool,
    ) -> Result<(), super::ingest::SqlCaptureFailure> {
        self.capture
            .revalidate(&self.walk, &self.fence, &self.requested_root, cancellation)
    }
}
/// Build an explicit selected SQL project projection from a confined capture.
/// Absence of a permit produces per-host unavailable terminals, not empty facts.
pub(super) fn project_sql_project(
    capture: std::sync::Arc<super::ingest::SqlProjectCapture>,
    walk: super::ingest::SqlWalkEvidence,
    fence: super::ingest::CompilerRevisionFence,
    requested_root: PathBuf,
    permit: Option<&SqliteWorkerPermit>,
    cancellation: &std::sync::atomic::AtomicBool,
) -> Result<SqlProjectProjection, SqlUnavailable> {
    use backend_engine::builtin::{
        SqlAttemptOutcome, SqlInventory, admit_sql_relation, sql_attempt_key, sql_host_fact_digest,
        sql_inventory_digest, sql_selection_digest, sql_selection_key,
    };
    capture
        .revalidate(&walk, &fence, &requested_root, cancellation)
        .map_err(|error| match error {
            super::ingest::SqlCaptureFailure::Cancelled => SqlUnavailable::Cancelled,
            super::ingest::SqlCaptureFailure::Limit => SqlUnavailable::Limit,
            _ => SqlUnavailable::SelectionUnavailable,
        })?;
    let project = capture.project();
    // The stored root must equal the selected project label exactly so cold
    // replay never relies on the later state of filesystem symlinks.
    if !exact_root_match(&requested_root, walk.root()) {
        return Err(SqlUnavailable::SelectionUnavailable);
    }
    let root = walk
        .root()
        .to_str()
        .ok_or(SqlUnavailable::SelectionUnavailable)?
        .to_owned();
    let mut inventory = Vec::with_capacity(capture.hosts().len());
    let mut rows = Vec::new();
    let mut budget = ProjectRowBudget {
        bytes: 0,
        max_bytes: 64 * 1024 * 1024,
        max_rows: 262_144,
    };
    let empty_fact_digest = sql_host_fact_digest(&[]).map_err(|_| SqlUnavailable::WorkerFailure)?;
    for host in capture.hosts() {
        if cancellation.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SqlUnavailable::Cancelled);
        }
        let dialect = match host.selected().dialect {
            backend_library::sql_workspace::SqlDialect::Sqlite => SqlDialect::Sqlite,
            backend_library::sql_workspace::SqlDialect::Postgres => SqlDialect::Postgres,
        };
        inventory.push((host.selected().path.clone(), dialect));
        let result = match (dialect, permit) {
            (SqlDialect::Sqlite, Some(permit)) => {
                capture_sqlite_host(project, walk.root(), host, permit, cancellation)
            }
            (SqlDialect::Sqlite, None) => Err(SqlUnavailable::EngineNotAdmitted),
            (SqlDialect::Postgres, _) => Err(SqlUnavailable::UnsupportedDialect),
        };
        match result {
            Ok(projected) => {
                budget.push(&mut rows, projected.host_key, projected.host)?;
                for (key, fact) in projected.facts {
                    budget.push(&mut rows, key, fact)?;
                }
            }
            Err(SqlUnavailable::Cancelled) => return Err(SqlUnavailable::Cancelled),
            Err(reason) => {
                let host_key = sql_host_key(project, &host.selected().path)
                    .map_err(|_| SqlUnavailable::SelectionUnavailable)?;
                budget.push(
                    &mut rows,
                    host_key,
                    SqlAnalysisRecord::Host {
                        project,
                        path: host.selected().path.clone(),
                        dialect,
                        input: Some(host.input().to_bytes()),
                        source: Some(*host.source().as_ref()),
                        length: Some(host.bytes().len() as u64),
                        producer: None,
                        settings: None,
                        context: None,
                        terminal: SqlHostTerminal::Unavailable(reason),
                        fact_count: 0,
                        fact_digest: empty_fact_digest,
                    },
                )?;
            }
        }
    }
    let settings = capture.settings();
    let selection = SqlAnalysisRecord::Selection {
        project,
        source_root: root,
        config_input: settings.input().to_bytes(),
        config_source: *settings.source().as_ref(),
        config_length: u32::try_from(capture.settings_len()).map_err(|_| SqlUnavailable::Limit)?,
        discovery: None,
        policy_complete: false,
        inventory: SqlInventory::Incomplete,
        inventory_digest: sql_inventory_digest(&inventory)
            .map_err(|_| SqlUnavailable::SelectionUnavailable)?,
        host_count: u32::try_from(inventory.len()).map_err(|_| SqlUnavailable::Limit)?,
    };
    let attempt = SqlAnalysisRecord::Attempt {
        project,
        outcome: SqlAttemptOutcome::Observed,
        served_selection: Some(
            sql_selection_digest(project, &selection).map_err(|_| SqlUnavailable::WorkerFailure)?,
        ),
    };
    budget.push(&mut rows, sql_selection_key(project), selection)?;
    budget.push(&mut rows, sql_attempt_key(project), attempt)?;
    rows.sort_by_key(|entry| entry.0);
    admit_sql_relation(
        &rows
            .iter()
            .map(|(key, row)| (*key, row))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| SqlUnavailable::WorkerFailure)?;
    Ok(SqlProjectProjection {
        capture,
        walk,
        fence,
        requested_root,
        rows,
    })
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn projection_fingerprint_binds_exact_ordered_support_sources() {
        let a = projection_support_for(&[b"adapter", b"schema"]);
        assert_eq!(a, projection_support_for(&[b"adapter", b"schema"]));
        assert_ne!(a, projection_support_for(&[b"adapter!", b"schema"]));
        assert_ne!(a, projection_support_for(&[b"schema", b"adapter"]));
    }
    #[test]
    fn project_label_must_equal_canonical_root_bytes() {
        assert!(exact_root_match(
            Path::new("/project"),
            Path::new("/project")
        ));
        assert!(!exact_root_match(
            Path::new("/project/."),
            Path::new("/project")
        ));
        assert!(!exact_root_match(Path::new("/link"), Path::new("/project")));
    }
    #[test]
    fn project_budget_refuses_before_retaining_an_overflowing_row() {
        use backend_engine::builtin::SqlAttemptOutcome;
        let row = SqlAnalysisRecord::Attempt {
            project: [1; 32],
            outcome: SqlAttemptOutcome::Unavailable(SqlUnavailable::EngineNotAdmitted),
            served_selection: None,
        };
        let mut encoded = Vec::new();
        backend_engine::builtin::SqlAnalysisRelation::encode_value(&row, &mut encoded);
        let single = encoded.len() + 40;
        let mut budget = ProjectRowBudget {
            bytes: 0,
            max_bytes: single * 2 - 1,
            max_rows: 4,
        };
        let mut rows = Vec::new();
        budget.push(&mut rows, [2; 32], row.clone()).unwrap();
        assert!(matches!(
            budget.push(&mut rows, [3; 32], row),
            Err(SqlUnavailable::Limit)
        ));
        assert_eq!(rows.len(), 1);
        assert_eq!(budget.bytes, single);
    }
}

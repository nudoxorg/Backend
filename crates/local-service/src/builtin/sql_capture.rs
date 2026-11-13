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

const DEFAULT_WORKER_WALL: Duration = Duration::from_secs(10);
/// Private capability constructed only by an owner that already admitted the
/// exact worker artifact and native support fingerprint. Merely hashing an
/// arbitrary path does not establish this capability.
pub(super) struct SqliteWorkerPermit {
    program: PathBuf,
    artifact: ToolchainArtifact,
    basis: backend_sql_contract::ProducerBasis,
    /// Wall-clock budget of one host launch; part of the recorded settings.
    wall: Duration,
    #[cfg(target_os = "linux")]
    library: Option<(PathBuf, backend_compile::ExecutableIdentity)>,
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
            wall: DEFAULT_WORKER_WALL,
            #[cfg(target_os = "linux")]
            library: None,
        })
    }
    /// A source-bound installed worker with one exact private runtime dependency.
    #[cfg(target_os = "linux")]
    pub(super) fn from_admitted_with_linux_library(
        program: PathBuf,
        artifact: ToolchainArtifact,
        basis: backend_sql_contract::ProducerBasis,
        library: PathBuf,
        identity: backend_compile::ExecutableIdentity,
    ) -> Result<Self, &'static str> {
        if !program.is_absolute()
            || !library.is_absolute()
            || library.file_name().and_then(|name| name.to_str()) != Some("libgcc_s.so.1")
            || artifact.dependencies() != [identity.digest()]
            || basis.engine != Engine::Syntaqlite4e4de76
            || basis.support_sha256.is_none_or(|value| value == [0; 32])
        {
            return Err("SQLite private runtime closure not admitted");
        }
        Ok(Self {
            program,
            artifact,
            basis,
            wall: DEFAULT_WORKER_WALL,
            library: Some((library, identity)),
        })
    }

    fn analyze<'a>(
        &self,
        request: &Request,
        host: &'a [u8],
        input: backend_compile::InputContentVersion,
        source: backend_version::ContentId<backend_version::SourceFactDomain>,
        mut launch: CheckedLaunch<'_>,
    ) -> Result<backend_sql_contract::CheckedResponse<'a>, backend_sql_contract::CheckedError> {
        #[cfg(target_os = "linux")]
        if let Some((path, identity)) = &self.library {
            let deadline = std::time::Instant::now()
                .checked_add(launch.limits.wall_time())
                .ok_or(backend_sql_contract::CheckedError::Process(
                    backend_compile::ProcessError::Deadline,
                ))?;
            let library = backend_compile::NativeLibraryLease::prepare(
                path,
                identity,
                &self.artifact,
                launch.cancellation,
                deadline,
            )
            .map_err(|error| match error {
                backend_compile::ProcessError::Cancelled => backend_sql_contract::CheckedError::Cancelled,
                error => backend_sql_contract::CheckedError::Process(error),
            })?;
            launch.environment = library
                .environment()
                .map_err(backend_sql_contract::CheckedError::Process)?;
            return backend_sql_contract::analyze_checked_with_linux_library_until(
                request,
                host,
                input,
                source,
                launch,
                Dialect::Sqlite,
                Engine::Syntaqlite4e4de76,
                deadline,
                library,
            );
        }
        backend_sql_contract::analyze_checked(
            request,
            host,
            input,
            source,
            launch,
            Dialect::Sqlite,
            Engine::Syntaqlite4e4de76,
        )
    }

    /// A different per-host wall budget, for hosts where process start alone
    /// can exceed the default (a saturated build machine). It is recorded in the
    /// analysis settings, so rows made under another budget are distinct.
    #[cfg(test)]
    pub(super) fn with_wall(mut self, wall: Duration) -> Self {
        self.wall = wall;
        self
    }
}
/// Private PostgreSQL capability. Construction still requires a separately
/// admitted artifact and a nonzero exact support fingerprint; the historical
/// experimental facade does not yet provide that fingerprint.
pub(super) struct PostgresWorkerPermit {
    program: PathBuf,
    artifact: ToolchainArtifact,
    basis: backend_sql_contract::ProducerBasis,
    /// Wall-clock budget of one host launch; part of the recorded settings.
    wall: Duration,
}
impl PostgresWorkerPermit {
    pub(super) fn from_admitted(
        program: PathBuf,
        artifact: ToolchainArtifact,
        basis: backend_sql_contract::ProducerBasis,
    ) -> Result<Self, &'static str> {
        if !program.is_absolute()
            || !artifact.dependencies().is_empty()
            || basis.engine != Engine::Pgrust38634b4
            || basis.support_sha256.is_none_or(|value| value == [0; 32])
        {
            return Err("PostgreSQL worker artifact or support not admitted");
        }
        Ok(Self {
            program,
            artifact,
            basis,
            wall: DEFAULT_WORKER_WALL,
        })
    }
    /// A different per-host wall budget, for hosts where process start alone
    /// can exceed the default (a saturated build machine). It is recorded in the
    /// analysis settings, so rows made under another budget are distinct.
    #[cfg(test)]
    pub(super) fn with_wall(mut self, wall: Duration) -> Self {
        self.wall = wall;
        self
    }
}
/// One host's checked worker result before project-wide binding. The syntactic
/// outline is a claim about this file alone; rows exist only after `finish_host`.
pub(super) struct SqlHostDraft {
    host_key: [u8; 32],
    path: String,
    dialect: SqlDialect,
    input: [u8; 32],
    source: [u8; 32],
    length: u64,
    producer: [u8; 32],
    settings: [u8; 32],
    context_id: Option<[u8; 32]>,
    terminal: SqlHostTerminal,
    legacy_facts: Vec<SqlFact>,
    outline: Option<backend_sql_contract::Outline>,
    /// Digest of the exact request, checked response, command and artifact.
    evidence: [u8; 32],
}
impl SqlHostDraft {
    pub(super) fn outline(&self) -> Option<&backend_sql_contract::Outline> {
        self.outline.as_ref()
    }
    pub(super) fn path(&self) -> &str {
        &self.path
    }
    pub(super) fn dialect(&self) -> SqlDialect {
        self.dialect
    }
}
enum HostOutcome {
    Draft(SqlHostDraft),
    Unavailable([u8; 32], SqlAnalysisRecord),
}
/// Projection still requires exact-root owner admission and source revalidation.
pub(super) struct SqlHostProjection {
    pub(super) host_key: [u8; 32],
    pub(super) host: SqlAnalysisRecord,
    pub(super) facts: Vec<([u8; 32], SqlAnalysisRecord)>,
    checked_response: [u8; 32],
}
#[derive(Serialize)]
struct ProposedSqliteContext<'a> {
    epoch: u64,
    tables: &'a [backend_library::sql_workspace::SqliteProjectTable],
}
#[derive(Serialize)]
struct ProposedPostgresContext<'a> {
    epoch: u64,
    search_path: &'a [String],
    schemas: &'a [backend_library::sql_workspace::PostgresProjectSchema],
}
fn digest(domain: &[u8], bytes: &[u8]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(domain);
    h.update(&(bytes.len() as u64).to_be_bytes());
    h.update(bytes);
    *h.finalize().as_bytes()
}
/// Identity of the native-to-canonical projection, vocabulary, and owner
/// workspace dependency lock. The full lock can conservatively invalidate for
/// unrelated dependency upgrades. This is not owner executable/toolchain
/// provenance; the worker artifact and native fingerprint are bound separately.
fn projection_support() -> [u8; 32] {
    let sources: &[&[u8]] = &[
        include_bytes!("sql_capture.rs"),
        include_bytes!("ingest.rs"),
        include_bytes!("profile.rs"),
        include_bytes!("../builtin.rs"),
        include_bytes!("commands/adapter.rs"),
        include_bytes!("commands/index_publication_worker.rs"),
        include_bytes!("sql_admission.rs"),
        include_bytes!("sql_receipt.rs"),
        include_bytes!("sql_receipt_index.rs"),
        include_bytes!("staged_intent.rs"),
        include_bytes!("staged_transport.rs"),
        include_bytes!("owner_receipt_context.rs"),
        include_bytes!("../../../engine/src/builtin/sql_receipt_index.rs"),
        include_bytes!("../../../semantic/src/sql_analysis.rs"),
        include_bytes!("../../../engine/src/builtin/sql_relation.rs"),
        include_bytes!("../../../library/sql_workspace.rs"),
        include_bytes!("../../../../frontends/sql-contract/src/lib.rs"),
        include_bytes!("../../../../Cargo.lock"),
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
) -> Result<SqlHostDraft, SqlUnavailable> {
    let _ = project;
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
        SqlProjectContext::PostgresProposed { .. } => return Err(SqlUnavailable::Context),
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
    let wall = permit.wall;
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
    let response = permit.analyze(
        &request, host.bytes(), host.input(), host.source(), launch,
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
                        if cancellation.is_cancelled() {
                            return Err(SqlUnavailable::Cancelled);
                        }
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
                        if cancellation.is_cancelled() {
                            return Err(SqlUnavailable::Cancelled);
                        }
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
                        if cancellation.is_cancelled() {
                            return Err(SqlUnavailable::Cancelled);
                        }
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
                source: if observed.outline.is_some() { SqlCoverage::Partial } else { SqlCoverage::Unavailable },
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
    settings_bytes.extend_from_slice(&wall.as_secs().to_be_bytes());
    settings_bytes.extend_from_slice(&(8u64 * 1024 * 1024).to_be_bytes());
    settings_bytes.extend_from_slice(&(128u64 * 1024).to_be_bytes());
    settings_bytes.extend_from_slice(&(9u64 * 1024 * 1024).to_be_bytes());
    settings_bytes.extend_from_slice(&(4u64 * 1024 * 1024).to_be_bytes());
    // ProcessSupervisor does not promise a hard memory bound on every host.
    settings_bytes.extend_from_slice(b"memory-limit-unavailable\0");
    let settings = digest(b"nudox.sql.sqlite.settings.v1\0", &settings_bytes);
    let evidence = response_evidence(&request, &response, &permit.artifact, cancellation)?;
    Ok(SqlHostDraft {
        host_key,
        path: host.selected().path.clone(),
        dialect: SqlDialect::Sqlite,
        input: host.input().to_bytes(),
        source: *host.source().as_ref(),
        length: host.bytes().len() as u64,
        producer,
        settings,
        context_id,
        terminal,
        legacy_facts: facts,
        outline: observed.outline.clone(),
        evidence,
    })
}

/// Bind a draft's rows. `outline_facts` must be the bound outline facts for this
/// host (ordinals from zero); legacy native facts follow them.
pub(super) fn finish_host(
    draft: SqlHostDraft,
    outline_facts: Vec<SqlFact>,
    project: [u8; 32],
    cancellation: &dyn CancellationObserver,
) -> Result<SqlHostProjection, SqlUnavailable> {
    let SqlHostDraft {
        host_key, path, dialect, input, source, length, producer, settings, context_id,
        terminal, legacy_facts, evidence, ..
    } = draft;
    let mut all = outline_facts;
    all.extend(legacy_facts);
    let mut rows = Vec::new();
    rows.try_reserve_exact(all.len()).map_err(|_| SqlUnavailable::Limit)?;
    for (i, fact) in all.into_iter().enumerate() {
        if cancellation.is_cancelled() {
            return Err(SqlUnavailable::Cancelled);
        }
        let ordinal = u32::try_from(i).map_err(|_| SqlUnavailable::Limit)?;
        let key = sql_fact_key(host_key, dialect, input, source, producer, settings, context_id, ordinal);
        let row = SqlAnalysisRecord::Fact { host: host_key, ordinal, fact };
        row.validate().map_err(|_| SqlUnavailable::Limit)?;
        rows.push((key, row));
    }
    let fact_count = u32::try_from(rows.len()).map_err(|_| SqlUnavailable::Limit)?;
    let fact_digest = sql_host_fact_digest(
        &rows.iter().map(|(key, row)| (*key, row)).collect::<Vec<_>>(),
    )
    .map_err(|_| SqlUnavailable::Limit)?;
    let row = SqlAnalysisRecord::Host {
        project,
        path,
        dialect,
        input: Some(input),
        source: Some(source),
        length: Some(length),
        producer: Some(producer),
        settings: Some(settings),
        context: context_id,
        terminal,
        fact_count,
        fact_digest,
    };
    row.validate().map_err(|_| SqlUnavailable::WorkerFailure)?;
    // This private commitment is minted only while the checked native
    // response and its exact request, admitted artifact and projected rows
    // coexist. It is not by itself durable authority or a signed receipt.
    let checked_response = host_commitment(evidence, &row, &rows, cancellation)?;
    Ok(SqlHostProjection { host_key, host: row, facts: rows, checked_response })
}

struct HashWriter<'a> { hash: blake3::Hasher, cancellation: &'a dyn CancellationObserver }
impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.cancellation.is_cancelled() { return Err(std::io::Error::other("cancelled")); }
        self.hash.update(bytes); Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
fn json_digest<T: Serialize>(value: &T, cancellation: &dyn CancellationObserver) -> Result<[u8; 32], SqlUnavailable> {
    let mut out = HashWriter { hash: blake3::Hasher::new(), cancellation };
    serde_json::to_writer(&mut out, value).map_err(|_| if cancellation.is_cancelled() { SqlUnavailable::Cancelled } else { SqlUnavailable::WorkerFailure })?;
    if cancellation.is_cancelled() { return Err(SqlUnavailable::Cancelled); }
    Ok(*out.hash.finalize().as_bytes())
}
/// Digest of the exact request, checked response, command identity and artifact.
fn response_evidence(
    request: &Request,
    checked: &backend_sql_contract::CheckedResponse<'_>,
    artifact: &ToolchainArtifact,
    cancellation: &dyn CancellationObserver,
) -> Result<[u8; 32], SqlUnavailable> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.sql.checked-host-response.v1\0");
    hash.update(&json_digest(request, cancellation)?);
    hash.update(&json_digest(checked.response(), cancellation)?);
    hash.update(&checked.command().to_bytes());
    hash.update(&artifact.identity().to_bytes());
    hash.update(&projection_support());
    Ok(*hash.finalize().as_bytes())
}
/// Bind the response evidence to the exact rows derived from it.
fn host_commitment(
    evidence: [u8; 32],
    host: &SqlAnalysisRecord,
    facts: &[([u8; 32], SqlAnalysisRecord)],
    cancellation: &dyn CancellationObserver,
) -> Result<[u8; 32], SqlUnavailable> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.sql.checked-host-rows.v2\0");
    hash.update(&evidence);
    let mut encoded = Vec::new();
    backend_engine::builtin::SqlAnalysisRelation::encode_value(host, &mut encoded);
    hash.update(&(encoded.len() as u64).to_be_bytes());
    hash.update(&encoded);
    hash.update(&(facts.len() as u64).to_be_bytes());
    for (key, fact) in facts {
        if cancellation.is_cancelled() { return Err(SqlUnavailable::Cancelled); }
        encoded.clear();
        backend_engine::builtin::SqlAnalysisRelation::encode_value(fact, &mut encoded);
        hash.update(key);
        hash.update(&(encoded.len() as u64).to_be_bytes());
        hash.update(&encoded);
    }
    if cancellation.is_cancelled() { return Err(SqlUnavailable::Cancelled); }
    Ok(*hash.finalize().as_bytes())
}

/// Same-scan SQL rows and their private source witness. These rows remain
/// claims until exact selected predecessor/target and closure admission.
pub(super) struct SqlProjectProjection {
    capture: super::ingest::SqlProjectWitness,
    walk: super::ingest::SqlWalkEvidence,
    fence: super::ingest::CompilerRevisionFence,
    requested_root: PathBuf,
    rows: Vec<([u8; 32], SqlAnalysisRecord)>,
    checked_responses: Vec<([u8; 32], [u8; 32])>,
}

/// Digest a complete, sorted project row partition (caller proves selection
/// completeness). Attempt rows are excluded so a later unavailable attempt
/// does not invalidate the retained selection's original custody.
pub(super) fn sql_project_rows_digest(
    project: [u8; 32], rows: &[([u8; 32], SqlAnalysisRecord)], cancellation: &dyn CancellationObserver,
) -> Result<[u8; 32], SqlUnavailable> {
    if rows.len() > 262_144 { return Err(SqlUnavailable::Limit); }
    let mut refs = Vec::new();
    refs.try_reserve_exact(rows.len()).map_err(|_| SqlUnavailable::Limit)?;
    refs.extend(rows.iter().map(|(key, row)| (*key, row)));
    sql_project_rows_digest_refs(project, &refs, cancellation)
}

/// As above, over borrowed rows to bound staging memory without cloning facts.
pub(super) fn sql_project_rows_digest_refs(
    project: [u8; 32], rows: &[([u8; 32], &SqlAnalysisRecord)], cancellation: &dyn CancellationObserver,
) -> Result<[u8; 32], SqlUnavailable> {
    if project == [0; 32] || rows.windows(2).any(|pair| pair[0].0 >= pair[1].0) { return Err(SqlUnavailable::SelectionUnavailable); }
    if rows.len() > 262_144 { return Err(SqlUnavailable::Limit); }
    let mut hosts = std::collections::BTreeSet::new();
    let mut selection_count = 0usize;
    let mut declared_hosts = 0u32;
    for (key, row) in rows {
        if cancellation.is_cancelled() { return Err(SqlUnavailable::Cancelled); }
        match *row {
            SqlAnalysisRecord::Selection { project: owner, host_count, .. } if *owner == project => { selection_count += 1; declared_hosts = *host_count; }
            SqlAnalysisRecord::Host { project: owner, .. } if *owner == project => { hosts.insert(*key); if hosts.len() > backend_engine::builtin::MAX_SQL_HOSTS as usize { return Err(SqlUnavailable::Limit); } }
            SqlAnalysisRecord::Attempt { project: owner, .. } if *owner == project => {},
            SqlAnalysisRecord::Fact { .. } => {},
            _ => return Err(SqlUnavailable::SelectionUnavailable),
        }
    }
    if selection_count != 1 || hosts.len() != declared_hosts as usize { return Err(SqlUnavailable::SelectionUnavailable); }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.sql.receipt-project-rows.v1\0");
    hash.update(&project);
    let count = rows.iter().filter(|(_, row)| !matches!(*row, SqlAnalysisRecord::Attempt { .. })).count();
    hash.update(&(count as u64).to_be_bytes());
    let mut encoded = Vec::new();
    let mut work_bytes = 0usize;
    for (key, row) in rows {
        if cancellation.is_cancelled() { return Err(SqlUnavailable::Cancelled); }
        if matches!(*row, SqlAnalysisRecord::Attempt { .. }) { continue; }
        if let SqlAnalysisRecord::Fact { host, .. } = *row { if !hosts.contains(host) { return Err(SqlUnavailable::SelectionUnavailable); } }
        row.validate().map_err(|_| SqlUnavailable::SelectionUnavailable)?;
        encoded.clear();
        backend_engine::builtin::SqlAnalysisRelation::encode_value(*row, &mut encoded);
        work_bytes = work_bytes.checked_add(encoded.len()).and_then(|n| n.checked_add(40)).ok_or(SqlUnavailable::Limit)?;
        if work_bytes > 64 * 1024 * 1024 { return Err(SqlUnavailable::Limit); }
        hash.update(key);
        hash.update(&(encoded.len() as u64).to_be_bytes());
        hash.update(&encoded);
    }
    if cancellation.is_cancelled() { return Err(SqlUnavailable::Cancelled); }
    Ok(*hash.finalize().as_bytes())
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


/// Run the native pgrust provider against the exact selected host and its
/// explicitly proposed catalog. This function has no installed-worker or
/// publication authority; callers must supply both independently.
pub(super) fn capture_postgres_host(
    project: [u8; 32],
    root: &Path,
    host: &SqlCapturedHost,
    permit: &PostgresWorkerPermit,
    cancellation: &dyn CancellationObserver,
) -> Result<SqlHostDraft, SqlUnavailable> {
    let _ = project;
    use backend_library::sql_workspace::{SqlDialect as SelectedDialect, SqlProjectContext};
    if cancellation.is_cancelled() {
        return Err(SqlUnavailable::Cancelled);
    }
    if host.selected().dialect != SelectedDialect::Postgres {
        return Err(SqlUnavailable::UnsupportedDialect);
    }
    let host_key = sql_host_key(project, &host.selected().path)
        .map_err(|_| SqlUnavailable::SelectionUnavailable)?;
    let (context, context_id) = match &host.selected().context {
        SqlProjectContext::Unavailable => (None, None),
        SqlProjectContext::PostgresProposed {
            epoch,
            search_path,
            schemas,
        } => {
            let payload = serde_json::to_vec(&ProposedPostgresContext {
                epoch: *epoch,
                search_path,
                schemas,
            })
            .map_err(|_| SqlUnavailable::Context)?;
            if payload.len() > backend_sql_contract::MAX_CONTEXT_BYTES {
                return Err(SqlUnavailable::Limit);
            }
            let identity: [u8; 32] = Sha256::digest(&payload).into();
            (
                Some(SemanticContext::Proposed {
                    dialect: Dialect::Postgres,
                    payload,
                }),
                Some(identity),
            )
        }
        SqlProjectContext::SqliteProposed { .. } => return Err(SqlUnavailable::Context),
    };
    let request = Request {
        version: backend_sql_contract::PROTOCOL_VERSION,
        expected_basis: Some(permit.basis.clone()),
        source: host.bytes().to_vec(),
        settings: Settings::ProcessDefaults,
        context,
    };
    let wall = permit.wall;
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
        Dialect::Postgres,
        Engine::Pgrust38634b4,
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
    let mut facts = Vec::new();
    let terminal = match &response.response().syntax {
        Syntax::Parsed { .. } => {
            let (source, contextual) = match &response.response().binding {
                Binding::ProposedPartialReads { reads, .. } => {
                    if reads.len() > 4096 {
                        return Err(SqlUnavailable::Limit);
                    }
                    facts
                        .try_reserve_exact(reads.len())
                        .map_err(|_| SqlUnavailable::Limit)?;
                    for read in reads {
                        facts.push(SqlFact::PostgresColumnRead {
                            relation_oid: read.relation_oid,
                            attribute: read.attribute,
                            extent: SqlByteRange {
                                start: u32::try_from(read.extent.start)
                                    .map_err(|_| SqlUnavailable::Limit)?,
                                end: u32::try_from(read.extent.end)
                                    .map_err(|_| SqlUnavailable::Limit)?,
                            },
                        });
                    }
                    (SqlCoverage::Partial, SqlCoverage::Partial)
                }
                Binding::MissingContext
                | Binding::UnsupportedContext
                | Binding::ContextUnavailable
                | Binding::NativeContextError { .. }
                | Binding::NotAttempted => (SqlCoverage::Unavailable, SqlCoverage::Unavailable),
                _ => return Err(SqlUnavailable::WorkerFailure),
            };
            SqlHostTerminal::Parsed {
                source: if response.response().outline.is_some() { SqlCoverage::Partial } else { source },
                contextual,
                sqlite_lineage: None,
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
    let producer = digest(b"nudox.sql.postgres.producer.v1\0", &producer_bytes);
    let mut settings_bytes = basis_bytes;
    for value in [
        wall.as_secs(),
        8 * 1024 * 1024,
        128 * 1024,
        9 * 1024 * 1024,
        4 * 1024 * 1024,
    ] {
        settings_bytes.extend_from_slice(&value.to_be_bytes());
    }
    settings_bytes.extend_from_slice(b"memory-limit-unavailable\0");
    let settings = digest(b"nudox.sql.postgres.settings.v1\0", &settings_bytes);
    let evidence = response_evidence(&request, &response, &permit.artifact, cancellation)?;
    Ok(SqlHostDraft {
        host_key,
        path: host.selected().path.clone(),
        dialect: SqlDialect::Postgres,
        input: host.input().to_bytes(),
        source: *host.source().as_ref(),
        length: host.bytes().len() as u64,
        producer,
        settings,
        context_id,
        terminal,
        legacy_facts: facts,
        outline: response.response().outline.clone(),
        evidence,
    })
}

impl SqlProjectProjection {
    /// Capacity-based retained allocation charge for the private source
    /// witness, selected rows, walk and fence. Returns None on overflow or
    /// nested reuse witnesses without their own accounting contract.
    pub(super) fn checked_retained_bytes(&self) -> Option<usize> {
        fn allocation(total: &mut usize, capacity: usize) -> Option<()> {
            if capacity != 0 { *total = total.checked_add(capacity)?.checked_add(2 * std::mem::size_of::<usize>())?; }
            Some(())
        }
        let mut total = std::mem::size_of::<Self>();
        total = total.checked_add(self.capture.checked_retained_bytes()?)?
            .checked_add(self.walk.checked_retained_bytes()?)?
            .checked_add(self.fence.checked_sql_retained_bytes()?)?;
        allocation(&mut total, self.requested_root.capacity())?;
        allocation(&mut total, self.checked_responses.capacity().checked_mul(std::mem::size_of::<([u8; 32], [u8; 32])>())?)?;
        allocation(&mut total, self.rows.capacity().checked_mul(std::mem::size_of::<([u8; 32], SqlAnalysisRecord)>())?)?;
        for (_, row) in &self.rows {
            match row {
                SqlAnalysisRecord::Selection { source_root, .. } => allocation(&mut total, source_root.capacity())?,
                SqlAnalysisRecord::Host { path, .. } => allocation(&mut total, path.capacity())?,
                SqlAnalysisRecord::Attempt { .. } => {},
                SqlAnalysisRecord::Fact { fact, .. } => match fact {
                    SqlFact::SqliteRelation { name, .. } => allocation(&mut total, name.capacity())?,
                    SqlFact::SqliteOutputLineage { output_name, origin, .. } => {
                        allocation(&mut total, output_name.capacity())?;
                        if let Some(origin) = origin {
                            allocation(&mut total, origin.relation.capacity())?;
                            allocation(&mut total, origin.column.capacity())?;
                        }
                    }
                    SqlFact::Diagnostic { .. } | SqlFact::PostgresColumnRead { .. } | SqlFact::Statement { .. } => {},
                    // Boxed outline facts own nested strings and entity paths; charge
                    // twice their canonical encoding, which bounds the heap they hold.
                    SqlFact::Definition(_) | SqlFact::Reference(_) => {
                        let encoded = serde_json::to_vec(fact).ok()?.len();
                        allocation(&mut total, encoded.checked_mul(2)?)?;
                    }
                }
            }
        }
        total.checked_add(2 * std::mem::size_of::<usize>())
    }
    /// Digest the canonical project-owned rows independently of worker
    /// response bytes, so retained data can be compared after unrelated SQL
    /// projects change the global relation root.
    pub(super) fn project_rows_digest(&self, cancellation: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<[u8; 32], SqlUnavailable> {
        if !self.capture.matches_cancellation(cancellation) { return Err(SqlUnavailable::SelectionUnavailable); }
        sql_project_rows_digest(self.capture.project(), &self.rows, cancellation.as_ref())
    }
    /// Fixed digest over all admitted-at-capture worker responses and the
    /// exact bounded canonical project rows. It is a private commitment only;
    /// signing and selected-closure admission are separate steps.
    pub(super) fn checked_capture_digest(&self, cancellation: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<[u8; 32], SqlUnavailable> {
        if !self.capture.matches_cancellation(cancellation) { return Err(SqlUnavailable::SelectionUnavailable); }
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox.sql.checked-project-capture.v1\0");
        hash.update(&self.capture.project());
        hash.update(&(self.checked_responses.len() as u64).to_be_bytes());
        for (host, response) in &self.checked_responses {
            if cancellation.load(std::sync::atomic::Ordering::Acquire) { return Err(SqlUnavailable::Cancelled); }
            hash.update(host);
            hash.update(response);
        }
        hash.update(&(self.rows.len() as u64).to_be_bytes());
        let mut encoded = Vec::new();
        for (key, row) in &self.rows {
            if cancellation.load(std::sync::atomic::Ordering::Acquire) { return Err(SqlUnavailable::Cancelled); }
            encoded.clear();
            backend_engine::builtin::SqlAnalysisRelation::encode_value(row, &mut encoded);
            hash.update(key);
            hash.update(&(encoded.len() as u64).to_be_bytes());
            hash.update(&encoded);
        }
        if cancellation.load(std::sync::atomic::Ordering::Acquire) { return Err(SqlUnavailable::Cancelled); }
        Ok(*hash.finalize().as_bytes())
    }
    pub(super) fn rows(&self) -> &[([u8; 32], SqlAnalysisRecord)] {
        &self.rows
    }
    /// Number of selected hosts that carry a Host row (available or not).
    pub(super) fn host_count(&self) -> usize {
        self.rows.iter().filter(|(_, row)| matches!(row, SqlAnalysisRecord::Host { .. })).count()
    }
    pub(super) fn project(&self) -> [u8; 32] {
        self.capture.project()
    }
    pub(super) fn matches_cancellation(&self, cancellation: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> bool {
        self.capture.matches_cancellation(cancellation)
    }
    /// Reread both host identity domains and actual configuration bytes.
    pub(super) fn revalidate(
        &self,
        cancellation: &std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    postgres_permit: Option<&PostgresWorkerPermit>,
    cancellation: &std::sync::Arc<std::sync::atomic::AtomicBool>,
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
    let walk_inventory_complete = walk.inventory_complete();
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
    let mut checked_responses = Vec::with_capacity(capture.hosts().len());
    let mut rows = Vec::new();
    let mut budget = ProjectRowBudget {
        bytes: 0,
        max_bytes: 64 * 1024 * 1024,
        max_rows: 262_144,
    };
    let empty_fact_digest = sql_host_fact_digest(&[]).map_err(|_| SqlUnavailable::WorkerFailure)?;
    let mut outcomes = Vec::with_capacity(capture.hosts().len());
    for host in capture.hosts() {
        if cancellation.load(std::sync::atomic::Ordering::Acquire) {
            return Err(SqlUnavailable::Cancelled);
        }
        let dialect = match host.selected().dialect {
            backend_library::sql_workspace::SqlDialect::Sqlite => SqlDialect::Sqlite,
            backend_library::sql_workspace::SqlDialect::Postgres => SqlDialect::Postgres,
        };
        if let Some(reason) = host.fault() {
            let host_key = sql_host_key(project, &host.selected().path)
                .map_err(|_| SqlUnavailable::SelectionUnavailable)?;
            inventory.push((host.selected().path.clone(), dialect));
            outcomes.push(HostOutcome::Unavailable(
                host_key,
                SqlAnalysisRecord::Host {
                    project,
                    path: host.selected().path.clone(),
                    dialect,
                    input: None,
                    source: None,
                    length: None,
                    producer: None,
                    settings: None,
                    context: None,
                    terminal: SqlHostTerminal::Unavailable(reason),
                    fact_count: 0,
                    fact_digest: empty_fact_digest,
                },
            ));
            continue;
        }
        let mut result = match (dialect, permit) {
            (SqlDialect::Sqlite, Some(permit)) => {
                capture_sqlite_host(project, walk.root(), host, permit, cancellation.as_ref())
            }
            (SqlDialect::Sqlite, None) => Err(SqlUnavailable::EngineNotAdmitted),
            (SqlDialect::Postgres, _) => match postgres_permit {
                Some(permit) => {
                    capture_postgres_host(project, walk.root(), host, permit, cancellation.as_ref())
                }
                None => Err(SqlUnavailable::UnsupportedDialect),
            },
        };
        // A host with no dialect signal is offered to the other native parser when
        // the first one rejects it. The accepted dialect is what gets recorded.
        if host.oracle() && dialect == SqlDialect::Sqlite {
            let rejected = matches!(&result, Ok(draft) if matches!(draft.terminal, SqlHostTerminal::NativeError))
                || matches!(&result, Err(SqlUnavailable::EngineNotAdmitted));
            if rejected {
                if let Some(permit) = postgres_permit {
                    let alternate = host.as_dialect(backend_library::sql_workspace::SqlDialect::Postgres);
                    if let Ok(draft) = capture_postgres_host(project, walk.root(), &alternate, permit, cancellation.as_ref()) {
                        if !matches!(draft.terminal, SqlHostTerminal::NativeError) {
                            result = Ok(draft);
                        }
                    }
                }
            }
        }
        let dialect = match &result {
            Ok(draft) => draft.dialect(),
            Err(_) => dialect,
        };
        inventory.push((host.selected().path.clone(), dialect));
        match result {
            Ok(draft) => outcomes.push(HostOutcome::Draft(draft)),
            Err(SqlUnavailable::Cancelled) => return Err(SqlUnavailable::Cancelled),
            Err(reason) => {
                let host_key = sql_host_key(project, &host.selected().path)
                    .map_err(|_| SqlUnavailable::SelectionUnavailable)?;
                outcomes.push(HostOutcome::Unavailable(
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
                ));
            }
        }
    }
    // Bind outlines across the whole project, then derive each host's rows.
    let bound = {
        let hosts: Vec<super::sql_resolve::HostOutline<'_>> = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                HostOutcome::Draft(draft) => draft.outline().map(|outline| super::sql_resolve::HostOutline {
                    path: draft.path(),
                    dialect: draft.dialect(),
                    outline,
                }),
                HostOutcome::Unavailable(..) => None,
            })
            .collect();
        super::sql_resolve::bind_project(&hosts)
    };
    let mut bound = bound.into_iter();
    for outcome in outcomes {
        match outcome {
            HostOutcome::Unavailable(host_key, row) => budget.push(&mut rows, host_key, row)?,
            HostOutcome::Draft(draft) => {
                let outline_facts = if draft.outline().is_some() {
                    bound.next().ok_or(SqlUnavailable::WorkerFailure)?
                } else {
                    Vec::new()
                };
                let projected = finish_host(draft, outline_facts, project, cancellation.as_ref())?;
                checked_responses.push((projected.host_key, projected.checked_response));
                budget.push(&mut rows, projected.host_key, projected.host)?;
                for (key, fact) in projected.facts {
                    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
                        return Err(SqlUnavailable::Cancelled);
                    }
                    budget.push(&mut rows, key, fact)?;
                }
            }
        }
    }
    let mut gaps = backend_semantic::sql_analysis::SqlGapCounts::default();
    for (_, row) in &rows {
        match row {
            SqlAnalysisRecord::Host { terminal: SqlHostTerminal::Unavailable(_), .. } => gaps.unavailable_hosts += 1,
            SqlAnalysisRecord::Host { terminal: SqlHostTerminal::NativeError, .. } => gaps.native_error_hosts += 1,
            SqlAnalysisRecord::Fact { fact: SqlFact::Statement { support, .. }, .. } => match support {
                backend_semantic::sql_analysis::SqlStatementSupport::Full => {}
                backend_semantic::sql_analysis::SqlStatementSupport::Partial(_) => gaps.partial_statements += 1,
                backend_semantic::sql_analysis::SqlStatementSupport::Unsupported(_) => gaps.unsupported_statements += 1,
            },
            _ => {}
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
        implicit: capture.implicit(),
        gaps,
        // An implicit selection is the complete bounded walk; an explicit one
        // only lists what the settings file named.
        inventory: if capture.implicit() && walk_inventory_complete { SqlInventory::Complete } else { SqlInventory::Incomplete },
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
    if cancellation.load(std::sync::atomic::Ordering::Acquire) {
        return Err(SqlUnavailable::Cancelled);
    }
    Ok(SqlProjectProjection {
        capture: capture.witness(),
        walk,
        fence,
        requested_root,
        rows,
        checked_responses,
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

#[cfg(all(test, target_os = "linux"))]
#[path = "sql_capture_installed_tests.rs"]
mod installed_companion_tests;

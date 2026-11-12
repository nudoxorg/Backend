//! Shared experimental SQL source, dialect and coverage DTOs, not semantic authority.
pub use backend_compile::{
    Cancellation, InputContentVersion, ProcessEnvironment, ProcessError, ProcessLimits,
    ProcessTerminal, SourceVerificationError, ToolchainArtifact, VerifiedSourceText,
};
#[cfg(target_os = "linux")]
pub use backend_compile::{
    ExecutableIdentity, NativeLibraryLease, ProcessSupervisor, SupervisedCommand,
};
pub use backend_version::{ContentId, SourceFactDomain};
use serde::{Deserialize, Serialize};
pub const MAX_SOURCE_BYTES: usize = 512 * 1024;
pub const MAX_CONTEXT_BYTES: usize = 128 * 1024;
pub const MAX_TOKENS: usize = 100_000;
pub const PROTOCOL_VERSION: u16 = 4;
mod outline;
pub use outline::*;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dialect {
    Postgres,
    Sqlite,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Engine {
    Pgrust38634b4,
    Syntaqlite4e4de76,
}
/// Process defaults are pinned only for the lifetime of the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Settings {
    ProcessDefaults,
}
/// Finite observed engine profile; unrepresented configuration is unsupported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProducerBasis {
    pub engine: Engine,
    pub native_revision: String,
    pub recipe: String,
    pub lock_sha256: [u8; 32],
    /// Bound source/dependency support fingerprint, or unavailable for historical producers.
    pub support_sha256: Option<[u8; 32]>,
    pub settings: EffectiveSettings,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum EffectiveSettings {
    Postgres {
        backslash_quote: i32,
        escape_string_warning: bool,
        standard_conforming_strings: bool,
        server_encoding: i32,
        client_encoding: i32,
    },
    /// Syntaqlite analysis fallback, not a claim about a SQLite connection's DQS flags.
    Sqlite {
        version: i32,
        cflags: u64,
        analyzer_dqs_fallback: bool,
    },
}
/// A raw context placeholder cannot authorize catalog-bound claims.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum SemanticContext {
    Unavailable,
    /// Adapter-specific canonical bytes describe proposed, not admitted, catalog authority.
    Proposed {
        dialect: Dialect,
        payload: Vec<u8>,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub version: u16,
    pub expected_basis: Option<ProducerBasis>,
    pub source: Vec<u8>,
    pub settings: Settings,
    pub context: Option<SemanticContext>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Extent {
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identifier {
    pub extent: Extent,
    pub spelling: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Syntax {
    Parsed {
        statements: usize,
        identifiers: Vec<Identifier>,
        identifier_coverage: IdentifierCoverage,
    },
    NativeError,
    UnsupportedSettings,
    Rejected,
    ResourceLimit,
    TimedOut,
    InitializationUnavailable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentifierCoverage {
    NativeIdentTokenSubset,
    Unavailable,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Binding {
    MissingContext,
    UnsupportedContext,
    /// Partial diagnostic evidence under caller-proposed context, never graph authority.
    ProposedAnalysis {
        context_sha256: [u8; 32],
        diagnostics: Vec<NativeDiagnostic>,
        /// Native SELECT model output; never an authored-site or global object identity.
        relations: Vec<SqliteRelationAccess>,
        /// Native output-column lineage, not all authored column references.
        lineage: Vec<SqliteColumnLineage>,
        /// Syntaqlite's own lineage status, scoped to this proposed catalog.
        native_lineage: SqliteLineageCoverage,
    },
    /// Partial reads under a proposed PostgreSQL context. OIDs are context-local.
    ProposedPartialReads {
        context_sha256: [u8; 32],
        reads: Vec<NativeRead>,
    },
    NativeContextError {
        context_sha256: [u8; 32],
    },
    ContextUnavailable,
    NotAttempted,
}
/// Native model relation kind. Context and dialect remain part of its identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqliteRelationKind {
    Table,
    View,
}
/// One native model relation access without a fabricated source occurrence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteRelationAccess {
    pub name: String,
    pub kind: SqliteRelationKind,
}
/// Native lineage availability, never a whole-source completeness guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqliteLineageCoverage {
    Unavailable,
    NativeComplete,
    NativePartial,
}
/// One SELECT output column; origin is absent for unresolved expressions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteColumnLineage {
    pub output_name: String,
    pub ordinal: u32,
    pub origin: Option<SqliteColumnOrigin>,
}
/// Native-reported origin in the caller-proposed SQLite catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqliteColumnOrigin {
    pub relation: String,
    pub column: String,
}
fn valid_model_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.contains('\0')
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeRead {
    pub relation_oid: u32,
    pub attribute: u16,
    pub extent: Extent,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeDiagnostic {
    pub kind: DiagnosticKind,
    pub extent: Option<Extent>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticKind {
    UnknownColumn,
    Other,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub version: u16,
    pub basis: Option<ProducerBasis>,
    pub dialect: Dialect,
    pub engine: Engine,
    pub content_sha256: [u8; 32],
    pub syntax: Syntax,
    pub binding: Binding,
    /// Syntactic outline of the whole source; present exactly when `syntax` is `Parsed`.
    pub outline: Option<Outline>,
}

/// Launch a one-shot adapter process; failed transport is never an empty result.
/// A finite wall-clock deadline and response-size bound protect the parent.
pub fn call_worker(
    worker: &std::path::Path,
    request: &Request,
    dialect: Dialect,
    engine: Engine,
) -> Response {
    call_worker_with_timeout(
        worker,
        request,
        dialect,
        engine,
        std::time::Duration::from_secs(10),
    )
}

fn call_worker_with_timeout(
    worker: &std::path::Path,
    request: &Request,
    dialect: Dialect,
    engine: Engine,
    timeout: std::time::Duration,
) -> Response {
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        process::{Command, Stdio},
        time::Instant,
    };
    let digest: [u8; 32] = Sha256::digest(&request.source).into();
    let unavailable = || Response {
        version: PROTOCOL_VERSION,
        basis: None,
        dialect,
        engine,
        content_sha256: digest,
        syntax: Syntax::InitializationUnavailable,
        binding: Binding::NotAttempted,
            outline: None,
    };
    if request.version != PROTOCOL_VERSION {
        return unavailable();
    }
    if matches!(&request.context, Some(SemanticContext::Proposed { payload, .. }) if payload.len() > MAX_CONTEXT_BYTES)
    {
        return Response {
            version: PROTOCOL_VERSION,
            basis: None,
            dialect,
            engine,
            content_sha256: digest,
            syntax: Syntax::ResourceLimit,
            binding: Binding::NotAttempted,
            outline: None,
        };
    }
    if request.source.len() > MAX_SOURCE_BYTES {
        return Response {
            version: PROTOCOL_VERSION,
            basis: None,
            dialect,
            engine,
            content_sha256: digest,
            syntax: Syntax::ResourceLimit,
            binding: Binding::NotAttempted,
            outline: None,
        };
    }
    let Ok(encoded) = serde_json::to_vec(request) else {
        return unavailable();
    };
    let Ok(mut child) = Command::new(worker)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return unavailable();
    };
    const MAX_OUTPUT: u64 = 8 * 1024 * 1024;
    let Some(mut input) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return unavailable();
    };
    let Some(output) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return unavailable();
    };
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel(1);
    let (read_tx, read_rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = write_tx.send(input.write_all(&encoded).is_ok());
    });
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let ok = output.take(MAX_OUTPUT + 1).read_to_end(&mut bytes).is_ok();
        let _ = read_tx.send((ok, bytes));
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Response {
                    version: PROTOCOL_VERSION,
                    basis: None,
                    dialect,
                    engine,
                    content_sha256: digest,
                    syntax: Syntax::TimedOut,
                    binding: Binding::NotAttempted,
            outline: None,
                };
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return unavailable();
            }
        }
    };
    if !status.success() {
        return unavailable();
    }
    let timed_out = || Response {
        version: PROTOCOL_VERSION,
        basis: None,
        dialect,
        engine,
        content_sha256: digest,
        syntax: Syntax::TimedOut,
        binding: Binding::NotAttempted,
            outline: None,
    };
    let wrote = match write_rx.recv_timeout(timeout.saturating_sub(started.elapsed())) {
        Ok(wrote) => wrote,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return timed_out(),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return unavailable(),
    };
    if !wrote {
        return unavailable();
    }
    let (ok, output) = match read_rx.recv_timeout(timeout.saturating_sub(started.elapsed())) {
        Ok(value) => value,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return timed_out(),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return unavailable(),
    };
    if !ok {
        return unavailable();
    }
    if output.len() as u64 > MAX_OUTPUT {
        return Response {
            version: PROTOCOL_VERSION,
            basis: None,
            dialect,
            engine,
            content_sha256: digest,
            syntax: Syntax::ResourceLimit,
            binding: Binding::NotAttempted,
            outline: None,
        };
    }
    decode_response(&output, request, dialect, engine, digest).unwrap_or_else(unavailable)
}

fn decode_response(
    output: &[u8],
    request: &Request,
    dialect: Dialect,
    engine: Engine,
    digest: [u8; 32],
) -> Option<Response> {
    use sha2::{Digest, Sha256};
    let Ok(response) = serde_json::from_slice::<Response>(output) else {
        return None;
    };
    if response.version != PROTOCOL_VERSION
        || request
            .expected_basis
            .as_ref()
            .is_some_and(|expected| response.basis.as_ref() != Some(expected))
            && response.syntax != Syntax::UnsupportedSettings
        || response.content_sha256 != digest
        || response.dialect != dialect
        || response.engine != engine
    {
        return None;
    }
    if let Syntax::Parsed {
        identifiers,
        identifier_coverage,
        ..
    } = &response.syntax
    {
        let expected_binding = if request.context.is_none() {
            Binding::MissingContext
        } else {
            Binding::UnsupportedContext
        };
        let expected_coverage = match dialect {
            Dialect::Postgres => IdentifierCoverage::NativeIdentTokenSubset,
            Dialect::Sqlite => IdentifierCoverage::Unavailable,
        };
        let valid_binding = match (&request.context, &response.binding) {
            (
                Some(SemanticContext::Proposed {
                    dialect: context_dialect,
                    payload,
                }),
                Binding::ProposedAnalysis {
                    context_sha256,
                    diagnostics,
                    relations,
                    lineage,
                    native_lineage,
                },
            ) if *context_dialect == dialect
                && dialect == Dialect::Sqlite
                && payload.len() <= MAX_CONTEXT_BYTES =>
            {
                let actual: [u8; 32] = Sha256::digest(payload).into();
                actual == *context_sha256
                    && diagnostics.len() <= MAX_TOKENS
                    && relations.len() <= 512
                    && lineage.len() <= 512
                    && (!matches!(native_lineage, SqliteLineageCoverage::Unavailable)
                        || (relations.is_empty() && lineage.is_empty()))
                    && relations.iter().all(|r| valid_model_name(&r.name))
                    && lineage.iter().enumerate().all(|(i, c)| {
                        usize::try_from(c.ordinal).ok() == Some(i)
                            && valid_model_name(&c.output_name)
                            && c.origin.as_ref().is_none_or(|o| {
                                valid_model_name(&o.relation) && valid_model_name(&o.column)
                            })
                    })
                    && diagnostics.iter().all(|d| {
                        d.extent.as_ref().is_none_or(|e| {
                            e.start <= e.end
                                && std::str::from_utf8(&request.source)
                                    .ok()
                                    .and_then(|text| text.get(e.start..e.end))
                                    .is_some()
                        })
                    })
            }
            (
                Some(SemanticContext::Proposed {
                    dialect: Dialect::Postgres,
                    payload,
                }),
                Binding::ProposedPartialReads {
                    context_sha256,
                    reads,
                },
            ) if dialect == Dialect::Postgres && payload.len() <= MAX_CONTEXT_BYTES => {
                let actual: [u8; 32] = Sha256::digest(payload).into();
                actual == *context_sha256
                    && reads.len() <= MAX_TOKENS
                    && reads
                        .windows(2)
                        .all(|pair| pair[0].extent.end <= pair[1].extent.start)
                    && reads.iter().all(|r| {
                        r.relation_oid != 0
                            && r.attribute != 0
                            && identifiers
                                .binary_search_by_key(&r.extent.start, |i| i.extent.start)
                                .ok()
                                .is_some_and(|index| identifiers[index].extent == r.extent)
                    })
            }
            (
                Some(SemanticContext::Proposed {
                    dialect: Dialect::Postgres,
                    payload,
                }),
                Binding::NativeContextError { context_sha256 },
            ) if dialect == Dialect::Postgres && payload.len() <= MAX_CONTEXT_BYTES => {
                let actual: [u8; 32] = Sha256::digest(payload).into();
                actual == *context_sha256
            }
            (
                Some(SemanticContext::Proposed {
                    dialect: Dialect::Postgres,
                    ..
                }),
                Binding::ContextUnavailable,
            ) if dialect == Dialect::Postgres => true,
            _ => response.binding == expected_binding,
        };
        if !valid_binding
            || *identifier_coverage != expected_coverage
            || identifiers.len() > MAX_TOKENS
        {
            return None;
        }
        if *identifier_coverage == IdentifierCoverage::Unavailable && !identifiers.is_empty() {
            return None;
        }
        match &response.outline {
            Some(outline) if outline.validate(&request.source) => {}
            _ => return None,
        }
        let mut previous_end = 0;
        for identifier in identifiers {
            let extent = &identifier.extent;
            if extent.start < previous_end
                || extent.start >= extent.end
                || request.source.get(extent.start..extent.end)
                    != Some(identifier.spelling.as_bytes())
            {
                return None;
            }
            previous_end = extent.end;
        }
    } else if response.binding != Binding::NotAttempted || response.outline.is_some() {
        return None;
    }
    Some(response)
}

fn supported_basis(basis: &ProducerBasis, dialect: Dialect, engine: Engine) -> bool {
    if basis.engine != engine {
        return false;
    }
    match (dialect, engine, &basis.settings) {
        (
            Dialect::Postgres,
            Engine::Pgrust38634b4,
            EffectiveSettings::Postgres {
                backslash_quote: 2,
                escape_string_warning: true,
                standard_conforming_strings: true,
                server_encoding: 0,
                client_encoding: 0,
            },
        ) => {
            basis.native_revision == "38634b408dea373f3944006dfbf74d19cbfc409f"
                && basis.recipe == "pgrust-parser-scanner-analyzer-int4-proposed-v1"
        }
        (
            Dialect::Sqlite,
            Engine::Syntaqlite4e4de76,
            EffectiveSettings::Sqlite {
                version: i32::MAX,
                cflags: 0,
                analyzer_dqs_fallback: true,
            },
        ) => {
            basis.native_revision == "4e4de76312429fb2235ae093ab2125914320d5bd"
                && basis.recipe == "syntaqlite-sqlite-fmt-analysis-dqs-default-v1"
                && basis.support_sha256.is_some_and(|value| value != [0; 32])
        }
        _ => false,
    }
}

/// Caller-supplied, already admitted worker and explicit execution policy.
pub struct CheckedLaunch<'a> {
    pub program: &'a std::path::Path,
    pub workspace: &'a std::path::Path,
    pub artifact: backend_compile::ToolchainArtifact,
    pub environment: backend_compile::ProcessEnvironment,
    pub limits: backend_compile::ProcessLimits,
    pub cancellation: &'a dyn backend_compile::CancellationObserver,
}
/// A checked response whose returned ranges were validated against actual host bytes.
pub struct CheckedResponse<'a> {
    response: Response,
    command: backend_compile::CommandId,
    extents: Vec<backend_compile::VerifiedSourceExtent<'a>>,
}
impl<'a> CheckedResponse<'a> {
    pub fn response(&self) -> &Response {
        &self.response
    }
    pub fn command(&self) -> backend_compile::CommandId {
        self.command
    }
    pub fn extents(&self) -> &[backend_compile::VerifiedSourceExtent<'a>] {
        &self.extents
    }
}
#[derive(Debug)]
pub enum CheckedError {
    Cancelled,
    Source(backend_compile::SourceVerificationError),
    SourceRequestMismatch,
    MissingBasis,
    InputLimit,
    UnsupportedSettings,
    Process(backend_compile::ProcessError),
    Terminal(backend_compile::ProcessTerminal),
    InvalidResponse,
}
/// Observe a pinned worker's producer basis using one fixed readiness request.
///
/// The caller must already admit the exact executable artifact; the response is
/// only a basis claim from those admitted bytes, not semantic authority. The
/// probe uses the same bounded supervisor and reaper custody as host analysis.
/// It accepts neither a caller-supplied query nor a private dependency closure.
///
/// # Errors
/// Returns process, timeout, cancellation, or response-validation failures.
pub fn probe_basis_checked(
    launch: CheckedLaunch<'_>,
    dialect: Dialect,
    engine: Engine,
) -> Result<ProducerBasis, CheckedError> {
    use backend_compile::{
        ProcessStdin, ProcessSupervisor, ProcessTerminal, ProtocolDescriptor, SupervisedCommand,
    };
    use sha2::{Digest, Sha256};
    if launch.cancellation.is_cancelled() {
        return Err(CheckedError::Cancelled);
    }
    if !launch.environment.variables().is_empty() || !launch.artifact.dependencies().is_empty() {
        return Err(CheckedError::UnsupportedSettings);
    }
    let deadline = std::time::Instant::now()
        .checked_add(launch.limits.wall_time())
        .ok_or(CheckedError::Process(
            backend_compile::ProcessError::Deadline,
        ))?;
    let request = Request {
        version: PROTOCOL_VERSION,
        expected_basis: None,
        source: b"SELECT 1".to_vec(),
        settings: Settings::ProcessDefaults,
        context: None,
    };
    let encoded = serde_json::to_vec(&request).map_err(|_| CheckedError::InvalidResponse)?;
    let command = SupervisedCommand::for_authority_with_artifact_until(
        launch.program.to_owned(),
        Vec::new(),
        launch.environment,
        launch.workspace.to_owned(),
        ProcessStdin::bytes(encoded),
        launch.artifact,
        None,
        ProtocolDescriptor::new(PROTOCOL_VERSION, false),
        launch.limits,
        launch.cancellation,
        deadline,
    )
    .map_err(CheckedError::Process)?;
    let receipt = ProcessSupervisor::new(command)
        .run_with_observer_until(launch.cancellation, deadline)
        .map_err(CheckedError::Process)?;
    if receipt.terminal() != ProcessTerminal::Success || !receipt.reaped() {
        return Err(CheckedError::Terminal(receipt.terminal()));
    }
    if launch.cancellation.is_cancelled() {
        return Err(CheckedError::Cancelled);
    }
    let digest: [u8; 32] = Sha256::digest(&request.source).into();
    let response = decode_response(receipt.stdout(), &request, dialect, engine, digest)
        .ok_or(CheckedError::InvalidResponse)?;
    if !matches!(response.syntax, Syntax::Parsed { .. }) {
        return Err(CheckedError::InvalidResponse);
    }
    let basis = response.basis.ok_or(CheckedError::MissingBasis)?;
    if !supported_basis(&basis, dialect, engine) {
        return Err(CheckedError::UnsupportedSettings);
    }
    if launch.cancellation.is_cancelled() {
        return Err(CheckedError::Cancelled);
    }
    if std::time::Instant::now() >= deadline {
        return Err(CheckedError::Process(
            backend_compile::ProcessError::Deadline,
        ));
    }
    Ok(basis)
}

/// Verify actual host against both caller-authenticated source domains and run a
/// caller-admitted executable via the existing bounded process supervisor.
/// This does not authenticate the source row, catalog, recipe or artifact admission.
pub fn analyze_checked<'a>(
    request: &Request,
    host: &'a [u8],
    expected_input: backend_compile::InputContentVersion,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    launch: CheckedLaunch<'_>,
    dialect: Dialect,
    engine: Engine,
) -> Result<CheckedResponse<'a>, CheckedError> {
    if launch.cancellation.is_cancelled() {
        return Err(CheckedError::Cancelled);
    }
    let deadline = std::time::Instant::now()
        .checked_add(launch.limits.wall_time())
        .ok_or(CheckedError::Process(
            backend_compile::ProcessError::Deadline,
        ))?;
    analyze_checked_until(
        request,
        host,
        expected_input,
        expected_source,
        launch,
        dialect,
        engine,
        deadline,
    )
}

/// Run the checked protocol under a caller's existing absolute deadline.
///
/// This preserves the empty-environment admission policy and never extends
/// either the supplied deadline or the launch's own wall-time bound.
///
/// # Errors
/// Returns the same source, policy, process and response errors as [`analyze_checked`].
pub fn analyze_checked_until<'a>(
    request: &Request,
    host: &'a [u8],
    expected_input: backend_compile::InputContentVersion,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    launch: CheckedLaunch<'_>,
    dialect: Dialect,
    engine: Engine,
    deadline: std::time::Instant,
) -> Result<CheckedResponse<'a>, CheckedError> {
    analyze_checked_inner(
        request,
        host,
        expected_input,
        expected_source,
        launch,
        dialect,
        engine,
        deadline,
        CheckedPrivateLibrary::None,
    )
}

enum CheckedPrivateLibrary {
    None,
    #[cfg(target_os = "linux")]
    Linux(std::sync::Arc<backend_compile::NativeLibraryLease>),
}

/// Run a checked SQLite worker with one already-admitted private Linux libgcc.
///
/// The lease's exact typed dependency and derived loader environment must
/// match the command. Its lifetime follows child/reaper custody on errors.
/// The caller must prepare the lease and invoke this function under one
/// absolute deadline; this does not admit the installed worker or source.
///
/// # Errors
/// Returns the same checked errors as [`analyze_checked_until`], including
/// dependency, environment, deadline, process and cleanup failures.
#[cfg(target_os = "linux")]
pub fn analyze_checked_with_linux_library_until<'a>(
    request: &Request,
    host: &'a [u8],
    expected_input: backend_compile::InputContentVersion,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    launch: CheckedLaunch<'_>,
    dialect: Dialect,
    engine: Engine,
    deadline: std::time::Instant,
    library: backend_compile::NativeLibraryLease,
) -> Result<CheckedResponse<'a>, CheckedError> {
    analyze_checked_inner(
        request,
        host,
        expected_input,
        expected_source,
        launch,
        dialect,
        engine,
        deadline,
        CheckedPrivateLibrary::Linux(std::sync::Arc::new(library)),
    )
}

fn analyze_checked_inner<'a>(
    request: &Request,
    host: &'a [u8],
    expected_input: backend_compile::InputContentVersion,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    launch: CheckedLaunch<'_>,
    dialect: Dialect,
    engine: Engine,
    deadline: std::time::Instant,
    library: CheckedPrivateLibrary,
) -> Result<CheckedResponse<'a>, CheckedError> {
    use backend_compile::{
        ProcessEnvironment, ProcessStdin, ProcessSupervisor, ProcessTerminal, ProtocolDescriptor,
        SupervisedCommand, VerifiedSourceText,
    };
    use sha2::{Digest, Sha256};
    if launch.cancellation.is_cancelled() {
        return Err(CheckedError::Cancelled);
    }
    let command_deadline = std::time::Instant::now()
        .checked_add(launch.limits.wall_time())
        .ok_or(CheckedError::Process(
            backend_compile::ProcessError::Deadline,
        ))?;
    let deadline = deadline.min(command_deadline);
    if std::time::Instant::now() >= deadline {
        return Err(CheckedError::Process(
            backend_compile::ProcessError::Deadline,
        ));
    }
    match &library {
        CheckedPrivateLibrary::None if !launch.environment.variables().is_empty() => {
            return Err(CheckedError::UnsupportedSettings);
        }
        #[cfg(target_os = "linux")]
        CheckedPrivateLibrary::Linux(lease) => {
            if dialect != Dialect::Sqlite
                || engine != Engine::Syntaqlite4e4de76
                || launch.artifact.dependencies() != [lease.digest()]
                || launch.environment != lease.environment().map_err(CheckedError::Process)?
            {
                return Err(CheckedError::UnsupportedSettings);
            }
        }
        _ => {}
    }
    if host.len() > MAX_SOURCE_BYTES
        || request.source.len() > MAX_SOURCE_BYTES
        || matches!(&request.context, Some(SemanticContext::Proposed { payload, .. }) if payload.len() > MAX_CONTEXT_BYTES)
    {
        return Err(CheckedError::InputLimit);
    }
    let verified = VerifiedSourceText::admit(host, expected_input, expected_source)
        .map_err(CheckedError::Source)?;
    if launch.cancellation.is_cancelled() {
        return Err(CheckedError::Cancelled);
    }
    if request.source != host {
        return Err(CheckedError::SourceRequestMismatch);
    }
    let basis = request
        .expected_basis
        .as_ref()
        .ok_or(CheckedError::MissingBasis)?;
    if !supported_basis(basis, dialect, engine) || request.version != PROTOCOL_VERSION {
        return Err(CheckedError::UnsupportedSettings);
    }
    let encoded = serde_json::to_vec(request).map_err(|_| CheckedError::InvalidResponse)?;
    // ProcessEnvironment is explicit; it is committed into command.identity().
    let environment: ProcessEnvironment = launch.environment;
    let command = SupervisedCommand::for_authority_with_artifact_until(
        launch.program.to_owned(),
        Vec::new(),
        environment,
        launch.workspace.to_owned(),
        ProcessStdin::bytes(encoded),
        launch.artifact,
        None,
        ProtocolDescriptor::new(PROTOCOL_VERSION, false),
        launch.limits,
        launch.cancellation,
        deadline,
    )
    .map_err(CheckedError::Process)?;
    let command_id = command.identity();
    let supervisor = ProcessSupervisor::new(command);
    let receipt = match &library {
        CheckedPrivateLibrary::None => {
            supervisor.run_with_observer_until(launch.cancellation, deadline)
        }
        #[cfg(target_os = "linux")]
        CheckedPrivateLibrary::Linux(lease) => supervisor.run_with_observer_and_library_until(
            launch.cancellation,
            deadline,
            std::sync::Arc::clone(lease),
        ),
    }
    .map_err(CheckedError::Process)?;
    #[cfg(target_os = "linux")]
    if let CheckedPrivateLibrary::Linux(lease) = library {
        let mut lease = std::sync::Arc::try_unwrap(lease)
            .map_err(|_| CheckedError::Process(backend_compile::ProcessError::NotReaped))?;
        lease
            .close_after_reaped(&receipt)
            .map_err(CheckedError::Process)?;
    }
    if receipt.terminal() != ProcessTerminal::Success || !receipt.reaped() {
        return Err(CheckedError::Terminal(receipt.terminal()));
    }
    if launch.cancellation.is_cancelled() {
        return Err(CheckedError::Cancelled);
    }
    let digest: [u8; 32] = Sha256::digest(host).into();
    let response = decode_response(receipt.stdout(), request, dialect, engine, digest)
        .ok_or(CheckedError::InvalidResponse)?;
    if response.syntax == Syntax::UnsupportedSettings {
        return Err(CheckedError::UnsupportedSettings);
    }
    let mut extents = Vec::new();
    if let Syntax::Parsed { identifiers, .. } = &response.syntax {
        for identifier in identifiers {
            extents.push(
                verified
                    .extent(identifier.extent.start..identifier.extent.end)
                    .map_err(CheckedError::Source)?,
            );
        }
    }
    if let Some(outline) = &response.outline {
        for extent in outline.extents() {
            extents.push(
                verified
                    .extent(extent.start..extent.end)
                    .map_err(CheckedError::Source)?,
            );
        }
    }
    match &response.binding {
        Binding::ProposedPartialReads { reads, .. } => {
            for read in reads {
                extents.push(
                    verified
                        .extent(read.extent.start..read.extent.end)
                        .map_err(CheckedError::Source)?,
                );
            }
        }
        Binding::ProposedAnalysis { diagnostics, .. } => {
            for d in diagnostics {
                if let Some(e) = &d.extent {
                    extents.push(
                        verified
                            .extent(e.start..e.end)
                            .map_err(CheckedError::Source)?,
                    );
                }
            }
        }
        _ => {}
    }
    if launch.cancellation.is_cancelled() {
        return Err(CheckedError::Cancelled);
    }
    if std::time::Instant::now() >= deadline {
        return Err(CheckedError::Process(
            backend_compile::ProcessError::Deadline,
        ));
    }
    Ok(CheckedResponse {
        response,
        command: command_id,
        extents,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    #[test]
    fn readiness_probe_supervises_pinned_worker_and_rejects_drift() {
        use backend_compile::{ProcessEnvironment, ProcessLimits, ToolchainArtifact};
        use sha2::{Digest, Sha256};
        use std::sync::atomic::AtomicBool;
        let path = std::env::temp_dir().join(format!("nudox-sql-probe-{}", std::process::id()));
        let basis = ProducerBasis {
            engine: Engine::Syntaqlite4e4de76,
            native_revision: "4e4de76312429fb2235ae093ab2125914320d5bd".into(),
            recipe: "syntaqlite-sqlite-fmt-analysis-dqs-default-v1".into(),
            lock_sha256: [3; 32],
            support_sha256: Some([4; 32]),
            settings: EffectiveSettings::Sqlite {
                version: i32::MAX,
                cflags: 0,
                analyzer_dqs_fallback: true,
            },
        };
        let response = Response {
            version: PROTOCOL_VERSION,
            basis: Some(basis.clone()),
            dialect: Dialect::Sqlite,
            engine: Engine::Syntaqlite4e4de76,
            content_sha256: Sha256::digest(b"SELECT 1").into(),
            syntax: Syntax::Parsed {
                statements: 1,
                identifiers: vec![],
                identifier_coverage: IdentifierCoverage::Unavailable,
            },
            binding: Binding::MissingContext,
            outline: Some(Outline {
                coverage: OutlineCoverage::Complete,
                statements: vec![],
            }),
        };
        let json = serde_json::to_string(&response).unwrap();
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nIFS= read -r request || :\nprintf '%s' '{}'\n",
                json
            ),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let artifact = ToolchainArtifact::from_path(&path, Vec::new()).unwrap();
        let cancelled = AtomicBool::new(false);
        let launch = || CheckedLaunch {
            program: &path,
            workspace: path.parent().unwrap(),
            artifact: artifact.clone(),
            environment: ProcessEnvironment::new(vec![]).unwrap(),
            limits: ProcessLimits::new(65536, 4096, Duration::from_secs(2), 69632).unwrap(),
            cancellation: &cancelled,
        };
        assert_eq!(
            probe_basis_checked(launch(), Dialect::Sqlite, Engine::Syntaqlite4e4de76).unwrap(),
            basis
        );
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            probe_basis_checked(launch(), Dialect::Sqlite, Engine::Syntaqlite4e4de76),
            Err(CheckedError::Cancelled)
        ));
        cancelled.store(false, std::sync::atomic::Ordering::Release);
        let mut with_environment = launch();
        with_environment.environment =
            ProcessEnvironment::new(vec![("PATH".into(), "/untrusted".into())]).unwrap();
        assert!(matches!(
            probe_basis_checked(with_environment, Dialect::Sqlite, Engine::Syntaqlite4e4de76),
            Err(CheckedError::UnsupportedSettings)
        ));
        let mut with_dependency = launch();
        with_dependency.artifact =
            ToolchainArtifact::new(artifact.executable().clone(), vec![artifact.identity()]);
        assert!(matches!(
            probe_basis_checked(with_dependency, Dialect::Sqlite, Engine::Syntaqlite4e4de76),
            Err(CheckedError::UnsupportedSettings)
        ));
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        assert!(probe_basis_checked(launch(), Dialect::Sqlite, Engine::Syntaqlite4e4de76).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn protocol_rejects_unknown_fields_and_wrong_version() {
        let invalid = br#"{"version":1,"source":[],"settings":"ProcessDefaults","context":null,"extra":true}"#;
        assert!(serde_json::from_slice::<Request>(invalid).is_err());
        let req = Request {
            expected_basis: None,
            version: 3,
            source: b"SELECT 1".to_vec(),
            settings: Settings::ProcessDefaults,
            context: None,
        };
        let result = call_worker(
            std::path::Path::new("/nonexistent"),
            &req,
            Dialect::Postgres,
            Engine::Pgrust38634b4,
        );
        assert_eq!(result.syntax, Syntax::InitializationUnavailable);
    }
    #[test]
    fn rejects_forged_coverage_and_binding() {
        use sha2::{Digest, Sha256};
        let path =
            std::env::temp_dir().join(format!("nudox-sql-forged-worker-{}", std::process::id()));
        let req = Request {
            expected_basis: None,
            version: PROTOCOL_VERSION,
            source: b"SELECT 1".to_vec(),
            settings: Settings::ProcessDefaults,
            context: None,
        };
        let forged = Response {
            version: PROTOCOL_VERSION,
            basis: None,
            dialect: Dialect::Postgres,
            engine: Engine::Pgrust38634b4,
            content_sha256: Sha256::digest(&req.source).into(),
            syntax: Syntax::Parsed {
                statements: 1,
                identifiers: vec![],
                identifier_coverage: IdentifierCoverage::Unavailable,
            },
            binding: Binding::UnsupportedContext,
            // A valid outline, so the rejection comes from the forged field under test.
            outline: Some(Outline { coverage: OutlineCoverage::Complete, statements: vec![] }),
        };
        let text = serde_json::to_string(&forged).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{}'\n", text)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let response = call_worker(&path, &req, Dialect::Postgres, Engine::Pgrust38634b4);
        std::fs::remove_file(path).unwrap();
        assert_eq!(response.syntax, Syntax::InitializationUnavailable);
    }
    #[test]
    fn rejects_duplicate_read_occurrence() {
        use sha2::{Digest, Sha256};
        let path =
            std::env::temp_dir().join(format!("nudox-sql-duplicate-worker-{}", std::process::id()));
        let payload = b"catalog".to_vec();
        let req = Request {
            expected_basis: None,
            version: PROTOCOL_VERSION,
            source: b"SELECT id".to_vec(),
            settings: Settings::ProcessDefaults,
            context: Some(SemanticContext::Proposed {
                dialect: Dialect::Postgres,
                payload: payload.clone(),
            }),
        };
        let extent = Extent { start: 7, end: 9 };
        let read = NativeRead {
            relation_oid: 20001,
            attribute: 1,
            extent: extent.clone(),
        };
        let forged = Response {
            version: PROTOCOL_VERSION,
            basis: None,
            dialect: Dialect::Postgres,
            engine: Engine::Pgrust38634b4,
            content_sha256: Sha256::digest(&req.source).into(),
            syntax: Syntax::Parsed {
                statements: 1,
                identifiers: vec![Identifier {
                    extent,
                    spelling: "id".into(),
                }],
                identifier_coverage: IdentifierCoverage::NativeIdentTokenSubset,
            },
            binding: Binding::ProposedPartialReads {
                context_sha256: Sha256::digest(&payload).into(),
                reads: vec![read.clone(), read],
            },
            // A valid outline, so the rejection comes from the forged field under test.
            outline: Some(Outline { coverage: OutlineCoverage::Complete, statements: vec![] }),
        };
        let text = serde_json::to_string(&forged).unwrap();
        std::fs::write(&path, format!("#!/bin/sh\nprintf '%s' '{}'\n", text)).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = call_worker(&path, &req, Dialect::Postgres, Engine::Pgrust38634b4);
        std::fs::remove_file(path).unwrap();
        assert_eq!(result.syntax, Syntax::InitializationUnavailable);
    }
    #[test]
    fn stuck_worker_is_a_typed_timeout() {
        let path =
            std::env::temp_dir().join(format!("nudox-sql-stuck-worker-{}", std::process::id()));
        std::fs::write(&path, "#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let req = Request {
            expected_basis: None,
            version: PROTOCOL_VERSION,
            source: b"SELECT 1".to_vec(),
            settings: Settings::ProcessDefaults,
            context: None,
        };
        let response = call_worker_with_timeout(
            &path,
            &req,
            Dialect::Postgres,
            Engine::Pgrust38634b4,
            Duration::from_millis(100),
        );
        std::fs::remove_file(path).unwrap();
        assert_eq!(response.syntax, Syntax::TimedOut);
        assert_eq!(response.binding, Binding::NotAttempted);
    }
}

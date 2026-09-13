//! CLI transport-independent execution and presentation.

use crate::{
    CertifiedCommandTransport, ClientError, Command, CommandDto, CommandTransport, LocalEngine,
    MAX_FRAME, ReplyDto, admit_reply, admit_reply_with_capability, admit_request,
};
use backend_library::{
    CapabilityFamily, CapabilityLifecycle, CapabilityStatus, CapabilityTarget,
    CapabilityUnavailable, CommandFailure, CommandReply, Coverage, CoverageCapability, Document,
    GraphQueryPage, GraphValue, HealthReport, Lane, LanguageOracleTask, OutlineExtent, OutlineNode,
    PageTerminal, ProjectionPage, Reason, Row, RowId, RowState, SourceAvailability, SourceExcerpt,
    SourceExcerptExtent, ViewRoot, ViewSnapshot,
};
use std::fmt::Write as _;

/// Executes one command using an injected transport.
///
/// # Errors
///
/// Returns the transport, protocol, admission, or reply validation error from
/// the injected command transport.
pub fn execute_with_transport(
    transport: &mut impl CommandTransport,
    command: Command,
) -> Result<ReplyDto, ClientError> {
    let request = CommandDto::new(1, command);
    execute_dto_with_transport(transport, &request)
}

/// Executes one correlated DTO using an injected transport.
///
/// # Errors
///
/// Returns the transport, protocol, admission, or reply validation error from
/// the injected command transport.
pub fn execute_dto_with_transport(
    transport: &mut impl CommandTransport,
    request: &CommandDto,
) -> Result<ReplyDto, ClientError> {
    admit_request(request)?;
    let reply = transport.request(request.clone())?;
    admit_reply(request, reply)
}

/// Executes one correlated DTO through a transport that explicitly admits
/// producer certificates and optional complete-view coverage.
///
/// # Errors
///
/// Returns the transport, protocol, certificate, coverage, or reply
/// validation error from the transport boundary.
pub fn execute_dto_with_transport_with_certificate(
    transport: &mut impl CertifiedCommandTransport,
    request: &CommandDto,
    capability: Option<CoverageCapability>,
) -> Result<ReplyDto, ClientError> {
    admit_request(request)?;
    let reply = transport.request_with_certificate(request.clone(), capability.clone())?;
    admit_reply_with_capability(request, reply, capability)
}

/// Formats one transport-backed command result.
///
/// # Errors
///
/// Returns the transport, protocol, admission, or reply validation error from
/// the injected command transport.
pub fn run_with_transport(
    transport: &mut impl CommandTransport,
    command: Command,
    json: bool,
) -> Result<String, ClientError> {
    let reply = execute_with_transport(transport, command)?;
    Ok(if json {
        run_json(&reply)
    } else {
        format_human(&reply)
    })
}

/// Executes one command through the compatibility in-process seam.
#[must_use]
pub fn execute(engine: &mut impl LocalEngine, command: Command) -> ReplyDto {
    execute_dto(engine, CommandDto::new(1, command))
}

/// Executes one already correlated DTO through the compatibility seam.
#[must_use]
pub fn execute_dto(engine: &mut impl LocalEngine, request: CommandDto) -> ReplyDto {
    let request_id = request.request_id;
    let mut transport = crate::InProcessTransport::new(engine);
    transport
        .request(request)
        .unwrap_or_else(|error| ReplyDto::error(request_id, error.to_string()))
}

/// Formats one compatibility in-process result.
#[must_use]
pub fn run(engine: &mut impl LocalEngine, command: Command, json: bool) -> String {
    let reply = execute(engine, command);
    if json {
        run_json(&reply)
    } else {
        format_human(&reply)
    }
}

/// Serializes one reply DTO using the shared versioned wire schema.
#[must_use]
pub fn run_json(reply: &ReplyDto) -> String {
    if crate::protocol::preflight_reply_memory(reply).is_err() {
        return serde_json::json!({
            "version": backend_library::protocol_version(),
            "request_id": reply.request_id,
            "reply": { "kind": "error", "data": { "message": "reply encoding failed or exceeded frame bound" } }
        })
        .to_string();
    }
    match serde_json::to_string(reply) {
        Ok(encoded) if encoded.len() <= MAX_FRAME => encoded,
        _ => serde_json::json!({
            "version": backend_library::protocol_version(),
            "request_id": reply.request_id,
            "reply": { "kind": "error", "data": { "message": "reply encoding failed or exceeded frame bound" } }
        })
        .to_string(),
    }
}

/// Emits a compact human-readable reply projection.
#[must_use]
pub fn format_human(reply: &ReplyDto) -> String {
    let mut formatted = String::new();
    match &reply.reply {
        CommandReply::Surface(surface) => {
            match serde_json::to_string_pretty(surface) {
                Ok(value) => formatted.push_str(&value),
                Err(error) => {
                    let _ = writeln!(formatted, "Error: surface reply encoding failed: {error}");
                }
            }
            formatted.push('\n');
        }
        CommandReply::Packages(snapshot) => {
            let rows = snapshot.root.rows();
            for row in rows
                .iter()
                .filter(|row| matches!(row.id, RowId::Package(_)))
            {
                let _ = writeln!(formatted, "{}", row.label);
            }
            if formatted.is_empty() {
                format_empty_snapshot(&mut formatted, snapshot, "No indexed projects");
            }
        }
        CommandReply::ProjectionPage(page) => format_projection_page(&mut formatted, page),
        CommandReply::GraphQueryPage(page) => format_graph_query_page(&mut formatted, page),
        CommandReply::Added(intent) => {
            let id = backend_library::encode_id(intent.as_bytes());
            let _ = writeln!(formatted, "Index request accepted · {}", &id[..12]);
            formatted.push_str("Check `backend health` for source readiness.\n");
        }
        CommandReply::Removed(intent) => {
            let id = backend_library::encode_id(intent.as_bytes());
            let _ = writeln!(formatted, "Remove request accepted · {}", &id[..12]);
            formatted.push_str("Check `backend health` for source readiness.\n");
        }
        CommandReply::Document(document) | CommandReply::Page(document) => {
            format_document(&mut formatted, document);
        }
        CommandReply::Outline(outline) => {
            for root in outline.roots() {
                format_outline(&mut formatted, root, 0);
            }
            let extent = match outline.extent {
                OutlineExtent::Complete => "complete",
                OutlineExtent::Truncated => "truncated",
            };
            let _ = writeln!(formatted, "Outline · {extent}");
        }
        CommandReply::Names(snapshot)
        | CommandReply::Search(snapshot)
        | CommandReply::Graph(snapshot) => format_rows(&mut formatted, snapshot),
        CommandReply::Resolved(rows) => format_row_slice(&mut formatted, rows),
        CommandReply::Health(root) => {
            let (projects, symbols) =
                root.rows()
                    .iter()
                    .fold((0usize, 0usize), |counts, row| match row.id {
                        RowId::Package(_) => (counts.0.saturating_add(1), counts.1),
                        RowId::Symbol(_) => (counts.0, counts.1.saturating_add(1)),
                        RowId::Object(_) => counts,
                    });
            let revision = backend_library::encode_id(root.root().as_bytes());
            let _ = writeln!(
                formatted,
                "{} · {projects} project(s) · {symbols} declaration(s) · {}",
                readiness(root),
                &revision[..12]
            );
            for coverage in root.coverage() {
                let _ = writeln!(formatted, "  coverage · {}", format_coverage(*coverage));
            }
            let sources = source_counts(root.rows());
            let _ = writeln!(
                formatted,
                "  sources · {} captured · {} not captured · {} not hydrated · {} unconfigured",
                sources.0, sources.1, sources.2, sources.3
            );
        }
        CommandReply::Readiness(report) => format_readiness(&mut formatted, report),
        CommandReply::Revision(receipt) => {
            let _ = writeln!(
                formatted,
                "revision {} · sequence {}",
                backend_library::encode_id(receipt.root().as_bytes()),
                receipt.cursor().sequence()
            );
        }
        CommandReply::Error(message) => {
            let _ = writeln!(formatted, "Error: {message}");
        }
        CommandReply::Failed(failure) => {
            let _ = writeln!(
                formatted,
                "Error [{}]: {failure}",
                command_failure_kind(failure)
            );
        }
    }
    bound_human(formatted)
}

fn format_graph_query_page(output: &mut String, page: &GraphQueryPage) {
    for row in &page.rows {
        let mut separator = "";
        for (name, value) in row.fields() {
            let _ = write!(output, "{separator}{name}={}", format_graph_value(value));
            separator = " · ";
        }
        output.push('\n');
    }
    let terminal = match page.terminal {
        PageTerminal::Complete => "complete",
        PageTerminal::More(_) => "more available",
        PageTerminal::Cancelled => "cancelled",
    };
    let _ = writeln!(output, "Graph query · {terminal}");
}

fn format_graph_value(value: &GraphValue) -> String {
    match value {
        GraphValue::Null => "null".to_owned(),
        GraphValue::Boolean(value) => value.to_string(),
        GraphValue::Signed(value) => value.to_string(),
        GraphValue::Unsigned(value) => value.to_string(),
        GraphValue::Float(_) => value
            .as_float()
            .map_or_else(|| "invalid float".to_owned(), |value| value.to_string()),
        GraphValue::String(value) => value.clone(),
        GraphValue::List(values) => format!(
            "[{}]",
            values
                .iter()
                .map(format_graph_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn format_document(output: &mut String, document: &Document) {
    output.push_str(&document.text());
    if !output.ends_with('\n') {
        output.push('\n');
    }
    match &document.location {
        SourceAvailability::Captured(source) => {
            let _ = writeln!(output, "Source · {}:{}", source.path(), source.start_line());
        }
        SourceAvailability::NotCaptured => output.push_str("Source · not captured\n"),
        SourceAvailability::NotHydrated => output.push_str("Source · not hydrated\n"),
        SourceAvailability::Unconfigured => output.push_str("Source · unconfigured\n"),
    }
    match &document.excerpt {
        SourceExcerpt::Captured { text, extent } => {
            let extent = match extent {
                SourceExcerptExtent::Complete => "complete",
                SourceExcerptExtent::Truncated => "truncated",
            };
            let _ = writeln!(output, "Source excerpt · {extent}");
            output.push_str(text);
            if !output.ends_with('\n') {
                output.push('\n');
            }
        }
        SourceExcerpt::NotCaptured => output.push_str("Source excerpt · not captured\n"),
        SourceExcerpt::NotHydrated => output.push_str("Source excerpt · not hydrated\n"),
        SourceExcerpt::Unconfigured => output.push_str("Source excerpt · unconfigured\n"),
    }
}

fn format_rows(output: &mut String, snapshot: &ViewSnapshot) {
    format_row_slice(output, snapshot.root.rows());
    if output.is_empty() {
        format_empty_snapshot(output, snapshot, "No matches");
    }
}

fn format_projection_page(output: &mut String, page: &ProjectionPage) {
    format_row_slice(output, page.snapshot.root.rows());
    if output.is_empty() {
        format_empty_snapshot(output, &page.snapshot, "No rows");
    }
    match page.terminal {
        PageTerminal::Complete => output.push_str("Page · complete\n"),
        PageTerminal::More(_) => output.push_str("Page · more available\n"),
        PageTerminal::Cancelled => output.push_str("Page · cancelled\n"),
    }
}

fn format_empty_snapshot(output: &mut String, snapshot: &ViewSnapshot, empty: &str) {
    match readiness(&snapshot.root) {
        "Unavailable" => output.push_str("Results unavailable.\n"),
        "Indexing" => output.push_str("Results incomplete while indexing.\n"),
        _ => {
            let _ = writeln!(output, "{empty}.");
        }
    }
    for coverage in snapshot.root.coverage() {
        let _ = writeln!(output, "  coverage · {}", format_coverage(*coverage));
    }
}

fn format_row_slice(output: &mut String, rows: &[Row]) {
    for row in rows {
        let state = match row.state {
            RowState::Ready => "ready",
            RowState::Loading => "loading",
            RowState::Failed => "failed",
        };
        let _ = writeln!(output, "{} · {state}", row.label);
        if let Some(signature) = &row.signature {
            let _ = writeln!(output, "  {signature}");
        }
        match &row.source {
            SourceAvailability::Captured(source) => {
                let _ = writeln!(
                    output,
                    "  source · {}:{}",
                    source.path(),
                    source.start_line()
                );
            }
            SourceAvailability::NotCaptured => output.push_str("  source · not captured\n"),
            SourceAvailability::NotHydrated => output.push_str("  source · not hydrated\n"),
            SourceAvailability::Unconfigured => output.push_str("  source · unconfigured\n"),
        }
    }
}

fn source_counts(rows: &[Row]) -> (usize, usize, usize, usize) {
    rows.iter()
        .fold((0, 0, 0, 0), |counts, row| match &row.source {
            SourceAvailability::Captured(_) => {
                (counts.0.saturating_add(1), counts.1, counts.2, counts.3)
            }
            SourceAvailability::NotCaptured => {
                (counts.0, counts.1.saturating_add(1), counts.2, counts.3)
            }
            SourceAvailability::NotHydrated => {
                (counts.0, counts.1, counts.2.saturating_add(1), counts.3)
            }
            SourceAvailability::Unconfigured => {
                (counts.0, counts.1, counts.2, counts.3.saturating_add(1))
            }
        })
}

fn readiness(root: &ViewRoot) -> &'static str {
    coverage_readiness(
        root.coverage(),
        root.rows().iter().any(|row| row.state == RowState::Failed),
        root.rows().iter().any(|row| row.state == RowState::Loading),
    )
}

fn coverage_readiness(coverage: &[Coverage], failed: bool, loading: bool) -> &'static str {
    if coverage
        .iter()
        .any(|coverage| matches!(coverage, Coverage::Unavailable { .. }))
        || failed
    {
        "Unavailable"
    } else if coverage
        .iter()
        .any(|coverage| matches!(coverage, Coverage::Partial { .. }))
        || loading
    {
        "Indexing"
    } else if !coverage.is_empty() && coverage.iter().all(|coverage| coverage.is_complete()) {
        "Ready"
    } else {
        "Unknown"
    }
}

fn format_readiness(output: &mut String, report: &HealthReport) {
    let revision = backend_library::encode_id(report.revision().root().as_bytes());
    let _ = writeln!(
        output,
        "{} · {} row(s) · {} · sequence {}",
        coverage_readiness(report.coverage(), false, false),
        report.row_count(),
        &revision[..12],
        report.revision().cursor().sequence()
    );
    for coverage in report.coverage() {
        let _ = writeln!(output, "  coverage · {}", format_coverage(*coverage));
    }
    for capability in report.capabilities().as_slice() {
        format_capability(output, capability);
    }
}

fn format_capability(output: &mut String, status: &CapabilityStatus) {
    let id = backend_library::encode_id(&status.id().as_bytes());
    let family = match status.family() {
        CapabilityFamily::StructuralFrontend { profile } => {
            format!("structural_frontend/{profile:?}")
        }
        CapabilityFamily::LanguageOracle { profile, task } => {
            let task = language_task_name(task);
            format!("language_oracle/{profile:?}/{task}")
        }
        CapabilityFamily::Embedding { recipe } => recipe.map_or_else(
            || "embedding/unconfigured".to_owned(),
            |recipe| {
                format!(
                    "embedding/{}",
                    backend_library::encode_id(&recipe.as_bytes())
                )
            },
        ),
    };
    let lifecycle = match status.lifecycle() {
        CapabilityLifecycle::Unavailable(reason) => {
            format!("unavailable/{}", capability_unavailable_name(reason))
        }
        CapabilityLifecycle::Probing => "probing".to_owned(),
        CapabilityLifecycle::Installed => "installed".to_owned(),
        CapabilityLifecycle::Resident => "resident".to_owned(),
        CapabilityLifecycle::Active => "active".to_owned(),
        CapabilityLifecycle::Ready => "ready".to_owned(),
        CapabilityLifecycle::Revoked => "revoked".to_owned(),
    };
    let target = status.target().map_or_else(
        || "no target".to_owned(),
        |target| match target {
            CapabilityTarget::Native { os, architecture } => {
                format!("native {os}/{architecture}")
            }
            CapabilityTarget::Managed(runtime) => format!("managed {runtime}"),
        },
    );
    let abi = status
        .protocol_abi()
        .map_or_else(|| "no ABI".to_owned(), |abi| format!("ABI {abi}"));
    let manifest = status.manifest().map_or_else(
        || "no manifest".to_owned(),
        |manifest| backend_library::encode_id(&manifest),
    );
    let _ = writeln!(
        output,
        "  capability · {} · {family} · {lifecycle} · {target} · {abi} · {manifest}",
        &id[..12]
    );
}

const fn language_task_name(task: LanguageOracleTask) -> &'static str {
    match task {
        LanguageOracleTask::Parse => "parse",
        LanguageOracleTask::TypeCheck => "type_check",
        LanguageOracleTask::SemanticIndex => "semantic_index",
    }
}

const fn capability_unavailable_name(reason: CapabilityUnavailable) -> &'static str {
    match reason {
        CapabilityUnavailable::NoManifest => "no_manifest",
        CapabilityUnavailable::NotInstalled => "not_installed",
        CapabilityUnavailable::UnsupportedTarget => "unsupported_target",
        CapabilityUnavailable::UnsupportedAbi => "unsupported_abi",
        CapabilityUnavailable::MissingDependency => "missing_dependency",
        CapabilityUnavailable::ProbeFailed => "probe_failed",
    }
}

fn format_coverage(coverage: Coverage) -> String {
    match coverage {
        Coverage::Complete => "complete".to_owned(),
        Coverage::Partial {
            lane,
            completed,
            total,
        } => format!("{} partial ({completed}/{total})", lane_name(lane)),
        Coverage::Unavailable { lane, reason } => {
            format!("{} unavailable ({})", lane_name(lane), reason_name(reason))
        }
    }
}

const fn lane_name(lane: Lane) -> &'static str {
    match lane {
        Lane::Exact => "exact",
        Lane::Names => "names",
        Lane::Graph => "graph",
        Lane::Semantic => "semantic",
    }
}

const fn reason_name(reason: Reason) -> &'static str {
    match reason {
        Reason::NoIndex => "no_index",
        Reason::Unconfigured => "unconfigured",
        Reason::Offline => "offline",
        Reason::Cancelled => "cancelled",
        Reason::Incomplete => "incomplete",
    }
}

const fn command_failure_kind(failure: &CommandFailure) -> &'static str {
    match failure {
        CommandFailure::NotFound => "not_found",
        CommandFailure::WrongBasis { .. } => "wrong_basis",
        CommandFailure::InvalidQuery(_) => "invalid_query",
        CommandFailure::CursorMismatch => "cursor_mismatch",
        CommandFailure::IncoherentView(_) => "incoherent_view",
        CommandFailure::SequenceOverflow => "sequence_overflow",
        CommandFailure::MutationRequiresOwner => "mutation_requires_owner",
    }
}

fn format_outline(output: &mut String, node: &OutlineNode, depth: usize) {
    let id = backend_library::encode_id(node.symbol.as_bytes());
    let _ = writeln!(output, "{}{}", "  ".repeat(depth.min(32)), &id[..12]);
    for child in &node.children {
        format_outline(output, child, depth.saturating_add(1));
    }
}

fn bound_human(formatted: String) -> String {
    if formatted.len() <= MAX_FRAME {
        return formatted;
    }
    let mut end = MAX_FRAME.saturating_sub(1);
    while end > 0 && !formatted.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = formatted[..end].to_owned();
    bounded.push('\n');
    bounded
}

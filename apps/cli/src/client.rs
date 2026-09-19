//! CLI transport-independent execution and presentation.

use crate::{
    CertifiedCommandTransport, ClientError, Command, CommandDto, CommandTransport, LocalEngine,
    MAX_FRAME, ReplyDto, admit_reply, admit_reply_with_capability, admit_request,
};
use backend_library::{CommandReply, CoverageCapability, OutlineNode, Row, RowId, ViewSnapshot};
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
        CommandReply::Packages(snapshot) => {
            let rows = snapshot.root.rows();
            for row in rows
                .iter()
                .filter(|row| matches!(row.id, RowId::Package(_)))
            {
                let _ = writeln!(formatted, "{}", row.label);
            }
            if formatted.is_empty() {
                formatted.push_str("No indexed projects. Run `backend-cli index`.\n");
            }
        }
        CommandReply::Added(intent) => {
            let id = backend_library::encode_id(intent.as_bytes());
            let _ = writeln!(formatted, "Indexed · {}", &id[..12]);
        }
        CommandReply::Removed(intent) => {
            let id = backend_library::encode_id(intent.as_bytes());
            let _ = writeln!(formatted, "Removed · {}", &id[..12]);
        }
        CommandReply::Document(document) | CommandReply::Page(document) => {
            formatted.push_str(&document.text());
            if !formatted.ends_with('\n') {
                formatted.push('\n');
            }
        }
        CommandReply::Outline(outline) => format_outline(&mut formatted, &outline.root, 0),
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
                "Ready · {projects} project(s) · {symbols} declaration(s) · {}",
                &revision[..12]
            );
        }
        CommandReply::Revision(receipt) => {
            println!(
                "revision {} · sequence {}",
                backend_library::encode_id(receipt.root().as_bytes()),
                receipt.cursor().sequence()
            );
        }
        CommandReply::Error(message) => {
            let _ = writeln!(formatted, "Error: {message}");
        }
    }
    bound_human(formatted)
}

fn format_rows(output: &mut String, snapshot: &ViewSnapshot) {
    format_row_slice(output, snapshot.root.rows());
    if output.is_empty() {
        output.push_str("No matches.\n");
    }
}

fn format_row_slice(output: &mut String, rows: &[Row]) {
    for row in rows {
        let _ = writeln!(output, "{}", row.label);
        if let Some(signature) = &row.signature {
            let _ = writeln!(output, "  {signature}");
        }
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

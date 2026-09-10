//! Defines server behavior for `interface-mcp`, whose purpose is to serve the one shared local library to agents over MCP.
//! This module owns the server invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! One decoded method dispatched to one [`interface_library::Library::execute`] and back as Markdown.
//!
//! The server holds no cache and no session state beyond the identity of the request it is
//! currently serving. Every read reaches the library, because another process may have added a
//! package since the last one, and a surface that answers from memory is a surface that is wrong
//! exactly when it matters.

use std::{
    io::{self, Write},
    time::{SystemTime, UNIX_EPOCH},
};

use interface_library::{
    AddProgress, Library, Reply, Timestamp,
    render::{common::RenderContext, markdown},
};
use interface_protocol::mcp::{
    self, JsonRpcCode, McpEnvelope, McpFault, McpMethod, initialize_result, resource_contents_result,
    resources_list_result, success_response, tool_text_result, tools_list_result,
};
use serde_json::Value;

use crate::{arguments, card, progress, resources, tools};

/// Name this server reports at handshake.
pub const SERVER_NAME: &str = "nudox";

/// One MCP session over one library.
pub struct Server {
    library: Library,
    /// Identity of the request being served right now, so a cancellation can name it.
    active: Option<Value>,
}

impl Server {
    /// Opens a session over one already-opened library.
    #[must_use]
    pub const fn new(library: Library) -> Self {
        Self {
            library,
            active: None,
        }
    }

    /// Handles one decoded request, writing any progress notifications to `output` as they occur.
    ///
    /// Returns the response body to frame, or `None` for a notification, which JSON-RPC forbids
    /// answering.
    ///
    /// # Errors
    ///
    /// Returns the writer's I/O error when a progress notification cannot be emitted.
    pub fn handle(
        &mut self,
        envelope: McpEnvelope,
        output: &mut impl Write,
    ) -> io::Result<Option<Value>> {
        let McpEnvelope { id, method } = envelope;
        if let McpMethod::Cancelled { request_id } = &method {
            self.cancel(request_id);
            return Ok(None);
        }
        if matches!(method, McpMethod::Initialized) {
            return Ok(None);
        }
        let Some(id) = id else {
            // Every remaining method is a request; without an id it is a notification this server
            // has no side effect for, and answering it would violate JSON-RPC.
            return Ok(None);
        };
        self.active = Some(id.clone());
        let response = self.respond(&id, method, output)?;
        self.active = None;
        Ok(Some(response))
    }

    fn respond(
        &self,
        id: &Value,
        method: McpMethod,
        output: &mut impl Write,
    ) -> io::Result<Value> {
        Ok(match method {
            McpMethod::Initialize { protocol_version } => success_response(
                id,
                initialize_result(
                    mcp::negotiate_version(protocol_version.as_deref()),
                    SERVER_NAME,
                    env!("CARGO_PKG_VERSION"),
                    tools::INSTRUCTIONS,
                ),
            ),
            McpMethod::ListTools => {
                success_response(id, tools_list_result(&tools::descriptors()))
            }
            McpMethod::ListResources => {
                success_response(id, resources_list_result(&resources::descriptors()))
            }
            McpMethod::ReadResource { uri } => match resources::read(&self.library, &uri, &context())
            {
                Ok(text) => success_response(
                    id,
                    resource_contents_result(&uri, "text/markdown", &text),
                ),
                Err(message) => mcp::error_response(id, JsonRpcCode::InvalidParams, &message),
            },
            McpMethod::Ping => success_response(id, serde_json::json!({})),
            McpMethod::CallTool {
                name,
                arguments,
                progress_token,
            } => success_response(
                id,
                self.call(&name, &arguments, progress_token.as_ref(), output)?,
            ),
            McpMethod::Unknown { method } => mcp::error_response(
                id,
                JsonRpcCode::MethodNotFound,
                &format!("`{method}` is not implemented by this server"),
            ),
            // Handled before dispatch; listed so a new method cannot be forgotten here.
            McpMethod::Initialized | McpMethod::Cancelled { .. } => {
                mcp::error_response(id, JsonRpcCode::InvalidRequest, "notification carried an id")
            }
        })
    }

    /// Runs one tool call. A typed failure is content with `isError`, never a JSON-RPC error:
    /// the operand and the next call belong in the reader's hands, not in a protocol code.
    fn call(
        &self,
        name: &str,
        arguments: &serde_json::Map<String, Value>,
        progress_token: Option<&Value>,
        output: &mut impl Write,
    ) -> io::Result<Value> {
        let command = match arguments::decode(name, arguments) {
            Ok(command) => command,
            Err(fault) => return Ok(tool_text_result(&markdown::render_fault(&fault), true)),
        };
        let mut emitted = io::Result::Ok(());
        let reply = {
            let mut report = |signal: AddProgress| {
                if let (Some(token), Ok(())) = (progress_token, &emitted) {
                    emitted = write_notification(output, &progress::notification(token, signal));
                }
            };
            self.library.execute(command, &mut report)
        };
        emitted?;
        let rendered = markdown::render(&reply, &context());
        Ok(tool_text_result(&rendered, is_failure(&reply)))
    }

    /// Requests cancellation only for the request this process is serving.
    ///
    /// This server serves one request at a time on one thread, so a cancellation for any other
    /// identity — including one already answered — is ignored, which is exactly what the
    /// specification requires. Nothing is fabricated to make the notification look effective.
    fn cancel(&self, request_id: &Value) {
        if self.active.as_ref() == Some(request_id) {
            self.library.cancel_active();
        }
    }
}

/// Whether one reply is a failure the client should see flagged.
fn is_failure(reply: &Reply) -> bool {
    match reply {
        Reply::Packages(result) => result.is_err(),
        Reply::Page(result) => result.is_err(),
        Reply::Outline(result) => result.is_err(),
        Reply::Resolved(result) => result.is_err(),
        Reply::Graphed(result) => result.is_err(),
        Reply::Added(outcome) => !matches!(outcome, interface_library::AddOutcome::Ready { .. }),
        // A search with every lane down is still an honest answer, and its `~lanes` line says so;
        // a remove that found nothing changed nothing; health always answers.
        Reply::Removed(_) | Reply::Searched(_) | Reply::Health(_) => false,
    }
}

fn write_notification(output: &mut impl Write, notification: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(notification).map_err(io::Error::other)?;
    interface_protocol::write_frame_bounded(output, &body, mcp::MAX_RESPONSE_FRAME_BYTES)
}

/// The reading instant every render in this process shares.
fn context() -> RenderContext {
    RenderContext {
        now: Timestamp(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_secs()),
        ),
    }
}

/// Projects a transport-level decode failure onto its JSON-RPC error response.
#[must_use]
pub fn fault_response(fault: &McpFault) -> Option<Value> {
    let id = fault.id.as_ref()?;
    Some(mcp::error_response(id, fault.code, &fault.message))
}

/// The URI of the card a client should read once, quoted wherever a tool points at it.
pub const SCHEMA_CARD_URI: &str = card::SCHEMA_CARD_URI;

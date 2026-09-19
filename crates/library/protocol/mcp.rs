//! Defines mcp behavior for `backend-library`, whose purpose is to decode and project the shared application vocabulary for external transports.
//! This module owns the mcp invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The MCP JSON-RPC transport: methods in, envelopes out, and nothing about what a tool means.
//!
//! This module sits below the library, so it never sees a `Command`, a `Reply`, or a tool's
//! arguments. It decodes one frame into a [`McpMethod`] with its parameters still as JSON, and it
//! encodes results, tool content, progress, and errors. Everything a caller needs to vary — the
//! tool rows, the instructions, the resources — is passed in, so this file cannot drift out of
//! agreement with a registry it does not own.

use serde_json::{Map, Value, json};

use crate::protocol::AdapterField;

/// The MCP revision this server implements.
pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Revisions this server will speak, newest first.
///
/// Version negotiation is conformant: a client that asks for one of these gets exactly it back; a
/// client that asks for anything else — including a newer revision — gets [`MCP_PROTOCOL_VERSION`],
/// which the specification defines as the server's offer to be accepted or the connection dropped.
pub const SUPPORTED_PROTOCOL_VERSIONS: [&str; 2] = [MCP_PROTOCOL_VERSION, "2025-03-26"];

/// Largest accepted request frame.
pub const MAX_REQUEST_FRAME_BYTES: usize = 64 * 1024;
/// Largest emitted response frame.
///
/// Responses are bounded separately and far more generously than requests: an outline of a large
/// package is legitimately hundreds of kilobytes, while no honest tool call is.
pub const MAX_RESPONSE_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Negotiates one protocol revision against what this server speaks.
#[must_use]
pub fn negotiate_version(requested: Option<&str>) -> &'static str {
    let Some(requested) = requested else {
        return MCP_PROTOCOL_VERSION;
    };
    SUPPORTED_PROTOCOL_VERSIONS
        .into_iter()
        .find(|supported| *supported == requested)
        .unwrap_or(MCP_PROTOCOL_VERSION)
}

/// One decoded JSON-RPC method with its parameters still unopened.
#[derive(Clone, Debug, PartialEq)]
pub enum McpMethod {
    /// Lifecycle handshake carrying the client's requested revision.
    Initialize {
        /// Revision the client asked for, when it supplied one.
        protocol_version: Option<Box<str>>,
    },
    /// Post-handshake notification with no response.
    Initialized,
    /// Registry enumeration.
    ListTools,
    /// One tool invocation.
    CallTool {
        /// Registry name the client asked for.
        name: Box<str>,
        /// Arguments object exactly as supplied; an absent object decodes as empty.
        arguments: Map<String, Value>,
        /// Token the client wants progress notifications addressed to.
        progress_token: Option<Value>,
    },
    /// Resource enumeration.
    ListResources,
    /// One resource read.
    ReadResource {
        /// Requested URI.
        uri: Box<str>,
    },
    /// Liveness.
    Ping,
    /// Cancellation of an in-flight request.
    Cancelled {
        /// Identity of the request the client is abandoning.
        request_id: Value,
    },
    /// A method this server does not implement.
    Unknown {
        /// Exact method name, retained so the error names it.
        method: Box<str>,
    },
}

/// One decoded request: its identity and its method.
#[derive(Clone, Debug, PartialEq)]
pub struct McpEnvelope {
    /// JSON-RPC identity; `None` marks a notification, which must never be answered.
    pub id: Option<Value>,
    /// Decoded method.
    pub method: McpMethod,
}

/// A frame that could not be decoded, with whatever identity it did carry.
#[derive(Clone, Debug, PartialEq)]
pub struct McpFault {
    /// Identity to answer, when the frame supplied one.
    pub id: Option<Value>,
    /// JSON-RPC error code.
    pub code: JsonRpcCode,
    /// One line naming what was wrong with the frame.
    pub message: Box<str>,
}

/// The JSON-RPC error codes this transport emits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JsonRpcCode {
    /// The body was not JSON.
    ParseError,
    /// The body was JSON but not a JSON-RPC request.
    InvalidRequest,
    /// The method is not implemented.
    MethodNotFound,
    /// The parameters were the wrong shape.
    InvalidParams,
}

impl JsonRpcCode {
    /// The wire number for this code.
    #[must_use]
    pub const fn number(self) -> i32 {
        match self {
            Self::ParseError => -32_700,
            Self::InvalidRequest => -32_600,
            Self::MethodNotFound => -32_601,
            Self::InvalidParams => -32_602,
        }
    }

    /// The adapter field this code blames, so a rejection names a place and not just a number.
    #[must_use]
    pub const fn field(self) -> AdapterField {
        match self {
            Self::ParseError | Self::InvalidRequest => AdapterField::Request,
            Self::MethodNotFound => AdapterField::Method,
            Self::InvalidParams => AdapterField::Params,
        }
    }
}

/// Decodes one JSON-RPC frame.
///
/// # Errors
///
/// Returns the exact shape rejection with the request identity retained whenever the frame carried
/// one, so a client always learns which of its requests failed.
pub fn decode(body: &[u8]) -> Result<McpEnvelope, McpFault> {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Err(McpFault {
            id: None,
            code: JsonRpcCode::ParseError,
            message: "request body is not JSON".into(),
        });
    };
    let Some(object) = value.as_object() else {
        return Err(McpFault {
            id: None,
            code: JsonRpcCode::InvalidRequest,
            message: "request body is not a JSON-RPC object".into(),
        });
    };
    let id = object.get("id").cloned();
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpFault {
            id,
            code: JsonRpcCode::InvalidRequest,
            message: "`jsonrpc` must be exactly \"2.0\"".into(),
        });
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Err(McpFault {
            id,
            code: JsonRpcCode::InvalidRequest,
            message: "`method` is absent or not a string".into(),
        });
    };
    let params = object.get("params");
    decode_method(method, params)
        .map(|method| McpEnvelope {
            id: id.clone(),
            method,
        })
        .map_err(|message| McpFault {
            id,
            code: JsonRpcCode::InvalidParams,
            message,
        })
}

fn decode_method(method: &str, params: Option<&Value>) -> Result<McpMethod, Box<str>> {
    match method {
        "initialize" => Ok(McpMethod::Initialize {
            protocol_version: params
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str)
                .map(Into::into),
        }),
        "notifications/initialized" => Ok(McpMethod::Initialized),
        "tools/list" => Ok(McpMethod::ListTools),
        "tools/call" => decode_call(params),
        "resources/list" => Ok(McpMethod::ListResources),
        "resources/read" => match params
            .and_then(|params| params.get("uri"))
            .and_then(Value::as_str)
        {
            Some(uri) => Ok(McpMethod::ReadResource { uri: uri.into() }),
            None => Err("`uri` is absent or not a string".into()),
        },
        "ping" => Ok(McpMethod::Ping),
        "notifications/cancelled" => match params.and_then(|params| params.get("requestId")) {
            Some(request_id) => Ok(McpMethod::Cancelled {
                request_id: request_id.clone(),
            }),
            None => Err("`requestId` is absent".into()),
        },
        other => Ok(McpMethod::Unknown {
            method: other.into(),
        }),
    }
}

fn decode_call(params: Option<&Value>) -> Result<McpMethod, Box<str>> {
    let Some(params) = params else {
        return Err("`params` is absent".into());
    };
    let Some(name) = params.get("name").and_then(Value::as_str) else {
        return Err("`name` is absent or not a string".into());
    };
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(object)) => object.clone(),
        Some(_) => return Err("`arguments` is not an object".into()),
    };
    Ok(McpMethod::CallTool {
        name: name.into(),
        arguments,
        progress_token: params
            .get("_meta")
            .and_then(|meta| meta.get("progressToken"))
            .cloned(),
    })
}

/// One tool row exactly as `tools/list` publishes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDescriptor {
    /// Registry name.
    pub name: &'static str,
    /// The description the client reads; the registry sentence plus this surface's guidance.
    pub description: String,
    /// JSON Schema for the arguments object.
    pub input_schema: Value,
}

/// One resource row exactly as `resources/list` publishes it.
#[derive(Clone, Debug, PartialEq)]
pub struct ResourceDescriptor {
    /// Stable URI.
    pub uri: String,
    /// Short name.
    pub name: String,
    /// One sentence saying what reading it gives you.
    pub description: String,
    /// MIME type of the body a read returns.
    pub mime_type: &'static str,
}

/// Builds the `initialize` result.
#[must_use]
pub fn initialize_result(
    protocol_version: &str,
    server_name: &str,
    server_version: &str,
    instructions: &str,
) -> Value {
    json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "listChanged": false, "subscribe": false },
        },
        "serverInfo": { "name": server_name, "version": server_version },
        "instructions": instructions,
    })
}

/// Builds the `tools/list` result.
#[must_use]
pub fn tools_list_result(tools: &[ToolDescriptor]) -> Value {
    json!({
        "tools": tools
            .iter()
            .map(|tool| json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": tool.input_schema,
            }))
            .collect::<Vec<Value>>(),
    })
}

/// Builds the `resources/list` result.
#[must_use]
pub fn resources_list_result(resources: &[ResourceDescriptor]) -> Value {
    json!({
        "resources": resources
            .iter()
            .map(|resource| json!({
                "uri": resource.uri,
                "name": resource.name,
                "description": resource.description,
                "mimeType": resource.mime_type,
            }))
            .collect::<Vec<Value>>(),
    })
}

/// Builds the `resources/read` result for one text body.
#[must_use]
pub fn resource_contents_result(uri: &str, mime_type: &str, text: &str) -> Value {
    json!({ "contents": [{ "uri": uri, "mimeType": mime_type, "text": text }] })
}

/// Builds a `tools/call` result carrying one Markdown block.
///
/// A typed failure is content: it comes back as the same Markdown with `isError` set, so a client
/// reads the operand and the affordance rather than a protocol-level code that says nothing.
#[must_use]
pub fn tool_text_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// Wraps one result value in a JSON-RPC success response.
#[must_use]
pub fn success_response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Wraps one code and message in a JSON-RPC error response.
#[must_use]
pub fn error_response(id: &Value, code: JsonRpcCode, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code.number(),
            "message": message,
            "data": { "field": code.field() },
        },
    })
}

/// Builds one `notifications/progress` notification.
#[must_use]
pub fn progress_notification(token: &Value, progress: u64, total: u64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": "notifications/progress",
        "params": {
            "progressToken": token,
            "progress": progress,
            "total": total,
            "message": message,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_decodes_its_name_arguments_and_progress_token() {
        let body = br#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"show","arguments":{"address":"cargo:serde@1.0.196"},"_meta":{"progressToken":"t1"}}}"#;
        let envelope = decode(body).unwrap_or_else(|_| unreachable!("fixture decodes"));
        assert_eq!(envelope.id, Some(json!(7)));
        let McpMethod::CallTool {
            name,
            arguments,
            progress_token,
        } = envelope.method
        else {
            unreachable!("fixture is a tools/call");
        };
        assert_eq!(name.as_ref(), "show");
        assert_eq!(
            arguments.get("address").and_then(Value::as_str),
            Some("cargo:serde@1.0.196")
        );
        assert_eq!(progress_token, Some(json!("t1")));
    }

    #[test]
    fn version_negotiation_offers_the_server_revision_for_anything_unknown() {
        assert_eq!(negotiate_version(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate_version(Some("2025-03-26")), "2025-03-26");
        assert_eq!(negotiate_version(Some("1999-01-01")), MCP_PROTOCOL_VERSION);
        assert_eq!(negotiate_version(None), MCP_PROTOCOL_VERSION);
    }

    #[test]
    fn a_malformed_frame_keeps_the_identity_it_carried() {
        let fault = decode(br#"{"jsonrpc":"1.0","id":"a","method":"ping"}"#)
            .err()
            .unwrap_or_else(|| unreachable!("fixture is rejected"));
        assert_eq!(fault.id, Some(json!("a")));
        assert_eq!(fault.code, JsonRpcCode::InvalidRequest);
    }

    #[test]
    fn an_unknown_method_is_named_rather_than_guessed() {
        let envelope = decode(br#"{"jsonrpc":"2.0","id":1,"method":"prompts/list"}"#)
            .unwrap_or_else(|_| unreachable!("fixture decodes"));
        assert_eq!(
            envelope.method,
            McpMethod::Unknown {
                method: "prompts/list".into()
            }
        );
    }
}

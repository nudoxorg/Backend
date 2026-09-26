//! The tool table an MCP client should see.
//!
//! The command registry stays complete for the CLI. This list is the session
//! an agent needs to add a project and read it: shelf, index, search, and the
//! coordinate tools. Schemas still come from [`backend_present::GRAMMARS`], so
//! the advertised operands cannot drift from the registry row. Registry rows
//! that are not in this list stay callable by name for existing probes; they
//! are not advertised, and neither are the Trustfall or surface escape hatches.

use super::codec::empty_cursor;
use super::{RpcError, default_detail};
use backend_library::CommandDomain;
use backend_present::{
    ArgumentKind, ArgumentSpec, CommandGrammar, DEFAULT_LIMIT, DEFAULT_RESPONSE_BUDGET_BYTES,
    Detail, domain_name, encode_serializable, grammar_for_tool, oversized_fault,
};
use serde_json::{Map, Value, json};

/// The escape hatch that takes one tagged `SurfaceCommand` object.
pub(super) const SURFACE_TOOL: &str = "backend.surface";

/// The typed Trustfall lane, which is a capability rather than a registry row.
pub(super) const QUERY_TOOL: &str = "backend.query";

/// Tools advertised to an MCP client, in the order the instructions use them.
const SESSION_TOOLS: &[&str] = &[
    "backend.packages",
    "backend.index",
    "backend.remove",
    "backend.status",
    "backend.outline",
    "backend.search",
    "backend.resolve",
    "backend.document",
    "backend.source",
    "backend.read",
    "backend.references",
    "backend.graph",
];

/// Lists the session tools. `backend.index` requires `path` here even though
/// the CLI grammar treats it as optional, so a client cannot accidentally
/// index the process working directory.
pub(super) fn list_tools(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    bounded_tools()
}

fn bounded_tools() -> Result<Value, RpcError> {
    let body = tools_value();
    let payload = encode_serializable(
        "tools",
        Detail::Summary,
        None,
        body,
        DEFAULT_RESPONSE_BUDGET_BYTES,
    )
    .map_err(|error| RpcError::from_fault(&oversized_fault(error)))?;
    serde_json::from_slice(&payload.bytes)
        .map_err(|error| RpcError::tool(format!("typed tools projection decode failed: {error}")))
}

fn tools_value() -> Value {
    let mut tools = Vec::with_capacity(SESSION_TOOLS.len());
    for name in SESSION_TOOLS {
        let grammar = grammar_for_tool(name)
            .unwrap_or_else(|| panic!("session tool {name} is not a registry row"));
        let domain = grammar
            .spec()
            .map(|spec| spec.domain)
            .unwrap_or(CommandDomain::Library);
        tools.push(registry_tool(grammar, domain));
    }
    json!({ "tools": tools })
}

/// Returns the canonical `tools/list` projection for the offline budget
/// fixture generator. This is kept at the protocol edge so the generator
/// cannot accidentally grow a second hand-written tool schema.
pub(crate) fn token_budget_tools() -> Value {
    tools_value()
}

/// Projects one registry row into one MCP tool definition.
fn registry_tool(grammar: CommandGrammar, domain: CommandDomain) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for spec in grammar.positional() {
        properties.insert(spec.name().to_owned(), property(*spec));
        if spec.is_required() || (grammar.tool() == "backend.index" && spec.name() == "path") {
            required.push(Value::String(spec.name().to_owned()));
        }
    }
    for spec in grammar.options() {
        properties.insert(spec.name().to_owned(), property(*spec));
    }
    // Presentation controls are shared by every read tool. Keeping them out
    // of the command grammar avoids making a presentation preference look
    // like a daemon operand while still making the accepted JSON explicit.
    properties.insert("detail".to_owned(), detail_property(grammar.tool()));
    if matches!(grammar.name(), "search" | "resolve" | "name" | "graph") {
        properties.insert(
            "cursor".to_owned(),
            json!({
                "type": "string",
                "description": "Opaque continuation returned by the preceding page."
            }),
        );
        if grammar.name() == "graph" {
            properties.insert(
                "limit".to_owned(),
                json!({
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 200,
                    "default": 25,
                    "description": "Maximum graph rows in this page."
                }),
            );
        }
    }
    let title = grammar
        .spec()
        .map_or_else(|| grammar.name(), |spec| spec.title);
    let write = grammar.is_write();
    json!({
        "name": grammar.tool(),
        "title": title,
        "description": grammar.description(),
        "inputSchema": {
            "type": "object",
            "properties": Value::Object(properties),
            "required": required,
            "additionalProperties": false
        },
        "outputSchema": answer_schema(),
        "annotations": {
            "title": title,
            "readOnlyHint": !write,
            "destructiveHint": grammar.is_destructive(),
            "idempotentHint": !write,
            "openWorldHint": false
        },
        "_meta": { "backend/domain": domain_name(domain), "backend/command": grammar.name() }
    })
}

/// Projects one operand into its JSON Schema property.
fn property(spec: ArgumentSpec) -> Value {
    let choices = spec.kind().enumeration();
    let mut scalar = json!({ "type": spec.kind().json_type() });
    if !choices.is_empty() {
        scalar["enum"] = json!(choices);
    }
    if spec.kind() == ArgumentKind::Limit {
        scalar["minimum"] = json!(1);
        scalar["maximum"] = json!(200);
        scalar["default"] = json!(DEFAULT_LIMIT);
    }
    let mut value = if spec.is_repeated() {
        json!({ "type": "array", "items": scalar, "minItems": 1 })
    } else {
        scalar
    };
    value["description"] = Value::String(spec.help().to_owned());
    value
}

/// Projects the presentation control accepted by every response-bearing MCP
/// tool. Keeping this constructor shared makes the advertised enum and its
/// default follow the same [`Detail`] parser used at call time.
fn detail_property(tool: &str) -> Value {
    let default_detail = match default_detail(tool) {
        Detail::Summary => "summary",
        Detail::Standard => "standard",
        Detail::Full => "full",
    };
    json!({
        "type": "string",
        "enum": ["summary", "standard", "full"],
        "default": default_detail,
        "description": "Response fields: summary, standard, or full."
    })
}

/// The shape every `structuredContent` takes: one tagged presentation answer.
fn answer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "answer": {
                "type": "string",
                "enum": ["page", "records", "shelf", "outline", "status", "product", "fault"],
                "description": "Which presentation answer this is; `fault` accompanies isError."
            }
        },
        "required": ["answer"],
        "additionalProperties": true
    })
}

//! The tool table, generated from the shared command registry.
//!
//! There is no hand-written tool list here and no hand-written `inputSchema`.
//! [`backend_present::GRAMMARS`] already says what every one of the thirty-five
//! registry rows takes and when to reach for it; this module projects that into
//! MCP's vocabulary. The previous generation kept thirteen tools written out by
//! hand, so twenty-two product commands were reachable only through a JSON
//! escape hatch an agent had to be told about — and the thirteen drifted from
//! the registry the moment anyone touched it. A generated table cannot drift:
//! the crate's tests iterate `COMMANDS` and demand a tool for every row.
//!
//! Tools are emitted grouped by [`backend_library::CommandDomain`], in the same
//! order the CLI's `--help` prints them, and each carries its domain in `_meta`
//! so a client can group them the same way. Two tools are not registry rows and
//! say so: `backend.query`, the typed Trustfall lane, and `backend.surface`,
//! the escape hatch that still takes a tagged `SurfaceCommand` verbatim.

use super::RpcError;
use super::codec::empty_cursor;
use backend_library::CommandDomain;
use backend_present::{
    ArgumentKind, ArgumentSpec, CommandGrammar, DEFAULT_LIMIT, GRAMMARS, domain_name, domains,
    grammars_in,
};
use serde_json::{Map, Value, json};

/// The escape hatch that takes one tagged `SurfaceCommand` object.
pub(super) const SURFACE_TOOL: &str = "backend.surface";

/// The typed Trustfall lane, which is a capability rather than a registry row.
pub(super) const QUERY_TOOL: &str = "backend.query";

/// Lists every registry row as one tool, grouped by domain.
pub(super) fn list_tools(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    let mut tools = Vec::with_capacity(GRAMMARS.len().saturating_add(2));
    for domain in domains() {
        for grammar in grammars_in(domain) {
            tools.push(registry_tool(grammar, domain));
        }
    }
    tools.push(query_tool());
    tools.push(surface_tool());
    Ok(json!({ "tools": tools }))
}

/// Projects one registry row into one MCP tool definition.
fn registry_tool(grammar: CommandGrammar, domain: CommandDomain) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for spec in grammar.positional() {
        properties.insert(spec.name().to_owned(), property(*spec));
        if spec.is_required() {
            required.push(Value::String(spec.name().to_owned()));
        }
    }
    for spec in grammar.options() {
        properties.insert(spec.name().to_owned(), property(*spec));
    }
    // Presentation controls are shared by every read tool. Keeping them out
    // of the command grammar avoids making a presentation preference look
    // like a daemon operand while still making the accepted JSON explicit.
    properties.insert(
        "detail".to_owned(),
        json!({
            "type": "string",
            "enum": ["summary", "standard", "full"],
            "default": "summary",
            "description": "Response projection; summary is context-efficient, full opts into every available field."
        }),
    );
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
    let title = grammar.spec().map_or_else(|| grammar.name(), |spec| spec.title);
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

fn query_tool() -> Value {
    json!({
        "name": QUERY_TOOL,
        "title": "Query the code graph",
        "description": "Run a typed Trustfall query over the current immutable revision. \
Use when a question is a join rather than a lookup — every declaration that calls one function, \
every child of a module — and read `backend://schema/query` first for the starting edges, the \
fields, and worked queries you can run unchanged.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Trustfall query text." },
                "variables": {
                    "type": "object",
                    "description": "Scalar or list query variables.",
                    "additionalProperties": true
                },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 25 },
                "cursor": { "type": "string", "description": "Opaque continuation returned by the preceding page." },
                "detail": { "type": "string", "enum": ["summary", "standard", "full"], "default": "summary" }
            },
            "required": ["query"],
            "additionalProperties": false
        },
        "outputSchema": { "type": "object", "additionalProperties": true },
        "annotations": {
            "title": "Query the code graph",
            "readOnlyHint": true,
            "destructiveHint": false,
            "idempotentHint": true,
            "openWorldHint": false
        },
        "_meta": { "backend/domain": "library" }
    })
}

fn surface_tool() -> Value {
    let operations = backend_library::COMMANDS
        .iter()
        .filter_map(|spec| spec.is_surface().then_some(spec.name))
        .collect::<Vec<_>>();
    json!({
        "name": SURFACE_TOOL,
        "title": "Product surface",
        "description": "Execute any daemon-owned typed product operation as a tagged \
SurfaceCommand object. Every operation here also has its own named tool, which validates operands \
and renders a readable answer; reach for this only when a client must pass a command through \
verbatim.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {
                    "type": "object",
                    "description": "A tagged SurfaceCommand object with an operation field and its typed operands.",
                    "properties": {
                        "operation": { "type": "string", "enum": operations }
                    },
                    "required": ["operation"],
                    "additionalProperties": true
                }
            },
            "required": ["command"],
            "additionalProperties": false
        },
        "outputSchema": { "type": "object", "additionalProperties": true },
        "annotations": {
            "title": "Product surface",
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": false,
            "openWorldHint": false
        },
        "_meta": { "backend/domain": "system" }
    })
}

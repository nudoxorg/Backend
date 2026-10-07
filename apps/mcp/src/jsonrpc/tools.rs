//! The tool table an MCP client should see.
//!
//! Each descriptor connects the route, default discovery policy, and registry
//! grammar. `tools/list` stays small and ready for a new session; the catalog
//! resource makes capability-dependent routes and the two advanced lanes
//! inspectable without promising that every workspace can answer them.

use super::codec::empty_cursor;
use super::{RpcError, default_detail};
use backend_library::CommandDomain;
use backend_present::{
    ArgumentKind, ArgumentSpec, CommandGrammar, DEFAULT_LIMIT, DEFAULT_RESPONSE_BUDGET_BYTES,
    Detail, Fault, GRAMMARS, domain_name, encode_serializable, grammar_for_tool, oversized_fault,
};
use serde_json::{Map, Value, json};
use std::fmt::Write as _;

/// The escape hatch that takes one tagged `SurfaceCommand` object.
pub(super) const SURFACE_TOOL: &str = "backend.surface";

/// The typed Trustfall lane, which is a capability rather than a registry row.
pub(super) const QUERY_TOOL: &str = "backend.query";

/// Starts one owner-managed package indexing operation.
pub(super) const INDEX_START_TOOL: &str = "backend.index_start";

/// Reads an immediate bounded observation of one owner-managed index job.
pub(super) const INDEX_PROGRESS_TOOL: &str = "backend.index_progress";

/// Requests cancellation of one exact owner-managed index job.
pub(super) const INDEX_CANCEL_TOOL: &str = "backend.index_cancel";

/// One MCP route and its source grammar. Both `tools/list` and `tools/call`
/// resolve names through this descriptor so a route cannot be advertised with
/// a schema that is disconnected from the handler which accepts it.
#[derive(Clone, Copy, Debug)]
pub(super) enum ToolRoute {
    Registry(CommandGrammar),
    Query,
    Surface,
    IndexStart,
    IndexProgress,
    IndexCancel,
    RefusedIndexAwait,
}

/// Whether a callable route belongs in the default agent tool set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Discovery {
    Listed,
    CatalogOnly,
}

#[derive(Clone, Copy, Debug)]
struct ToolDescriptor {
    name: &'static str,
    route: ToolRoute,
    discovery: Discovery,
}

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
    "backend.package",
    "backend.index_search",
];

pub(super) const INDEX_JOB_TOOLS: &[&str] =
    &[INDEX_START_TOOL, INDEX_PROGRESS_TOOL, INDEX_CANCEL_TOOL];

/// Resolves every callable tool name through its route descriptor.
pub(super) fn tool_route(name: &str) -> Option<ToolRoute> {
    descriptor(name).map(|descriptor| descriptor.route)
}

fn descriptor(name: &str) -> Option<ToolDescriptor> {
    let descriptor = match name {
        SURFACE_TOOL => ToolDescriptor {
            name: SURFACE_TOOL,
            route: ToolRoute::Surface,
            discovery: Discovery::CatalogOnly,
        },
        QUERY_TOOL => ToolDescriptor {
            name: QUERY_TOOL,
            route: ToolRoute::Query,
            discovery: Discovery::CatalogOnly,
        },
        INDEX_START_TOOL => ToolDescriptor {
            name: INDEX_START_TOOL,
            route: ToolRoute::IndexStart,
            discovery: Discovery::Listed,
        },
        INDEX_PROGRESS_TOOL => ToolDescriptor {
            name: INDEX_PROGRESS_TOOL,
            route: ToolRoute::IndexProgress,
            discovery: Discovery::Listed,
        },
        INDEX_CANCEL_TOOL => ToolDescriptor {
            name: INDEX_CANCEL_TOOL,
            route: ToolRoute::IndexCancel,
            discovery: Discovery::Listed,
        },
        "backend.index_await" => ToolDescriptor {
            name: "backend.index_await",
            route: ToolRoute::RefusedIndexAwait,
            discovery: Discovery::CatalogOnly,
        },
        _ => {
            let grammar = grammar_for_tool(name)?;
            ToolDescriptor {
                name: grammar.tool(),
                route: ToolRoute::Registry(grammar),
                discovery: if SESSION_TOOLS.contains(&grammar.tool()) {
                    Discovery::Listed
                } else {
                    Discovery::CatalogOnly
                },
            }
        }
    };
    Some(descriptor)
}

fn descriptors() -> Vec<ToolDescriptor> {
    let mut descriptors = Vec::with_capacity(GRAMMARS.len() + 2);
    descriptors.extend(GRAMMARS.iter().copied().map(|grammar| {
        descriptor(grammar.tool()).expect("every grammar row has an MCP descriptor")
    }));
    descriptors.extend([
        descriptor(QUERY_TOOL).expect("query tool descriptor"),
        descriptor(SURFACE_TOOL).expect("surface tool descriptor"),
    ]);
    descriptors
}

/// Describes every callable-but-not-default route, including its registry
/// description, so capability-dependent features remain inspectable without
/// advertising them as universally ready.
pub(super) fn catalog_markdown() -> String {
    let mut output = String::from(
        "# MCP tool catalog\n\n`tools/list` contains the default session tools. The registry routes below are callable by exact name but are not in the default set. Each input schema is the accepted JSON shape. A route can still return a typed fault when its required source metadata or product capability is unavailable; read `backend.status` and `backend://workspace/current` before treating an empty or unavailable result as evidence that the project has none.\n\n## Capability-dependent registry routes\n\n",
    );
    let descriptors = descriptors();
    for descriptor in &descriptors {
        if descriptor.discovery == Discovery::CatalogOnly
            && let ToolRoute::Registry(grammar) = descriptor.route
        {
            let domain = grammar.domain().unwrap_or(CommandDomain::Library);
            let schema = registry_tool(grammar, domain)["inputSchema"].clone();
            let schema = serde_json::to_string_pretty(&schema)
                .expect("registry input schema always serializes");
            let _ = writeln!(
                output,
                "- `{}` — {}\n\n```json\n{schema}\n```\n",
                descriptor.name,
                grammar.description()
            );
        }
    }
    output.push_str("\n## Advanced and retired routes\n\n");
    for descriptor in &descriptors {
        if descriptor.discovery != Discovery::CatalogOnly {
            continue;
        }
        match descriptor.route {
            ToolRoute::Registry(_) => {}
            ToolRoute::Query => output.push_str(
                "- `backend.query` — Typed Trustfall joins over the selected immutable revision. This advanced lane is not in the default session tool set; read `backend://schema/query` for its input contract and executable examples.\n",
            ),
            ToolRoute::Surface => output.push_str(
                "- `backend.surface` — Generic typed `SurfaceCommand` escape hatch. Pass `command` as a tagged object with an `operation` field; its accepted operations and fields are defined by `SurfaceCommand`. Prefer a matching named tool when one exists.\n",
            ),
            ToolRoute::RefusedIndexAwait => output.push_str(
                "- `backend.index_await` — Retired over MCP because it may block on owner work; use `backend.index_progress` for immediate bounded polling. Calls are refused.\n",
            ),
            ToolRoute::IndexStart | ToolRoute::IndexProgress | ToolRoute::IndexCancel => {}
        }
    }
    output
}

/// Checks JSON values against the primitive and collection shapes projected by
/// the registry schema before the compatibility `Invocation` parser converts
/// values into command-line text. Semantic validation remains in the shared
/// command lowering path.
pub(super) fn validate_registry_arguments(
    grammar: CommandGrammar,
    arguments: &Map<String, Value>,
) -> Result<(), Fault> {
    for spec in grammar.positional().iter().chain(grammar.options()) {
        let Some(value) = arguments.get(spec.json_name()) else {
            continue;
        };
        if spec.is_repeated() {
            let Some(values) = value.as_array() else {
                return Err(argument_fault(*spec, "an array"));
            };
            if values.is_empty() {
                return Err(argument_fault(*spec, "a non-empty array"));
            }
            for value in values {
                validate_scalar(*spec, value)?;
            }
        } else {
            validate_scalar(*spec, value)?;
        }
    }
    Ok(())
}

fn validate_scalar(spec: ArgumentSpec, value: &Value) -> Result<(), Fault> {
    let type_matches = match spec.kind().json_type() {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        _ => false,
    };
    if type_matches {
        Ok(())
    } else {
        let expected = if spec.kind() == ArgumentKind::Limit {
            "an integer from 1 through 200"
        } else if spec.is_repeated() {
            "an array of values with the declared type"
        } else {
            match spec.kind().json_type() {
                "integer" => "an integer",
                "boolean" => "a boolean",
                "object" => "an object",
                _ => "a string",
            }
        };
        Err(argument_fault(spec, expected))
    }
}

fn argument_fault(spec: ArgumentSpec, expected: &str) -> Fault {
    Fault::usage(
        spec.json_name(),
        format!("`{}` must be {expected}", spec.json_name()),
    )
}

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
    let mut tools = Vec::with_capacity(SESSION_TOOLS.len() + INDEX_JOB_TOOLS.len());
    for name in SESSION_TOOLS.iter().chain(INDEX_JOB_TOOLS) {
        let descriptor =
            descriptor(name).unwrap_or_else(|| panic!("listed MCP tool {name} has no descriptor"));
        assert_eq!(descriptor.discovery, Discovery::Listed);
        tools.push(listed_tool(descriptor));
    }
    json!({ "tools": tools })
}

fn listed_tool(descriptor: ToolDescriptor) -> Value {
    match descriptor.route {
        ToolRoute::Registry(grammar) => {
            let domain = grammar.domain().unwrap_or(CommandDomain::Library);
            registry_tool(grammar, domain)
        }
        ToolRoute::IndexStart => index_start_tool(),
        ToolRoute::IndexProgress => index_progress_tool(),
        ToolRoute::IndexCancel => index_cancel_tool(),
        ToolRoute::Query | ToolRoute::Surface | ToolRoute::RefusedIndexAwait => {
            panic!(
                "non-default route {} was put in tools/list",
                descriptor.name
            )
        }
    }
}

fn index_start_tool() -> Value {
    job_tool(
        INDEX_START_TOOL,
        "Start Indexing",
        "Start indexing one local package or pinned package URL and return its owner-issued ticket. The operation continues after this call returns; poll backend.index_progress with the exact ticket. Tickets are scoped to one owner process and become unknown after an owner restart.",
        json!({
            "type": "object",
            "properties": {
                "package": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Local package path or exact version-pinned package URL."
                },
                "execution_intent": {
                    "type": "string",
                    "enum": ["interactive", "background"],
                    "default": "interactive"
                },
                "detail": detail_property(INDEX_START_TOOL)
            },
            "required": ["package"],
            "additionalProperties": false
        }),
        false,
        false,
    )
}

fn index_progress_tool() -> Value {
    job_tool(
        INDEX_PROGRESS_TOOL,
        "Index Progress",
        "Read one immediate, bounded observation of an owner-issued indexing ticket. Pass the exact ticket returned by backend.index_start and the previous next_sequence; pending, terminal, and unknown-after-restart/retention are distinct states.",
        json!({
            "type": "object",
            "properties": {
                "ticket": ticket_schema(),
                "after_sequence": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Return only progress events after this sequence. Reuse next_sequence from the previous pending observation."
                },
                "detail": detail_property(INDEX_PROGRESS_TOOL)
            },
            "required": ["ticket"],
            "additionalProperties": false
        }),
        true,
        true,
    )
}

fn index_cancel_tool() -> Value {
    job_tool(
        INDEX_CANCEL_TOOL,
        "Cancel Indexing",
        "Request cancellation of the exact owner-issued indexing ticket. A requested cancellation is not terminal; poll backend.index_progress until it reports a terminal receipt. Tickets are scoped to one owner process.",
        json!({
            "type": "object",
            "properties": {
                "ticket": ticket_schema(),
                "detail": detail_property(INDEX_CANCEL_TOOL)
            },
            "required": ["ticket"],
            "additionalProperties": false
        }),
        false,
        false,
    )
}

fn job_tool(
    name: &str,
    title: &str,
    description: &str,
    input_schema: Value,
    read_only: bool,
    idempotent: bool,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": input_schema,
        "outputSchema": index_job_output_schema(),
        "annotations": {
            "title": title,
            "readOnlyHint": read_only,
            "destructiveHint": false,
            "idempotentHint": idempotent,
            "openWorldHint": false
        },
        "_meta": { "backend/domain": "library", "backend/command": name }
    })
}

fn index_job_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "surface": {
                "type": "object",
                "properties": {
                    "result": {
                        "type": "string",
                        "enum": ["index-started", "index-progress", "index-cancellation"]
                    },
                    "data": { "type": "object" },
                    "index_job": {
                        "type": "object",
                        "properties": {
                            "kind": {
                                "type": "string",
                                "enum": ["started", "progress", "cancellation"]
                            },
                            "value": { "type": "object" }
                        },
                        "required": ["kind", "value"],
                        "additionalProperties": false
                    }
                },
                "required": ["result", "data", "index_job"],
                "additionalProperties": false
            }
        },
        "required": ["surface"],
        "additionalProperties": false
    })
}

fn ticket_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "id": { "type": "integer", "minimum": 1 },
            "owner_epoch": {
                "type": "array",
                "items": { "type": "integer", "minimum": 0, "maximum": 255 },
                "minItems": 16,
                "maxItems": 16
            },
            "package": {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["purl", "local"] },
                    "value": { "type": "string", "minLength": 1 }
                },
                "required": ["kind", "value"],
                "additionalProperties": false
            }
        },
        "required": ["id", "owner_epoch", "package"],
        "additionalProperties": false
    })
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
        let mut argument = property(*spec);
        if grammar.tool() == "backend.index" && spec.name() == "path" {
            argument["minLength"] = json!(1);
        }
        properties.insert(spec.json_name().to_owned(), argument);
        if spec.is_required() || (grammar.tool() == "backend.index" && spec.name() == "path") {
            required.push(Value::String(spec.json_name().to_owned()));
        }
    }
    for spec in grammar.options() {
        properties.insert(spec.json_name().to_owned(), property(*spec));
    }
    // Presentation controls are shared by every read tool. Keeping them out
    // of the command grammar avoids making a presentation preference look
    // like a daemon operand while still making the accepted JSON explicit.
    properties.insert("detail".to_owned(), detail_property(grammar.tool()));
    if matches!(
        grammar.name(),
        "search" | "resolve" | "name" | "graph" | "index-search"
    ) {
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
    if spec.kind() == ArgumentKind::ExecutionIntent {
        scalar["default"] = json!("interactive");
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

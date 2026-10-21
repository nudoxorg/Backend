//! The tool table an MCP client should see.
//!
//! Each descriptor connects the route, default discovery policy, and registry
//! grammar. `tools/list` stays small and ready for a new session; the catalog
//! resource makes capability-dependent routes and the two advanced lanes
//! inspectable without promising that every workspace can answer them.

use super::codec::{empty_cursor, no_extra, string};
use super::{RpcError, default_detail};
use backend_library::CommandDomain;
use backend_present::{
    ArgumentKind, ArgumentSpec, CommandGrammar, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, Fault,
    GRAMMARS, domain_name, encode_serializable, grammar_for_tool, oversized_fault,
};
use serde_json::{Map, Value, json};
use std::fmt::Write as _;

/// The escape hatch that takes one tagged `SurfaceCommand` object.
pub(super) const SURFACE_TOOL: &str = "backend.surface";

/// The typed Trustfall lane, which is a capability rather than a registry row.
pub(super) const QUERY_TOOL: &str = "backend.query";

/// Compact access to registered named routes on tool-list-only hosts.
pub(super) const CALL_TOOL: &str = "backend.call";
pub(super) const CATALOG_RESOURCE_URI: &str = "backend://tools/catalog";
pub(super) const CATALOG_SCHEMA: &str = "backend-mcp.tool-catalog/v2";

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
    NamedCall,
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

/// One admitted target with its original borrowed inputs. The held descriptor
/// owns both canonical name and route; forwarding cannot pair a route with a
/// caller-supplied or separately mutable identity.
pub(super) struct NamedToolCall<'a> {
    descriptor: ToolDescriptor,
    arguments: &'a Map<String, Value>,
}

impl<'a> NamedToolCall<'a> {
    pub(super) fn admit(arguments: &'a Map<String, Value>) -> Result<Self, RpcError> {
        no_extra(arguments, &["name", "arguments"])?;
        let name = string(arguments, "name")?;
        let descriptor = descriptor(name)
            .filter(|descriptor| named_target(descriptor.route))
            .ok_or_else(|| {
                RpcError::invalid_argument(
                    "name",
                    "backend.call requires a registered named tool from its name enum; surface, recursive and unknown routes are refused",
                )
            })?;
        let arguments = arguments
            .get("arguments")
            .and_then(Value::as_object)
            .ok_or_else(|| {
                RpcError::invalid_argument(
                    "arguments",
                    "backend.call requires arguments.arguments as an object",
                )
            })?;
        Ok(Self {
            descriptor,
            arguments,
        })
    }

    pub(super) const fn name(&self) -> &'static str {
        self.descriptor.name
    }

    pub(super) const fn route(&self) -> ToolRoute {
        self.descriptor.route
    }

    pub(super) const fn arguments(&self) -> &'a Map<String, Value> {
        self.arguments
    }
}

const fn named_target(route: ToolRoute) -> bool {
    matches!(
        route,
        ToolRoute::Registry(_)
            | ToolRoute::Query
            | ToolRoute::IndexStart
            | ToolRoute::IndexProgress
            | ToolRoute::IndexCancel
            | ToolRoute::RefusedIndexAwait
    )
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
        CALL_TOOL => ToolDescriptor {
            name: CALL_TOOL,
            route: ToolRoute::NamedCall,
            discovery: Discovery::Listed,
        },
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
    let mut descriptors = Vec::with_capacity(GRAMMARS.len() + 3);
    descriptors.extend(GRAMMARS.iter().copied().map(|grammar| {
        descriptor(grammar.tool()).expect("every grammar row has an MCP descriptor")
    }));
    descriptors.extend([
        descriptor(QUERY_TOOL).expect("query tool descriptor"),
        descriptor(SURFACE_TOOL).expect("surface tool descriptor"),
        descriptor(CALL_TOOL).expect("named call descriptor"),
    ]);
    descriptors
}

/// Small complete route index. Exact schemas live at the named resource
/// linked by each entry, so one complex operand cannot hide other capabilities.
pub(super) fn catalog_markdown() -> String {
    let mut output = format!(
        "# MCP tool catalog\n\nSchema: `{CATALOG_SCHEMA}`. Resource: `{CATALOG_RESOURCE_URI}`.\n\n"
    );
    output.push_str(
        "`tools/list` contains the default session tools. Read a route's linked card for its complete input schema, result contract and help before calling it. A large card returns UTF-8 JSON text ranges: append contents[0].text in exact backend/catalogRange.offset/end order, copy result._meta[backend/nextUri] until complete, verify its unchanged version and totalBytes, then parse the full JSON. Fragments use text/plain; an unsplit card uses application/json. Every registered named handler's argument schema is retained; direct-only Surface is expert protocol guidance, not a named schema card. On a host exposing only listed tools, pass {\"name\":\"backend.<route>\",\"arguments\":{...}} to `backend.call`; put detail and cursor inside that selected arguments object. Copy `structuredContent.continuation_call` unchanged to continue an adaptive page. Preserve effective limits, typed native JSON cursors, exact source references and byte ranges.\n\nA registered route can still return a typed capability/source refusal. Read `backend.status` and `backend://workspace/current`; an unavailable or partial lane is not a complete empty answer. `backend.query` also has executable examples at `backend://schema/query`. `backend.surface` is direct-only and is refused by `backend.call`. `backend.index_await` is retired over MCP; poll `backend.index_progress` instead.\n\n## All closed routes\n\n",
    );
    for descriptor in descriptors() {
        let title = match descriptor.route {
            ToolRoute::Registry(grammar) => {
                grammar.spec().map_or(grammar.name(), |spec| spec.title)
            }
            ToolRoute::Query => "Typed graph query",
            ToolRoute::Surface => "Direct-only expert protocol guidance",
            ToolRoute::IndexStart => "Start indexing",
            ToolRoute::IndexProgress => "Saved operation progress",
            ToolRoute::IndexCancel => "Cancel indexing",
            ToolRoute::RefusedIndexAwait => "Retired MCP wait",
            ToolRoute::NamedCall => "Call registered tool",
        };
        let policy = if matches!(descriptor.route, ToolRoute::RefusedIndexAwait) {
            "retired; calls refused"
        } else if matches!(descriptor.route, ToolRoute::Surface) {
            "direct-only"
        } else if descriptor.discovery == Discovery::Listed {
            "listed"
        } else {
            "capability-dependent"
        };
        let link = if matches!(descriptor.route, ToolRoute::Surface) {
            "expert protocol guidance"
        } else {
            "schema and help"
        };
        let _ = writeln!(
            output,
            "- `{}` — {title} ({policy}); [{link}]({CATALOG_RESOURCE_URI}/{})",
            descriptor.name, descriptor.name
        );
    }
    output
}

/// Complete descriptor-driven JSON card; constructing it performs no owner
/// access and never substitutes a command-line grammar for an MCP job handler.
pub(super) fn catalog_tool(name: &str) -> Option<Value> {
    let descriptor = descriptor(name)?;
    let mut tool = match descriptor.route {
        ToolRoute::Query => query_catalog_tool(),
        ToolRoute::Surface => json!({
            "name":SURFACE_TOOL, "title":"Direct-only expert protocol guidance",
            "description":"Direct clients may send {command:<closed tagged SurfaceCommand>,detail:<summary|standard|full>}. The command carries an operation tag and its exact typed operands. This private escape hatch is not a registered named target: backend.call refuses it before native dispatch. Use the equivalent registered route's complete schema and executable continuation when available; never move a returned command cursor into an outer argument.",
            "_meta":{"backend/directOnly":true,"backend/registeredTarget":false}
        }),
        ToolRoute::RefusedIndexAwait => json!({
            "name":"backend.index_await", "title":"Retired MCP wait",
            "description":"MCP always refuses this retired route without owner work. Use backend.index_progress with the exact returned operation_key or ticket and admitted conditional-wait arguments; CLI add --wait is a separate caller lifetime.",
            "_meta":{"backend/retired":true},
            "inputSchema":{"type":"object","additionalProperties":true},
            "outputSchema":{"type":"object","description":"Typed usage refusal; this route never publishes a terminal owner outcome."}
        }),
        _ => listed_tool(descriptor),
    };
    tool["_meta"]["backend/catalog"] = json!({
        "schema":CATALOG_SCHEMA, "uri":format!("{CATALOG_RESOURCE_URI}/{name}"),
        "index":CATALOG_RESOURCE_URI, "registeredTarget":named_target(descriptor.route),
        "listed":descriptor.discovery == Discovery::Listed,
        "continuation":"Copy structuredContent.continuation_call unchanged, including exposure, effective limit, cursor, source reference and byte range."
    });
    Some(tool)
}

fn query_catalog_tool() -> Value {
    json!({
        "name":QUERY_TOOL, "title":"Typed graph query",
        "description":"Typed Trustfall joins over one selected immutable revision. Read backend://schema/query for starting edges, fields and executable examples. Variables are scalars or lists, never objects, and nesting is bounded. A first page may adapt its limit; copy its exact continuation_call to resume the same query/revision with the issued limit. Typed refusals preserve unavailable, cancelled and stale-source facts.",
        "inputSchema":{
            "type":"object", "properties":{
                "query":{"type":"string","minLength":1,
                    "description":format!("Nonempty query text; at most {} UTF-8 bytes, not Unicode characters.", backend_library::MAX_GRAPH_QUERY_BYTES)},
                "variables":{
                    "description":format!("At most {} nonempty variable names. Aggregate admission is at most {} bytes: UTF-8 variable names and string values, recursively summed lists, and 16 bytes per non-string scalar.",backend_library::MAX_GRAPH_QUERY_FIELDS,backend_library::MAX_GRAPH_VALUE_BYTES),
                    "anyOf":[{"type":"null"},{"type":"object","maxProperties":backend_library::MAX_GRAPH_QUERY_FIELDS,"propertyNames":{"minLength":1},"additionalProperties":{"$ref":"#/$defs/value"}}]},
                "limit":{"type":"integer","minimum":1,"maximum":backend_library::QueryLimit::MAX,"default":backend_present::DEFAULT_LIMIT},
                "cursor":{"type":"string","minLength":1,"description":"Exact owner continuation for this immutable query; prefer the complete executable continuation_call."},
                "detail":detail_property(QUERY_TOOL)
            },
            "required":["query"],"additionalProperties":false,
            "$defs":{"value":{"description":format!("Finite JSON scalars or lists, with at most {} nested array levels; object values are refused.",backend_library::MAX_GRAPH_VALUE_DEPTH),
                "anyOf":[{"type":["null","boolean","number","string"]},{"type":"array","items":{"$ref":"#/$defs/value"}}]}}
        },
        "outputSchema":{"type":"object","description":"Unchanged graph-page structuredContent, including revision, rows, emitted count, terminal and optional executable continuation_call; errors are typed refusals."},
        "annotations":{"readOnlyHint":true,"destructiveHint":false,"idempotentHint":true,"openWorldHint":false},
        "_meta":{"backend/querySchema":"backend://schema/query",
            "backend/queryAdmission":{"maxQueryUtf8Bytes":backend_library::MAX_GRAPH_QUERY_BYTES,
                "maxVariables":backend_library::MAX_GRAPH_QUERY_FIELDS,
                "maxAggregateVariableBytes":backend_library::MAX_GRAPH_VALUE_BYTES,
                "nonStringScalarBytes":16,"maxValueDepth":backend_library::MAX_GRAPH_VALUE_DEPTH,
                "variableNames":"nonempty UTF-8; name bytes count toward the aggregate"}}
    })
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
    for field in arguments.keys() {
        if !grammar
            .positional()
            .iter()
            .chain(grammar.options())
            .any(|spec| spec.json_name() == field)
        {
            return Err(Fault::usage(
                field.clone(),
                format!("{} takes no argument called `{field}`", grammar.name()),
            ));
        }
    }
    for spec in grammar
        .positional()
        .iter()
        .chain(grammar.options())
        .filter(|spec| spec.is_required())
    {
        if !arguments.contains_key(spec.json_name()) {
            return Err(Fault::usage(
                spec.json_name(),
                format!(
                    "{} requires `arguments.{}`: {}",
                    grammar.tool(),
                    spec.json_name(),
                    spec.help()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_scalar(spec: ArgumentSpec, value: &Value) -> Result<(), Fault> {
    if spec.kind() == ArgumentKind::IndexJobTicket {
        return backend_present::decode_index_job_ticket(value).map(|_| ());
    }
    if spec.kind() == ArgumentKind::SelectedSymbol {
        return serde_json::from_value::<backend_present::IdentitySemanticData>(value.clone())
            .map(|_| ())
            .map_err(|_| {
                argument_fault(
                    spec,
                    "the exact selected-symbol-id object returned in identity.semantic_data",
                )
            });
    }
    let type_matches = match spec.json_type() {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        _ => false,
    };
    if type_matches {
        Ok(())
    } else if let Some(domain) = spec.page_limit_domain() {
        Err(argument_fault(
            spec,
            &format!(
                "an integer from {} through {}",
                domain.minimum(),
                domain.maximum()
            ),
        ))
    } else {
        let expected = if spec.is_repeated() {
            "an array of values with the declared type"
        } else {
            match spec.json_type() {
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
    let mut tools = Vec::with_capacity(SESSION_TOOLS.len() + INDEX_JOB_TOOLS.len() + 1);
    for name in SESSION_TOOLS.iter().chain(INDEX_JOB_TOOLS) {
        let descriptor =
            descriptor(name).unwrap_or_else(|| panic!("listed MCP tool {name} has no descriptor"));
        assert_eq!(descriptor.discovery, Discovery::Listed);
        tools.push(listed_tool(descriptor));
    }
    tools.push(listed_tool(
        descriptor(CALL_TOOL).expect("named call descriptor"),
    ));
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
        ToolRoute::NamedCall => named_call_tool(),
        ToolRoute::Query | ToolRoute::Surface | ToolRoute::RefusedIndexAwait => {
            panic!(
                "non-default route {} was put in tools/list",
                descriptor.name
            )
        }
    }
}

fn named_call_tool() -> Value {
    let names = descriptors()
        .into_iter()
        .filter(|descriptor| named_target(descriptor.route))
        .map(|descriptor| descriptor.name)
        .collect::<Vec<_>>();
    json!({
        "name": CALL_TOOL,
        "title": "Call Registered Tool",
        "description": "Call a registered named tool using its exact catalog arguments. Read backend://tools/catalog for capability-dependent routes and backend://schema/query for the typed query lane. Pass the selected name and its arguments object; the original route validates inputs and returns its original result. Source/owner capabilities may remain unavailable. Mutation, deadlines, cancellation, cursor identity and response budgets follow the selected tool. Surface and recursive calls are refused.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "name": {"type": "string", "enum": names},
                "arguments": {"type": "object", "description": "Exact input object described by the selected registered tool's catalog schema; place detail and cursor here when that route accepts them."}
            },
            "required": ["name", "arguments"],
            "additionalProperties": false
        },
        "outputSchema": {"type": "object", "description": "Unchanged structuredContent of the selected registered tool; its response contract is defined by that route.", "additionalProperties": true},
        "annotations": {"title": "Call Registered Tool", "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false},
        "_meta": {"backend/catalog": {"uri": CATALOG_RESOURCE_URI, "schema": CATALOG_SCHEMA}}
    })
}

fn index_start_tool() -> Value {
    job_tool(
        INDEX_START_TOOL,
        "Start Indexing",
        "Start indexing one local package or pinned package URL. Accepted or active work includes index_poll with the exact next observation. Default requests share a saved durable key with backend.index and CLI add, joining matching active work. An explicit operation_key retains that exact identity across owner restarts. Active receipts also retain the owner-issued ticket for cancellation. Terminal receipts do not require polling.",
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
                "operation_key": {
                    "type": "string", "pattern": "^[0-9a-f]{64}$",
                    "description": "Optional caller-persisted durable key for exact replay and status reconciliation across owner restarts."
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
        "Read an immediate observation of an owner-issued indexing ticket or durable operation_key. Copy the returned index_poll.call for a conditional wait of at most 10 seconds: unchanged pending observations return a compact reference, while changed/terminal/unknown evidence returns the complete receipt. For ticket-based calls, set compact_receipt=true with detail=summary to replace duplicate surface.data with a JSON pointer to the exact complete surface.index_job.value; no receipt fields are removed. Durable operation status plus profile progress are distinct evidence and stay uncompressed. Omit conditional options for the complete immediate snapshot. Ticket unknown-after-restart, operation unknown, unresolved, active and published evidence remain distinct.",
        json!({
            "type": "object",
            "properties": {
                "ticket": ticket_schema(),
                "operation_key": {
                    "type": "string", "pattern": "^[0-9a-f]{64}$",
                    "description": "Saved operation key; use instead of ticket to reconcile durable owner evidence across process restarts."
                },
                "after_sequence": {
                    "type": "integer",
                    "minimum": 0,
                    "default": 0,
                    "description": "Return only progress events after this sequence. Reuse next_sequence from the previous pending observation."
                },
                "after_observation": {
                    "type": "string", "pattern": "^[0-9a-f]{64}$",
                    "description": "Exact full active observation fingerprint returned in index_poll.call; requires operation_key. Changes of owner, state, request or source capture cannot be represented as unchanged."
                },
                "wait_millis": {
                    "type": "integer", "minimum": 0, "maximum": 10000,
                    "description": "Defaults to 10000 when after_observation is supplied, otherwise 0. Explicit 0 reads immediately. Bounded conditional wait under the original request deadline/cancellation. With an operation_key requires after_observation; ticket uses after_sequence. Never starts or cancels owner work."
                },
                "compact_receipt": {
                    "type": "boolean", "default": false,
                    "description": "Opt-in lossless envelope compaction for ticket calls only, with detail=summary. When the ticket observation and index_job.value are identical, surface.data is {receipt_pointer:\"#/surface/index_job/value\"}; follow that JSON pointer to recover the unchanged full typed receipt. Durable operation status plus profile progress are distinct and remain raw."
                },
                "detail": detail_property(INDEX_PROGRESS_TOOL)
            },
            "oneOf": [
                {"required": ["ticket"], "not": {"anyOf": [{"required": ["operation_key"]}, {"required": ["after_observation"]}]}},
                {"required": ["operation_key"], "not": {"anyOf": [{"required": ["ticket"]}, {"required": ["after_sequence"]}]}}
            ],
            "allOf": [{
                "if": {"required": ["operation_key", "wait_millis"], "properties": {"wait_millis": {"minimum": 1}}},
                "then": {"required": ["after_observation"]}
            }, {
                "if": {"required": ["compact_receipt"], "properties": {"compact_receipt": {"const": true}}},
                "then": {
                    "required": ["ticket"],
                    "properties": {"detail": {"const": "summary"}}
                }
            }],
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
        "outputSchema": if name == INDEX_PROGRESS_TOOL { index_progress_output_schema() } else { index_job_output_schema() },
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
            "answer": {"type":"string"},
            "detail": {"enum":["summary", "standard", "full"]},
            "budget": {"type":"object", "properties": {
                "bytes":{"type":"integer", "minimum":0},
                "estimatedTokens":{"type":"integer", "minimum":0},
                "bytesPerToken":{"type":"integer", "minimum":1}
            }, "required":["bytes", "estimatedTokens", "bytesPerToken"], "additionalProperties":false},
            "fault": {"type": "object"},
            "index_poll": index_poll_schema(),
            "surface": {
                "type": "object",
                "properties": {
                    "result": {
                        "type": "string",
                        "enum": ["index-started", "index-progress", "index-cancellation", "index-operation-started", "index-operation-status"]
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
                "required": ["result", "data"],
                "allOf": [{"if": {"properties": {"result": {"enum": ["index-started", "index-progress", "index-cancellation"]}}}, "then": {"required": ["index_job"]}}],
                "additionalProperties": false
            }
        },
        "required": ["surface"],
        "additionalProperties": false
    })
}

fn index_progress_output_schema() -> Value {
    let complete = index_job_output_schema();
    let mut legacy_surface = complete["properties"]["surface"].clone();
    legacy_surface["not"] = json!({
        "properties": {
            "result": {"const": "index-progress"},
            "data": {"required": ["receipt_pointer"]}
        },
        "required": ["result", "data"]
    });
    let compact = json!({
        "type": "object",
        "properties": {
            "result": {"const": "index-progress"},
            "data": {
                "type": "object",
                "properties": {
                    "receipt_pointer": {"const": "#/surface/index_job/value"}
                },
                "required": ["receipt_pointer"],
                "additionalProperties": false
            },
            "index_job": {
                "type": "object",
                "properties": {
                    "kind": {"const": "progress"},
                    "value": complete["properties"]["surface"]["properties"]["index_job"]["properties"]["value"]
                },
                "required": ["kind", "value"],
                "additionalProperties": false
            }
        },
        "required": ["result", "data", "index_job"],
        "additionalProperties": false
    });
    let reference = json!({"oneOf": [
        {"type":"object", "properties": {
            "identity":{"const":"operation"},
            "operation_key":{"type":"string", "pattern":"^[0-9a-f]{64}$"},
            "ticket":ticket_schema()["oneOf"][0],
            "after_observation":{"type":"string", "pattern":"^[0-9a-f]{64}$"}
        }, "required":["identity", "operation_key", "ticket", "after_observation"], "additionalProperties":false},
        {"type":"object", "properties": {
            "identity":{"const":"ticket"},
            "ticket":ticket_schema()["oneOf"][0],
            "after_sequence":{"type":"integer", "minimum":0},
            "stage":{"enum":["acquiring", "staging", "scanning", "compiling", "publishing"]}
        }, "required":["identity", "ticket", "after_sequence", "stage"], "additionalProperties":false}
    ]});
    json!({
        "type":"object",
        "properties": {
            "answer":complete["properties"]["answer"],
            "detail":complete["properties"]["detail"],
            "budget":complete["properties"]["budget"],
            "fault":complete["properties"]["fault"],
            "index_poll":index_poll_schema(),
            "surface":{"oneOf":[legacy_surface, compact, {
                "type":"object", "properties": {
                    "result":{"const":"index-progress-unchanged"},
                    "data":{"type":"object", "properties": {
                        "state":{"const":"pending-unchanged"}, "reference":reference
                    }, "required":["state", "reference"], "additionalProperties":false}
                }, "required":["result", "data"], "additionalProperties":false
            }]}
        },
        "required":["surface"],
        "allOf":[{
            "if":{"properties":{"surface":{"properties":{"result":{"const":"index-progress-unchanged"}}}}},
            "then":{"required":["index_poll"], "not":{"required":["fault"]}}
        }],
        "additionalProperties":false
    })
}

fn index_poll_schema() -> Value {
    json!({
        "type": "object",
        "description": "Exact next observation for accepted or active work; omitted for terminal, unknown, unresolved or expired operations.",
        "properties": {
            "shell": {"type": "string", "description":"Optional exact command for an observed CLI in the serving bundle and a reproducible default authority attachment; absent when unavailable."},
            "call": {
                "type": "object",
                "properties": {
                    "name": {"const": "backend.index_progress"},
                    "arguments": {"type": "object"}
                },
                "required": ["name", "arguments"],
                "additionalProperties": false
            }
        },
        "required": ["call"],
        "additionalProperties": false
    })
}

fn ticket_schema() -> Value {
    let object = json!({
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
    });
    json!({
        "description": "Exact ticket returned by backend.index_start. Pass its object or copy its JSON string; preserve id, owner_epoch and package.",
        "oneOf": [object, {
            "type": "string", "minLength": 1,
            "maxLength": backend_library::MAX_COMMAND_TEXT,
            "description": "JSON encoding of the exact ticket object; the UTF-8 byte bound is checked before decoding."
        }]
    })
}

/// Returns the canonical `tools/list` projection for the offline budget
/// fixture generator. This is kept at the protocol edge so the generator
/// cannot accidentally grow a second hand-written tool schema.
#[cfg(feature = "token-budget")]
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
            argument["description"] = json!(
                "Required existing repository directory. Pass an absolute path; directory symlinks are accepted. MCP never defaults this operand to its working directory, and rejects invalid targets before owner startup or indexing."
            );
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
    if let Some(cursor_domain) = grammar.continuation_domain() {
        properties.entry("cursor".to_owned()).or_insert_with(|| json!({
            "type": if cursor_domain == backend_present::ContinuationDomain::NativeOwnerJson {"object"} else {"string"},
            "description":"Exact continuation returned by the preceding page. Preserve its original operands and page limit."
        }));
        let id = grammar.spec().map(|spec| spec.id);
        if matches!(
            id,
            Some(
                backend_library::CommandId::Graph
                    | backend_library::CommandId::GraphQuery
                    | backend_library::CommandId::Outline
            )
        ) {
            let outline = matches!(id, Some(backend_library::CommandId::Outline));
            properties.insert("limit".to_owned(),json!({
                "type":"integer", "minimum":1, "maximum":backend_library::QueryLimit::MAX,
                "default":if outline {backend_library::QueryLimit::MAX} else {backend_present::DEFAULT_LIMIT},
                "description":if outline {"Maximum declarations requested. The first page may use a smaller effective limit to fit the named token budget; continue with page.limit and the returned cursor."} else {"Maximum graph rows in this page."}
            }));
        }
    }
    let title = grammar
        .spec()
        .map_or_else(|| grammar.name(), |spec| spec.title);
    let write = grammar.is_write();
    let mut output_schema = answer_schema();
    if grammar.tool() == "backend.index" {
        output_schema["properties"]["index_poll"] = index_poll_schema();
    }
    if grammar.tool() == "backend.select_semantic_version" {
        output_schema["description"] = json!(
            "A bounded selection acknowledgement, including when reconfirming an already selected generation. Complete semantic records and history catalogs are separate reads through metadata_call and semantic-source ranges; detail=full does not inline that catalog."
        );
        output_schema["properties"]["selection_receipt"] = json!({
            "type":"object", "additionalProperties":false,
            "required":["schema","package","coordinate","profile","generation","selected"],
            "properties":{
                "schema":{"const":"backend.semantic-selection-receipt.v1"},
                "package":{
                    "type":"object","additionalProperties":false,"required":["kind","value"],
                    "properties":{"kind":{"enum":["purl","local"]},"value":{"type":"string"}}
                },
                "coordinate":{"type":"string"},
                "profile":{"type":"array","minItems":2,"maxItems":2,"items":{"type":"integer","minimum":0,"maximum":255}},
                "generation":{"type":"array","minItems":32,"maxItems":32,"items":{"type":"integer","minimum":0,"maximum":255}},
                "selected":{"const":true}
            }
        });
        output_schema["properties"]["metadata_call"] = json!({"type":"object","description":"Copy this separately executable read, then follow descriptor pages to the receipt's exact coordinate/profile/generation and semantic-source ranges for the complete captured record."});
        output_schema["properties"]["metadata_read"] = json!({"type":"string"});
    }
    let mut tool = json!({
        "name": grammar.tool(),
        "title": title,
        "description": grammar.description(),
        "inputSchema": {
            "type": "object",
            "properties": Value::Object(properties),
            "required": required,
            "additionalProperties": false
        },
        "outputSchema": output_schema,
        "annotations": {
            "title": title,
            "readOnlyHint": !write,
            "destructiveHint": grammar.is_destructive(),
            "idempotentHint": !write,
            "openWorldHint": false
        },
        "_meta": { "backend/domain": domain_name(domain), "backend/command": grammar.name() }
    });
    if grammar
        .options()
        .iter()
        .any(|spec| spec.kind() == ArgumentKind::SelectedSymbol)
    {
        tool["inputSchema"]["anyOf"] =
            json!([{ "required": ["coordinate"] }, { "required": ["selected_symbol"] }]);
    }
    tool
}

/// Projects one operand into its JSON Schema property.
fn property(spec: ArgumentSpec) -> Value {
    if spec.kind() == ArgumentKind::SelectedSymbol {
        return json!({"type":"object", "properties": {
            "kind":{"const":"selected-symbol-id"},
            "value":{"type":"array", "items":{"type":"integer", "minimum":0, "maximum":255}, "minItems":32, "maxItems":32, "contains":{"type":"integer","minimum":1,"maximum":255}}
        }, "required":["kind","value"], "additionalProperties":false, "description":spec.help()});
    }
    if spec.kind() == ArgumentKind::SemanticShapeRequest {
        let mut schema = backend_library::SemanticShapeReadRequest::json_schema();
        schema["description"] = json!(spec.help());
        return schema;
    }
    if spec.kind() == ArgumentKind::SemanticSourceRequest {
        let mut schema = backend_library::SemanticSourceRequest::json_schema();
        schema["description"] = json!(spec.help());
        return schema;
    }
    if spec.kind() == ArgumentKind::IndexJobTicket {
        return ticket_schema();
    }
    let choices = spec.kind().enumeration();
    let mut scalar = json!({ "type": spec.json_type() });
    if !choices.is_empty() {
        scalar["enum"] = json!(choices);
    }
    if let Some(domain) = spec.page_limit_domain() {
        scalar["minimum"] = json!(domain.minimum());
        scalar["maximum"] = json!(domain.maximum());
        scalar["default"] = json!(domain.default_limit());
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
    let description = if matches!(tool, "backend.document" | "backend.source") {
        "Page text and structured fields: summary, standard, or full."
    } else {
        "Response fields: summary, standard, or full."
    };
    json!({
        "type": "string",
        "enum": ["summary", "standard", "full"],
        "default": default_detail,
        "description": description
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
                "description": "Which presentation answer this is; faults may be attached to a product answer while its exact operation receipt remains available."
            }
        },
        "required": ["answer"],
        "additionalProperties": true
    })
}

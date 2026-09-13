//! Standards-compliant MCP JSON-RPC surface over one admitted product session.

use backend_client::Session;
use backend_library::{
    CommandReply, DiffRecord, Document, GraphQueryPage, GraphValue, HealthReport, OutlineExtent,
    OutlineNode, PackageReference, PageTerminal, ProjectionPage, ReplyDto, Row, RowId,
    SourceAvailability, SurfaceCommand, SurfaceReply, ViewRoot, ViewSnapshot, ViewStateRoot,
    encode_id,
};
use serde_json::{Map, Value, json};
use std::io::{self, BufRead, Write};

mod codec;
mod projection;
use codec::{
    empty_cursor, error_reply, limit, no_extra, object, optional_string, percent_decode,
    percent_encode, query_variables, read_line, required_string, string, success, valid_id,
    write_message,
};
use projection::{
    capability_value, command_failure_kind, coverage_readiness, coverage_value, display_name,
    error_value, fragments_text, readiness, row_state_name, short_root, source_availability_value,
    source_excerpt_value,
};

const STABLE_PROTOCOL: &str = "2025-11-25";
const CANDIDATE_PROTOCOL: &str = "2026-07-28";
const SERVER_NAME: &str = "backend";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(super) trait Product {
    fn health(&mut self) -> Result<HealthReport, String>;
    fn revision(&mut self) -> Result<ViewStateRoot, String>;
    fn packages(&mut self) -> Result<ReplyDto, String>;
    fn index(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn remove(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn search(&mut self, text: &str, limit: u16) -> Result<ReplyDto, String>;
    fn names(&mut self, text: &str, limit: u16) -> Result<ReplyDto, String>;
    fn document(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn source(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn outline(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn graph(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn related(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn diff(
        &mut self,
        from: PackageReference,
        to: PackageReference,
    ) -> Result<Box<[DiffRecord]>, String>;
    fn graph_query(
        &mut self,
        query: String,
        variables: std::collections::BTreeMap<String, GraphValue>,
        limit: u16,
    ) -> Result<GraphQueryPage, String>;
    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, String>;
}

impl Product for Session {
    fn health(&mut self) -> Result<HealthReport, String> {
        Session::health(self).map_err(|error| error.to_string())
    }

    fn revision(&mut self) -> Result<ViewStateRoot, String> {
        Session::revision(self)
            .map(|revision| revision.root)
            .map_err(|error| error.to_string())
    }

    fn packages(&mut self) -> Result<ReplyDto, String> {
        Session::packages(self).map_err(|error| error.to_string())
    }

    fn index(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::index(self, coordinate).map_err(|error| error.to_string())
    }

    fn remove(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::remove(self, coordinate).map_err(|error| error.to_string())
    }

    fn search(&mut self, text: &str, limit: u16) -> Result<ReplyDto, String> {
        Session::search(self, text, limit).map_err(|error| error.to_string())
    }

    fn names(&mut self, text: &str, limit: u16) -> Result<ReplyDto, String> {
        Session::names(self, text, limit).map_err(|error| error.to_string())
    }

    fn document(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::document(self, coordinate).map_err(|error| error.to_string())
    }

    fn source(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::source(self, coordinate).map_err(|error| error.to_string())
    }

    fn outline(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::outline(self, coordinate).map_err(|error| error.to_string())
    }

    fn graph(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::graph(self, coordinate).map_err(|error| error.to_string())
    }

    fn related(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::related(self, coordinate).map_err(|error| error.to_string())
    }

    fn diff(
        &mut self,
        from: PackageReference,
        to: PackageReference,
    ) -> Result<Box<[DiffRecord]>, String> {
        Session::diff(self, from, to).map_err(|error| error.to_string())
    }

    fn graph_query(
        &mut self,
        query: String,
        variables: std::collections::BTreeMap<String, GraphValue>,
        limit: u16,
    ) -> Result<GraphQueryPage, String> {
        Session::graph_query(self, query, variables, limit, None, false)
            .map_err(|error| error.to_string())
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, String> {
        Session::surface(self, command).map_err(|error| error.to_string())
    }
}

/// Runs newline-delimited MCP stdio until the client closes stdin.
pub(super) fn serve_stdio(
    session: Session,
    project: String,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> io::Result<()> {
    let mut server = Server::new(session, project);
    loop {
        let Some(line) = read_line(reader)? else {
            return Ok(());
        };
        if let Some(reply) = server.handle(&line) {
            write_message(writer, &reply)?;
        }
    }
}

pub(super) struct Server<P> {
    product: P,
    project: String,
    handshake: HandshakeState,
    protocol: &'static str,
}

/// MCP's initialization handshake is a protocol state, not a boolean.
///
/// Keeping the acknowledgement phase distinct prevents an unsolicited
/// `notifications/initialized` message from granting access before the
/// server has admitted an `initialize` request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HandshakeState {
    AwaitInitialize,
    AwaitInitializedNotification,
    Ready,
}

impl<P: Product> Server<P> {
    pub(super) fn new(product: P, project: String) -> Self {
        Self {
            product,
            project,
            handshake: HandshakeState::AwaitInitialize,
            protocol: STABLE_PROTOCOL,
        }
    }

    pub(super) fn handle(&mut self, input: &[u8]) -> Option<Value> {
        let value: Value = match serde_json::from_slice(input) {
            Ok(value) => value,
            Err(error) => {
                return Some(error_reply(
                    Value::Null,
                    -32700,
                    "Parse error",
                    Some(json!({ "detail": error.to_string() })),
                ));
            }
        };
        let Some(object) = value.as_object() else {
            return Some(error_reply(Value::Null, -32600, "Invalid Request", None));
        };
        let id = object.get("id").cloned();
        let response_id = id.clone().unwrap_or(Value::Null);
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || id.as_ref().is_some_and(|id| !valid_id(id))
        {
            return id.map(|_| error_reply(response_id, -32600, "Invalid Request", None));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return id.map(|_| error_reply(response_id, -32600, "Invalid Request", None));
        };
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));

        if id.is_none() {
            if method == "notifications/initialized"
                && self.handshake == HandshakeState::AwaitInitializedNotification
            {
                self.handshake = HandshakeState::Ready;
            }
            return None;
        }

        if method == "initialize" {
            if self.handshake != HandshakeState::AwaitInitialize {
                return Some(error_reply(
                    response_id,
                    -32600,
                    "Initialize request is not valid in the current state",
                    None,
                ));
            }
            return Some(match self.initialize(&params) {
                Ok(result) => success(response_id, result),
                Err(error) => error.into_reply(response_id),
            });
        }
        if self.handshake != HandshakeState::Ready {
            return Some(error_reply(
                response_id,
                -32002,
                "Server is not initialized",
                None,
            ));
        }

        let result = match method {
            "ping" => Ok(json!({})),
            "tools/list" => list_tools(&params),
            "tools/call" => self.call_tool(&params),
            "resources/list" => self.list_resources(&params),
            "resources/templates/list" => list_resource_templates(&params),
            "resources/read" => self.read_resource(&params),
            "prompts/list" => list_prompts(&params),
            "prompts/get" => self.get_prompt(&params),
            _ => Err(RpcError::new(-32601, "Method not found")),
        };
        Some(match result {
            Ok(result) => success(response_id, result),
            Err(error) => error.into_reply(response_id),
        })
    }

    fn initialize(&mut self, params: &Value) -> Result<Value, RpcError> {
        let params = object(params)?;
        let offered = string(params, "protocolVersion")?;
        self.protocol = match offered {
            CANDIDATE_PROTOCOL => CANDIDATE_PROTOCOL,
            _ => STABLE_PROTOCOL,
        };
        self.handshake = HandshakeState::AwaitInitializedNotification;
        Ok(json!({
            "protocolVersion": self.protocol,
            "capabilities": {
                "tools": { "listChanged": false },
                "resources": { "subscribe": false, "listChanged": false },
                "prompts": { "listChanged": false }
            },
            "serverInfo": {
                "name": SERVER_NAME,
                "title": "Backend Code Intelligence",
                "version": SERVER_VERSION
            },
            "instructions": "Check backend.status before querying and call backend.index when the current project is not ready. Use backend.search to find declarations, then backend.document or backend.graph with the returned coordinate. Every result is pinned to an immutable local revision."
        }))
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, RpcError> {
        let params = object(params)?;
        let name = string(params, "name")?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => return Err(RpcError::invalid("arguments must be an object")),
        };
        let reply = match name {
            "backend.status" => {
                no_extra(arguments, &[])?;
                let report = self.product.health().map_err(RpcError::tool)?;
                return Ok(tool_result(readiness_value(&report, &self.project), false));
            }
            "backend.projects" => {
                no_extra(arguments, &[])?;
                self.product.packages()
            }
            "backend.index" => {
                no_extra(arguments, &["path"])?;
                let path = optional_string(arguments, "path")?.unwrap_or(&self.project);
                self.product.index(path)
            }
            "backend.remove" => {
                no_extra(arguments, &["path"])?;
                let path = optional_string(arguments, "path")?.unwrap_or(&self.project);
                self.product.remove(path)
            }
            "backend.search" => {
                no_extra(arguments, &["query", "limit"])?;
                self.product
                    .search(required_string(arguments, "query")?, limit(arguments)?)
            }
            "backend.names" => {
                no_extra(arguments, &["query", "limit"])?;
                self.product.names(
                    optional_string(arguments, "query")?.unwrap_or(""),
                    limit(arguments)?,
                )
            }
            "backend.document" => {
                no_extra(arguments, &["coordinate"])?;
                self.product
                    .document(required_string(arguments, "coordinate")?)
            }
            "backend.source" => {
                no_extra(arguments, &["coordinate"])?;
                self.product
                    .source(required_string(arguments, "coordinate")?)
            }
            "backend.outline" => {
                no_extra(arguments, &["path"])?;
                let path = optional_string(arguments, "path")?.unwrap_or(&self.project);
                self.product.outline(path)
            }
            "backend.graph" => {
                no_extra(arguments, &["coordinate"])?;
                self.product
                    .graph(required_string(arguments, "coordinate")?)
            }
            "backend.related" => {
                no_extra(arguments, &["coordinate"])?;
                self.product
                    .related(required_string(arguments, "coordinate")?)
            }
            "backend.diff" => return self.diff_tool(arguments),
            "backend.surface" => return self.surface_tool(arguments),
            "backend.query" => {
                return self.query_tool(arguments);
            }
            _ => return Err(RpcError::new(-32602, "Unknown tool")),
        };
        match reply {
            Ok(reply) => {
                let (value, is_error) = reply_value(reply);
                Ok(tool_result(value, is_error))
            }
            Err(error) => Ok(tool_result(error_value("transport", error), true)),
        }
    }

    fn diff_tool(&mut self, arguments: &Map<String, Value>) -> Result<Value, RpcError> {
        no_extra(arguments, &["from", "to"])?;
        let package = |field| {
            PackageReference::parse(required_string(arguments, field)?.to_owned())
                .map_err(|error| RpcError::invalid(format!("{field}: {error}")))
        };
        Ok(match self.product.diff(package("from")?, package("to")?) {
            Ok(reply) => tool_result(
                json!({ "surface": { "result": "diff", "data": reply } }),
                false,
            ),
            Err(error) => tool_result(error_value("transport", error), true),
        })
    }

    fn query_tool(&mut self, arguments: &Map<String, Value>) -> Result<Value, RpcError> {
        no_extra(arguments, &["query", "variables", "limit"])?;
        let query = required_string(arguments, "query")?;
        let variables = query_variables(arguments)?;
        Ok(
            match self
                .product
                .graph_query(query.to_owned(), variables, limit(arguments)?)
            {
                Ok(page) => tool_result(graph_query_page_value(&page), false),
                Err(error) => tool_result(error_value("transport", error), true),
            },
        )
    }

    fn surface_tool(&mut self, arguments: &Map<String, Value>) -> Result<Value, RpcError> {
        no_extra(arguments, &["command"])?;
        let encoded = arguments
            .get("command")
            .cloned()
            .ok_or_else(|| RpcError::invalid("command must be a tagged surface object"))?;
        let command = serde_json::from_value::<SurfaceCommand>(encoded)
            .map_err(|error| RpcError::invalid(format!("command: {error}")))?;
        command
            .admit()
            .map_err(|error| RpcError::invalid(format!("command: {error}")))?;
        Ok(match self.product.surface(command) {
            Ok(reply) => tool_result(json!({ "surface": reply }), false),
            Err(error) => tool_result(error_value("transport", error), true),
        })
    }

    fn list_resources(&mut self, params: &Value) -> Result<Value, RpcError> {
        empty_cursor(params)?;
        let packages = self.product.packages().map_err(RpcError::tool)?;
        let rows = match packages.reply {
            CommandReply::Packages(snapshot) => snapshot.root.rows().to_vec(),
            CommandReply::Error(message) => return Err(RpcError::tool(message)),
            CommandReply::Failed(failure) => return Err(RpcError::tool(failure.to_string())),
            _ => return Err(RpcError::tool("packages reply changed shape")),
        };
        let mut resources = vec![json!({
            "uri": "backend://workspace/current",
            "name": "current-workspace",
            "title": "Current indexed workspace",
            "description": "Immutable revision, source readiness, honest lane coverage, project count, and declaration count.",
            "mimeType": "application/json"
        })];
        resources.extend(rows.iter().filter_map(|row| match row.id {
            RowId::Package(_) => Some(json!({
                "uri": format!("backend://outline/{}", percent_encode(&row.label)),
                "name": row.label,
                "title": display_name(&row.label),
                "description": "Version-pinned project outline.",
                "mimeType": "application/json"
            })),
            RowId::Symbol(_) | RowId::Object(_) => None,
        }));
        Ok(json!({ "resources": resources }))
    }

    fn read_resource(&mut self, params: &Value) -> Result<Value, RpcError> {
        let params = object(params)?;
        let uri = string(params, "uri")?;
        let value = if uri == "backend://workspace/current" {
            let report = self.product.health().map_err(RpcError::tool)?;
            readiness_value(&report, &self.project)
        } else if let Some(coordinate) = uri.strip_prefix("backend://document/") {
            let coordinate = percent_decode(coordinate)?;
            let reply = self.product.document(&coordinate).map_err(RpcError::tool)?;
            resource_value(reply)?
        } else if let Some(coordinate) = uri.strip_prefix("backend://outline/") {
            let coordinate = percent_decode(coordinate)?;
            let reply = self.product.outline(&coordinate).map_err(RpcError::tool)?;
            resource_value(reply)?
        } else {
            return Err(RpcError::new(-32002, "Resource not found"));
        };
        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": serde_json::to_string_pretty(&value).map_err(|error| RpcError::tool(error.to_string()))?
            }]
        }))
    }

    fn get_prompt(&mut self, params: &Value) -> Result<Value, RpcError> {
        let params = object(params)?;
        if string(params, "name")? != "backend.explore" {
            return Err(RpcError::new(-32602, "Unknown prompt"));
        }
        let query = params
            .get("arguments")
            .and_then(Value::as_object)
            .and_then(|arguments| arguments.get("query"))
            .and_then(Value::as_str)
            .unwrap_or("the relevant implementation");
        let view = self.product.revision().map_err(RpcError::tool)?;
        Ok(json!({
            "description": "Explore the current immutable code index.",
            "messages": [{
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!("Explore {query} in the indexed project. Search broadly, open the most relevant declarations, inspect their graph neighbors, and ground the answer in revision {}.", short_root(&view))
                }
            }]
        }))
    }
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: &'static str,
    kind: &'static str,
    detail: Option<String>,
}

impl RpcError {
    const fn new(code: i64, message: &'static str) -> Self {
        Self {
            code,
            message,
            kind: "json_rpc",
            detail: None,
        }
    }

    fn invalid(detail: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: "Invalid params",
            kind: "invalid_params",
            detail: Some(detail.into()),
        }
    }

    fn tool(detail: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: "Backend request failed",
            kind: "transport",
            detail: Some(detail.into()),
        }
    }

    fn into_reply(self, id: Value) -> Value {
        error_reply(
            id,
            self.code,
            self.message,
            self.detail
                .map(|detail| json!({ "kind": self.kind, "detail": detail })),
        )
    }
}

fn list_tools(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    let text = |description: &str| json!({ "type": "string", "description": description });
    let limit = json!({ "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 });
    Ok(json!({ "tools": [
        tool("backend.status", "Workspace status", "Read the current immutable revision, source readiness, lane coverage, and index counts.", &json!({}), &[], true, false),
        tool("backend.projects", "Indexed projects", "List projects in the current workspace revision.", &json!({}), &[], true, false),
        tool("backend.search", "Search code", "Search declaration names, signatures, and documentation. Returned coordinates can be passed directly to other tools.", &json!({ "query": text("Text to find."), "limit": limit }), &["query"], true, false),
        tool("backend.names", "Find names", "Find declaration names in the current immutable revision.", &json!({ "query": text("Optional name prefix or substring."), "limit": limit }), &[], true, false),
        tool("backend.document", "Read declaration", "Read the signature and documentation for an exact coordinate returned by search.", &json!({ "coordinate": text("Exact declaration coordinate.") }), &["coordinate"], true, false),
        tool("backend.source", "Read source", "Read captured source for an exact declaration coordinate returned by search.", &json!({ "coordinate": text("Exact declaration coordinate.") }), &["coordinate"], true, false),
        tool("backend.outline", "Project outline", "Read the declaration tree for a project path.", &json!({ "path": text("Project path; defaults to the active project.") }), &[], true, false),
        tool("backend.graph", "Declaration graph", "Read the bounded graph neighborhood for an exact declaration coordinate.", &json!({ "coordinate": text("Exact declaration coordinate.") }), &["coordinate"], true, false),
        tool("backend.related", "Related declarations", "Read incoming and outgoing semantic relations for an exact declaration coordinate.", &json!({ "coordinate": text("Exact declaration coordinate.") }), &["coordinate"], true, false),
        tool("backend.diff", "Compare package versions", "Compare declarations from two indexed package versions using compiler semantic identity and payloads.", &json!({ "from": text("Older pinned package URL or exact local package label."), "to": text("Newer pinned package URL or exact local package label.") }), &["from", "to"], true, false),
        tool("backend.query", "Query the code graph", "Run a typed Trustfall query over the current immutable revision. Starting edges are Item, Project, and Declaration; fields include coordinate, name, kind, signature, documentation, revision, project, parent, children, related, and sameProject.", &json!({ "query": text("Trustfall query text."), "variables": { "type": "object", "description": "Scalar or list query variables.", "additionalProperties": true }, "limit": limit }), &["query"], true, false),
        tool("backend.index", "Refresh project index", "Submit an incremental index request. The accepted intent is returned immediately; use backend.status to observe source readiness.", &json!({ "path": text("Project path; defaults to the active project.") }), &[], false, false),
        tool("backend.remove", "Remove project", "Submit a request to remove a project and its file frontier. The accepted intent is returned immediately.", &json!({ "path": text("Project path; defaults to the active project.") }), &[], false, true),
        surface_tool()
    ] }))
}

fn surface_tool() -> Value {
    let operations = backend_library::COMMANDS
        .iter()
        .filter_map(|spec| spec.is_surface().then_some(spec.name))
        .collect::<Vec<_>>();
    json!({
        "name": "backend.surface",
        "title": "Product surface",
        "description": "Execute any daemon-owned typed product operation. The command is the tagged SurfaceCommand object; parsing and admission reject unknown operations, fields, identities, and bounds.",
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
        }
    })
}

fn tool(
    name: &str,
    title: &str,
    description: &str,
    properties: &Value,
    required: &[&str],
    read_only: bool,
    destructive: bool,
) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "outputSchema": { "type": "object", "additionalProperties": true },
        "annotations": {
            "title": title,
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": true,
            "openWorldHint": false
        }
    })
}

fn list_resource_templates(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    Ok(json!({ "resourceTemplates": [
        {
            "uriTemplate": "backend://document/{coordinate}",
            "name": "declaration-document",
            "title": "Declaration document",
            "description": "A version-pinned declaration selected by an exact coordinate.",
            "mimeType": "application/json"
        },
        {
            "uriTemplate": "backend://outline/{path}",
            "name": "project-outline",
            "title": "Project outline",
            "description": "A version-pinned outline selected by project path.",
            "mimeType": "application/json"
        }
    ] }))
}

fn list_prompts(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    Ok(json!({ "prompts": [{
        "name": "backend.explore",
        "title": "Explore indexed code",
        "description": "Search, read, and trace declarations from one immutable revision.",
        "arguments": [{ "name": "query", "description": "Feature or concept to investigate.", "required": false }]
    }] }))
}

fn tool_result(value: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&value)
        .unwrap_or_else(|_| "Backend result could not be rendered".to_owned());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": object_value(value),
        "isError": is_error
    })
}

fn graph_value(value: &GraphValue) -> Value {
    match value {
        GraphValue::Null => Value::Null,
        GraphValue::Boolean(value) => Value::Bool(*value),
        GraphValue::Signed(value) => (*value).into(),
        GraphValue::Unsigned(value) => (*value).into(),
        GraphValue::Float(_) => value.as_float().map_or(Value::Null, Value::from),
        GraphValue::String(value) => Value::String(value.clone()),
        GraphValue::List(values) => Value::Array(values.iter().map(graph_value).collect()),
    }
}

fn graph_query_page_value(page: &GraphQueryPage) -> Value {
    let terminal = match page.terminal {
        PageTerminal::Complete => "complete",
        PageTerminal::More(_) => "limit_reached",
        PageTerminal::Cancelled => "cancelled",
    };
    json!({
        "revision": encode_id(page.revision.as_bytes()),
        "rows": page.rows.iter().map(|row| Value::Object(
            row.fields().iter().map(|(name, value)| (name.clone(), graph_value(value))).collect()
        )).collect::<Vec<_>>(),
        "terminal": terminal,
        "emitted": page.rows.len()
    })
}

fn object_value(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        other => json!({ "value": other }),
    }
}

fn reply_value(reply: ReplyDto) -> (Value, bool) {
    match reply.reply {
        CommandReply::Surface(surface) => (json!({ "surface": surface }), false),
        CommandReply::Packages(snapshot) => (snapshot_value("projects", &snapshot), false),
        CommandReply::ProjectionPage(page) => (projection_page_value(&page), false),
        CommandReply::Names(snapshot) => (snapshot_value("names", &snapshot), false),
        CommandReply::Search(snapshot) => (snapshot_value("search", &snapshot), false),
        CommandReply::Graph(snapshot) => (snapshot_value("graph", &snapshot), false),
        CommandReply::GraphQueryPage(page) => (graph_query_page_value(&page), false),
        CommandReply::Resolved(rows) => (
            json!({ "rows": rows.iter().map(row_value).collect::<Vec<_>>() }),
            false,
        ),
        CommandReply::Added(intent) => (
            json!({
                "accepted": true,
                "operation": "index",
                "intent": encode_id(intent.as_bytes()),
                "completion": "pending",
                "message": "Index request accepted. Check backend.status for source readiness."
            }),
            false,
        ),
        CommandReply::Removed(intent) => (
            json!({
                "accepted": true,
                "operation": "remove",
                "intent": encode_id(intent.as_bytes()),
                "completion": "pending",
                "message": "Remove request accepted. Check backend.status for source readiness."
            }),
            false,
        ),
        CommandReply::Document(document) | CommandReply::Page(document) => {
            (document_value(&document), false)
        }
        CommandReply::Outline(outline) => (
            json!({
                "project": encode_id(outline.package.as_bytes()),
                "revision": encode_id(outline.basis.as_bytes()),
                "root": outline_node_value(&outline.root),
                "roots": outline.roots().map(outline_node_value).collect::<Vec<_>>(),
                "extent": match outline.extent {
                    OutlineExtent::Complete => "complete",
                    OutlineExtent::Truncated => "truncated",
                }
            }),
            false,
        ),
        CommandReply::Health(view) => (status_value(&view, ""), false),
        CommandReply::Readiness(report) => (readiness_value(&report, ""), false),
        CommandReply::Revision(receipt) => (
            serde_json::json!({
                "revision": encode_id(receipt.root().as_bytes()),
                "sequence": receipt.cursor().sequence(),
            }),
            false,
        ),
        CommandReply::Error(message) => (error_value("command", message), true),
        CommandReply::Failed(failure) => (
            error_value(command_failure_kind(&failure), failure.to_string()),
            true,
        ),
    }
}

fn snapshot_value(kind: &str, snapshot: &ViewSnapshot) -> Value {
    json!({
        "kind": kind,
        "revision": encode_id(snapshot.root.root().as_bytes()),
        "freshness": format!("{:?}", snapshot.freshness).to_lowercase(),
        "readiness": readiness(&snapshot.root),
        "coverage": snapshot.root.coverage().iter().map(|coverage| coverage_value(*coverage)).collect::<Vec<_>>(),
        "rows": snapshot.root.rows().iter().map(row_value).collect::<Vec<_>>(),
        "next": snapshot.next.map(|_| "available")
    })
}

fn projection_page_value(page: &ProjectionPage) -> Value {
    let mut value = snapshot_value("projection", &page.snapshot);
    let terminal = match page.terminal {
        PageTerminal::Complete => json!({ "state": "complete" }),
        PageTerminal::More(_) => json!({ "state": "more", "continuation": "available" }),
        PageTerminal::Cancelled => json!({ "state": "cancelled" }),
    };
    value["terminal"] = terminal;
    value
}

fn resource_value(reply: ReplyDto) -> Result<Value, RpcError> {
    let (value, is_error) = reply_value(reply);
    if is_error {
        let detail = value
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("backend command failed")
            .to_owned();
        Err(RpcError::tool(detail))
    } else {
        Ok(value)
    }
}

fn row_value(row: &Row) -> Value {
    let source = source_availability_value(&row.source);
    json!({
        "id": row.id.stable_key(),
        "kind": match row.id { RowId::Package(_) => "project", RowId::Symbol(_) => "declaration", RowId::Object(_) => "object" },
        "declarationKind": row.kind.map(backend_library::DeclarationKind::name),
        "state": row_state_name(row.state),
        "coordinate": row.label,
        "name": display_name(&row.label),
        "signature": row.signature,
        "documentation": fragments_text(&row.document),
        "score": row.score,
        "revision": encode_id(row.basis.root.as_bytes()),
        "source": source,
        "excerpt": source_excerpt_value(&row.excerpt)
    })
}

fn document_value(document: &Document) -> Value {
    json!({
        "symbol": encode_id(document.symbol.as_bytes()),
        "revision": encode_id(document.basis.as_bytes()),
        "signature": document.signature,
        "documentation": fragments_text(&document.fragments),
        "source": source_availability_value(&document.location),
        "excerpt": source_excerpt_value(&document.excerpt)
    })
}

fn outline_node_value(node: &OutlineNode) -> Value {
    json!({
        "symbol": encode_id(node.symbol.as_bytes()),
        "children": node.children.iter().map(outline_node_value).collect::<Vec<_>>()
    })
}

fn status_value(view: &ViewRoot, project: &str) -> Value {
    let (projects, declarations) =
        view.rows()
            .iter()
            .fold((0usize, 0usize), |counts, row| match row.id {
                RowId::Package(_) => (counts.0.saturating_add(1), counts.1),
                RowId::Symbol(_) => (counts.0, counts.1.saturating_add(1)),
                RowId::Object(_) => counts,
            });
    let sources = source_counts(view.rows());
    json!({
        "revision": encode_id(view.root().as_bytes()),
        "project": project,
        "readiness": readiness(view),
        "coverage": view.coverage().iter().map(|coverage| coverage_value(*coverage)).collect::<Vec<_>>(),
        "sourceAvailability": {
            "captured": sources.0,
            "notCaptured": sources.1,
            "notHydrated": sources.2,
            "unconfigured": sources.3
        },
        "projects": projects,
        "declarations": declarations
    })
}

fn readiness_value(report: &HealthReport, project: &str) -> Value {
    let revision = report.revision();
    let basis = report.basis();
    json!({
        "revision": encode_id(revision.root().as_bytes()),
        "sequence": revision.cursor().sequence(),
        "sourceRevision": encode_id(basis.root.as_bytes()),
        "sourceObject": encode_id(basis.object.as_bytes()),
        "project": project,
        "readiness": coverage_readiness(report.coverage(), false, false),
        "coverage": report.coverage().iter().map(|coverage| coverage_value(*coverage)).collect::<Vec<_>>(),
        "capabilities": report.capabilities().as_slice().iter().map(capability_value).collect::<Vec<_>>(),
        "rows": report.row_count()
    })
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

#[cfg(test)]
#[path = "jsonrpc/tests.rs"]
mod tests;

//! Standards-compliant MCP JSON-RPC surface over one admitted product session.

use backend_client::Session;
use backend_extension_trustfall::{FieldValue, TransparentValue, execute_view_query};
use backend_library::{
    CommandReply, Document, Fragment, OutlineNode, ReplyDto, Row, RowId, ViewRoot, ViewSnapshot,
    encode_id,
};
use futures_util::StreamExt as _;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

const STABLE_PROTOCOL: &str = "2025-11-25";
const CANDIDATE_PROTOCOL: &str = "2026-07-28";
const SERVER_NAME: &str = "backend";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

trait Product {
    fn revision(&mut self) -> Result<ViewRoot, String>;
    fn packages(&mut self) -> Result<ReplyDto, String>;
    fn index(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn remove(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn search(&mut self, text: &str, limit: u16) -> Result<ReplyDto, String>;
    fn names(&mut self, text: &str, limit: u16) -> Result<ReplyDto, String>;
    fn document(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn outline(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
    fn graph(&mut self, coordinate: &str) -> Result<ReplyDto, String>;
}

impl Product for Session {
    fn revision(&mut self) -> Result<ViewRoot, String> {
        Session::view(self).map_err(|error| error.to_string())
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

    fn outline(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::outline(self, coordinate).map_err(|error| error.to_string())
    }

    fn graph(&mut self, coordinate: &str) -> Result<ReplyDto, String> {
        Session::graph(self, coordinate).map_err(|error| error.to_string())
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

struct Server<P> {
    product: P,
    project: String,
    initialized: bool,
    protocol: &'static str,
}

impl<P: Product> Server<P> {
    fn new(product: P, project: String) -> Self {
        Self {
            product,
            project,
            initialized: false,
            protocol: STABLE_PROTOCOL,
        }
    }

    fn handle(&mut self, input: &[u8]) -> Option<Value> {
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
            if method == "notifications/initialized" {
                self.initialized = true;
            }
            return None;
        }

        if method == "initialize" {
            return Some(match self.initialize(&params) {
                Ok(result) => success(response_id, result),
                Err(error) => error.into_reply(response_id),
            });
        }
        if !self.initialized {
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
        self.initialized = true;
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
            "instructions": "The current project is indexed automatically. Use backend.search to find declarations, then backend.document or backend.graph with the returned coordinate. Every result is pinned to an immutable local revision."
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
                let view = self.product.revision().map_err(RpcError::tool)?;
                return Ok(tool_result(status_value(&view, &self.project), false));
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
            "backend.query" => {
                no_extra(arguments, &["query", "variables", "limit"])?;
                let query = required_string(arguments, "query")?;
                let variables = query_variables(arguments)?;
                let limit = usize::from(limit(arguments)?);
                let view = self.product.revision().map_err(RpcError::tool)?;
                let revision = encode_id(view.root().as_bytes());
                let stream = execute_view_query(view, query, variables)
                    .map_err(|error| RpcError::invalid(error.to_string()))?;
                let rows = futures_executor::block_on(stream.take(limit).collect::<Vec<_>>());
                return Ok(tool_result(
                    json!({
                        "revision": revision,
                        "rows": rows.into_iter().map(query_row_value).collect::<Vec<_>>()
                    }),
                    false,
                ));
            }
            _ => return Err(RpcError::new(-32602, "Unknown tool")),
        };
        match reply {
            Ok(reply) => {
                let (value, is_error) = reply_value(reply);
                Ok(tool_result(value, is_error))
            }
            Err(error) => Ok(tool_result(json!({ "error": error }), true)),
        }
    }

    fn list_resources(&mut self, params: &Value) -> Result<Value, RpcError> {
        empty_cursor(params)?;
        let view = self.product.revision().map_err(RpcError::tool)?;
        let mut resources = vec![json!({
            "uri": "backend://workspace/current",
            "name": "current-workspace",
            "title": "Current indexed workspace",
            "description": "Immutable revision, project count, and declaration count.",
            "mimeType": "application/json"
        })];
        resources.extend(view.rows().iter().filter_map(|row| match row.id {
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
            let view = self.product.revision().map_err(RpcError::tool)?;
            status_value(&view, &self.project)
        } else if let Some(coordinate) = uri.strip_prefix("backend://document/") {
            let coordinate = percent_decode(coordinate)?;
            let reply = self.product.document(&coordinate).map_err(RpcError::tool)?;
            reply_value(reply).0
        } else if let Some(coordinate) = uri.strip_prefix("backend://outline/") {
            let coordinate = percent_decode(coordinate)?;
            let reply = self.product.outline(&coordinate).map_err(RpcError::tool)?;
            reply_value(reply).0
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
    detail: Option<String>,
}

impl RpcError {
    const fn new(code: i64, message: &'static str) -> Self {
        Self {
            code,
            message,
            detail: None,
        }
    }

    fn invalid(detail: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: "Invalid params",
            detail: Some(detail.into()),
        }
    }

    fn tool(detail: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: "Backend request failed",
            detail: Some(detail.into()),
        }
    }

    fn into_reply(self, id: Value) -> Value {
        error_reply(
            id,
            self.code,
            self.message,
            self.detail.map(|detail| json!({ "detail": detail })),
        )
    }
}

fn list_tools(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    let text = |description: &str| json!({ "type": "string", "description": description });
    let limit = json!({ "type": "integer", "minimum": 1, "maximum": 1000, "default": 50 });
    Ok(json!({ "tools": [
        tool("backend.status", "Workspace status", "Read the current immutable revision and index counts.", &json!({}), &[], true, false),
        tool("backend.projects", "Indexed projects", "List projects in the current workspace revision.", &json!({}), &[], true, false),
        tool("backend.search", "Search code", "Search declaration names, signatures, and documentation. Returned coordinates can be passed directly to other tools.", &json!({ "query": text("Text to find."), "limit": limit }), &["query"], true, false),
        tool("backend.names", "Find names", "Find declaration names in the current immutable revision.", &json!({ "query": text("Optional name prefix or substring."), "limit": limit }), &[], true, false),
        tool("backend.document", "Read declaration", "Read the signature and documentation for an exact coordinate returned by search.", &json!({ "coordinate": text("Exact declaration coordinate.") }), &["coordinate"], true, false),
        tool("backend.outline", "Project outline", "Read the declaration tree for a project path.", &json!({ "path": text("Project path; defaults to the active project.") }), &[], true, false),
        tool("backend.graph", "Declaration graph", "Read the bounded graph neighborhood for an exact declaration coordinate.", &json!({ "coordinate": text("Exact declaration coordinate.") }), &["coordinate"], true, false),
        tool("backend.query", "Query the code graph", "Run a typed Trustfall query over the current immutable revision. Starting edges are Item, Project, and Declaration; fields include coordinate, name, kind, signature, documentation, revision, project, parent, children, related, and sameProject.", &json!({ "query": text("Trustfall query text."), "variables": { "type": "object", "description": "Scalar or list query variables.", "additionalProperties": true }, "limit": limit }), &["query"], true, false),
        tool("backend.index", "Refresh project index", "Incrementally index changed files. Unchanged versioned objects are reused.", &json!({ "path": text("Project path; defaults to the active project.") }), &[], false, false),
        tool("backend.remove", "Remove project", "Remove a project and its file frontier from this local workspace.", &json!({ "path": text("Project path; defaults to the active project.") }), &[], false, true)
    ] }))
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

fn query_variables(
    arguments: &Map<String, Value>,
) -> Result<BTreeMap<String, FieldValue>, RpcError> {
    match arguments.get("variables") {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(variables)) => variables
            .iter()
            .map(|(name, value)| query_value(value).map(|value| (name.clone(), value)))
            .collect(),
        Some(_) => Err(RpcError::invalid("variables must be an object")),
    }
}

fn query_value(value: &Value) -> Result<FieldValue, RpcError> {
    match value {
        Value::Null => Ok(FieldValue::Null),
        Value::Bool(value) => Ok((*value).into()),
        Value::Number(value) => value
            .as_i64()
            .map(FieldValue::Int64)
            .or_else(|| value.as_u64().map(FieldValue::Uint64))
            .or_else(|| value.as_f64().map(FieldValue::Float64))
            .ok_or_else(|| RpcError::invalid("query variable number is not finite")),
        Value::String(value) => Ok(value.as_str().into()),
        Value::Array(values) => values
            .iter()
            .map(query_value)
            .collect::<Result<Vec<_>, _>>()
            .map(|values| FieldValue::List(values.into())),
        Value::Object(_) => Err(RpcError::invalid(
            "query variables may be scalars or lists, not objects",
        )),
    }
}

fn query_row_value(row: backend_extension_trustfall::ViewQueryRow) -> Value {
    Value::Object(
        row.into_iter()
            .map(|(name, value)| {
                let value =
                    serde_json::to_value(TransparentValue::from(value)).unwrap_or(Value::Null);
                (name.to_string(), value)
            })
            .collect(),
    )
}

fn object_value(value: Value) -> Value {
    match value {
        Value::Object(_) => value,
        other => json!({ "value": other }),
    }
}

fn reply_value(reply: ReplyDto) -> (Value, bool) {
    match reply.reply {
        CommandReply::Packages(snapshot) => (snapshot_value("projects", &snapshot), false),
        CommandReply::Names(snapshot) => (snapshot_value("names", &snapshot), false),
        CommandReply::Search(snapshot) => (snapshot_value("search", &snapshot), false),
        CommandReply::Graph(snapshot) => (snapshot_value("graph", &snapshot), false),
        CommandReply::Resolved(rows) => (
            json!({ "rows": rows.iter().map(row_value).collect::<Vec<_>>() }),
            false,
        ),
        CommandReply::Added(intent) => (json!({ "indexed": encode_id(intent.as_bytes()) }), false),
        CommandReply::Removed(intent) => {
            (json!({ "removed": encode_id(intent.as_bytes()) }), false)
        }
        CommandReply::Document(document) | CommandReply::Page(document) => {
            (document_value(&document), false)
        }
        CommandReply::Outline(outline) => (
            json!({
                "project": encode_id(outline.package.as_bytes()),
                "revision": encode_id(outline.basis.as_bytes()),
                "root": outline_node_value(&outline.root)
            }),
            false,
        ),
        CommandReply::Health(view) => (status_value(&view, ""), false),
        CommandReply::Revision(receipt) => (
            serde_json::json!({
                "revision": encode_id(receipt.root().as_bytes()),
                "sequence": receipt.cursor().sequence(),
            }),
            false,
        ),
        CommandReply::Error(message) => (json!({ "error": message }), true),
    }
}

fn snapshot_value(kind: &str, snapshot: &ViewSnapshot) -> Value {
    json!({
        "kind": kind,
        "revision": encode_id(snapshot.root.root().as_bytes()),
        "freshness": format!("{:?}", snapshot.freshness).to_lowercase(),
        "rows": snapshot.root.rows().iter().map(row_value).collect::<Vec<_>>(),
        "next": snapshot.next.map(|_| "available")
    })
}

fn row_value(row: &Row) -> Value {
    json!({
        "id": row.id.stable_key(),
        "kind": match row.id { RowId::Package(_) => "project", RowId::Symbol(_) => "declaration", RowId::Object(_) => "object" },
        "coordinate": row.label,
        "name": display_name(&row.label),
        "signature": row.signature,
        "documentation": fragments_text(&row.document),
        "score": row.score,
        "revision": encode_id(row.basis.root.as_bytes())
    })
}

fn document_value(document: &Document) -> Value {
    json!({
        "symbol": encode_id(document.symbol.as_bytes()),
        "revision": encode_id(document.basis.as_bytes()),
        "signature": document.signature,
        "documentation": fragments_text(&document.fragments)
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
    json!({
        "revision": encode_id(view.root().as_bytes()),
        "project": project,
        "projects": projects,
        "declarations": declarations
    })
}

fn fragments_text(fragments: &[Fragment]) -> String {
    let mut output = String::new();
    for fragment in fragments {
        match fragment {
            Fragment::Text(text) | Fragment::Code(text) => output.push_str(text),
            Fragment::Link { label, .. } => output.push_str(label),
            Fragment::Break => output.push('\n'),
        }
    }
    output
}

fn short_root(view: &ViewRoot) -> String {
    encode_id(view.root().as_bytes())[..12].to_owned()
}

fn display_name(coordinate: &str) -> &str {
    coordinate.rsplit("::").next().unwrap_or(coordinate)
}

fn success(id: Value, result: Value) -> Value {
    let mut reply = Map::with_capacity(3);
    reply.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    reply.insert("id".to_owned(), id);
    reply.insert("result".to_owned(), result);
    Value::Object(reply)
}

fn error_reply(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    let mut reply = Map::with_capacity(3);
    reply.insert("jsonrpc".to_owned(), Value::String("2.0".to_owned()));
    reply.insert("id".to_owned(), id);
    reply.insert("error".to_owned(), error);
    Value::Object(reply)
}

fn valid_id(id: &Value) -> bool {
    matches!(id, Value::String(_) | Value::Number(_))
}

fn object(value: &Value) -> Result<&Map<String, Value>, RpcError> {
    value
        .as_object()
        .ok_or_else(|| RpcError::invalid("params must be an object"))
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, RpcError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| RpcError::invalid(format!("{key} must be a non-empty string")))
}

fn required_string<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, RpcError> {
    string(object, key)
}

fn optional_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, RpcError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => Ok(Some(value)),
        Some(_) => Err(RpcError::invalid(format!(
            "{key} must be a non-empty string"
        ))),
    }
}

fn limit(arguments: &Map<String, Value>) -> Result<u16, RpcError> {
    match arguments.get("limit") {
        None => Ok(50),
        Some(value) => value
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .filter(|value| (1..=1000).contains(value))
            .ok_or_else(|| RpcError::invalid("limit must be an integer from 1 through 1000")),
    }
}

fn no_extra(arguments: &Map<String, Value>, allowed: &[&str]) -> Result<(), RpcError> {
    if let Some(key) = arguments
        .keys()
        .find(|key| !allowed.contains(&key.as_str()))
    {
        Err(RpcError::invalid(format!("unknown argument: {key}")))
    } else {
        Ok(())
    }
}

fn empty_cursor(params: &Value) -> Result<(), RpcError> {
    let params = object(params)?;
    if params.get("cursor").is_some_and(|value| !value.is_null()) {
        Err(RpcError::invalid(
            "this bounded list has no continuation cursor",
        ))
    } else {
        Ok(())
    }
}

fn percent_encode(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            output.push(char::from(byte));
        } else {
            output.push('%');
            output.push(hex_digit(byte >> 4));
            output.push(hex_digit(byte & 0x0f));
        }
    }
    output
}

fn percent_decode(value: &str) -> Result<String, RpcError> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let high = bytes.get(at + 1).and_then(|byte| hex_value(*byte));
            let low = bytes.get(at + 2).and_then(|byte| hex_value(*byte));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(RpcError::invalid(
                    "resource URI has invalid percent encoding",
                ));
            };
            output.push(high * 16 + low);
            at += 3;
        } else {
            output.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(output).map_err(|_| RpcError::invalid("resource URI is not UTF-8"))
}

const fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        _ => (b'A' + value - 10) as char,
    }
}

const fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn read_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut output = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return if output.is_empty() {
                Ok(None)
            } else {
                Ok(Some(output))
            };
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let take = end.map_or(available.len(), |position| position + 1);
        if output.len().saturating_add(take) > crate::MAX_FRAME.saturating_add(1) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "MCP message exceeds the bounded frame",
            ));
        }
        output.extend_from_slice(&available[..take]);
        reader.consume(take);
        if end.is_some() {
            output.pop();
            if output.last() == Some(&b'\r') {
                output.pop();
            }
            return Ok(Some(output));
        }
    }
}

fn write_message(writer: &mut impl Write, value: &Value) -> io::Result<()> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    if body.len() > crate::MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MCP response exceeds the bounded frame",
        ));
    }
    writer.write_all(&body)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
#[path = "jsonrpc/tests.rs"]
mod tests;

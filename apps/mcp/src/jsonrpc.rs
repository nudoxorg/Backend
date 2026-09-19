//! Standards-compliant MCP JSON-RPC over the shared presentation model.
//!
//! This surface owns three things and no more: the protocol handshake, the
//! projection of the shared command registry into MCP's tool vocabulary
//! ([`tools`]), and the resources an agent attaches to its context
//! ([`resources`]). What a command *is* lives in [`backend_library::COMMANDS`],
//! what it *takes* lives in `backend_present::GRAMMARS`, which round trips it
//! needs lives in `backend_present::answer`, and what the answer *reads like*
//! lives in `backend_present::markdown`. The CLI reaches all four the same way,
//! which is what makes `backend --format markdown` and a `tools/call` text
//! block byte-identical rather than merely similar.
//!
//! Two rules decide every `tools/call` result:
//!
//! * the text block is `markdown::answer`, and `structuredContent` is
//!   `answer_value` — the same typed DTO the CLI's `--format json` emits;
//! * `isError` is true exactly when the answer is a [`Fault`], and the fault is
//!   rendered in the shared three-line grammar with its affordance as the exact
//!   next tool call an agent can paste back.

use backend_client::{ClientError, Session};
use backend_library::{
    GraphQueryPage, GraphValue, HealthReport, PageTerminal, ReplyDto, SurfaceCommand, SurfaceReply,
    ViewStateRoot, encode_id,
};
use backend_present::{
    Answer, Engine, Fault, Invocation, Probe, Request, answer_value, fault_value, grammar_for_tool,
    lower, markdown,
};
use serde_json::{Map, Value, json};
use std::io::{self, BufRead, Write};

mod codec;
mod reconnect;
mod resources;
mod tools;
use codec::{
    empty_cursor, error_reply, limit, no_extra, object, query_variables, read_line, string, success,
    valid_id, write_message,
};
use tools::{QUERY_TOOL, SURFACE_TOOL, list_tools};

const STABLE_PROTOCOL: &str = "2025-11-25";
const CANDIDATE_PROTOCOL: &str = "2026-07-28";
const SERVER_NAME: &str = "backend";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whatever this server reads product answers from.
///
/// It is the shared driver's [`Engine`] plus the one capability that is not a
/// registry row: the typed Trustfall lane behind `backend.query`.
pub(super) trait Product: Engine {
    fn graph_query(
        &mut self,
        query: String,
        variables: std::collections::BTreeMap<String, GraphValue>,
        limit: u16,
    ) -> Result<GraphQueryPage, ClientError>;
}

/// One connected local daemon session, seen as this server's product.
///
/// The newtype exists because both the trait and the session are foreign to
/// this crate. Its bodies carry no judgement: every arm forwards one probe to
/// the matching session method, and every decision about which probes an answer
/// needs is made once, in the shared driver.
pub(super) struct SessionProduct(Session);

impl SessionProduct {
    pub(super) const fn new(session: Session) -> Self {
        Self(session)
    }
}

impl Engine for SessionProduct {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.0.revision().map(|revision| revision.root)
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        self.0.health()
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Packages => self.0.packages(),
            Probe::Index(path) => self.0.index(path),
            Probe::Remove(path) => self.0.remove(path),
            Probe::Document(at) => self.0.document(at),
            Probe::Source(at) => self.0.source(at),
            Probe::Related(at) => self.0.related(at),
            Probe::Graph(at) => self.0.graph(at),
            Probe::Search { text, limit } => self.0.search(text, limit),
            Probe::Names { text, limit } => self.0.names(text, limit),
            Probe::Outline(path) => self.0.outline(path),
            Probe::OutlinePage { path, limit } => self.0.outline_page(path, limit, None),
        }
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        self.0.surface(command)
    }
}

impl Product for SessionProduct {
    fn graph_query(
        &mut self,
        query: String,
        variables: std::collections::BTreeMap<String, GraphValue>,
        limit: u16,
    ) -> Result<GraphQueryPage, ClientError> {
        self.0.graph_query(query, variables, limit, None, false)
    }
}

/// The local daemon endpoint this server reconnects to.
///
/// The server outlives any one connection: the daemon closes a connection that
/// has been idle for its read timeout, and an agent thinks for longer than
/// that between tool calls. Remembering the endpoint is what lets the next
/// call open a fresh connection instead of failing on a dead one.
struct SessionEndpoint(std::path::PathBuf);

impl reconnect::Endpoint for SessionEndpoint {
    type Product = SessionProduct;

    fn connect(&mut self) -> Result<Self::Product, ClientError> {
        Session::connect(&self.0).map(SessionProduct::new)
    }
}

/// Runs newline-delimited MCP stdio until the client closes stdin.
pub(super) fn serve_stdio(
    session: Session,
    project: String,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> io::Result<()> {
    let endpoint = SessionEndpoint(session.endpoint().to_path_buf());
    let product = reconnect::Reconnecting::new(endpoint, SessionProduct::new(session));
    let mut server = Server::new(product, project);
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
        Some(match self.route(method, &params) {
            Ok(result) => success(response_id, result),
            Err(error) => error.into_reply(response_id),
        })
    }

    fn route(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "ping" => Ok(json!({})),
            "tools/list" => list_tools(params),
            "tools/call" => self.call_tool(params),
            "resources/list" => resources::list_resources(&mut self.product, params),
            "resources/templates/list" => resources::list_resource_templates(params),
            "resources/read" => resources::read_resource(&mut self.product, params),
            "prompts/list" => list_prompts(params),
            "prompts/get" => self.get_prompt(params),
            _ => Err(RpcError::new(-32601, "Method not found")),
        }
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
            "instructions": INSTRUCTIONS
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
        if name == QUERY_TOOL {
            return self.query_tool(arguments);
        }
        if name == SURFACE_TOOL {
            return self.surface_tool(arguments);
        }
        let Some(grammar) = grammar_for_tool(name) else {
            return Err(RpcError::new(-32602, "Unknown tool"));
        };
        let planned = Invocation::from_json(grammar, arguments)
            .and_then(|invocation| lower(&invocation, &self.project));
        Ok(match planned {
            Ok(request) => match backend_present::answer(&mut self.product, &request) {
                Ok(answer) => rendered(&answer),
                Err(fault) => refused(&fault),
            },
            Err(fault) => refused(&fault),
        })
    }

    fn query_tool(&mut self, arguments: &Map<String, Value>) -> Result<Value, RpcError> {
        no_extra(arguments, &["query", "variables", "limit"])?;
        let query = string(arguments, "query")?.to_owned();
        let variables = query_variables(arguments)?;
        Ok(
            match self.product.graph_query(query.clone(), variables, limit(arguments)?) {
                Ok(page) => graph_page_result(&page),
                Err(error) => refused(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Text(query),
                )),
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
        let reply = self
            .product
            .surface(command)
            .map_err(|error| RpcError::tool(error.to_string()))?;
        let view = backend_present::product_view(&reply);
        Ok(json!({
            "content": [{ "type": "text", "text": markdown::product(&view) }],
            "structuredContent": { "surface": reply },
            "isError": false
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
        let view = Engine::revision(&mut self.product)
            .map_err(|error| RpcError::tool(error.to_string()))?;
        Ok(json!({
            "description": "Explore the current immutable code index.",
            "messages": [{
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!(
                        "Explore {query} in the indexed project. Search broadly, open the most relevant declarations, inspect their graph neighbors, and ground the answer in revision {}.",
                        abbreviate(view.as_bytes())
                    )
                }
            }]
        }))
    }
}

const INSTRUCTIONS: &str = "Every tool is one row of one command registry, grouped by domain in \
tools/list. Read backend://workspace/current first: it says in four lines which lanes are ready \
and which are simply not configured, so a thin answer is never mistaken for an empty one. Then \
backend.search or backend.outline to find a coordinate, and backend.document to read it — a \
coordinate returned by any tool is accepted verbatim by every other. Answers are Markdown in the \
text block and the same typed values in structuredContent. A refusal names the exact operand it \
refused and the next tool call to make.";

/// Returns the readable head of one stable identity.
fn abbreviate(bytes: &[u8; 32]) -> String {
    encode_id(bytes).chars().take(12).collect()
}

/// Renders one answer: Markdown for a reader, the typed DTO for a program.
fn rendered(answer: &Answer) -> Value {
    json!({
        "content": [{ "type": "text", "text": markdown::answer(answer) }],
        "structuredContent": answer_value(answer),
        "isError": false
    })
}

/// Renders one fault in the shared three-line grammar.
fn refused(fault: &Fault) -> Value {
    json!({
        "content": [{ "type": "text", "text": markdown::fault(fault) }],
        "structuredContent": fault_value(fault),
        "isError": true
    })
}

fn graph_page_result(page: &GraphQueryPage) -> Value {
    let value = graph_query_page_value(page);
    json!({
        "content": [{ "type": "text", "text": graph_page_text(page) }],
        "structuredContent": value,
        "isError": false
    })
}

fn graph_page_text(page: &GraphQueryPage) -> String {
    let mut out = format!(
        "~query {} row(s) · {} · revision {}\n",
        page.rows.len(),
        terminal_name(page.terminal),
        abbreviate(page.revision.as_bytes())
    );
    for row in &page.rows {
        let fields = row
            .fields()
            .iter()
            .map(|(name, value)| format!("{name}={}", scalar_text(value)))
            .collect::<Vec<_>>();
        out.push_str(&fields.join("  "));
        out.push('\n');
    }
    if page.terminal != PageTerminal::Complete {
        out.push_str("… raise `limit` or narrow the query to see the rest\n");
    }
    out
}

fn scalar_text(value: &GraphValue) -> String {
    match value {
        GraphValue::String(text) => text.clone(),
        GraphValue::Null => "·".to_owned(),
        other => graph_value(other).to_string(),
    }
}

const fn terminal_name(terminal: PageTerminal) -> &'static str {
    match terminal {
        PageTerminal::Complete => "complete",
        PageTerminal::More(_) => "limit_reached",
        PageTerminal::Cancelled => "cancelled",
    }
}

/// Lifts one answer for a resource read, where a fault has no `isError` to go in.
fn answer_for(engine: &mut dyn Engine, request: &Request) -> Result<Answer, RpcError> {
    backend_present::answer(engine, request).map_err(|fault| RpcError::from_fault(&fault))
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

    /// Lowers one shared fault into the JSON-RPC error a resource read reports.
    fn from_fault(fault: &Fault) -> Self {
        Self {
            code: if fault.slug().is_usage() { -32602 } else { -32603 },
            message: "Backend request failed",
            kind: fault.slug().as_str(),
            detail: Some(markdown::fault(fault)),
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

fn list_prompts(params: &Value) -> Result<Value, RpcError> {
    empty_cursor(params)?;
    Ok(json!({ "prompts": [{
        "name": "backend.explore",
        "title": "Explore indexed code",
        "description": "Search, read, and trace declarations from one immutable revision.",
        "arguments": [{ "name": "query", "description": "Feature or concept to investigate.", "required": false }]
    }] }))
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
    json!({
        "answer": "query",
        "revision": encode_id(page.revision.as_bytes()),
        "rows": page.rows.iter().map(|row| Value::Object(
            row.fields().iter().map(|(name, value)| (name.clone(), graph_value(value))).collect()
        )).collect::<Vec<_>>(),
        "terminal": terminal_name(page.terminal),
        "emitted": page.rows.len()
    })
}

#[cfg(test)]
#[path = "jsonrpc/tests.rs"]
mod tests;

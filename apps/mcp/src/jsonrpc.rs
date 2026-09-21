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
//! * the text block is `markdown::answer`, and `structuredContent` uses the
//!   same typed projection the CLI's `--format json` emits;
//! * `isError` is true exactly when the answer is a [`Fault`], and the fault is
//!   rendered in the shared three-line grammar with its affordance as the exact
//!   next tool call an agent can paste back.

use backend_client::{ClientError, Session};
use backend_library::{
    GraphQueryPage, GraphQueryRow, GraphValue, HealthReport, PageContinuation, PageTerminal,
    ReplyDto, SurfaceCommand, SurfaceReply, ViewStateRoot, encode_id,
};
use backend_present::{
    Answer, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, Engine, Fault, Invocation, Probe, Request,
    answer_paged, encode_answer, encode_serializable, fault_value, grammar_for_tool, lower,
    markdown, oversized_fault, record_list,
};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

mod codec;
mod reconnect;
mod resources;
mod tools;
use codec::{
    empty_cursor, error_reply, limit, no_extra, object, query_variables, read_line, string,
    success, valid_id, write_message,
};
use tools::{QUERY_TOOL, SURFACE_TOOL, list_tools};

pub(crate) use tools::token_budget_tools as token_budget_tools_projection;

const STABLE_PROTOCOL: &str = "2025-11-25";
const CANDIDATE_PROTOCOL: &str = "2026-07-28";
const SERVER_NAME: &str = "backend";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whatever this server reads product answers from.
///
/// It is the shared driver's [`Engine`] plus the one capability that is not a
/// registry row: the typed Trustfall lane behind `backend.query`.
pub(super) trait Product: Engine {
    /// Encodes an owner-issued continuation as a self-describing token.
    fn encode_continuation(
        &mut self,
        continuation: PageContinuation,
    ) -> Result<String, ClientError>;

    /// Admits a self-describing continuation against the current owner.
    fn decode_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError>;

    /// Reads one bounded graph-neighborhood page.
    fn graph_page(
        &mut self,
        coordinate: String,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let _ = (coordinate, limit, continuation);
        Err(ClientError::Protocol(
            "this product does not expose graph pagination".to_owned(),
        ))
    }

    fn graph_query(
        &mut self,
        query: String,
        variables: std::collections::BTreeMap<String, GraphValue>,
        limit: u16,
        continuation: Option<PageContinuation>,
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

    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Search { text, limit } => self.0.search_page(text, limit, continuation),
            Probe::Names { text, limit } => self.0.names_page(text, limit, continuation),
            _ if continuation.is_none() => self.probe(probe),
            _ => Err(ClientError::Protocol(
                "this MCP command does not support continuation pages".to_owned(),
            )),
        }
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        self.0.surface(command)
    }
}

impl Product for SessionProduct {
    fn encode_continuation(
        &mut self,
        continuation: PageContinuation,
    ) -> Result<String, ClientError> {
        Ok(self.0.encode_page_continuation(continuation))
    }

    fn decode_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        self.0.decode_page_continuation(token)
    }

    fn graph_page(
        &mut self,
        coordinate: String,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.0.graph_page(&coordinate, limit, continuation)
    }

    fn graph_query(
        &mut self,
        query: String,
        variables: std::collections::BTreeMap<String, GraphValue>,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<GraphQueryPage, ClientError> {
        self.0
            .graph_query(query, variables, limit, continuation, false)
    }
}

/// The local daemon endpoint this server reconnects to.
///
/// The server outlives any one connection: the daemon closes a connection that
/// has been idle for its read timeout, and an agent thinks for longer than
/// that between tool calls. Remembering the endpoint is what lets the next
/// call open a fresh connection instead of failing on a dead one.
struct SessionEndpoint {
    /// The complete runtime selection, retained for reconnects that need to
    /// compose a daemon after its previous process exited.
    paths: backend_runtime::WorkspacePaths,
}

impl reconnect::Endpoint for SessionEndpoint {
    type Product = SessionProduct;

    fn connect(&mut self) -> Result<Self::Product, ClientError> {
        let endpoint =
            backend_runtime::ensure_locald(&self.paths).map_err(map_runtime_connect_error)?;
        Session::connect(endpoint).map(SessionProduct::new)
    }
}

/// Converts a daemon restart window into the same reconnectable class as a
/// reset on an established stream. Startup/configuration failures remain
/// ordinary client errors so a broken workspace is reported directly instead
/// of being retried forever by a long-lived MCP process.
fn map_runtime_connect_error(error: backend_runtime::RuntimeError) -> ClientError {
    match error {
        backend_runtime::RuntimeError::Io(error)
            if is_disconnect_kind(error.kind()) || error.kind() == io::ErrorKind::NotFound =>
        {
            ClientError::Disconnected(error.kind())
        }
        other => ClientError::Io(other.to_string()),
    }
}

const fn is_disconnect_kind(kind: io::ErrorKind) -> bool {
    matches!(
        kind,
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::TimedOut
            | io::ErrorKind::WouldBlock
    )
}

/// Runs newline-delimited MCP stdio until the client closes stdin.
pub(super) fn serve_stdio(
    session: Session,
    paths: &backend_runtime::WorkspacePaths,
    project: String,
    cursor_secret: [u8; 32],
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> io::Result<()> {
    let endpoint = SessionEndpoint {
        paths: paths.clone(),
    };
    let product = reconnect::Reconnecting::new(endpoint, SessionProduct::new(session));
    let working_directory = std::env::current_dir()
        .ok()
        .map(backend_runtime::normalize_surface_path)
        .map(|path| path.to_string_lossy().into_owned());
    let mut server =
        Server::with_invocation_context(product, project, working_directory, cursor_secret);
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
    working_directory: Option<String>,
    handshake: HandshakeState,
    protocol: &'static str,
    cursor_secret: [u8; 32],
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
    pub(super) fn with_authority(product: P, project: String, cursor_secret: [u8; 32]) -> Self {
        Self::with_invocation_context(product, project, None, cursor_secret)
    }

    pub(super) fn with_invocation_context(
        product: P,
        project: String,
        working_directory: Option<String>,
        cursor_secret: [u8; 32],
    ) -> Self {
        Self {
            product,
            project,
            working_directory,
            handshake: HandshakeState::AwaitInitialize,
            protocol: STABLE_PROTOCOL,
            cursor_secret,
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
            "resources/read" => self.read_resource(params),
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
            "instructions": self.instructions()
        }))
    }

    /// Adds the selected project and launch directory to the handshake so a
    /// host can diagnose a stale project setting before querying an empty
    /// index.
    fn instructions(&self) -> String {
        let mut instructions = String::with_capacity(INSTRUCTIONS.len() + self.project.len() + 160);
        instructions.push_str(INSTRUCTIONS);
        instructions.push_str("\n\nMCP workspace selection:\n- selected project: ");
        instructions.push_str(&self.project);
        match self.working_directory.as_deref() {
            Some(directory) if directory != self.project => {
                instructions.push_str("\n- process working directory: ");
                instructions.push_str(directory);
                instructions.push_str(
                    "\n- status: configuration mismatch; use the selected project path when interpreting relative coordinates",
                );
            }
            Some(directory) => {
                instructions.push_str("\n- process working directory: ");
                instructions.push_str(directory);
                instructions.push_str("\n- status: configuration matches");
            }
            None => instructions.push_str("\n- process working directory: unavailable"),
        }
        instructions
    }

    fn read_resource(&mut self, params: &Value) -> Result<Value, RpcError> {
        let is_workspace = params
            .get("uri")
            .and_then(Value::as_str)
            .is_some_and(|uri| uri == "backend://workspace/current");
        let mut value = resources::read_resource(&mut self.product, params)?;
        // The legacy constructor intentionally keeps the resource byte-for-
        // byte identical to `backend.status`; only process sessions that
        // supplied launch context need the extra diagnostic block.
        if !is_workspace || self.working_directory.is_none() {
            return Ok(value);
        }
        let Some(text) = value
            .get("contents")
            .and_then(Value::as_array)
            .and_then(|contents| contents.first())
            .and_then(|content| content.get("text"))
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return Ok(value);
        };
        if let Some(content) = value
            .get_mut("contents")
            .and_then(Value::as_array_mut)
            .and_then(|contents| contents.first_mut())
            .and_then(|content| content.get_mut("text"))
        {
            *content = Value::String(format!("{text}\n\n{}", self.instructions_context()));
        }
        Ok(value)
    }

    fn instructions_context(&self) -> String {
        let mut context = String::from("## MCP workspace selection\n\n- selected project: ");
        context.push_str(&self.project);
        match self.working_directory.as_deref() {
            Some(directory) if directory != self.project => {
                context.push_str("\n- process working directory: ");
                context.push_str(directory);
                context.push_str("\n- status: configuration mismatch");
            }
            Some(directory) => {
                context.push_str("\n- process working directory: ");
                context.push_str(directory);
                context.push_str("\n- status: configuration matches");
            }
            None => context.push_str("\n- process working directory: unavailable"),
        }
        context
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
        let detail = response_detail(name, arguments)?;
        let context = continuation_context(&self.project, name, arguments, detail);
        let continuation = self.continuation(arguments, &context)?;
        if name == QUERY_TOOL {
            return self.query_tool(arguments, detail, continuation, &context);
        }
        if name == "backend.graph"
            && (arguments.contains_key("limit") || arguments.contains_key("cursor"))
        {
            return self.graph_tool(arguments, detail, continuation, &context);
        }
        if name == SURFACE_TOOL {
            if continuation.is_some() {
                return Err(RpcError::invalid(
                    "cursor is only valid for a paged search or graph query",
                ));
            }
            let mut surface_arguments = arguments.clone();
            surface_arguments.remove("detail");
            return self.surface_tool(&surface_arguments, detail);
        }
        let Some(grammar) = grammar_for_tool(name) else {
            return Err(RpcError::new(-32602, "Unknown tool"));
        };
        let mut command_arguments = arguments.clone();
        command_arguments.remove("detail");
        command_arguments.remove("cursor");
        let planned = Invocation::from_json(grammar, &command_arguments)
            .and_then(|invocation| lower(&invocation, &self.project));
        match planned {
            Ok(request) => match answer_paged(&mut self.product, &request, continuation) {
                Ok(answer) => self.rendered(&answer, detail, &context),
                Err(fault) => Ok(refused(&fault)),
            },
            Err(fault) => Ok(refused(&fault)),
        }
    }

    fn query_tool(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
        continuation: Option<PageContinuation>,
        context: &[u8],
    ) -> Result<Value, RpcError> {
        no_extra(
            arguments,
            &["query", "variables", "limit", "cursor", "detail"],
        )?;
        let query = string(arguments, "query")?.to_owned();
        let variables = query_variables(arguments)?;
        match self
            .product
            .graph_query(query.clone(), variables, limit(arguments)?, continuation)
        {
            Ok(page) => self.graph_page_result(&page, detail, context),
            Err(error) => Ok(refused(&Fault::from_client_error(
                &error,
                backend_present::Operand::Text(query),
            ))),
        }
    }

    fn graph_tool(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
        continuation: Option<PageContinuation>,
        context: &[u8],
    ) -> Result<Value, RpcError> {
        no_extra(arguments, &["coordinate", "limit", "cursor", "detail"])?;
        let coordinate = string(arguments, "coordinate")?.to_owned();
        let reply = self
            .product
            .graph_page(coordinate.clone(), limit(arguments)?, continuation)
            .map_err(|error| RpcError::tool(error.to_string()))?;
        let backend_library::CommandReply::ProjectionPage(page) = reply.reply else {
            return Err(RpcError::tool("graph page reply changed shape"));
        };
        let answer = Answer::Records(Box::new(record_list(&coordinate, &page.snapshot)));
        self.rendered(&answer, detail, context)
    }

    fn surface_tool(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
    ) -> Result<Value, RpcError> {
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
        let payload = match encode_serializable(
            "surface",
            detail,
            None,
            SurfaceBody { surface: &reply },
            DEFAULT_RESPONSE_BUDGET_BYTES,
        ) {
            Ok(payload) => payload,
            Err(error) => return Ok(refused(&oversized_fault(error))),
        };
        let structured: Value = serde_json::from_slice(&payload.bytes).map_err(|error| {
            RpcError::tool(format!("typed surface projection decode failed: {error}"))
        })?;
        Ok(json!({
            "content": [{ "type": "text", "text": bounded_text(&markdown::product(&view)) }],
            "structuredContent": structured,
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

    fn continuation(
        &mut self,
        arguments: &Map<String, Value>,
        context: &[u8],
    ) -> Result<Option<PageContinuation>, RpcError> {
        let Some(value) = arguments.get("cursor") else {
            return Ok(None);
        };
        let token = value
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| RpcError::invalid("cursor must be a non-empty opaque string"))?;
        let owner_token = self.verify_cursor_token(token, context).ok_or_else(|| {
            RpcError::invalid("cursor is unknown, expired, or belongs to another MCP session")
        })?;
        self.product
            .decode_continuation(owner_token)
            .map(Some)
            .map_err(|error| match error {
                ClientError::StaleCursor => RpcError::stale_cursor(),
                _ => RpcError::invalid(
                    "cursor is unknown, expired, or belongs to another MCP session",
                ),
            })
    }

    fn issue_continuation(
        &mut self,
        continuation: PageContinuation,
        context: &[u8],
    ) -> Result<String, RpcError> {
        let owner_token = self
            .product
            .encode_continuation(continuation)
            .map_err(|error| RpcError::tool(error.to_string()))?;
        Ok(self.sign_cursor_token(&owner_token, context))
    }

    fn sign_cursor_token(&self, owner_token: &str, context: &[u8]) -> String {
        self.sign_cursor_token_at(unix_seconds().saturating_add(900), owner_token, context)
    }

    fn sign_cursor_token_at(&self, expiry: u64, owner_token: &str, context: &[u8]) -> String {
        let body = format!("{expiry}-{owner_token}");
        let mac = self.cursor_mac(&body, context);
        format!("mcp1-{body}-{}", hex_bytes(&mac.as_bytes()[..16]))
    }

    fn verify_cursor_token<'a>(&self, token: &'a str, context: &[u8]) -> Option<&'a str> {
        let body = token.strip_prefix("mcp1-")?;
        let (body, encoded_mac) = body.rsplit_once('-')?;
        let (expiry, owner_token) = body.split_once('-')?;
        let expiry = expiry.parse::<u64>().ok()?;
        // Treat the expiry second as closed: a token is valid strictly before
        // its deadline. This avoids a one-second replay window at the exact
        // boundary and makes rotation/expiry tests deterministic.
        if expiry <= unix_seconds() {
            return None;
        }
        let expected = self.cursor_mac(body, context);
        if encoded_mac != hex_bytes(&expected.as_bytes()[..16]) {
            return None;
        }
        Some(owner_token)
    }

    fn cursor_mac(&self, body: &str, context: &[u8]) -> blake3::Hash {
        let mut payload = Vec::with_capacity(body.len() + context.len() + 1);
        payload.extend_from_slice(body.as_bytes());
        payload.push(0);
        payload.extend_from_slice(context);
        blake3::keyed_hash(&self.cursor_secret, &payload)
    }

    fn rendered(
        &mut self,
        answer: &Answer,
        detail: Detail,
        context: &[u8],
    ) -> Result<Value, RpcError> {
        let next = answer
            .continuation()
            .map(|cursor| self.issue_continuation(cursor, context))
            .transpose()?;
        let payload = match encode_answer(
            answer,
            detail,
            next.as_deref(),
            DEFAULT_RESPONSE_BUDGET_BYTES,
        ) {
            Ok(payload) => payload,
            Err(error) => return Ok(refused(&oversized_fault(error))),
        };
        let structured: Value = serde_json::from_slice(&payload.bytes)
            .map_err(|error| RpcError::tool(format!("typed projection decode failed: {error}")))?;
        let text = bounded_text(&markdown::answer(answer));
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": structured,
            "isError": false
        }))
    }

    fn graph_page_result(
        &mut self,
        page: &GraphQueryPage,
        detail: Detail,
        context: &[u8],
    ) -> Result<Value, RpcError> {
        let next = match page.terminal {
            PageTerminal::More(cursor) => Some(self.issue_continuation(cursor, context)?),
            PageTerminal::Complete | PageTerminal::Cancelled => None,
        };
        let payload = match encode_serializable(
            "query",
            detail,
            next.as_deref(),
            GraphPageBody { page },
            DEFAULT_RESPONSE_BUDGET_BYTES,
        ) {
            Ok(payload) => payload,
            Err(error) => return Ok(refused(&oversized_fault(error))),
        };
        let value: Value = serde_json::from_slice(&payload.bytes).map_err(|error| {
            RpcError::tool(format!("typed graph projection decode failed: {error}"))
        })?;
        Ok(json!({
            "content": [{ "type": "text", "text": bounded_text(&graph_page_text(page)) }],
            "structuredContent": value,
            "isError": false
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

pub(super) fn read_authority_secret(path: &std::path::Path) -> Result<[u8; 32], String> {
    backend_engine::read_authority_secret(path).map_err(|error| {
        format!(
            "cannot admit MCP authority secret {}: {error}",
            path.display()
        )
    })
}

fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn continuation_context(
    workspace: &str,
    name: &str,
    arguments: &Map<String, Value>,
    detail: Detail,
) -> Vec<u8> {
    let mut canonical = BTreeMap::new();
    canonical.insert("detail", Value::String(detail.name().to_owned()));
    canonical.insert("schema", Value::String("mcp1".to_owned()));
    canonical.insert("tool", Value::String(name.to_owned()));
    canonical.insert("workspace", Value::String(workspace.to_owned()));
    for (key, value) in arguments {
        if key != "cursor" {
            canonical.insert(key.as_str(), normalize_context_value(key, value));
        }
    }
    serde_json::to_vec(&canonical).unwrap_or_default()
}

/// Canonicalizes caller text before it enters a cursor MAC.
///
/// Search and Trustfall clients commonly differ only in surrounding or
/// repeated whitespace. Treating those spellings as the same query keeps a
/// continuation stable across transports while retaining case, punctuation,
/// variables, and coordinate identity exactly. All other values are retained
/// recursively so a cursor cannot cross a limit, variable, or projection
/// boundary by accident.
fn normalize_context_value(key: &str, value: &Value) -> Value {
    match value {
        Value::String(text) if matches!(key, "query" | "text" | "coordinate") => {
            let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
            Value::String(normalized)
        }
        Value::Array(values) => Value::Array(
            values
                .iter()
                .map(|value| normalize_context_value(key, value))
                .collect(),
        ),
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(field, value)| (field.clone(), normalize_context_value(field, value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Renders one answer: Markdown for a reader, the typed DTO for a program.
/// Renders one fault in the shared three-line grammar.
fn refused(fault: &Fault) -> Value {
    json!({
        "content": [{ "type": "text", "text": markdown::fault(fault) }],
        "structuredContent": fault_value(fault),
        "isError": true
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

    fn stale_cursor() -> Self {
        Self {
            code: -32010,
            message: "Stale cursor",
            kind: "stale_cursor",
            detail: Some(
                "the continuation belongs to an older immutable revision; restart the query"
                    .to_owned(),
            ),
        }
    }

    /// Lowers one shared fault into the JSON-RPC error a resource read reports.
    fn from_fault(fault: &Fault) -> Self {
        Self {
            code: if fault.slug().is_usage() {
                -32602
            } else {
                -32603
            },
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

struct GraphPageBody<'a> {
    page: &'a GraphQueryPage,
}

impl Serialize for GraphPageBody<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("revision", &encode_id(self.page.revision.as_bytes()))?;
        map.serialize_entry("rows", &GraphRows(&self.page.rows))?;
        map.serialize_entry("terminal", terminal_name(self.page.terminal))?;
        map.serialize_entry("emitted", &self.page.rows.len())?;
        map.end()
    }
}

struct GraphRows<'a>(&'a [GraphQueryRow]);

impl Serialize for GraphRows<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            sequence.serialize_element(&GraphRow(row))?;
        }
        sequence.end()
    }
}

struct GraphRow<'a>(&'a GraphQueryRow);

impl Serialize for GraphRow<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.fields().len()))?;
        for (name, value) in self.0.fields() {
            map.serialize_entry(name, &GraphValueRef(value))?;
        }
        map.end()
    }
}

struct GraphValueRef<'a>(&'a GraphValue);

impl Serialize for GraphValueRef<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.0 {
            GraphValue::Null => serializer.serialize_unit(),
            GraphValue::Boolean(value) => serializer.serialize_bool(*value),
            GraphValue::Signed(value) => serializer.serialize_i64(*value),
            GraphValue::Unsigned(value) => serializer.serialize_u64(*value),
            GraphValue::Float(_) => match self.0.as_float() {
                Some(value) => serializer.serialize_f64(value),
                None => serializer.serialize_unit(),
            },
            GraphValue::String(value) => serializer.serialize_str(value),
            GraphValue::List(values) => {
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(&GraphValueRef(value))?;
                }
                sequence.end()
            }
        }
    }
}

fn response_detail(tool: &str, arguments: &Map<String, Value>) -> Result<Detail, RpcError> {
    let Some(value) = arguments.get("detail") else {
        return Ok(default_detail(tool));
    };
    let value = value
        .as_str()
        .ok_or_else(|| RpcError::invalid("detail must be summary, standard, or full"))?;
    Detail::parse(value)
        .ok_or_else(|| RpcError::invalid("detail must be summary, standard, or full"))
}

/// Returns the smallest projection that fulfils a tool's advertised promise.
///
/// A document or source lookup exists to return code, so reducing it to an
/// identity-only summary by default makes a successful call unusable and
/// forces a second round trip. Discovery and collection tools retain the
/// context-efficient summary default; callers may still explicitly request
/// any detail level for every tool.
pub(super) fn default_detail(tool: &str) -> Detail {
    match tool {
        "backend.document" | "backend.source" => Detail::Standard,
        _ => Detail::Summary,
    }
}

fn bounded_text(text: &str) -> String {
    // Keep the human-readable duplicate small enough that it cannot consume
    // the context budget beside the typed projection. The projection is the
    // complete machine-readable answer; Markdown remains a bounded preview.
    const MAX_TEXT_BYTES: usize = 16 * 1024;
    if text.len() <= MAX_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_TEXT_BYTES.saturating_sub(32);
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n\n… output truncated; request a narrower page or detail=summary",
        &text[..end]
    )
}

#[derive(Serialize)]
struct SurfaceBody<'a> {
    surface: &'a SurfaceReply,
}

#[cfg(test)]
#[path = "jsonrpc/tests.rs"]
mod tests;

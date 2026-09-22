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
    AdmittedGraphQueryInput, GraphQueryPage, GraphQueryRow, GraphValue, HealthReport,
    PageContinuation, PageTerminal, ReplyDto, SurfaceCommand, SurfaceReply, ViewStateRoot,
    encode_id,
};
use backend_present::{
    Answer, BudgetExceeded, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, Engine, Fault, Invocation,
    Probe, Request, answer_paged, bounded_text, encode_answer, encode_serializable, fault_value,
    grammar_for_tool, lower, markdown, oversized_fault, record_list,
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
    empty_cursor, error_reply, json_depth_within, limit, no_extra, object, query_variables,
    read_line, string, success, valid_id, write_message,
};
use tools::{QUERY_TOOL, SURFACE_TOOL, list_tools};

#[cfg(feature = "token-budget")]
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
        input: AdmittedGraphQueryInput,
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

    fn into_session(self) -> Session {
        self.0
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
        input: AdmittedGraphQueryInput,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<GraphQueryPage, ClientError> {
        self.0
            .graph_query_admitted(input, limit, continuation, false)
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
    /// Bounded continuation state retained if the daemon is still between
    /// leases and the replacement dial has not succeeded yet.
    pending_continuation_state: Option<backend_client::SessionContinuationState>,
}

impl reconnect::Endpoint for SessionEndpoint {
    type Product = SessionProduct;

    fn connect(&mut self, previous: Option<Self::Product>) -> Result<Self::Product, ClientError> {
        let state = previous
            .map(SessionProduct::into_session)
            .map(|mut session| session.take_continuation_state())
            .or_else(|| self.pending_continuation_state.take());
        let endpoint = match backend_runtime::ensure_locald(&self.paths) {
            Ok(endpoint) => endpoint,
            Err(error) => {
                self.pending_continuation_state = state;
                return Err(map_runtime_connect_error(error));
            }
        };
        let mut session = match Session::connect(endpoint) {
            Ok(session) => session,
            Err(error) => {
                self.pending_continuation_state = state;
                return Err(error);
            }
        };
        if let Some(state) = state {
            session.restore_continuation_state(state);
        }
        Ok(SessionProduct::new(session))
    }

    fn retire(&mut self, previous: Self::Product) {
        let mut session = previous.into_session();
        self.pending_continuation_state = Some(session.take_continuation_state());
    }
}

pub(super) type ReconnectingProduct = reconnect::Reconnecting<SessionEndpoint>;

pub(super) fn reconnecting_product(
    session: Session,
    paths: &backend_runtime::WorkspacePaths,
) -> ReconnectingProduct {
    reconnect::Reconnecting::new(
        SessionEndpoint {
            paths: paths.clone(),
            pending_continuation_state: None,
        },
        SessionProduct::new(session),
    )
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
        pending_continuation_state: None,
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
        self.handle_inner(input).map(bound_rpc_reply)
    }

    fn handle_inner(&mut self, input: &[u8]) -> Option<Value> {
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
        if !json_depth_within(&value) {
            return Some(error_reply(
                Value::Null,
                -32600,
                "Invalid Request",
                Some(json!({ "detail": "JSON nesting exceeds the bounded request depth" })),
            ));
        }
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
        Some(
            match self.route(method, &params).and_then(bound_route_value) {
                Ok(result) => success(response_id, result),
                Err(error) => error.into_reply(response_id),
            },
        )
    }

    fn route(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        // Product-bearing tools/resources perform typed admission before they
        // become `Value`s. The registry and protocol metadata are finite
        // constructors (with their schema budget checked in the token audit);
        // this final gate covers their JSON-RPC envelope as well.
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
        let project = bounded_text(&self.project);
        let directory = self.working_directory.as_deref().map(bounded_text);
        let mut instructions = String::with_capacity(INSTRUCTIONS.len() + project.len() + 160);
        instructions.push_str(INSTRUCTIONS);
        instructions.push_str("\n\nMCP workspace selection:\n- selected project: ");
        instructions.push_str(&project);
        match directory.as_deref() {
            Some(directory) if directory != project => {
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
        let input = AdmittedGraphQueryInput::new(
            string(arguments, "query")?.to_owned(),
            query_variables(arguments)?,
        )
        .map_err(|error| RpcError::invalid(error.to_string()))?;
        let query = input.query().to_owned();
        match self
            .product
            .graph_query(input, limit(arguments)?, continuation)
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
        Ok(tool_result(
            &bounded_text(&markdown::product(&view)),
            structured,
            false,
        ))
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
            .map_or_else(|| "the relevant implementation".to_owned(), bounded_text);
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
        Ok(tool_result(&text, structured, false))
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
        Ok(tool_result(
            &bounded_text(&graph_page_text(page)),
            value,
            false,
        ))
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
            canonical.insert(key.as_str(), value.clone());
        }
    }
    serde_json::to_vec(&canonical).unwrap_or_default()
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
    tool_result(
        &bounded_text(&markdown::fault(fault)),
        fault_value(fault),
        true,
    )
}

/// Context-sized ceiling for a complete JSON-RPC reply. The transport frame
/// limit is intentionally much larger; this is the product budget that keeps
/// a typed projection and its readable duplicate bounded together.
const MCP_RESULT_BUDGET_BYTES: usize = DEFAULT_RESPONSE_BUDGET_BYTES;

fn serialized_bytes(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(MCP_RESULT_BUDGET_BYTES.saturating_add(1), |bytes| {
        bytes.len()
    })
}

fn bound_route_value(value: Value) -> Result<Value, RpcError> {
    let bytes = serialized_bytes(&value);
    if bytes <= MCP_RESULT_BUDGET_BYTES {
        return Ok(value);
    }
    Err(RpcError::from_fault(&oversized_fault(BudgetExceeded {
        bytes,
        budget: MCP_RESULT_BUDGET_BYTES,
    })))
}

fn bound_rpc_reply(value: Value) -> Value {
    let bytes = serialized_bytes(&value);
    if bytes <= MCP_RESULT_BUDGET_BYTES {
        return value;
    }
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let fault = oversized_fault(BudgetExceeded {
        bytes,
        budget: MCP_RESULT_BUDGET_BYTES,
    });
    let fallback = error_reply(
        id.clone(),
        -32000,
        "MCP response exceeds the bounded context budget",
        Some(json!({
            "kind": fault.slug().as_str(),
            "detail": bounded_text(&markdown::fault(&fault)),
            "structuredContent": fault_value(&fault),
        })),
    );
    if serialized_bytes(&fallback) <= MCP_RESULT_BUDGET_BYTES {
        fallback
    } else {
        error_reply(
            // A request ID is normally a scalar, but preserve the hard cap
            // even if a caller supplied a very large valid JSON string.
            Value::Null,
            -32000,
            "MCP response exceeds the bounded context budget",
            None,
        )
    }
}

/// Serialize the exact bounded response envelope used by `tools/call`.
///
/// This narrow dev hook lets the checked-in budget generator exercise the
/// production `tool_result` and `bound_rpc_reply` paths without making the
/// fixture generator depend on a live daemon or a test-only product. It is
/// deliberately hidden from normal API documentation.
#[cfg(feature = "token-budget")]
pub(super) fn token_budget_rpc_response(
    id: Value,
    text: &str,
    structured: Value,
    is_error: bool,
) -> Value {
    token_budget_rpc_response_with_observed(id, text, structured, is_error).0
}

/// As [`token_budget_rpc_response`], also returning the candidate response
/// size measured immediately before the production whole-reply admission
/// gate. A fallback fault is still a valid bounded response, but the matrix
/// needs the rejected candidate size to explain why it was reduced.
#[cfg(feature = "token-budget")]
pub(super) fn token_budget_rpc_response_with_observed(
    id: Value,
    text: &str,
    structured: Value,
    is_error: bool,
) -> (Value, usize) {
    let result = tool_result(text, structured, is_error);
    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    });
    let observed = serialized_bytes(&response);
    (bound_rpc_reply(response), observed)
}

fn tool_result(text: &str, structured: Value, is_error: bool) -> Value {
    let candidate = json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": is_error
    });
    let bytes = serialized_bytes(&candidate);
    if bytes <= MCP_RESULT_BUDGET_BYTES {
        return candidate;
    }
    let fault = oversized_fault(BudgetExceeded {
        bytes,
        budget: MCP_RESULT_BUDGET_BYTES,
    });
    let fallback = json!({
        "content": [{ "type": "text", "text": bounded_text(&markdown::fault(&fault)) }],
        "structuredContent": fault_value(&fault),
        "isError": true
    });
    if serialized_bytes(&fallback) <= MCP_RESULT_BUDGET_BYTES {
        fallback
    } else {
        json!({
            "content": [{ "type": "text", "text": "✗ transport response exceeded its context budget" }],
            "structuredContent": { "answer": "fault", "slug": "transport", "cause": "oversized", "detail": "the response exceeded its context budget" },
            "isError": true
        })
    }
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
    structured: Option<Value>,
}

impl RpcError {
    const fn new(code: i64, message: &'static str) -> Self {
        Self {
            code,
            message,
            kind: "json_rpc",
            detail: None,
            structured: None,
        }
    }

    fn invalid(detail: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: "Invalid params",
            kind: "invalid_params",
            detail: Some(detail.into()),
            structured: None,
        }
    }

    fn tool(detail: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: "Backend request failed",
            kind: "transport",
            detail: Some(detail.into()),
            structured: None,
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
            structured: None,
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
            detail: Some(bounded_text(&markdown::fault(fault))),
            structured: Some(fault_value(fault)),
        }
    }

    fn into_reply(self, id: Value) -> Value {
        let mut data = serde_json::Map::new();
        data.insert("kind".to_owned(), Value::String(self.kind.to_owned()));
        if let Some(detail) = self.detail {
            data.insert("detail".to_owned(), Value::String(bounded_text(&detail)));
        }
        if let Some(structured) = self.structured {
            data.insert("structuredContent".to_owned(), structured);
        }
        error_reply(id, self.code, self.message, Some(Value::Object(data)))
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
        .filter(|detail| detail.name() == value)
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

#[derive(Serialize)]
struct SurfaceBody<'a> {
    surface: &'a SurfaceReply,
}

#[cfg(test)]
#[path = "jsonrpc/tests.rs"]
mod tests;

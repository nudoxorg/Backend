//! Standards-compliant MCP JSON-RPC over the shared presentation model.
//!
//! This surface owns three things and no more: the protocol handshake, the
//! projection of the shared command registry into MCP's tool vocabulary
//! ([`tools`]), and the resources an agent attaches to its context
//! ([`resources`]). What a command *is* lives in [`backend_library::COMMANDS`],
//! what it *takes* lives in `backend_present::GRAMMARS`, which round trips it
//! needs lives in `backend_present::answer`, and what the answer *reads like*
//! lives in `backend_present::markdown`. The CLI and MCP use the same renderer;
//! each adds the exact continuation instruction for its own next-call syntax.
//!
//! Two rules decide every `tools/call` result:
//!
//! * the text block is the shared Markdown rendering with its MCP continuation
//!   instruction, and `structuredContent` uses the same typed projection the
//!   CLI's `--format json` emits;
//! * `isError` is true when the shared answer carries a [`Fault`], including
//!   a product refusal that retains its exact typed operation receipt. The fault is
//!   rendered in the shared three-line grammar with its affordance as the exact
//!   next tool call an agent can paste back.

use backend_client::{ClientError, RequestCancellationReason, RequestContext, Session};
use backend_library::protocol::mcp::{MCP_PROTOCOL_VERSION, negotiate_version};
use backend_library::{
    AdmittedGraphQueryInput, CompileExecutionIntent, GraphQueryPage, GraphQueryRow, GraphValue,
    HealthReport, IndexJobTicket, IndexSearchCursor, PackageReference, PageContinuation,
    PageTerminal, ProductText, ReplyDto, SurfaceCommand, SurfaceReply, TreeOpener, ViewStateRoot,
    encode_id,
};
use backend_present::{
    Answer, BudgetExceeded, CL100K_POLICY, CL100K_VOCABULARY_SHA256, ContinuationCursor,
    ContinuationTarget, CursorTarget, DEFAULT_RESPONSE_BUDGET_BYTES, Detail,
    ESTIMATED_BYTES_PER_TOKEN, Engine, Fault, Invocation, Probe, Request, answer_paged,
    bounded_text, cl100k_tokens, encode_answer, encode_serializable, estimate_tokens, fault_value,
    lower_with_opener, markdown, oversized_fault, record_list,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{
    Serialize, Serializer,
    ser::{SerializeMap, SerializeSeq},
};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::{self, BufRead, Write};

mod codec;
mod cursor_authority;
mod index_progress;
mod wire_admission;
pub(super) use codec::{ResponseTransport, encode_response, finalize_response};
pub(super) use cursor_authority::CursorAuthority;
use wire_admission::MeasuredEnvelope;
mod reconnect;
pub(crate) mod requests;
mod resources;
mod tools;
use codec::{
    empty_cursor, error_reply, json_depth_within, limit, no_extra, object, query_variables,
    read_line, string, success, valid_id, write_message,
};
use tools::{
    INDEX_CANCEL_TOOL, INDEX_PROGRESS_TOOL, INDEX_START_TOOL, QUERY_TOOL, SURFACE_TOOL, ToolRoute,
    list_tools, tool_route, validate_registry_arguments,
};

#[cfg(feature = "token-budget")]
pub(crate) use tools::token_budget_tools as token_budget_tools_projection;

const SERVER_NAME: &str = "backend";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whatever this server reads product answers from.
///
/// It is the shared driver's [`Engine`] plus the one capability that is not a
/// registry row: the typed Trustfall lane behind `backend.query`.
pub(super) trait Product: Engine {
    fn set_request_context(&mut self, context: Option<RequestContext>) -> Result<(), ClientError> {
        context.as_ref().map_or(Ok(()), RequestContext::check)
    }

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
pub(super) struct SessionProduct(Session, backend_runtime::WorkspacePaths);

impl SessionProduct {
    pub(super) const fn new(session: Session, paths: backend_runtime::WorkspacePaths) -> Self {
        Self(session, paths)
    }

    fn into_session(self) -> Session {
        self.0
    }
}

impl Engine for SessionProduct {
    fn index(
        &mut self,
        package: PackageReference,
        execution_intent: CompileExecutionIntent,
    ) -> Result<SurfaceReply, ClientError> {
        backend_client::index_with_journal(&self.1, package, execution_intent, |command| {
            self.0.surface(command)
        })
    }
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
            Probe::IndexWithExecutionIntent {
                path,
                execution_intent,
            } => self.0.index_with_execution_intent(path, execution_intent),
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
            Probe::OutlinePage { path, limit } => self.0.outline_page(path, limit, continuation),
            _ if continuation.is_none() => self.probe(probe),
            _ => Err(ClientError::Protocol(
                "this MCP command does not support continuation pages".to_owned(),
            )),
        }
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        backend_client::surface_with_index_journal(&self.1, command, |command| {
            self.0.surface(command)
        })
    }
}

impl Product for SessionProduct {
    fn set_request_context(&mut self, context: Option<RequestContext>) -> Result<(), ClientError> {
        self.0.set_request_context(context)
    }

    fn encode_continuation(
        &mut self,
        continuation: PageContinuation,
    ) -> Result<String, ClientError> {
        if self.0.has_portable_query_continuation(continuation) {
            self.0.encode_query_continuation(continuation)
        } else {
            Ok(self.0.encode_page_continuation(continuation))
        }
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
pub(super) struct SessionEndpoint {
    /// The complete runtime selection, retained for reconnects that need to
    /// compose a daemon after its previous process exited.
    paths: backend_runtime::WorkspacePaths,
    /// Bounded continuation state retained if the daemon is still between
    /// leases and the replacement dial has not succeeded yet.
    pending_continuation_state: Option<backend_client::SessionContinuationState>,
    request_context: Option<RequestContext>,
}

impl reconnect::Endpoint for SessionEndpoint {
    type Product = SessionProduct;

    fn set_request_context(&mut self, context: Option<RequestContext>) {
        self.request_context = context;
    }

    fn connect(&mut self, previous: Option<Self::Product>) -> Result<Self::Product, ClientError> {
        // Park the retired authority before any cancellation, deadline, or
        // startup check can return. Only a fully admitted replacement takes it.
        if let Some(previous) = previous {
            self.retire(previous);
        }
        if let Some(context) = &self.request_context {
            context.check()?;
        }
        let read_budget = self
            .request_context
            .as_ref()
            .map(RequestContext::remaining_read_budget)
            .transpose()?
            .flatten();
        let endpoint =
            match backend_runtime::ensure_locald_cancellable(&self.paths, read_budget, || {
                self.request_context
                    .as_ref()
                    .is_none_or(|context| context.cancellation_reason().is_none())
            }) {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    return Err(map_runtime_startup_error(
                        error,
                        self.request_context.as_ref(),
                    ));
                }
            };
        let connected = match &self.request_context {
            Some(context) => Session::connect_with_context(endpoint, context.clone()),
            None => Session::connect(endpoint),
        };
        let mut session = match connected {
            Ok(session) => session,
            Err(error) => {
                if let Some(context) = &self.request_context {
                    context.check()?;
                }
                return Err(error);
            }
        };
        if let Some(state) = self.pending_continuation_state.take() {
            session.restore_continuation_state(state);
        }
        Ok(SessionProduct::new(session, self.paths.clone()))
    }

    fn retire(&mut self, previous: Self::Product) {
        let mut session = previous.into_session();
        self.pending_continuation_state = Some(session.take_continuation_state());
    }
}

pub(super) type ReconnectingProduct = reconnect::Reconnecting<SessionEndpoint>;

pub(super) fn disconnected_reconnecting_product(
    paths: &backend_runtime::WorkspacePaths,
) -> ReconnectingProduct {
    reconnect::Reconnecting::disconnected(SessionEndpoint {
        paths: paths.clone(),
        pending_continuation_state: None,
        request_context: None,
    })
}

/// Converts a daemon restart window into the same reconnectable class as a
/// reset on an established stream. Startup/configuration failures remain
/// typed startup observations so a broken workspace is reported directly instead
/// of being retried forever by a long-lived MCP process.
fn map_runtime_connect_error(error: backend_runtime::RuntimeError) -> ClientError {
    if let Some(kind) = error.reconnect_kind() {
        ClientError::Disconnected(kind)
    } else {
        ClientError::from(error)
    }
}

/// A dispatched owner may still be starting when this caller's read budget
/// ends. Preserve that actual observation without extending the request or
/// submitting its command. Explicit cancellation always takes precedence.
fn map_runtime_startup_error(
    error: backend_runtime::RuntimeError,
    context: Option<&RequestContext>,
) -> ClientError {
    if let Some(context) = context {
        if let Some(reason) = context.cancellation_reason() {
            return ClientError::RequestCancelled(reason);
        }
        #[cfg(unix)]
        if matches!(&error, backend_runtime::RuntimeError::StartupPending(pending)
            if pending.candidate_pid().is_some())
        {
            return map_runtime_connect_error(error);
        }
        if let Err(error) = context.check() {
            return error;
        }
    }
    map_runtime_connect_error(error)
}

/// Runs newline-delimited MCP stdio until the client closes stdin.
pub(super) fn serve_stdio(
    paths: &backend_runtime::WorkspacePaths,
    project: String,
    cursor_secret: CursorAuthority,
    reader: impl BufRead + Send + 'static,
    writer: &mut impl Write,
) -> io::Result<()> {
    let product = disconnected_reconnecting_product(paths);
    let working_directory = std::env::current_dir()
        .ok()
        .map(backend_runtime::normalize_surface_path)
        .map(|path| path.to_string_lossy().into_owned());
    let server =
        Server::with_invocation_context(product, project, working_directory, cursor_secret);
    serve_stdio_server(server, reader, writer)
}

fn serve_stdio_server<P: Product>(
    mut server: Server<P>,
    mut reader: impl BufRead + Send + 'static,
    writer: &mut impl Write,
) -> io::Result<()> {
    let registry = requests::RequestRegistry::default();
    let _shutdown = StdioShutdown(registry.clone());
    let intake_registry = registry.clone();
    // Full intake terminates the session instead of blocking cancellation
    // behind queued ordinary requests. There is one reader, never one task/line.
    let (sender, receiver) = std::sync::mpsc::sync_channel(requests::MAX_PENDING_REQUESTS);
    let reader_thread = std::thread::Builder::new()
        .name("mcp-stdin".to_owned())
        .spawn(move || {
            loop {
                let line = match read_line(&mut reader) {
                    Ok(Some(line)) => line,
                    // EOF is a read half-close: finite piped requests still
                    // own their responses. Drain them before retiring the actor.
                    Ok(None) => return Ok(()),
                    Err(error) => {
                        intake_registry.close(RequestCancellationReason::Shutdown);
                        return Err(error);
                    }
                };
                let item = match requests::decode(&line) {
                    Ok(value) if intake_registry.cancel_notification(&value) => continue,
                    Ok(value) => {
                        let lease = match intake_registry.admit(&value) {
                            Ok(lease) => lease,
                            Err(reply) => {
                                if sender.try_send(StdioInput::Reply(reply)).is_err() {
                                    intake_registry.close(RequestCancellationReason::Shutdown);
                                    return Err(io::Error::other("MCP intake capacity reached"));
                                }
                                continue;
                            }
                        };
                        StdioInput::Request(value, lease)
                    }
                    Err(reply) => StdioInput::Reply(reply),
                };
                if sender.try_send(item).is_err() {
                    intake_registry.close(RequestCancellationReason::Shutdown);
                    return Err(io::Error::other("MCP intake capacity reached"));
                }
            }
        })?;
    for item in receiver {
        let reply = match item {
            StdioInput::Request(value, lease) => {
                let context = lease.as_ref().map(|lease| lease.context.clone());
                let reply = server.handle_value(value, context);
                drop(lease);
                reply
            }
            StdioInput::Reply(reply) => Some(reply),
        };
        if let Some(reply) = reply
            && let Err(error) = write_message(writer, &reply)
        {
            registry.close(RequestCancellationReason::Shutdown);
            // A reader blocked on a still-open stdin owns no borrowed data.
            // Process exit closes it; do not block shutdown joining that read.
            return Err(error);
        }
    }
    reader_thread
        .join()
        .map_err(|_| io::Error::other("MCP stdin reader failed"))?
}

enum StdioInput {
    Request(Value, Option<requests::RequestLease>),
    Reply(Value),
}

struct StdioShutdown(requests::RequestRegistry);

impl Drop for StdioShutdown {
    fn drop(&mut self) {
        self.0.close(RequestCancellationReason::Shutdown);
    }
}

pub(super) struct Server<P> {
    product: P,
    project: String,
    working_directory: Option<String>,
    handshake: HandshakeState,
    protocol: &'static str,
    opener: Option<TreeOpener>,
    cursor_secret: CursorAuthority,
    request_context: Option<RequestContext>,
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
    pub(super) fn with_authority(
        product: P,
        project: String,
        cursor_secret: impl Into<CursorAuthority>,
    ) -> Self {
        Self::with_invocation_context(product, project, None, cursor_secret)
    }

    pub(super) fn with_invocation_context(
        product: P,
        project: String,
        working_directory: Option<String>,
        cursor_secret: impl Into<CursorAuthority>,
    ) -> Self {
        Self {
            product,
            project,
            working_directory,
            handshake: HandshakeState::AwaitInitialize,
            protocol: MCP_PROTOCOL_VERSION,
            opener: None,
            cursor_secret: cursor_secret.into(),
            request_context: None,
        }
    }

    /// Dispatches the semantic reply; the selected transport owns encoding
    /// and admission of the complete envelope immediately before emission.
    pub(super) fn handle(&mut self, input: &[u8]) -> Option<Value> {
        match requests::decode(input) {
            Ok(value) => self.handle_value(value, None),
            Err(reply) => Some(reply),
        }
    }

    pub(super) fn handle_value(
        &mut self,
        value: Value,
        context: Option<RequestContext>,
    ) -> Option<Value> {
        let id = value.get("id").cloned().unwrap_or(Value::Null);
        if let Some(context) = &context
            && let Some(reply) = requests::cancelled_reply(id.clone(), context)
        {
            return Some(reply);
        }
        if let Err(error) = self.product.set_request_context(context.clone()) {
            let _ = self.product.set_request_context(None);
            if let Some(context) = &context
                && let Some(reply) = requests::cancelled_reply(id.clone(), context)
            {
                return Some(reply);
            }
            return Some(error_reply(
                id,
                -32000,
                "Request unavailable",
                Some(json!({"detail": error.to_string()})),
            ));
        }
        self.request_context = context.clone();
        let reply = self.handle_inner(value);
        self.request_context = None;
        let _ = self.product.set_request_context(None);
        context
            .as_ref()
            .and_then(|context| requests::cancelled_reply(id, context))
            .or(reply)
    }

    fn handle_inner(&mut self, value: Value) -> Option<Value> {
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
            "resources/list" => resources::list_resources(params),
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
        let client = object(params.get("clientInfo").ok_or_else(|| {
            RpcError::invalid_argument("clientInfo", "initialize requires clientInfo")
        })?)?;
        let client_name = ProductText::new(string(client, "name")?)
            .map_err(|error| RpcError::invalid_argument("clientInfo.name", error.to_string()))?;
        self.protocol = negotiate_version(Some(offered));
        self.opener = Some(TreeOpener::Mcp(client_name));
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
        let mut instructions = String::with_capacity(INSTRUCTIONS.len() + project.len() + 400);
        instructions.push_str(INSTRUCTIONS);
        instructions.push_str(
            "\n\nQuick start: `nudox add .`; for accepted work, copy the returned poll command \
             and inspect its terminal receipt before `nudox search \"error handling\"`. \
             Register this stdio server in Claude Code with `claude mcp add --scope user \
             --transport stdio nudox -- \"$(command -v backend-mcp)\" --project \
             '${CLAUDE_PROJECT_DIR:-.}'`, then verify with \
             `claude mcp get nudox`. Use `backend.package` for one pinned registry package and \
             `backend.index_search` for a name-first registry lookup.",
        );
        instructions.push_str("\n\n");
        instructions.push_str(&self.instructions_context());
        instructions.push_str(
            "\n- when this path is not the repository you were asked about, pass that repository's absolute path to backend.index",
        );
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
        context.push_str(&bounded_text(&self.project));
        match self.working_directory.as_deref() {
            Some(directory) if directory != self.project => {
                context.push_str("\n- process working directory: ");
                context.push_str(&bounded_text(directory));
                context.push_str("\n- status: selected project is active; the process working directory may differ\n- relative coordinates use the selected project path");
            }
            Some(directory) => {
                context.push_str("\n- process working directory: ");
                context.push_str(&bounded_text(directory));
                context.push_str("\n- status: selected project is active");
            }
            None => context.push_str("\n- process working directory: unavailable"),
        }
        context
    }

    fn call_tool(&mut self, params: &Value) -> Result<Value, RpcError> {
        let params = object(params)?;
        let name = string(params, "name")?;
        let route = tool_route(name).ok_or_else(|| RpcError::new(-32602, "Unknown tool"))?;
        // Admit the CallToolRequest envelope before interpreting tool inputs.
        if !matches!(params.get("arguments"), None | Some(Value::Object(_))) {
            return Err(RpcError::invalid("arguments must be an object"));
        }
        self.call_routed_tool(params, name, route)
            .or_else(|error| error.into_tool_result_for_call(name, params.get("arguments")))
    }
    /// A recognized route owns input/domain refusals. Envelope and retained
    /// proof failures remain JSON-RPC errors; no display string classifies one.
    fn call_routed_tool(
        &mut self,
        params: &Map<String, Value>,
        name: &str,
        route: ToolRoute,
    ) -> Result<Value, RpcError> {
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => return Err(RpcError::invalid("arguments must be an object")),
        };
        // The MCP schema requires an explicit path even though the shared CLI
        // grammar permits its project default. Admit it before any owner call.
        if name == "backend.index" {
            if !arguments
                .get("path")
                .and_then(Value::as_str)
                .is_some_and(|path| !path.trim().is_empty())
            {
                return Err(RpcError::from_fault(&Fault::usage(
                    "path",
                    "backend.index requires arguments.path: pass the repository's absolute path; use backend.index_start for a resumable operation",
                )));
            }
        }
        let detail = response_detail(name, arguments)?;
        if matches!(route, ToolRoute::Surface) {
            let mut surface_arguments = arguments.clone();
            surface_arguments.remove("detail");
            let context = continuation_context(&self.project, name, &surface_arguments, detail);
            return self.surface_tool(&surface_arguments, detail, &context);
        }
        if matches!(
            route,
            ToolRoute::IndexStart | ToolRoute::IndexProgress | ToolRoute::IndexCancel
        ) {
            let context = continuation_context(&self.project, name, arguments, detail);
            return self.index_job_tool(name, arguments, detail, &context);
        }
        if matches!(route, ToolRoute::RefusedIndexAwait) {
            return Err(RpcError::from_fault(&Fault::usage(
                "ticket",
                "Use backend.index_progress for bounded polling",
            )));
        }
        if name == "backend.outline" {
            return self.outline_tool(arguments, detail);
        }
        let context = continuation_context(&self.project, name, arguments, detail);
        let grammar = match route {
            ToolRoute::Registry(grammar) => Some(grammar),
            ToolRoute::Query | ToolRoute::Surface => None,
            ToolRoute::IndexStart
            | ToolRoute::IndexProgress
            | ToolRoute::IndexCancel
            | ToolRoute::RefusedIndexAwait => None,
        };
        let index_search_tool = grammar.is_some_and(|grammar| grammar.name() == "index-search");
        let index_search_cursor = if index_search_tool {
            self.index_search_cursor(arguments, &context)?
        } else {
            None
        };
        let continuation = if index_search_tool {
            None
        } else {
            self.continuation(arguments, &context)?
        };
        if matches!(route, ToolRoute::Query) {
            return self.query_tool(arguments, detail, continuation, &context);
        }
        if grammar.is_some_and(|grammar| grammar.name() == "graph") {
            return self.graph_tool(name, arguments, detail, continuation, &context);
        }
        let Some(grammar) = grammar else {
            return Err(RpcError::new(-32602, "Unknown tool"));
        };
        let mut command_arguments = arguments.clone();
        command_arguments.remove("detail");
        if index_search_tool {
            if let Some(cursor) = index_search_cursor {
                command_arguments.insert(
                    "cursor".to_owned(),
                    Value::String(cursor.as_str().to_owned()),
                );
            }
        } else {
            command_arguments.remove("cursor");
        }
        let opener = self.opener()?;
        let planned = validate_registry_arguments(grammar, &command_arguments)
            .and_then(|()| Invocation::from_json(grammar, &command_arguments))
            .and_then(|invocation| lower_with_opener(&invocation, &self.project, opener));
        match planned {
            Ok(request) => match answer_paged(&mut self.product, &request, continuation) {
                Ok(answer) => self.rendered(
                    &answer,
                    detail,
                    &context,
                    if index_search_tool {
                        CursorTarget::McpIndexSearchTool
                    } else {
                        CursorTarget::CliOption
                    },
                    name,
                ),
                Err(fault) => Err(RpcError::from_fault(&fault)),
            },
            Err(fault) => Err(RpcError::from_fault(&fault)),
        }
    }
    /// A complete native outline page is selected before output admission.
    /// Only the first page may be re-read with a smaller effective limit. Every
    /// candidate must retain the original producer basis/frontier; continuation
    /// requests retain their issued limit and are never silently resized.
    fn outline_tool(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
    ) -> Result<Value, RpcError> {
        let lifetime = self.request_context.clone().unwrap_or_default();
        lifetime
            .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
            .map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Whole,
                ))
            })?;
        self.product
            .set_request_context(Some(lifetime.clone()))
            .map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Whole,
                ))
            })?;
        no_extra(arguments, &["path", "limit", "cursor", "detail"])?;
        let grammar = backend_present::grammar_for_tool("backend.outline")
            .ok_or_else(|| RpcError::new(-32602, "Unknown tool"))?;
        let mut command = arguments.clone();
        command.remove("detail");
        command.remove("limit");
        command.remove("cursor");
        validate_registry_arguments(grammar, &command)?;
        let invocation = Invocation::from_json(grammar, &command)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let request = lower_with_opener(&invocation, &self.project, self.opener()?)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let Request::Outline(path) = request else {
            return Err(RpcError::invalid("outline request changed shape"));
        };
        let mut effective = match arguments.get("limit") {
            Some(_) => limit(arguments)?,
            None => backend_library::QueryLimit::MAX,
        };
        let mut page_arguments = arguments.clone();
        page_arguments.insert("path".to_owned(), json!(path));
        page_arguments.insert("limit".to_owned(), json!(effective));
        let mut context =
            continuation_context(&self.project, "backend.outline", &page_arguments, detail);
        let continuation = self.continuation(arguments, &context)?;
        let mut pinned = None;
        loop {
            lifetime.check().map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Whole,
                ))
            })?;
            let answer = answer_paged(
                &mut self.product,
                &Request::OutlinePage {
                    path: path.clone(),
                    limit: effective,
                },
                continuation,
            )
            .map_err(|fault| RpcError::from_fault(&fault))?;
            let Answer::Outline(tree) = &answer else {
                return Err(RpcError::invalid("outline page changed shape"));
            };
            let basis = tree
                .page_basis()
                .ok_or_else(|| RpcError::invalid("outline page omits producer basis"))?;
            if pinned.is_some_and(|expected| expected != basis) {
                return Err(RpcError::tool(
                    "outline source changed while selecting a bounded page; restart the outline explicitly",
                ));
            }
            pinned = Some(basis);
            let mut result = self.rendered(
                &answer,
                detail,
                &context,
                CursorTarget::CliOption,
                "backend.outline",
            )?;
            // The opaque token appears once. Copy these exact arguments and
            // take cursor from structuredContent.nextCursor; no second token
            // copy is charged to the agent context.
            if result
                .get("structuredContent")
                .is_some_and(|value| value.get("nextCursor").is_some())
            {
                let next_arguments =
                    json!({"path":path, "limit":effective, "detail":detail.name()});
                if let Some(text) = result
                    .get_mut("content")
                    .and_then(Value::as_array_mut)
                    .and_then(|content| content.first_mut())
                    .and_then(|entry| entry.get_mut("text"))
                {
                    let preview = text.as_str().unwrap_or_default();
                    *text = Value::String(format!(
                        "{preview}\n\nContinue `backend.outline` with arguments `{next_arguments}` and `cursor` copied from `structuredContent.nextCursor`."
                    ));
                }
            }
            // Select against a complete provisional envelope; final encoding
            // independently measures the actual caller ID and transport. A
            // large ID can still receive an honest final admission fault.
            let candidate = MeasuredEnvelope::new(
                attach_wire_budget(
                    codec::success(Value::Null, result),
                    ResponseTransport::StdioLine,
                ),
                ResponseTransport::StdioLine,
            );
            let fitted = if candidate.fits() {
                candidate
            } else {
                fit_measured_preview(candidate, ResponseTransport::StdioLine)
            };
            if fitted.fits()
                && !fitted
                    .value()
                    .get("result")
                    .and_then(|result| result.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                let mut frame = fitted.into_value();
                return Ok(frame
                    .as_object_mut()
                    .and_then(|frame| frame.remove("result"))
                    .unwrap_or(Value::Null));
            }
            if continuation.is_some() || effective == 1 {
                let mut frame = fitted.into_value();
                return Ok(frame
                    .as_object_mut()
                    .and_then(|frame| frame.remove("result"))
                    .unwrap_or(Value::Null));
            }
            effective = effective.div_ceil(2);
            page_arguments.insert("limit".to_owned(), json!(effective));
            context =
                continuation_context(&self.project, "backend.outline", &page_arguments, detail);
        }
    }

    fn opener(&self) -> Result<&TreeOpener, RpcError> {
        self.opener
            .as_ref()
            .ok_or_else(|| RpcError::new(-32002, "Server not initialized"))
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
            Err(error) => Err(RpcError::from_fault(&Fault::from_client_error(
                &error,
                backend_present::Operand::Text(query),
            ))),
        }
    }

    fn graph_tool(
        &mut self,
        tool_name: &str,
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
            .map_err(|error| {
                RpcError::from_fault(&backend_present::probe_fault(
                    &error,
                    Probe::Graph(&coordinate),
                ))
            })?;
        let page = match reply.reply {
            backend_library::CommandReply::ProjectionPage(page) => page,
            backend_library::CommandReply::Failed(failure) => {
                return Err(RpcError::from_fault(&backend_present::probe_fault(
                    &ClientError::CommandFailed(failure),
                    Probe::Graph(&coordinate),
                )));
            }
            _ => return Err(RpcError::tool("graph page reply changed shape")),
        };
        let answer = Answer::Records(Box::new(record_list(&coordinate, &page.snapshot)));
        self.rendered(&answer, detail, context, CursorTarget::CliOption, tool_name)
    }
    fn surface_tool(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
        context: &[u8],
    ) -> Result<Value, RpcError> {
        no_extra(arguments, &["command"])?;
        let encoded = arguments
            .get("command")
            .cloned()
            .ok_or_else(|| RpcError::invalid("command must be a tagged surface object"))?;
        let mut command = serde_json::from_value::<SurfaceCommand>(encoded)
            .map_err(|error| RpcError::invalid(format!("command: {error}")))?;
        if let SurfaceCommand::TreeOpen { opener, .. } = &mut command {
            *opener = self.opener()?.clone();
        }
        if matches!(&command, SurfaceCommand::IndexAwait { .. }) {
            return Err(RpcError::invalid(
                "IndexAwait can block on owner work; use IndexProgress for immediate bounded polling",
            ));
        }
        if let SurfaceCommand::IndexSearch {
            cursor: Some(cursor),
            ..
        } = &mut command
        {
            *cursor = self.verified_index_search_cursor(cursor.as_str(), context)?;
        }
        command
            .admit()
            .map_err(|error| RpcError::invalid(format!("command: {error}")))?;
        let operand = surface_error_operand(&command);
        let reply = self
            .product
            .surface(command.clone())
            .map_err(|error| match error {
                ClientError::StaleCursor => RpcError::stale_cursor(),
                other => RpcError::from_fault(&Fault::from_client_error(&other, operand)),
            })?;
        self.surface_reply_result(&command, &reply, detail, context)
    }
    fn index_job_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
        detail: Detail,
        context: &[u8],
    ) -> Result<Value, RpcError> {
        let command = match name {
            INDEX_START_TOOL => {
                no_extra(
                    arguments,
                    &["package", "execution_intent", "operation_key", "detail"],
                )?;
                let spelling = string(arguments, "package")?.to_owned();
                let package = PackageReference::parse(spelling)
                    .map_err(|error| RpcError::invalid(error.to_string()))?;
                let execution_intent = match arguments.get("execution_intent") {
                    None => CompileExecutionIntent::Interactive,
                    Some(value) => serde_json::from_value::<CompileExecutionIntent>(value.clone())
                        .map_err(|_| {
                            RpcError::invalid("execution_intent must be interactive or background")
                        })?,
                };
                match arguments.get("operation_key") {
                    Some(key) => SurfaceCommand::IndexOperationStart {
                        operation_key:
                            serde_json::from_value::<backend_library::IndexOperationKey>(
                                key.clone(),
                            )
                            .map_err(|error| RpcError::invalid(format!("operation_key: {error}")))?,
                        package,
                        execution_intent,
                    },
                    None => SurfaceCommand::IndexStart {
                        package,
                        execution_intent,
                    },
                }
            }
            INDEX_PROGRESS_TOOL => {
                no_extra(
                    arguments,
                    &[
                        "ticket",
                        "after_sequence",
                        "operation_key",
                        "after_observation",
                        "wait_millis",
                        "detail",
                    ],
                )?;
                if let Some(key) = arguments.get("operation_key") {
                    if arguments.contains_key("ticket") || arguments.contains_key("after_sequence")
                    {
                        return Err(RpcError::invalid(
                            "use either operation_key or ticket with after_sequence",
                        ));
                    }
                    let operation_key =
                        serde_json::from_value::<backend_library::IndexOperationKey>(key.clone())
                            .map_err(|error| RpcError::invalid(format!("operation_key: {error}")))?;
                    let command = SurfaceCommand::IndexOperationStatus { operation_key };
                    if let Some(result) =
                        self.conditional_index_progress(arguments, &command, detail, context)?
                    {
                        return Ok(result);
                    }
                    let reply = self.product.surface(command.clone()).map_err(|error| {
                        RpcError::from_fault(&Fault::from_client_error(
                            &error,
                            surface_error_operand(&command),
                        ))
                    })?;
                    return self.surface_reply_result(&command, &reply, detail, context);
                }
                let ticket = index_job_ticket(arguments)?;
                let after_sequence = match arguments.get("after_sequence") {
                    None => 0,
                    Some(value) => value.as_u64().ok_or_else(|| {
                        RpcError::invalid("after_sequence must be a non-negative integer")
                    })?,
                };
                let command = SurfaceCommand::IndexProgress {
                    ticket,
                    after_sequence,
                };
                if let Some(result) =
                    self.conditional_index_progress(arguments, &command, detail, context)?
                {
                    return Ok(result);
                }
                command
            }
            INDEX_CANCEL_TOOL => {
                no_extra(arguments, &["ticket", "detail"])?;
                SurfaceCommand::IndexCancel {
                    ticket: index_job_ticket(arguments)?,
                }
            }
            _ => return Err(RpcError::new(-32602, "Unknown tool")),
        };
        command
            .admit()
            .map_err(|error| RpcError::invalid(format!("command: {error}")))?;
        let operand = surface_error_operand(&command);
        let reply = self
            .product
            .surface(command.clone())
            .map_err(|error| RpcError::from_fault(&Fault::from_client_error(&error, operand)))?;
        self.surface_reply_result(&command, &reply, detail, context)
    }
    fn surface_reply_result(
        &mut self,
        command: &SurfaceCommand,
        reply: &SurfaceReply,
        detail: Detail,
        context: &[u8],
    ) -> Result<Value, RpcError> {
        let view = backend_present::product_view_for_command(command, reply);
        let view = view.with_bounded_index_poll().map_err(|error| {
            RpcError::from_fault(&Fault::from_client_error(
                &error,
                surface_error_operand(command),
            ))
        })?;
        let view = match view.cursor_family().cloned() {
            Some(cursor) => {
                let token = self.issue_cursor(cursor, context)?;
                view.project_cursor(token, CursorTarget::SurfaceCommand)
            }
            None => view,
        };
        let next_cursor = view.index_search_page().and_then(|page| page.next_cursor());
        let payload = match encode_serializable(
            "surface",
            detail,
            None,
            SurfaceBody {
                fault: view.fault().map(backend_present::FaultDto::new),
                index_poll: backend_present::IndexPollDto::for_detail(&view, detail),
                surface: SurfaceProjection {
                    reply,
                    next_cursor,
                    index_job: view.index_job(),
                },
            },
            DEFAULT_RESPONSE_BUDGET_BYTES,
        ) {
            Ok(payload) => payload,
            Err(error) => return Ok(refused(&oversized_fault(error))),
        };
        let structured: Value = serde_json::from_slice(&payload.bytes).map_err(|error| {
            RpcError::tool(format!("typed surface projection decode failed: {error}"))
        })?;
        Ok(tool_result(
            &bounded_text(&markdown::product_with_detail(&view, detail)),
            structured,
            view.fault().is_some(),
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
        let view = Engine::revision(&mut self.product).map_err(|error| {
            RpcError::from_fault(&Fault::from_client_error(
                &error,
                backend_present::Operand::Text(self.project.clone()),
            ))
        })?;
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
        let owner_token = self.verify_cursor_token(token, context)?.ok_or_else(|| {
            RpcError::invalid(
                "cursor is unknown, expired, or belongs to another workspace authority",
            )
        })?;
        self.product
            .decode_continuation(&owner_token)
            .map(Some)
            .map_err(|error| match error {
                ClientError::StaleCursor => RpcError::stale_cursor(),
                other => RpcError::from_fault(&Fault::from_client_error(
                    &other,
                    backend_present::Operand::Argument("cursor".to_owned()),
                )),
            })
    }
    fn index_search_cursor(
        &self,
        arguments: &Map<String, Value>,
        context: &[u8],
    ) -> Result<Option<IndexSearchCursor>, RpcError> {
        let Some(value) = arguments.get("cursor") else {
            return Ok(None);
        };
        let token = value
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| RpcError::invalid("cursor must be a non-empty opaque string"))?;
        self.verified_index_search_cursor(token, context).map(Some)
    }

    fn verified_index_search_cursor(
        &self,
        token: &str,
        context: &[u8],
    ) -> Result<IndexSearchCursor, RpcError> {
        let owner_token = self.verify_cursor_token(token, context)?.ok_or_else(|| {
            RpcError::invalid(
                "cursor is unknown, expired, or belongs to another workspace authority",
            )
        })?;
        IndexSearchCursor::new(owner_token).map_err(|_| {
            RpcError::invalid(
                "cursor is unknown, expired, or belongs to another workspace authority",
            )
        })
    }

    fn issue_continuation(
        &mut self,
        continuation: PageContinuation,
        context: &[u8],
    ) -> Result<String, RpcError> {
        self.issue_cursor(ContinuationCursor::Page(continuation), context)
    }

    fn issue_cursor(
        &mut self,
        cursor: ContinuationCursor,
        context: &[u8],
    ) -> Result<String, RpcError> {
        let owner_token = match cursor {
            ContinuationCursor::Page(continuation) => self
                .product
                .encode_continuation(continuation)
                .map_err(|error| {
                    RpcError::from_fault(&Fault::from_client_error(
                        &error,
                        backend_present::Operand::Text(self.project.clone()),
                    ))
                })?,
            ContinuationCursor::IndexSearch(cursor) => cursor.as_str().to_owned(),
        };
        self.sign_cursor_token(&owner_token, context)
    }

    fn sign_cursor_token(&self, owner_token: &str, context: &[u8]) -> Result<String, RpcError> {
        self.sign_cursor_token_at(unix_seconds().saturating_add(900), owner_token, context)
    }

    fn sign_cursor_token_at(
        &self,
        expiry: u64,
        owner_token: &str,
        context: &[u8],
    ) -> Result<String, RpcError> {
        if !valid_direct_cursor_owner(owner_token) {
            return Err(RpcError::invalid(
                "owner cursor has an unsupported encoding or exceeds its bound",
            ));
        }
        // Owner tokens already use URL-safe encodings. Sign their exact bytes
        // rather than expanding them through a redundant second base64 layer.
        // Include the version in the authenticated body to prevent rebinding.
        let body = format!("mcp2-{expiry}-{owner_token}");
        let key = self.cursor_secret.key()?;
        let mac = Self::cursor_mac(&key, &body, context);
        Ok(format!("{body}-{}", hex_bytes(&mac.as_bytes()[..16])))
    }

    fn verify_cursor_token(&self, token: &str, context: &[u8]) -> Result<Option<String>, RpcError> {
        if token.starts_with("mcp2-") {
            return self.verify_direct_cursor_token(token, context);
        }
        // Keep the historical mcp1 parser, canonical outer base64 check and
        // MAC domain unchanged so previously issued tokens remain verifiable.
        let parsed = (|| {
            let body = token.strip_prefix("mcp1-")?;
            let (body, encoded_mac) = body.rsplit_once('-')?;
            let (expiry, encoded_owner) = body.split_once('-')?;
            let expiry = expiry.parse::<u64>().ok()?;
            // The expiry second is closed. Malformed and expired input needs
            // no durable authority and must not initialize a fresh workspace.
            (expiry > unix_seconds()).then_some((body, encoded_mac, encoded_owner))
        })();
        let Some((body, encoded_mac, encoded_owner)) = parsed else {
            return Ok(None);
        };
        if encoded_mac.len() != 32
            || !encoded_mac
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Ok(None);
        }
        let Some(owner) = decode_cursor_owner(encoded_owner) else {
            return Ok(None);
        };
        let Some(key) = self.cursor_secret.verification_key()? else {
            return Ok(None);
        };
        let expected = Self::cursor_mac(&key, body, context);
        if encoded_mac != hex_bytes(&expected.as_bytes()[..16]) {
            return Ok(None);
        }
        Ok(Some(owner))
    }

    fn verify_direct_cursor_token(
        &self,
        token: &str,
        context: &[u8],
    ) -> Result<Option<String>, RpcError> {
        // Bound the raw string before parsing, allocating or hashing it.
        if token.len() > backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES.saturating_add(59) {
            return Ok(None);
        }
        let Some((body, encoded_mac)) = token.rsplit_once('-') else {
            return Ok(None);
        };
        let Some((expiry_text, owner)) = body
            .strip_prefix("mcp2-")
            .and_then(|body| body.split_once('-'))
        else {
            return Ok(None);
        };
        let Some(expiry) = expiry_text.parse::<u64>().ok() else {
            return Ok(None);
        };
        if expiry <= unix_seconds()
            || expiry.to_string() != expiry_text
            || !valid_direct_cursor_owner(owner)
            || encoded_mac.len() != 32
            || !encoded_mac
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Ok(None);
        }
        let Some(key) = self.cursor_secret.verification_key()? else {
            return Ok(None);
        };
        let expected = Self::cursor_mac(&key, body, context);
        if encoded_mac != hex_bytes(&expected.as_bytes()[..16]) {
            return Ok(None);
        }
        Ok(Some(owner.to_owned()))
    }

    fn cursor_mac(key: &[u8; 32], body: &str, context: &[u8]) -> blake3::Hash {
        let mut payload = Vec::with_capacity(body.len() + context.len() + 1);
        payload.extend_from_slice(body.as_bytes());
        payload.push(0);
        payload.extend_from_slice(context);
        blake3::keyed_hash(key, &payload)
    }

    fn rendered(
        &mut self,
        answer: &Answer,
        detail: Detail,
        context: &[u8],
        cursor_target: CursorTarget,
        tool_name: &str,
    ) -> Result<Value, RpcError> {
        let next = answer
            .cursor()
            .map(|cursor| self.issue_cursor(cursor, context))
            .transpose()?;
        let projected_product = match (answer, next.as_deref()) {
            (Answer::Product(product), None)
                if product.index_operation().is_none() && product.index_job().is_none() =>
            {
                None
            }
            (Answer::Product(product), token) => {
                let product = match token {
                    Some(token) => (**product).clone().project_cursor(token, cursor_target),
                    None => (**product).clone(),
                };
                let product = product.with_bounded_index_poll().map_err(|error| {
                    RpcError::from_fault(&Fault::from_client_error(
                        &error,
                        backend_present::Operand::Whole,
                    ))
                })?;
                Some(Answer::Product(Box::new(product)))
            }
            _ => None,
        };
        let answer = projected_product.as_ref().unwrap_or(answer);
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
        let preview = match detail {
            Detail::Summary => markdown::summary_with_cursor(
                answer,
                next.as_deref(),
                ContinuationTarget::McpTool(tool_name),
            ),
            Detail::Standard | Detail::Full => markdown::answer_with_detail(
                answer,
                detail,
                next.as_deref(),
                ContinuationTarget::McpTool(tool_name),
            ),
        };
        let text = bounded_text(&preview);
        Ok(tool_result(&text, structured, answer.fault().is_some()))
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
            &bounded_text(&graph_page_text(page, next.as_deref())),
            value,
            false,
        ))
    }
}

const INSTRUCTIONS: &str = "This server attaches to the current index without changing it. Call \
backend.packages to inspect the shelf. Index or reindex a repository explicitly with backend.index \
and its absolute path. For accepted or active indexing work, copy the returned index_poll.call \
verbatim as your next backend.index_progress tool call. Repeat only while index_poll is present \
and inspect the terminal receipt's outcome. Unknown, unresolved \
and expired observations do not establish publication. A published receipt does not make every \
optional lane ready. An empty shelf means no project has been indexed yet, and a lane that is not \
configured is not an empty codebase. Find \
a coordinate with backend.search, or with backend.outline when you do not know the names. Copy \
that coordinate verbatim into backend.document for the signature and docs, backend.source for \
the body, backend.references for uses, and backend.graph for calls. Do not invent coordinates \
and do not guess from filenames when a tool can answer. A refusal names the operand and the next \
call; follow it. Read backend://workspace/current when a result looks thin. Read \
backend://tools/catalog for callable capability-dependent routes that are not in tools/list. \
Status reports owner availability, published rows, lane coverage, and native oracle readiness separately; \
structural rows do not prove native semantic coverage.";

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
    // This request-context schema is shared with already issued mcp1 tokens.
    // mcp2 binds its wrapper version in the MAC body, not by changing the
    // query/project/role context and invalidating legacy continuations.
    canonical.insert("schema", Value::String("mcp1".to_owned()));
    canonical.insert("tool", Value::String(name.to_owned()));
    canonical.insert("workspace", Value::String(workspace.to_owned()));
    for (key, value) in arguments {
        if key != "cursor" {
            let value = if name == SURFACE_TOOL && key == "command" {
                surface_command_without_cursor(value)
            } else {
                value.clone()
            };
            canonical.insert(key.as_str(), value);
        }
    }
    serde_json::to_vec(&canonical).unwrap_or_default()
}

fn surface_command_without_cursor(value: &Value) -> Value {
    let mut command = value.clone();
    if let Some(command) = command.as_object_mut()
        && command.get("operation").and_then(Value::as_str) == Some("index-search")
    {
        command.remove("cursor");
    }
    command
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

const MAX_CURSOR_OWNER_ENCODED_BYTES: usize = backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES
    .saturating_mul(4)
    .saturating_add(2)
    / 3;

fn decode_cursor_owner(encoded: &str) -> Option<String> {
    if encoded.len() > MAX_CURSOR_OWNER_ENCODED_BYTES {
        return None;
    }
    let decoded = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    if decoded.len() > backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES
        || URL_SAFE_NO_PAD.encode(&decoded) != encoded
    {
        return None;
    }
    String::from_utf8(decoded)
        .ok()
        .filter(|owner_token| !owner_token.is_empty())
}

/// Encoding admission only: the owner still checks the retained proof,
/// selected roots, query roles and index state after wrapper authentication.
fn valid_direct_cursor_owner(owner: &str) -> bool {
    if owner.is_empty() || owner.len() > backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES {
        return false;
    }
    if let Some(encoded) = owner.strip_prefix("pc1-") {
        return encoded.len() == backend_library::CURSOR_QUERY_BYTES.saturating_mul(2)
            && encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    }
    if let Some(encoded) = owner.strip_prefix("pc2-") {
        return !encoded.is_empty()
            && encoded.len() % 2 == 0
            && encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    }
    let (encoded, portable) = owner
        .strip_prefix("pc3-")
        .or_else(|| owner.strip_prefix("pc4-"))
        .map_or((owner, false), |encoded| (encoded, true));
    if encoded.is_empty()
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return false;
    }
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    if decoded.is_empty() || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return false;
    }
    // The unprefixed index-search issuer serializes a JSON object. This is a
    // framing check, not authority to admit its fields or its selected state.
    portable || (decoded.first() == Some(&b'{') && decoded.last() == Some(&b'}'))
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

/// Context-sized ceiling for one complete encoded JSON-RPC response, including
/// its ID, text block, typed projection, metadata, and transport framing.
const MCP_RESULT_BUDGET_BYTES: usize = DEFAULT_RESPONSE_BUDGET_BYTES;
const PREVIEW_MARKER: &str = "\n\n… readable preview shortened to fit this response; structuredContent contains the complete page and any nextCursor.";

fn serialized_bytes(value: &Value) -> usize {
    let mut count = codec::ResponseByteCounter::default();
    serde_json::to_writer(&mut count, value).map_or(usize::MAX, |()| count.bytes())
}

fn transport_serialized_bytes(value: &Value, transport: ResponseTransport) -> usize {
    transport
        .encoded_len(serialized_bytes(value))
        .unwrap_or(usize::MAX)
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

/// Named ordinary-token contract; other encodings may have different counts.
const MCP_CL100K_TOKEN_LIMIT: usize = 12_000;

#[cfg(test)]
fn wire_tokens(value: &Value) -> Result<usize, backend_present::Cl100kError> {
    MeasuredEnvelope::new(value.clone(), ResponseTransport::HttpBody).tokens()
}
#[cfg(test)]
fn wire_fits(value: &Value, transport: ResponseTransport) -> bool {
    MeasuredEnvelope::new(value.clone(), transport).fits()
}

#[cfg(any(test, feature = "token-budget"))]
fn bound_rpc_reply(value: Value, transport: ResponseTransport) -> Value {
    bound_rpc_envelope(value, transport).into_value()
}

fn bound_rpc_envelope(value: Value, transport: ResponseTransport) -> MeasuredEnvelope {
    let budget = transport.limit_bytes();
    let measured = MeasuredEnvelope::new(attach_wire_budget(value, transport), transport);
    let measured = if measured.fits() {
        measured
    } else {
        fit_measured_preview(measured, transport)
    };
    if measured.fits() {
        return measured;
    }
    let bytes = measured.bytes();
    let value = measured.value();
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    if value.get("error").is_some() {
        let mut uncorrelated = value.clone();
        uncorrelated["id"] = Value::Null;
        let uncorrelated =
            MeasuredEnvelope::new(attach_wire_budget(uncorrelated, transport), transport);
        if uncorrelated.fits() {
            return uncorrelated;
        }
    }
    let fault = if bytes > budget {
        oversized_fault(BudgetExceeded { bytes, budget })
    } else {
        let detail = match measured.tokens() {
            Ok(tokens) => format!(
                "complete MCP JSON envelope has {tokens} {CL100K_POLICY} tokens, above the {MCP_CL100K_TOKEN_LIMIT} token budget; use the returned paging limit or request a narrower page"
            ),
            Err(error) => error.to_string(),
        };
        Fault::new(
            backend_present::FaultSlug::Transport,
            backend_present::Operand::Argument("response".to_owned()),
            backend_present::Cause::new(backend_present::CauseSlug::Oversized, detail),
            backend_present::Affordance::None,
        )
    };
    let fallback = MeasuredEnvelope::new(
        attach_wire_budget(
            error_reply(
                id,
                -32000,
                "MCP response exceeds the bounded context budget",
                Some(
                    json!({"kind":fault.slug().as_str(),"detail":bounded_text(&markdown::fault(&fault)),"structuredContent":fault_value(&fault)}),
                ),
            ),
            transport,
        ),
        transport,
    );
    if fallback.fits() {
        fallback
    } else {
        MeasuredEnvelope::new(
            attach_wire_budget(
                error_reply(
                    Value::Null,
                    -32000,
                    "MCP response exceeds the bounded context budget",
                    None,
                ),
                transport,
            ),
            transport,
        )
    }
}

/// Reclaims only duplicate human-readable text when a large request ID makes
/// the caller-specific JSON-RPC envelope exceed the budget. Structured rows,
/// terminal state, and owner cursors stay byte-for-byte intact.
#[cfg(test)]
fn fit_tool_preview(value: Value, transport: ResponseTransport) -> Value {
    fit_measured_preview(MeasuredEnvelope::new(value, transport), transport).into_value()
}
fn fit_measured_preview(
    measured: MeasuredEnvelope,
    transport: ResponseTransport,
) -> MeasuredEnvelope {
    let Some(current_text) = measured
        .value()
        .get("result")
        .and_then(|result| result.get("content"))
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|entry| entry.get("text"))
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return measured;
    };
    let prefix = current_text
        .strip_suffix(PREVIEW_MARKER)
        .unwrap_or(&current_text);
    let mut value = measured.into_value();
    value["result"]["content"][0]["text"] = Value::String(PREVIEW_MARKER.to_owned());
    let mut best = MeasuredEnvelope::new(attach_wire_budget(value.clone(), transport), transport);
    if !best.fits() {
        return best;
    }
    let mut lower = 0;
    let mut upper = prefix.len();
    let mut best_end = 0;
    while lower < upper {
        let midpoint = lower + (upper - lower).div_ceil(2);
        let end = readable_prefix_end(prefix, midpoint);
        let mut candidate = value.clone();
        candidate["result"]["content"][0]["text"] =
            Value::String(format!("{}{PREVIEW_MARKER}", &prefix[..end]));
        let candidate = MeasuredEnvelope::new(attach_wire_budget(candidate, transport), transport);
        if candidate.fits() {
            lower = midpoint;
            if end >= best_end {
                best_end = end;
                best = candidate;
            }
        } else {
            upper = midpoint - 1;
        }
    }
    best
}

/// Adds exact transport accounting to result objects and JSON-RPC error data
/// while preserving the shared structured projection's own payload budget.
/// The count is fixed-point measured because its digits are part of the frame.
fn attach_wire_budget(mut value: Value, transport: ResponseTransport) -> Value {
    let metadata_path: &[&str] = if value.get("result").is_some_and(Value::is_object) {
        &["result", "_meta", "backend/wireBudget"]
    } else if value.get("error").is_some_and(Value::is_object) {
        // JSON-RPC error data is application-owned. Preserve a scalar data
        // payload, which cannot carry MCP metadata without changing its shape.
        let error = value["error"].as_object_mut().expect("error object");
        if !error.contains_key("data") {
            error.insert("data".to_owned(), json!({}));
        }
        if !error["data"].is_object()
            || error["data"]
                .get("_meta")
                .is_some_and(|meta| !meta.is_object())
        {
            // Unlike MCP result metadata, arbitrary JSON-RPC error data is
            // not ours to normalize. Admission still measures its exact frame.
            return value;
        }
        &["error", "data", "_meta", "backend/wireBudget"]
    } else {
        return value;
    };
    {
        let mut parent = &mut value;
        for key in &metadata_path[..metadata_path.len() - 1] {
            if !parent[*key].is_object() {
                parent[*key] = json!({});
            }
            parent = &mut parent[*key];
        }
        // This reserved field belongs to the final transport encoder. Other
        // object metadata survives; a stale budget cannot describe this frame.
        parent["backend/wireBudget"] = json!({
            "bytes": 0,
            "estimatedTokens": 0,
            "bytesPerToken": ESTIMATED_BYTES_PER_TOKEN,
            "limitBytes": transport.limit_bytes(),
            "tokenPolicy": CL100K_POLICY,
            "tokenScope": "complete_jsonrpc_json_excluding_transport_framing",
            "limitTokens": MCP_CL100K_TOKEN_LIMIT,
            "vocabularySha256": CL100K_VOCABULARY_SHA256,
            "scope": transport.scope()
        });
    }
    // Starting at zero makes digit widths monotone. Each pass either fixes
    // the count or increases a bounded decimal width; include both reported
    // numbers, existing metadata, escaped JSON and actual transport framing.
    loop {
        let bytes = transport_serialized_bytes(&value, transport);
        let mut budget = &mut value;
        for key in metadata_path {
            budget = &mut budget[*key];
        }
        budget["bytes"] = json!(bytes);
        budget["estimatedTokens"] = json!(estimate_tokens(bytes));
        if transport_serialized_bytes(&value, transport) == bytes {
            return value;
        }
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
    let response = attach_wire_budget(response, ResponseTransport::StdioLine);
    let observed = transport_serialized_bytes(&response, ResponseTransport::StdioLine);
    (
        bound_rpc_reply(response, ResponseTransport::StdioLine),
        observed,
    )
}

fn tool_result(text: &str, structured: Value, is_error: bool) -> Value {
    let mut candidate = json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": is_error
    });
    let bytes = serialized_bytes(&candidate);
    // Leave room for the ordinary JSON-RPC envelope. The final reply gate
    // still checks the actual caller ID, including deliberately huge IDs.
    const ENVELOPE_RESERVE: usize = 256;
    let budget = MCP_RESULT_BUDGET_BYTES.saturating_sub(ENVELOPE_RESERVE);
    if bytes <= budget {
        return candidate;
    }
    // Text duplicates the typed answer. A shorter, explicitly labelled
    // preview keeps every structured row, coverage fact, and owner cursor
    // intact; it never turns an oversized typed page into a partial result.
    candidate["content"][0]["text"] = Value::String(PREVIEW_MARKER.to_owned());
    if serialized_bytes(&candidate) <= budget {
        let mut lower = 0;
        let mut upper = text.len();
        let mut best_end = 0;
        while lower < upper {
            let midpoint = lower + (upper - lower).div_ceil(2);
            let end = readable_prefix_end(text, midpoint);
            let mut trial = candidate.clone();
            trial["content"][0]["text"] =
                Value::String(format!("{}{PREVIEW_MARKER}", &text[..end]));
            if serialized_bytes(&trial) <= budget {
                lower = midpoint;
                if end >= best_end {
                    best_end = end;
                    candidate = trial;
                }
            } else {
                upper = midpoint - 1;
            }
        }
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

/// Returns a UTF-8 boundary no later than `limit`, backing up to the start of
/// a rendered summary row if the candidate would leave only its label or a
/// partial coordinate. Both result-level and final-envelope budgeting use
/// this so an exact native operand is never cut after the page was rendered.
fn readable_prefix_end(text: &str, limit: usize) -> usize {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if !text.starts_with("• ") && !text.contains("\n• ") {
        return end;
    }

    let mut line_start = 0;
    while line_start < text.len() {
        let line_end = text[line_start..]
            .find('\n')
            .map_or(text.len(), |offset| line_start + offset);
        let line = &text[line_start..line_end];
        if line.starts_with("• ") {
            let label_code_end = code_span_end(line, "• ".len());
            let coordinate_start = line_end.saturating_add(1);
            if let (Some(label_code_end), Some(coordinate_line_end)) = (
                label_code_end,
                (coordinate_start < text.len()).then(|| {
                    text[coordinate_start..]
                        .find('\n')
                        .map_or(text.len(), |offset| coordinate_start + offset)
                }),
            ) && line[label_code_end..].starts_with(" · ")
            {
                let coordinate_line = &text[coordinate_start..coordinate_line_end];
                let coordinate_code_end = code_span_end(coordinate_line, 2);
                if coordinate_line.starts_with("  ")
                    && coordinate_code_end == Some(coordinate_line.len())
                {
                    let row_end =
                        coordinate_line_end.saturating_add(if coordinate_line_end < text.len() {
                            1
                        } else {
                            0
                        });
                    if line_start < end && end < row_end {
                        return line_start;
                    }
                }
            }
        }
        if line_end == text.len() {
            break;
        }
        line_start = line_end + 1;
    }
    end
}

/// Finds the close of an inline code span beginning at `start` in one line.
/// The returned offset is after the closing delimiter.
fn code_span_end(line: &str, start: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut delimiter = 0;
    while bytes.get(start.saturating_add(delimiter)) == Some(&b'`') {
        delimiter += 1;
    }
    if delimiter == 0 {
        return None;
    }

    let mut position = start + delimiter;
    while position < bytes.len() {
        if bytes[position] != b'`' {
            position += 1;
            continue;
        }
        let run_start = position;
        while bytes.get(position) == Some(&b'`') {
            position += 1;
        }
        if position - run_start == delimiter {
            return Some(position);
        }
    }
    None
}

fn graph_page_text(page: &GraphQueryPage, cursor: Option<&str>) -> String {
    let mut out = format!(
        "~query {} row(s) · {} · revision {}\n",
        page.rows.len(),
        terminal_name(page.terminal),
        abbreviate(page.revision.as_bytes())
    );
    match page.terminal {
        PageTerminal::More(_) => {
            if let Some(cursor) = cursor {
                out.push_str(&backend_present::continuation_note(
                    ContinuationTarget::McpTool(QUERY_TOOL),
                    cursor,
                ));
                out.push('\n');
            } else {
                out.push_str("… more rows are available, but no continuation cursor was issued\n");
            }
        }
        PageTerminal::Cancelled => {
            out.push_str(
                "… query cancelled before completion; no continuation cursor is available\n",
            );
        }
        PageTerminal::Complete => {}
    }
    for row in &page.rows {
        let fields = row
            .fields()
            .iter()
            .map(|(name, value)| format!("{name}={}", scalar_text(value)))
            .collect::<Vec<_>>();
        out.push_str(&fields.join("  "));
        out.push('\n');
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
    // Retain the typed explanation until a recognized tool route decides its
    // MCP result shape. Serialized text is never parsed back into authority.
    fault: Option<Fault>,
}

impl RpcError {
    /// Only a typed startup refusal proves that the original command was not
    /// submitted. Attach the caller's exact call as advice, never execute it.
    fn into_tool_result_for_call(
        self,
        name: &str,
        arguments: Option<&Value>,
    ) -> Result<Value, Self> {
        if let Some(fault) = &self.fault
            && fault.startup_recovery()
                == Some(backend_client::StartupRecovery::RetryOriginalCommand)
        {
            let call = json!({
                "name": name,
                "arguments": arguments.cloned().unwrap_or_else(|| json!({})),
            });
            let mut structured = fault_value(fault);
            structured["call"] = call.clone();
            structured
                .as_object_mut()
                .expect("fault object")
                .remove("shell");
            let text = format!(
                "Retry the exact tool call in structuredContent.call; this command was not submitted.\n{}",
                markdown::fault(fault),
            );
            let result = tool_result(&bounded_text(&text), structured, true);
            if result
                .get("structuredContent")
                .and_then(|content| content.get("call"))
                == Some(&call)
            {
                return Ok(result);
            }

            // The request frame can be larger than the result budget. Keep
            // the typed startup refusal and its not-submitted meaning, but do
            // not emit a truncated or fabricated call when the exact one
            // cannot fit in structuredContent.
            let structured = bounded_startup_fault(fault);
            let explanation = format!(
                "The command was not submitted. The exact tool call could not be included in the {}-byte response budget, so no retry was performed.",
                MCP_RESULT_BUDGET_BYTES,
            );
            let text = format!(
                "{explanation}\nStartup failure: {}",
                bounded_fault_field(fault.cause().sentence()),
            );
            return Ok(tool_result(&text, structured, true));
        }
        self.into_tool_result()
    }

    const fn new(code: i64, message: &'static str) -> Self {
        Self {
            code,
            message,
            kind: "json_rpc",
            detail: None,
            structured: None,
            fault: None,
        }
    }

    fn invalid(detail: impl Into<String>) -> Self {
        Self::invalid_argument("arguments", detail)
    }
    fn invalid_argument(argument: &str, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            code: -32602,
            message: "Invalid params",
            kind: "invalid_params",
            fault: Some(Fault::usage(argument, detail.clone())),
            detail: Some(detail),
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
            fault: None,
        }
    }

    fn stale_cursor() -> Self {
        Self {
            code: -32010,
            message: "Stale cursor",
            kind: "stale_cursor",
            fault: Some(Fault::from_client_error(
                &ClientError::StaleCursor,
                backend_present::Operand::Argument("cursor".to_owned()),
            )),
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
            code: if fault.slug() == backend_present::FaultSlug::NotFound {
                -32002
            } else if fault.slug().is_usage() {
                -32602
            } else {
                -32603
            },
            message: if fault.slug() == backend_present::FaultSlug::NotFound {
                "Resource not found"
            } else {
                "Backend request failed"
            },
            kind: fault.slug().as_str(),
            detail: Some(bounded_text(&markdown::fault(fault))),
            structured: Some(fault_value(fault)),
            fault: Some(fault.clone()),
        }
    }

    /// This classification is exhaustive so a new shared failure class must
    /// deliberately choose its MCP contract. Proof failures cannot become a
    /// domain refusal merely because another route used a different adapter.
    fn into_tool_result(self) -> Result<Value, Self> {
        use backend_present::FaultSlug;
        if let Some(fault) = &self.fault {
            let domain = match fault.slug() {
                FaultSlug::Protocol
                | FaultSlug::RequestMismatch
                | FaultSlug::Freshness
                | FaultSlug::IncoherentView => false,
                FaultSlug::NotFound
                | FaultSlug::WrongBasis
                | FaultSlug::InvalidQuery
                | FaultSlug::QueryPreparation
                | FaultSlug::CompilerRefused
                | FaultSlug::PartiallyPublished
                | FaultSlug::CursorMismatch
                | FaultSlug::SequenceOverflow
                | FaultSlug::MutationRequiresOwner
                | FaultSlug::Endpoint
                | FaultSlug::Transport
                | FaultSlug::LaneUnavailable
                | FaultSlug::LanePartial
                | FaultSlug::SourceUnavailable
                | FaultSlug::Rejected
                | FaultSlug::Usage => true,
            };
            if domain {
                return Ok(refused(fault));
            }
        }
        Err(self)
    }
    fn into_reply(self, id: Value) -> Value {
        let mut data = Map::new();
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

/// Projects the startup fault when its exact input call cannot fit the MCP
/// result budget. Large user operands and diagnostics are shortened with an
/// explicit marker so the typed refusal itself remains within the hard cap.
fn bounded_startup_fault(fault: &Fault) -> Value {
    const FIELDS: [&str; 2] = ["operand", "detail"];
    let mut structured = fault_value(fault);
    if let Some(fields) = structured.as_object_mut() {
        fields.remove("shell");
        fields.remove("call");
        for field in FIELDS {
            if let Some(Value::String(value)) = fields.get_mut(field) {
                *value = bounded_fault_field(value);
            }
        }
    }
    structured
}

fn bounded_fault_field(text: &str) -> String {
    const MAX_BYTES: usize = 1024;
    const MARKER: &str = "…[shortened for response budget]";
    if text.len() <= MAX_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_BYTES.saturating_sub(MARKER.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MARKER}", &text[..end])
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

fn index_job_ticket(arguments: &Map<String, Value>) -> Result<IndexJobTicket, RpcError> {
    backend_present::decode_index_job_ticket(arguments.get("ticket").unwrap_or(&Value::Null))
        .map_err(|fault| RpcError::from_fault(&fault))
}

fn surface_error_operand(command: &SurfaceCommand) -> backend_present::Operand {
    let package = match command {
        SurfaceCommand::IndexStart { package, .. } => Some(package),
        SurfaceCommand::IndexProgress { ticket, .. }
        | SurfaceCommand::IndexCancel { ticket }
        | SurfaceCommand::IndexAwait { ticket } => Some(ticket.package()),
        _ => None,
    };
    backend_present::Operand::Text(
        package
            .map_or(SURFACE_TOOL, PackageReference::as_str)
            .to_owned(),
    )
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
    #[serde(skip_serializing_if = "Option::is_none")]
    fault: Option<backend_present::FaultDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    index_poll: Option<backend_present::IndexPollDto>,
    surface: SurfaceProjection<'a>,
}

struct SurfaceProjection<'a> {
    reply: &'a SurfaceReply,
    next_cursor: Option<&'a str>,
    index_job: Option<&'a backend_present::IndexJobProjection>,
}

impl Serialize for SurfaceProjection<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.reply {
            SurfaceReply::IndexSearchPage(page) => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry("result", "index-search-page")?;
                map.serialize_entry(
                    "data",
                    &IndexSearchPageProjection {
                        snapshot: &page.snapshot,
                        evaluated_at_millis: page.evaluated_at_millis,
                        hits: &page.hits,
                        next_cursor: self.next_cursor,
                        result_count: &page.result_count,
                    },
                )?;
                map.end()
            }
            SurfaceReply::IndexStarted(data) => {
                self.serialize_job(serializer, "index-started", data)
            }
            SurfaceReply::IndexTerminal(data) => {
                self.serialize_job(serializer, "index-terminal", data)
            }
            SurfaceReply::IndexProgress(data) => {
                self.serialize_job(serializer, "index-progress", data)
            }
            SurfaceReply::IndexCancellation(data) => {
                self.serialize_job(serializer, "index-cancellation", data)
            }
            reply => reply.serialize(serializer),
        }
    }
}

impl SurfaceProjection<'_> {
    fn serialize_job<S, D>(
        &self,
        serializer: S,
        result: &'static str,
        data: &D,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        D: Serialize,
    {
        let Some(index_job) = self.index_job else {
            return self.reply.serialize(serializer);
        };
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("result", result)?;
        map.serialize_entry("data", data)?;
        map.serialize_entry("index_job", index_job)?;
        map.end()
    }
}

#[derive(Serialize)]
struct IndexSearchPageProjection<'a> {
    snapshot: &'a [u8; 32],
    evaluated_at_millis: u64,
    hits: &'a [backend_library::RegistrySearchHit],
    next_cursor: Option<&'a str>,
    result_count: &'a backend_library::IndexSearchResultCount,
}

#[cfg(test)]
#[path = "jsonrpc/tests.rs"]
mod tests;

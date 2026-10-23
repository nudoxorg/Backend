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
//!   CLI's `--format json` emits, except that semantic selection returns a
//!   pre-admitted mutation acknowledgement and a separate metadata read;
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
#[cfg(test)]
use backend_present::cl100k_tokens;
use backend_present::{
    Answer, BudgetExceeded, CL100K_POLICY, CL100K_VOCABULARY_SHA256, ContinuationCursor,
    ContinuationTarget, CursorTarget, DEFAULT_RESPONSE_BUDGET_BYTES, Detail,
    ESTIMATED_BYTES_PER_TOKEN, Engine, Fault, Invocation, McpExecutableCall, McpExposure,
    McpInvocation, Probe, Request, answer_paged, bounded_text, encode_answer_with_call,
    encode_serializable, estimate_tokens, fault_value, lower_with_opener, markdown,
    oversized_fault, record_list,
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
mod package_graph;
mod record_pages;
mod semantic_pages;
mod semantic_selection;
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
    INDEX_CANCEL_TOOL, INDEX_PROGRESS_TOOL, INDEX_START_TOOL, NamedToolCall, QUERY_TOOL,
    SURFACE_TOOL, ToolRoute, list_tools, tool_route, validate_registry_arguments,
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
    /// Whether this product has the explicit owner-side rehydration route for
    /// compact outline witnesses. Other products retain full-proof cursors.
    fn supports_outline_owner_witness_route(&self) -> bool {
        false
    }

    /// Already-selected local serving metadata; must not connect or start an owner.
    /// Frozen local bundle/attachment metadata, available without connecting.
    fn cli_context(&self) -> Option<backend_present::StartupRecoveryContext> {
        None
    }

    fn set_request_context(&mut self, context: Option<RequestContext>) -> Result<(), ClientError> {
        context.as_ref().map_or(Ok(()), RequestContext::check)
    }

    /// Request frames the owner will still accept on this connection, or
    /// `None` when the product has no per-connection budget. The owner closes
    /// a connection that has served its budget, so a call that must not be
    /// replayed is only safe to write while this is not nearly exhausted.
    fn connection_frames_remaining(&self) -> Option<usize> {
        None
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

    /// Reads at most the native delivery capacity under unchanged issued credit.
    fn graph_page_window(
        &mut self,
        coordinate: String,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if window.is_windowed() {
            return Err(ClientError::Protocol(
                "product does not expose graph delivery windows".to_owned(),
            ));
        }
        self.graph_page(coordinate, window.issued().get(), continuation)
    }

    /// Resumes an outline from an authenticated compact token whose page root
    /// remains untrusted until the owner reconstructs its predecessor rows.
    fn outline_page_token(
        &mut self,
        _coordinate: String,
        _window: backend_library::QueryWindow,
        _token: &str,
    ) -> Result<ReplyDto, ClientError> {
        Err(ClientError::Protocol(
            "this product does not expose owner-rehydrated outline tokens".to_owned(),
        ))
    }

    /// Reads a selected declaration window, without weakening its opaque cursor.
    fn graph_page_declaration(
        &mut self,
        selector: backend_library::DeclarationSelector,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let _ = (selector, window, continuation);
        Err(ClientError::Protocol(
            "this product does not expose selected graph windows".to_owned(),
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
    fn members_page(
        &mut self,
        document: &backend_library::Document,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.0.members_page(document, limit, continuation)
    }

    fn index(
        &mut self,
        package: PackageReference,
        execution_intent: CompileExecutionIntent,
        cargo_target: Option<backend_library::CargoTargetSelector>,
    ) -> Result<SurfaceReply, ClientError> {
        let context = self.0.request_context().cloned();
        backend_client::index_with_journal_recovery_wait_for_target(
            &self.1,
            package,
            execution_intent,
            cargo_target,
            context.as_ref(),
            |dispatch| self.0.surface(dispatch.command()),
        )
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
            Probe::Declaration { selector, read } => self.0.declaration(selector, read),
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

    fn probe_page_window(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
        window: backend_library::QueryWindow,
    ) -> Result<ReplyDto, ClientError> {
        match probe {
            Probe::Search { text, limit } if limit == window.issued().get() => {
                self.0.search_page_window(text, window, continuation)
            }
            Probe::Names { text, limit } if limit == window.issued().get() => {
                self.0.names_page_window(text, window, continuation)
            }
            Probe::OutlinePage { path, limit } if limit == window.issued().get() => {
                self.0.outline_page_window(path, window, continuation)
            }
            _ => Err(ClientError::Protocol(
                "catalog window changed query family/credit".to_owned(),
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
    fn supports_outline_owner_witness_route(&self) -> bool {
        self.0.supports_outline_owner_witness_route()
    }

    fn cli_context(&self) -> Option<backend_present::StartupRecoveryContext> {
        observed_cli_context(&self.1)
    }

    fn set_request_context(&mut self, context: Option<RequestContext>) -> Result<(), ClientError> {
        self.0.set_request_context(context)
    }

    fn connection_frames_remaining(&self) -> Option<usize> {
        self.0.connection_frames_remaining()
    }

    fn encode_continuation(
        &mut self,
        continuation: PageContinuation,
    ) -> Result<String, ClientError> {
        if self.supports_outline_owner_witness_route()
            && self
            .0
            .outline_query_continuation_compact_eligible(continuation)
        {
            self.0.encode_outline_query_continuation(continuation)
        } else if self.0.has_portable_query_continuation(continuation) {
            self.0.encode_query_continuation(continuation)
        } else {
            Ok(self.0.encode_page_continuation(continuation))
        }
    }

    fn decode_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        self.0.decode_page_continuation(token)
    }

    fn outline_page_token(
        &mut self,
        coordinate: String,
        window: backend_library::QueryWindow,
        token: &str,
    ) -> Result<ReplyDto, ClientError> {
        self.0
            .outline_page_from_query_token(&coordinate, window, token)
    }

    fn graph_page(
        &mut self,
        coordinate: String,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.0.graph_page(&coordinate, limit, continuation)
    }

    fn graph_page_window(
        &mut self,
        coordinate: String,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.0.graph_page_window(&coordinate, window, continuation)
    }

    fn graph_page_declaration(
        &mut self,
        selector: backend_library::DeclarationSelector,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.0
            .graph_page_declaration(&selector, window, continuation)
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
    cli_context: Option<backend_present::StartupRecoveryContext>,
    /// Bounded continuation state retained if the daemon is still between
    /// leases and the replacement dial has not succeeded yet.
    pending_continuation_state: Option<backend_client::SessionContinuationState>,
    request_context: Option<RequestContext>,
}

impl reconnect::Endpoint for SessionEndpoint {
    type Product = SessionProduct;

    fn cli_context(&self) -> Option<backend_present::StartupRecoveryContext> {
        self.cli_context.clone()
    }

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
        let context = self.request_context.as_ref();
        let mut session = connect_session_owner_with(
            self.paths.endpoint(),
            context,
            |_endpoint| {
                let connected = match context {
                    Some(context) => Session::connect_matched_owner(&self.paths, Some(context), false),
                    None => Session::connect_matched_owner(&self.paths, None, false),
                };
                if connected.is_err()
                    && let Some(context) = context
                {
                    context.check()?;
                }
                connected
            },
            || {
                let startup_budget = session_owner_startup_budget(context)?;
                backend_runtime::ensure_locald_cancellable(
                    &self.paths,
                    Some(startup_budget),
                    || context.is_none_or(|context| context.cancellation_reason().is_none()),
                )
                .map_err(|error| map_runtime_startup_error(error, context))
            },
        )?;
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

// Owner startup precedes command admission. Bound that observation separately
// so a cold first mutation can report Starting without installing a deadline
// on the mutation itself or manufacturing an accepted operation receipt.
const MCP_OWNER_STARTUP_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

fn session_owner_startup_budget(
    context: Option<&RequestContext>,
) -> Result<std::time::Duration, ClientError> {
    Ok(context
        .map(RequestContext::remaining_read_budget)
        .transpose()?
        .flatten()
        .map_or(MCP_OWNER_STARTUP_BUDGET, |remaining| {
            remaining.min(MCP_OWNER_STARTUP_BUDGET)
        }))
}

/// Initial safe-read connections use the same bounded owner convergence as
/// CLI recovery. A readiness probe is not a lease: its final dial may refuse.
/// Mutation contexts keep one startup/dial and acquire no execution deadline;
/// their separate startup observation can return honest Pending before dispatch.
fn connect_session_owner_with<T>(
    endpoint: &std::path::Path,
    context: Option<&RequestContext>,
    mut dial: impl FnMut(&std::path::Path) -> Result<T, ClientError>,
    mut bootstrap: impl FnMut() -> Result<std::path::PathBuf, ClientError>,
) -> Result<T, ClientError> {
    if let Some(context) = context
        && context.remaining_read_budget()?.is_some()
    {
        return backend_client::connect_local_owner_with(context, endpoint, dial, bootstrap);
    }
    let ready = bootstrap()?;
    dial(&ready)
}

pub(super) type ReconnectingProduct = reconnect::Reconnecting<SessionEndpoint>;

pub(super) fn disconnected_reconnecting_product(
    paths: &backend_runtime::WorkspacePaths,
) -> ReconnectingProduct {
    reconnect::Reconnecting::disconnected(SessionEndpoint {
        paths: paths.clone(),
        cli_context: observed_cli_context(paths),
        pending_continuation_state: None,
        request_context: None,
    })
}

fn observed_cli_context(
    paths: &backend_runtime::WorkspacePaths,
) -> Option<backend_present::StartupRecoveryContext> {
    #[cfg(unix)]
    {
        let executable = backend_runtime::installed_cli_executable()?;
        cli_context_for_executable(paths, &executable)
    }
    #[cfg(not(unix))]
    {
        let _ = paths;
        None
    }
}

#[cfg(unix)]
fn cli_context_for_executable(
    paths: &backend_runtime::WorkspacePaths,
    executable: &std::path::Path,
) -> Option<backend_present::StartupRecoveryContext> {
    fn operand(path: &std::path::Path) -> Option<String> {
        let text = path.to_str()?;
        (path.is_absolute() && !text.contains('\0')).then(|| text.to_owned())
    }
    let executable = backend_runtime::observed_cli_executable(executable)?;
    let selected = paths.startup_recovery_selection()?;
    if !selected.workspace_authority {
        return None;
    }
    operand(selected.data)?;
    let directory = match selected.workspace {
        backend_runtime::StartupWorkspaceSelection::Explicit(path) => {
            backend_present::StartupRecoveryWorkspace::Explicit(operand(path)?)
        }
        backend_runtime::StartupWorkspaceSelection::Home(anchor) => {
            backend_present::StartupRecoveryWorkspace::Home(operand(anchor)?)
        }
        backend_runtime::StartupWorkspaceSelection::XdgState(anchor) => {
            backend_present::StartupRecoveryWorkspace::XdgState(operand(anchor)?)
        }
        backend_runtime::StartupWorkspaceSelection::XdgData(anchor) => {
            backend_present::StartupRecoveryWorkspace::XdgData(operand(anchor)?)
        }
    };
    Some(backend_present::StartupRecoveryContext::new(
        operand(selected.project)?,
        operand(selected.endpoint)?,
        operand(&executable)?,
        backend_present::StartupRecoveryCommand::Status,
        backend_present::StartupRecoveryMode::Active,
        directory,
    ))
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

/// The named package-profile tool treats `.` as the MCP session's selected
/// project. Keep the generic `backend.surface` escape hatch source-exact.
fn normalize_named_package_profile_dot(request: Request, project: &str) -> Result<Request, Fault> {
    let Request::Surface(command) = request else {
        return Ok(request);
    };
    let (label, compiler_outcome) = match *command {
        SurfaceCommand::PackageProfile {
            package: PackageReference::Local(label),
            compiler_outcome,
        } if label.as_str() == "." => (label, compiler_outcome),
        other => return Ok(Request::Surface(Box::new(other))),
    };
    let selected = std::path::Path::new(project);
    if !selected.is_absolute() {
        return Ok(Request::Surface(Box::new(SurfaceCommand::PackageProfile {
            package: PackageReference::Local(label),
            compiler_outcome,
        })));
    }
    let selected = backend_runtime::normalize_surface_path(selected);
    let Some(selected) = selected.to_str() else {
        return Ok(Request::Surface(Box::new(SurfaceCommand::PackageProfile {
            package: PackageReference::Local(label),
            compiler_outcome,
        })));
    };
    let package = PackageReference::from_kind(
        backend_library::PackageReferenceKind::Local,
        selected.to_owned(),
    )
    .map_err(|error| Fault::admission(error, backend_present::Operand::Argument(".".to_owned())))?;
    let command = SurfaceCommand::PackageProfile {
        package,
        compiler_outcome,
    };
    command.admit().map_err(|error| {
        Fault::admission(error, backend_present::Operand::Argument(".".to_owned()))
    })?;
    Ok(Request::Surface(Box::new(command)))
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
            // Admission and the routed read share the same typed failure
            // contract. In particular an exhausted original deadline must not
            // become an unclassified transport error before tools/call runs.
            let error = RpcError::from_fault(&Fault::from_client_error(
                &error,
                backend_present::Operand::Whole,
            ));
            return Some(
                if value.get("method").and_then(Value::as_str) == Some("tools/call") {
                    match error.into_tool_result() {
                        Ok(result) => success(id, result),
                        Err(error) => error.into_reply(id),
                    }
                } else {
                    error.into_reply(id)
                },
            );
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
            match self
                .route(method, &params, &response_id)
                .and_then(bound_route_value)
            {
                Ok(result) => success(response_id, result),
                Err(error) => error.into_reply(response_id),
            },
        )
    }

    fn route(
        &mut self,
        method: &str,
        params: &Value,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        // Product-bearing tools/resources perform typed admission before they
        // become `Value`s. The registry and protocol metadata are finite
        // constructors (with their schema budget checked in the token audit);
        // this final gate covers their JSON-RPC envelope as well.
        match method {
            "ping" => Ok(json!({})),
            "tools/list" => list_tools(params),
            "tools/call" => self.call_tool(params, response_id),
            "resources/list" => resources::list_resources(params),
            "resources/templates/list" => resources::list_resource_templates(params),
            "resources/read" => self.read_resource(params, response_id),
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
            "\n\nQuick start: call backend.packages, then backend.index with the absolute project path. \
             For accepted work, copy index_poll.call and inspect its terminal receipt before backend.search. CLI support is optional: \
             source builds may have backend-cli, and index_poll.shell is absent when no runnable CLI attachment is observed. \
             Use `backend.package` for one pinned registry package and \
             `backend.index_search` for a name-first registry lookup.",
        );
        instructions.push_str("\n\n");
        instructions.push_str(crate::process::STDIO_SETUP_GUIDANCE);
        instructions.push_str("\n\n");
        instructions.push_str(&self.instructions_context());
        instructions.push_str(
            "\n- when this path is not the repository you were asked about, pass that repository's absolute path to backend.index",
        );
        instructions
    }

    fn read_resource(&mut self, params: &Value, response_id: &Value) -> Result<Value, RpcError> {
        if let Some(uri) = params.get("uri").and_then(Value::as_str)
            && uri.starts_with("backend://semantic-source/")
        {
            no_extra(object(params)?, &["uri", "_meta"])?;
            let request = backend_library::SemanticSourceRequest::from_resource_uri(uri)
                .map_err(|error| RpcError::invalid(format!("semantic source URI: {error}")))?;
            return self.semantic_source_resource(&request, uri, response_id);
        }
        let is_workspace = params
            .get("uri")
            .and_then(Value::as_str)
            .is_some_and(|uri| uri == "backend://workspace/current");
        let context =
            (is_workspace && self.working_directory.is_some()).then(|| self.instructions_context());
        resources::read_resource(&mut self.product, params, context.as_deref(), response_id)
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

    fn call_tool(&mut self, params: &Value, response_id: &Value) -> Result<Value, RpcError> {
        let params = object(params)?;
        let name = string(params, "name")?;
        let route = tool_route(name).ok_or_else(|| RpcError::new(-32602, "Unknown tool"))?;
        // Admit the CallToolRequest envelope before interpreting tool inputs.
        if !matches!(params.get("arguments"), None | Some(Value::Object(_))) {
            return Err(RpcError::invalid("arguments must be an object"));
        }
        let empty = Map::new();
        let arguments = params
            .get("arguments")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        self.call_named_tool(arguments, name, route, McpExposure::Direct, response_id)
    }

    fn call_named_tool(
        &mut self,
        arguments: &Map<String, Value>,
        name: &str,
        route: ToolRoute,
        exposure: McpExposure,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        let invocation = McpInvocation::new(exposure, name, arguments);
        let preflight = if name == "backend.search" {
            let grammar = backend_present::grammar_for_tool(name)
                .ok_or_else(|| RpcError::new(-32602, "Unknown tool"));
            grammar.and_then(|grammar| {
                if arguments
                    .get("query")
                    .and_then(Value::as_str)
                    .is_some_and(|query| query.trim().is_empty())
                {
                    return Err(RpcError::from_fault(&Fault::usage(
                        "query",
                        "backend.search requires at least one non-whitespace character; pass a search term",
                    )));
                }
                let mut search_arguments = arguments.clone();
                search_arguments.remove("detail");
                validate_registry_arguments(grammar, &search_arguments)
                    .map_err(|fault| RpcError::from_fault(&fault))?;
                Invocation::from_json(grammar, &search_arguments)
                    .map(|_| ())
                    .map_err(|fault| RpcError::from_fault(&fault))
            })
        } else {
            Ok(())
        };
        preflight
            .and_then(|()| self.call_routed_tool(arguments, name, route, invocation, response_id))
            .or_else(|error| error.into_tool_result_for_invocation(invocation))
    }
    /// A recognized route owns input/domain refusals. Envelope and retained
    /// proof failures remain JSON-RPC errors; no display string classifies one.
    fn call_routed_tool(
        &mut self,
        arguments: &Map<String, Value>,
        name: &str,
        route: ToolRoute,
        invocation: McpInvocation<'_>,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        if matches!(route, ToolRoute::NamedCall) {
            let call = NamedToolCall::admit(arguments)?;
            return self.call_named_tool(
                call.arguments(),
                call.name(),
                call.route(),
                McpExposure::RegisteredCall,
                response_id,
            );
        }
        // The MCP schema requires an explicit path even though the shared CLI
        // grammar permits its project default. Admit it before any owner call.
        if name == "backend.index" {
            let Some(path) = arguments
                .get("path")
                .and_then(Value::as_str)
                .filter(|path| !path.trim().is_empty())
            else {
                return Err(RpcError::from_fault(&Fault::usage(
                    "path",
                    "backend.index requires arguments.path: pass the repository's absolute path; use backend.index_start for a resumable operation",
                )));
            };
            // This precedes opener admission, reconnect/startup and the durable
            // caller journal. A bad local operand must not occupy this session
            // while an owner processes a mutation it cannot admit.
            let target = std::path::Path::new(path);
            if !target.is_absolute() || !target.is_dir() {
                return Err(RpcError::from_fault(&Fault::usage(
                    "path",
                    "backend.index requires an existing absolute directory; correct arguments.path before indexing; no index operation was submitted",
                )));
            }
        }
        let detail = response_detail(name, arguments)?;
        if matches!(route, ToolRoute::Surface) {
            let mut surface_arguments = arguments.clone();
            surface_arguments.remove("detail");
            let context = continuation_context(&self.project, name, &surface_arguments, detail);
            return self.surface_tool(
                &surface_arguments,
                detail,
                &context,
                invocation,
                response_id,
            );
        }
        if matches!(
            route,
            ToolRoute::IndexStart | ToolRoute::IndexProgress | ToolRoute::IndexCancel
        ) {
            let context = continuation_context(&self.project, name, arguments, detail);
            return self.index_job_tool(name, arguments, detail, &context, invocation.exposure());
        }
        if matches!(route, ToolRoute::RefusedIndexAwait) {
            return Err(RpcError::from_fault(&Fault::usage(
                "ticket",
                "Use backend.index_progress for bounded polling",
            )));
        }
        if name == "backend.outline" {
            return self.outline_tool(arguments, detail, invocation, response_id);
        }
        let context = continuation_context(&self.project, name, arguments, detail);
        let grammar = match route {
            ToolRoute::Registry(grammar) => Some(grammar),
            ToolRoute::Query | ToolRoute::Surface => None,
            ToolRoute::IndexStart
            | ToolRoute::IndexProgress
            | ToolRoute::IndexCancel
            | ToolRoute::RefusedIndexAwait
            | ToolRoute::NamedCall => None,
        };
        let cursor_domain = grammar.and_then(|grammar| grammar.continuation_domain());
        let index_search_tool = matches!(
            cursor_domain,
            Some(backend_present::ContinuationDomain::PresentationOwnerToken)
        );
        let index_search_cursor = if index_search_tool {
            self.index_search_cursor(arguments, &context)?
        } else {
            None
        };
        let continuation = if matches!(route, ToolRoute::Query)
            || matches!(
                cursor_domain,
                Some(backend_present::ContinuationDomain::PresentationPage)
            ) {
            self.continuation(arguments, &context)?
        } else {
            None
        };
        if matches!(route, ToolRoute::Query) {
            return self.query_tool(arguments, detail, continuation, invocation, response_id);
        }
        if grammar.is_some_and(|grammar| grammar.name() == "graph") {
            return self.graph_tool(arguments, detail, continuation, invocation, response_id);
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
        } else if matches!(
            cursor_domain,
            Some(backend_present::ContinuationDomain::PresentationPage)
        ) {
            command_arguments.remove("cursor");
        }
        // Native domains retain the exact input cursor for their owner to
        // admit. A command with no cursor grammar rejects the unknown field.
        let opener = self.opener()?;
        let planned = validate_registry_arguments(grammar, &command_arguments)
            .and_then(|()| Invocation::from_json(grammar, &command_arguments))
            .and_then(|invocation| lower_with_opener(&invocation, &self.project, opener))
            .and_then(|request| {
                if name == "backend.package_profile" {
                    normalize_named_package_profile_dot(request, &self.project)
                } else {
                    Ok(request)
                }
            });
        match planned {
            Ok(request)
                if matches!(&request, Request::Surface(command)
                if matches!(command.as_ref(), SurfaceCommand::SemanticVersions { .. })) =>
            {
                let Request::Surface(command) = request else {
                    unreachable!()
                };
                self.semantic_versions_tool(&command, detail, invocation, response_id)
            }
            Ok(Request::Surface(command))
                if matches!(command.as_ref(), SurfaceCommand::SemanticSource { .. }) =>
            {
                let SurfaceCommand::SemanticSource { request } = command.as_ref() else {
                    unreachable!()
                };
                self.semantic_source_tool(request, detail, invocation, response_id)
            }
            Ok(Request::Surface(command))
                if matches!(command.as_ref(), SurfaceCommand::PackageGraphPage { .. }) =>
            {
                let SurfaceCommand::PackageGraphPage { request } = command.as_ref() else {
                    unreachable!()
                };
                self.package_graph_tool(
                    request,
                    arguments,
                    detail,
                    invocation,
                    response_id,
                    package_graph::GraphToolProjection::Registered,
                )
            }
            Ok(Request::Surface(command))
                if matches!(
                    command.as_ref(),
                    SurfaceCommand::PackageSourceMembership { .. }
                ) =>
            {
                let SurfaceCommand::PackageSourceMembership { request } = command.as_ref() else {
                    unreachable!()
                };
                self.source_membership_tool(request, detail, invocation, response_id)
            }
            Ok(Request::Surface(command))
                if matches!(
                    command.as_ref(),
                    SurfaceCommand::SelectSemanticVersion { .. }
                ) =>
            {
                self.select_semantic_version_tool(&command, detail, invocation, response_id)
            }
            Ok(request) if matches!(&request, Request::Search { .. } | Request::Resolve { .. }) => {
                self.records_tool(&request, detail, continuation, invocation, response_id)
            }
            Ok(request) => match answer_paged(&mut self.product, &request, continuation) {
                Ok(answer) => self.rendered(
                    answer,
                    detail,
                    &context,
                    if index_search_tool {
                        CursorTarget::McpIndexSearchTool
                    } else {
                        CursorTarget::CliOption
                    },
                    invocation,
                ),
                Err(fault) => Err(RpcError::from_fault(&fault)),
            },
            Err(fault) => Err(RpcError::from_fault(&fault)),
        }
    }
    /// A complete native outline page is selected before output admission.
    /// The first page selects issued credit. Continued deliveries may shrink
    /// under that same ceiling; native witnesses bind each exact prior span.
    /// Every attempt retains the original input cursor and source frontier.
    fn outline_tool(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
        invocation: McpInvocation<'_>,
        response_id: &Value,
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
        validate_registry_arguments(grammar, &command)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let planned_invocation = Invocation::from_json(grammar, &command)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let request = lower_with_opener(&planned_invocation, &self.project, self.opener()?)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let Request::Outline(path) = request else {
            return Err(RpcError::invalid("outline request changed shape"));
        };
        let mut effective = match arguments.get("limit") {
            Some(_) => limit(arguments)?,
            None => backend_library::QueryLimit::MAX,
        };
        let mut delivery = effective;
        let mut page_arguments = arguments.clone();
        page_arguments.insert("path".to_owned(), json!(path));
        page_arguments.insert("limit".to_owned(), json!(effective));
        let mut context =
            continuation_context(&self.project, "backend.outline", &page_arguments, detail);
        // PC5 is authenticated by the same workspace/path/limit wrapper as
        // legacy cursors, but its inner page-root commitment is deliberately
        // not decoded as a trusted PageContinuation. The owner rehydrates it
        // through the dedicated outline route below.
        let outline_owner_token = match arguments
            .get("cursor")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
        {
            Some(token) => self
                .verify_cursor_token(token, &context)?
                .filter(|owner| owner.starts_with("pc5-")),
            None => None,
        };
        let continuation = if outline_owner_token.is_some() {
            None
        } else {
            self.continuation(arguments, &context)?
        };
        let has_continuation = continuation.is_some() || outline_owner_token.is_some();
        let mut pinned = None;
        let mut pinned_revision = None;
        loop {
            lifetime.check().map_err(|error| {
                RpcError::from_fault(&Fault::from_client_error(
                    &error,
                    backend_present::Operand::Whole,
                ))
            })?;
            if outline_owner_token.is_none() {
                self.reimport_catalog_cursor(&page_arguments, detail, invocation, continuation)?;
            }
            let selected = Request::OutlinePage { path: path.clone(), limit: effective };
            let probe = Probe::OutlinePage { path: &path, limit: effective };
            let window = record_pages::query_window(effective, delivery)?;
            let reply = if let Some(token) = outline_owner_token.as_deref() {
                self.product.outline_page_token(path.clone(), window, token)
            } else {
                self.product.probe_page_window(probe, continuation, window)
            };
            lifetime.check().map_err(semantic_pages::semantic_client_error)?;
            let reply = match reply {
                Err(ClientError::ResponseTooLarge { basis: Some(basis) }) => {
                    record_pages::pin_revision(&mut pinned_revision, basis)?;
                    if record_pages::shrink_window(has_continuation, &mut effective,
                        &mut delivery, &mut page_arguments) {
                        context = continuation_context(&self.project, "backend.outline", &page_arguments, detail);
                        continue;
                    }
                    return Err(RpcError::from_fault(&backend_present::probe_fault(
                        &ClientError::ResponseTooLarge { basis: Some(basis) }, probe)));
                }
                Err(error) => return Err(RpcError::from_fault(&backend_present::probe_fault(&error, probe))),
                Ok(reply) => reply,
            };
            let answer = backend_present::catalog_page_answer(&selected, reply)
                .map_err(|fault| RpcError::from_fault(&fault))?;
            let Answer::Outline(tree) = &answer else {
                return Err(RpcError::invalid("outline page changed shape"));
            };
            let basis = tree
                .page_basis()
                .ok_or_else(|| RpcError::invalid("outline page omits producer basis"))?;
            record_pages::pin_revision(&mut pinned_revision, basis.0.root.into())?;
            if pinned.is_some_and(|expected| expected != basis) {
                return Err(RpcError::tool(
                    "outline source changed while selecting a bounded page; restart the outline explicitly",
                ));
            }
            pinned = Some(basis);
            let mut result = self.rendered(
                answer,
                detail,
                &context,
                CursorTarget::CliOption,
                invocation.with_arguments(&page_arguments),
            )?;
            // The opaque token appears once. Copy these exact arguments and
            // take cursor from structuredContent.nextCursor; no second token
            // copy is charged to the agent context.
            if invocation.exposure() == McpExposure::Direct
                && result
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
            // Select using the complete envelope and the actual caller ID.
            let candidate = MeasuredEnvelope::new(
                attach_wire_budget(
                    codec::success(response_id.clone(), result),
                    ResponseTransport::StdioLine,
                ),
                ResponseTransport::StdioLine,
            );
            let fitted = if candidate.fits() {
                candidate
            } else {
                fit_measured_preview(candidate, ResponseTransport::StdioLine)
            };
            lifetime.check().map_err(semantic_pages::semantic_client_error)?;
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
            if delivery == 1 {
                let mut frame = fitted.into_value();
                return Ok(frame
                    .as_object_mut()
                    .and_then(|frame| frame.remove("result"))
                    .unwrap_or(Value::Null));
            }
            if !record_pages::shrink_window(has_continuation, &mut effective,
                &mut delivery, &mut page_arguments) {
                return Err(RpcError::tool("outline delivery cannot make progress"));
            }
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
        invocation: McpInvocation<'_>,
        response_id: &Value,
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
        let lifetime = self.semantic_lifetime()?;
        let budget = wire_admission::ResponseBudget::new(response_id);
        let mut effective = limit(arguments)?;
        let mut arguments = arguments.clone();
        let mut pinned = None;
        loop {
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            let page = self
                .product
                .graph_query(input.clone(), effective, continuation)
                .map_err(|error| {
                    RpcError::from_fault(&Fault::from_client_error(
                        &error,
                        backend_present::Operand::Text(query.clone()),
                    ))
                })?;
            let basis = (page.revision, page.source);
            if pinned.is_some_and(|expected| expected != basis) {
                return Err(RpcError::tool(
                    "query source changed while selecting a bounded page; restart the query explicitly",
                ));
            }
            pinned = Some(basis);
            let context = continuation_context(
                &self.project,
                invocation.selected_name(),
                &arguments,
                detail,
            );
            let result = self
                .graph_page_result(
                    &page,
                    detail,
                    &context,
                    invocation.with_arguments(&arguments),
                )
                .and_then(|result| budget.admit(result));
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            match result {
                Ok(result) => return Ok(result),
                Err(error)
                    if continuation.is_none()
                        && effective > 1
                        && semantic_pages::oversized(&error) =>
                {
                    effective = effective.div_ceil(2);
                    arguments.insert("limit".to_owned(), json!(effective));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn graph_tool(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
        continuation: Option<PageContinuation>,
        invocation: McpInvocation<'_>,
        response_id: &Value,
    ) -> Result<Value, RpcError> {
        no_extra(
            arguments,
            &["coordinate", "selected_symbol", "limit", "cursor", "detail"],
        )?;
        let operands = arguments
            .iter()
            .filter(|(key, _)| matches!(key.as_str(), "coordinate" | "selected_symbol"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let grammar = backend_present::grammar_for("graph").expect("graph grammar");
        tools::validate_registry_arguments(grammar, &operands)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let invocation_operand = backend_present::Invocation::from_json(grammar, &operands)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let selector = backend_present::declaration_selector(&invocation_operand)
            .map_err(|fault| RpcError::from_fault(&fault))?;
        let selector = match selector {
            backend_library::DeclarationSelector::Coordinate(coordinate) => {
                backend_library::DeclarationSelector::Coordinate(
                    backend_present::resolve_declaration(&mut self.product, &coordinate, "graph")
                        .map_err(|fault| RpcError::from_fault(&fault))?,
                )
            }
            selected => selected,
        };
        let coordinate = match &selector {
            backend_library::DeclarationSelector::Coordinate(value)
            | backend_library::DeclarationSelector::SelectedAt {
                coordinate: value, ..
            } => value.clone(),
            backend_library::DeclarationSelector::Selected(id) => {
                backend_library::encode_id(id.bytes())
            }
        };
        let probe = if selector.selected_id().is_some() {
            Probe::Declaration {
                selector: &selector,
                read: backend_library::DeclarationRead::Graph,
            }
        } else {
            Probe::Graph(&coordinate)
        };
        let lifetime = self.semantic_lifetime()?;
        let budget = wire_admission::ResponseBudget::new(response_id);
        let mut effective = limit(arguments)?;
        let mut delivery = effective;
        let mut arguments = arguments.clone();
        let mut pinned = None;
        let mut pinned_revision = None;
        loop {
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            let window = record_pages::query_window(effective, delivery)?;
            self.reimport_catalog_cursor(&arguments, detail, invocation, continuation)?;
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            let reply = if selector.selected_id().is_some() {
                self.product
                    .graph_page_declaration(selector.clone(), window, continuation)
            } else {
                self.product
                    .graph_page_window(coordinate.clone(), window, continuation)
            };
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            let reply = match reply {
                Err(ClientError::ResponseTooLarge { basis: Some(basis) }) => {
                    record_pages::pin_revision(&mut pinned_revision, basis)?;
                    if record_pages::shrink_window(
                        continuation.is_some(),
                        &mut effective,
                        &mut delivery,
                        &mut arguments,
                    ) {
                        continue;
                    }
                    return Err(RpcError::from_fault(&backend_present::probe_fault(
                        &ClientError::ResponseTooLarge { basis: Some(basis) },
                        probe,
                    )));
                }
                Err(error) => {
                    return Err(RpcError::from_fault(&backend_present::probe_fault(
                        &error, probe,
                    )));
                }
                Ok(reply) => reply,
            };
            let page = match reply.reply {
                backend_library::CommandReply::ProjectionPage(page) => page,
                backend_library::CommandReply::Failed(failure) => {
                    return Err(RpcError::from_fault(&backend_present::probe_fault(
                        &ClientError::CommandFailed(failure),
                        probe,
                    )));
                }
                _ => return Err(RpcError::tool("graph page reply changed shape")),
            };
            if page.terminal
                != page.snapshot.next.map_or(PageTerminal::Complete, |cursor| {
                    PageTerminal::More(PageContinuation::from_cursor(cursor))
                })
            {
                return Err(RpcError::tool(
                    "graph page terminal disagrees with native snapshot",
                ));
            }
            let records = record_list(&coordinate, &page.snapshot);
            let basis = records
                .page_basis()
                .ok_or_else(|| RpcError::tool("graph page omits native scope"))?;
            record_pages::pin_revision(&mut pinned_revision, basis.0.root.into())?;
            if pinned.is_some_and(|expected| expected != basis) {
                return Err(RpcError::tool(
                    "graph source changed while selecting a bounded page; restart discovery explicitly",
                ));
            }
            pinned = Some(basis);
            let context = continuation_context(
                &self.project,
                invocation.selected_name(),
                &arguments,
                detail,
            );
            let result = self
                .rendered_admitted(
                    Answer::Records(Box::new(records)),
                    detail,
                    &context,
                    CursorTarget::CliOption,
                    invocation.with_arguments(&arguments),
                )
                .and_then(|result| budget.admit(result));
            lifetime
                .check()
                .map_err(semantic_pages::semantic_client_error)?;
            match result {
                Ok(result) => return Ok(result),
                Err(error)
                    if semantic_pages::oversized(&error)
                        && record_pages::shrink_window(
                            continuation.is_some(),
                            &mut effective,
                            &mut delivery,
                            &mut arguments,
                        ) => {}
                Err(error) => return Err(error),
            }
        }
    }
    fn surface_tool(
        &mut self,
        arguments: &Map<String, Value>,
        detail: Detail,
        context: &[u8],
        invocation: McpInvocation<'_>,
        response_id: &Value,
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
        if let SurfaceCommand::PackageGraphPage { request } = &command {
            return self.package_graph_tool(
                request,
                arguments,
                detail,
                invocation,
                response_id,
                package_graph::GraphToolProjection::Surface,
            );
        }
        if matches!(&command, SurfaceCommand::SelectSemanticVersion { .. }) {
            return self.select_semantic_version_tool(&command, detail, invocation, response_id);
        }
        if matches!(&command, SurfaceCommand::SemanticVersions { .. }) {
            return self.semantic_versions_tool(&command, detail, invocation, response_id);
        }
        if let SurfaceCommand::SemanticSource { request } = &command {
            return self.semantic_source_tool(request, detail, invocation, response_id);
        }
        if let SurfaceCommand::PackageSourceMembership { request } = &command {
            return self.source_membership_tool(request, detail, invocation, response_id);
        }
        let operand = surface_error_operand(&command);
        let reply = self
            .product
            .surface(command.clone())
            .map_err(|error| match error {
                ClientError::StaleCursor => RpcError::stale_cursor(),
                other => RpcError::from_fault(&Fault::from_client_error(&other, operand)),
            })?;
        self.surface_reply_result(&command, &reply, detail, context, McpExposure::Direct)
    }
    fn index_job_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
        detail: Detail,
        context: &[u8],
        exposure: McpExposure,
    ) -> Result<Value, RpcError> {
        let compact_receipt = match arguments.get("compact_receipt") {
            None => false,
            Some(Value::Bool(value)) if *value && detail != Detail::Summary => {
                return Err(RpcError::invalid("compact_receipt requires detail=summary"));
            }
            Some(Value::Bool(value)) => *value,
            Some(_) => {
                return Err(RpcError::invalid("compact_receipt must be a boolean"));
            }
        };
        let command = match name {
            INDEX_START_TOOL => {
                no_extra(
                    arguments,
                    &[
                        "package",
                        "cargo_target",
                        "execution_intent",
                        "operation_key",
                        "detail",
                    ],
                )?;
                let spelling = string(arguments, "package")?.to_owned();
                let package = PackageReference::parse(spelling)
                    .map_err(|error| RpcError::invalid(error.to_string()))?;
                let cargo_target = arguments
                    .get("cargo_target")
                    .map(|value| {
                        let name = value
                            .as_str()
                            .ok_or_else(|| RpcError::invalid("cargo_target must be a string"))?;
                        backend_library::CargoTargetSelector::new(name.to_owned())
                            .map_err(|error| RpcError::invalid(error.to_owned()))
                    })
                    .transpose()?;
                let execution_intent = match arguments.get("execution_intent") {
                    None => CompileExecutionIntent::Interactive,
                    Some(value) => serde_json::from_value::<CompileExecutionIntent>(value.clone())
                        .map_err(|_| {
                            RpcError::invalid("execution_intent must be interactive or background")
                        })?,
                };
                match arguments.get("operation_key") {
                    Some(key) => SurfaceCommand::IndexOperationStart {
                        cargo_target: cargo_target.clone(),
                        operation_key:
                            serde_json::from_value::<backend_library::IndexOperationKey>(
                                key.clone(),
                            )
                            .map_err(|error| RpcError::invalid(format!("operation_key: {error}")))?,
                        package,
                        execution_intent,
                    },
                    None => {
                        // Default starts share the same saved request claim as
                        // CLI Add and backend.index, so an active matching
                        // request is observed rather than queued under a new key.
                        let reply = self
                            .product
                            .index(package.clone(), execution_intent, cargo_target.clone())
                            .map_err(|error| {
                                RpcError::from_fault(&Fault::from_client_error(
                                    &error,
                                    backend_present::Operand::Text(package.as_str().to_owned()),
                                ))
                            })?;
                        let (observation, started) = match &reply {
                            SurfaceReply::IndexOperationStarted(observation) => (observation, true),
                            SurfaceReply::IndexOperationStatus(observation) => (observation, false),
                            _ => {
                                return Err(RpcError::tool(
                                    "default index_start did not return a durable operation receipt",
                                ));
                            }
                        };
                        let operation_key = match observation {
                            backend_library::IndexOperationObservation::Known(status) => {
                                status.operation_key
                            }
                            backend_library::IndexOperationObservation::Unknown {
                                operation_key,
                            }
                            | backend_library::IndexOperationObservation::OutsideReceiptWindow {
                                operation_key,
                                ..
                            } => *operation_key,
                        };
                        // The shared journal admits the exact canonical tuple;
                        // retain its returned package instead of reconstructing
                        // an absolute path from this tool's original spelling.
                        let command = if started {
                            let package = match observation {
                                backend_library::IndexOperationObservation::Known(status) => {
                                    status.package.clone()
                                }
                                _ => package,
                            };
                            SurfaceCommand::IndexOperationStart {
                                cargo_target,
                                operation_key,
                                package,
                                execution_intent,
                            }
                        } else {
                            SurfaceCommand::IndexOperationStatus { operation_key }
                        };
                        backend_library::admit_surface_reply(&command, &reply).map_err(
                            |error| RpcError::tool(format!("default index_start receipt: {error}")),
                        )?;
                        return self
                            .surface_reply_result(&command, &reply, detail, context, exposure);
                    }
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
                        "compact_receipt",
                        "detail",
                    ],
                )?;
                if let Some(key) = arguments.get("operation_key") {
                    if compact_receipt {
                        return Err(RpcError::invalid(
                            "compact_receipt is only supported with ticket-based index_progress",
                        ));
                    }
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
                    if let Some(result) = self.conditional_index_progress(
                        arguments,
                        &command,
                        detail,
                        context,
                        exposure,
                        compact_receipt,
                    )? {
                        return Ok(result);
                    }
                    let reply = self.product.surface(command.clone()).map_err(|error| {
                        RpcError::from_fault(&Fault::from_client_error(
                            &error,
                            surface_error_operand(&command),
                        ))
                    })?;
                    return self.surface_reply_result_with_compact_receipt(
                        &command,
                        &reply,
                        detail,
                        context,
                        exposure,
                        compact_receipt,
                    );
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
                if let Some(result) = self.conditional_index_progress(
                    arguments,
                    &command,
                    detail,
                    context,
                    exposure,
                    compact_receipt,
                )? {
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
        self.surface_reply_result_with_compact_receipt(
            &command,
            &reply,
            detail,
            context,
            exposure,
            compact_receipt,
        )
    }
    fn surface_reply_result(
        &mut self,
        command: &SurfaceCommand,
        reply: &SurfaceReply,
        detail: Detail,
        context: &[u8],
        exposure: McpExposure,
    ) -> Result<Value, RpcError> {
        self.surface_reply_result_with_compact_receipt(
            command, reply, detail, context, exposure, false,
        )
    }

    fn surface_reply_result_with_compact_receipt(
        &mut self,
        command: &SurfaceCommand,
        reply: &SurfaceReply,
        detail: Detail,
        context: &[u8],
        exposure: McpExposure,
        compact_receipt: bool,
    ) -> Result<Value, RpcError> {
        self.surface_reply_with_profile_progress(
            command,
            reply,
            None,
            detail,
            context,
            exposure,
            compact_receipt,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn surface_reply_with_profile_progress(
        &mut self,
        command: &SurfaceCommand,
        reply: &SurfaceReply,
        profile_progress: Option<backend_library::IndexProgressPage>,
        detail: Detail,
        context: &[u8],
        exposure: McpExposure,
        compact_receipt: bool,
    ) -> Result<Value, RpcError> {
        let mut reply = std::borrow::Cow::Borrowed(reply);
        let mut profile_progress = profile_progress;
        let fit_progress_window = profile_progress
            .as_ref()
            .or_else(|| match reply.as_ref() {
                SurfaceReply::IndexProgress(backend_library::IndexJobObservation::Pending(
                    page,
                )) => Some(page),
                _ => None,
            })
            .is_some_and(|page| {
                page.events.iter().any(|event| {
                    matches!(
                        &event.kind,
                        backend_library::IndexJobProgressKind::ProfileRefused { .. }
                    )
                })
            });
        loop {
            let reply_ref = reply.as_ref();
            let mut view = backend_present::product_view_for_command(command, reply_ref)
                .with_index_profile_progress(profile_progress.clone())
                .map_err(|error| {
                    RpcError::from_fault(&Fault::from_client_error(
                        &error,
                        surface_error_operand(command),
                    ))
                })?;
            view.expose_mcp_advice(exposure);
            let view = view
                .with_cli_context(self.product.cli_context())
                .with_bounded_index_poll()
                .map_err(|error| {
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
            let mut index_poll = backend_present::IndexPollDto::for_detail(&view, detail);
            preserve_compact_receipt_in_poll(index_poll.as_mut(), command, compact_receipt);
            let payload = match encode_serializable(
                "surface",
                detail,
                None,
                SurfaceBody {
                    fault: view.fault().map(backend_present::FaultDto::new),
                    continuation_call: None,
                    index_poll,
                    surface: SurfaceProjection {
                        reply: reply_ref,
                        next_cursor,
                        index_job: view.index_job(),
                        compact_receipt,
                    },
                },
                DEFAULT_RESPONSE_BUDGET_BYTES,
            ) {
                Ok(payload) => payload,
                Err(error) => {
                    if fit_progress_window
                        && narrow_progress_window(reply.to_mut(), &mut profile_progress)
                    {
                        continue;
                    }
                    return Ok(refused(&oversized_fault(error)));
                }
            };
            let structured: Value = serde_json::from_slice(&payload.bytes).map_err(|error| {
                RpcError::tool(format!("typed surface projection decode failed: {error}"))
            })?;
            let result = tool_result(
                &bounded_text(&markdown::product_with_detail(&view, detail)),
                structured,
                view.fault().is_some(),
            );
            if !fit_progress_window {
                return Ok(result);
            }
            // Include the real full-envelope byte/token admission, with room for
            // ordinary caller IDs. The final encoder still checks the actual ID.
            let framed = bound_rpc_envelope(
                success(Value::String("0".repeat(256)), result.clone()),
                ResponseTransport::StdioLine,
            );
            if framed
                .value()
                .get("result")
                .and_then(|value| value.get("structuredContent"))
                != result.get("structuredContent")
                && narrow_progress_window(reply.to_mut(), &mut profile_progress)
            {
                continue;
            }
            return Ok(result);
        }
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
            // This cursor is already authenticated by the durable native owner
            // and binds exact roots/package/key/limit, including after restart.
            ContinuationCursor::SemanticVersions(cursor) => return Ok(cursor.as_str().to_owned()),
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
        answer: Answer,
        detail: Detail,
        context: &[u8],
        cursor_target: CursorTarget,
        invocation: McpInvocation<'_>,
    ) -> Result<Value, RpcError> {
        match self.rendered_admitted(answer, detail, context, cursor_target, invocation) {
            Err(error)
                if error.fault.as_ref().is_some_and(|fault| {
                    fault.cause().slug() == backend_present::CauseSlug::Oversized
                }) =>
            {
                Ok(refused(
                    error.fault.as_ref().expect("matched typed overflow"),
                ))
            }
            result => result,
        }
    }

    fn rendered_admitted(
        &mut self,
        answer: Answer,
        detail: Detail,
        context: &[u8],
        cursor_target: CursorTarget,
        invocation: McpInvocation<'_>,
    ) -> Result<Value, RpcError> {
        let next = answer
            .cursor()
            .map(|cursor| self.issue_cursor(cursor, context))
            .transpose()?;
        let answer = match answer {
            Answer::Product(product) => {
                let product = match next.as_deref() {
                    Some(token) => (*product).project_cursor(token, cursor_target),
                    None => *product,
                };
                let product = product
                    .with_cli_context(self.product.cli_context())
                    .with_bounded_index_poll()
                    .map_err(|error| {
                        RpcError::from_fault(&Fault::from_client_error(
                            &error,
                            backend_present::Operand::Whole,
                        ))
                    })?;
                Answer::Product(Box::new(product))
            }
            answer => answer,
        }
        .with_mcp_exposure(invocation.exposure());
        let native_graph_cursor = match &answer {
            Answer::Product(product) => {
                product
                    .package_graph_page()
                    .and_then(|page| match &page.terminal {
                        backend_library::PackageGraphPageTerminal::More(cursor) => Some(cursor),
                        _ => None,
                    })
            }
            _ => None,
        };
        let source_next = match &answer {
            Answer::Product(product) => match product.semantic_data() {
                Some(backend_present::ProductSemanticData::Source(chunk)) => chunk.next,
                _ => None,
            },
            _ => None,
        };
        let native_versions = matches!(&answer, Answer::Product(product)
            if matches!(product.semantic_data(), Some(backend_present::ProductSemanticData::VersionsPage(_))));
        let continuation_call = if let Some(offset) = source_next {
            Some(invocation.native_source_continuation_call(offset))
        } else if let Some(cursor) = native_graph_cursor {
            Some(invocation.native_graph_continuation_call(cursor))
        } else {
            next.as_deref()
                .filter(|_| {
                    native_versions
                        || matches!(&answer, Answer::Records(_))
                        || invocation.exposure() == McpExposure::RegisteredCall
                })
                .map(|cursor| invocation.continuation_call(cursor))
        };
        // Long portable proofs occur once in the executable next call. No
        // owner bytes or selectable rows are discarded to reduce duplication.
        let executable_only = matches!(&answer, Answer::Records(_))
            && next.as_ref().is_some_and(|cursor| cursor.len() > 1024);
        let target = match invocation.exposure() {
            McpExposure::Direct
                if native_graph_cursor.is_none()
                    && source_next.is_none()
                    && !native_versions
                    && !executable_only
                    && !invocation.adapted_arguments() =>
            {
                ContinuationTarget::McpTool(invocation.selected_name())
            }
            _ => ContinuationTarget::McpInvocation(invocation),
        };
        let payload = match encode_answer_with_call(
            &answer,
            detail,
            next.as_deref().filter(|_| !executable_only),
            DEFAULT_RESPONSE_BUDGET_BYTES,
            continuation_call.as_ref(),
        ) {
            Ok(payload) => payload,
            Err(error) => return Err(RpcError::from_fault(&oversized_fault(error))),
        };
        let structured: Value = serde_json::from_slice(&payload.bytes)
            .map_err(|error| RpcError::tool(format!("typed projection decode failed: {error}")))?;
        let preview = match &answer {
            Answer::Product(product)
                if matches!(
                    product.semantic_data(),
                    Some(backend_present::ProductSemanticData::VersionsPage(_))
                ) =>
            {
                let Some(backend_present::ProductSemanticData::VersionsPage(page)) =
                    product.semantic_data()
                else {
                    unreachable!()
                };
                semantic_pages::versions_preview(page, page.more, invocation)
            }
            _ => match detail {
                Detail::Summary => markdown::summary_with_cursor(&answer, next.as_deref(), target),
                Detail::Standard | Detail::Full => {
                    markdown::answer_with_detail(&answer, detail, next.as_deref(), target)
                }
            },
        };
        if matches!(&answer, Answer::Product(view) if view.package_graph_page().is_some() || matches!(view.semantic_data(), Some(backend_present::ProductSemanticData::VersionsPage(_) | backend_present::ProductSemanticData::Source(_))))
            && preview.len() > backend_present::MAX_PREVIEW_TEXT_BYTES
        {
            return Err(RpcError::from_fault(&oversized_fault(BudgetExceeded {
                bytes: preview.len(),
                budget: backend_present::MAX_PREVIEW_TEXT_BYTES,
            })));
        }
        let text = bounded_text(&preview);
        if matches!(&answer, Answer::Records(_))
            || matches!(&answer, Answer::Product(view) if view.package_graph_page().is_some() || matches!(view.semantic_data(), Some(backend_present::ProductSemanticData::VersionsPage(_) | backend_present::ProductSemanticData::Source(_))))
        {
            Ok(complete_tool_result(
                &text,
                structured,
                answer.fault().is_some(),
            ))
        } else {
            Ok(tool_result(&text, structured, answer.fault().is_some()))
        }
    }

    fn graph_page_result(
        &mut self,
        page: &GraphQueryPage,
        detail: Detail,
        context: &[u8],
        invocation: McpInvocation<'_>,
    ) -> Result<Value, RpcError> {
        let next = match page.terminal {
            PageTerminal::More(cursor) => Some(self.issue_continuation(cursor, context)?),
            PageTerminal::Complete | PageTerminal::Cancelled => None,
        };
        let continuation_call = next
            .as_deref()
            .map(|cursor| invocation.continuation_call(cursor));
        let executable_only = next.as_ref().is_some_and(|cursor| cursor.len() > 1024);
        let target = match invocation.exposure() {
            McpExposure::Direct if !invocation.adapted_arguments() && !executable_only => {
                ContinuationTarget::McpTool(QUERY_TOOL)
            }
            _ => ContinuationTarget::McpInvocation(invocation),
        };
        let payload = match encode_serializable(
            "query",
            detail,
            next.as_deref().filter(|_| !executable_only),
            GraphPageBody {
                page,
                continuation_call: continuation_call.as_ref(),
            },
            DEFAULT_RESPONSE_BUDGET_BYTES,
        ) {
            Ok(payload) => payload,
            Err(error) => return Err(RpcError::from_fault(&oversized_fault(error))),
        };
        let value: Value = serde_json::from_slice(&payload.bytes).map_err(|error| {
            RpcError::tool(format!("typed graph projection decode failed: {error}"))
        })?;
        Ok(complete_tool_result(
            &bounded_text(&graph_page_text_for_target(page, next.as_deref(), target)),
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
structuredContent.continuation_call to read more search, graph, query or semantic pages. Initial \
pages may select a smaller limit to fit the whole response; the next call preserves that limit \
and the exact native cursor. Long portable proofs occur once in this call. Copy \
that coordinate verbatim into backend.document for the signature and docs, backend.source for \
the body, backend.references for uses, and backend.graph for calls. Do not invent coordinates \
and do not guess from filenames when a tool can answer. A refusal names the operand and the next \
call; follow it. Read backend://workspace/current when a result looks thin. Read \
backend://tools/catalog for capability-dependent route schemas and pass their exact \
name and arguments to backend.call when the host exposes only listed tools. \
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
        return (encoded.len() == backend_library::CURSOR_QUERY_BYTES.saturating_mul(2)
            || encoded.len() == backend_library::CURSOR_WINDOW_QUERY_BYTES.saturating_mul(2))
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
    if let Some(encoded) = owner.strip_prefix("is1-") {
        return valid_compressed_index_search_owner(encoded);
    }
    if let Some(encoded) = owner.strip_prefix("pc5-") {
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
        return !decoded.is_empty()
            && decoded.len() <= backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES
            && URL_SAFE_NO_PAD.encode(decoded) == encoded;
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

/// Checks the bounded framing of the compressed index-search owner token.
/// The local-service decoder still performs typed cursor and snapshot checks
/// before the token can resume any work.
fn valid_compressed_index_search_owner(encoded: &str) -> bool {
    if encoded.is_empty() {
        return false;
    }
    let Ok(compressed) = URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    if compressed.len() > backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES
        || URL_SAFE_NO_PAD.encode(&compressed) != encoded
    {
        return false;
    }

    let mut decoder = flate2::Decompress::new(true);
    let mut expanded = Vec::new();
    let mut input_offset = 0;
    loop {
        let mut chunk = [0_u8; 8192];
        let input = &compressed[input_offset..];
        let input_before = decoder.total_in();
        let output_before = decoder.total_out();
        let Ok(status) = decoder.decompress(input, &mut chunk, flate2::FlushDecompress::Finish)
        else {
            return false;
        };
        let consumed = decoder.total_in().saturating_sub(input_before) as usize;
        let produced = decoder.total_out().saturating_sub(output_before) as usize;
        input_offset = input_offset.saturating_add(consumed);
        if expanded.len().saturating_add(produced)
            > backend_library::MAX_INDEX_SEARCH_CURSOR_BYTES
        {
            return false;
        }
        expanded.extend_from_slice(&chunk[..produced]);
        match status {
            flate2::Status::StreamEnd => {
                if input_offset != compressed.len() {
                    return false;
                }
                return serde_json::from_slice::<Value>(&expanded)
                    .is_ok_and(|value| value.is_object());
            }
            flate2::Status::Ok | flate2::Status::BufError => {
                if consumed == 0 && produced == 0 {
                    return false;
                }
            }
        }
    }
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
const PREVIEW_MARKER: &str = "\n\n… readable preview shortened to fit this response; structuredContent contains the complete page and any nextCursor; when present, copy structuredContent.continuation_call unchanged.";

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
const MCP_CL100K_TOKEN_LIMIT: usize = backend_present::DEFAULT_RESPONSE_TOKEN_BUDGET;

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
    value["result"]["content"][0]["text"] = Value::String(shortened_preview(prefix, 0));
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
        candidate["result"]["content"][0]["text"] = Value::String(shortened_preview(prefix, end));
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

/// Lossless window of an already admitted owner's ordered event page.
/// Every omitted event remains addressable through the original ticket and
/// `after_sequence`; no compiler cause or source/coverage fact is shortened.
fn narrow_progress_window(
    reply: &mut SurfaceReply,
    profile_progress: &mut Option<backend_library::IndexProgressPage>,
) -> bool {
    let page = match reply {
        SurfaceReply::IndexProgress(backend_library::IndexJobObservation::Pending(page)) => page,
        SurfaceReply::IndexOperationStatus(_) => {
            let Some(page) = profile_progress.as_mut() else {
                return false;
            };
            page
        }
        _ => return false,
    };
    if page.events.len() <= 1 {
        return false;
    }
    let keep = page.events.len().div_ceil(2);
    let events = page.events[..keep].to_vec().into_boxed_slice();
    let Some(last) = events.last() else {
        return false;
    };
    page.next_sequence = last.sequence;
    page.events = events;
    page.has_more = true;
    true
}

fn complete_tool_result(text: &str, structured: Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": is_error
    })
}

fn tool_result(text: &str, structured: Value, is_error: bool) -> Value {
    let mut candidate = complete_tool_result(text, structured, is_error);
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
    candidate["content"][0]["text"] = Value::String(shortened_preview(text, 0));
    if serialized_bytes(&candidate) <= budget {
        let mut lower = 0;
        let mut upper = text.len();
        let mut best_end = 0;
        while lower < upper {
            let midpoint = lower + (upper - lower).div_ceil(2);
            let end = readable_prefix_end(text, midpoint);
            let mut trial = candidate.clone();
            trial["content"][0]["text"] = Value::String(shortened_preview(text, end));
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

/// Shared record text retains coverage/count/continuation when rows are removed.
/// Non-record prose retains the original UTF-8/whole-coordinate prefix policy.
fn shortened_preview(text: &str, prefix_budget: usize) -> String {
    if let Some(preview) = markdown::shorten_record_preview(text, prefix_budget) {
        return format!("{preview}{PREVIEW_MARKER}");
    }
    let end = readable_prefix_end(text, prefix_budget);
    format!("{}{PREVIEW_MARKER}", &text[..end])
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

#[cfg(test)]
fn graph_page_text(page: &GraphQueryPage, cursor: Option<&str>) -> String {
    graph_page_text_for_target(page, cursor, ContinuationTarget::McpTool(QUERY_TOOL))
}

fn graph_page_text_for_target(
    page: &GraphQueryPage,
    cursor: Option<&str>,
    target: ContinuationTarget<'_>,
) -> String {
    let mut out = format!(
        "~query {} row(s) · {} · revision {}\n",
        page.rows.len(),
        terminal_name(page.terminal),
        abbreviate(page.revision.as_bytes())
    );
    match page.terminal {
        PageTerminal::More(_) => {
            if let Some(cursor) = cursor {
                out.push_str(&backend_present::continuation_note(target, cursor));
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
    // Reserve truthful page-preview counts before adding only whole rows.
    // The complete typed page still includes every field and coordinate.
    let count_reserve = format!("query preview: {} row(s), {} shown, {} omitted from text; structuredContent contains every row.\n", page.rows.len(), page.rows.len(), page.rows.len()).len();
    let row_budget =
        backend_present::MAX_PREVIEW_TEXT_BYTES.saturating_sub(out.len() + count_reserve);
    let mut rows_text = String::new();
    let mut shown = 0;
    for row in &page.rows {
        let fields = row
            .fields()
            .iter()
            .map(|(name, value)| format!("{name}={}", scalar_text(value)))
            .collect::<Vec<_>>();
        let text = format!("{}\n", fields.join("  "));
        if rows_text.len().saturating_add(text.len()) > row_budget {
            break;
        }
        rows_text.push_str(&text);
        shown += 1;
    }
    out.push_str(&rows_text);
    if shown < page.rows.len() {
        out.push_str(&format!("query preview: {} row(s), {shown} shown, {} omitted from text; structuredContent contains every row.\n", page.rows.len(), page.rows.len() - shown));
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
    #[cfg(test)]
    fn into_tool_result_for_call(
        self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, Self> {
        self.into_tool_result_for_invocation(McpInvocation::new(
            McpExposure::Direct,
            name,
            arguments,
        ))
    }

    fn into_tool_result_for_invocation(
        mut self,
        invocation: McpInvocation<'_>,
    ) -> Result<Value, Self> {
        self.fault = self
            .fault
            .map(|fault| fault.with_mcp_exposure(invocation.exposure()));
        if let Some(fault) = &self.fault
            && fault.startup_recovery()
                == Some(backend_client::StartupRecovery::RetryOriginalCommand)
        {
            let call = invocation.executable_call();
            let mut projected = backend_present::FaultDto::new(fault);
            projected.shell = None;
            projected.call = None;
            // Admit the complete borrowed invocation before allocating its
            // JSON value. Neither queries nor operand lists are cloned here.
            if let Ok(payload) = backend_present::encode_value(
                &RetryFaultBody {
                    answer: "fault",
                    fault: projected,
                    call: &call,
                },
                MCP_RESULT_BUDGET_BYTES,
            ) {
                let structured: Value =
                    serde_json::from_slice(&payload.bytes).map_err(|error| {
                        RpcError::tool(format!("typed retry projection decode failed: {error}"))
                    })?;
                let text = format!(
                    "Retry the exact tool call in structuredContent.call; this command was not submitted.\n{}",
                    markdown::fault(fault),
                );
                let result = tool_result(&bounded_text(&text), structured, true);
                if result
                    .get("structuredContent")
                    .and_then(|content| content.get("call"))
                    .is_some()
                {
                    return Ok(result);
                }
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
            } else if fault.cause().slug() == backend_present::CauseSlug::OwnerVersionMismatch {
                "NuDox owner version mismatch; restart the running owner before retrying"
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
                FaultSlug::Ambiguous
                | FaultSlug::NotFound
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

#[derive(Serialize)]
struct RetryFaultBody<'a> {
    answer: &'static str,
    #[serde(flatten)]
    fault: backend_present::FaultDto,
    call: &'a McpExecutableCall<'a>,
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
    continuation_call: Option<&'a McpExecutableCall<'a>>,
}

impl Serialize for GraphPageBody<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map =
            serializer.serialize_map(Some(4 + usize::from(self.continuation_call.is_some())))?;
        map.serialize_entry("revision", &encode_id(self.page.revision.as_bytes()))?;
        map.serialize_entry("rows", &GraphRows(&self.page.rows))?;
        map.serialize_entry("terminal", terminal_name(self.page.terminal))?;
        map.serialize_entry("emitted", &self.page.rows.len())?;
        if let Some(call) = self.continuation_call {
            map.serialize_entry("continuation_call", call)?;
        }
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
    continuation_call: Option<&'a backend_present::McpExecutableCall<'a>>,
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
    compact_receipt: bool,
}

fn preserve_compact_receipt_in_poll(
    index_poll: Option<&mut backend_present::IndexPollDto>,
    command: &SurfaceCommand,
    compact_receipt: bool,
) {
    if !compact_receipt || !matches!(command, SurfaceCommand::IndexProgress { .. }) {
        return;
    }
    let Some(arguments) = index_poll
        .and_then(|poll| poll.call.get_mut("arguments"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    arguments.insert("compact_receipt".to_owned(), Value::Bool(true));
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
            SurfaceReply::IndexOperationStatus(data)
                if matches!(self.index_job,
                Some(backend_present::IndexJobProjection::Progress(backend_library::IndexJobObservation::Pending(page))) if !page.events.is_empty()) =>
            {
                self.serialize_job(serializer, "index-operation-status", data)
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
        // On ticket progress, `data` and `index_job.value` are the same typed
        // observation. Durable operation status plus profile progress carries
        // two distinct receipts, so keep both raw even when compaction is opted in.
        let compact_receipt =
            self.compact_receipt && matches!(self.reply, SurfaceReply::IndexProgress(_));
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("result", result)?;
        if compact_receipt {
            map.serialize_entry(
                "data",
                &IndexJobReceiptReference {
                    pointer: "#/surface/index_job/value",
                },
            )?;
        } else {
            map.serialize_entry("data", data)?;
        }
        map.serialize_entry("index_job", index_job)?;
        map.end()
    }
}

#[derive(Serialize)]
struct IndexJobReceiptReference {
    #[serde(rename = "receipt_pointer")]
    pointer: &'static str,
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

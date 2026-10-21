//! A product connection that survives the daemon retiring it.
//!
//! The local service closes any connection that has sent nothing for its read
//! timeout (`crates/local-service/src/listener.rs`, `io_timeout`), which is
//! 30 seconds. This server, by contrast, opens one session at startup and
//! serves an agent from it for as long as the agent is attached — and an agent
//! routinely thinks for longer than 30 seconds between tool calls. Every call
//! after that gap therefore failed with `ConnectionReset`, and the only cure
//! was restarting the server.
//!
//! [`Reconnecting`] closes that gap. It remembers how to open a connection
//! rather than only holding one, and when a call fails because the connection
//! is gone it opens a fresh one. Whether it then *retries* the call depends on
//! what the call is:
//!
//! * a read is retried once — repeating it observes the same daemon twice and
//!   changes nothing;
//! * a mutation is never retried. A disconnect cannot prove whether the
//!   daemon had already admitted the command (see
//!   `ClientError::MutationOutcomeUnknown` and the write path in
//!   `crates/client`), and admitting a project twice is not the same as
//!   admitting it once. A replacement connection is kept when one opens, but
//!   the returned fault never recommends repeating the ambiguous mutation.
//!
//! One reconnect per call, never a loop: if the fresh connection fails the
//! same way, that is the daemon's answer and it is reported.
//!
//! The daemon also retires a connection that has served its frame budget
//! (256 frames by default). A read recovers from that through the retry
//! above, but a mutation written to such a connection would come back as an
//! unknown outcome even though the daemon never saw it. The wrapper therefore
//! asks the product how many frames its connection has left and opens the
//! replacement *before* writing, so retirement never reaches a mutation.

use super::{Engine, Probe, Product};
use backend_client::{ClientError, RequestContext};
use backend_library::{
    AdmittedGraphQueryInput, GraphQueryPage, HealthReport, PageContinuation, ReplyDto,
    SurfaceCommand, SurfaceReply, ViewStateRoot,
};

/// Something that can open a fresh product connection on demand.
///
/// The trait exists so the reconnect decision is testable without a daemon:
/// what [`Reconnecting`] needs from a connection is the ability to make
/// another one, not a socket.
pub(crate) trait Endpoint {
    /// The connected product this endpoint yields.
    type Product: Product;

    fn set_request_context(&mut self, _context: Option<RequestContext>) {}

    /// Immutable attachment metadata remains available without a lease or dial.
    fn cli_context(&self) -> Option<backend_present::StartupRecoveryContext> {
        None
    }

    /// Opens one fresh connection, transferring owner-admitted page state
    /// from the retired product when one exists.
    ///
    /// # Errors
    /// Returns the client fault when the endpoint cannot be reached.
    fn connect(&mut self, previous: Option<Self::Product>) -> Result<Self::Product, ClientError>;

    /// Retires a replacement lease that also disconnected during its one
    /// allowed retry. Stateful endpoints may move bounded admission state to
    /// their next connection attempt; stateless fixtures can let it drop.
    fn retire(&mut self, previous: Self::Product) {
        drop(previous);
    }
}

/// Frames of a connection's budget that must remain before a call is written.
///
/// One product call can exchange several frames (a pinned query reads the
/// revision, sends the query, and polls its preparation), so a connection is
/// replaced while a few frames are still left rather than at the last one.
const FRAME_HEADROOM: usize = 4;

/// Whether repeating a call after a reconnect is the same as making it once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Repeatable {
    /// A read: retrying observes the same daemon again.
    Yes,
    /// A mutation whose admission a disconnect cannot rule out.
    No,
}

/// A product connection that reopens itself when the daemon retires it.
pub(crate) struct Reconnecting<E: Endpoint> {
    endpoint: E,
    /// The current transport lease, if one is available.
    ///
    /// A dead lease is removed before opening its replacement. The retired
    /// product is passed to the endpoint so it can transfer bounded owner
    /// state such as continuation certificates; it is never used for another
    /// request.
    product: Option<E::Product>,
    request_context: Option<RequestContext>,
}

impl<E: Endpoint> Reconnecting<E> {
    /// Wraps an already-connected product together with the endpoint that
    /// produced it.
    #[cfg(test)]
    pub(crate) const fn new(endpoint: E, product: E::Product) -> Self {
        Self {
            endpoint,
            product: Some(product),
            request_context: None,
        }
    }

    /// Defers opening the first product connection until the first operation
    /// that needs the owner. This lets the MCP handshake and static tool
    /// discovery complete while the local daemon is unavailable; the first
    /// owner-backed request then receives the ordinary typed connection
    /// failure instead of the process exiting before it reads stdin.
    pub(crate) const fn disconnected(endpoint: E) -> Self {
        Self {
            endpoint,
            product: None,
            request_context: None,
        }
    }

    // Taking the dead lease transfers custody even if cancellation arrives
    // at this boundary. Its endpoint must retain any continuation authority.
    fn take_reconnect_lease(&mut self) -> Result<Option<E::Product>, ClientError> {
        let previous = self.product.take();
        if let Err(error) = self
            .request_context
            .as_ref()
            .map_or(Ok(()), RequestContext::check)
        {
            if let Some(previous) = previous {
                self.endpoint.retire(previous);
            }
            return Err(error);
        }
        Ok(previous)
    }

    /// Runs one call, reopening the connection if it is gone.
    ///
    /// `call` is invoked at most twice and only ever a second time when the
    /// first failure was a disconnect and the call is repeatable.
    fn attempt<T>(
        &mut self,
        repeatable: Repeatable,
        mut call: impl FnMut(&mut E::Product) -> Result<T, ClientError>,
    ) -> Result<T, ClientError> {
        if let Some(context) = &self.request_context {
            if repeatable == Repeatable::Yes {
                context.read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)?;
            }
            context.check()?;
        }
        // A failed reconnect leaves no lease behind. The next invocation is
        // therefore bounded to one fresh endpoint attempt rather than first
        // touching an already-dead product again.
        if self.product.is_none() {
            self.product = Some(self.endpoint.connect(None)?);
        } else if self
            .product
            .as_ref()
            .and_then(Product::connection_frames_remaining)
            .is_some_and(|remaining| remaining < FRAME_HEADROOM)
        {
            // The owner closes a connection once it has served its frame
            // budget, whatever the next frame is. Writing a mutation to one
            // that is about to be retired leaves its admission unknowable, so
            // the replacement is opened here, before anything is sent. Nothing
            // has been written yet: a failure to open it is a plain connect
            // failure, never an unknown outcome.
            let previous = self.take_reconnect_lease()?;
            self.product = Some(self.endpoint.connect(previous)?);
        }
        if let Some(product) = &mut self.product {
            product.set_request_context(self.request_context.clone())?;
        }
        if let Some(context) = &self.request_context {
            context.check()?;
        }
        let first = match self.product.as_mut() {
            Some(product) => call(product),
            None => Err(ClientError::Disconnected(std::io::ErrorKind::NotConnected)),
        };
        // Reads can still be cancelled while their transport is returning.
        // For a mutation, preserve the result of the invoked operation: a
        // confirmed reply is known, and a disconnect is classified as an
        // unknown outcome below. A later cancellation must not erase either.
        let first = match (&self.request_context, repeatable) {
            (Some(context), Repeatable::Yes) => context.check().and(first),
            _ => first,
        };
        match first {
            Err(
                error @ (ClientError::RequestCancelled(_) | ClientError::RemoteDeadlineExceeded),
            ) => {
                if let Some(previous) = self.product.take() {
                    self.endpoint.retire(previous);
                }
                Err(error)
            }
            Err(ClientError::ResponseFrameTooLarge { basis }) => {
                // Header-only overflow leaves unread bytes on this exact lease.
                // Preserve continuation authority, but retry only after the
                // caller re-imports its signed input on a fresh connection.
                if let Some(previous) = self.product.take() {
                    self.endpoint.retire(previous);
                }
                if repeatable == Repeatable::No {
                    Err(ClientError::MutationOutcomeUnknown(
                        std::io::ErrorKind::InvalidData,
                    ))
                } else {
                    Err(ClientError::ResponseTooLarge { basis })
                }
            }
            Err(error @ ClientError::QueryPreparationDisconnected { .. }) => {
                // Keep the original pinned read's failure. A later explicit
                // request can connect, but this call cannot acquire a new
                // revision and quietly resume on another selection.
                if let Some(previous) = self.product.take() {
                    self.endpoint.retire(previous);
                }
                Err(error)
            }
            Err(ClientError::Disconnected(kind)) => {
                // Drop the dead transport lease before opening a replacement
                // either way: the agent's next call must not meet the same
                // socket, even if opening the replacement fails.
                let previous = match self.take_reconnect_lease() {
                    Ok(previous) => previous,
                    // Once a non-repeatable operation was invoked and its
                    // transport disconnected, cancellation/deadline at the
                    // lease boundary cannot establish that it was unsent.
                    Err(_) if repeatable == Repeatable::No => {
                        return Err(ClientError::MutationOutcomeUnknown(kind));
                    }
                    Err(error) => return Err(error),
                };
                let replacement = match self.endpoint.connect(previous) {
                    Ok(replacement) => replacement,
                    // A failed replacement cannot prove the mutation was
                    // unsent. Report the same ambiguous outcome whether the
                    // next lease opened or not.
                    Err(_) if repeatable == Repeatable::No => {
                        return Err(ClientError::MutationOutcomeUnknown(kind));
                    }
                    Err(error) => return Err(error),
                };
                match repeatable {
                    Repeatable::Yes => {
                        let mut replacement = replacement;
                        if let Err(error) =
                            replacement.set_request_context(self.request_context.clone())
                        {
                            self.endpoint.retire(replacement);
                            return Err(error);
                        }
                        let result = match &self.request_context {
                            Some(context) => context.check().and_then(|()| call(&mut replacement)),
                            None => call(&mut replacement),
                        };
                        let result = match &self.request_context {
                            Some(context) => context.check().and(result),
                            None => result,
                        };
                        // The replacement can itself be retired while the
                        // first retried read is in flight. Do not retain a
                        // second dead lease: the following request should
                        // begin at the endpoint and make one bounded fresh
                        // connection attempt.
                        if matches!(
                            &result,
                            Err(ClientError::Disconnected(_)
                                | ClientError::RequestCancelled(_)
                                | ClientError::RemoteDeadlineExceeded
                                | ClientError::QueryPreparationDisconnected { .. }
                                | ClientError::ResponseFrameTooLarge { .. })
                        ) {
                            self.endpoint.retire(replacement);
                            self.product = None;
                        } else {
                            self.product = Some(replacement);
                        }
                        match result {
                            Err(ClientError::ResponseFrameTooLarge { basis }) => {
                                Err(ClientError::ResponseTooLarge { basis })
                            }
                            result => result,
                        }
                    }
                    Repeatable::No => {
                        self.product = Some(replacement);
                        Err(ClientError::MutationOutcomeUnknown(kind))
                    }
                }
            }
            other => other,
        }
    }
}

/// Returns whether repeating one probe is the same as making it once.
///
/// Reads are listed explicitly and everything else is a mutation, so a probe
/// added later is never retried by default.
const fn probe_is_repeatable(probe: &Probe<'_>) -> Repeatable {
    match probe {
        Probe::Packages
        | Probe::Declaration { .. }
        | Probe::Document(_)
        | Probe::Source(_)
        | Probe::Related(_)
        | Probe::Graph(_)
        | Probe::Search { .. }
        | Probe::Names { .. }
        | Probe::Outline(_)
        | Probe::OutlinePage { .. } => Repeatable::Yes,
        Probe::Index(_) | Probe::IndexWithExecutionIntent { .. } | Probe::Remove(_) => {
            Repeatable::No
        }
    }
}

/// Returns whether repeating one surface command is the same as making it
/// once.
///
/// Reads are listed explicitly. Every command that writes — a subscription, a
/// project edit, a generation selection, a tree change, a release page that
/// marks what it returned as seen — falls through to [`Repeatable::No`], and
/// so does any command added after this was written.
const fn surface_is_repeatable(command: &SurfaceCommand) -> Repeatable {
    match command {
        SurfaceCommand::Advisory { .. }
        | SurfaceCommand::Read { .. }
        | SurfaceCommand::References { .. }
        | SurfaceCommand::Diff { .. }
        | SurfaceCommand::Explore { .. }
        | SurfaceCommand::Package { .. }
        | SurfaceCommand::Dependents { .. }
        | SurfaceCommand::Dependencies { .. }
        | SurfaceCommand::Owner { .. }
        | SurfaceCommand::IndexSearch { .. }
        | SurfaceCommand::IndexOperationStatus { .. }
        | SurfaceCommand::IndexProgress { .. }
        | SurfaceCommand::PackageVersions { .. }
        | SurfaceCommand::SemanticVersions { .. }
        | SurfaceCommand::SemanticSource { .. }
        | SurfaceCommand::PackageProfile { .. }
        | SurfaceCommand::ForgeReference { .. }
        | SurfaceCommand::Subscriptions
        | SurfaceCommand::Projects
        | SurfaceCommand::Tree
        | SurfaceCommand::Releases { mark_seen: false } => Repeatable::Yes,
        _ => Repeatable::No,
    }
}

impl<E: Endpoint> Engine for Reconnecting<E> {
    fn members_page(
        &mut self,
        document: &backend_library::Document,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        // A replacement owner must re-admit this exact old document/source;
        // it cannot refresh the operand or silently restart a new selection.
        self.attempt(Repeatable::Yes, |product| {
            product.members_page(document, limit, continuation)
        })
    }

    fn index(
        &mut self,
        package: backend_library::PackageReference,
        execution_intent: backend_library::CompileExecutionIntent,
    ) -> Result<SurfaceReply, ClientError> {
        // The journal owns reconciliation on the next explicit invocation;
        // a disconnected mutation never submits an automatic replacement.
        self.attempt(Repeatable::No, |product| {
            product.index(package.clone(), execution_intent)
        })
    }
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.attempt(Repeatable::Yes, Engine::revision)
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        self.attempt(Repeatable::Yes, Engine::health)
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        if matches!(&probe, Probe::Search { .. })
            && let Some(context) = &self.request_context
        {
            context.begin_default_search(backend_client::CLIENT_REQUEST_TIMEOUT)?;
        }
        let repeatable = probe_is_repeatable(&probe);
        self.attempt(repeatable, |product| product.probe(probe))
    }

    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        if matches!(&probe, Probe::Search { .. })
            && let Some(context) = &self.request_context
        {
            context.begin_default_search(backend_client::CLIENT_REQUEST_TIMEOUT)?;
        }
        let repeatable = probe_is_repeatable(&probe);
        self.attempt(repeatable, |product| {
            product.probe_page(probe, continuation)
        })
    }

    fn probe_page_window(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
        window: backend_library::QueryWindow,
    ) -> Result<ReplyDto, ClientError> {
        if matches!(&probe, Probe::Search { .. })
            && let Some(context) = &self.request_context
        {
            context.begin_default_search(backend_client::CLIENT_REQUEST_TIMEOUT)?;
        }
        self.attempt(probe_is_repeatable(&probe), |product| {
            product.probe_page_window(probe, continuation, window)
        })
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let repeatable = surface_is_repeatable(&command);
        self.attempt(repeatable, |product| product.surface(command.clone()))
    }
}

impl<E: Endpoint> Product for Reconnecting<E> {
    fn supports_outline_owner_witness_route(&self) -> bool {
        self.product
            .as_ref()
            .is_some_and(|product| product.supports_outline_owner_witness_route())
    }

    fn cli_context(&self) -> Option<backend_present::StartupRecoveryContext> {
        self.endpoint.cli_context()
    }

    fn set_request_context(&mut self, context: Option<RequestContext>) -> Result<(), ClientError> {
        if context.is_none()
            && self
                .request_context
                .as_ref()
                .is_some_and(|context| context.check().is_err())
            && let Some(previous) = self.product.take()
        {
            self.endpoint.retire(previous);
        }
        self.request_context = context.clone();
        self.endpoint.set_request_context(context.clone());
        if let Some(product) = &mut self.product {
            product.set_request_context(context.clone())?;
        }
        context.as_ref().map_or(Ok(()), RequestContext::check)
    }

    fn encode_continuation(
        &mut self,
        continuation: PageContinuation,
    ) -> Result<String, ClientError> {
        if self.product.is_none() {
            self.product = Some(self.endpoint.connect(None)?);
        }
        match self.product.as_mut() {
            Some(product) => product.encode_continuation(continuation),
            None => Err(ClientError::Disconnected(std::io::ErrorKind::NotConnected)),
        }
    }

    fn decode_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        self.attempt(Repeatable::Yes, |product| {
            product.decode_continuation(token)
        })
    }

    fn outline_page_token(
        &mut self,
        coordinate: String,
        window: backend_library::QueryWindow,
        token: &str,
    ) -> Result<ReplyDto, ClientError> {
        self.attempt(Repeatable::Yes, |product| {
            if !product.supports_outline_owner_witness_route() {
                return Err(ClientError::Protocol(
                    "connected owner does not expose owner-rehydrated outline tokens".to_owned(),
                ));
            }
            product.outline_page_token(coordinate.clone(), window, token)
        })
    }

    fn graph_page(
        &mut self,
        coordinate: String,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.attempt(Repeatable::Yes, |product| {
            product.graph_page(coordinate.clone(), limit, continuation)
        })
    }

    fn graph_page_window(
        &mut self,
        coordinate: String,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.attempt(Repeatable::Yes, |product| {
            product.graph_page_window(coordinate.clone(), window, continuation)
        })
    }

    fn graph_page_declaration(
        &mut self,
        selector: backend_library::DeclarationSelector,
        window: backend_library::QueryWindow,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        self.attempt(Repeatable::Yes, |product| {
            product.graph_page_declaration(selector.clone(), window, continuation)
        })
    }

    fn graph_query(
        &mut self,
        input: AdmittedGraphQueryInput,
        limit: u16,
        continuation: Option<PageContinuation>,
    ) -> Result<GraphQueryPage, ClientError> {
        self.attempt(Repeatable::Yes, |product| {
            product.graph_query(input.clone(), limit, continuation)
        })
    }
}

/// Reconnect laws.
///
/// Before these, this server held one connection for its whole life and the
/// daemon closed it after 30 idle seconds, so every tool call after the first
/// failed with `ConnectionReset` until the server was restarted.
///
/// The assertions are on content and on what the fake actually received — the
/// reply a read returns, the fault a mutation returns, and the exact call log
/// across connections — because a wrapper that reconnected and then repeated a
/// mutation would keep every count of *successful* calls identical.
#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "a test that cannot see its own fixture has nothing to assert"
)]
mod tests {
    use super::*;
    use backend_library::{
        Basis, CommandReply, CompileExecutionIntent, Coverage, Freshness, Frontier,
        IndexOperationKey, IndexOperationObservation, Intent, Lane, PageTerminal, ProjectionPage,
        Reason, ViewRoot, ViewSnapshot, object_version, package_key, view_key, view_state_root,
    };
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    /// Every call any connection of one fixture endpoint received.
    type Log = Arc<Mutex<Vec<String>>>;

    /// One connection that is dead until the endpoint hands out a live one.
    struct Connection {
        log: Log,
        /// Whether this connection answers or reports itself gone.
        alive: bool,
    }

    impl Connection {
        fn record(&self, call: &str) -> Result<(), ClientError> {
            self.log.lock().expect("fixture log").push(format!(
                "{}:{call}",
                if self.alive { "live" } else { "dead" }
            ));
            if self.alive {
                Ok(())
            } else {
                Err(ClientError::Disconnected(
                    std::io::ErrorKind::ConnectionReset,
                ))
            }
        }
    }

    fn empty_snapshot() -> ViewSnapshot {
        let basis = Basis::new(view_state_root(&[]), object_version(b"source"));
        let root = ViewRoot::new_incomplete(
            view_key(b"reconnect-fixture"),
            basis,
            Frontier::new(
                basis.branch,
                basis.log,
                basis.schema,
                view_state_root(&[]),
                0,
            ),
            Vec::new(),
            vec![Coverage::Unavailable {
                lane: Lane::Semantic,
                reason: Reason::Unconfigured,
            }],
        )
        .expect("fixture view root");
        ViewSnapshot {
            root,
            freshness: Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        }
    }

    impl Engine for Connection {
        fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
            self.record("revision")?;
            Ok(view_state_root(&[]))
        }

        fn health(&mut self) -> Result<HealthReport, ClientError> {
            self.record("health")?;
            let library = backend_library::Library::new();
            Ok(HealthReport::from_root(library.view(), library.cursor()))
        }

        fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
            let reply = match probe {
                Probe::Packages => {
                    self.record("packages")?;
                    CommandReply::Packages(empty_snapshot())
                }
                Probe::Index(path) => {
                    self.record("index")?;
                    CommandReply::Added(Intent::request_package(package_key(path)).id())
                }
                Probe::IndexWithExecutionIntent { path, .. } => {
                    self.record("index")?;
                    CommandReply::Added(Intent::request_package(package_key(path)).id())
                }
                Probe::OutlinePage { .. } => {
                    self.record("outline-page")?;
                    CommandReply::ProjectionPage(ProjectionPage {
                        outline_order: Some(backend_library::OutlinePageOrder::new(
                            0, Vec::new(),
                        ).expect("empty reconnect outline order")),
                        snapshot: empty_snapshot(),
                        terminal: PageTerminal::Complete,
                    })
                }
                other => {
                    self.record("other")?;
                    return Err(ClientError::Protocol(format!(
                        "fixture has no reply for {other:?}"
                    )));
                }
            };
            Ok(ReplyDto::new(1, reply))
        }

        fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
            match command {
                SurfaceCommand::Subscriptions => {
                    self.record("subscriptions")?;
                    Ok(SurfaceReply::Subscriptions(Box::new([])))
                }
                SurfaceCommand::IndexOperationStatus { operation_key } => {
                    self.record("operation-status")?;
                    Ok(SurfaceReply::IndexOperationStatus(
                        IndexOperationObservation::Unknown { operation_key },
                    ))
                }
                SurfaceCommand::Subscribe { .. } => {
                    self.record("subscribe")?;
                    Ok(SurfaceReply::Subscriptions(Box::new([])))
                }
                other => {
                    self.record("other-surface")?;
                    Err(ClientError::Protocol(format!(
                        "fixture has no reply for {:?}",
                        other.id()
                    )))
                }
            }
        }
    }

    impl Product for Connection {
        fn supports_outline_owner_witness_route(&self) -> bool {
            true
        }

        fn encode_continuation(&mut self, _: PageContinuation) -> Result<String, ClientError> {
            Err(ClientError::Protocol(
                "fixture has no portable cursor".to_owned(),
            ))
        }

        fn decode_continuation(&mut self, _: &str) -> Result<PageContinuation, ClientError> {
            Err(ClientError::Protocol(
                "fixture has no portable cursor".to_owned(),
            ))
        }

        fn outline_page_token(
            &mut self,
            coordinate: String,
            _window: backend_library::QueryWindow,
            token: &str,
        ) -> Result<ReplyDto, ClientError> {
            self.record(&format!("outline-owner-token:{coordinate}:{token}"))?;
            Ok(ReplyDto::new(
                1,
                CommandReply::ProjectionPage(backend_library::ProjectionPage {
                    outline_order: Some(
                        backend_library::OutlinePageOrder::new(0, Vec::new())
                            .expect("empty reconnect outline order"),
                    ),
                    snapshot: empty_snapshot(),
                    terminal: PageTerminal::Complete,
                }),
            ))
        }

        fn graph_query(
            &mut self,
            _: AdmittedGraphQueryInput,
            _: u16,
            _: Option<PageContinuation>,
        ) -> Result<GraphQueryPage, ClientError> {
            self.record("graph-query")?;
            Ok(GraphQueryPage {
                revision: view_state_root(&[]).into(),
                source: object_version(b"source"),
                rows: Box::new([]),
                terminal: PageTerminal::Complete,
            })
        }
    }

    /// A deterministic stand-in for the daemon's idle read timeout. Advancing
    /// the shared clock models a long thinking gap without sleeping in a
    /// test, and the replacement connection starts its lease at the exact
    /// reconnect instant.
    struct TimedConnection {
        inner: Connection,
        clock: Arc<AtomicU64>,
        last_activity: u64,
        timeout: u64,
    }

    impl TimedConnection {
        fn record_idle(&mut self, call: &str) -> Result<(), ClientError> {
            let now = self.clock.load(Ordering::Relaxed);
            if now.saturating_sub(self.last_activity) > self.timeout {
                self.inner
                    .log
                    .lock()
                    .expect("fixture log")
                    .push(format!("expired:{call}"));
                return Err(ClientError::Disconnected(
                    std::io::ErrorKind::ConnectionReset,
                ));
            }
            self.last_activity = now;
            self.inner.record(call)
        }
    }

    impl Engine for TimedConnection {
        fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
            self.record_idle("revision")?;
            Ok(view_state_root(&[]))
        }

        fn health(&mut self) -> Result<HealthReport, ClientError> {
            self.record_idle("health")?;
            let library = backend_library::Library::new();
            Ok(HealthReport::from_root(library.view(), library.cursor()))
        }

        fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
            match probe {
                Probe::Packages => {
                    self.record_idle("packages")?;
                    Ok(ReplyDto::new(1, CommandReply::Packages(empty_snapshot())))
                }
                Probe::Index(path) => {
                    self.record_idle("index")?;
                    Ok(ReplyDto::new(
                        1,
                        CommandReply::Added(Intent::request_package(package_key(path)).id()),
                    ))
                }
                Probe::IndexWithExecutionIntent { path, .. } => {
                    self.record_idle("index")?;
                    Ok(ReplyDto::new(
                        1,
                        CommandReply::Added(Intent::request_package(package_key(path)).id()),
                    ))
                }
                other => {
                    self.record_idle("other")?;
                    Err(ClientError::Protocol(format!(
                        "timed fixture has no reply for {other:?}"
                    )))
                }
            }
        }

        fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
            match command {
                SurfaceCommand::Subscriptions => {
                    self.record_idle("subscriptions")?;
                    Ok(SurfaceReply::Subscriptions(Box::new([])))
                }
                SurfaceCommand::Subscribe { .. } => {
                    self.record_idle("subscribe")?;
                    Ok(SurfaceReply::Subscriptions(Box::new([])))
                }
                other => {
                    self.record_idle("other-surface")?;
                    Err(ClientError::Protocol(format!(
                        "timed fixture has no reply for {:?}",
                        other.id()
                    )))
                }
            }
        }
    }

    impl Product for TimedConnection {
        fn encode_continuation(&mut self, _: PageContinuation) -> Result<String, ClientError> {
            Err(ClientError::Protocol(
                "timed fixture has no portable cursor".to_owned(),
            ))
        }

        fn decode_continuation(&mut self, _: &str) -> Result<PageContinuation, ClientError> {
            Err(ClientError::Protocol(
                "timed fixture has no portable cursor".to_owned(),
            ))
        }

        fn graph_query(
            &mut self,
            _: AdmittedGraphQueryInput,
            _: u16,
            _: Option<PageContinuation>,
        ) -> Result<GraphQueryPage, ClientError> {
            self.record_idle("graph-query")?;
            Ok(GraphQueryPage {
                revision: view_state_root(&[]).into(),
                source: object_version(b"source"),
                rows: Box::new([]),
                terminal: PageTerminal::Complete,
            })
        }
    }

    struct TimedFixture {
        log: Log,
        clock: Arc<AtomicU64>,
        timeout: u64,
        connects: usize,
    }

    impl Endpoint for TimedFixture {
        type Product = TimedConnection;

        fn connect(
            &mut self,
            _previous: Option<Self::Product>,
        ) -> Result<Self::Product, ClientError> {
            self.connects = self.connects.checked_add(1).expect("connect count");
            Ok(TimedConnection {
                inner: Connection {
                    log: Arc::clone(&self.log),
                    alive: true,
                },
                clock: Arc::clone(&self.clock),
                last_activity: self.clock.load(Ordering::Relaxed),
                timeout: self.timeout,
            })
        }
    }

    /// An endpoint whose connections are all live: the first one this fixture
    /// is *given* is the dead one, standing in for the socket the daemon
    /// closed while the agent was thinking.
    struct Fixture {
        log: Log,
        connects: usize,
    }

    impl Endpoint for Fixture {
        type Product = Connection;

        fn cli_context(&self) -> Option<backend_present::StartupRecoveryContext> {
            Some(backend_present::StartupRecoveryContext::new(
                "/scope/project".to_owned(),
                "/scope/owner.sock".to_owned(),
                "/observed/bundle/backend-cli".to_owned(),
                backend_present::StartupRecoveryCommand::Status,
                backend_present::StartupRecoveryMode::Active,
                backend_present::StartupRecoveryWorkspace::Explicit("/scope/workspace".to_owned()),
            ))
        }

        fn connect(
            &mut self,
            _previous: Option<Self::Product>,
        ) -> Result<Self::Product, ClientError> {
            self.connects = self.connects.checked_add(1).expect("connect count");
            Ok(Connection {
                log: Arc::clone(&self.log),
                alive: true,
            })
        }
    }

    #[test]
    fn immutable_local_context_does_not_open_a_disconnected_owner() {
        let handle = Reconnecting::disconnected(Fixture {
            log: Arc::new(std::sync::Mutex::new(Vec::new())),
            connects: 0,
        });
        assert!(handle.cli_context().is_some());
        assert!(handle.product.is_none());
        assert_eq!(handle.endpoint.connects, 0);
    }

    #[test]
    fn reconnecting_forwards_pc5_only_to_a_negotiated_owner_product() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mut product = Reconnecting::disconnected(Fixture {
            log: Arc::clone(&log),
            connects: 0,
        });
        assert!(!product.supports_outline_owner_witness_route());

        let coordinate = "/scope/project";
        let token = "pc5-owner-token";
        let window = backend_library::QueryWindow::fixed(
            backend_library::QueryLimit::new(2).expect("bounded outline limit"),
        );
        let reply = product
            .outline_page_token(coordinate.to_owned(), window, token)
            .expect("PC5 read reaches negotiated owner product");
        assert!(matches!(reply.reply, CommandReply::ProjectionPage(_)));
        assert!(product.supports_outline_owner_witness_route());
        let calls = log.lock().expect("fixture log").clone();
        assert_eq!(
            calls,
            vec!["live:outline-owner-token:/scope/project:pc5-owner-token".to_owned()]
        );
        assert_eq!(product.endpoint.connects, 1);
    }

    /// Builds a handle whose current connection is already dead.
    fn idle_out() -> (Reconnecting<Fixture>, Log) {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let dead = Connection {
            log: Arc::clone(&log),
            alive: false,
        };
        let endpoint = Fixture {
            log: Arc::clone(&log),
            connects: 0,
        };
        (Reconnecting::new(endpoint, dead), log)
    }

    /// A slow owner answers a poll after the read window. The progress read
    /// changes nothing, so it is dropped and repeated once on a fresh
    /// connection; the connection endpoint reuses the existing listener (it
    /// only starts an owner when none accepts), and a mutation hit by the same
    /// timeout stays an unknown outcome.
    #[test]
    fn a_read_timeout_is_repeated_on_a_fresh_connection_but_a_mutation_is_not() {
        for kind in [std::io::ErrorKind::WouldBlock, std::io::ErrorKind::TimedOut] {
            let (mut product, _) = idle_out();
            let mut calls = 0;
            let result: Result<u8, ClientError> = product.attempt(Repeatable::Yes, |_| {
                calls += 1;
                if calls == 1 {
                    Err(ClientError::Disconnected(kind))
                } else {
                    Ok(7)
                }
            });
            assert_eq!(result, Ok(7), "{kind:?}");
            assert_eq!(calls, 2);
            assert_eq!(product.endpoint.connects, 1, "{kind:?}");

            let (mut product, _) = idle_out();
            let mut calls = 0;
            let result: Result<u8, ClientError> = product.attempt(Repeatable::No, |_| {
                calls += 1;
                Err(ClientError::Disconnected(kind))
            });
            assert_eq!(result, Err(ClientError::MutationOutcomeUnknown(kind)));
            assert_eq!(calls, 1, "a mutation is never repeated");
        }
    }

    #[test]
    fn pinned_preparation_disconnect_does_not_reconnect_or_rebind_the_same_call() {
        let (mut product, _) = idle_out();
        let expected = ClientError::QueryPreparationDisconnected {
            basis: view_state_root(&[]).into(),
            kind: std::io::ErrorKind::ConnectionReset,
        };
        let mut calls = 0;
        let result: Result<(), ClientError> = product.attempt(Repeatable::Yes, |_| {
            calls += 1;
            Err(expected.clone())
        });
        assert_eq!(result, Err(expected));
        assert_eq!(calls, 1);
        assert_eq!(product.endpoint.connects, 0);
        assert!(
            product.product.is_none(),
            "dead preparation lease is retired"
        );
        product.health().expect("a later explicit call may connect");
        assert_eq!(product.endpoint.connects, 1);
    }

    #[test]
    fn replacement_pinned_first_reply_failure_is_retired_without_a_third_attempt() {
        let (mut product, _) = idle_out();
        let expected = ClientError::QueryPreparationDisconnected {
            basis: view_state_root(&[]).into(),
            kind: std::io::ErrorKind::ConnectionReset,
        };
        let mut calls = 0;
        let result: Result<(), ClientError> = product.attempt(Repeatable::Yes, |_| {
            calls += 1;
            if calls == 1 {
                // No basis was chosen on this initial lease.
                Err(ClientError::Disconnected(std::io::ErrorKind::BrokenPipe))
            } else {
                // The replacement chose an exact basis, then lost its first reply.
                Err(expected.clone())
            }
        });
        assert_eq!(result, Err(expected));
        assert_eq!(calls, 2);
        assert_eq!(product.endpoint.connects, 1);
        assert!(
            product.product.is_none(),
            "replacement pinned failure is a dead lease"
        );
        product.health().expect("later explicit call may reconnect");
        assert_eq!(product.endpoint.connects, 2);
    }

    #[test]
    fn incomplete_oversized_frame_retires_without_replay_and_preserves_mutation_ambiguity() {
        for repeatable in [Repeatable::Yes, Repeatable::No] {
            let (mut product, _) = idle_out();
            let basis = Some(view_state_root(&[]).into());
            let mut calls = 0;
            let result: Result<(), ClientError> = product.attempt(repeatable, |_| {
                calls += 1;
                Err(ClientError::ResponseFrameTooLarge { basis })
            });
            assert_eq!(calls, 1, "the caller must re-import before retrying");
            assert!(product.product.is_none(), "poisoned lease retired");
            assert_eq!(product.endpoint.connects, 0);
            assert_eq!(
                result,
                Err(if repeatable == Repeatable::Yes {
                    ClientError::ResponseTooLarge { basis }
                } else {
                    ClientError::MutationOutcomeUnknown(std::io::ErrorKind::InvalidData)
                })
            );
        }
    }

    /// A reconnect must hand the retired lease to its endpoint. The real
    /// session endpoint uses that ownership transfer to move continuation
    /// certificates; this fixture proves the wrapper does not silently drop
    /// the state before the endpoint gets a chance to move it.
    struct TransferFixture {
        log: Log,
        saw_previous: bool,
    }

    impl Endpoint for TransferFixture {
        type Product = Connection;

        fn connect(
            &mut self,
            previous: Option<Self::Product>,
        ) -> Result<Self::Product, ClientError> {
            self.saw_previous = previous.is_some();
            Ok(Connection {
                log: Arc::clone(&self.log),
                alive: true,
            })
        }
    }

    struct RetireFixture {
        log: Log,
        connects: usize,
        saw_previous: bool,
        retired: bool,
    }

    impl Endpoint for RetireFixture {
        type Product = Connection;

        fn connect(
            &mut self,
            previous: Option<Self::Product>,
        ) -> Result<Self::Product, ClientError> {
            self.saw_previous |= previous.is_some();
            self.connects = self.connects.saturating_add(1);
            Ok(Connection {
                log: Arc::clone(&self.log),
                alive: self.connects > 1,
            })
        }

        fn retire(&mut self, previous: Self::Product) {
            self.retired = true;
            drop(previous);
        }
    }

    fn calls(log: &Log) -> Vec<String> {
        log.lock().expect("fixture log").clone()
    }

    /// A read after the daemon closed the idle connection succeeds on a fresh
    /// one.
    #[test]
    fn a_read_reconnects_and_returns_its_reply() {
        let (mut handle, log) = idle_out();

        let reply = handle
            .probe(Probe::Packages)
            .expect("a read must survive the daemon closing an idle connection");

        assert!(
            matches!(reply.reply, CommandReply::Packages(_)),
            "the retried read must return its own reply, got {:?}",
            reply.reply
        );
        assert_eq!(
            calls(&log),
            vec!["dead:packages".to_owned(), "live:packages".to_owned()],
            "the read must be tried once on the dead connection and once on \
             the fresh one"
        );
        assert_eq!(
            handle.endpoint.connects, 1,
            "exactly one reconnect, never a loop"
        );
    }

    #[test]
    fn reconnect_transfers_the_retired_product_to_the_endpoint() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let dead = Connection {
            log: Arc::clone(&log),
            alive: false,
        };
        let endpoint = TransferFixture {
            log: Arc::clone(&log),
            saw_previous: false,
        };
        let mut handle = Reconnecting::new(endpoint, dead);

        handle
            .probe(Probe::Packages)
            .expect("a read must recover on the replacement lease");
        assert!(handle.endpoint.saw_previous);
    }

    #[test]
    fn a_second_disconnect_parks_the_replacement_for_the_next_call() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let dead = Connection {
            log: Arc::clone(&log),
            alive: false,
        };
        let endpoint = RetireFixture {
            log: Arc::clone(&log),
            connects: 0,
            saw_previous: false,
            retired: false,
        };
        let mut handle = Reconnecting::new(endpoint, dead);

        let first = handle
            .probe(Probe::Packages)
            .expect_err("one bounded retry must still report its disconnect");
        assert!(matches!(first, ClientError::Disconnected(_)));
        assert!(handle.endpoint.saw_previous);
        assert!(handle.endpoint.retired);

        handle
            .probe(Probe::Packages)
            .expect("the following call must open a new lease");
        assert_eq!(handle.endpoint.connects, 2);
        assert_eq!(
            calls(&log),
            vec![
                "dead:packages".to_owned(),
                "dead:packages".to_owned(),
                "live:packages".to_owned(),
            ]
        );
    }

    #[test]
    fn deterministic_idle_gap_matrix_reconnects_only_after_the_timeout() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let clock = Arc::new(AtomicU64::new(0));
        let initial = TimedConnection {
            inner: Connection {
                log: Arc::clone(&log),
                alive: true,
            },
            clock: Arc::clone(&clock),
            last_activity: 0,
            timeout: 30,
        };
        let endpoint = TimedFixture {
            log: Arc::clone(&log),
            clock: Arc::clone(&clock),
            timeout: 30,
            connects: 0,
        };
        let mut handle = Reconnecting::new(endpoint, initial);

        handle
            .probe(Probe::Packages)
            .expect("the initial call at t=0 is live");
        clock.store(30, Ordering::Relaxed);
        handle
            .probe(Probe::Packages)
            .expect("the timeout boundary is still usable");
        clock.store(70, Ordering::Relaxed);
        handle
            .probe(Probe::Packages)
            .expect("a read after the long idle gap reconnects and retries");

        assert_eq!(
            calls(&log),
            vec![
                "live:packages".to_owned(),
                "live:packages".to_owned(),
                "expired:packages".to_owned(),
                "live:packages".to_owned(),
            ],
            "the 0/30/70 second matrix must show one retry on a fresh lease"
        );
        assert_eq!(
            handle.endpoint.connects, 1,
            "the long idle gap opens exactly one replacement session"
        );
    }

    /// A connection the owner retires once it has served its frame budget,
    /// exactly as the real listener does: the frame after the last one is not
    /// admitted and the stream is closed.
    struct Retiring {
        inner: Connection,
        remaining: usize,
        advertise: bool,
    }

    impl Retiring {
        fn frame(&mut self, call: &str) -> Result<(), ClientError> {
            if self.remaining == 0 {
                self.inner
                    .log
                    .lock()
                    .expect("fixture log")
                    .push(format!("retired:{call}"));
                return Err(ClientError::Disconnected(
                    std::io::ErrorKind::ConnectionReset,
                ));
            }
            self.remaining -= 1;
            self.inner.record(call)
        }
    }

    impl Engine for Retiring {
        fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
            self.frame("revision")?;
            Ok(view_state_root(&[]))
        }

        fn health(&mut self) -> Result<HealthReport, ClientError> {
            self.frame("health")?;
            let library = backend_library::Library::new();
            Ok(HealthReport::from_root(library.view(), library.cursor()))
        }

        fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
            match probe {
                Probe::Packages => {
                    self.frame("packages")?;
                    Ok(ReplyDto::new(1, CommandReply::Packages(empty_snapshot())))
                }
                Probe::Index(path) => {
                    self.frame("index")?;
                    Ok(ReplyDto::new(
                        1,
                        CommandReply::Added(Intent::request_package(package_key(path)).id()),
                    ))
                }
                other => Err(ClientError::Protocol(format!(
                    "retiring fixture has no reply for {other:?}"
                ))),
            }
        }

        fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
            Err(ClientError::Protocol(format!(
                "retiring fixture has no reply for {:?}",
                command.id()
            )))
        }
    }

    impl Product for Retiring {
        fn connection_frames_remaining(&self) -> Option<usize> {
            self.advertise.then_some(self.remaining)
        }

        fn encode_continuation(&mut self, _: PageContinuation) -> Result<String, ClientError> {
            Err(ClientError::Protocol("no portable cursor".to_owned()))
        }

        fn decode_continuation(&mut self, _: &str) -> Result<PageContinuation, ClientError> {
            Err(ClientError::Protocol("no portable cursor".to_owned()))
        }

        fn graph_query(
            &mut self,
            _: AdmittedGraphQueryInput,
            _: u16,
            _: Option<PageContinuation>,
        ) -> Result<GraphQueryPage, ClientError> {
            Err(ClientError::Protocol("no graph query".to_owned()))
        }
    }

    struct RetiringFixture {
        log: Log,
        budget: usize,
        advertise: bool,
        connects: usize,
    }

    impl RetiringFixture {
        fn connection(&self) -> Retiring {
            Retiring {
                inner: Connection {
                    log: Arc::clone(&self.log),
                    alive: true,
                },
                remaining: self.budget,
                advertise: self.advertise,
            }
        }
    }

    impl Endpoint for RetiringFixture {
        type Product = Retiring;

        fn connect(
            &mut self,
            _previous: Option<Self::Product>,
        ) -> Result<Self::Product, ClientError> {
            self.connects += 1;
            Ok(self.connection())
        }
    }

    /// W1: `index_start` as the first call on a connection the owner retired
    /// for its frame budget used to be written to a dead stream and reported
    /// as an unknown outcome although the owner never saw it. Knowing the
    /// budget, the wrapper opens the replacement before writing.
    #[test]
    fn a_mutation_is_never_written_to_a_connection_at_its_frame_budget() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let fixture = RetiringFixture {
            log: Arc::clone(&log),
            budget: 8,
            advertise: true,
            connects: 0,
        };
        let first = fixture.connection();
        let mut product = Reconnecting::new(fixture, first);
        // Eight reads exhaust the owner's budget for the first connection.
        for _ in 0..8 {
            product.probe(Probe::Packages).expect("read within budget");
        }
        product
            .probe(Probe::Index("/project"))
            .expect("the mutation goes to a fresh connection, not a retired one");
        let calls = calls(&log);
        assert!(
            calls.iter().all(|call| !call.starts_with("retired:")),
            "nothing was ever written to a retired connection: {calls:?}"
        );
        assert_eq!(
            calls.iter().filter(|call| call.ends_with(":index")).count(),
            1,
            "the mutation is sent exactly once: {calls:?}"
        );
        assert!(
            product.endpoint.connects >= 1,
            "a replacement was opened before the budget ran out"
        );
    }

    /// The hazard itself, without the budget knowledge: the same sequence
    /// reports an unknown outcome for a mutation the owner never admitted.
    #[test]
    fn without_the_budget_a_mutation_at_retirement_is_an_unknown_outcome() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let fixture = RetiringFixture {
            log: Arc::clone(&log),
            budget: 8,
            advertise: false,
            connects: 0,
        };
        let first = fixture.connection();
        let mut product = Reconnecting::new(fixture, first);
        for _ in 0..8 {
            product.probe(Probe::Packages).expect("read within budget");
        }
        assert_eq!(
            product.probe(Probe::Index("/project")).err(),
            Some(ClientError::MutationOutcomeUnknown(
                std::io::ErrorKind::ConnectionReset
            ))
        );
        assert!(calls(&log).contains(&"retired:index".to_owned()));
    }

    /// Many calls across many retirements: every read succeeds and every
    /// mutation is sent once, never replayed and never unknown.
    #[test]
    fn mixed_calls_survive_repeated_frame_budget_retirement() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let fixture = RetiringFixture {
            log: Arc::clone(&log),
            budget: 8,
            advertise: true,
            connects: 0,
        };
        let first = fixture.connection();
        let mut product = Reconnecting::new(fixture, first);
        let mut mutations = 0;
        for call in 0..100 {
            if call % 7 == 3 {
                product.probe(Probe::Index("/project")).expect("mutation");
                mutations += 1;
            } else {
                product.probe(Probe::Packages).expect("read");
            }
        }
        let calls = calls(&log);
        assert!(calls.iter().all(|call| !call.starts_with("retired:")));
        assert_eq!(
            calls.iter().filter(|call| call.ends_with(":index")).count(),
            mutations
        );
        assert!(product.endpoint.connects > 10, "many retirements happened");
    }

    #[test]
    fn daemon_restart_io_kinds_are_reconnectable_but_configuration_errors_are_not() {
        for kind in [
            std::io::ErrorKind::ConnectionReset,
            std::io::ErrorKind::ConnectionAborted,
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::BrokenPipe,
            std::io::ErrorKind::NotConnected,
            std::io::ErrorKind::UnexpectedEof,
            std::io::ErrorKind::TimedOut,
            std::io::ErrorKind::WouldBlock,
            std::io::ErrorKind::NotFound,
        ] {
            let mapped = super::super::map_runtime_connect_error(
                backend_runtime::RuntimeError::Io(std::io::Error::from(kind)),
            );
            assert!(
                matches!(mapped, ClientError::Disconnected(_)),
                "{kind:?} must reopen the endpoint: {mapped}"
            );
        }
        let mapped = super::super::map_runtime_connect_error(backend_runtime::RuntimeError::Io(
            std::io::Error::from(std::io::ErrorKind::InvalidData),
        ));
        assert!(
            matches!(mapped, ClientError::Startup(_)),
            "a configuration/data error must be surfaced, not retried: {mapped}"
        );
    }

    /// Every read-shaped call on the surface recovers, not only `probe`.
    #[test]
    fn revision_health_and_graph_query_all_recover() {
        for expected in [
            vec!["dead:revision".to_owned(), "live:revision".to_owned()],
            vec!["dead:health".to_owned(), "live:health".to_owned()],
            vec!["dead:graph-query".to_owned(), "live:graph-query".to_owned()],
            vec![
                "dead:subscriptions".to_owned(),
                "live:subscriptions".to_owned(),
            ],
            vec![
                "dead:operation-status".to_owned(),
                "live:operation-status".to_owned(),
            ],
            vec![
                "dead:outline-page".to_owned(),
                "live:outline-page".to_owned(),
            ],
        ] {
            let (mut handle, log) = idle_out();
            let call = expected.first().expect("expected call").clone();
            match call.as_str() {
                "dead:revision" => drop(handle.revision().expect("revision recovers")),
                "dead:health" => drop(handle.health().expect("health recovers")),
                "dead:graph-query" => drop(
                    handle
                        .graph_query(
                            AdmittedGraphQueryInput::new("fixture", Default::default())
                                .expect("fixture query is admitted"),
                            1,
                            None,
                        )
                        .expect("graph query recovers"),
                ),
                "dead:subscriptions" => drop(
                    handle
                        .surface(SurfaceCommand::Subscriptions)
                        .expect("a read surface command recovers"),
                ),
                "dead:operation-status" => drop(
                    handle
                        .surface(SurfaceCommand::IndexOperationStatus {
                            operation_key: IndexOperationKey::from_bytes([7; 32])
                                .expect("operation key"),
                        })
                        .expect("durable operation status is a repeatable read"),
                ),
                _ => {
                    let reply = handle.probe(Probe::OutlinePage {
                        path: "/abs/project", limit: 1,
                    }).expect("outline page recovers");
                    let CommandReply::ProjectionPage(page) = reply.reply else { panic!("outline page"); };
                    page.outline_order.as_ref().expect("positive empty reconnect order")
                        .validate(&page.snapshot, package_key("/abs/project"),
                            backend_library::QueryLimit::new(1).expect("limit"))
                        .expect("empty positive page has an exact witness");
                }
            }
            assert_eq!(calls(&log), expected, "{call} did not recover on a retry");
        }
    }

    /// A mutation is never repeated: the disconnect cannot prove the daemon
    /// did not already admit it.
    #[test]
    fn a_mutation_reconnects_without_being_sent_twice() {
        let (mut handle, log) = idle_out();

        let error = handle
            .probe(Probe::Index("/abs/project"))
            .expect_err("a mutation whose admission is unknown must not be retried");

        assert!(matches!(error, ClientError::MutationOutcomeUnknown(_)));
        assert_eq!(
            calls(&log),
            vec!["dead:index".to_owned()],
            "the mutation must reach the daemon at most once"
        );
        assert_eq!(
            handle.endpoint.connects, 1,
            "the connection must still be replaced so the next call works"
        );
        assert!(error.to_string().contains("outcome is unknown"));
        let fault = backend_present::Fault::from_client_error(
            &error,
            backend_present::Operand::Path("/abs/project".into()),
        );
        assert_eq!(fault.affordance(), &backend_present::Affordance::None);
        let result = super::super::RpcError::from_fault(&fault)
            .into_tool_result_for_call(
                "backend.index",
                serde_json::json!({"path":"/abs/project"})
                    .as_object()
                    .expect("arguments"),
            )
            .expect("ambiguous mutation is a typed tool result");
        assert!(result["structuredContent"].get("shell").is_none());
        assert!(result["structuredContent"].get("call").is_none());
        assert!(
            result["structuredContent"]["detail"]
                .as_str()
                .expect("fault detail")
                .contains("outcome is unknown")
        );
        assert!(
            !result["structuredContent"]["detail"]
                .as_str()
                .expect("fault detail")
                .contains("fresh connection was opened")
        );
    }

    /// A mutating surface command is treated the same way as a mutating probe.
    #[test]
    fn a_mutating_surface_command_is_not_repeated() {
        let (mut handle, log) = idle_out();

        let error = handle
            .surface(SurfaceCommand::Subscribe {
                package: backend_library::PackageReference::parse("serde")
                    .expect("package reference"),
                project: None,
            })
            .expect_err("a write surface command must not be retried");

        assert!(matches!(error, ClientError::MutationOutcomeUnknown(_)));
        assert_eq!(
            calls(&log),
            vec!["dead:subscribe".to_owned()],
            "a subscription must not be created twice"
        );
    }

    /// An explicit following call can use the replacement lease, without the
    /// reconnect layer replaying the ambiguous mutation.
    #[test]
    fn the_next_explicit_mutation_uses_the_replacement_lease() {
        let (mut handle, log) = idle_out();
        drop(
            handle
                .probe(Probe::Index("/abs/project"))
                .expect_err("refused"),
        );

        let reply = handle
            .probe(Probe::Index("/abs/project"))
            .expect("an explicit follow-up call uses the fresh connection");

        assert!(
            matches!(reply.reply, CommandReply::Added(_)),
            "the explicit follow-up mutation must be admitted, got {:?}",
            reply.reply
        );
        assert_eq!(
            calls(&log),
            vec!["dead:index".to_owned(), "live:index".to_owned()],
            "the first attempt failed on the dead connection and the agent's \
             own retry succeeded on the fresh one"
        );
        assert_eq!(
            handle.endpoint.connects, 1,
            "the second call must reuse the connection the first one opened"
        );
    }

    /// A fault that is not a disconnect never reconnects.
    #[test]
    fn a_product_fault_does_not_reconnect() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let live = Connection {
            log: Arc::clone(&log),
            alive: true,
        };
        let endpoint = Fixture {
            log: Arc::clone(&log),
            connects: 0,
        };
        let mut handle = Reconnecting::new(endpoint, live);

        let error = handle
            .probe(Probe::Search {
                text: "ferris",
                limit: 1,
            })
            .expect_err("the fixture refuses this probe");

        assert!(
            matches!(error, ClientError::Protocol(_)),
            "a protocol fault must be returned as itself, got {error}"
        );
        assert_eq!(
            handle.endpoint.connects, 0,
            "a live connection must not be thrown away over a product fault"
        );
        assert_eq!(calls(&log), vec!["live:other".to_owned()]);
    }

    #[test]
    fn every_read_only_surface_variant_is_repeatable() {
        let reads = [
            SurfaceCommand::Advisory {
                package: backend_library::PackageReference::parse("serde")
                    .expect("package reference"),
                override_evidence: None,
            },
            SurfaceCommand::References {
                target: backend_library::ProductText::new("pkg::callee").expect("target text"),
            },
            SurfaceCommand::Releases { mark_seen: false },
            SurfaceCommand::IndexProgress {
                ticket: backend_library::IndexJobTicket::new(
                    std::num::NonZeroU64::new(1).expect("nonzero ticket"),
                    [3; 16],
                    backend_library::PackageReference::parse("serde").expect("package reference"),
                ),
                after_sequence: 0,
            },
            SurfaceCommand::IndexOperationStatus {
                operation_key: IndexOperationKey::from_bytes([7; 32]).expect("operation key"),
            },
        ];
        for command in reads {
            assert_eq!(
                surface_is_repeatable(&command),
                Repeatable::Yes,
                "read-only surface command was classified as a mutation: {:?}",
                command.id()
            );
        }
        assert_eq!(
            surface_is_repeatable(&SurfaceCommand::Releases { mark_seen: true }),
            Repeatable::No
        );
        assert_eq!(
            surface_is_repeatable(&SurfaceCommand::Subscribe {
                package: backend_library::PackageReference::parse("serde")
                    .expect("package reference"),
                project: None,
            }),
            Repeatable::No
        );
        assert_eq!(
            surface_is_repeatable(&SurfaceCommand::IndexOperationStart {
                operation_key: IndexOperationKey::from_bytes([8; 32]).expect("operation key"),
                package: backend_library::PackageReference::parse("serde")
                    .expect("package reference"),
                execution_intent: CompileExecutionIntent::Interactive,
            }),
            Repeatable::No,
            "a start mutation must not become retryable with its status read"
        );
    }

    /// A reconnect failure must discard the old lease. Once the endpoint is
    /// available again, the next call starts with one fresh connection rather
    /// than sending the request back into the stream that already died.
    #[test]
    fn failed_reconnect_does_not_reuse_the_dead_lease() {
        struct Flaky {
            inner: Fixture,
            fail_connects: usize,
        }

        impl Endpoint for Flaky {
            type Product = Connection;

            fn connect(
                &mut self,
                _previous: Option<Self::Product>,
            ) -> Result<Self::Product, ClientError> {
                if self.fail_connects > 0 {
                    self.fail_connects -= 1;
                    return Err(ClientError::Io("daemon is restarting".to_owned()));
                }
                self.inner.connect(_previous)
            }
        }

        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let dead = Connection {
            log: Arc::clone(&log),
            alive: false,
        };
        let endpoint = Flaky {
            inner: Fixture {
                log: Arc::clone(&log),
                connects: 0,
            },
            fail_connects: 1,
        };
        let mut handle = Reconnecting::new(endpoint, dead);

        let first = handle
            .probe(Probe::Packages)
            .expect_err("the daemon is still restarting");
        assert!(matches!(first, ClientError::Io(_)));

        let reply = handle
            .probe(Probe::Packages)
            .expect("the next call must establish a new lease");
        assert!(matches!(reply.reply, CommandReply::Packages(_)));
        assert_eq!(
            calls(&log),
            vec!["dead:packages".to_owned(), "live:packages".to_owned(),]
        );
        assert_eq!(handle.endpoint.inner.connects, 1);
    }

    #[test]
    fn failed_startup_after_ambiguous_mutation_does_not_offer_unsent_retry() {
        struct Restarting(Fixture);
        impl Endpoint for Restarting {
            type Product = Connection;
            fn connect(
                &mut self,
                _previous: Option<Connection>,
            ) -> Result<Connection, ClientError> {
                self.0.connects += 1;
                Err(ClientError::from(
                    backend_runtime::RuntimeError::StartTimeout("/socket".into()),
                ))
            }
        }
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let dead = Connection {
            log: Arc::clone(&log),
            alive: false,
        };
        let mut handle = Reconnecting::new(
            Restarting(Fixture {
                log: Arc::clone(&log),
                connects: 0,
            }),
            dead,
        );
        let error = handle
            .probe(Probe::Index("/abs/project"))
            .expect_err("ambiguous mutation");
        assert!(matches!(error, ClientError::MutationOutcomeUnknown(_)));
        assert_eq!(calls(&log), vec!["dead:index".to_owned()]);
        assert_eq!(handle.endpoint.0.connects, 1);
        assert!(handle.product.is_none());
        let fault = backend_present::Fault::from_client_error(
            &error,
            backend_present::Operand::Path("/abs/project".into()),
        );
        assert!(fault.startup_recovery().is_none());
        assert_eq!(fault.affordance(), &backend_present::Affordance::None);
        let result = super::super::RpcError::from_fault(&fault)
            .into_tool_result_for_call(
                "backend.index",
                serde_json::json!({"path":"/abs/project"})
                    .as_object()
                    .expect("arguments"),
            )
            .expect("ordinary ambiguous disconnect refusal");
        assert!(result["structuredContent"].get("shell").is_none());
        assert!(result["structuredContent"].get("call").is_none());
        assert!(
            result["structuredContent"]["detail"]
                .as_str()
                .expect("typed ambiguity detail")
                .contains("outcome is unknown")
        );
        assert!(
            !result["structuredContent"]["detail"]
                .as_str()
                .expect("typed ambiguity detail")
                .contains("fresh connection was opened")
        );
        assert!(
            result["content"][0]["text"]
                .as_str()
                .expect("text block")
                .contains("outcome is unknown")
        );
        assert!(
            !result["content"][0]["text"]
                .as_str()
                .expect("text block")
                .contains("re-run the same command")
        );
        let next = handle
            .probe(Probe::Index("/abs/project"))
            .expect_err("fresh unsent startup");
        assert!(matches!(next, ClientError::Startup(_)));
        let next_fault = backend_present::Fault::from_client_error(
            &next,
            backend_present::Operand::Path("/abs/project".into()),
        );
        assert_eq!(
            next_fault.startup_recovery(),
            Some(backend_client::StartupRecovery::RetryOriginalCommand)
        );
        let next_result = super::super::RpcError::from_fault(&next_fault)
            .into_tool_result_for_call(
                "backend.index",
                serde_json::json!({"path":"/abs/project"})
                    .as_object()
                    .expect("arguments"),
            )
            .expect("the next explicit request failed before submission");
        assert_eq!(
            next_result["structuredContent"]["call"],
            serde_json::json!({
                "name": "backend.index",
                "arguments": {"path":"/abs/project"}
            })
        );
        assert_eq!(calls(&log), vec!["dead:index".to_owned()]);
        assert_eq!(handle.endpoint.0.connects, 2);
    }
    #[test]
    fn cancellation_preserves_read_and_mutation_dispatch_outcomes() {
        struct CancelConnection {
            inner: Connection,
            context: Option<RequestContext>,
            cancel: bool,
        }
        impl Engine for CancelConnection {
            fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
                self.inner.revision()
            }
            fn health(&mut self) -> Result<HealthReport, ClientError> {
                self.inner.health()
            }
            fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
                if self.cancel {
                    self.context
                        .as_ref()
                        .expect("request context")
                        .cancel(backend_client::RequestCancellationReason::Explicit);
                }
                self.inner.probe(probe)
            }
            fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
                self.inner.surface(command)
            }
        }
        impl Product for CancelConnection {
            fn set_request_context(
                &mut self,
                context: Option<RequestContext>,
            ) -> Result<(), ClientError> {
                self.context = context;
                self.context.as_ref().map_or(Ok(()), RequestContext::check)
            }
            fn encode_continuation(
                &mut self,
                continuation: PageContinuation,
            ) -> Result<String, ClientError> {
                self.inner.encode_continuation(continuation)
            }
            fn decode_continuation(
                &mut self,
                token: &str,
            ) -> Result<PageContinuation, ClientError> {
                self.inner.decode_continuation(token)
            }
            fn graph_query(
                &mut self,
                input: AdmittedGraphQueryInput,
                limit: u16,
                continuation: Option<PageContinuation>,
            ) -> Result<GraphQueryPage, ClientError> {
                self.inner.graph_query(input, limit, continuation)
            }
        }
        struct CancelEndpoint {
            log: Log,
            connects: usize,
        }
        impl Endpoint for CancelEndpoint {
            type Product = CancelConnection;
            fn connect(&mut self, _: Option<Self::Product>) -> Result<Self::Product, ClientError> {
                self.connects += 1;
                Ok(CancelConnection {
                    inner: Connection {
                        log: self.log.clone(),
                        alive: true,
                    },
                    context: None,
                    cancel: false,
                })
            }
        }
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut product = Reconnecting::new(
            CancelEndpoint {
                log: log.clone(),
                connects: 0,
            },
            CancelConnection {
                inner: Connection {
                    log: log.clone(),
                    alive: false,
                },
                context: None,
                cancel: true,
            },
        );
        product
            .set_request_context(Some(RequestContext::new()))
            .expect("request admission");
        assert!(matches!(
            product.probe(Probe::Packages),
            Err(ClientError::RequestCancelled(
                backend_client::RequestCancellationReason::Explicit
            ))
        ));
        assert_eq!(
            product.endpoint.connects, 0,
            "cancellation never opens replacement to replay"
        );
        assert_eq!(*log.lock().expect("owner log"), ["dead:packages"]);
        product
            .set_request_context(None)
            .expect("retire cancelled request");
        product
            .probe(Probe::Packages)
            .expect("next independent request may reconnect");
        assert_eq!(product.endpoint.connects, 1);
        assert_eq!(
            *log.lock().expect("owner log"),
            ["dead:packages", "live:packages"]
        );

        // A non-repeatable command that entered the dead transport has an
        // unknown outcome, even when that same exchange signals cancellation.
        let mutation_log = Arc::new(Mutex::new(Vec::new()));
        let mut mutation = Reconnecting::new(
            CancelEndpoint {
                log: mutation_log.clone(),
                connects: 0,
            },
            CancelConnection {
                inner: Connection {
                    log: mutation_log.clone(),
                    alive: false,
                },
                context: None,
                cancel: true,
            },
        );
        mutation
            .set_request_context(Some(RequestContext::new()))
            .expect("mutation request admission");
        assert!(matches!(
            mutation.probe(Probe::Index("/abs/project")),
            Err(ClientError::MutationOutcomeUnknown(
                std::io::ErrorKind::ConnectionReset
            ))
        ));
        assert_eq!(
            mutation.endpoint.connects, 0,
            "cancelled mutation does not redial"
        );
        assert_eq!(
            *mutation_log.lock().expect("mutation log"),
            ["dead:index"],
            "the scripted transport received exactly one mutation write"
        );

        // Cancellation before dispatch remains an ordinary cancellation and
        // leaves the scripted transport untouched.
        let undispatched_log = Arc::new(Mutex::new(Vec::new()));
        let mut undispatched = Reconnecting::new(
            CancelEndpoint {
                log: undispatched_log.clone(),
                connects: 0,
            },
            CancelConnection {
                inner: Connection {
                    log: undispatched_log.clone(),
                    alive: false,
                },
                context: None,
                cancel: false,
            },
        );
        let context = RequestContext::new();
        undispatched
            .set_request_context(Some(context.clone()))
            .expect("request admission before cancellation");
        context.cancel(backend_client::RequestCancellationReason::Explicit);
        assert!(matches!(
            undispatched.probe(Probe::Index("/abs/project")),
            Err(ClientError::RequestCancelled(
                backend_client::RequestCancellationReason::Explicit
            ))
        ));
        assert_eq!(undispatched.endpoint.connects, 0);
        assert!(undispatched_log.lock().expect("undispatched log").is_empty());

        // A reply already returned for an invoked mutation is known even if
        // cancellation is signalled during that call.
        let completed_log = Arc::new(Mutex::new(Vec::new()));
        let mut completed = Reconnecting::new(
            CancelEndpoint {
                log: completed_log.clone(),
                connects: 0,
            },
            CancelConnection {
                inner: Connection {
                    log: completed_log.clone(),
                    alive: true,
                },
                context: None,
                cancel: true,
            },
        );
        completed
            .set_request_context(Some(RequestContext::new()))
            .expect("completed mutation request admission");
        assert!(matches!(
            completed.probe(Probe::Index("/abs/project")),
            Ok(reply) if matches!(reply.reply, CommandReply::Added(_))
        ));
        assert_eq!(completed.endpoint.connects, 0);
        assert_eq!(
            *completed_log.lock().expect("completed mutation log"),
            ["live:index"]
        );
    }

    #[test]
    fn original_read_deadline_survives_preselection_replacement_dial() {
        struct DelayedEndpoint {
            log: Log,
            context: Option<RequestContext>,
            original: std::time::Instant,
            connects: usize,
        }
        impl Endpoint for DelayedEndpoint {
            type Product = Connection;
            fn set_request_context(&mut self, context: Option<RequestContext>) {
                self.context = context;
            }
            fn connect(&mut self, _: Option<Self::Product>) -> Result<Self::Product, ClientError> {
                self.connects += 1;
                let context = self.context.as_ref().expect("original request");
                assert_eq!(
                    context
                        .read_deadline(backend_client::CLIENT_REQUEST_TIMEOUT)
                        .expect("same deadline before dial"),
                    self.original,
                    "replacement cannot renew the read budget"
                );
                std::thread::sleep(
                    self.original
                        .saturating_duration_since(std::time::Instant::now())
                        + std::time::Duration::from_millis(1),
                );
                Ok(Connection {
                    log: self.log.clone(),
                    alive: true,
                })
            }
        }
        let context = RequestContext::new();
        let original = context
            .read_deadline(std::time::Duration::from_millis(80))
            .expect("original read lease");
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mut product = Reconnecting::new(
            DelayedEndpoint {
                log: log.clone(),
                context: None,
                original,
                connects: 0,
            },
            Connection {
                log: log.clone(),
                alive: false,
            },
        );
        product
            .set_request_context(Some(context))
            .expect("request admission");
        assert!(matches!(
            product.probe(Probe::Packages),
            Err(ClientError::RemoteDeadlineExceeded)
        ));
        assert_eq!(product.endpoint.connects, 1);
        assert_eq!(
            *log.lock().expect("calls"),
            ["dead:packages"],
            "expired replacement must not submit another owner read"
        );
    }

    #[test]
    fn mutation_cancellation_context_does_not_install_a_read_deadline() {
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let mut product = Reconnecting::new(
            Fixture {
                log: log.clone(),
                connects: 0,
            },
            Connection { log, alive: true },
        );
        let context = RequestContext::new();
        product
            .set_request_context(Some(context.clone()))
            .expect("request lifetime");
        product
            .probe(Probe::Index("/abs/project"))
            .expect("mutation");
        assert_eq!(
            context.remaining_read_budget().expect("active request"),
            None
        );
    }

    #[test]
    fn cancellation_at_pre_dial_transfer_retires_previous_before_returning() {
        for expired in [false, true] {
            let log: Log = Arc::new(Mutex::new(Vec::new()));
            let mut product = Reconnecting::new(
                RetireFixture {
                    log: log.clone(),
                    connects: 0,
                    saw_previous: false,
                    retired: false,
                },
                Connection { log, alive: false },
            );
            let context = RequestContext::new();
            let expected = if expired {
                context
                    .read_deadline(std::time::Duration::ZERO)
                    .expect_err("deterministically expired original read");
                ClientError::RemoteDeadlineExceeded
            } else {
                context.cancel(backend_client::RequestCancellationReason::Explicit);
                ClientError::RequestCancelled(backend_client::RequestCancellationReason::Explicit)
            };
            // Enter the exact take/check ownership boundary with cancellation
            // already signalled, rather than hoping a thread wins its race.
            product.request_context = Some(context);
            assert!(matches!(product.take_reconnect_lease(), Err(error) if error == expected));
            assert!(
                product.endpoint.retired,
                "endpoint receives the previous authority"
            );
            assert!(product.product.is_none());
            assert_eq!(product.endpoint.connects, 0, "cancelled caller cannot dial");
        }
    }

    #[test]
    fn cancellation_during_replacement_admission_retires_restored_authority() {
        struct CancelAfterConnect {
            log: Log,
            context: RequestContext,
            connects: usize,
            transferred: bool,
            retired: bool,
        }
        impl Endpoint for CancelAfterConnect {
            type Product = Connection;
            fn connect(&mut self, previous: Option<Connection>) -> Result<Connection, ClientError> {
                self.connects += 1;
                self.transferred = previous.is_some();
                self.context
                    .cancel(backend_client::RequestCancellationReason::Explicit);
                Ok(Connection {
                    log: self.log.clone(),
                    alive: true,
                })
            }
            fn retire(&mut self, _previous: Connection) {
                self.retired = true;
            }
        }
        let log: Log = Arc::new(Mutex::new(Vec::new()));
        let context = RequestContext::new();
        let mut product = Reconnecting::new(
            CancelAfterConnect {
                log: log.clone(),
                context: context.clone(),
                connects: 0,
                transferred: false,
                retired: false,
            },
            Connection {
                log: log.clone(),
                alive: false,
            },
        );
        product
            .set_request_context(Some(context))
            .expect("original request");
        assert!(matches!(
            product.probe(Probe::Packages),
            Err(ClientError::RequestCancelled(
                backend_client::RequestCancellationReason::Explicit
            ))
        ));
        assert!(
            product.endpoint.transferred,
            "old authority reached replacement admission"
        );
        assert!(
            product.endpoint.retired,
            "cancelled replacement returned its restored authority"
        );
        assert!(product.product.is_none());
        assert_eq!(product.endpoint.connects, 1);
        assert_eq!(*log.lock().expect("owner calls"), ["dead:packages"]);
    }
}

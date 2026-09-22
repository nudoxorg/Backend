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
//!   daemon had already admitted the command (see `ClientError::Disconnected`
//!   and the write path in `crates/client`), and admitting a project twice is
//!   not the same as admitting it once. The fresh connection is kept and the
//!   fault is returned, so the agent's next attempt succeeds on it.
//!
//! One reconnect per call, never a loop: if the fresh connection fails the
//! same way, that is the daemon's answer and it is reported.

use super::{Engine, Probe, Product};
use backend_client::ClientError;
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
}

impl<E: Endpoint> Reconnecting<E> {
    /// Wraps an already-connected product together with the endpoint that
    /// produced it.
    pub(crate) const fn new(endpoint: E, product: E::Product) -> Self {
        Self {
            endpoint,
            product: Some(product),
        }
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
        // A failed reconnect leaves no lease behind. The next invocation is
        // therefore bounded to one fresh endpoint attempt rather than first
        // touching an already-dead product again.
        if self.product.is_none() {
            self.product = Some(self.endpoint.connect(None)?);
        }
        let first = match self.product.as_mut() {
            Some(product) => call(product),
            None => Err(ClientError::Disconnected(std::io::ErrorKind::NotConnected)),
        };
        match first {
            Err(ClientError::Disconnected(kind)) => {
                // Drop the dead transport lease before opening a replacement
                // either way: the agent's next call must not meet the same
                // socket, even if opening the replacement fails.
                let previous = self.product.take();
                let mut replacement = self.endpoint.connect(previous)?;
                match repeatable {
                    Repeatable::Yes => {
                        let result = call(&mut replacement);
                        // The replacement can itself be retired while the
                        // first retried read is in flight. Do not retain a
                        // second dead lease: the following request should
                        // begin at the endpoint and make one bounded fresh
                        // connection attempt.
                        if matches!(&result, Err(ClientError::Disconnected(_))) {
                            self.endpoint.retire(replacement);
                            self.product = None;
                        } else {
                            self.product = Some(replacement);
                        }
                        result
                    }
                    Repeatable::No => {
                        self.product = Some(replacement);
                        Err(ClientError::Disconnected(kind))
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
        | Probe::Document(_)
        | Probe::Source(_)
        | Probe::Related(_)
        | Probe::Graph(_)
        | Probe::Search { .. }
        | Probe::Names { .. }
        | Probe::Outline(_)
        | Probe::OutlinePage { .. } => Repeatable::Yes,
        Probe::Index(_) | Probe::Remove(_) => Repeatable::No,
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
        | SurfaceCommand::PackageVersions { .. }
        | SurfaceCommand::SemanticVersions { .. }
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
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        self.attempt(Repeatable::Yes, Engine::revision)
    }

    fn health(&mut self) -> Result<HealthReport, ClientError> {
        self.attempt(Repeatable::Yes, Engine::health)
    }

    fn probe(&mut self, probe: Probe<'_>) -> Result<ReplyDto, ClientError> {
        let repeatable = probe_is_repeatable(&probe);
        self.attempt(repeatable, |product| product.probe(probe))
    }

    fn probe_page(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
    ) -> Result<ReplyDto, ClientError> {
        let repeatable = probe_is_repeatable(&probe);
        self.attempt(repeatable, |product| {
            product.probe_page(probe, continuation)
        })
    }

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let repeatable = surface_is_repeatable(&command);
        self.attempt(repeatable, |product| product.surface(command.clone()))
    }
}

impl<E: Endpoint> Product for Reconnecting<E> {
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
        Basis, CommandReply, Coverage, Freshness, Frontier, Intent, Lane, PageTerminal,
        ProjectionPage, Reason, ViewRoot, ViewSnapshot, object_version, package_key, view_key,
        view_state_root,
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
                Probe::OutlinePage { .. } => {
                    self.record("outline-page")?;
                    CommandReply::ProjectionPage(ProjectionPage {
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
            matches!(mapped, ClientError::Io(_)),
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
                _ => drop(
                    handle
                        .probe(Probe::OutlinePage {
                            path: "/abs/project",
                            limit: 1,
                        })
                        .expect("outline page recovers"),
                ),
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

        assert!(
            matches!(error, ClientError::Disconnected(_)),
            "the fault must say the connection went away, got {error}"
        );
        assert_eq!(
            calls(&log),
            vec!["dead:index".to_owned()],
            "the mutation must reach the daemon at most once"
        );
        assert_eq!(
            handle.endpoint.connects, 1,
            "the connection must still be replaced so the next call works"
        );
        assert!(
            error.to_string().contains("disconnected"),
            "the fault must read as a disconnect, got {error}"
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

        assert!(matches!(error, ClientError::Disconnected(_)));
        assert_eq!(
            calls(&log),
            vec!["dead:subscribe".to_owned()],
            "a subscription must not be created twice"
        );
    }

    /// The call after a refused mutation succeeds, because the connection was
    /// replaced even though the call was not retried.
    #[test]
    fn the_call_after_a_refused_mutation_succeeds() {
        let (mut handle, log) = idle_out();
        drop(
            handle
                .probe(Probe::Index("/abs/project"))
                .expect_err("refused"),
        );

        let reply = handle
            .probe(Probe::Index("/abs/project"))
            .expect("the agent's retry must land on the fresh connection");

        assert!(
            matches!(reply.reply, CommandReply::Added(_)),
            "the retried mutation must be admitted, got {:?}",
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
}

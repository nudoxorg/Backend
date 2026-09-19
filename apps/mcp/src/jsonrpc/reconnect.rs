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
    GraphQueryPage, GraphValue, HealthReport, ReplyDto, SurfaceCommand, SurfaceReply, ViewStateRoot,
};
use std::collections::BTreeMap;

/// Something that can open a fresh product connection on demand.
///
/// The trait exists so the reconnect decision is testable without a daemon:
/// what [`Reconnecting`] needs from a connection is the ability to make
/// another one, not a socket.
pub(crate) trait Endpoint {
    /// The connected product this endpoint yields.
    type Product: Product;

    /// Opens one fresh connection.
    ///
    /// # Errors
    /// Returns the client fault when the endpoint cannot be reached.
    fn connect(&mut self) -> Result<Self::Product, ClientError>;
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
    product: E::Product,
}

impl<E: Endpoint> Reconnecting<E> {
    /// Wraps an already-connected product together with the endpoint that
    /// produced it.
    pub(crate) const fn new(endpoint: E, product: E::Product) -> Self {
        Self { endpoint, product }
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
        match call(&mut self.product) {
            Err(ClientError::Disconnected(kind)) => {
                // Replace the connection either way: the agent's next call
                // must not meet the same dead socket.
                self.product = self.endpoint.connect()?;
                match repeatable {
                    Repeatable::Yes => call(&mut self.product),
                    Repeatable::No => Err(ClientError::Disconnected(kind)),
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
        SurfaceCommand::Read { .. }
        | SurfaceCommand::Diff { .. }
        | SurfaceCommand::Explore { .. }
        | SurfaceCommand::Package { .. }
        | SurfaceCommand::Dependents { .. }
        | SurfaceCommand::Owner { .. }
        | SurfaceCommand::IndexSearch { .. }
        | SurfaceCommand::PackageVersions { .. }
        | SurfaceCommand::SemanticVersions { .. }
        | SurfaceCommand::PackageProfile { .. }
        | SurfaceCommand::Subscriptions
        | SurfaceCommand::Projects
        | SurfaceCommand::Tree => Repeatable::Yes,
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

    fn surface(&mut self, command: SurfaceCommand) -> Result<SurfaceReply, ClientError> {
        let repeatable = surface_is_repeatable(&command);
        self.attempt(repeatable, |product| product.surface(command.clone()))
    }
}

impl<E: Endpoint> Product for Reconnecting<E> {
    fn graph_query(
        &mut self,
        query: String,
        variables: BTreeMap<String, GraphValue>,
        limit: u16,
    ) -> Result<GraphQueryPage, ClientError> {
        self.attempt(Repeatable::Yes, |product| {
            product.graph_query(query.clone(), variables.clone(), limit)
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
            self.log
                .lock()
                .expect("fixture log")
                .push(format!("{}:{call}", if self.alive { "live" } else { "dead" }));
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
            Frontier::new(basis.branch, basis.log, basis.schema, view_state_root(&[]), 0),
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
        fn graph_query(
            &mut self,
            _: String,
            _: BTreeMap<String, GraphValue>,
            _: u16,
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

    /// An endpoint whose connections are all live: the first one this fixture
    /// is *given* is the dead one, standing in for the socket the daemon
    /// closed while the agent was thinking.
    struct Fixture {
        log: Log,
        connects: usize,
    }

    impl Endpoint for Fixture {
        type Product = Connection;

        fn connect(&mut self) -> Result<Self::Product, ClientError> {
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
            vec!["dead:outline-page".to_owned(), "live:outline-page".to_owned()],
        ] {
            let (mut handle, log) = idle_out();
            let call = expected.first().expect("expected call").clone();
            match call.as_str() {
                "dead:revision" => drop(handle.revision().expect("revision recovers")),
                "dead:health" => drop(handle.health().expect("health recovers")),
                "dead:graph-query" => drop(
                    handle
                        .graph_query(String::new(), BTreeMap::new(), 1)
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
        drop(handle.probe(Probe::Index("/abs/project")).expect_err("refused"));

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
}

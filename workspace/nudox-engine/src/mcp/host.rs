//! Starting and stopping the endpoint from a *synchronous* caller.
//!
//! # Why this module exists at all
//!
//! [`McpEndpoint::start`] and [`McpEndpoint::stop`] are `async`, and the only
//! process that is supposed to call them — `lindsey` — cannot await anything.
//! LD-2 forbids blocking on the GPUI foreground thread and LR-9 says the engine
//! owns every runtime, so `lindsey` links no `tokio` at all: it has no
//! executor, no `block_on`, and no `spawn`.
//!
//! Left there, "started by lindsey after the engine, stopped on window close"
//! (GUI-LOCAL-PLAN §L6) is unimplementable, which is exactly why it stayed
//! unimplemented — see docs/LIMITATIONS.md **L35**, where a fully working, fully
//! tested server had zero callers and the status bar's `set_mcp_endpoint` had
//! zero call sites repo-wide.
//!
//! [`McpHost`] closes that gap by borrowing the engine's runtime
//! ([`EngineHandle::runtime_handle`]) and driving the two async calls on it.
//! The host holds its own [`EngineHandle`] clone, so the runtime provably
//! outlives the server: there is no ordering for a caller to get wrong.
//!
//! # Shutdown is a property of the type, not of a call site
//!
//! [`McpHost::stop`] drains; [`Drop`] cancels. Both paths stop the listener, so
//! forgetting to call `stop` costs a clean drain of in-flight requests and
//! nothing else. That asymmetry is deliberate — a lifecycle that only works
//! when someone remembers a call is the same class of defect as the dead setter
//! this module exists to revive.
//!
//! # Restart stability
//!
//! [`McpHost::start`] is also where `crate::mcp::identity::EndpointIdentity`
//! gets loaded and saved (`tests/mcp/endpoint_stability.rs`). It is the right
//! layer for that: [`McpEndpoint`] only knows how to bind a
//! [`crate::mcp::endpoint::PortPreference`] and start a caller-supplied
//! token, and has no opinion about where either comes from. [`McpHost::start`]
//! is the one caller `lindsey` actually uses in production, so it is the one
//! place "remember what happened last launch" belongs — [`McpHost::start_with_token`]
//! stays deliberately ignorant of it, because a test or caller that hands in
//! its own token wants that exact token, not one a state file might override.

use std::net::SocketAddr;
use std::time::Duration;

use crate::EngineHandle;

use crate::mcp::account::AccountGate;
use crate::mcp::endpoint::{McpEndpoint, PortPreference};
use crate::mcp::error::McpError;
use crate::mcp::identity::EndpointIdentity;
use crate::mcp::server::NudoxMcpServer;
use crate::mcp::session::SessionToken;

/// How long [`McpHost::stop`] waits for in-flight requests to drain.
///
/// Bounded because this runs on the GPUI main thread during app quit: an
/// agent holding an SSE stream open must not be able to stall the window's
/// close by refusing to disconnect. Cancellation has already been signalled
/// before this timer starts, so the wait is for *drain*, not for the decision.
pub const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// What happened when a server was asked to stop.
///
/// Returned rather than logged: "we stopped serving but did not wait" and "we
/// stopped serving and every request completed" are different facts about the
/// process, and a `tracing::warn!` puts that difference somewhere nothing can
/// branch on it (doctrine §8 — a warn does not stop the next caller).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ShutdownOutcome {
    /// The listener closed and every in-flight request finished.
    Drained,
    /// Shutdown was signalled, but the drain was not awaited to completion —
    /// either it exceeded [`DRAIN_TIMEOUT`], or `stop` was called from inside
    /// an async context where blocking would deadlock a worker thread.
    ///
    /// The socket is closed either way; what is unknown is whether a request
    /// that was already running got to finish.
    Signalled,
    /// This host had already been stopped.
    AlreadyStopped,
}

/// A running MCP server, owned by a synchronous host process.
///
/// Construct with [`start`](Self::start) after the engine, keep it alive for as
/// long as the window is open, and drop it (or call [`stop`](Self::stop)) to
/// shut it down.
pub struct McpHost {
    /// Held, not borrowed: the endpoint's serving task runs on this engine's
    /// runtime, so the engine must not be droppable before the host is. Making
    /// that an ownership fact rather than a documented ordering is the point —
    /// dropping the engine first would leave `stop` blocking on a runtime that
    /// no longer exists.
    engine: EngineHandle,
    /// `None` once [`stop`](Self::stop) has run.
    endpoint: Option<McpEndpoint>,
}

impl McpHost {
    /// Bind the loopback listener and begin serving, returning once the port is
    /// known.
    ///
    /// The bind is synchronous *and complete* before this returns: there is no
    /// window in which a caller holds an [`McpHost`] whose address is not yet
    /// accepting connections, so a status bar fed from
    /// [`url`](Self::url) can never advertise a port that is not listening.
    ///
    /// `gate` is the account gate every tool call is admitted through. It is a
    /// parameter rather than something this function constructs because the
    /// same gate is also what `lindsey`'s account view renders and what its
    /// sign-in flow drives — one gate, several readers, exactly as one
    /// `EngineHandle` has several.
    ///
    /// # Restart stability
    ///
    /// Loads whatever [`EndpointIdentity`] a previous launch left in
    /// `crate::mcp::account::state_dir` and, if there is one, asks
    /// [`McpEndpoint::start_with_preference`] to reuse its port and starts
    /// with its token rather than a fresh [`SessionToken::generate`] — that is
    /// the whole fix `tests/mcp/endpoint_stability.rs` is written against: the
    /// `mcpServers` snippet a user pasted last time keeps working. Once the
    /// bind succeeds — whether it got the remembered port or fell back to an
    /// ephemeral one — the port actually bound and the token actually used are
    /// saved back, so the *next* launch has something to remember even on a
    /// fresh install. A state directory that cannot be resolved or written to
    /// is not fatal: this degrades to exactly today's behaviour, a fresh port
    /// and token every launch.
    pub fn start(engine: &EngineHandle, gate: AccountGate) -> Result<Self, McpError> {
        let state_dir = crate::mcp::account::state_dir();
        let remembered = state_dir.as_deref().and_then(EndpointIdentity::load);

        let (token, preference) = remembered.as_ref().map_or_else(
            || (SessionToken::generate(), PortPreference::Any),
            |identity| (
                SessionToken::from_secret(identity.token.clone()),
                PortPreference::Remembered(identity.port),
            ),
        );
        // Captured before `token` moves into `start_inner`: it is the secret
        // this launch is actually using regardless of what the bind below
        // does with the port, so there is no need to read it back out of the
        // endpoint afterwards.
        let secret = token.expose().to_owned();

        let host = Self::start_inner(engine, gate, token, preference)?;

        if let (Some(dir), Some(addr)) = (state_dir.as_deref(), host.addr()) {
            let identity = EndpointIdentity {
                port: addr.port(),
                token: secret,
            };
            if let Err(error) = identity.save(dir) {
                // Not fatal: the server is already up and serving. The only
                // cost of a failed save is that the *next* launch gets a
                // fresh port and token instead of this one's — the same
                // outcome every launch had before this feature existed — so
                // this is worth knowing about, not worth failing over.
                tracing::warn!(
                    %error,
                    dir = %dir.display(),
                    "could not persist the mcp endpoint identity; the next launch will get a \
                     fresh port and token",
                );
            }
        }

        Ok(host)
    }

    /// Bind with a caller-supplied token, on an ephemeral port.
    ///
    /// Used by tests, which need the secret before the server exists — the
    /// same reason [`McpEndpoint::start_with_token`] exists. Deliberately does
    /// not consult or write [`EndpointIdentity`]: a caller that supplies its
    /// own token wants exactly that token in use, not one silently swapped
    /// for whatever a state file remembers.
    pub fn start_with_token(
        engine: &EngineHandle,
        gate: AccountGate,
        token: SessionToken,
    ) -> Result<Self, McpError> {
        Self::start_inner(engine, gate, token, PortPreference::Any)
    }

    /// Shared bind path for [`start`](Self::start) and
    /// [`start_with_token`](Self::start_with_token) — the only difference
    /// between them is where `token` and `preference` come from.
    fn start_inner(
        engine: &EngineHandle,
        gate: AccountGate,
        token: SessionToken,
        preference: PortPreference,
    ) -> Result<Self, McpError> {
        let server = NudoxMcpServer::new(engine.clone(), gate);
        // `block_on` puts us inside the engine's runtime for the duration, so
        // the `tokio::spawn` inside `McpEndpoint::start` has a reactor to
        // attach the listener to. That is why this borrows the engine's
        // runtime instead of constructing one: a second runtime would leave the
        // accept loop on a different reactor from every engine call the tools
        // make (LR-9).
        let endpoint = engine
            .runtime_handle()
            .block_on(McpEndpoint::start_with_preference(
                server, token, preference,
            ))?;
        Ok(Self {
            engine: engine.clone(),
            endpoint: Some(endpoint),
        })
    }

    /// The bound address, including the kernel-assigned port.
    ///
    /// `None` after [`stop`](Self::stop): an address that is no longer
    /// listening is not an address, and returning the stale one is how a
    /// status bar ends up advertising a closed socket.
    pub fn addr(&self) -> Option<SocketAddr> {
        self.endpoint.as_ref().map(McpEndpoint::addr)
    }

    /// The endpoint URL a client should be pointed at, or `None` once stopped.
    ///
    /// This is the string the status bar displays, and
    /// `workspace/gui/tests/mcp_endpoint.rs` connects to *this value* — not to
    /// `addr()` — so the displayed text and the reachable socket cannot drift.
    pub fn url(&self) -> Option<String> {
        self.endpoint.as_ref().map(McpEndpoint::url)
    }

    /// A ready-to-paste MCP client configuration, or `None` once stopped.
    pub fn client_config_snippet(&self) -> Option<String> {
        self.endpoint
            .as_ref()
            .map(McpEndpoint::client_config_snippet)
    }

    /// Stop serving and wait up to [`DRAIN_TIMEOUT`] for in-flight requests.
    ///
    /// Idempotent: the second call reports
    /// [`ShutdownOutcome::AlreadyStopped`] rather than doing anything.
    pub fn stop(&mut self) -> ShutdownOutcome {
        let Some(endpoint) = self.endpoint.take() else {
            return ShutdownOutcome::AlreadyStopped;
        };

        // Signal first, unconditionally. Whatever happens below, the listener
        // is closing — the only open question is whether we wait for it.
        endpoint.cancel();

        if tokio::runtime::Handle::try_current().is_ok() {
            // Blocking a runtime worker on a task scheduled to that same
            // runtime is a deadlock, not a slow path. `lindsey` never reaches
            // here (it has no async context at all); a test that calls `stop`
            // from inside `block_on` would, and would otherwise hang forever.
            return ShutdownOutcome::Signalled;
        }

        let drained = self.engine.runtime_handle().block_on(async move {
            tokio::time::timeout(DRAIN_TIMEOUT, endpoint.stop())
                .await
                .is_ok()
        });

        if drained {
            ShutdownOutcome::Drained
        } else {
            ShutdownOutcome::Signalled
        }
    }
}

impl Drop for McpHost {
    /// Close the listener even when nobody called [`stop`](McpHost::stop).
    ///
    /// Cancel-only, never blocking: `Drop` can run during process teardown,
    /// on any thread, and a `block_on` there would turn a missed `stop` into a
    /// hang. The drain is the thing `stop` buys you; the shutdown is not
    /// optional.
    fn drop(&mut self) {
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.cancel();
        }
    }
}

impl std::fmt::Debug for McpHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpHost")
            .field("addr", &self.addr())
            .finish_non_exhaustive()
    }
}

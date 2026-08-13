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
//! unimplemented — see LIMITATIONS.md **L35**, where a fully working, fully
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

use std::net::SocketAddr;
use std::time::Duration;

use nudox_engine::EngineHandle;

use crate::account::AccountGate;
use crate::endpoint::McpEndpoint;
use crate::error::McpError;
use crate::server::NudoxMcpServer;
use crate::session::SessionToken;

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
    pub fn start(engine: &EngineHandle, gate: AccountGate) -> Result<Self, McpError> {
        Self::start_with_token(engine, gate, SessionToken::generate())
    }

    /// Bind with a caller-supplied token.
    ///
    /// Used by tests, which need the secret before the server exists — the same
    /// reason [`McpEndpoint::start_with_token`] exists.
    pub fn start_with_token(
        engine: &EngineHandle,
        gate: AccountGate,
        token: SessionToken,
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
            .block_on(McpEndpoint::start_with_token(server, token))?;
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

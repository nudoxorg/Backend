//! The streamable-HTTP transport: loopback-only, ephemeral port, token-gated.
//!
//! # Why streamable HTTP and not stdio
//!
//! stdio is impossible here (§L6): `lindsey` is a GUI process and does not own
//! its stdin/stdout — a second consumer of those handles would corrupt both.
//! Streamable HTTP is the transport MCP defines for exactly this case.
//!
//! # Why an ephemeral port, and why it can now be a *remembered* one
//!
//! The listener defaults to `127.0.0.1:0`, so the kernel picks a free port. A
//! fixed port would mean two `lindsey` windows on one machine collide on
//! start-up, with the loser either failing or — worse — silently attaching an
//! agent to the other window's corpus. The bound [`SocketAddr`] is available
//! from [`McpEndpoint::addr`] the moment [`McpEndpoint::start`] returns, so the
//! status bar and Settings → Connection can display it (GUI-PLAN §21).
//!
//! That collision hazard is real, but it is not a reason for the port to
//! change on *every* launch — only on a genuine collision, which is
//! detectable: the second binder finds the port taken. [`PortPreference`] and
//! [`preferred_bind`] carry that distinction: a caller may ask this module to
//! try a specific port again (typically the one a previous launch used and
//! recorded — see `crate::mcp::identity::EndpointIdentity`), and
//! [`McpEndpoint::start_with_preference`] falls back to the kernel-assigned
//! behaviour only when that specific bind fails. The single-window case, which
//! is nearly every case, gets a stable port; the two-window case keeps the
//! guarantee it always had.
//!
//! # Local-only by construction
//!
//! [`LOOPBACK_BIND`] is a constant, and [`preferred_bind`] is a pure function
//! from [`PortPreference`] to a [`SocketAddr`] — no I/O, no environment. Every
//! branch of that function hard-codes [`Ipv4Addr::LOCALHOST`]; only the *port*
//! varies with the preference. So introducing a state file that can influence
//! the port does not reopen "local only is a property of a default a config
//! file could override" — the config file this module reads (indirectly, via
//! [`McpEndpoint::start_with_preference`]'s caller) can only ever supply a
//! *port number*, never an address, and `tests/mcp/endpoint_stability.rs`
//! pins that no reachable [`PortPreference`] can change that. rmcp's own
//! `allowed_hosts` DNS-rebinding guard is left at its loopback default on top
//! of that.
//!
//! # Why the token layer sits outside rmcp
//!
//! rmcp's `Mcp-Session-Id` is a *transport* session — it correlates a client's
//! SSE stream with its request stream. It is issued by the server on demand and
//! is not a secret, so it authenticates nothing. The §L6 launch token is a
//! separate concern and is enforced in a layer *in front of*
//! `StreamableHttpService`, which means an unauthenticated request is rejected
//! before rmcp allocates a session for it.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::mcp::error::McpError;
use crate::mcp::server::NudoxMcpServer;
use crate::mcp::session::{SessionToken, Unauthenticated};

/// Loopback, kernel-assigned port — [`preferred_bind`] with [`PortPreference::Any`].
///
/// Kept as a named constant because it is what every caller wanted before
/// [`PortPreference`] existed, and is still what [`McpEndpoint::start`] uses.
/// Not configurable in the sense that matters (§L6): every value
/// [`preferred_bind`] can produce is loopback, so nothing that reaches this
/// module can make the server bind off-box — only the port varies, never the
/// address.
pub const LOOPBACK_BIND: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));

/// The HTTP path the MCP endpoint is mounted at.
pub const MCP_PATH: &str = "/mcp";

/// What port [`McpEndpoint::start_with_preference`] should try to bind.
///
/// A caller-facing choice, not a caller-facing address: this says *what to
/// try*, and [`preferred_bind`] is the only thing that turns it into a
/// [`SocketAddr`], so there is exactly one place that decision can go wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortPreference {
    /// No memory of a previous launch — first run, or a discarded/corrupt
    /// state file (`crate::mcp::identity::EndpointIdentity::load` returning
    /// `None`). The kernel picks a free port, as it always has.
    Any,
    /// A previous launch's port, worth trying again so a client config a user
    /// pasted once keeps working. Not a guarantee: [`McpEndpoint::start_with_preference`]
    /// falls back to [`PortPreference::Any`]'s behaviour if this port turns
    /// out to be taken.
    Remembered(u16),
}

/// Turn a [`PortPreference`] into the loopback address to bind.
///
/// Pure: no I/O, no environment access, so the whole decision table —
/// including "a remembered port of zero is not a preference" — is a plain
/// unit test rather than something that needs a real socket, following
/// `app::corpus::select`'s split between a pure decision and the I/O shell
/// around it. Every branch binds [`Ipv4Addr::LOCALHOST`]; only the port
/// changes. That is what keeps the "Local-only by construction" claim above
/// true now that a state file feeds this function a port.
pub const fn preferred_bind(preference: PortPreference) -> SocketAddr {
    let port = match preference {
        // Zero is what an empty or corrupt state file decodes to, not a real
        // preference. Binding it *would* happen to work (port 0 is ephemeral
        // either way), but treating that as intentional would hide a corrupt
        // state file behind a coincidence — so it is normalised to `Any`
        // explicitly instead.
        PortPreference::Any | PortPreference::Remembered(0) => 0,
        PortPreference::Remembered(port) => port,
    };
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

/// A running MCP server.
///
/// Holds everything Settings → Connection needs to render a working client
/// config, plus the handle that stops the server on window close.
pub struct McpEndpoint {
    /// The address the listener actually bound.
    addr: SocketAddr,
    /// The per-launch token clients must present.
    token: SessionToken,
    /// Signals the axum server and every live rmcp session to stop.
    ///
    /// This is the very token `StreamableHttpServerConfig` already owns, kept
    /// rather than created, so cancelling it stops the HTTP loop and rmcp's
    /// sessions together instead of leaving one running behind the other.
    shutdown: CancellationToken,
    /// The serving task, so `stop` can wait for a clean drain.
    task: tokio::task::JoinHandle<()>,
}

impl McpEndpoint {
    /// Bind loopback on an ephemeral port and start serving.
    ///
    /// Returns once the listener is bound and its address is known, so the
    /// caller can display the port immediately; serving continues on the
    /// engine's runtime.
    pub async fn start(server: NudoxMcpServer) -> Result<Self, McpError> {
        Self::start_with_token(server, SessionToken::generate()).await
    }

    /// Bind and start with a caller-supplied token, on an ephemeral port.
    ///
    /// Used by tests, which need to know the secret before the server exists.
    /// Equivalent to [`start_with_preference`](Self::start_with_preference)
    /// with [`PortPreference::Any`] — port stability is a decision for a
    /// caller that knows whether a previous launch is worth remembering
    /// (`McpHost::start`), not something every test and doc example should
    /// have to thread through by hand.
    pub async fn start_with_token(
        server: NudoxMcpServer,
        token: SessionToken,
    ) -> Result<Self, McpError> {
        Self::start_with_preference(server, token, PortPreference::Any).await
    }

    /// Bind and start with a caller-supplied token and port preference.
    ///
    /// `preference` names what to *try*, not what is guaranteed: see
    /// [`PortPreference::Remembered`]. Which address was actually requested
    /// and, if it had to fall back, why, are always logged — a silent
    /// fallback would make "why did my pasted config stop working" the kind
    /// of question nobody can answer from the running process.
    pub async fn start_with_preference(
        server: NudoxMcpServer,
        token: SessionToken,
        preference: PortPreference,
    ) -> Result<Self, McpError> {
        let (listener, addr) = Self::bind_preferring(preference).await?;

        let config = StreamableHttpServerConfig::default();
        let shutdown = config.cancellation_token.clone();

        let mcp: StreamableHttpService<NudoxMcpServer, LocalSessionManager> =
            StreamableHttpService::new(
                move || Ok(server.clone()),
                std::sync::Arc::new(LocalSessionManager::default()),
                config,
            );

        let app =
            Router::new()
                .nest_service(MCP_PATH, mcp)
                .layer(axum::middleware::from_fn_with_state(
                    token.clone(),
                    require_session_token,
                ));

        let serve_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async move { serve_shutdown.cancelled().await })
                .await;
            if let Err(e) = result {
                tracing::error!("mcp http server terminated: {e}");
            }
        });

        tracing::info!(%addr, "mcp server listening on loopback");
        Ok(Self {
            addr,
            token,
            shutdown,
            task,
        })
    }

    /// Bind loopback, preferring `preference`'s port and falling back to
    /// [`PortPreference::Any`]'s kernel-assigned behaviour on failure.
    ///
    /// Only a genuinely *remembered* port (a non-zero one — see
    /// [`preferred_bind`]'s normalisation of `Remembered(0)`) is worth
    /// retrying: if the kernel was already free to pick, a second bind would
    /// just ask it the same question again and could only fail the same way.
    /// The fallback is logged rather than silent, because a config that
    /// worked yesterday and does not today is otherwise undebuggable from the
    /// running process.
    async fn bind_preferring(
        preference: PortPreference,
    ) -> Result<(TcpListener, SocketAddr), McpError> {
        let requested = preferred_bind(preference);
        let (listener, bound_at) = match TcpListener::bind(requested).await {
            Ok(listener) => (listener, requested),
            Err(source) if requested.port() != 0 => {
                let fallback = preferred_bind(PortPreference::Any);
                tracing::info!(
                    remembered = %requested,
                    error = %source,
                    "remembered mcp port is unavailable; falling back to an ephemeral port",
                );
                let listener =
                    TcpListener::bind(fallback)
                        .await
                        .map_err(|source| McpError::Bind {
                            addr: fallback,
                            source,
                        })?;
                (listener, fallback)
            }
            Err(source) => {
                return Err(McpError::Bind {
                    addr: requested,
                    source,
                });
            }
        };

        let addr = listener.local_addr().map_err(|source| McpError::Bind {
            addr: bound_at,
            source,
        })?;
        debug_assert!(
            addr.ip().is_loopback(),
            "preferred_bind must always be loopback"
        );
        Ok((listener, addr))
    }

    /// The bound address, including the kernel-assigned port.
    ///
    /// Shown in the status bar and in Settings → Connection (§L6).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The full endpoint URL a client should be pointed at.
    pub fn url(&self) -> String {
        format!("http://{}{MCP_PATH}", self.addr)
    }

    /// The per-launch token clients must present.
    pub fn token(&self) -> &SessionToken {
        &self.token
    }

    /// A ready-to-paste MCP client configuration (§L6, GUI-PLAN §21).
    ///
    /// Serialised from [`ClientConfig`] rather than assembled by string
    /// formatting, so the snippet cannot be malformed JSON and cannot drift out
    /// of step with the URL and header the server actually accepts — both are
    /// read from the live endpoint.
    pub fn client_config_snippet(&self) -> String {
        let config = ClientConfig::for_endpoint(self);
        // Infallible in practice: the value is a tree of owned `String`s with
        // no non-string map keys and no floats. `unwrap` is banned outside
        // tests (§L7.6), so the impossible branch degrades instead of panicking.
        serde_json::to_string_pretty(&config)
            .unwrap_or_else(|_| String::from("{\"error\":\"failed to render client config\"}"))
    }

    /// Signal shutdown without waiting for the drain.
    ///
    /// Split out of [`stop`](Self::stop) because the two callers want different
    /// halves of it: a host that is quitting wants the drain, and a `Drop` impl
    /// wants only the guarantee that the socket closes — blocking in `Drop`
    /// turns a forgotten `stop` into a hang. Cancelling twice is harmless
    /// (`CancellationToken::cancel` is idempotent), so `stop` still calls this
    /// path rather than duplicating it.
    pub fn cancel(&self) {
        self.shutdown.cancel();
    }

    /// Stop the server and wait for in-flight requests to drain.
    ///
    /// Called on window close (§L6 lifecycle).
    pub async fn stop(self) {
        self.shutdown.cancel();
        if let Err(e) = self.task.await {
            tracing::warn!("mcp server task did not shut down cleanly: {e}");
        }
    }
}

impl std::fmt::Debug for McpEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpEndpoint")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Client config
// ---------------------------------------------------------------------------

/// The MCP client configuration for a running endpoint.
///
/// Modelled as a type rather than a format string so the snippet Settings shows
/// is generated the same way the server's own contract is described (LR-2). The
/// shape is the `mcpServers` object every MCP client accepts.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientConfig {
    /// One entry per server; this endpoint registers itself as `nudox`.
    #[serde(rename = "mcpServers")]
    pub mcp_servers: std::collections::BTreeMap<String, ClientServerEntry>,
}

/// One server entry inside a [`ClientConfig`].
#[derive(Debug, Clone, serde::Serialize)]
pub struct ClientServerEntry {
    /// Always `"http"` — this crate serves only streamable HTTP (§L6).
    #[serde(rename = "type")]
    pub transport: String,
    /// The endpoint URL, including the ephemeral port.
    pub url: String,
    /// Headers the client must send; carries the per-launch token.
    pub headers: std::collections::BTreeMap<String, String>,
}

impl ClientConfig {
    /// Build the configuration that talks to `endpoint`.
    pub fn for_endpoint(endpoint: &McpEndpoint) -> Self {
        let headers = std::iter::once((
            axum::http::header::AUTHORIZATION.as_str().to_owned(),
            endpoint.token.bearer_header_value(),
        ))
        .collect();

        let entry = ClientServerEntry {
            transport: "http".to_owned(),
            url: endpoint.url(),
            headers,
        };

        Self {
            mcp_servers: std::iter::once(("nudox".to_owned(), entry)).collect(),
        }
    }
}

/// Reject any request that does not carry the launch token.
///
/// Runs in front of `StreamableHttpService`, so an unauthenticated caller never
/// reaches rmcp and never causes a session to be allocated. The `Session`
/// returned by [`Unauthenticated::authenticate`] is placed in the request
/// extensions: the value is proof the check ran, and putting it on the request
/// means a future per-session policy can consume it without re-deriving trust.
async fn require_session_token(
    State(expected): State<SessionToken>,
    mut request: Request,
    next: Next,
) -> Response {
    // Scoped so the borrow of `request`'s headers ends before the mutable
    // borrow below; `Session` carries no lifetime, only the proof.
    let authenticated = {
        let credential = Unauthenticated::from_headers(request.headers());
        credential.authenticate(&expected)
    };

    match authenticated {
        Ok(session) => {
            request.extensions_mut().insert(session);
            next.run(request).await
        }
        Err(_) => unauthorized().into_response(),
    }
}

/// The rejection every failed authentication produces.
///
/// One shape for "absent" and "wrong" alike: a caller that guessed learns only
/// that it guessed wrong. `WWW-Authenticate` is set so a well-behaved client
/// reports a configuration problem instead of retrying forever.
fn unauthorized() -> impl IntoResponse {
    (
        StatusCode::UNAUTHORIZED,
        [(
            axum::http::header::WWW_AUTHENTICATE,
            "Bearer realm=\"nudox\"",
        )],
        "missing or invalid session token",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bind_address_is_loopback_with_an_ephemeral_port() {
        assert!(
            LOOPBACK_BIND.ip().is_loopback(),
            "must never bind a routable address"
        );
        assert_eq!(
            LOOPBACK_BIND.port(),
            0,
            "port 0 asks the kernel for a free port"
        );
    }

    /// The full decision table for `preferred_bind` lives in
    /// `tests/mcp/endpoint_stability.rs`, which is the contract; this pins
    /// just the two cases co-located with the function they test.
    #[test]
    fn preferred_bind_tries_a_remembered_port_and_normalises_zero_to_any() {
        assert_eq!(preferred_bind(PortPreference::Any), LOOPBACK_BIND);
        assert_eq!(
            preferred_bind(PortPreference::Remembered(0)),
            LOOPBACK_BIND,
            "a remembered zero is what a corrupt state file decodes to, not a real port"
        );
        assert_eq!(
            preferred_bind(PortPreference::Remembered(51234)).port(),
            51234
        );
    }
}

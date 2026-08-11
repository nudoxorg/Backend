//! Hosting the MCP server (GUI-LOCAL-PLAN §L6, LIMITATIONS.md **L35**).
//!
//! # What L35 was
//!
//! The server was written, tested end-to-end, and *never started*. `grep -rn
//! 'nudox_mcp|McpEndpoint|NudoxMcpServer' workspace/gui` returned zero hits,
//! and `StatusBar::set_mcp_endpoint` had exactly one occurrence repo-wide — its
//! own definition. Every individual piece was real; the integration was not.
//! A dead setter is the tell.
//!
//! # The two states this module refuses to conflate
//!
//! The status bar's old field was `Option<SharedString>`, documented as "the
//! loopback endpoint … or `None` before it has bound its port". One `None`
//! stood for *four* different facts: not started yet, started and failed to
//! bind, stopped on quit, and "this process does not host a server at all"
//! (every test app). A reader who saw no MCP segment could not tell which, and
//! neither could a caller — which is doctrine §8's "degraded case presented as
//! the good one" in its smallest possible form.
//!
//! [`McpStatus`] makes each of those a variant, so the status bar branches on
//! them instead of on the absence of a string, and a server that failed to bind
//! says so on screen rather than looking like a server that is merely quiet.
//!
//! # Why there is no `Starting`
//!
//! [`McpHost::start`] returns only once the listener is bound and its port is
//! known. There is therefore no instant at which a host exists but is not yet
//! accepting connections, so "queried before startup completed" is not a state
//! this type can be in — it is unrepresentable rather than merely handled. The
//! pre-start state is [`McpStatus::Absent`], which advertises nothing.
//!
//! # Ownership and shutdown
//!
//! [`McpService`] is a GPUI [`Global`]: one server per process, matching §L6's
//! one-endpoint-per-lindsey and LD-20's one window. `main` stops it from
//! `on_app_quit`, which GPUI runs with a bounded timeout before the process
//! exits; `McpHost`'s own `Drop` closes the listener if that hook never fires.

use gpui::{App, Global, SharedString};
use nudox_engine::EngineHandle;
use nudox_mcp::{McpError, McpHost, ShutdownOutcome};

/// What the process's MCP server is doing, in the vocabulary the status bar
/// renders.
///
/// Every variant is reachable and they are mutually exclusive — that is the
/// point. See the module docs for the four facts the previous
/// `Option<SharedString>` collapsed into one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpStatus {
    /// This process does not host an MCP server.
    ///
    /// True of every `#[gpui::test]` app and of the frames rendered before
    /// `main` installs the service. The status bar shows no MCP segment —
    /// which is honest, because there is nothing to connect to and never was.
    Absent,

    /// Listening, with a URL a client can be pointed at right now.
    Listening {
        /// Exactly `McpHost::url()` — the string
        /// `tests/mcp_endpoint.rs` dials. Not a re-rendering of the address,
        /// so the displayed text and the reachable socket cannot drift.
        url: SharedString,
        /// The `mcpServers` JSON for Settings → Connection (GUI-PLAN §21).
        client_config: SharedString,
    },

    /// The server could not be started.
    ///
    /// `reason` is the flattened `McpError` chain. Flattened to a string at
    /// this seam for the same reason `PackageLoadEvent::LoadFailed` flattens
    /// `SourceError`: `McpError` is `#[non_exhaustive]`, so a `match` here
    /// would need a wildcard arm — which §6 lists as a weakening — and the
    /// GUI's response to every variant is identical anyway (say so, keep
    /// running). The distinction that matters to a *reader* is
    /// `Failed` vs `Absent`, and that one is typed.
    Failed {
        /// Human-readable cause, including every `#[source]` link.
        reason: SharedString,
    },

    /// The server was started and has since been stopped (app quit).
    Stopped,
}

impl McpStatus {
    /// The status of the process-wide server, or [`Absent`](Self::Absent) when
    /// this process never started one.
    ///
    /// The status bar reads through this rather than being handed a value at
    /// construction, so a window opened before — or without — the service still
    /// renders something true.
    pub fn from_app(cx: &App) -> Self {
        cx.try_global::<McpService>()
            .map_or(Self::Absent, |service| service.status().clone())
    }

    /// The URL a client should be pointed at, when there is one.
    ///
    /// `None` in every state except [`Listening`](Self::Listening) — a stopped
    /// or failed server has no address, and returning the last one it had is
    /// how a status bar ends up advertising a port that has since been reused.
    pub fn url(&self) -> Option<&SharedString> {
        match self {
            Self::Listening { url, .. } => Some(url),
            Self::Absent | Self::Failed { .. } | Self::Stopped => None,
        }
    }

    /// The label the status bar paints, or `None` to hide the segment entirely.
    ///
    /// Formatted here, once per state change, rather than in `render`
    /// (§1.1.4 — a status bar that ran `format!` per frame would allocate on
    /// every one of the 120 frames a second it is visible).
    pub fn label(&self) -> Option<SharedString> {
        match self {
            // Nothing to say: no server was ever asked for.
            Self::Absent => None,
            Self::Listening { url, .. } => Some(SharedString::from(format!("mcp {url}"))),
            Self::Failed { .. } => Some(SharedString::from("mcp unavailable")),
            Self::Stopped => Some(SharedString::from("mcp stopped")),
        }
    }

    /// The line the **application menu** shows for this state.
    ///
    /// # Why this is not [`label`](Self::label)
    ///
    /// Two differences, both forced by where the text appears rather than by
    /// taste:
    ///
    /// * It is **total**. `label` returns `None` for [`Absent`](Self::Absent)
    ///   so the status bar can drop the segment — correct there, because the
    ///   rest of the bar still tells the reader the app is alive. The menu is
    ///   the *only* surface left once the window is dismissed
    ///   (`app::lifecycle`), so a hidden line there reproduces exactly the
    ///   silence L35 was: a process running with an endpoint nobody can see.
    /// * It is **prose**. `label` is chrome text under a 11 px caption token in
    ///   a crowded bar, so it reads `mcp …`. A menu row has room for a
    ///   sentence, and is read once rather than glanced at continuously.
    ///
    /// Both derive from the same `self`, in the same file, so a state that
    /// gains a variant breaks both at once — which is the only reason having
    /// two renderings is safe.
    pub fn menu_label(&self) -> SharedString {
        match self {
            Self::Absent => SharedString::from("No MCP endpoint in this process"),
            Self::Listening { url, .. } => SharedString::from(format!("MCP endpoint: {url}")),
            Self::Failed { .. } => SharedString::from("MCP endpoint unavailable"),
            Self::Stopped => SharedString::from("MCP endpoint stopped"),
        }
    }

    /// Whether this state is a failure the reader should be able to see is a
    /// failure. Drives the segment's colour; kept here so "which states are
    /// bad" is answered once instead of in a `match` inside `render`.
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }

    /// Build the status for a failed start.
    ///
    /// Walks the whole `#[source]` chain, because doctrine §8 records that
    /// reading only the top-level `Display` is how a five-second diagnosis
    /// becomes an hour — `McpError::Bind`'s own message names the address, but
    /// the `io::Error` underneath it is what says *why*.
    pub fn failed(error: &McpError) -> Self {
        let mut reason = error.to_string();
        let mut source = std::error::Error::source(error);
        while let Some(link) = source {
            reason.push_str(": ");
            reason.push_str(&link.to_string());
            source = std::error::Error::source(link);
        }
        Self::Failed {
            reason: SharedString::from(reason),
        }
    }
}

/// The process-wide MCP server and its current status.
///
/// Installed by `main` with `cx.set_global`, in the same way `MotionTokens` and
/// `NudoxThemeExt` are: it is one-per-process state that outlives any window.
pub struct McpService {
    /// `None` when the server failed to start, or after [`stop`](Self::stop).
    host: Option<McpHost>,
    /// Derived from `host` at every transition, never accumulated beside it
    /// (doctrine §8): the two cannot disagree because nothing else writes it.
    status: McpStatus,
}

impl Global for McpService {}

impl McpService {
    /// Start the server on the engine's runtime.
    ///
    /// Never returns an error: a lindsey that cannot host an agent endpoint is
    /// still a working documentation browser, so a bind failure becomes a
    /// visible [`McpStatus::Failed`] rather than a refusal to open the window.
    /// That choice is only defensible *because* it is visible — an unstarted
    /// server that rendered like an absent one is exactly what L35 was.
    ///
    /// `gate` is the account gate every tool call is admitted through
    /// (`auth.md`). It is a parameter because `NudoxMcpServer` requires one and
    /// because the same gate is what `app::account` renders — a second gate
    /// would be a second answer to "is this user signed in?".
    pub fn start(engine: &EngineHandle, gate: nudox_mcp::AccountGate) -> Self {
        match McpHost::start(engine, gate) {
            Ok(host) => {
                let status = Self::status_of(&host);
                tracing::info!(
                    url = ?host.url(),
                    "mcp server hosted; paste Settings → Connection into an agent",
                );
                Self {
                    host: Some(host),
                    status,
                }
            }
            Err(error) => {
                tracing::error!(%error, "mcp server could not start");
                Self {
                    host: None,
                    status: McpStatus::failed(&error),
                }
            }
        }
    }

    /// The current status. Cheap; clone it into the status bar.
    pub fn status(&self) -> &McpStatus {
        &self.status
    }

    /// Stop the server and move to [`McpStatus::Stopped`].
    ///
    /// Idempotent. Returns the host's own typed outcome so a caller can tell a
    /// clean drain from a bare signal — see [`ShutdownOutcome`].
    pub fn stop(&mut self) -> ShutdownOutcome {
        let outcome = match self.host.as_mut() {
            Some(host) => host.stop(),
            None => ShutdownOutcome::AlreadyStopped,
        };
        self.host = None;
        // A server that failed to bind was never listening, so "stopped" would
        // overwrite the only record of *why* the reader sees no endpoint.
        if !self.status.is_failure() {
            self.status = McpStatus::Stopped;
        }
        outcome
    }

    /// The status implied by a live host.
    ///
    /// Private and total: the only way to reach `Listening` is to hold a host
    /// that reported a URL, so no code path can advertise an endpoint it did
    /// not get from the server itself.
    fn status_of(host: &McpHost) -> McpStatus {
        match (host.url(), host.client_config_snippet()) {
            (Some(url), Some(client_config)) => McpStatus::Listening {
                url: SharedString::from(url),
                client_config: SharedString::from(client_config),
            },
            // Unreachable while the host is running — both accessors are `Some`
            // for exactly the same reason (the endpoint exists). Encoded as
            // `Stopped` rather than `unwrap`ped because `unwrap` outside tests
            // is banned (§L7.6) and because a host that stopped between the two
            // calls really is stopped.
            _ => McpStatus::Stopped,
        }
    }
}

impl Drop for McpService {
    /// Backstop for the `on_app_quit` hook.
    ///
    /// `McpHost::drop` cancels the listener without blocking, so a teardown
    /// path that never runs the quit handler still closes the socket.
    fn drop(&mut self) {
        self.host = None;
    }
}

impl std::fmt::Debug for McpService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpService")
            .field("status", &self.status)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

    fn bind_error() -> McpError {
        McpError::Bind {
            addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            source: std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                "Address already in use (os error 48)",
            ),
        }
    }

    /// The state L35 could not express. A server that failed to bind must not
    /// render the same as a process that never hosted one.
    #[test]
    fn a_failed_start_is_distinguishable_from_never_having_started() {
        let failed = McpStatus::failed(&bind_error());

        assert_ne!(failed, McpStatus::Absent);
        assert!(failed.is_failure());
        assert!(!McpStatus::Absent.is_failure());
        assert_eq!(
            McpStatus::Absent.label(),
            None,
            "a process with no server shows no segment",
        );
        assert!(
            failed.label().is_some(),
            "a server that failed must be visible on screen",
        );
    }

    /// Doctrine §8: reading only the top-level `Display` is how a five-second
    /// diagnosis becomes an hour. The reason must carry the whole chain.
    #[test]
    fn a_failure_reason_carries_the_whole_source_chain() {
        let McpStatus::Failed { reason } = McpStatus::failed(&bind_error()) else {
            panic!("a bind error must produce a Failed status");
        };
        assert!(
            reason.contains("127.0.0.1:0"),
            "the address we tried to bind must survive: {reason}",
        );
        assert!(
            reason.contains("Address already in use"),
            "the underlying io::Error must survive: {reason}",
        );
    }

    /// Only a listening server has an address. A stopped or failed one that
    /// still reported its last URL would send a reader to a port that may
    /// since have been reused by another process.
    #[test]
    fn only_a_listening_server_advertises_a_url() {
        let listening = McpStatus::Listening {
            url: SharedString::from("http://127.0.0.1:51234/mcp"),
            client_config: SharedString::from("{}"),
        };
        assert_eq!(
            listening.url().map(SharedString::to_string).as_deref(),
            Some("http://127.0.0.1:51234/mcp"),
        );
        assert_eq!(McpStatus::Absent.url(), None);
        assert_eq!(McpStatus::Stopped.url(), None);
        assert_eq!(McpStatus::failed(&bind_error()).url(), None);
    }

    /// The displayed label must contain the dialable URL verbatim, because
    /// that is what a reader copies out of the window by hand.
    #[test]
    fn the_listening_label_contains_the_url_verbatim() {
        let listening = McpStatus::Listening {
            url: SharedString::from("http://127.0.0.1:51234/mcp"),
            client_config: SharedString::from("{}"),
        };
        let label = listening.label().expect("a listening server has a label");
        assert!(
            label.contains("http://127.0.0.1:51234/mcp"),
            "label {label:?} must show the dialable url",
        );
    }
}

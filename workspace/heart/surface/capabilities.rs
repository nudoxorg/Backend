//! The capability handshake — what a remote node can authoritatively serve,
//! learned in one probe before any query is delegated to it.
//!
//! # Why this exists
//!
//! `LOCAL-REMOTE-CONTRACT.md` gives a caller `Arc<dyn Serve<S>>` and a
//! [`Federated`](super::Federated) that fans every request to *all* its sources
//! and merges. That is the right default when both a local engine and a remote
//! are peers of equal authority. It is the *wrong* default when the remote is
//! authoritative and the whole point is for the client to do the **minimum**
//! work — there, fanning to a fully-capable local engine as well is pure wasted
//! compute and memory.
//!
//! §6.2 of that document names the missing piece outright: *"Prefer an explicit
//! merge policy parameter when the index crate is next open."* This module is
//! the input to that policy. A client probes [`CAPABILITIES_PATH`] once, learns
//! which [`SurfaceId`]s the remote serves, at which [`GenerationId`], speaking
//! which [`PROTOCOL_VERSION`] — and a router can then decide, per surface,
//! whether to delegate to the remote (and skip local work entirely) or serve
//! locally. With no reachable remote, or one speaking a protocol it cannot
//! understand, the client serves locally, exactly as a standalone node always
//! does. Standalone is the floor; delegation is the optimization the handshake
//! unlocks.

use serde::{Deserialize, Serialize};

use super::GenerationId;

/// The wire protocol version this build speaks.
///
/// Bumped only on a backwards-*incompatible* change to the request/response
/// shapes or the framing of the surface contract. Additive fields — every
/// `#[serde(default)]` field on [`Capabilities`], for instance — do not require
/// a bump, because an older peer simply does not send them and a newer peer
/// fills the default. A client compares a remote's [`Capabilities::protocol`]
/// against this constant ([`Capabilities::is_compatible`]) to decide whether it
/// may delegate at all.
pub const PROTOCOL_VERSION: u32 = 1;

/// The route the capability handshake is served at.
///
/// Named once here, exactly as [`Surface::PATH`](super::Surface::PATH) is, so a
/// client and server cannot drift onto two different spellings — there is one
/// string, not one hand-written probe method per side that might disagree.
pub const CAPABILITIES_PATH: &str = "/capabilities";

/// A streaming surface a node can serve, named by its stable
/// [`Surface::NAME`](super::Surface::NAME).
///
/// A `dyn Surface` cannot cross the wire, but its *identity* can — this is the
/// enumerable, wire-safe projection of the surface set that a capability
/// response carries. The `snake_case` rendering matches each surface's `NAME`
/// (`"symbols"`, `"packages"`, `"usages"`), so [`SurfaceId::as_str`] and a
/// `Surface`'s `NAME` are the same token by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceId {
    /// Symbol search — [`Symbols`](super::Symbols).
    Symbols,
    /// Package search — [`Packages`](super::Packages).
    Packages,
    /// Reverse usages — [`Usages`](super::Usages).
    Usages,
}

impl SurfaceId {
    /// Every streaming surface a fully-featured node serves. A server builds its
    /// advertised surface list from this so adding a surface to the contract is
    /// a compile error here until it is accounted for.
    pub const ALL: [SurfaceId; 3] = [Self::Symbols, Self::Packages, Self::Usages];

    /// The stable token for this surface — identical to the corresponding
    /// [`Surface::NAME`](super::Surface::NAME).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Symbols => "symbols",
            Self::Packages => "packages",
            Self::Usages => "usages",
        }
    }

    /// Recover a [`SurfaceId`] from a [`Surface::NAME`](super::Surface::NAME)
    /// token. `None` for an unknown name — a newer peer naming a surface this
    /// build does not have.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "symbols" => Some(Self::Symbols),
            "packages" => Some(Self::Packages),
            "usages" => Some(Self::Usages),
            _ => None,
        }
    }
}

/// What a remote node can authoritatively answer.
///
/// The input to a client's routing/merge policy: with a node's `Capabilities`
/// in hand, a router decides — per surface — whether to delegate to the remote
/// (doing no local work) or serve locally, rather than unconditionally fanning
/// to both. See the module docs for how this makes "minimum work when
/// connected, fully standalone when not" expressible instead of hardcoded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// The wire protocol version the node speaks (see [`PROTOCOL_VERSION`]).
    pub protocol: u32,
    /// The streaming surfaces the node authoritatively serves.
    pub surfaces: Vec<SurfaceId>,
    /// The corpus generation the node is currently serving, when it tracks one.
    ///
    /// A router uses this to reason about staleness — an item held locally as
    /// [`Residence::Synced`](super::Residence::Synced) at an older generation
    /// than the remote now serves is a candidate for refresh — and to tag
    /// delegated results with the right [`Residence`](super::Residence). `None`
    /// when the node does not (yet) expose a generation; a router then treats
    /// delegated data as current-but-ungeneration-stamped rather than assuming
    /// staleness.
    #[serde(default)]
    pub generation: Option<GenerationId>,
    /// Whether the node can answer semantic (embedding-backed) queries.
    ///
    /// A distinct axis from [`surfaces`](Capabilities::surfaces): a node with no
    /// vector plane still serves `Symbols` by name and type, so a router may
    /// delegate precise symbol search to it while keeping semantic search local
    /// (or declining semantics entirely). `false` unless the node affirmatively
    /// advertises a working semantic plane.
    #[serde(default)]
    pub semantic: bool,
}

impl Capabilities {
    /// Whether a client at [`PROTOCOL_VERSION`] may safely delegate to a node
    /// advertising these capabilities.
    ///
    /// Exact-match today (there is only version 1). Kept as a method so the
    /// compatibility rule — and any future "client tolerates server N-1" window
    /// — lives in exactly one place rather than being re-derived at every
    /// routing decision.
    pub fn is_compatible(&self) -> bool {
        self.protocol == PROTOCOL_VERSION
    }

    /// Whether the node authoritatively serves `surface`.
    pub fn serves(&self, surface: SurfaceId) -> bool {
        self.surfaces.contains(&surface)
    }
}

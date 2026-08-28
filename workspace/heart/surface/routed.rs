//! `Routed<S>` — the capability-gated router: one `Serve<S>` that delegates to a
//! remote when the remote authoritatively serves this surface, and otherwise
//! serves locally.
//!
//! # What this is for
//!
//! [`Federated`](super::Federated) fans every request to *all* its sources and
//! merges — the right shape when a local engine and a remote are peers of equal
//! authority. It is the wrong shape when the whole goal is for the client to do
//! the **minimum** work: fanning to a fully-capable local engine *as well as* an
//! authoritative remote runs the expensive local query (name/type/semantic
//! search over the resident corpus) for an answer the remote already has. That
//! is wasted compute and memory on every keystroke.
//!
//! `Routed` is the "explicit merge policy parameter" `LOCAL-REMOTE-CONTRACT.md`
//! §6.2 asks for, in its simplest honest form: a **selection**, not a merge. Per
//! request it serves exactly one side —
//!
//! - the **remote**, when a [`Capabilities`] handshake says the remote speaks a
//!   compatible protocol and authoritatively serves this surface. The local
//!   engine is never invoked, so no local query runs — this is the "minimum work
//!   when connected" the whole contract is being bent toward.
//! - the **local** engine otherwise: no remote configured, an incompatible
//!   protocol, or a remote that does not advertise this surface. This is the
//!   standalone floor — a partial or unreachable remote never causes a query to
//!   be dropped, it just falls back to serving locally, exactly as a node with
//!   no remote always does.
//!
//! # Why the decision is captured at construction
//!
//! The routing decision is computed once, from a capability snapshot, not
//! re-evaluated per request. A client re-probes capabilities — the same
//! [`RemoteClient::capabilities`](../client/remote/struct.RemoteClient.html)
//! `GET /capabilities` that answers "am I connected?" — and rebuilds the router
//! when connectivity changes. So a remote that dies mid-session degrades the
//! *in-flight* answer through the ordinary [`Frame::Degraded`](super::Frame)/
//! [`Frame::Failed`](super::Frame) path the GUI already renders, and the next
//! probe routes subsequent queries back to local. Connectivity is a handshake
//! decision, deliberately, rather than a per-request retry ladder that would add
//! latency to the common case where the remote is up.
//!
//! `Routed` is itself a `Serve<S>`, so a caller holding `Arc<dyn Serve<S>>`
//! cannot tell whether it has a local engine, a remote client, a `Federated`
//! merge, or a `Routed` selector — the same transparency every other impl in
//! this module preserves.

use std::sync::Arc;

use super::capabilities::{Capabilities, SurfaceId};
use super::{Answer, Gen, Serve, Surface};

/// A capability-gated selector between a local engine and an optional remote.
/// See the module docs for the policy.
pub struct Routed<S: Surface> {
    local: Arc<dyn Serve<S>>,
    remote: Option<Arc<dyn Serve<S>>>,
    /// The captured decision: serve this surface from the remote (`true`) or the
    /// local engine (`false`). Always `false` when `remote` is `None`.
    delegate: bool,
}

impl<S: Surface> Routed<S> {
    /// A standalone router: no remote, every request served locally. The floor
    /// the contract guarantees — identical behaviour to holding the local
    /// `Serve<S>` directly, but spelled as a `Routed` so a caller can hold one
    /// type whether or not a remote is present.
    pub fn local_only(local: Arc<dyn Serve<S>>) -> Self {
        Self {
            local,
            remote: None,
            delegate: false,
        }
    }

    /// Route between `local` and `remote` using the remote's advertised
    /// `capabilities`.
    ///
    /// The remote is delegated to for this surface iff it speaks a compatible
    /// protocol ([`Capabilities::is_compatible`]) **and** advertises this
    /// surface ([`Capabilities::serves`], matched by [`Surface::NAME`]). If the
    /// surface is one [`SurfaceId`] does not know — a `Surface::NAME` this build
    /// cannot enumerate — the remote is never delegated to for it, and the local
    /// engine serves. Every "no" here falls back to local; none drops the query.
    pub fn new(
        local: Arc<dyn Serve<S>>,
        remote: Arc<dyn Serve<S>>,
        capabilities: &Capabilities,
    ) -> Self {
        let delegate = capabilities.is_compatible()
            && SurfaceId::from_name(S::NAME).is_some_and(|id| capabilities.serves(id));
        Self {
            local,
            remote: Some(remote),
            delegate,
        }
    }

    /// Whether this router will serve from the remote (doing no local work).
    /// Exposed so a caller can label a result group "served remotely" and so a
    /// router's decision is inspectable in a test without observing which source
    /// ran.
    pub fn delegates(&self) -> bool {
        self.delegate && self.remote.is_some()
    }
}

impl<S: Surface> Serve<S> for Routed<S> {
    /// Serve `request` from exactly one side — the remote when [`delegates`]
    /// (`Routed::delegates`) is true, the local engine otherwise. The request is
    /// handed to that side unmodified; `Routed` adds no framing of its own, so
    /// the residence tags on the delivered items are the serving side's own
    /// (`Residence::Remote`/`Synced` from a remote, `Residence::Local` from the
    /// engine) with nothing to re-tag.
    fn serve(&self, request: S::Request, generation: Gen) -> Answer<S> {
        match (self.delegate, &self.remote) {
            (true, Some(remote)) => remote.serve(request, generation),
            _ => self.local.serve(request, generation),
        }
    }
}

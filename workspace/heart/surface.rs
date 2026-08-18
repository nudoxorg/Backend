//! The one local/remote contract: `Surface` + `Frame<S>` + `Answer<S>` + `Serve<S>`.
//!
//! # The error class this closes
//!
//! `docs/LOCAL-REMOTE-CONTRACT.md` §0 catalogues the damage of not having this
//! module: twelve unrelated streaming envelopes (`StreamFrame`, `DocEvent`,
//! `SearchEvent`, `QueryEvent`, …) that each re-declare their own terminal pair
//! and their own `generation: Gen` field; residence encoded six different,
//! subtly incompatible ways (`SourceRole`, `sync_endpoint`, the
//! `vector/{local,remote}` split, `RemoteRouteReason`, `Ir`/`ObjectAvailability`,
//! `wire::Provenance`); and a remote path that is a buffered `Vec` wearing a
//! streaming costume, forcing the GUI to run two parallel state machines — one
//! for local (fully streaming, cancellable, generation-guarded) and a second,
//! visibly second-class one for remote (a spinner, then a `Vec`, in its own
//! `RemoteStatus` field).
//!
//! `Surface` is the unit this collapses onto: it ties a request to its item,
//! out-of-band metadata, and dedup identity *at compile time*, so the runtime
//! check the old code needed (`search.rs:104`: does this request's target match
//! this engine's hit type?) becomes unrepresentable. `Frame<S>` is the one
//! envelope every surface streams — replacing the twelve. `Located<T>` puts
//! residence on the *item*, not the call, so one answer can legitimately mix
//! local and remote rows. `Answer<S>` is the one thing every backend returns,
//! and `Serve<S>` is deliberately object-safe so a caller holding
//! `Arc<dyn Serve<S>>` cannot tell whether it is local, remote, or a federating
//! merge of both — see §2 of the contract doc for why that is the whole point.
//!
//! # Why `flume::Receiver` and not `impl Stream`
//!
//! This is the one non-obvious choice in here, and the contract doc is explicit
//! that it is forced by a real constraint rather than taste (§1.4). The GUI has
//! no async reactor of its own — GPUI's executor is the only runtime it drives,
//! and it needs to drain a burst of frames synchronously (await one, then
//! `try_recv` the rest into a single repaint; see
//! `an_answer_supports_the_gui_batched_drain_pattern` in the test module this
//! implements). The server side needs a genuine `Stream` for
//! `axum::body::Body::from_stream`. A `flume::Receiver` is both: drainable
//! outside any executor via `try_recv`, awaitable via `recv`, and convertible to
//! a `Stream` via `into_stream`. It is a strict superset of `impl Stream` for
//! this crate's purposes, and it is already the shape the local engine's
//! `EngineHandle` uses — this promotes a proven mechanism rather than inventing
//! a new one.

use std::collections::VecDeque;
use std::fmt;
use std::hash::Hash;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::{Bytes, BytesMut};
use futures::Stream;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::access::SourceId;
use crate::query::UnixMilliseconds;
use crate::stream::WireError;

// ---------------------------------------------------------------------------
// Gen / GenerationId
// ---------------------------------------------------------------------------

/// A logical generation for one *query* — bumped every time a caller issues a
/// new request that supersedes the last (the canonical case: a keystroke in
/// search-as-you-type). Frames tagged with a stale `Gen` are dropped by the
/// reader without inspection; this is the same generation-guard discipline
/// `nudox_engine`'s event arms already apply to every event, promoted here so
/// `Answer<S>` can report which generation it is answering
/// ([`Answer::generation`]) without the caller having to thread it through by
/// hand.
///
/// Distinct from [`GenerationId`]: a `Gen` numbers *requests made by this
/// process*; a `GenerationId` numbers *corpus snapshots on a remote*. They are
/// different axes that happen to share a representation — keeping them as
/// separate types stops a caller from comparing one to the other by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Gen(pub u64);

/// The generation of a *remote corpus* an item was fetched from or is claimed
/// to be current as of — the identifier [`Residence::Synced`] and
/// [`Residence::Remote`] carry so a client can tell "these two items are the
/// same underlying data, one materialized locally and one not" from "these two
/// items are from different corpus snapshots entirely".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct GenerationId(pub u64);

// ---------------------------------------------------------------------------
// Residence / Located
// ---------------------------------------------------------------------------

/// Where an item came from, and how much it can be trusted while offline.
///
/// This is `nudox_engine::wire::Provenance` promoted into `heart` and actually
/// populated (contract §1.3, §0.5) — it replaces six incompatible encodings
/// that had accreted across the system: `SourceRole`, `sync_endpoint`, the
/// `vector/{local,remote}` split, `RemoteRouteReason`,
/// `Ir`/`ObjectAvailability`, and `wire::Provenance` itself, which was
/// structurally the best of the six but dead in practice because nothing below
/// the GUI could actually construct `Provenance::Remote`.
///
/// `#[non_exhaustive]`: a future residence (e.g. a federated-and-cached tier)
/// must not silently break every existing `match` on this type — readers add a
/// fallback arm instead of failing to compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Residence {
    /// Materialized here, produced from a source this process can see
    /// directly.
    Local,
    /// Fetched from a remote generation and content-verified — the same data
    /// as [`Residence::Remote`] at the same [`GenerationId`], except this copy
    /// is on disk. This is the distinction that makes offline-first honest:
    /// `Synced` survives the network dropping; `Remote` does not.
    Synced { generation: GenerationId },
    /// Served remotely; not materialized locally. Needs the network to be
    /// served again.
    Remote { generation: GenerationId },
    /// Last-known-good, served while the source that produced it is
    /// unreachable. Carries *when* it was last known good rather than a
    /// generation, because a stale item's originating generation is exactly
    /// the thing that could not be confirmed.
    Stale { as_of: UnixMilliseconds },
}

impl Residence {
    /// The remote generation this residence is pinned to, if it has one.
    /// `Local` has no remote generation by definition; `Stale` deliberately
    /// does not carry one either (see the variant's own doc comment).
    pub const fn generation(&self) -> Option<GenerationId> {
        match self {
            Residence::Synced { generation } | Residence::Remote { generation } => {
                Some(*generation)
            }
            Residence::Local | Residence::Stale { .. } => None,
        }
    }

    /// Whether this item can still be served with the network down. True for
    /// everything except [`Residence::Remote`] — `Local`, `Synced`, and
    /// `Stale` all name data that is materialized (or was, and is still being
    /// offered as last-known-good) on this machine.
    pub const fn is_offline_capable(&self) -> bool {
        !matches!(self, Residence::Remote { .. })
    }
}

/// A value tagged with where it came from.
///
/// Residence rides on the *item*, not on the call that produced it (contract
/// §1.3) — this is what makes a merged answer representable at all. If
/// residence were a property of the answer instead, a federated result set
/// with rows from two sources would need one label for both, which is exactly
/// why the GUI currently has to render remote hits as a visibly separate
/// group. Per-item residence means the merge just interleaves `Located<T>`
/// values; nothing downstream has to special-case where a given row came from
/// unless it wants to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Located<T> {
    pub value: T,
    pub residence: Residence,
}

impl<T> Located<T> {
    /// Tag `value` with `residence`.
    pub const fn new(value: T, residence: Residence) -> Self {
        Self { value, residence }
    }

    /// Transform the carried value, leaving residence untouched. Used by
    /// merge/reconcile code that needs to project `Located<A>` to
    /// `Located<B>` (e.g. down to a dedup key) without losing the provenance
    /// tag along the way.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Located<U> {
        Located {
            value: f(self.value),
            residence: self.residence,
        }
    }
}

// ---------------------------------------------------------------------------
// Completeness / Summary
// ---------------------------------------------------------------------------

/// Whether an answer covers everything it was asked for.
///
/// This is a first-class, renderable *value*, not an error and not a silent
/// truncation (contract §1.2). It is the single mechanism that replaces both
/// today's separate `RemoteStatus` field ("the remote was unreachable, so this
/// is partial") and `SectionStatus::Building` ("the semantic index is still
/// warming up, so this is partial") — `End` cannot be constructed without
/// saying which kind of complete it is.
///
/// `#[non_exhaustive]`: a future third reason for partiality must not force
/// every existing reader to add a match arm it cannot yet do anything useful
/// with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Completeness {
    /// Everything that could be found was found.
    Complete,
    /// Only part of the corpus was covered. `total` is `None` when the corpus
    /// size itself is not yet known (e.g. a semantic index still building) —
    /// partiality must not be forced to claim a total it does not have.
    Partial { covered: u64, total: Option<u64> },
}

/// The terminal-success payload of an [`Answer`]: how many items were
/// delivered, and whether that was all of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Summary {
    /// How many [`Frame::Item`] frames preceded this one.
    pub items: u64,
    pub complete: Completeness,
}

impl Summary {
    /// A summary over a fully-covered answer of `items` rows.
    pub const fn complete(items: u64) -> Self {
        Self {
            items,
            complete: Completeness::Complete,
        }
    }

    /// A summary over a partial answer: `covered` of an (optionally unknown)
    /// `total`.
    pub const fn partial(items: u64, covered: u64, total: Option<u64>) -> Self {
        Self {
            items,
            complete: Completeness::Partial { covered, total },
        }
    }

    /// Shorthand for `matches!(self.complete, Completeness::Complete)`.
    pub const fn is_complete(&self) -> bool {
        matches!(self.complete, Completeness::Complete)
    }
}

// ---------------------------------------------------------------------------
// Degradation
// ---------------------------------------------------------------------------

/// One source dropped out mid-answer.
///
/// Carried by [`Frame::Degraded`], which is deliberately **not** terminal —
/// this is the behavioural difference that makes offline-first honest. A
/// remote source going away must leave whatever the local source already
/// delivered standing, rather than failing the whole answer the way a bare
/// `Result<Vec<T>, E>` would force.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Degradation {
    pub source: SourceId,
    pub reason: WireError,
}

// ---------------------------------------------------------------------------
// Surface
// ---------------------------------------------------------------------------

/// One answerable question: what the caller asks, what comes back, and how
/// answer rows are identified.
///
/// This is the unit the whole contract is generic over (§1.1). Today
/// `Query.target` and `QueryEngine::Hit` are unrelated types, so the pairing
/// between "what was asked" and "what kind of thing comes back" is checked at
/// runtime (`search.rs:104`). Tying them together as associated types on one
/// trait makes a mismatched pairing a compile error instead, with nothing left
/// for a runtime check to reject.
///
/// `Request`/`Item`/`Note` all require `Serialize + DeserializeOwned` because
/// every one of them has to cross the wire unmodified for the remote client
/// impl this contract exists to make free (§2.1) — a surface author does not
/// hand-write a wire DTO alongside the domain type, the domain type *is* the
/// wire type. `Send + Clone + 'static` on all three (`+ Sync` on `Request`
/// specifically) is what lets `Frame<S>` and `Answer<S>` cross an
/// `Emitter`/task boundary and be handed to `tokio::spawn`.
pub trait Surface: Send + Sync + 'static {
    /// Stable name — for tracing, and for a future `Note`/`Frame` schema
    /// registry.
    const NAME: &'static str;
    /// The remote route this surface is served at. Naming it once here, on
    /// the trait, is what stops a client and server from drifting onto two
    /// different strings — there is one route per surface, not one
    /// hand-written client method per route that might disagree with it.
    const PATH: &'static str;

    /// What the caller asks. Serialized as the request body verbatim.
    type Request: Serialize + DeserializeOwned + Send + Sync + Clone + 'static;

    /// One element of the answer.
    type Item: Serialize + DeserializeOwned + Send + Clone + 'static;

    /// Out-of-band metadata this surface emits alongside items: section
    /// states, column headers, latencies, page skeletons. Genuinely
    /// surface-specific — a widget search's columns are not a symbol search's
    /// section latency — so this stays a distinct type per surface; only the
    /// [`Frame`] framing around it is shared.
    type Note: Serialize + DeserializeOwned + Send + Clone + 'static;

    /// Per-item identity, for dedup and reconciliation across sources. Must be
    /// hashable so a federating merge can dedup with a `HashSet` rather than
    /// an O(n²) scan of everything seen so far.
    type Key: Eq + Hash + Send + 'static;

    /// Extract the dedup identity of one item.
    fn key(item: &Self::Item) -> Self::Key;
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// One line of a surface's answer stream — the single envelope replacing the
/// twelve unrelated ones catalogued in contract §0.4
/// (`StreamFrame`/`DocEvent`/`SearchEvent`/`QueryEvent`/… — eleven of twelve
/// re-declaring their own terminal pair, nine re-declaring their own
/// `generation` field). `heart::stream::StreamFrame<T>` becomes the degenerate
/// case of this: `Note = ()`, `Item = T`, and no source ever degrades.
///
/// Serialized **externally tagged, snake_case** — `{"note": …}` / `{"item":
/// …}` / `{"degraded": …}` / `{"end": …}` / `{"failed": …}` — the same
/// discipline [`crate::stream::StreamFrame`] already established, carried
/// forward here: every NDJSON line is a single-key object that says what kind
/// of line it is before it says anything else, so a reader never has to guess
/// a bare value's role or distinguish "a hit that happens to look odd" from "a
/// line that isn't a hit at all".
///
/// `#[non_exhaustive]`: a surface, or the framing itself, may grow a new frame
/// kind later (a progress marker, say); an older reader must not fail to
/// compile or panic on it — readers match with an explicit fallback arm.
///
/// # Why `Clone`/`Debug`/`PartialEq`/`Serialize`/`Deserialize` are hand-written
///
/// `#[derive(..)]` adds a bound on the *generic parameter itself* — here,
/// `S: Surface`, the marker type — for every derived trait, even though `S`
/// only ever appears through its associated types (`S::Note`, `S::Item`). A
/// surface marker (like the test-only `Widgets` this module's tests use) has
/// no reason to itself be `Clone`/`Debug`/`PartialEq`/etc.; forcing it to be
/// would leak an implementation accident into every surface author's code.
/// Each impl below is therefore bounded only on the associated types it
/// actually touches. `Clone` needs no `where` clause at all: `Surface`
/// already requires `S::Note: Clone` and `S::Item: Clone`, and the compiler
/// can see that through `S: Surface` without restating it. `Debug` and
/// `PartialEq` are not implied by `Surface`, so those impls add exactly the
/// associated-type bounds they need and no more. `Serialize`/`Deserialize` use
/// `#[serde(bound = "")]` to suppress serde's own (mistaken, for the same
/// reason as derive) auto-detected `S: Serialize` bound — the associated-type
/// bounds `Surface` already declares (`Note`/`Item`: `Serialize` +
/// `DeserializeOwned`) are visible to the generated impl through `S: Surface`
/// and supply everything it needs.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[serde(bound = "")]
#[non_exhaustive]
pub enum Frame<S: Surface> {
    /// Surface-specific out-of-band metadata.
    Note(S::Note),
    /// One element of the answer, tagged with where it came from.
    Item(Located<S::Item>),
    /// A source dropped out. Not terminal — see [`Degradation`]'s doc comment.
    Degraded(Degradation),
    /// Terminal, successful. Its presence is what distinguishes a complete
    /// answer from a truncated one — the same rule
    /// [`crate::stream::StreamFrame`] established for the single-payload case.
    End(Summary),
    /// Terminal, failed. No usable answer at all.
    Failed(WireError),
}

impl<S: Surface> Frame<S> {
    /// Whether this frame ends the answer. Only [`Frame::End`] and
    /// [`Frame::Failed`] do — in particular [`Frame::Degraded`] does not, so a
    /// reader that stops at the first terminal frame keeps consuming past a
    /// degraded source rather than discarding an answer that is still
    /// arriving.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Frame::End(_) | Frame::Failed(_))
    }
}

impl<S: Surface> Clone for Frame<S> {
    fn clone(&self) -> Self {
        match self {
            Frame::Note(note) => Frame::Note(note.clone()),
            Frame::Item(located) => Frame::Item(located.clone()),
            Frame::Degraded(degradation) => Frame::Degraded(degradation.clone()),
            Frame::End(summary) => Frame::End(*summary),
            Frame::Failed(error) => Frame::Failed(error.clone()),
        }
    }
}

impl<S: Surface> fmt::Debug for Frame<S>
where
    S::Note: fmt::Debug,
    S::Item: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Frame::Note(note) => f.debug_tuple("Note").field(note).finish(),
            Frame::Item(located) => f.debug_tuple("Item").field(located).finish(),
            Frame::Degraded(degradation) => f.debug_tuple("Degraded").field(degradation).finish(),
            Frame::End(summary) => f.debug_tuple("End").field(summary).finish(),
            Frame::Failed(error) => f.debug_tuple("Failed").field(error).finish(),
        }
    }
}

impl<S: Surface> PartialEq for Frame<S>
where
    S::Note: PartialEq,
    S::Item: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Frame::Note(a), Frame::Note(b)) => a == b,
            (Frame::Item(a), Frame::Item(b)) => a == b,
            (Frame::Degraded(a), Frame::Degraded(b)) => a == b,
            (Frame::End(a), Frame::End(b)) => a == b,
            (Frame::Failed(a), Frame::Failed(b)) => a == b,
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// StreamHandle
// ---------------------------------------------------------------------------

/// A cancellable stream slot handle.
///
/// Dropping the handle cancels the corresponding work. The caller never
/// touches the cancellation token directly.
///
/// This is `nudox_engine::runtime::StreamHandle` moved into `heart`, unchanged
/// in shape or semantics — `heart` is now the crate every surface answer flows
/// through, so this is the one home for it rather than a copy kept in sync by
/// hand.
///
/// # Why the canceller is `Sync` and not merely `Send`
///
/// It was `Box<dyn Fn() + Send>`, which made `StreamHandle` itself `!Sync`,
/// and that propagated to *anything holding one*. Holding a handle is exactly
/// what a caller does when it owns a long-running job rather than draining a
/// stream inline — `crate::mcp`'s index-job registry was the first such
/// caller, and the failure landed on rmcp's `ServerHandler: Sync` bound,
/// several types away from the cause and with no mention of cancellation in
/// the message.
///
/// Requiring `Sync` on the closure instead of adding a `Mutex` at that call
/// site is the fix at the right level: every canceller in the system is
/// `CancellationToken::cancel`, `cancel` takes `&self`, and `CancellationToken`
/// is `Sync` — so no existing construction loses anything, and a future
/// canceller that genuinely could not be shared across threads would now say
/// so at its own definition rather than at a distant holder's.
pub struct StreamHandle {
    /// The generation this stream answers.
    pub generation: Gen,
    /// Invoked on drop to signal cancellation.
    canceller: Box<dyn Fn() + Send + Sync>,
}

impl StreamHandle {
    /// Construct a handle for `generation`, cancelled by `canceller` on drop.
    ///
    /// `Sync` on `canceller` is load-bearing — see the type-level docs.
    pub fn new(generation: Gen, canceller: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            generation,
            canceller: Box::new(canceller),
        }
    }

    /// Cancel the stream explicitly. Also called implicitly on `Drop`.
    pub fn cancel(&self) {
        (self.canceller)();
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        (self.canceller)();
    }
}

impl fmt::Debug for StreamHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StreamHandle")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// EmitError
// ---------------------------------------------------------------------------

/// Why [`Emitter`] could not deliver a frame.
///
/// A typed variant rather than a bare `()` or a stringly-typed error, per the
/// crate's own doctrine (never assert on a message string where a variant will
/// do). Two genuinely distinct causes exist, and they mean different things at
/// a call site: [`EmitError::Cancelled`] says "the *consumer* went away, stop
/// doing the work that produces frames"; [`EmitError::Finished`] says "*this
/// answer itself* is already over, whatever you were about to emit arrived too
/// late to matter" — the consumer may be alive and well. Collapsing both into
/// one variant would make a caller's `match` lie about which situation it is
/// in.
///
/// `#[non_exhaustive]`: adding a failure class must not break a caller's
/// `match` — callers fold the unknown into their most conservative decision,
/// same as every other error enum in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum EmitError {
    /// The [`Answer`] side of this channel was dropped. The producer must
    /// stop — there is nothing left to receive this frame, and further
    /// `Emitter` calls will keep returning this rather than silently
    /// succeeding into the void.
    #[error("the answer was dropped; there is nothing left to receive this frame")]
    Cancelled,
    /// A terminal frame ([`Frame::End`] or [`Frame::Failed`]) was already
    /// emitted on this `Emitter`. The answer ended there — the same rule
    /// [`crate::stream::StreamFrame`] enforces on readers ("a line after the
    /// terminal frame is a protocol violation"), enforced here on the writer
    /// instead, so a frame emitted after the answer is over is rejected
    /// rather than silently queued for a consumer that has already stopped
    /// listening for it (see [`Answer::recv`]'s doc comment: the consumer
    /// stops at the terminal frame regardless of whether this `Emitter` is
    /// still alive).
    #[error("this answer already ended; a terminal frame was already emitted")]
    Finished,
}

// ---------------------------------------------------------------------------
// Answer / Emitter
// ---------------------------------------------------------------------------

/// The one thing every `Serve<S>` impl returns — local, remote, or a
/// federating merge of both (contract §1.4, §2). A caller holding an `Answer`
/// cannot tell which kind of backend produced it.
///
/// Backed by a `flume` channel — see this module's top-level doc comment for
/// why that shape and not `impl Stream` directly.
pub struct Answer<S: Surface> {
    generation: Gen,
    rx: flume::Receiver<Frame<S>>,
    /// Present once a producer has attached real cancellable work via
    /// [`Answer::with_handle`]. Dropping `Answer` drops this field along with
    /// everything else, which is what makes the attached canceller fire on
    /// drop with no custom `Drop` impl needed here.
    handle: Option<StreamHandle>,
    /// Set once a terminal frame ([`Frame::End`] or [`Frame::Failed`]) has
    /// been yielded to the consumer. See [`Answer::recv`]'s doc comment for
    /// why this exists as state on `Answer` at all rather than relying on
    /// `flume`'s own disconnect detection.
    terminal_seen: AtomicBool,
}

impl<S: Surface> Answer<S> {
    /// The generation this answer is answering.
    pub fn generation(&self) -> Gen {
        self.generation
    }

    /// Await the next frame. Returns `None` once a terminal frame has already
    /// been yielded, *or* once the [`Emitter`] side has been dropped and every
    /// frame it already sent has been drained.
    ///
    /// # Why a terminal frame ends the stream on its own, without waiting for
    /// disconnect
    ///
    /// The terminal frame — not the `Emitter`'s liveness — is what ends an
    /// answer, the same rule [`crate::stream::StreamFrame`] already applies on
    /// the wire ("a stream that ends MUST carry a terminal frame; anything
    /// after one is a protocol violation"). A producer may legitimately emit
    /// `End` and then keep its `Emitter` parked — held in a struct field,
    /// waiting in a `select!` for the next request, whatever — because owning
    /// the *ability* to emit again is not the same as intending to. If `recv`
    /// only stopped on disconnect, a consumer that had already received `End`
    /// would block forever on the next call waiting for a sender that has no
    /// more frames to give it: the channel is far from over even though the
    /// *answer* plainly is. `terminal_seen` decouples the two: it is set the
    /// moment a terminal frame is actually yielded from here, so every
    /// subsequent call returns `None` immediately without ever touching
    /// `flume`, regardless of whether the `Emitter` — or a hundred more
    /// already-queued frames the emitter tried to send after being rejected
    /// by [`EmitError::Finished`] — are still around.
    pub async fn recv(&self) -> Option<Frame<S>> {
        if self.terminal_seen.load(Ordering::Acquire) {
            return None;
        }
        let frame = self.rx.recv_async().await.ok()?;
        if frame.is_terminal() {
            self.terminal_seen.store(true, Ordering::Release);
        }
        Some(frame)
    }

    /// Non-blocking poll for the next frame, for the GUI batched-drain
    /// pattern: `await` one frame with [`Answer::recv`], then drain the rest
    /// of a burst with this before issuing a single repaint. Both methods
    /// operate on the same underlying receiver — this is the whole reason
    /// `Answer` is a `flume` receiver rather than a bare `impl Stream`, which
    /// only offers the `await`-driven half. Agrees with `recv` on ending at
    /// the terminal frame (see its doc comment) rather than on disconnect.
    pub fn try_recv(&self) -> Option<Frame<S>> {
        if self.terminal_seen.load(Ordering::Acquire) {
            return None;
        }
        let frame = self.rx.try_recv().ok()?;
        if frame.is_terminal() {
            self.terminal_seen.store(true, Ordering::Release);
        }
        Some(frame)
    }

    /// Convert into a `Stream`, for the server side (`axum::body::Body::from_stream`).
    ///
    /// Consumes `self` rather than borrowing, so any attached
    /// [`StreamHandle`] moves into the returned stream's state and keeps
    /// firing its cancel-on-drop contract for as long as the stream itself is
    /// alive — dropping the stream early (a client disconnecting mid-response,
    /// say) cancels the underlying work exactly as dropping the `Answer`
    /// directly would. Also agrees with `recv`/`try_recv` on ending at the
    /// terminal frame: the `unfold` state carries its own "have we already
    /// yielded a terminal frame" bit rather than sharing `terminal_seen`,
    /// since `into_stream` consumes `self` and nothing else can observe
    /// `Answer`'s copy afterward anyway.
    pub fn into_stream(self) -> impl Stream<Item = Frame<S>> + Send {
        use futures::StreamExt as _;

        let Self { rx, handle, .. } = self;
        futures::stream::unfold(
            (rx.into_stream(), handle, false),
            |(mut rx, handle, terminal_seen)| async move {
                if terminal_seen {
                    return None;
                }
                let frame = rx.next().await?;
                let terminal_seen = terminal_seen || frame.is_terminal();
                Some((frame, (rx, handle, terminal_seen)))
            },
        )
    }

    /// Attach a canceller that fires when this `Answer` (or whatever it is
    /// later converted into, e.g. via [`Answer::into_stream`]) is dropped.
    /// This is how dropping an `Answer` stops real *work* — a spawned task, a
    /// Trustfall walk — and not merely the channel: the channel side of
    /// cancellation ([`Emitter::is_cancelled`]) already falls out for free
    /// from `flume`'s disconnect detection, but nothing makes the *producer's
    /// task* actually stop running without a handle attached here.
    pub fn with_handle(mut self, handle: StreamHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// An already-finished answer carrying exactly one `Frame::End(Summary::complete(0))`.
    /// For trivial `Serve<S>` impls (a static answer, a cache hit already
    /// fully materialized) that need to answer without spawning anything or
    /// holding a live `Emitter` open.
    pub fn empty(generation: Gen) -> Self {
        let (tx, rx) = flume::unbounded();
        // The sender is dropped at the end of this function, so after this one
        // frame is drained the channel reports "disconnected and empty" —
        // `recv`/`try_recv` correctly see exactly one frame and then `None`
        // (via `terminal_seen`, same as the general case).
        let _ = tx.send(Frame::End(Summary::complete(0)));
        Self {
            generation,
            rx,
            handle: None,
            terminal_seen: AtomicBool::new(false),
        }
    }

    /// An already-finished answer carrying exactly one `Frame::Failed(error)`.
    pub fn failed(generation: Gen, error: WireError) -> Self {
        let (tx, rx) = flume::unbounded();
        let _ = tx.send(Frame::Failed(error));
        Self {
            generation,
            rx,
            handle: None,
            terminal_seen: AtomicBool::new(false),
        }
    }
}

/// The producer side of an [`Answer`]. `Send + Sync` (via the underlying
/// `flume::Sender`) so it can be moved into a spawned task — producers are
/// almost always on a different thread from the consumer that awaits the
/// `Answer`.
pub struct Emitter<S: Surface> {
    tx: flume::Sender<Frame<S>>,
    /// Set when a terminal frame has been sent through `tx`, so a later call
    /// on *this* `Emitter` can reject anything further with
    /// [`EmitError::Finished`] instead of queuing frames the paired `Answer`
    /// has already stopped listening for (see [`Answer::recv`]'s doc
    /// comment).
    terminated: AtomicBool,
}

impl<S: Surface> Emitter<S> {
    /// Emit one answer row, tagged with where it came from.
    pub fn item(&self, value: S::Item, residence: Residence) -> Result<(), EmitError> {
        self.push(Frame::Item(Located::new(value, residence)))
    }

    /// Emit surface-specific out-of-band metadata.
    pub fn note(&self, note: S::Note) -> Result<(), EmitError> {
        self.push(Frame::Note(note))
    }

    /// Report that a source dropped out. Does not end the answer — the caller
    /// keeps emitting whatever else it can still produce.
    pub fn degraded(&self, degradation: Degradation) -> Result<(), EmitError> {
        self.push(Frame::Degraded(degradation))
    }

    /// End the answer successfully.
    pub fn end(&self, summary: Summary) -> Result<(), EmitError> {
        self.push(Frame::End(summary))
    }

    /// End the answer with a failure.
    pub fn failed(&self, error: WireError) -> Result<(), EmitError> {
        self.push(Frame::Failed(error))
    }

    /// Whether the [`Answer`] side has been dropped. True once the caller has
    /// stopped listening — search-as-you-type drops the previous `Answer` on
    /// every keystroke, and the abandoned query has to be able to observe
    /// that and actually stop rather than keep doing wasted work.
    pub fn is_cancelled(&self) -> bool {
        self.tx.is_disconnected()
    }

    /// Send an already-built frame.
    ///
    /// Public because a *forwarding* producer — [`merge`], a proxy, a recorder —
    /// relays frames it did not construct and must not have to destructure and
    /// rebuild them to do it. That matters for forward compatibility: `Frame` is
    /// `#[non_exhaustive]`, so a relay that could only re-emit via the typed
    /// constructors would be structurally unable to pass along a variant its
    /// build does not know about, and would silently drop it.
    pub fn push(&self, frame: Frame<S>) -> Result<(), EmitError> {
        // A terminal frame claims the "answer is over" state via `swap`: if it
        // was already `true`, some earlier call already ended the answer, so
        // this second terminal frame is rejected outright rather than sent —
        // exactly `emitting_after_a_terminal_frame_is_rejected`'s "a second
        // terminal frame must be rejected". A non-terminal frame just checks
        // whether that state is already set; either way, once it is, nothing
        // further gets past this point, because `Answer::recv`/`try_recv`
        // stop reading at the first terminal frame regardless of what else is
        // still queued behind it (or could be sent behind it) on the channel.
        if frame.is_terminal() {
            if self.terminated.swap(true, Ordering::AcqRel) {
                return Err(EmitError::Finished);
            }
        } else if self.terminated.load(Ordering::Acquire) {
            return Err(EmitError::Finished);
        }

        // `flume::Sender::send` only ever blocks when the underlying channel
        // is bounded; `answer_channel` deliberately uses an unbounded one (see
        // its own doc comment), so this never stalls the calling thread — a
        // producer must be free to call this from a plain, non-async context
        // (the engine's worker, or synchronously inside `Serve::serve` itself)
        // without risking blocking on a slow or absent consumer. The only
        // failure mode left is the receiver having disconnected, which is
        // exactly `EmitError::Cancelled`.
        self.tx.send(frame).map_err(|_| EmitError::Cancelled)
    }
}

/// Create a fresh [`Emitter`]/[`Answer`] pair for one query at `generation`.
///
/// # Why `capacity` does not bound a blocking channel
///
/// The channel underneath is `flume::unbounded`, not `flume::bounded(capacity)`
/// — deliberately. `Emitter`'s emit methods are plain, non-async functions (see
/// [`Serve::serve`]'s own doc comment on why `serve` itself cannot be `async`:
/// the GUI calls it from the foreground thread and cannot await a
/// constructor), so they must never block the calling thread waiting for a
/// slow or momentarily-absent consumer to make room — a producer stalling the
/// GUI's foreground thread, or an engine worker, on backpressure would be
/// exactly the kind of stall this whole contract exists to design out.
/// `capacity` is kept as an explicit, validated parameter anyway: it states
/// the caller's expected burst size (useful for tuning and for keeping the
/// call site symmetric with `heart::stream`'s bounded-channel precedent), and
/// a future genuinely-bounded backpressure mechanism belongs on the *consumer*
/// side (a slow reader), not by blocking emission.
pub fn answer_channel<S: Surface>(capacity: usize, generation: Gen) -> (Emitter<S>, Answer<S>) {
    assert!(
        capacity > 0,
        "answer_channel capacity must be at least 1, got 0"
    );
    let (tx, rx) = flume::unbounded();
    (
        Emitter {
            tx,
            terminated: AtomicBool::new(false),
        },
        Answer {
            generation,
            rx,
            handle: None,
            terminal_seen: AtomicBool::new(false),
        },
    )
}

// ---------------------------------------------------------------------------
// Serve
// ---------------------------------------------------------------------------

/// Answer requests for one [`Surface`].
///
/// **Object safety is the transparency mechanism** (contract §2). If this
/// trait were not object-safe, a caller could not hold one `Arc<dyn
/// Serve<S>>` that might be a local engine, a remote client, or a federating
/// merge of both — transparency would become a convention enforced by review
/// instead of a property the compiler checks. `serve` deliberately returns
/// `Answer<S>` rather than an associated future or an `impl Future`, and takes
/// no generic parameters beyond the trait's own `S`, so `dyn Serve<S>` is
/// always constructible.
///
/// `serve` is **not** `async`: it must return immediately, with the answer
/// arriving on the returned `Answer`'s channel afterward. The GUI calls this
/// from the foreground thread and cannot await a constructor there.
pub trait Serve<S: Surface>: Send + Sync + 'static {
    /// Begin answering `request` at `generation`. Returns immediately.
    fn serve(&self, request: S::Request, generation: Gen) -> Answer<S>;
}

// ---------------------------------------------------------------------------
// decode_frames
// ---------------------------------------------------------------------------

/// Decode a byte stream into a [`Frame<S>`] stream, one frame per NDJSON
/// line, emitted **the moment its line is complete** — never after the whole
/// body has arrived.
///
/// # The error class this closes
///
/// `heart::client::http`'s `NudoxClient` reads the entire response with
/// `response.text().await` before it decodes a single frame (contract §0.2,
/// §3(c)) — the wire is genuinely an incremental NDJSON stream, but nothing
/// downstream of the socket has ever seen it that way. This function is that
/// missing piece: it reassembles frames across arbitrary chunk boundaries —
/// down to one byte per chunk, since HTTP chunking is a transport artifact
/// and must be invisible to the caller — and yields each frame as soon as its
/// line is complete, so a caller genuinely receives the first hit before the
/// last byte of the response has arrived, instead of only after.
///
/// It also carries forward, unweakened, every terminal-frame guarantee
/// [`crate::stream::StreamFrame`] and [`Frame`] already established for the
/// whole-body case — moving from "parse a complete body" to "parse a live
/// stream" must not lose any of them:
///
/// * A body that ends **without** a terminal [`Frame::End`]/[`Frame::Failed`]
///   — whether that means no bytes at all, a clean break between two lines,
///   or a connection dying mid-line — is truncation, never a short
///   successful answer: a synthesized [`Frame::Failed`] ends the stream
///   instead. An empty body and a body containing only `End` are therefore
///   *not* the same thing and must not collapse into one outcome: the former
///   is a synthesized [`Frame::Failed`], the latter a genuine zero-item
///   [`Frame::End`] the caller actually sent.
/// * A malformed line ends the stream as a typed [`Frame::Failed`], and
///   decoding does not resume after it — the same discipline `heart::stream`
///   applies to a line arriving *after* the terminal frame (a protocol
///   violation), applied here to a line the decoder could not parse at all.
/// * A transport error (`Err` from `body`) ends the stream as a
///   [`Frame::Failed`] rather than silently stopping with whatever partial
///   result had already arrived.
/// * Frames after the first terminal frame are dropped, not emitted — a
///   writer bug must not leak extra rows into the result set.
/// * [`Frame::Degraded`] is **not** terminal; decoding continues past it, the
///   same as every other reader of [`Frame`] in this module.
///
/// A server-sent [`Frame::Failed`] (a *value* the peer chose to send) and a
/// decoder-synthesized one (this function's own verdict that the stream was
/// truncated or malformed) are both [`Frame::Failed`], by design — the caller
/// already treats any `Failed` as "no usable answer", so there is no reader
/// benefit to a second failure shape, only a second thing every reader would
/// have to remember to check.
///
/// # Why this is not behind the `client` feature
///
/// `index` (the server) depends on `heart` *without* `client`, and it is
/// exactly the crate most likely to want to exercise its own NDJSON writer
/// against this reader. Decoding an already-obtained byte stream needs
/// nothing beyond `serde_json`/`bytes`/`futures`, all base dependencies of
/// this crate — only the `reqwest`-backed byte *source* (`heart::client::http`)
/// stays behind `client`.
pub fn decode_frames<S, B, E>(body: B) -> impl Stream<Item = Frame<S>> + Send
where
    S: Surface,
    B: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: fmt::Display + Send + 'static,
{
    use futures::StreamExt as _;

    /// Decoder state threaded through the `unfold`. `queue` exists because one
    /// chunk (or, after reassembly, one buffered run of bytes) can contain
    /// several complete lines at once — `multiple_frames_in_one_chunk_all_decode`
    /// pins exactly this — so each poll first drains whatever was already
    /// parsed before it ever touches `body` again. `done` is set the moment a
    /// terminal frame is *yielded*, mirroring `Answer::terminal_seen`: once
    /// true, every later poll returns `None` immediately without consulting
    /// `body` or `buffer` again, which is what makes "frames after the
    /// terminal frame are dropped" true even when the underlying bytes are
    /// still sitting in `buffer` unexamined.
    struct State<S: Surface, B> {
        body: std::pin::Pin<Box<B>>,
        buffer: BytesMut,
        queue: VecDeque<Frame<S>>,
        done: bool,
    }

    let state = State::<S, B> {
        body: Box::pin(body),
        buffer: BytesMut::new(),
        queue: VecDeque::new(),
        done: false,
    };

    futures::stream::unfold(state, |mut state| async move {
        loop {
            if state.done {
                return None;
            }

            // Drain whatever is already parsed before pulling more bytes —
            // this is what lets several frames sitting in one chunk come out
            // one at a time across several polls, and what lets a frame that
            // arrived before `body` finished ever reach the caller at all.
            if let Some(frame) = state.queue.pop_front() {
                if frame.is_terminal() {
                    state.done = true;
                }
                return Some((frame, state));
            }

            match take_line(&mut state.buffer) {
                Some(line) => {
                    if line.iter().all(u8::is_ascii_whitespace) {
                        // A blank line carries nothing to say — skip it
                        // without consuming a `body` pull or ending anything.
                        continue;
                    }
                    match serde_json::from_slice::<Frame<S>>(&line) {
                        Ok(frame) => state.queue.push_back(frame),
                        Err(err) => {
                            state.queue.push_back(Frame::Failed(WireError::Internal(
                                format!("malformed frame line: {err}"),
                            )));
                            // "Decoding does not resume after a malformed
                            // line": whatever else is already buffered is
                            // discarded rather than parsed, matching the
                            // terminal-frame rule this failure is about to
                            // enforce on itself.
                            state.buffer.clear();
                        }
                    }
                }
                None => match state.body.next().await {
                    Some(Ok(bytes)) => state.buffer.extend_from_slice(&bytes),
                    Some(Err(err)) => {
                        state.queue.push_back(Frame::Failed(WireError::Internal(
                            format!("transport error: {err}"),
                        )));
                        state.buffer.clear();
                    }
                    None => {
                        // `body` is exhausted and `buffer` holds no complete
                        // line. A well-formed stream never reaches this arm —
                        // its terminal frame would already have set `done` in
                        // the queue-drain branch above and short-circuited
                        // every later poll. Reaching here means the peer
                        // stopped talking without saying it was done: either
                        // cleanly between two lines (nothing left in
                        // `buffer`) or mid-line (a partial line still sits in
                        // it). Both are truncation — never a silent, short
                        // "success".
                        let reason = if state.buffer.is_empty() {
                            "stream ended with no terminal frame".to_owned()
                        } else {
                            "stream ended mid-line: trailing partial frame".to_owned()
                        };
                        state
                            .queue
                            .push_back(Frame::Failed(WireError::Internal(reason)));
                        state.buffer.clear();
                    }
                },
            }
        }
    })
}

/// Pull one complete `\n`-terminated line out of `buffer`, if one is fully
/// present, leaving any remainder (a partial next line) in place. The
/// trailing `\n` — and a `\r` immediately before it, for a peer that writes
/// CRLF — is stripped from the returned line; NDJSON writers in this codebase
/// emit bare `\n` (see `heart::stream`'s writer and this module's own tests),
/// but a reader costs nothing by tolerating the other common convention too.
fn take_line(buffer: &mut BytesMut) -> Option<Bytes> {
    let pos = buffer.iter().position(|&b| b == b'\n')?;
    let mut line = buffer.split_to(pos + 1);
    line.truncate(pos);
    if line.last() == Some(&b'\r') {
        line.truncate(line.len() - 1);
    }
    Some(line.freeze())
}

mod merge_impl;
mod surfaces;

pub use merge_impl::{MergePump, merge, merge_bounded};
pub use surfaces::{Packages, SearchNote, Symbols, UsageHit, Usages};

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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

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

    /// Resolve two copies of one identity (same [`Surface::key`]) into the
    /// single row a consumer should hold.
    ///
    /// # The error class this closes
    ///
    /// [`merge`]'s dedup rule was precedence-wins-wholesale: when a
    /// higher-precedence source's copy of an already-claimed key arrives, it
    /// *replaces* the row outright. That is correct when the two copies are
    /// rival **versions** of a record — the newer one really should win
    /// completely. It is wrong when the two copies are the same record at two
    /// different **fidelities**: if a lower-precedence source already
    /// answered with an enriched copy (say, a rendered signature) and the
    /// higher-precedence source's copy is merely *bare*, wholesale
    /// replacement throws the enrichment away and the consumer sees a row get
    /// visibly *worse* mid-query — the richer copy was already painted on
    /// screen. `hit_fusion.rs`'s `a_merged_duplicate_is_fused_not_merely_
    /// replaced` is the end-to-end pin for exactly this case, with `Symbols`
    /// as the motivating surface (a bare local hit superseding a signed
    /// remote one must keep the signature).
    ///
    /// # Why the default is precedence-wins, unchanged
    ///
    /// This method has to exist on every [`Surface`], but most surfaces have
    /// no notion of "fidelity" distinct from "which source answered" — for
    /// those, precedence-wins *is* correct, and was already the only
    /// behaviour before this hook existed. Defaulting to `winner` verbatim
    /// means adding this method changes nothing for any surface that does not
    /// deliberately override it — [`Packages`] and [`Usages`] included, both
    /// of which use the default today. Only a surface whose `Item` can
    /// genuinely arrive at different fidelities (today, only [`Symbols`],
    /// because of `SymbolHit::signature`) has a reason to override it.
    ///
    /// # Contract
    ///
    /// An override must be **idempotent**: `fuse(fuse(a, a), fuse(a, a)) ==
    /// fuse(a, a)` for any `a`. [`merge`] may call this repeatedly as
    /// duplicates keep arriving for the same key over the life of one query,
    /// so a non-idempotent override could oscillate a consumer's row instead
    /// of converging on a stable, maximally-enriched value. An override must
    /// also never let `loser` win a field `winner` can already supply on its
    /// own — fusion is precedence-wins *plus backfill*, not a free-for-all
    /// field-by-field merge; see [`Symbols::fuse`]'s own doc comment for the
    /// concrete rule it applies.
    fn fuse(winner: Self::Item, _loser: Self::Item) -> Self::Item {
        winner
    }
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
    ///
    /// # This is an upsert keyed by [`Surface::key`], not an append
    ///
    /// A consumer **must** hold answer rows in a structure keyed by
    /// `S::key(&item)` and *replace* on a repeat key. It must not push every
    /// `Item` frame onto a list.
    ///
    /// Receiving the same key twice is normal, not a producer bug. Delivery is
    /// progressive (rows paint as they arrive, with no global sort), so a
    /// higher-precedence source's copy of a key routinely arrives *after* a
    /// lower-precedence one has already been painted. [`merge`] handles that by
    /// fusing the two copies via [`Surface::fuse`] and **re-emitting** the fused
    /// row, precisely so the consumer can replace what is already on screen —
    /// see `merge_impl.rs`'s supersede arm, and
    /// `a_higher_precedence_duplicate_arriving_late_supersedes` in
    /// `tests/merge.rs`. Refusing to re-emit would strand a stale copy on screen
    /// forever, so the re-emit is deliberately not budget-checked either.
    ///
    /// The failure mode if a consumer appends instead is the exact bug this
    /// whole contract exists to prevent: **the same symbol rendered twice, once
    /// per plane, with nothing erroring**. It is also intermittent, because
    /// whether a supersede happens at all depends on which source answered
    /// first — so it will pass in a test run and fail in front of a user.
    ///
    /// Note that this makes [`Summary::items`] a count of *frames emitted*, not
    /// of distinct rows; a merge that superseded twice reports more `items` than
    /// the consumer holds keys. Page limits, by contrast, count **distinct
    /// keys** (`merge_bounded`), because a supersede replaces a row rather than
    /// adding one.
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
/// do). Three genuinely distinct causes exist, and they mean different things
/// at a call site — collapsing any two into one variant would make a caller's
/// `match` lie about which situation it is actually in:
///
/// * [`EmitError::Cancelled`] — the *consumer* went away. Stop doing the work
///   that produces frames; nothing will ever read another one.
/// * [`EmitError::Finished`] — *this answer itself* is already over. The
///   consumer may be alive and well; whatever was about to be emitted just
///   arrived too late to matter.
/// * [`EmitError::Lagged`] — neither of the above. The consumer is still
///   there and the answer is still open; it is simply behind, and
///   [`answer_channel`]'s bound (`workspace/heart/surface.rs:845`) was
///   reached. A producer must not treat this like `Cancelled` (the consumer
///   has not gone anywhere — stopping work would turn a slow reader into a
///   truncated answer for no reason) or like `Finished` (the answer has not
///   ended — a later emit past the moment of lag can, and routinely does,
///   succeed again once the consumer drains; see
///   `a_lagged_emitter_recovers_when_the_consumer_drains` in
///   `tests/backpressure.rs`). It is only ever returned by [`Emitter::push`]
///   and its non-async wrappers (`item`/`note`) — [`Emitter::push_async`]
///   never returns it, because an async producer parks for room instead of
///   dropping the frame (`tests/backpressure.rs` §7).
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
    /// A [`Frame::Item`] or [`Frame::Note`] was dropped because the channel
    /// had already reached [`answer_channel`]'s `capacity` — the consumer is
    /// behind, not gone. This is what makes `push`/`item`/`note` safe to call
    /// from a plain, non-async context (`Serve::serve` itself, an engine
    /// worker with no reactor): rather than blocking the calling thread until
    /// room appears (which would stall lindsey's foreground thread — see this
    /// module's own doc comment on why `flume::Receiver` was chosen and
    /// `answer_channel`'s doc comment on the two emit paths), the frame is
    /// dropped and counted, and the drop is folded into `end`'s summary
    /// automatically (see [`Emitter::end`]) rather than left for the producer
    /// to notice and report itself. A producer with a runtime available
    /// should prefer [`Emitter::push_async`]/[`Emitter::item_async`], which
    /// park instead of dropping and so never return this variant.
    #[error("the answer's consumer is behind; this frame was dropped rather than blocking")]
    Lagged,
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
        // A small bounded channel, not `flume::unbounded()`: exactly one frame
        // is ever sent here, so there is nothing for a large bound to buy and
        // no reason for this constructor to be the one place in the module
        // that does not go through a bounded channel (see `answer_channel`'s
        // doc comment for why every other channel in this module is bounded).
        let (tx, rx) = flume::bounded(1);
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
        let (tx, rx) = flume::bounded(1);
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
    /// The `capacity` [`answer_channel`] was built with — the point at which
    /// [`Emitter::push`] starts refusing [`Frame::Item`]/[`Frame::Note`] with
    /// [`EmitError::Lagged`], strictly below the channel's physical size
    /// (`capacity + RESERVE`; see `answer_channel`'s doc comment). Stored here
    /// rather than read back off `tx.capacity()` so the gate and the reserve
    /// stay two clearly separate numbers in the code, matching how this
    /// module's docs (and `tests/backpressure.rs`) talk about them.
    capacity: usize,
    /// Set when a terminal frame has been sent through `tx`, so a later call
    /// on *this* `Emitter` can reject anything further with
    /// [`EmitError::Finished`] instead of queuing frames the paired `Answer`
    /// has already stopped listening for (see [`Answer::recv`]'s doc
    /// comment).
    terminated: AtomicBool,
    /// Count of [`Frame::Item`]s that actually made it onto `tx`. Read by
    /// [`Emitter::end`] to decide whether the producer's own [`Summary`] can
    /// be trusted verbatim — see that method's doc comment.
    delivered: AtomicU64,
    /// Count of [`Frame::Item`]/[`Frame::Note`] frames refused by the
    /// `capacity` gate in [`Emitter::push`]. Non-zero here is what turns
    /// `end`'s summary dishonest if left unchecked — see [`Emitter::end`].
    dropped: AtomicU64,
}

impl<S: Surface> Emitter<S> {
    /// Emit one answer row, tagged with where it came from. Non-blocking: on a
    /// full channel the row is dropped and this returns
    /// [`EmitError::Lagged`] rather than parking the calling thread — see
    /// [`answer_channel`]'s doc comment for why, and
    /// [`Emitter::item_async`] for the alternative that never drops.
    pub fn item(&self, value: S::Item, residence: Residence) -> Result<(), EmitError> {
        self.push(Frame::Item(Located::new(value, residence)))
    }

    /// The async twin of [`Emitter::item`]: awaits room instead of dropping
    /// the row on a full channel, so a producer with a runtime (a spawned
    /// task, not `Serve::serve` itself — see [`Emitter::push_async`]'s doc
    /// comment) gets real backpressure and never loses an item.
    pub async fn item_async(&self, value: S::Item, residence: Residence) -> Result<(), EmitError> {
        self.push_async(Frame::Item(Located::new(value, residence))).await
    }

    /// Emit surface-specific out-of-band metadata. Shares `item`'s budget and
    /// its non-blocking, may-drop-and-report behaviour — see
    /// `notes_are_bounded_by_the_same_budget` in `tests/backpressure.rs`.
    pub fn note(&self, note: S::Note) -> Result<(), EmitError> {
        self.push(Frame::Note(note))
    }

    /// The async twin of [`Emitter::note`] — see [`Emitter::item_async`].
    pub async fn note_async(&self, note: S::Note) -> Result<(), EmitError> {
        self.push_async(Frame::Note(note)).await
    }

    /// Report that a source dropped out. Does not end the answer — the caller
    /// keeps emitting whatever else it can still produce. Not gated by
    /// `capacity`: a degradation notice may spend [`answer_channel`]'s
    /// reserve, the same as a terminal frame, because a consumer that fell
    /// behind has the strongest claim on being told *why* the answer it is
    /// about to receive is incomplete.
    pub fn degraded(&self, degradation: Degradation) -> Result<(), EmitError> {
        self.push(Frame::Degraded(degradation))
    }

    /// End the answer successfully.
    ///
    /// # Honesty is structural, not the producer's job
    ///
    /// If this `Emitter` has dropped anything (any [`EmitError::Lagged`]
    /// returned by `push`/`item`/`note` along the way), `summary` is
    /// discarded and rewritten as [`Summary::partial`] over exactly what was
    /// delivered — even if the producer explicitly asked to report
    /// [`Completeness::Complete`]. A producer cannot know, at the call site
    /// that built `summary`, how many of its own earlier emits were silently
    /// dropped by a full channel; letting it report `Complete` anyway would
    /// make a truncated answer indistinguishable from a whole one, which is
    /// exactly the failure this module's whole backpressure design exists to
    /// avoid (see `tests/backpressure.rs`'s "A truncated answer cannot claim
    /// to be complete" section). When nothing was dropped, `summary` is sent
    /// verbatim, `Complete` included — this override only ever makes an
    /// answer *more* honest than what the producer supplied, never less.
    pub fn end(&self, summary: Summary) -> Result<(), EmitError> {
        let dropped = self.dropped.load(Ordering::Acquire);
        let summary = if dropped == 0 {
            summary
        } else {
            let delivered = self.delivered.load(Ordering::Acquire);
            Summary::partial(delivered, delivered, Some(delivered + dropped))
        };
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

    /// Send an already-built frame. Non-blocking — see [`Emitter::push_async`]
    /// for the awaiting twin.
    ///
    /// Public because a *forwarding* producer — [`merge`], a proxy, a recorder —
    /// relays frames it did not construct and must not have to destructure and
    /// rebuild them to do it. That matters for forward compatibility: `Frame` is
    /// `#[non_exhaustive]`, so a relay that could only re-emit via the typed
    /// constructors would be structurally unable to pass along a variant its
    /// build does not know about, and would silently drop it.
    ///
    /// # Why this drops instead of blocking
    ///
    /// `answer_channel` builds a *bounded* channel now, but `push` still must
    /// never block the calling thread: `Serve::serve` is called from
    /// lindsey's foreground thread, which has no Tokio reactor to park on
    /// (see `Serve::serve`'s own doc comment), so a blocking send on a full
    /// channel there would stall the GUI rather than merely lose a frame —
    /// see this file's top-of-module `# Why the fix is not "make it
    /// flume::bounded"` reasoning and `push_never_blocks_the_calling_thread`
    /// in `tests/backpressure.rs`. So instead: [`Frame::Item`]/[`Frame::Note`]
    /// are gated at `capacity` and refused with [`EmitError::Lagged`] once it
    /// is reached, dropped rather than queued; [`Frame::Degraded`] and the
    /// terminal frames are *not* gated and may spend `answer_channel`'s
    /// reserve, because an answer must always be able to end and always be
    /// able to say it degraded, no matter how far behind the consumer fell
    /// (`a_terminal_frame_fits_even_when_the_channel_is_saturated` /
    /// `a_degradation_fits_even_when_the_channel_is_saturated`). `try_send`
    /// (never `send`) is used throughout for exactly this reason: this
    /// function must return immediately either way.
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

        // Items and notes are gated at `capacity`, checked *before* attempting
        // the send at all, so a producer that is already over budget never
        // even touches the reserve `Frame::Degraded`/terminal frames rely on
        // (`items_cannot_spend_the_terminal_reserve`). `tx.len()` racing a
        // concurrent sender can only ever admit a small handful more than
        // `capacity` before the reserve absorbs it — a benign off-by-one, not
        // a correctness issue (see `answer_channel`'s doc comment).
        let gated = matches!(frame, Frame::Item(_) | Frame::Note(_));
        if gated && self.tx.len() >= self.capacity {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return Err(EmitError::Lagged);
        }

        let is_item = matches!(frame, Frame::Item(_));
        match self.tx.try_send(frame) {
            Ok(()) => {
                if is_item {
                    self.delivered.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            }
            Err(flume::TrySendError::Disconnected(_)) => Err(EmitError::Cancelled),
            Err(flume::TrySendError::Full(_)) => {
                // Only reachable by losing a race against concurrent senders
                // between the length check above and this send, or — for an
                // ungated `Degraded`/terminal frame — by finding the reserve
                // itself spent by concurrent degradation notices. `push` must
                // never block (see this method's own doc comment), so this is
                // reported the same way the gate above reports it rather than
                // parking for room.
                if gated {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(EmitError::Lagged)
            }
        }
    }

    /// The async twin of [`Emitter::push`]: awaits room on a full channel
    /// instead of dropping the frame. This is the other half of the two-path
    /// policy `answer_channel`'s doc comment describes — for a producer that
    /// *does* have a runtime to park on (a spawned task, not `Serve::serve`
    /// itself, which must return immediately with no reactor guaranteed to
    /// exist), real backpressure with no data loss is strictly better than
    /// `push`'s drop-and-report.
    ///
    /// The already-ended check happens *before* parking, synchronously, the
    /// same as `push` — awaiting must not become a way to sneak a frame past
    /// a finished answer (`push_async_is_rejected_after_a_terminal_frame`).
    /// Resolves `Ok(())` once the frame is actually sent, or
    /// [`EmitError::Cancelled`] if the paired [`Answer`] is dropped while this
    /// call is parked (`push_async_returns_cancelled_when_the_answer_is_dropped`
    /// — this must not hang). Never returns [`EmitError::Lagged`]: there is
    /// nothing to drop when a full channel means "wait", not "give up".
    pub async fn push_async(&self, frame: Frame<S>) -> Result<(), EmitError> {
        if frame.is_terminal() {
            if self.terminated.swap(true, Ordering::AcqRel) {
                return Err(EmitError::Finished);
            }
        } else if self.terminated.load(Ordering::Acquire) {
            return Err(EmitError::Finished);
        }

        // Item/note admission has to be parked at `capacity`, not at the
        // channel's full physical size (`capacity + RESERVE`) — otherwise an
        // async producer would happily fill the reserve with items, and the
        // reserve would already be spent by the time a terminal frame needed
        // it (exactly the failure `items_cannot_spend_the_terminal_reserve`
        // pins for the sync path). `flume` has no "wait for room below a
        // custom watermark" primitive, only "wait for room in the queue at
        // all" — so this polls `tx.len()` against `capacity` and yields to
        // the runtime between checks, giving the consumer (and flume's own
        // wakeups on the physical channel) a chance to make progress. This is
        // deliberately a spin-yield rather than a fixed sleep: it must react
        // the instant the consumer frees a slot, not after some polling
        // delay, so `push_async_parks_the_producer_until_the_consumer_drains`
        // sees the producer resume promptly rather than in coarse steps.
        let gated = matches!(frame, Frame::Item(_) | Frame::Note(_));
        if gated {
            while self.tx.len() >= self.capacity {
                if self.tx.is_disconnected() {
                    return Err(EmitError::Cancelled);
                }
                tokio::task::yield_now().await;
            }
        }

        let is_item = matches!(frame, Frame::Item(_));
        match self.tx.send_async(frame).await {
            Ok(()) => {
                if is_item {
                    self.delivered.fetch_add(1, Ordering::Relaxed);
                }
                Ok(())
            }
            // The receiver — the paired `Answer`, or whatever it was
            // converted into — was dropped while this send was parked
            // waiting for room. `flume::Sender::send_async` resolves an `Err`
            // the instant that happens rather than parking forever, which is
            // what keeps this safe to `.await` unconditionally: a consumer
            // going away can never leak the task awaiting this call.
            Err(_) => Err(EmitError::Cancelled),
        }
    }
}

/// Create a fresh [`Emitter`]/[`Answer`] pair for one query at `generation`.
///
/// # Two emit paths for two kinds of producer
///
/// `capacity` is a real bound now: the channel underneath is
/// `flume::bounded(capacity + RESERVE)`, not `flume::unbounded()`. A producer
/// that outruns its consumer used to grow this channel without limit — every
/// call site *stated* a capacity and nothing enforced it — until the process
/// ran out of memory. That is fixed by giving emission exactly two paths,
/// because there are genuinely two kinds of producer:
///
/// * [`Emitter::push`]/[`Emitter::item`]/[`Emitter::note`] stay plain,
///   non-async functions that never block the calling thread. On a full
///   channel the frame is dropped and [`EmitError::Lagged`] is returned. This
///   is the only option for a producer with no runtime to park on —
///   [`Serve::serve`] is called from lindsey's foreground thread, which has
///   no Tokio reactor, so a blocking send there would stall the GUI rather
///   than merely lose a frame (see this module's top-of-file `# Why the fix
///   is not "make it flume::bounded"` section).
/// * [`Emitter::push_async`]/[`Emitter::item_async`]/[`Emitter::note_async`]
///   await room instead, for a producer that *does* have a runtime (a
///   spawned task: the merge pump, the server's search tasks, the remote
///   client's fetch loop) — real backpressure, no data loss, the stall
///   propagating all the way back to whatever is producing too fast.
///
/// # The reserve
///
/// The channel is built with `RESERVE` frames of headroom *above* `capacity`,
/// and only [`Frame::Item`]/[`Frame::Note`] are gated at `capacity` — a
/// terminal frame or a [`Frame::Degraded`] may spend the reserve. Without
/// this, a channel saturated with items could not deliver its own `End`, and
/// the fix for the memory leak would just be a new way to hang forever
/// waiting for a terminal frame that can structurally never arrive
/// (`a_terminal_frame_fits_even_when_the_channel_is_saturated` in
/// `tests/backpressure.rs`). `RESERVE = 2` covers the one terminal frame
/// every answer ends with plus one concurrent degradation notice; a producer
/// that piles up more `Degraded` frames than that before ending is currently
/// unheard of in this codebase (`merge` sends at most one per source, and
/// only once).
pub fn answer_channel<S: Surface>(capacity: usize, generation: Gen) -> (Emitter<S>, Answer<S>) {
    assert!(
        capacity > 0,
        "answer_channel capacity must be at least 1, got 0"
    );
    /// Headroom above `capacity` reserved for terminal/`Degraded` frames —
    /// see this function's own doc comment.
    const RESERVE: usize = 2;
    let (tx, rx) = flume::bounded(capacity + RESERVE);
    (
        Emitter {
            tx,
            capacity,
            terminated: AtomicBool::new(false),
            delivered: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
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

mod capabilities;
mod federated;
mod hit;
mod merge_impl;
mod routed;
mod surfaces;

pub use capabilities::{CAPABILITIES_PATH, Capabilities, PROTOCOL_VERSION, SurfaceId};
pub use federated::Federated;
pub use routed::Routed;
pub use hit::{SigToken, Signature, SymbolHit};
pub use merge_impl::{MergePump, Timer, merge, merge_bounded, merge_with_deadline};
pub use surfaces::{Packages, SearchNote, Symbols, UsageHit, Usages};

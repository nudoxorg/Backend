//! `SearchStore` — GUI-PLAN §12.3, `docs/LOCAL-REMOTE-CONTRACT.md` §2 (task S4).
//!
//! ## The one federated search path
//!
//! There is no local call path and a separate remote one. `trigger_search`
//! builds a `heart::surface::Federated<Symbols>` over the local engine (always)
//! and a remote `RemoteClient` (when `NUDOX_SERVER_URL` is configured), calls
//! its one `serve`, and drains the merged `Answer<Symbols>` — see
//! [`SearchStore::apply_frame`] for the consumption rule (upsert, never
//! append) and why every hit lands in one section.
//!
//! ## State ownership
//!
//! - `input`, `scope`, `mode` — raw user intent; mutated synchronously.
//! - `sections: [SectionBuf; 3]` — render-ready `Arc<[PreparedRow]>`, though
//!   only `sections[0]` is ever populated today (`SectionBuf`'s own doc
//!   comment); updated once per drained batch, never in render (§1.1.4).
//! - `raw_rows` — the upsert buffer `sections[0].rows` is rebuilt from.
//! - `selection: Option<Cursor>` — typed cursor; the §15 "never wraps silently
//!   across sections" invariant is checkable here, not inferred from a flat int.
//! - `gens: GenSource` — per-slot monotonic counter.
//! - `generation: u64` — current generation counter (mirrors `slot.generation`).
//! - `gen_arrival: Option<Instant>` — when the current gen's first events landed;
//!   controls entrance-animation windows (§4.1).
//! - `offline: bool` — whether the current answer is degraded (some source
//!   did not answer; local-first rows already collected still render).
//! - `debounce_task: Option<Task<()>>` — the 24 ms foreground timer task;
//!   owned here (LD-18), replaced on each keystroke, which drops and cancels
//!   the predecessor.
//! - `search_task: Task<()>` — drains the federated `Answer<Symbols>`; owns it,
//!   so replacing this task drops the `Answer` and cancels every source.
//!
//! ## The §7.4 shape, applied
//!
//! `trigger_search` is the only place that issues a query. It follows the
//! four-line §7.4 pattern exactly. `debounce_task` is the 24 ms wrapper that
//! delays calling `trigger_search`.

use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long one search may wait on a source that never terminates.
///
/// Far longer than any healthy search, far shorter than "never". See
/// `Federated::with_deadline` — it bounds waiting, not delivering: rows already
/// received are kept and a source that finishes in time is untouched.
const SEARCH_DEADLINE: Duration = Duration::from_secs(10);

use gpui::{Context, SharedString, Task};
use heart::client::RemoteClient;
use heart::surface::{
    Federated, Frame, Located, MergePump, SearchNote, Serve, Surface as _, SymbolHit, Symbols,
};
use heart::{PageSpecification, Query, QueryMode, RankSpecification, Routing, Scope, Scored, Target};

use crate::bridge::generation::GenSource;
use crate::stores::events::{OpenDisposition, OpenSymbol};
use crate::stores::search_model::{
    Cursor, PreparedRow, SECTION_COUNT, ScopeChip, SearchMode, SearchSnapshot, SectionData,
    SectionStatus,
};

// ---------------------------------------------------------------------------
// Remote (`heart::client::RemoteClient`) federation source
// ---------------------------------------------------------------------------

/// Env var naming the remote `nudox-serve` base URL. Unset/unparseable → no
/// remote source is added to the federation, and `SearchStore` becomes a
/// federation of exactly one (the local engine) — a passthrough, not a
/// special case (`heart::surface::Federated`'s own doc comment).
const NUDOX_SERVER_URL_ENV: &str = "NUDOX_SERVER_URL";

/// Matches `ServerConfiguration::default().serving_address`
/// (`workspace/index/server/config.rs`) — the single-node dev default, so a
/// `nudox-serve` started with no config at all is reachable with no GUI
/// config either.
const NUDOX_SERVER_URL_DEFAULT: &str = "http://127.0.0.1:8080";

/// Build the remote federation source from `NUDOX_SERVER_URL` (default
/// [`NUDOX_SERVER_URL_DEFAULT`]). Parsing a URL and constructing a
/// `RemoteClient` does no I/O, so this is synchronous and safe to call from
/// [`SearchStore::new`].
fn remote_source_from_env() -> Option<Arc<dyn Serve<Symbols>>> {
    let base =
        std::env::var(NUDOX_SERVER_URL_ENV).unwrap_or_else(|_| NUDOX_SERVER_URL_DEFAULT.to_owned());
    let url = url::Url::parse(&base).ok()?;
    let client = RemoteClient::new(url).ok()?;
    Some(Arc::new(client) as Arc<dyn Serve<Symbols>>)
}

/// Wraps a remote federation source so its `Serve::serve` call runs with the
/// engine's Tokio runtime entered.
///
/// # Why this exists
///
/// `heart::client::RemoteClient::serve` — the blanket `Serve<S>` impl the
/// federation relies on — spawns its HTTP-streaming pump with a bare
/// `tokio::spawn(pump::<S>(...))`. That call needs a Tokio runtime entered on
/// whatever thread calls it (`Handle::current()` must resolve), and lindsey
/// links none of its own (LR-9: "the engine owns every runtime and lindsey
/// links none"). Calling `RemoteClient::serve` directly from `Federated::serve`
/// — itself called synchronously from `trigger_search`, on GPUI's own thread —
/// would panic with the same "there is no reactor running" failure the old
/// `search_remote_inner` worked around by building a throwaway runtime per
/// request (deleted along with it; see this module's own doc comment).
///
/// `EngineHandle::runtime_handle` is the sanctioned alternative to building a
/// second runtime: "hosts borrow this runtime, they do not build one" (that
/// method's own doc comment). Entering it for the duration of one synchronous
/// `serve` call — which is exactly when the `tokio::spawn` happens — gives
/// `RemoteClient`'s pump a valid context without lindsey ever constructing a
/// runtime of its own; the spawned task then runs on the engine's own worker
/// pool, the same as every other piece of async work the engine hands out
/// (`open_symbol`, `search`, …).
struct EngineScopedRemote {
    engine: nudox_engine::EngineHandle,
    remote: Arc<dyn Serve<Symbols>>,
}

impl Serve<Symbols> for EngineScopedRemote {
    fn serve(&self, request: Query, generation: heart::surface::Gen) -> heart::surface::Answer<Symbols> {
        // Named, not chained: `EnterGuard` borrows from the handle, so the
        // handle needs a binding that outlives it in this scope — chaining
        // `.runtime_handle().enter()` would drop the handle (a temporary)
        // while the guard still borrowed it.
        let handle = self.engine.runtime_handle();
        let _entered = handle.enter();
        self.remote.serve(request, generation)
    }
}

/// The 24 ms debounce interval (§12.3).
const DEBOUNCE_MS: u64 = 24;

// ---------------------------------------------------------------------------
// Search scope
// ---------------------------------------------------------------------------

/// Below this, a search's duration is not reported.
///
/// 100 ms is Nielsen's first response-time limit — "the limit for having the
/// user feel that the system is reacting instantaneously". A duration under it
/// describes an event the reader did not perceive, so printing it puts a number
/// on the screen that can only ever be noise. See the `Latency` arm of
/// `apply_event` for the defect this removed.
const PERCEPTIBLE_LATENCY_MS: u128 = 100;

/// Restricts the search to a subset of the corpus.
///
/// `kinds` is not wired into `heart::Query` yet — that request type has no
/// kind-filter field (only `scope.packages`/`scope.ecosystems`), the same gap
/// `SearchMode`'s own doc comment already documents for the mode chips. Kept
/// here rather than deleted so the scope-chip UI has somewhere to land its
/// state once the query algebra grows the field; toggling it today changes
/// nothing observable, honestly (no query is re-shaped by a value that goes
/// nowhere).
#[derive(Clone, Debug, Default)]
pub struct SearchScope {
    /// When `Some`, restrict to these package coordinates.
    pub packages: Vec<SharedString>,
    /// Kind filter — empty means "all kinds".
    pub kinds: Vec<nudox_engine::wire::KindTag>,
}

// ---------------------------------------------------------------------------
// Internal section buffer (store-side, pre-converted)
// ---------------------------------------------------------------------------

/// One section's render-ready state, owned by the store.
///
/// Only `sections[0]` (Name) is ever populated today: `heart::surface::Symbols`
/// (what `Federated::serve` answers) carries no per-item section — see
/// `SearchStore::apply_frame`'s own doc comment. `sections[1]`/`[2]` (Type,
/// Semantic) stay `Idle` and empty; `Section`/`SECTION_COUNT`/`Cursor` are
/// otherwise untouched so cmd-1/2/3 and the rest of the selection model keep
/// working exactly as before over however many sections actually hold rows.
#[derive(Clone, Debug)]
struct SectionBuf {
    /// Render-ready rows. Cloning is a refcount bump.
    rows: Arc<[PreparedRow]>,
    /// Pre-formatted latency string (e.g. `"4 ms"`). Empty until reported.
    latency: SharedString,
    /// Current phase, for `SectionData::status`.
    status: SectionStatus,
}

impl SectionBuf {
    fn empty() -> Self {
        Self {
            rows: Arc::from([] as [PreparedRow; 0]),
            latency: SharedString::default(),
            status: SectionStatus::Idle,
        }
    }

    fn into_section_data(&self) -> SectionData {
        SectionData {
            rows: self.rows.clone(),
            status: self.status,
            latency: self.latency.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// SearchStore
// ---------------------------------------------------------------------------

/// `SearchStore` — GUI-PLAN §12.3.
///
/// # Why this is not generic over the section it populates
///
/// It still is generic — over `E`, the local engine capability — but only at
/// [`SearchStore::new`]: `E: Serve<Symbols>` is erased into `Arc<dyn
/// Serve<Symbols>>` immediately, the same "transparency mechanism" every
/// `Serve<S>` caller in the system relies on (`heart::surface::Serve`'s own
/// doc comment). Nothing past construction cares whether the local source is
/// a real `EngineHandle` or a test double.
pub struct SearchStore {
    // ── User intent ─────────────────────────────────────────────────────
    pub input: SharedString,
    pub scope: SearchScope,
    pub mode: SearchMode,

    // ── Section storage (render-ready, §12 / §1.1.4) ──────────────────────
    /// Per-section render-ready state. Only `sections[0]` (Name) is ever
    /// populated — see [`SectionBuf`]'s own doc comment.
    sections: [SectionBuf; SECTION_COUNT],
    /// Upsert buffer backing `sections[0].rows`, keyed implicitly by
    /// `Symbols::key`. A repeat key from `Frame::Item` replaces its slot in
    /// place rather than appending — see [`SearchStore::apply_frame`].
    raw_rows: Vec<Located<Scored<SymbolHit>>>,

    // ── Generation tracking ────────────────────────────────────────────────
    pub gens: GenSource,
    /// Current generation (echoed into snapshots for entrance-animation keying).
    generation: u64,
    /// When the current generation's first page landed.
    gen_arrival: Option<Instant>,
    /// Whether the current answer is degraded — see [`SearchSnapshot::offline`]'s
    /// own doc comment.
    offline: bool,

    // ── Selection ─────────────────────────────────────────────────────────
    /// Cursor into `sections`.  `Cursor { section, row }` makes the §15
    /// "never wraps silently" invariant checkable.
    pub selection: Option<Cursor>,

    // ── Task ownership (LD-18) ────────────────────────────────────────────
    /// The 24 ms debounce timer. Replaced per keystroke; dropping cancels it.
    debounce_task: Option<Task<()>>,
    /// Drains the current federated `Answer<Symbols>`. Replacing this task
    /// drops the `Answer` it holds — which cancels every source in the
    /// federation, local and remote alike (`Federated::serve`'s doc comment on
    /// `dropping_the_answer_cancels_every_source`), the same way dropping
    /// `stream_handle` used to cancel the old single-source stream.
    search_task: Task<()>,

    // ── The one federated search path (§2, task S4) ─────────────────────────
    /// The local source. Always present, always first in precedence —
    /// `nudox_engine`'s `Serve<Symbols>` adapter, erased once at construction.
    local: Arc<dyn Serve<Symbols>>,
    /// The remote source, when `NUDOX_SERVER_URL` parses to a reachable
    /// client — see [`remote_source_from_env`]. `None` here is not a branch
    /// callers have to think about: [`SearchStore::sources`] just omits it
    /// from the `Vec` handed to [`Federated::new`], and a federation of one is
    /// a passthrough (pinned by `heart/tests/federated.rs::
    /// a_federation_of_one_is_a_passthrough`).
    remote: Option<Arc<dyn Serve<Symbols>>>,
}

impl SearchStore {
    /// Construct against the real engine — the only production path.
    ///
    /// Builds the federation — local always, remote when `NUDOX_SERVER_URL`
    /// parses to a reachable client (see [`remote_source_from_env`]) — and
    /// wraps the remote source in [`EngineScopedRemote`] so its `Serve::serve`
    /// call runs with the engine's Tokio runtime entered. See that type's own
    /// doc comment for why this is necessary and why it does not violate LR-9.
    pub fn new(engine: nudox_engine::EngineHandle) -> Self {
        let remote = remote_source_from_env().map(|remote| {
            Arc::new(EngineScopedRemote {
                engine: engine.clone(),
                remote,
            }) as Arc<dyn Serve<Symbols>>
        });
        Self::from_parts(Arc::new(engine), remote)
    }

    /// Construct with an arbitrary local source and no remote. Test-only: a
    /// production host always has a real `EngineHandle` and goes through
    /// [`SearchStore::new`], which also wires up [`EngineScopedRemote`] — a
    /// generic local-only constructor would make it easy to accidentally ship
    /// a store whose remote source (if any were added generically) skips the
    /// runtime-entering wrapper and panics the first time it actually runs.
    #[cfg(test)]
    fn for_local<E: Serve<Symbols>>(local: E) -> Self {
        Self::from_parts(Arc::new(local), None)
    }

    fn from_parts(local: Arc<dyn Serve<Symbols>>, remote: Option<Arc<dyn Serve<Symbols>>>) -> Self {
        Self {
            input: SharedString::default(),
            scope: SearchScope::default(),
            mode: SearchMode::default(),
            sections: [
                SectionBuf::empty(),
                SectionBuf::empty(),
                SectionBuf::empty(),
            ],
            raw_rows: Vec::new(),
            gens: GenSource::new(),
            generation: 0,
            gen_arrival: None,
            offline: false,
            selection: None,
            debounce_task: None,
            search_task: Task::ready(()),
            local,
            remote,
        }
    }

    /// The federation sources, in precedence order (local first) — built
    /// fresh on every call, which is cheap (two `Arc` clones and a `Vec`).
    /// This is the one place "is a remote configured" is asked; nowhere else
    /// in the store branches on it (per the task's own "single federated
    /// search path" requirement).
    fn sources(&self) -> Vec<(heart::SourceId, Arc<dyn Serve<Symbols>>)> {
        let mut sources = vec![(heart::SourceId::new_random(), Arc::clone(&self.local))];
        if let Some(remote) = &self.remote {
            sources.push((heart::SourceId::new_random(), Arc::clone(remote)));
        }
        sources
    }

    // ── §7.4 shape ────────────────────────────────────────────────────────

    /// Issue a new search query immediately (without debounce).
    ///
    /// This is the inner half of the §7.4 shape. Called by the debounce timer,
    /// not directly from input handlers.
    fn trigger_search(&mut self, cx: &mut Context<Self>) {
        // 1. Supersede — advance the generation.
        let generation = self.gens.next();
        self.generation = generation.0;
        // 2. Reset section state for the new query. Only section 0 (Name) is
        //    ever populated — see `SectionBuf`'s own doc comment.
        self.sections[0] = SectionBuf::empty();
        self.sections[0].status = SectionStatus::Loading;
        self.raw_rows.clear();
        self.gen_arrival = None;
        self.offline = false;

        // 3. Build the federation and issue the query. `serve` returns
        //    immediately, before any source has answered (`Federated::serve`'s
        //    own doc comment) — the answer arrives on `answer`'s channel.
        let executor = cx.background_executor().clone();
        let spawn: Arc<dyn Fn(MergePump) + Send + Sync> = Arc::new(move |pump| {
            // GPUI's executor, not `tokio::spawn` — lindsey has no Tokio
            // reactor of its own (`Federated`'s own doc comment on why the
            // spawner is injected rather than hard-coded). Detached: nothing
            // here holds the returned `Task`, so the pump must keep running on
            // its own — the same shape `drain`'s doc comment describes for the
            // "nothing is `.detach()`ed" rule, except the pump's lifetime is
            // tied to the `Answer` it feeds, not to a GPUI entity, so there is
            // nothing else to own it.
            executor.spawn(pump).detach();
        });
        // The offline-first bound. `merge` ends an answer only once *every*
        // source has produced a terminal frame, and `RemoteClient` sets a
        // connect timeout but deliberately no whole-request timeout (a search
        // stream is legitimately long-lived). So an index that accepts the
        // connection and then wedges — a half-open socket, a stalled server, a
        // laptop that changed networks mid-query — would leave this answer open
        // forever: local rows painted instantly and correctly, over a spinner
        // that never stops.
        //
        // Ten seconds is chosen to be far longer than any healthy search and far
        // shorter than "never". It bounds *waiting*, not delivering: rows already
        // received are kept, a source that finishes in time is untouched, and
        // only a source still silent at expiry is reported as degraded. See
        // `heart/tests/offline_first.rs`.
        //
        // The timer is injected for the same reason the spawner is: the merge
        // pump runs on GPUI's executor, where `tokio::time::sleep` panics with
        // "there is no reactor running". GPUI's own `timer` is the right clock
        // here — it is the one this pump is actually being driven by.
        let timer_executor = cx.background_executor().clone();
        let timer: heart::surface::Timer = Arc::new(move || {
            let sleep = timer_executor.timer(SEARCH_DEADLINE);
            Box::pin(async move {
                sleep.await;
            })
        });
        let federated = Federated::new(self.sources(), spawn).with_deadline(timer);
        let query = build_query(&self.input);
        let answer = federated.serve(query, heart::surface::Gen(generation.0));

        // 4. Spin up the drain loop (owned, not detached — LD-18). Mirrors
        //    `bridge::drain::drain`'s batching rule exactly (await one frame,
        //    `try_recv` the rest, one `cx.notify()`) — `Answer` cannot be
        //    handed to `drain` directly (it is not a bare `flume::Receiver`),
        //    but it exposes the same `recv`/`try_recv` pairing for precisely
        //    this pattern (`heart::surface::Answer::try_recv`'s own doc
        //    comment).
        self.search_task = cx.spawn(async move |store, cx| {
            loop {
                let Some(first) = answer.recv().await else {
                    break;
                };
                let alive = store.update(cx, |store, cx| {
                    store.apply_frame(first);
                    while let Some(frame) = answer.try_recv() {
                        store.apply_frame(frame);
                    }
                    store.rebuild_rows();
                    cx.notify();
                });
                if alive.is_err() {
                    // The store entity was dropped — stop draining.
                    break;
                }
            }
        });
        // Clear selection for the new generation.
        self.selection = None;
        cx.notify();
    }

    /// Restart the 24 ms debounce timer.
    ///
    /// Drops the previous timer task (cancelling it) and spawns a new one.
    /// When the timer fires it calls `trigger_search`.
    fn restart_debounce(&mut self, cx: &mut Context<Self>) {
        // Dropping the old task cancels the pending timer.
        self.debounce_task = None;
        let duration = std::time::Duration::from_millis(DEBOUNCE_MS);
        self.debounce_task = Some(cx.spawn(async move |store, cx| {
            cx.background_executor().timer(duration).await;
            let _ = store.update(cx, |store, cx| {
                store.trigger_search(cx);
            });
        }));
    }

    // ── Event application ─────────────────────────────────────────────────

    /// Apply one frame from the federated `Answer<Symbols>`.
    ///
    /// # THE consumption rule
    ///
    /// `Frame::Item` is an **upsert keyed by `Surface::key`**, not an append —
    /// see that variant's own doc comment in `heart/surface.rs`. A repeat key
    /// is a supersede: `merge` already fused the two copies before re-emitting,
    /// so this replaces the held row rather than pushing a second one. Getting
    /// this wrong reintroduces the exact bug the whole federated contract
    /// exists to remove: the same symbol rendered twice, intermittently,
    /// depending on which plane answered first.
    ///
    /// # Why every hit lands in `sections[0]`
    ///
    /// `heart::surface::Symbols` — what `Federated::serve` answers, for both
    /// planes — carries no per-item section: a `Frame::Item` is `Located<
    /// Scored<SymbolHit>>`, with nothing naming which of the local engine's
    /// three internal ranking passes (name/type/semantic) produced it, and the
    /// remote index's own ranking is a single fused list (`RankSpecification::
    /// Fused`) with no section concept at all. `sections[1]`/`[2]` (Type,
    /// Semantic) stay empty; `SECTION_COUNT`/`Section`/`Cursor` are otherwise
    /// unchanged, so cmd-1 (jump to Name) and the rest of the selection model
    /// keep working over whichever sections actually hold rows.
    ///
    /// Does not itself rebuild `sections[0].rows` or notify — `trigger_search`'s
    /// drain loop calls this for every frame in a batch, then
    /// [`SearchStore::rebuild_rows`] and `cx.notify()` once, mirroring
    /// `bridge::drain::drain`'s batching rule.
    fn apply_frame(&mut self, frame: Frame<Symbols>) {
        match frame {
            Frame::Item(located) => {
                if self.gen_arrival.is_none() {
                    self.gen_arrival = Some(Instant::now());
                }
                let key = Symbols::key(&located.value);
                match self
                    .raw_rows
                    .iter_mut()
                    .find(|existing| Symbols::key(&existing.value) == key)
                {
                    Some(slot) => *slot = located,
                    None => self.raw_rows.push(located),
                }
            }

            // One source dropped out. Not terminal (`Frame::Degraded`'s own
            // doc comment) — whatever the other source(s) already delivered
            // keeps rendering; this only says the answer may be incomplete.
            // Offline-first honesty, not failure.
            Frame::Degraded(_) => {
                self.offline = true;
            }

            Frame::Note(SearchNote::Latency { millis }) => {
                // Pre-format here — §1.1.4 forbids formatting in render.
                //
                // # Why fast searches report no latency at all
                //
                // The results header used to read `2  0 ms`: a count and a
                // duration, adjacent, both unlabelled, the second of them
                // always zero because a local fixture search takes under a
                // millisecond. Nielsen's threshold is the reason to drop it
                // rather than punctuate it: 0.1 s is "the limit for having the
                // user feel that the system is reacting instantaneously"
                // (nngroup.com/articles/response-times-3-important-limits).
                // Below that, the reader did not experience a wait, so a
                // duration explains nothing that happened to them.
                self.sections[0].latency = if u128::from(millis) >= PERCEPTIBLE_LATENCY_MS {
                    SharedString::from(format!("{millis} ms"))
                } else {
                    SharedString::default()
                };
            }

            Frame::Note(SearchNote::Coverage { covered, total }) => {
                self.sections[0].status = SectionStatus::Building {
                    covered: u32::try_from(covered).unwrap_or(u32::MAX),
                    total: total
                        .and_then(|t| u32::try_from(t).ok())
                        .unwrap_or(u32::MAX),
                };
            }

            Frame::End(_) => {
                // "Settled" is not always `Ready`: a `Coverage` note that
                // already set `Building` must not be silently upgraded to a
                // completed, uncaveated search just because the stream closed.
                if self.sections[0].status == SectionStatus::Loading {
                    self.sections[0].status = SectionStatus::Ready;
                }
            }

            // Structurally rare: `Federated` appends a phantom
            // always-succeeding source specifically so "every real source
            // failed" degrades (`Frame::Degraded` + `Frame::End`) rather than
            // failing the whole answer (`Federated`'s own doc comment).
            // Handled anyway, honestly, in case a future host ever hands this
            // store a bare `Serve<Symbols>` with no federation wrapper.
            Frame::Failed(_) if self.sections[0].status == SectionStatus::Loading => {
                self.sections[0].status = SectionStatus::Offline;
            }
            Frame::Failed(_) => {}

            // `Frame` is `#[non_exhaustive]`: a future frame kind is ignored,
            // not a compile break.
            _ => {}
        }
    }

    /// Rebuild `sections[0].rows` from `raw_rows` and re-clamp the cursor.
    /// Called once per drained batch (not once per frame) — see
    /// [`SearchStore::apply_frame`]'s own doc comment.
    fn rebuild_rows(&mut self) {
        self.sections[0].rows = PreparedRow::prepare(&self.raw_rows);
        self.clamp_selection();
    }

    // ── Selection helpers ─────────────────────────────────────────────────

    /// Clamp the cursor after section rows change.
    ///
    /// If the row the cursor pointed at no longer exists (the section shrank),
    /// re-seats to the last available row.  If the section is now empty, moves
    /// to the first populated one.  §15 acceptance: arriving results never
    /// move an in-range cursor.
    fn clamp_selection(&mut self) {
        let Some(cursor) = self.selection else {
            return;
        };
        let counts = self.row_counts();
        self.selection = clamp_cursor_inner(cursor, &counts);
    }

    fn row_counts(&self) -> [usize; SECTION_COUNT] {
        [
            self.sections[0].rows.len(),
            self.sections[1].rows.len(),
            self.sections[2].rows.len(),
        ]
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Update the search input and restart the debounce timer.
    pub fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>) {
        self.input = text;
        self.restart_debounce(cx);
    }

    /// Switch the active search mode.
    ///
    /// Re-triggers the search immediately (mode change is intentional, not
    /// incremental, so debounce is skipped).
    pub fn set_mode_internal(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if !self.input.is_empty() {
            self.trigger_search(cx);
        }
    }

    /// Toggle a kind filter in the search scope.
    pub fn toggle_kind_internal(
        &mut self,
        kind: nudox_engine::wire::KindTag,
        cx: &mut Context<Self>,
    ) {
        if let Some(pos) = self.scope.kinds.iter().position(|k| *k == kind) {
            self.scope.kinds.remove(pos);
        } else {
            self.scope.kinds.push(kind);
        }
        if !self.input.is_empty() {
            self.trigger_search(cx);
        }
    }

    /// Move the selection up (`delta = -1`) or down (`delta = 1`).
    ///
    /// Does not wrap; stops at the first/last row (§15 invariant).
    pub fn move_selection_delta(&mut self, delta: i32, cx: &mut Context<Self>) {
        let counts = self.row_counts();
        let total: usize = counts.iter().sum();
        if total == 0 {
            return;
        }

        let next = match delta.signum() {
            1 => {
                // Move down: use step_cursor_inner
                step_down(self.selection, &counts)
            }
            -1 => step_up(self.selection, &counts),
            _ => return,
        };
        self.selection = next;
        cx.notify();
    }

    /// Commit the currently selected row, emitting `OpenSymbol`.
    pub fn commit_selection(&mut self, disposition: OpenDisposition, cx: &mut Context<Self>) {
        let cursor = match self.selection {
            Some(c) => c,
            None => {
                // Nothing selected: commit first row of first populated section.
                let counts = self.row_counts();
                match first_populated_from(&counts, 0) {
                    Some(s) if !self.sections[s].rows.is_empty() => Cursor { section: s, row: 0 },
                    _ => return,
                }
            }
        };

        if cursor.section < SECTION_COUNT
            && let Some(row) = self.sections[cursor.section].rows.get(cursor.row)
            && let Some(key) = row.key.clone()
        {
            // `row.key` is `None` when the source could not supply a
            // `StableReference` — see `PreparedRow::key`'s own doc comment.
            // Committing such a row does nothing rather than opening a
            // fabricated (and therefore wrong) symbol.
            cx.emit(OpenSymbol { key, disposition });
        }
    }
}

/// Build the one `heart::Query` every generation issues, local and remote
/// alike — the single federated search path has exactly one request shape,
/// not a local `SearchQuery` and a separate remote `Query`.
fn build_query(text: &str) -> Query {
    Query {
        target: Target::Symbols,
        text: text.to_owned(),
        scope: Scope::default(),
        rank: RankSpecification::default(),
        // Precise (lexical): matches the local engine's own default routing.
        mode: QueryMode::Precise,
        routing: Routing::default(),
        session: None,
        at: None,
        page: PageSpecification {
            limit: 50,
            cursor: None,
        },
        query_id: None,
    }
}

// ---------------------------------------------------------------------------
// SearchAccess impl
// ---------------------------------------------------------------------------

use crate::stores::search_model::SearchAccess;

impl SearchAccess for SearchStore {
    fn snapshot(&self) -> SearchSnapshot {
        // Cheap: only `Arc` refcount bumps and `SharedString` clones — no row
        // data allocation, no formatting (§12: stores hold render-ready state).
        SearchSnapshot {
            input: self.input.clone(),
            mode: self.mode,
            scopes: Arc::from([] as [ScopeChip; 0]), // populated when ScopeChip store lands
            sections: [
                self.sections[0].into_section_data(),
                self.sections[1].into_section_data(),
                self.sections[2].into_section_data(),
            ],
            recents: Arc::from([] as [PreparedRow; 0]), // populated by NavHistory store
            selection: self.selection,
            generation: self.generation,
            gen_arrival: self.gen_arrival,
            offline: self.offline,
        }
    }

    fn set_input(&mut self, text: SharedString, cx: &mut Context<Self>) {
        SearchStore::set_input(self, text, cx);
    }

    fn set_mode(&mut self, mode: SearchMode, cx: &mut Context<Self>) {
        SearchStore::set_mode_internal(self, mode, cx);
    }

    fn toggle_scope(&mut self, _ix: usize, cx: &mut Context<Self>) {
        // Scope chips are not yet wired; notify to keep the view consistent.
        cx.notify();
    }

    fn set_selection(&mut self, cursor: Option<Cursor>, cx: &mut Context<Self>) {
        self.selection = cursor;
        cx.notify();
    }
}

impl gpui::EventEmitter<OpenSymbol> for SearchStore {}

// ---------------------------------------------------------------------------
// Pure cursor helpers (mirrors the view's policy; the view is authoritative)
// ---------------------------------------------------------------------------

fn first_populated_from(counts: &[usize; SECTION_COUNT], from: usize) -> Option<usize> {
    (from..SECTION_COUNT).find(|&s| counts[s] > 0)
}

fn last_populated_before(counts: &[usize; SECTION_COUNT], from: usize) -> Option<usize> {
    (0..=from.min(SECTION_COUNT - 1))
        .rev()
        .find(|&s| counts[s] > 0)
}

fn clamp_cursor_inner(cursor: Cursor, counts: &[usize; SECTION_COUNT]) -> Option<Cursor> {
    if cursor.section < SECTION_COUNT && cursor.row < counts[cursor.section] {
        return Some(cursor);
    }
    let section = cursor.section.min(SECTION_COUNT - 1);
    if counts[section] > 0 {
        return Some(Cursor {
            section,
            row: counts[section] - 1,
        });
    }
    first_populated_from(counts, 0).map(|s| Cursor { section: s, row: 0 })
}

fn step_down(current: Option<Cursor>, counts: &[usize; SECTION_COUNT]) -> Option<Cursor> {
    let Some(cursor) = current else {
        return first_populated_from(counts, 0).map(|s| Cursor { section: s, row: 0 });
    };
    let cursor = clamp_cursor_inner(cursor, counts)?;
    if cursor.row + 1 < counts[cursor.section] {
        Some(Cursor {
            section: cursor.section,
            row: cursor.row + 1,
        })
    } else {
        match first_populated_from(counts, cursor.section + 1) {
            Some(s) => Some(Cursor { section: s, row: 0 }),
            None => Some(cursor),
        }
    }
}

fn step_up(current: Option<Cursor>, counts: &[usize; SECTION_COUNT]) -> Option<Cursor> {
    let Some(cursor) = current else {
        return last_populated_before(counts, SECTION_COUNT - 1).map(|s| Cursor {
            section: s,
            row: counts[s].saturating_sub(1),
        });
    };
    let cursor = clamp_cursor_inner(cursor, counts)?;
    if cursor.row > 0 {
        Some(Cursor {
            section: cursor.section,
            row: cursor.row - 1,
        })
    } else if cursor.section == 0 {
        Some(cursor)
    } else {
        match last_populated_before(counts, cursor.section - 1) {
            Some(s) => Some(Cursor {
                section: s,
                row: counts[s].saturating_sub(1),
            }),
            None => Some(cursor),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests (pure — no GPUI executor needed)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use heart::surface::Residence;
    use heart::{PackageId, Score, SymbolKind};

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// A federated hit — `Located<Scored<SymbolHit>>`, exactly what a
    /// `Frame::Item` carries. `package`/`path` together are `Symbols::key`, so
    /// tests that want two hits to collide pass the same `package` and `path`.
    fn hit(package: PackageId, path: &str, score: f32) -> Located<Scored<SymbolHit>> {
        Located::new(
            Scored::new(
                SymbolHit {
                    package,
                    path: path.into(),
                    display_name: path.into(),
                    ecosystem: heart::Language::Rust,
                    kind: SymbolKind::Module,
                    signature: None,
                    reference: None,
                },
                Score::try_new(score).expect("test score is finite"),
            ),
            Residence::Local,
        )
    }

    // Minimal test double: a local source that never answers on its own —
    // every test here drives `apply_frame`/`rebuild_rows` directly rather than
    // through a live `Answer`.
    struct NullEngine;

    impl Serve<Symbols> for NullEngine {
        fn serve(&self, _request: Query, generation: heart::surface::Gen) -> heart::surface::Answer<Symbols> {
            heart::surface::Answer::empty(generation)
        }
    }

    // ── Generation monotonicity ────────────────────────────────────────────

    #[test]
    fn gen_source_is_monotonic() {
        let mut gens = GenSource::new();
        let g1 = gens.next();
        let g2 = gens.next();
        let g3 = gens.next();
        assert!(g1 < g2);
        assert!(g2 < g3);
    }

    // ── The upsert rule (THE consumption rule) ──────────────────────────────

    /// A repeat key must replace the held row, never append a second one —
    /// `Frame::Item`'s own doc comment, and the exact bug class the whole
    /// federated contract exists to remove (the same symbol rendered twice,
    /// intermittently, depending on which plane answered first).
    #[test]
    fn a_repeat_key_replaces_rather_than_appends() {
        let mut store = SearchStore::for_local(NullEngine);
        let package = PackageId::new_random();

        store.apply_frame(Frame::Item(hit(package, "fixture::Item", 0.5)));
        store.apply_frame(Frame::Item(hit(package, "fixture::Item", 0.9)));
        store.rebuild_rows();

        assert_eq!(
            store.sections[0].rows.len(),
            1,
            "a repeat (package, path) key must replace in place, not append"
        );
        assert_eq!(store.sections[0].rows[0].relevance, 0.9, "the later copy wins");
    }

    /// Two genuinely distinct keys both survive.
    #[test]
    fn distinct_keys_both_land() {
        let mut store = SearchStore::for_local(NullEngine);
        let package = PackageId::new_random();

        store.apply_frame(Frame::Item(hit(package, "fixture::One", 0.9)));
        store.apply_frame(Frame::Item(hit(package, "fixture::Two", 0.5)));
        store.rebuild_rows();

        assert_eq!(store.sections[0].rows.len(), 2);
    }

    /// `heart::surface::Symbols` carries no per-item section, so every hit
    /// lands in `sections[0]` — `sections[1]`/`[2]` must stay exactly as a
    /// caller left them (see `SectionBuf`'s own doc comment).
    #[test]
    fn only_section_zero_is_ever_populated() {
        let mut store = SearchStore::for_local(NullEngine);
        let package = PackageId::new_random();
        // Simulate a stray write to section 1, as a regression would.
        store.sections[1].rows = PreparedRow::prepare(&[hit(package, "fixture::Stray", 0.9)]);
        store.sections[1].status = SectionStatus::Ready;

        store.apply_frame(Frame::Item(hit(package, "fixture::Item", 1.0)));
        store.rebuild_rows();

        assert_eq!(store.sections[0].rows.len(), 1, "section 0 holds the new row");
        assert_eq!(
            store.sections[1].rows.len(),
            1,
            "apply_frame/rebuild_rows must never touch section 1"
        );
        assert_eq!(store.sections[2].rows.len(), 0, "section 2 untouched");
    }

    // ── Degradation ──────────────────────────────────────────────────────

    /// One source failing degrades the snapshot's `offline` flag; local rows
    /// already collected still render (offline-first honesty, not failure).
    #[test]
    fn a_degraded_frame_sets_offline_without_dropping_rows() {
        let mut store = SearchStore::for_local(NullEngine);
        let package = PackageId::new_random();

        store.apply_frame(Frame::Item(hit(package, "fixture::Item", 1.0)));
        store.apply_frame(Frame::Degraded(heart::surface::Degradation {
            source: heart::SourceId::new_random(),
            reason: heart::stream::WireError::Timeout,
        }));
        store.rebuild_rows();

        assert!(store.offline, "a degraded source must set offline");
        assert_eq!(
            store.sections[0].rows.len(),
            1,
            "the healthy source's row still renders"
        );
    }

    // ── Latency pre-formatting ─────────────────────────────────────────────

    /// Latency is formatted as `"{ms} ms"` at ingest, not in render.
    #[test]
    fn latency_preformatted_as_ms_string() {
        let mut store = SearchStore::for_local(NullEngine);
        // Above the 100 ms perceptibility threshold, or this is
        // indistinguishable from the "no line at all" case below.
        store.apply_frame(Frame::Note(SearchNote::Latency { millis: 142 }));
        assert_eq!(store.sections[0].latency.as_ref(), "142 ms");
    }

    /// Below Nielsen's 100 ms threshold, nothing is reported at all — see
    /// `apply_frame`'s own doc comment on why sub-perceptible latency is noise.
    #[test]
    fn imperceptible_latency_reports_nothing() {
        let mut store = SearchStore::for_local(NullEngine);
        store.apply_frame(Frame::Note(SearchNote::Latency { millis: 4 }));
        assert!(store.sections[0].latency.is_empty());
    }

    // ── Cursor-based selection ─────────────────────────────────────────────

    /// Selection never wraps silently from last row to first across sections.
    #[test]
    fn step_down_at_last_row_holds() {
        let counts = [2usize, 2, 0];
        let last = Cursor { section: 1, row: 1 };
        let next = step_down(Some(last), &counts);
        assert_eq!(next, Some(last), "should hold at last row");
    }

    #[test]
    fn step_up_at_first_row_holds() {
        let counts = [2usize, 2, 0];
        let first = Cursor { section: 0, row: 0 };
        let next = step_up(Some(first), &counts);
        assert_eq!(next, Some(first), "should hold at first row");
    }

    /// Moving down crosses into the next populated section.
    #[test]
    fn step_down_crosses_section_boundary() {
        let counts = [2usize, 0, 3];
        let at_end_of_name = Cursor { section: 0, row: 1 };
        let next = step_down(Some(at_end_of_name), &counts);
        assert_eq!(
            next,
            Some(Cursor { section: 2, row: 0 }),
            "empty Type section is transparent"
        );
    }

    /// Moving up crosses back to the last row of the previous section.
    #[test]
    fn step_up_crosses_section_boundary() {
        let counts = [3usize, 0, 2];
        let top_of_semantic = Cursor { section: 2, row: 0 };
        let next = step_up(Some(top_of_semantic), &counts);
        assert_eq!(next, Some(Cursor { section: 0, row: 2 }));
    }

    /// A shrinking section re-seats the cursor to its last row.
    #[test]
    fn clamp_cursor_reseats_to_last_row() {
        let cursor = Cursor { section: 0, row: 9 };
        let counts = [3usize, 0, 0];
        let clamped = clamp_cursor_inner(cursor, &counts);
        assert_eq!(clamped, Some(Cursor { section: 0, row: 2 }));
    }

    /// A cursor in a now-empty section moves to the first populated one.
    #[test]
    fn clamp_cursor_moves_to_first_populated() {
        let cursor = Cursor { section: 1, row: 0 };
        let counts = [2usize, 0, 0];
        let clamped = clamp_cursor_inner(cursor, &counts);
        assert_eq!(clamped, Some(Cursor { section: 0, row: 0 }));
    }

    /// Semantic arriving never moves an in-range cursor (§15 acceptance).
    #[test]
    fn semantic_arriving_leaves_cursor_untouched() {
        let cursor = Cursor { section: 0, row: 2 };
        let before = [4usize, 0, 0];
        let after = [4usize, 0, 20];
        assert_eq!(clamp_cursor_inner(cursor, &before), Some(cursor));
        assert_eq!(
            clamp_cursor_inner(cursor, &after),
            Some(cursor),
            "semantic arriving must not move the cursor"
        );
    }

    // ── Snapshot cost ──────────────────────────────────────────────────────

    /// Snapshot clones only Arc/SharedString; it does not allocate row data.
    #[test]
    fn snapshot_is_cheap_clones_only() {
        let store = SearchStore::for_local(NullEngine);
        // This is a compile-time / structural test: we verify snapshot() returns
        // a SearchSnapshot and that the fields are Arc / SharedString types
        // (refcount bumps, no row allocation).  We cannot check allocation
        // counts without a custom allocator, so we verify the snapshot value
        // matches what we put in.
        let snap = SearchAccess::snapshot(&store);
        assert_eq!(snap.input.as_ref(), "");
        assert_eq!(snap.sections[0].rows.len(), 0);
        assert_eq!(snap.generation, 0);
        assert!(snap.selection.is_none());
    }

    // ── PreparedRow conversion at ingest ──────────────────────────────────

    /// `PreparedRow::prepare` converts federated hits once; the result is an
    /// `Arc` slice.
    #[test]
    fn prepare_converts_hits_to_prepared_rows() {
        let package = PackageId::new_random();
        let hits = vec![
            hit(package, "fixture::One", 0.9),
            hit(package, "fixture::Two", 0.5),
        ];
        let prepared = PreparedRow::prepare(&hits);
        assert_eq!(prepared.len(), 2);
    }

    /// `PreparedRow::prepare` splits qualified names correctly.
    #[test]
    fn prepared_row_splits_qualified_name() {
        let one = hit(PackageId::new_random(), "serde_json::value::Value", 1.0);
        let row = &PreparedRow::prepare(std::slice::from_ref(&one))[0];
        assert_eq!(row.path.as_ref(), "serde_json::value::");
        assert_eq!(row.leaf.as_ref(), "Value");
    }

    /// `PreparedRow::prepare` handles a bare name (no separator).
    #[test]
    fn prepared_row_bare_name_has_empty_path() {
        let one = hit(PackageId::new_random(), "Value", 1.0);
        let row = &PreparedRow::prepare(std::slice::from_ref(&one))[0];
        assert!(row.path.is_empty());
        assert_eq!(row.leaf.as_ref(), "Value");
    }

    /// A hit with no `reference` gets no fabricated key — it renders, but
    /// `PreparedRow::key` is `None` rather than a made-up `SymbolKey`.
    #[test]
    fn a_hit_with_no_reference_has_no_key() {
        let one = hit(PackageId::new_random(), "fixture::Item", 1.0);
        let row = &PreparedRow::prepare(std::slice::from_ref(&one))[0];
        assert!(row.key.is_none());
    }
}

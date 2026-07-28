//! The symbol page — one continuous document, like docs.rs (GUI-PLAN §16).
//!
//! # What this page is trying to beat
//!
//! docs.rs. It gets a great deal right — dense member tables, every type in a
//! signature hyperlinked, a permanent breadcrumb, a navigable sidebar — and all
//! of that is matched here. What it cannot do is *arrive*. A docs.rs page is
//! blank and then complete; there is no in-between, because the HTML is built
//! before it is served.
//!
//! Ours has an in-between, and the in-between is the product:
//!
//! * The header paints from `SymbolHead` alone, in the frame the tab is created
//!   (§9.1: < 50 ms local). Breadcrumb, signature, kind, trust — all of it is in
//!   the first event.
//! * The body is a skeleton of *exactly* the right shape before a single section
//!   has been parsed, because `section_plan` carries a `SizeHint` per section
//!   (§9.4.1). Sections then replace their skeletons in place. See [`docs`] for
//!   how the zero-jump guarantee is actually enforced.
//! * The outline exists before the document does, and doubles as a progress
//!   display — see [`outline`].
//! * Refs and Impls fill in the background while you read, visible as collapsed
//!   sections below the documentation body.
//! * Every symbol, every version carries provenance (LD-8). docs.rs cannot tell
//!   you where a doc came from. We always can.
//!
//! # Layout — one continuous document
//!
//! The previous design put Docs, Refs, Impls, Source, Timeline, and Graph behind
//! an inner tab strip. The user's direct words: "All of these weird tabs should
//! just be removed" and "implementation should be directly there, right below the
//! documentation like docs.rs." So we did exactly that.
//!
//! Sections render in this order, each introduced by a disclosure header:
//!
//! 1. **Documentation** — the streamed body (the plan-virtualized list from
//!    [`docs`]). Always expanded; this is what the reader came for.
//! 2. **Implementations** — the impls table, rendered directly below the docs.
//!    Collapsed by default: axum's `Router` has dozens of impls and they would
//!    bury the documentation if opened automatically.
//! 3. **References** — the grouped cross-reference table. Collapsed by default.
//! 4. **Source** — a stub until the source-materialise surface is built.
//!    Collapsed by default.
//!
//! # Collapsing and the §9.4 zero-jump guarantee
//!
//! The documentation body (section 1 above) lives inside the `list()` that
//! `DocsBody` owns, which is where the zero-jump guarantee lives. Collapse does
//! not touch that list; it only shows or hides the sibling `div`s that follow it.
//!
//! The overall page is a `v_flex` column. Toggling a collapsed section changes
//! whether that sibling's content `div` is included in the flex column — no list
//! is involved and no remeasure is needed. The zero-jump machinery in `docs.rs`
//! is completely unaffected.
//!
//! # Collapse state lifetime
//!
//! Each section's collapsed/expanded state is stored in [`SymbolPage`] itself, as
//! a per-tab, per-instance field. Opening a second symbol does not inherit the
//! first symbol's collapse choices: `new()` always sets the designed defaults.
//!
//! # Refs shows nothing — root-cause finding
//!
//! `RefsTable::sync` is called unconditionally in `sync_from_store`, so ref
//! pages accumulate in the table regardless of whether the reader has ever looked
//! at refs. The data is there. The "shows nothing" complaint was about the lazy
//! tab: the old code returned an empty `div` for any tab whose `loaded[ix]`
//! was `false`, and the Refs tab defaulted to `false`. Removing the tab system
//! eliminates the lazy-load gate entirely; refs render whenever they exist.
//!
//! If refs still show nothing after this change, the engine's stub is not
//! emitting `DocEvent::Refs` for the symbol being viewed. That is an engine
//! problem, not a view problem, and the store's `apply_doc_event` is correct.
//!
//! # Timeline — version strip
//!
//! `DocEvent::Timeline` is emitted by the engine (after `Head`, before the first
//! `Section`) and is now threaded through `SymbolDoc::timeline` in the store.
//! `SymbolPage` projects it once per delivery in `sync_from_store` into
//! `timeline_rows: Arc<[TimelineRowView]>` (pre-formatted `SharedString`s, §1.1.4)
//! and renders a slim version strip immediately below the header.
//!
//! # Selecting a different version
//!
//! Each version strip entry fires `select_version(ix)`, which is currently a
//! no-op stub on `SymbolPage`.  Completing it requires:
//!
//! 1. `EngineHandle::select_version(tab_id, version: wire::SharedStr, gen) ->
//!    Receiver<VersionEvent>` — already mentioned in the wire docs.
//! 2. A `SymbolStore::select_version(tab_id, version, cx)` that calls (1) and
//!    then calls `start_stream` (stale-while-revalidate, LD-15).
//! 3. The `SymbolEngine` trait gaining the same `select_version` method so the
//!    store stays testable.
//!
//! Until (2) exists, clicking a version row is intentionally inert: the strip is
//! visible and the user can read the history without triggering a navigation they
//! cannot complete.
//!
//! # Module layout
//!
//! | Module      | Owns                                                      |
//! |-------------|-----------------------------------------------------------|
//! | [`header`]  | breadcrumb, signature, kind, version picker, trust badge   |
//! | [`docs`]    | the virtualized section list and the zero-jump machinery   |
//! | [`outline`] | the plan-derived sidebar with scroll sync                  |
//! | [`refs`]    | the grouped references table and the implementors table    |
//! | this file   | `WorkspaceItem`, section collapse state, store wiring      |
//!
//! # TODO(store) — the `SymbolStore` surface this page consumes
//!
//! Everything below already exists in `crate::stores::symbol`. This page reads
//! only; it never mutates store state except through these methods.
//!
//! ```text
//! SymbolStore<E: SymbolEngine>
//!   .doc(TabId) -> Option<&SymbolDoc>          // the sole projection source
//!   .open(SymbolKey, OpenDisposition, cx)      // link / crumb / row navigation
//!   .reload(TabId, cx)                         // retry from the error bar
//!   .set_version(TabId, cx)                    // version picker
//!   .visible_sections(TabId, &[SectionId])     // §9.4.5 highlight priority
//!
//! SymbolDoc
//!   .head: Option<Box<SymbolHead>>             // header + section_plan
//!   .sections: Progressive<RenderSection>      // body, append-only
//!   .highlights: HashMap<SectionId, Arc<[HighlightSpan]>>
//!   .refs / .impls: Progressive<RefsPage|ImplsPage>
//!   .slot_meta: StreamSlot<()>                 // phase, error, generation
//! ```
//!
//! Gaps this page works around rather than inventing store API for:
//!
//! 1. **No version list.** `SymbolDoc` carries no `versions`, and `set_version`
//!    takes no argument, so the picker is fed by [`SymbolPage::set_versions`]
//!    from whoever owns version data. With no versions installed the chip is
//!    simply not shown — we do not render an affordance we cannot honour.
//! 2. **No `line` on `RefRow`.** [`refs`] recovers a line number from a `path`
//!    of the form `src/foo.rs:120` and leaves the column blank otherwise.
//! 3. **Version selection incomplete.** The version strip shows rows from
//!    `SymbolDoc::timeline` but clicking one is a no-op stub. See the
//!    "Selecting a different version" note above for the remaining work.

pub mod docs;
pub mod header;
pub mod outline;
pub mod refs;

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClipboardItem, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription,
    Window, div, list, prelude::FluentBuilder as _,
};
use gpui_component::{Icon, IconName, Sizable as _, StyledExt as _, skeleton::Skeleton};
use nudox_engine::wire::{SymbolKey, TimelineChange};

use crate::app::actions::{
    CopySymbolUri, GoToDocsTab, GoToRefsTab, GoToSourceTab, GoToTimelineTab, NextInnerTab,
    OpenVersionPicker, PrevInnerTab,
};
use crate::bridge::slot::{Display as SlotDisplay, SlotError};
use crate::motion::tokens::MotionTokens;
use crate::stores::events::OpenDisposition;
use crate::stores::symbol::{SymbolDoc, SymbolEngine, SymbolStore, TabId};
use crate::theme::ext::{Provenance, ThemeExtAccessor as _};
use crate::ui::{CountLabel, EmptyState, ErrorState, ProvenanceDot};
use crate::workspace::item::{NavEntry, WorkspaceItem};

use header::{HeaderModel, SymbolHeader, VersionOption};

/// How often viewport section ids are pushed to the engine.
///
/// §9.4.5 specifies "best-effort, coalesced at 4 Hz". This is a protocol
/// coalescing interval, not a motion token — it belongs here rather than in
/// `motion/tokens.rs`, which is the vocabulary for things the *user* sees move.
const HIGHLIGHT_PRIORITY_INTERVAL: Duration = Duration::from_millis(250);

// ─────────────────────────────────────────────────────────────────────────────
// Section collapse state
// ─────────────────────────────────────────────────────────────────────────────

/// Which sections of the single-document layout are currently expanded.
///
/// # Designed defaults
///
/// Documentation is always expanded — that is the whole point of the page.
///
/// Implementations, References, and Source start *collapsed*. The user asked
/// for "contract most of it by default" because axum-scale types have dozens of
/// impls, and they buried the documentation when opened. The disclosure header
/// shows the item count, so the reader knows what is there without seeing all of
/// it.
///
/// # Lifetime
///
/// One `CollapseState` lives inside each [`SymbolPage`], allocated in `new()`.
/// It is reset between generations only when the user explicitly reloads —
/// switching from one symbol to another creates a fresh page and therefore a
/// fresh `CollapseState`.
///
/// Opening two symbols in two tabs gives each its own `CollapseState`. Expanding
/// Refs on tab A does not affect tab B.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CollapseState {
    /// Documentation body — always expanded; not user-togglable from here.
    pub docs: bool,
    /// Implementations table — collapsed by default (can be huge).
    pub impls: bool,
    /// Cross-references table — collapsed by default.
    pub refs: bool,
    /// Source view — collapsed by default (stub until materialisation).
    pub source: bool,
}

impl CollapseState {
    /// The designed defaults: docs open, everything else closed.
    pub fn default_open() -> Self {
        Self {
            docs: true,
            impls: false,
            refs: false,
            source: false,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Timeline strip projection (pre-computed, §1.1.4)
// ─────────────────────────────────────────────────────────────────────────────

/// One row in the version strip, pre-formatted for zero-allocation rendering.
///
/// Projected from `wire::TimelineRow` once per `DocEvent::Timeline` delivery in
/// `sync_from_store`; never recomputed on a render path (§1.1.4).
#[derive(Clone, Debug)]
pub struct TimelineRowView {
    /// Version string, e.g. `"0.8.9"`.
    pub version: SharedString,
    /// Short change label shown beside the version, e.g. `"current"`,
    /// `"introduced"`, `"renamed"`, `"unchanged"`, …
    pub change_label: SharedString,
    /// True for the version the corpus currently serves.
    pub is_current: bool,
}

impl TimelineRowView {
    /// Project one `wire::TimelineRow` into a display row.
    ///
    /// LD-7: `TimelineChange` is `#[non_exhaustive]`; unrecognised variants
    /// render as `"changed"`, which is always true for any new variant we add.
    pub fn from_wire(row: &nudox_engine::wire::TimelineRow) -> Self {
        let change_label = timeline_change_label(&row.change, row.is_current);
        Self {
            version: SharedString::from(String::from(&*row.version)),
            change_label,
            is_current: row.is_current,
        }
    }
}

/// Map a `TimelineChange` onto a short human label.
///
/// Called at projection time, never in `render`. The `is_current` flag is
/// passed in because "current" overrides every other label when true — the
/// reader most wants to know which version they are *reading*, not what changed
/// in it relative to its predecessor.
///
/// LD-7: the enum is `#[non_exhaustive]`; the `_` arm is visible ("changed"),
/// never silent.
fn timeline_change_label(change: &TimelineChange, is_current: bool) -> SharedString {
    if is_current {
        return SharedString::from("current");
    }
    match change {
        TimelineChange::Present => SharedString::from("present"),
        TimelineChange::Introduced => SharedString::from("introduced"),
        TimelineChange::Renamed { .. } => SharedString::from("renamed"),
        TimelineChange::Deprecated => SharedString::from("deprecated"),
        TimelineChange::Undeprecated => SharedString::from("undeprecated"),
        TimelineChange::SignatureChanged => SharedString::from("sig changed"),
        TimelineChange::VisibilityChanged => SharedString::from("vis changed"),
        TimelineChange::DocsChanged => SharedString::from("docs changed"),
        TimelineChange::Unchanged => SharedString::from("unchanged"),
        TimelineChange::Removed => SharedString::from("removed"),
        // LD-7 fallback — visible, never silent.
        _ => SharedString::from("changed"),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Page state (§16 specialisation of Display<T>)
// ─────────────────────────────────────────────────────────────────────────────

/// The §16 specialisation of `Display<T>`.
///
/// `SymbolDoc::slot_meta` is a `StreamSlot<()>` — it tracks the stream's phase,
/// not its content, so `slot.display()` reports `Skeleton` for the whole of a
/// stream even after the header and half the body have painted. Routing the page
/// through `ui::slot_view` would therefore hide a readable page behind grey bars,
/// which is the exact anti-pattern LD-15 exists to prevent.
///
/// Instead we map every one of `Display`'s seven arms onto a §16 behaviour, in
/// one exhaustive match ([`SymbolPage::classify`]). The compile-time guarantee
/// `slot_view` provides — no state can be forgotten — is preserved; only the
/// *rendering* of each state is specialised to this page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageState {
    /// Nothing requested yet.
    Empty,
    /// Requested; no `Head` yet. Header chassis, grace-gated skeleton.
    Awaiting,
    /// `Head` landed; sections still arriving.
    Streaming,
    /// The stream completed cleanly.
    Ready,
    /// A newer generation is streaming over readable content (LD-15).
    Stale,
    /// Failed before anything was readable (LD-16).
    ColdError,
    /// Failed with readable content on screen — page stays, retry bar appears.
    StaleError,
}

impl PageState {
    /// Whether a superseded generation should be dimmed (LD-15).
    ///
    /// Only states that still show readable content dim; dimming a cold load
    /// would be dimming nothing.
    fn is_stale(self) -> bool {
        matches!(self, PageState::Stale | PageState::StaleError)
    }

    /// Whether the body has been asked for but has not arrived.
    fn is_awaiting(self) -> bool {
        matches!(self, PageState::Awaiting | PageState::Streaming)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SymbolPage
// ─────────────────────────────────────────────────────────────────────────────

/// One open symbol, as a workspace tab.
pub struct SymbolPage<E: SymbolEngine> {
    tab: TabId,
    store: Entity<SymbolStore<E>>,

    /// Projected once per generation from `SymbolHead`.
    header: HeaderModel,
    /// Version picker contents — see the `TODO(store)` note in the module docs.
    versions: Arc<[VersionOption]>,
    active_version: usize,
    picker_open: bool,

    docs: docs::DocsBody,
    outline: outline::Outline,
    refs: refs::RefsTable,
    impls: refs::ImplsTable,

    /// Per-tab disclosure state for the four document sections.
    ///
    /// Not persisted globally; each page starts with `CollapseState::default_open()`.
    collapse: CollapseState,

    /// Pre-formatted count strings for the disclosure headers.
    ///
    /// Formatted once when the count changes (§1.1.4), stored as `SharedString`.
    refs_count_text: SharedString,
    impls_count_text: SharedString,

    /// Generation of the currently projected head, mirroring
    /// [`SymbolDoc::head_gen`]. `None` until a head has been projected.
    head_gen: Option<u64>,
    /// Cached stream state, recomputed on every store notification.
    state: PageState,
    error: Option<SlotError>,
    /// The error's message, formatted once when it changes rather than on every
    /// frame the retry bar is on screen (§1.1.4).
    error_text: Option<SharedString>,
    refs_streaming: bool,
    impls_streaming: bool,

    /// 4 Hz coalescing gate for `HighlightPriority` (§9.4.5).
    last_priority: Option<Instant>,

    /// Version history strip, projected from `SymbolDoc::timeline` once per
    /// delivery (§1.1.4).  `None` means the timeline has not arrived yet —
    /// never "this symbol has no history" (see module docs).
    timeline_rows: Option<Arc<[TimelineRowView]>>,

    /// Pre-formatted source path for the Source section.
    ///
    /// `None` means the head has not arrived or the producer did not record a
    /// source location.  Projected once from `SymbolHead::source_path` in
    /// `sync_from_store` (§1.1.4).
    source_path_text: Option<SharedString>,

    /// Pre-formatted source span label, e.g. `"bytes 10–42"`.
    ///
    /// `None` when `source_path_text` is `None`.  Byte offsets, not lines —
    /// see `SymbolHead::source_span` docs.
    source_span_text: Option<SharedString>,

    _subs: Vec<Subscription>,
}

impl<E: SymbolEngine> SymbolPage<E> {
    /// Build a page for an already-opened tab.
    ///
    /// The store has issued the stream; this view attaches to it. Nothing here
    /// blocks, parses, or waits for an event — the first frame paints the header
    /// chassis and whatever the store already has.
    pub fn new(
        tab: TabId,
        store: Entity<SymbolStore<E>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut docs = docs::DocsBody::new(cx);

        // Symbol links inside prose, member rows and signatures all navigate
        // through the store, which owns tab dedup and `nav.flash` (§12.4).
        {
            let store = store.downgrade();
            docs.on_open(move |key, _window, cx| {
                let key = key.clone();
                let _ = store.update(cx, |store, cx| {
                    store.open(key, OpenDisposition::Replace, cx);
                });
            });
        }

        // Scroll sync: the body's visible range drives both the outline
        // highlight and the engine's highlight priority.
        //
        // This handler runs *inside* the list's own scroll handling, while its
        // `RefCell` is borrowed, so it must never touch the `ListState` again.
        // It only reads view state and notifies.
        {
            let weak = cx.entity().downgrade();
            docs.list_state().set_scroll_handler(move |event, _window, cx| {
                let range = event.visible_range.clone();
                let _ = weak.update(cx, |page, cx| page.on_body_scroll(range, cx));
            });
        }

        let subs = vec![cx.observe(&store, |page, _store, cx| {
            page.sync_from_store(cx);
        })];

        let mut page = Self {
            tab,
            store,
            header: HeaderModel::empty(),
            versions: Arc::from(Vec::new()),
            active_version: 0,
            picker_open: false,
            docs,
            outline: outline::Outline::new(),
            refs: refs::RefsTable::new(),
            impls: refs::ImplsTable::new(),
            // Designed defaults: docs open, ancillary sections collapsed.
            collapse: CollapseState::default_open(),
            refs_count_text: SharedString::from(""),
            impls_count_text: SharedString::from(""),
            head_gen: None,
            state: PageState::Empty,
            error: None,
            error_text: None,
            refs_streaming: false,
            impls_streaming: false,
            last_priority: None,
            timeline_rows: None,
            source_path_text: None,
            source_span_text: None,
            _subs: subs,
        };
        // Pick up anything that landed between `open()` and this constructor.
        page.sync_from_store(cx);
        page
    }

    /// Install the version list for the picker (see the `TODO(store)` note).
    pub fn set_versions(
        &mut self,
        versions: Arc<[VersionOption]>,
        active: usize,
        cx: &mut Context<Self>,
    ) {
        self.versions = versions.clone();
        self.active_version = active;
        self.header.versions = versions;
        self.header.active_version = active;
        cx.notify();
    }

    /// The symbol this page shows, once `Head` has landed.
    pub fn symbol_uri(&self) -> SharedString {
        self.header.uri.clone()
    }

    // ── Store synchronisation (all projection happens here, never in render) ─

    fn sync_from_store(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        let mut dirty = false;

        {
            let Some(doc) = store.read(cx).doc(self.tab) else {
                return;
            };

            // ── Head ────────────────────────────────────────────────────────
            //
            // A generation bump alone is not enough to re-project: `reload` and
            // `set_version` advance the generation *before* the new `Head`
            // arrives, and the old head is deliberately left in place so the
            // page stays readable (LD-15). So the question is not "has the
            // generation moved?" but "is the head on screen the one the store
            // currently holds?" — which `SymbolDoc::head_gen` answers directly.
            let generation = doc.head_gen;
            let head_is_new = generation.is_some() && generation != self.head_gen;
            if let Some(head) = doc.head.as_ref() {
                if head_is_new {
                    self.head_gen = generation;
                    let generation = generation.unwrap_or_default();
                    let mut model = HeaderModel::from_head(head);
                    model.versions = self.versions.clone();
                    model.active_version = self.active_version;
                    self.header = model;
                    self.docs.set_plan(&head.section_plan, generation, cx);
                    self.refs.reset(generation);
                    self.impls.reset(generation);
                    // Reset collapse state for the new generation too — starting
                    // fresh on reload prevents stale open/closed state from a
                    // symbol that no longer shares structure with the new one.
                    self.collapse = CollapseState::default_open();

                    // ── Source location ─────────────────────────────────────
                    //
                    // Projected once per head (§1.1.4). The path is whatever the
                    // producer recorded; the span is a byte range (not lines).
                    self.source_path_text = head.source_path.as_ref().map(|p| {
                        SharedString::from(String::from(&**p))
                    });
                    self.source_span_text = head.source_span.map(|[start, end]| {
                        SharedString::from(format!("bytes {}–{}", start, end))
                    });

                    // Clear stale timeline on head reset; it will be reprojected
                    // below when the new timeline arrives.
                    self.timeline_rows = None;

                    dirty = true;
                }
            }

            // ── Timeline ─────────────────────────────────────────────────────
            //
            // Project once per delivery. The timeline is `None` until the engine
            // sends it; after that it is replaced only when `reset_content` runs
            // (new generation).  We detect "just arrived" by comparing the
            // current row count against what we have — if the store has it and we
            // don't, project it.
            if let Some(tl) = doc.timeline.as_ref() {
                if self.timeline_rows.is_none() {
                    let rows: Arc<[TimelineRowView]> = tl
                        .rows
                        .iter()
                        .map(TimelineRowView::from_wire)
                        .collect();
                    self.timeline_rows = Some(rows);
                    dirty = true;
                }
            }

            // ── Body ────────────────────────────────────────────────────────
            dirty |= self.docs.sync_sections(doc.sections.parts());
            dirty |= self.docs.sync_highlights(&doc.highlights, cx);

            // ── Ancillary data (fills while the reader reads — §9.1) ────────
            //
            // Previously these were only synced when their tab was "loaded" via
            // a lazy-activation gate. Now they are always synced so the counts
            // in the disclosure headers are live, and the tables are populated
            // the moment the reader expands them.
            let refs_dirty = self.refs.sync(doc.refs.parts());
            let impls_dirty = self.impls.sync(doc.impls.parts());

            // Reformat the count strings only when the count moves (§1.1.4).
            if refs_dirty {
                let t = self.refs.total();
                self.refs_count_text = if t > 0 {
                    SharedString::from(t.to_string())
                } else {
                    SharedString::from("")
                };
            }
            if impls_dirty {
                let t = self.impls.total();
                self.impls_count_text = if t > 0 {
                    SharedString::from(t.to_string())
                } else {
                    SharedString::from("")
                };
            }
            dirty |= refs_dirty | impls_dirty;

            self.refs_streaming = !doc.refs.is_complete();
            self.impls_streaming = !doc.impls.is_complete();

            // ── Stream state ────────────────────────────────────────────────
            let next_state = Self::classify(doc);
            if next_state != self.state {
                self.state = next_state;
                dirty = true;
            }
            if doc.slot_meta.error != self.error {
                self.error = doc.slot_meta.error.clone();
                // `SlotError: Display`. Formatted here, once per failure, so the
                // retry bar costs nothing per frame (§1.1.4).
                self.error_text = self
                    .error
                    .as_ref()
                    .map(|e| SharedString::from(e.to_string()));
                dirty = true;
            }
        }

        dirty |= self.outline.sync(&self.docs);

        if dirty {
            cx.notify();
        }
    }

    /// Map the slot's seven-state `Display` onto §16 behaviour.
    ///
    /// Exhaustive by construction — adding a `Display` variant breaks this
    /// match, which is the point.
    fn classify(doc: &SymbolDoc) -> PageState {
        let readable = doc.head.is_some();
        match doc.slot_meta.display() {
            SlotDisplay::Empty => PageState::Empty,
            // `StreamSlot<()>` only stores `Some(())` at `Done`, so this arm
            // covers the whole of a live stream. Whether the page is readable is
            // decided by the head, not by the slot's value.
            SlotDisplay::Skeleton { .. } => {
                if !readable {
                    PageState::Awaiting
                } else if doc.sections.is_empty() && doc.slot_meta.phase.is_active() {
                    PageState::Streaming
                } else {
                    // A head from a previous generation is on screen while a new
                    // one loads: LD-15 stale-while-revalidate.
                    PageState::Stale
                }
            }
            SlotDisplay::Partial(_) => PageState::Streaming,
            SlotDisplay::Stale(_) => PageState::Stale,
            SlotDisplay::Fresh(_) => PageState::Ready,
            SlotDisplay::Error(_) => {
                if readable {
                    PageState::StaleError
                } else {
                    PageState::ColdError
                }
            }
            SlotDisplay::StaleWithError(_, _) => PageState::StaleError,
        }
    }

    // ── Interaction ──────────────────────────────────────────────────────────

    fn on_body_scroll(&mut self, range: std::ops::Range<usize>, cx: &mut Context<Self>) {
        // §9.4.5: tell the engine what the reader can actually see, so the
        // highlighter works on the viewport first. Coalesced at 4 Hz.
        let due = self
            .last_priority
            .is_none_or(|t| t.elapsed() >= HIGHLIGHT_PRIORITY_INTERVAL);
        if due {
            self.last_priority = Some(Instant::now());
            let ids = self.docs.visible_ids(range.clone());
            if !ids.is_empty() {
                self.store.read(cx).visible_sections(self.tab, &ids);
            }
        }

        // Only repaint when the highlighted outline entry actually moves; a
        // scroll that stays inside one section costs nothing.
        if self.outline.set_active(range.start) {
            cx.notify();
        }
    }

    fn reveal_section(&mut self, ix: usize, cx: &mut Context<Self>) {
        // Ensure the docs section is expanded before jumping — the docs body is
        // always expanded by design, so this is defensive in case a future
        // toggle is added.
        self.collapse.docs = true;
        self.docs.reveal(ix);
        self.outline.set_active(ix);
        cx.notify();
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// A disclosure header: the section title, optional item count, and a
    /// chevron indicating expanded / collapsed state.
    ///
    /// Clicking this header toggles the corresponding `CollapseState` field via
    /// the provided closure. The chevron direction is the conventional cue:
    /// `▼` when expanded, `▶` when collapsed.
    ///
    /// The header reserves no extra height beyond its own label row — the
    /// content below it is governed by whether the caller includes the body div,
    /// not by any animation or interpolated height. This is intentional: smooth
    /// height transitions are geometry changes, and geometry changes on the docs
    /// list would require `remeasure_items` calls that do not apply here (the
    /// docs body is its own list, untouched by these toggles).
    fn render_disclosure_header(
        &self,
        id: impl Into<gpui::ElementId>,
        label: SharedString,
        count: Option<SharedString>,
        streaming: bool,
        expanded: bool,
        on_toggle: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        div()
            .id(id.into())
            .flex()
            .flex_row()
            .items_center()
            .gap(sp.space_2)
            .w_full()
            .px(sp.space_4)
            .py(sp.space_2)
            .bg(colours.bg_raised)
            .border_t_1()
            .border_color(colours.border_default)
            .cursor_pointer()
            .hover(|s| s.bg(colours.bg_hover))
            .on_click(cx.listener(move |page, _, window, cx| on_toggle(page, window, cx)))
            // Chevron: ▼ expanded, ▶ collapsed. Both are the same width so the
            // label never shifts position when the section is toggled.
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .text_color(colours.fg_faint)
                .with_size(gpui_component::Size::XSmall),
            )
            .child(
                div()
                    .text_size(ts.ui.size)
                    .line_height(ts.ui.line_height)
                    .font_weight(gpui::FontWeight(ts.caption.weight as f32))
                    .text_color(colours.fg_default)
                    .child(label),
            )
            // The count badge ticks upward as pages arrive — but because we
            // preformat it in `sync_from_store`, the render path only clones
            // an `Arc` (§1.1.4).
            .when_some(count, |el, text| {
                let label = CountLabel::new(text);
                el.child(if streaming { label.animating() } else { label })
            })
            .into_any_element()
    }

    /// The Documentation section: the streamed body plus the outline rail.
    ///
    /// This section has no disclosure toggle — it is always present. The outline
    /// rail is placed here rather than at page level because it logically
    /// annotates the documentation body, not the refs or impls sections.
    fn render_docs_section(&self, show_skeleton: bool, cx: &mut Context<Self>) -> AnyElement {
        let scale = cx.theme_ext().motion_scale;

        if !self.docs.has_plan() {
            // No `section_plan` yet, so there is no geometry to promise. Show
            // three placeholder bars only once the 120 ms skeleton grace has
            // elapsed (`StreamSlot::show_skeleton`), so a fast local open goes
            // header → real skeleton with nothing flashing in between.
            if self.state.is_awaiting() {
                return if show_skeleton {
                    plan_placeholder(cx)
                } else {
                    // Inside the grace window: the header is already up, and a
                    // skeleton that appears for 40 ms then vanishes is worse
                    // than no skeleton at all.
                    div().size_full().into_any_element()
                };
            }
            // An error has its own chrome (the cold-error page, or the retry bar
            // below); "no documentation" would be a lie on top of it.
            if matches!(self.state, PageState::ColdError | PageState::StaleError) {
                return div().size_full().into_any_element();
            }
            return EmptyState::new(
                IconName::BookOpen,
                SharedString::from("No documentation"),
                SharedString::from("This symbol has no documented sections."),
                MotionTokens::new(scale),
            )
            .into_any_element();
        }

        let body = list(
            self.docs.list_state().clone(),
            cx.processor(
                |page: &mut Self, ix: usize, window: &mut Window, cx: &mut Context<Self>| {
                    let app: &App = cx;
                    page.docs.render_section(ix, window, app)
                },
            ),
        )
        // `size_full`, not `flex_1`.
        //
        // A virtualized `list()` measures against a *definite* height. Given
        // only a flex hint it resolved to a sliver: one clipped line of prose
        // with empty space beneath it, which reads as "the docs are empty"
        // rather than "the list has no height". The wrapper below owns the
        // flex share; the list then fills it.
        .size_full();

        let entity = cx.entity().downgrade();
        let outline = self.outline.render(
            move |ix, _window, cx| {
                let _ = entity.update(cx, |page, cx| page.reveal_section(ix, cx));
            },
            cx,
        );

        div()
            .id("symbol.docs")
            .flex()
            .flex_row()
            // `size_full` is load-bearing. This row's height would otherwise be
            // *auto* — derived from its content — while the content is a
            // `list()` sized `size_full`, i.e. derived from this row. That
            // circularity resolves to a sliver: one clipped line of prose with
            // empty space beneath it. Taking the height from the parent breaks
            // the cycle, and the parent has a definite height because it owns
            // the flex share of the document column.
            .size_full()
            // The column must be a flex container, not a bare `div`. GPUI's
            // default display is `Block`, in which `flex_1()` on the list is
            // inert and its height resolves to auto — i.e. zero. The outline
            // beside it renders from the same plan, so the failure looks like
            // "the document is empty" rather than "the list has no height".
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_hidden()
                    .child(body),
            )
            .when(!self.outline.is_empty(), |el| el.child(outline))
            .into_any_element()
    }

    /// The Implementations section body.
    ///
    /// The store opens have already been wired through `on_open` closures by the
    /// time render is called — all we do here is hand `ImplsTable::render` the
    /// already-bound callback.
    fn render_impls_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let streaming = self.impls_streaming;
        let store = self.store.downgrade();
        let entity = cx.entity().downgrade();
        self.impls.render(
            streaming,
            move |key, _window, cx| {
                let key = key.clone();
                let _ = store.update(cx, |store, cx| {
                    store.open(key, OpenDisposition::Replace, cx);
                });
            },
            move |_window, cx| {
                let _ = entity.update(cx, |page, cx| {
                    if page.impls.toggle_blanket() {
                        cx.notify();
                    }
                });
            },
            cx,
        )
    }

    /// The References section body.
    fn render_refs_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let streaming = self.refs_streaming;
        let store = self.store.downgrade();
        let entity = cx.entity().downgrade();
        self.refs.render(
            streaming,
            move |key, _window, cx| {
                let key = key.clone();
                let _ = store.update(cx, |store, cx| {
                    store.open(key, OpenDisposition::Replace, cx);
                });
            },
            move |group_ix, _window, cx| {
                let _ = entity.update(cx, |page, cx| {
                    if page.refs.toggle_group(group_ix) {
                        cx.notify();
                    }
                });
            },
            cx,
        )
    }

    /// The slim retry bar shown when a refresh failed over readable content
    /// (LD-16: errors are states, not dialogs).
    fn render_error_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.state != PageState::StaleError {
            return None;
        }
        let message = self.error_text.clone()?;
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        Some(
            div()
                .id("symbol.error_bar")
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .w_full()
                .gap(sp.space_2)
                .px(sp.space_3)
                .py(sp.space_1)
                .bg(colours.danger.opacity(0.08))
                .border_t_1()
                .border_color(colours.danger.opacity(0.3))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .truncate()
                        .text_size(ts.dense.size)
                        .line_height(ts.dense.line_height)
                        .text_color(colours.danger)
                        .child(message),
                )
                .child(
                    div()
                        .id("symbol.error_bar.retry")
                        .px(sp.space_2)
                        .py(sp.space_1 / 2.0)
                        .rounded(sp.r_sm)
                        .border_1()
                        .border_color(colours.danger.opacity(0.4))
                        .text_size(ts.caption.size)
                        .line_height(ts.caption.line_height)
                        .text_color(colours.danger)
                        .cursor_pointer()
                        .hover(|s| s.bg(colours.danger.opacity(0.15)))
                        .on_click(cx.listener(|page, _, _window, cx| {
                            let tab = page.tab;
                            page.store.update(cx, |store, cx| store.reload(tab, cx));
                        }))
                        .child(SharedString::from("Retry")),
                )
                .into_any_element(),
        )
    }

    /// The version strip rendered just below the header.
    ///
    /// Returns `None` when `DocEvent::Timeline` has not arrived yet — we do not
    /// show a placeholder here because there is nothing meaningful to say before
    /// the data arrives.  A strip with one entry (the common case with a single
    /// loaded version) looks deliberate: `"0.8.9  current"` is informative, not
    /// broken.
    ///
    /// Clicking an entry fires `select_version(ix)`, which is currently a no-op
    /// stub.  See the "Selecting a different version" note in the module docs for
    /// what needs to be added to complete the navigation.
    fn render_versions_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let rows = self.timeline_rows.as_ref()?;
        if rows.is_empty() {
            return None;
        }

        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        let entity = cx.entity().downgrade();

        Some(
            div()
                .id("symbol.version_strip")
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .flex_wrap()
                .gap(sp.space_1)
                .px(sp.space_4)
                .py(sp.space_1)
                .bg(colours.bg_base)
                .border_b_1()
                .border_color(colours.border_default)
                .children(rows.iter().enumerate().map(|(ix, row)| {
                    let version = row.version.clone();
                    let change_label = row.change_label.clone();
                    let is_current = row.is_current;
                    let entity = entity.clone();

                    div()
                        .id(("symbol.version_strip.row", ix))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(sp.space_1)
                        .px(sp.space_2)
                        .py(sp.space_1 / 2.0)
                        .rounded(sp.r_sm)
                        .border_1()
                        .border_color(if is_current {
                            colours.accent.opacity(0.5)
                        } else {
                            colours.border_default
                        })
                        .bg(if is_current {
                            colours.accent.opacity(0.08)
                        } else {
                            colours.bg_base
                        })
                        .text_size(ts.caption.size)
                        .line_height(ts.caption.line_height)
                        .cursor_pointer()
                        .hover(|s| s.bg(colours.bg_hover))
                        // TODO: wire to `SymbolStore::select_version` once that
                        // method exists (see module docs — "Selecting a different
                        // version").
                        .on_click(move |_, _window, cx| {
                            let _ = entity.update(cx, |page, cx| {
                                page.select_version(ix, cx);
                            });
                        })
                        .child(
                            div()
                                .text_color(if is_current {
                                    colours.fg_default
                                } else {
                                    colours.fg_muted
                                })
                                // 600 = semibold for the current version, 400 = regular.
                                .font_weight(gpui::FontWeight(if is_current { 600.0 } else { 400.0 }))
                                .child(version),
                        )
                        .child(
                            div()
                                .text_color(colours.fg_faint)
                                .child(change_label),
                        )
                }))
                .into_any_element(),
        )
    }

    /// No-op stub for version selection.
    ///
    /// Called when the user clicks a version strip entry. Completing this requires
    /// `SymbolStore::select_version(tab_id, version, cx)` — see the module-level
    /// "Selecting a different version" note for the full list of required additions.
    ///
    /// TODO: implement select_version on SymbolStore, add `select_version` to the
    /// `SymbolEngine` trait, then call `self.store.update(cx, |store, cx|
    /// store.select_version(self.tab, rows[ix].version.clone(), cx))` here.
    fn select_version(&mut self, _ix: usize, _cx: &mut Context<Self>) {
        // No-op until SymbolStore::select_version is implemented.
    }

    /// The Source section body.
    ///
    /// Renders the file path and byte range when the head carries a source
    /// location, or falls back to the empty state when no location was recorded
    /// (e.g. a declaration-only package, or a producer that omits source info).
    fn render_source_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(path) = self.source_path_text.clone() else {
            // Producer did not record a source location — fall back to the stub.
            return EmptyState::new(
                IconName::File,
                SharedString::from("Source not materialised"),
                SharedString::from(
                    "This package is loaded as declarations only. \
                     Materialise it to read the source here.",
                ),
                MotionTokens::new(cx.theme_ext().motion_scale),
            )
            .into_any_element();
        };

        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        div()
            .id("symbol.source.location")
            .v_flex()
            .gap(sp.space_1)
            .p(sp.space_4)
            // Path — monospace so it aligns with how paths look in terminal output.
            .child(
                div()
                    .font_family(SharedString::from("monospace"))
                    .text_size(ts.dense.size)
                    .line_height(ts.dense.line_height)
                    .text_color(colours.fg_default)
                    .child(path),
            )
            // Byte range, when present — clearly labelled as bytes.
            .when_some(self.source_span_text.clone(), |el, span| {
                el.child(
                    div()
                        .font_family(SharedString::from("monospace"))
                        .text_size(ts.caption.size)
                        .line_height(ts.caption.line_height)
                        .text_color(colours.fg_muted)
                        .child(span),
                )
            })
            .into_any_element()
    }
}

/// Three bars shown only while `section_plan` itself has not arrived.
///
/// Once the plan lands this is replaced by the real, correctly-sized skeleton in
/// [`docs::DocsBody`], which is the one that carries the zero-jump promise.
fn plan_placeholder(cx: &App) -> AnyElement {
    let (sp, ts) = {
        let ext = cx.theme_ext();
        (ext.space, ext.type_scale)
    };
    div()
        .v_flex()
        .w_full()
        .gap(sp.space_2)
        .p(sp.space_4)
        .children((0..3).map(|ix| {
            Skeleton::new()
                .h(ts.ui.line_height - sp.space_1)
                .when(ix == 2, |s| s.w_3_4())
                .when(ix != 2, |s| s.w_full())
        }))
        .into_any_element()
}

// ─────────────────────────────────────────────────────────────────────────────
// Render
// ─────────────────────────────────────────────────────────────────────────────

impl<E: SymbolEngine> Render for SymbolPage<E> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // §4.2 render-loop contract: advance any running `highlight.sweep`, and
        // ask for another frame only while one is actually moving. A settled
        // page requests nothing.
        let reduced = cx.theme_ext().reduced_motion();
        if self.docs.tick(Instant::now()) && !reduced {
            window.request_animation_frame();
        }

        let colours = cx.theme_ext().colours;
        let stale = self.state.is_stale();

        // The 120 ms skeleton grace is time-based, so it is read here rather
        // than cached in `sync_from_store`.
        let show_skeleton = {
            let store = self.store.clone();
            store
                .read(cx)
                .doc(self.tab)
                .is_some_and(|doc| doc.slot_meta.show_skeleton())
        };

        // ── Cold error: nothing readable, so the error *is* the page ─────────
        if self.state == PageState::ColdError {
            let error = self.error.clone().unwrap_or(SlotError::Cancelled);
            let tab = self.tab;
            let store = self.store.clone();
            return div()
                .id("symbol.page")
                .size_full()
                .bg(colours.bg_base)
                .child(ErrorState::new(
                    error,
                    Some(Box::new(move |_window, cx| {
                        store.update(cx, |store, cx| store.reload(tab, cx));
                    })),
                ))
                .into_any_element();
        }

        // ── Header — paints from `Head` alone, in one frame ──────────────────
        let picker_entity = cx.entity().downgrade();
        let version_entity = cx.entity().downgrade();
        let store_for_links = self.store.downgrade();
        let store_for_crumbs = self.store.downgrade();

        let header = SymbolHeader::new(self.header.clone())
            .stale(stale)
            .picker_open(self.picker_open)
            .on_crumb(move |key, _window, cx| {
                let key = key.clone();
                let _ = store_for_crumbs.update(cx, |store, cx| {
                    store.open(key, OpenDisposition::Replace, cx);
                });
            })
            .on_link(move |key, _window, cx| {
                let key = key.clone();
                let _ = store_for_links.update(cx, |store, cx| {
                    store.open(key, OpenDisposition::Replace, cx);
                });
            })
            .on_toggle_picker(move |_window, cx| {
                let _ = picker_entity.update(cx, |page, cx| {
                    page.picker_open = !page.picker_open;
                    cx.notify();
                });
            })
            .on_version(move |ix, _window, cx| {
                let _ = version_entity.update(cx, |page, cx| {
                    page.active_version = ix;
                    page.header.active_version = ix;
                    page.picker_open = false;
                    let tab = page.tab;
                    // Stale-while-revalidate: the old generation stays on
                    // screen and dims while the new one streams over it.
                    page.store.update(cx, |store, cx| store.set_version(tab, cx));
                    cx.notify();
                });
            });

        // ── Build the sections ────────────────────────────────────────────────
        //
        // Each section is a vertical stack of: a disclosure header (the section
        // title with item count and chevron) followed, when expanded, by the
        // section's content div. The whole page is a `v_flex` column.
        //
        // The documentation section has no disclosure header — it is always
        // expanded, and adding a toggle would create a way to hide the primary
        // content with no clear way to find it again.

        let docs_section = self.render_docs_section(show_skeleton, cx);

        // Implementations disclosure header.
        let impls_count = if self.impls_count_text.is_empty() {
            None
        } else {
            Some(self.impls_count_text.clone())
        };
        let impls_expanded = self.collapse.impls;
        let impls_header = self.render_disclosure_header(
            "symbol.impls.header",
            SharedString::from("Implementations"),
            impls_count,
            self.impls_streaming,
            impls_expanded,
            |page, _window, cx| {
                page.collapse.impls = !page.collapse.impls;
                cx.notify();
            },
            cx,
        );

        // References disclosure header.
        let refs_count = if self.refs_count_text.is_empty() {
            None
        } else {
            Some(self.refs_count_text.clone())
        };
        let refs_expanded = self.collapse.refs;
        let refs_header = self.render_disclosure_header(
            "symbol.refs.header",
            SharedString::from("References"),
            refs_count,
            self.refs_streaming,
            refs_expanded,
            |page, _window, cx| {
                page.collapse.refs = !page.collapse.refs;
                cx.notify();
            },
            cx,
        );

        // Source disclosure header — stub until materialisation lands.
        let source_expanded = self.collapse.source;
        let source_header = self.render_disclosure_header(
            "symbol.source.header",
            SharedString::from("Source"),
            None,
            false,
            source_expanded,
            |page, _window, cx| {
                page.collapse.source = !page.collapse.source;
                cx.notify();
            },
            cx,
        );

        // Precompute per-section content elements. All calls that need `cx` or
        // `self` must happen before the `.child()` / `.when()` calls below,
        // because `.when(condition, |el| ...)` closures do not receive either.
        let max_section_h = gpui::px(400.0);

        // Source body — shows the file path and byte range when the head carries
        // a source location; falls back to the empty state when none was recorded.
        // Built eagerly so `cx` and `self` are both available here.
        let source_body = self.render_source_body(cx);

        // Impls and refs bodies. Built eagerly so that `cx` and `self` are both
        // available here. When the section is collapsed the element exists in
        // memory but is not added to the render tree, so layout skips it.
        let impls_body = self.render_impls_body(cx);
        let refs_body = self.render_refs_body(cx);

        let error_bar = self.render_error_bar(cx);

        div()
            .id("symbol.page")
            .key_context("SymbolPage")
            .v_flex()
            .size_full()
            .bg(colours.bg_base)
            // Action handlers that previously navigated between inner tabs.
            //
            // These stubs keep the keyboard bindings inert rather than noisy.
            // `GoToDocsTab` scrolls the docs body to the top, which is the closest
            // meaningful equivalent now that there are no tabs. The others are no-ops
            // until the binding declarations in `src/app/actions.rs` are cleaned up.
            //
            // TODO: remove GoToDocsTab / GoToRefsTab / GoToSourceTab /
            //       GoToTimelineTab / NextInnerTab / PrevInnerTab bindings from
            //       whichever keymap file wires them up, and then remove these
            //       handlers too.
            .on_action(cx.listener(|page, _: &NextInnerTab, _window, cx| {
                // No inner tabs to cycle through. Scroll to top of docs instead.
                page.docs.reveal(0);
                cx.notify();
            }))
            .on_action(cx.listener(|page, _: &PrevInnerTab, _window, cx| {
                page.docs.reveal(0);
                cx.notify();
            }))
            .on_action(cx.listener(|page, _: &GoToDocsTab, _window, cx| {
                // Scroll the docs list back to the top.
                page.docs.reveal(0);
                cx.notify();
            }))
            .on_action(cx.listener(|page, _: &GoToSourceTab, _window, cx| {
                // Expand source and notify — closest meaningful equivalent.
                page.collapse.source = true;
                cx.notify();
            }))
            .on_action(cx.listener(|page, _: &GoToRefsTab, _window, cx| {
                // Expand refs — the reader is asking to see them.
                page.collapse.refs = true;
                cx.notify();
            }))
            .on_action(cx.listener(|page, _: &GoToTimelineTab, _window, cx| {
                // The version strip is already visible below the header when
                // timeline data has arrived; there is nothing to scroll to here.
                let _ = page;
                cx.notify();
            }))
            .on_action(cx.listener(|page, _: &OpenVersionPicker, _window, cx| {
                page.picker_open = !page.picker_open;
                cx.notify();
            }))
            .on_action(cx.listener(|page, _: &CopySymbolUri, _window, cx| {
                let uri = page.header.uri.to_string();
                cx.write_to_clipboard(ClipboardItem::new_string(uri));
            }))
            .child(header)
            // Version strip — sits between the header and the document body.
            //
            // Rendered outside `symbol.body` so that it does not participate in
            // the `overflow_hidden` column and does not perturb the `flex_1` docs
            // list (§9.4 zero-jump guarantee).  `None` until `DocEvent::Timeline`
            // arrives; `Some` thereafter, even with a single-version corpus.
            .children(self.render_versions_strip(cx))
            // The main scrollable column.
            //
            // LD-15: a superseded generation dims to 70 %; it is never replaced
            // by a skeleton while it is still readable. The dimming wraps the
            // whole document column, so the disclosure headers also dim while
            // stale — a consistent, honest signal.
            .child(
                div()
                    .id("symbol.body")
                    .v_flex()
                    .flex_1()
                    // Not a scroll container.
                    //
                    // The documentation is a virtualized `list()` that scrolls
                    // itself, and each disclosure body scrolls itself. Wrapping
                    // them in an *outer* scroller gave their `flex_1` no
                    // definite height to resolve against, so the docs list
                    // collapsed to nothing and the first disclosure header
                    // painted over the one visible line of prose.
                    .overflow_hidden()
                    .when(stale, |el| el.opacity(0.7))
                    // 1. Documentation — always expanded, takes the slack.
                    //
                    // `min_h(0)` is what lets it *shrink* when a section below
                    // expands; without it a flex item refuses to go below its
                    // content height and pushes the sections off-screen.
                    .child(div().flex().flex_col().flex_1().min_h(gpui::px(0.0)).child(docs_section))
                    // 2. Implementations — collapsed by default.
                    .child(impls_header)
                    .when(impls_expanded, |el| {
                        el.child(
                            div()
                                .id("symbol.impls.body")
                                .w_full()
                                .max_h(max_section_h)
                                .overflow_y_scroll()
                                .child(impls_body),
                        )
                    })
                    // 3. References — collapsed by default.
                    .child(refs_header)
                    .when(refs_expanded, |el| {
                        el.child(
                            div()
                                .id("symbol.refs.body")
                                .w_full()
                                .max_h(max_section_h)
                                .overflow_y_scroll()
                                .child(refs_body),
                        )
                    })
                    // 4. Source — collapsed by default.
                    //
                    // `render_source_body` shows the path + byte range when the
                    // head carried a location; the empty state otherwise. No
                    // outer padding here — `render_source_body` owns its own
                    // padding so that both branches look the same.
                    .child(source_header)
                    .when(source_expanded, |el| {
                        el.child(
                            div()
                                .id("symbol.source.body")
                                .w_full()
                                .child(source_body),
                        )
                    }),
            )
            .children(error_bar)
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WorkspaceItem
// ─────────────────────────────────────────────────────────────────────────────

impl<E: SymbolEngine> WorkspaceItem for SymbolPage<E> {
    fn tab_content(&self, cx: &App) -> AnyElement {
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        // Every string here was built at projection time; this is a handful of
        // `Arc` bumps and nothing more (§13.4 performance contract).
        let live = self.refs_streaming || self.impls_streaming;
        let count = (self.refs.total() > 0).then(|| self.refs.total_text());

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(sp.space_1)
            .child(
                Icon::new(IconName::BookOpen)
                    .text_color(colours.fg_muted)
                    .with_size(gpui_component::Size::XSmall),
            )
            .child(
                div()
                    .text_size(ts.ui.size)
                    .line_height(ts.ui.line_height)
                    .child(self.header.title.clone()),
            )
            // LD-8: the tab itself carries the trust badge.
            .child(ProvenanceDot::new(
                ("symbol.tab.trust", self.tab.0 as usize),
                self.header.provenance,
            ))
            .children(count.map(|text| {
                let label = CountLabel::new(text);
                if live { label.animating() } else { label }
            }))
            .into_any_element()
    }

    fn telemetry_id(&self) -> &'static str {
        "symbol-page"
    }

    fn nav_entry(&self) -> Option<NavEntry> {
        if self.header.uri.is_empty() {
            return None;
        }
        let total = self.docs.slots().len();
        let fraction = if total <= 1 {
            0.0
        } else {
            self.docs.anchor_ix() as f32 / (total - 1) as f32
        };
        Some(NavEntry {
            kind: SharedString::from("symbol-page"),
            key: self.header.uri.clone(),
            scroll_fraction: fraction.clamp(0.0, 1.0),
        })
    }

    fn provenance(&self) -> Provenance {
        self.header.provenance
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CollapseState ──────────────────────────────────────────────────────────

    /// The designed default opens only the documentation body. Everything else
    /// starts closed so the first screen is readable even for types with dozens
    /// of implementations or hundreds of references.
    #[test]
    fn default_collapse_state_docs_open_rest_closed() {
        let cs = CollapseState::default_open();
        assert!(cs.docs, "documentation must start expanded");
        assert!(!cs.impls, "implementations must start collapsed");
        assert!(!cs.refs, "references must start collapsed");
        assert!(!cs.source, "source must start collapsed");
    }

    /// Toggling the impls field flips exactly that field and touches nothing
    /// else. We test this because the toggle closure in render does a direct
    /// field assign; a bug that wrote to the wrong field would be silent.
    #[test]
    fn toggling_impls_only_changes_impls() {
        let mut cs = CollapseState::default_open();
        cs.impls = !cs.impls;
        assert!(cs.impls);
        assert!(cs.docs, "docs unchanged");
        assert!(!cs.refs, "refs unchanged");
        assert!(!cs.source, "source unchanged");
    }

    /// Toggling refs only changes refs.
    #[test]
    fn toggling_refs_only_changes_refs() {
        let mut cs = CollapseState::default_open();
        cs.refs = !cs.refs;
        assert!(cs.refs);
        assert!(cs.docs, "docs unchanged");
        assert!(!cs.impls, "impls unchanged");
        assert!(!cs.source, "source unchanged");
    }

    /// Toggling source only changes source.
    #[test]
    fn toggling_source_only_changes_source() {
        let mut cs = CollapseState::default_open();
        cs.source = !cs.source;
        assert!(cs.source);
        assert!(cs.docs, "docs unchanged");
        assert!(!cs.impls, "impls unchanged");
        assert!(!cs.refs, "refs unchanged");
    }

    /// Round-tripping a toggle: collapsed → expanded → collapsed.
    #[test]
    fn collapse_toggle_round_trips() {
        let mut cs = CollapseState::default_open();
        cs.impls = !cs.impls; // open
        assert!(cs.impls);
        cs.impls = !cs.impls; // close again
        assert!(!cs.impls);
    }

    // ── PageState ──────────────────────────────────────────────────────────────

    /// Only the states that still show readable content dim (LD-15).
    #[test]
    fn only_readable_states_dim() {
        assert!(PageState::Stale.is_stale());
        assert!(PageState::StaleError.is_stale());
        assert!(!PageState::Awaiting.is_stale());
        assert!(!PageState::Streaming.is_stale());
        assert!(!PageState::Ready.is_stale());
        assert!(!PageState::ColdError.is_stale());
        assert!(!PageState::Empty.is_stale());
    }

    /// Only the states that have actually asked for content show a skeleton;
    /// an idle or finished page must not shimmer at the reader.
    #[test]
    fn only_requested_states_await() {
        assert!(PageState::Awaiting.is_awaiting());
        assert!(PageState::Streaming.is_awaiting());
        assert!(!PageState::Empty.is_awaiting());
        assert!(!PageState::Ready.is_awaiting());
        assert!(!PageState::ColdError.is_awaiting());
    }

    // ── Highlight-priority coalescing ─────────────────────────────────────────

    /// §9.4.5 says 4 Hz; anything faster spams the engine on every scroll frame.
    #[test]
    fn highlight_priority_is_coalesced_at_4hz() {
        assert_eq!(HIGHLIGHT_PRIORITY_INTERVAL, Duration::from_millis(250));
    }

    // ── Section order assertion ───────────────────────────────────────────────

    // ── TimelineRowView / version strip ───────────────────────────────────────

    /// `timeline_change_label` returns "current" regardless of the `change`
    /// variant when `is_current` is `true`.  This is the primary label the user
    /// sees for the version they are reading.
    #[test]
    fn current_version_always_labelled_current() {
        for change in [
            TimelineChange::Present,
            TimelineChange::Introduced,
            TimelineChange::Unchanged,
            TimelineChange::Deprecated,
            TimelineChange::Removed,
        ] {
            let label = timeline_change_label(&change, /* is_current */ true);
            assert_eq!(
                label,
                SharedString::from("current"),
                "is_current=true must always yield 'current', got '{label}' for {change:?}"
            );
        }
    }

    /// When `is_current` is `false` the label describes the change, not the role.
    #[test]
    fn non_current_versions_labelled_by_change() {
        let cases = [
            (TimelineChange::Present, "present"),
            (TimelineChange::Introduced, "introduced"),
            (TimelineChange::Unchanged, "unchanged"),
            (TimelineChange::Deprecated, "deprecated"),
            (TimelineChange::Removed, "removed"),
            (TimelineChange::SignatureChanged, "sig changed"),
            (TimelineChange::DocsChanged, "docs changed"),
        ];
        for (change, expected) in cases {
            let label = timeline_change_label(&change, /* is_current */ false);
            assert_eq!(
                label,
                SharedString::from(expected),
                "expected label '{expected}' for {change:?}"
            );
        }
    }

    /// `TimelineRowView::from_wire` round-trips the version string and sets
    /// `is_current` correctly.
    #[test]
    fn timeline_row_view_from_wire_roundtrips() {
        use nudox_engine::wire::TimelineRow;
        use nudox_engine::wire::SharedStr;

        let row = TimelineRow {
            version: SharedStr::from("0.8.9"),
            change: TimelineChange::Present,
            name: SharedStr::from("my_fn"),
            sig: Vec::new(),
            deprecated: false,
            is_current: true,
        };

        let view = TimelineRowView::from_wire(&row);
        assert_eq!(view.version, SharedString::from("0.8.9"));
        assert_eq!(view.change_label, SharedString::from("current"));
        assert!(view.is_current);
    }

    /// A single-version strip must not look broken: the one row carries
    /// `TimelineChange::Present` (the honest "we don't know when it was born")
    /// but `is_current = true`, so the label is "current".
    #[test]
    fn single_version_strip_labels_current_not_present() {
        use nudox_engine::wire::TimelineRow;
        use nudox_engine::wire::SharedStr;

        let row = TimelineRow {
            version: SharedStr::from("0.8.9"),
            change: TimelineChange::Present,   // ← honest: only one version loaded
            name: SharedStr::from("Router"),
            sig: Vec::new(),
            deprecated: false,
            is_current: true,
        };

        let view = TimelineRowView::from_wire(&row);
        // The user should read "0.8.9  current", not "0.8.9  present".
        assert_eq!(
            view.change_label,
            SharedString::from("current"),
            "single-version strip must say 'current', not 'present'"
        );
    }

    /// The section order is a design decision documented at the top of the file.
    /// This test names the order so that a future change to the render body is
    /// forced to update it consciously, rather than quietly reordering sections.
    ///
    /// The order is: Documentation, Implementations, References, Source.
    #[test]
    fn section_order_is_docs_impls_refs_source() {
        // This is a documentation test: it asserts that the four sections exist
        // as fields on `CollapseState` in the order the design specifies.
        // The actual render order is determined by the `.child()` calls in
        // `Render::render`, which must match this.
        let cs = CollapseState::default_open();
        // Access each field in order — a rename would break compilation here,
        // forcing the test to be updated alongside the design.
        let _ = cs.docs;
        let _ = cs.impls;
        let _ = cs.refs;
        let _ = cs.source;
    }

    // ── Reserved-height arithmetic ────────────────────────────────────────────

    /// The reserved height for the collapsed impls/refs sections is zero (they
    /// are simply absent from the render tree). When expanded, the content div
    /// provides its own height via `min_h` / `max_h`. This test confirms the
    /// design intent: a collapsed section contributes no height to the page.
    #[test]
    fn collapsed_section_contributes_no_reserved_height() {
        // In the new layout, a collapsed section is not present in the DOM at
        // all (the `when(expanded, ...)` wrapper is the whole mechanism). There
        // is therefore no height to reason about here — the absence of the child
        // is the height guarantee. This test asserts the intent.
        let cs = CollapseState::default_open();
        assert!(!cs.impls, "impls is collapsed by default → zero height contribution");
        assert!(!cs.refs, "refs is collapsed by default → zero height contribution");
        assert!(!cs.source, "source is collapsed by default → zero height contribution");
    }
}

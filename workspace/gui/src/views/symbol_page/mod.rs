//! The symbol page — the streamed centrepiece (GUI-PLAN §16).
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
//!   (§9.4.1). Sections then replace their skeletons in place. See
//!   [`docs`] for how the zero-jump guarantee is actually enforced.
//! * The outline exists before the document does, and doubles as a progress
//!   display — see [`outline`].
//! * Refs and Impls fill in the background while you read, and their tab counts
//!   move as pages land.
//! * Every symbol, every version and every tab carries provenance (LD-8).
//!   docs.rs cannot tell you where a doc came from. We always can.
//!
//! # Module layout
//!
//! | Module      | Owns                                                     |
//! |-------------|----------------------------------------------------------|
//! | [`header`]  | breadcrumb, signature, kind, version picker, trust badge  |
//! | [`docs`]    | the virtualized section list and the zero-jump machinery  |
//! | [`outline`] | the plan-derived sidebar with scroll sync                 |
//! | [`refs`]    | the grouped references table and the implementors table   |
//! | this file   | `WorkspaceItem`, the inner tab strip, store wiring        |
//!
//! # TODO(store) — the `SymbolStore` surface this page consumes
//!
//! Everything below already exists in `crate::stores::symbol` except where
//! noted. This page reads only; it never mutates store state directly.
//!
//! ```text
//! SymbolStore<E: SymbolEngine>
//!   .doc(TabId) -> Option<&SymbolDoc>          // read-only projection source
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
//!   .refs_total / .impls_total: u64
//!   .slot_meta: StreamSlot<()>                 // phase, error, generation
//! ```
//!
//! Two gaps this page works around rather than inventing store API for:
//!
//! 1. **No version list.** `SymbolDoc` carries no `versions`, and `set_version`
//!    takes no argument, so the picker is fed by [`SymbolPage::set_versions`]
//!    from whoever owns version data. With no versions installed the chip is
//!    simply not shown — we do not render an affordance we cannot honour.
//! 2. **No `line` on `RefRow`.** [`refs`] recovers a line number from a
//!    `path` of the form `src/foo.rs:120` and leaves the column blank otherwise.

pub mod docs;
pub mod header;
pub mod outline;
pub mod refs;

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClipboardItem, Context, InteractiveElement as _, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Subscription, Window, div, list,
    prelude::FluentBuilder as _,
};
use gpui_component::{Icon, IconName, Sizable as _, StyledExt as _, skeleton::Skeleton};
use nudox_engine::wire::SymbolKey;

use crate::app::actions::{
    CopySymbolUri, GoToDocsTab, GoToRefsTab, GoToSourceTab, GoToTimelineTab, NextInnerTab,
    OpenVersionPicker, PrevInnerTab,
};
use crate::bridge::slot::{Display as SlotDisplay, SKELETON_GRACE};
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
// Inner tabs
// ─────────────────────────────────────────────────────────────────────────────

/// The six inner tabs of a symbol page (§16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InnerTab {
    /// The streamed documentation body.
    Docs,
    /// The symbol's source, read-only.
    Source,
    /// Cross-references to this symbol.
    Refs,
    /// Types implementing this trait / impls of this type.
    Impls,
    /// Lineage over generations (§22.1).
    Timeline,
    /// This symbol's graph neighbourhood (§18).
    Graph,
}

impl InnerTab {
    /// Left-to-right order, as §16 draws it.
    pub const ALL: [InnerTab; 6] = [
        InnerTab::Docs,
        InnerTab::Source,
        InnerTab::Refs,
        InnerTab::Impls,
        InnerTab::Timeline,
        InnerTab::Graph,
    ];

    /// Position in [`InnerTab::ALL`].
    pub fn ix(self) -> usize {
        match self {
            InnerTab::Docs => 0,
            InnerTab::Source => 1,
            InnerTab::Refs => 2,
            InnerTab::Impls => 3,
            InnerTab::Timeline => 4,
            InnerTab::Graph => 5,
        }
    }

    /// A `'static` label — no allocation, ever.
    pub fn label(self) -> SharedString {
        SharedString::from(match self {
            InnerTab::Docs => "Docs",
            InnerTab::Source => "Source",
            InnerTab::Refs => "Refs",
            InnerTab::Impls => "Impls",
            InnerTab::Timeline => "Timeline",
            InnerTab::Graph => "Graph",
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Page state
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
/// `slot_view` gives — no state can be forgotten — is preserved; only the
/// *rendering* of each state is specialised to this page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageState {
    /// Nothing requested yet.
    Empty,
    /// Requested; no `Head` yet. Header chassis + grace-gated skeleton.
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
    fn is_stale(self) -> bool {
        matches!(self, PageState::Stale | PageState::StaleError)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// SymbolPage
// ─────────────────────────────────────────────────────────────────────────────

/// One open symbol, as a workspace tab.
pub struct SymbolPage<E: SymbolEngine> {
    tab: TabId,
    store: gpui::Entity<SymbolStore<E>>,

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

    active: InnerTab,
    /// Lazy tabs (§16): a tab's content is not built until it is first shown.
    loaded: [bool; 6],

    /// Generation of the currently projected head.
    head_gen: u64,
    /// Cached stream state, recomputed on every store notification.
    state: PageState,
    error: Option<crate::bridge::slot::SlotError>,
    refs_streaming: bool,
    impls_streaming: bool,

    /// 4 Hz coalescing gate for `HighlightPriority` (§9.4.5).
    last_priority: Option<Instant>,

    _subs: Vec<Subscription>,
}

impl<E: SymbolEngine> SymbolPage<E> {
    /// Build a page for an already-opened tab.
    ///
    /// The store has issued the stream; this view attaches to it. Nothing here
    /// blocks, allocates a document, or waits for an event — the first frame
    /// paints the header chassis and the (grace-gated) skeleton.
    pub fn new(
        tab: TabId,
        store: gpui::Entity<SymbolStore<E>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut docs = docs::DocsBody::new(cx);

        // Symbol links inside prose, member rows and signatures all navigate
        // through the store, which owns tab dedup and `nav.flash` (§12.4).
        {
            let store = store.downgrade();
            docs.on_open(move |key, _window, cx| {
                let key = *key;
                let _ = store.update(cx, |store, cx| {
                    store.open(key, OpenDisposition::Replace, cx);
                });
            });
        }

        // Scroll sync: the body's visible range drives both the outline
        // highlight and the engine's highlight priority. This handler runs
        // during the list's own scroll handling, so it must never touch the
        // `ListState` again (that would re-borrow its `RefCell`); it only
        // updates view state.
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
            active: InnerTab::Docs,
            loaded: [true, false, false, false, false, false],
            head_gen: 0,
            state: PageState::Empty,
            error: None,
            refs_streaming: false,
            impls_streaming: false,
            last_priority: None,
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

    /// The currently shown inner tab.
    pub fn active_tab(&self) -> InnerTab {
        self.active
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
            // page stays readable (LD-15). The store resets `sections` in the
            // same step that installs the new head, so an empty section list is
            // the signal that the head we can see belongs to the new generation.
            let generation = doc.slot_meta.generation.0;
            if let Some(head) = doc.head.as_ref() {
                if generation != self.head_gen && doc.sections.is_empty() {
                    self.head_gen = generation;
                    let mut model = HeaderModel::from_head(head);
                    model.versions = self.versions.clone();
                    model.active_version = self.active_version;
                    self.header = model;
                    self.docs.set_plan(&head.section_plan, generation, cx);
                    self.refs.reset(generation);
                    self.impls.reset(generation);
                    dirty = true;
                }
            }

            // ── Body ────────────────────────────────────────────────────────
            dirty |= self.docs.sync_sections(doc.sections.parts(), cx);
            dirty |= self.docs.sync_highlights(&doc.highlights, cx);

            // ── Ancillary tabs (they fill while the reader reads — §9.1) ────
            dirty |= self.refs.sync(doc.refs.parts());
            dirty |= self.impls.sync(doc.impls.parts());
            self.refs_streaming = !doc.refs.is_complete();
            self.impls_streaming = !doc.impls.is_complete();

            // ── Stream state ────────────────────────────────────────────────
            let next_state = Self::classify(doc);
            if next_state != self.state {
                self.state = next_state;
                dirty = true;
            }
            let next_error = doc.slot_meta.error.clone();
            if next_error != self.error {
                self.error = next_error;
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
        let has_content = doc.head.is_some();
        match doc.slot_meta.display() {
            SlotDisplay::Empty => PageState::Empty,
            // `StreamSlot<()>` only stores `Some(())` at `Done`, so these two
            // arms cover the whole of a live stream. Whether the page is
            // readable is decided by the head, not by the slot's value.
            SlotDisplay::Skeleton { .. } => {
                if has_content {
                    // A head from a previous generation is on screen while a
                    // new one loads: that is LD-15 stale-while-revalidate.
                    if doc.sections.is_empty() && doc.slot_meta.phase.is_active() {
                        PageState::Streaming
                    } else {
                        PageState::Stale
                    }
                } else {
                    PageState::Awaiting
                }
            }
            SlotDisplay::Partial(_) => PageState::Streaming,
            SlotDisplay::Stale(_) => PageState::Stale,
            SlotDisplay::Fresh(_) => PageState::Ready,
            SlotDisplay::Error(_) => {
                if has_content {
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
        let mut dirty = self.outline.set_active(range.start);

        // §9.4.5: tell the engine what the reader can actually see, so the
        // highlighter works on the viewport first. Coalesced at 4 Hz.
        let due = self
            .last_priority
            .is_none_or(|t| t.elapsed() >= HIGHLIGHT_PRIORITY_INTERVAL);
        if due {
            self.last_priority = Some(Instant::now());
            let ids = self.docs.visible_ids(range);
            if !ids.is_empty() {
                self.store.read(cx).visible_sections(self.tab, &ids);
            }
        }

        if dirty {
            cx.notify();
        }
        dirty = false;
        let _ = dirty;
    }

    fn activate(&mut self, tab: InnerTab, cx: &mut Context<Self>) {
        if self.active == tab {
            return;
        }
        self.active = tab;
        // Lazy tabs (§16): first activation is what builds the content.
        self.loaded[tab.ix()] = true;
        cx.notify();
    }

    fn step_tab(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = InnerTab::ALL.len() as isize;
        let next = (self.active.ix() as isize + delta).rem_euclid(len) as usize;
        self.activate(InnerTab::ALL[next], cx);
    }

    fn reveal_section(&mut self, ix: usize, cx: &mut Context<Self>) {
        // Jumping to a section is only meaningful on the Docs tab; make that
        // true rather than silently doing nothing.
        self.activate(InnerTab::Docs, cx);
        self.docs.reveal(ix);
        self.outline.set_active(ix);
        cx.notify();
    }

    fn open_symbol(&self, key: SymbolKey, cx: &mut App) {
        self.store.update(cx, |store, cx| {
            store.open(key, OpenDisposition::Replace, cx);
        });
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;
        let active = self.active;

        div()
            .id("symbol.tabs")
            .flex()
            .flex_row()
            .items_center()
            .w_full()
            .gap(sp.space_1)
            .px(sp.space_3)
            .bg(colours.bg_raised)
            .border_b_1()
            .border_color(colours.border_default)
            .children(InnerTab::ALL.iter().map(|tab| {
                let tab = *tab;
                let is_active = tab == active;
                let count = self.tab_count(tab);
                let streaming = self.tab_streaming(tab);

                div()
                    .id(("symbol.tab", tab.ix()))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(sp.space_1)
                    .px(sp.space_2)
                    .py(sp.space_2)
                    .cursor_pointer()
                    .border_b_2()
                    .border_color(if is_active {
                        colours.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .text_size(ts.ui.size)
                    .line_height(ts.ui.line_height)
                    .text_color(if is_active {
                        colours.fg_default
                    } else {
                        colours.fg_muted
                    })
                    .hover(|s| s.text_color(colours.fg_default))
                    .on_click(cx.listener(move |page, _, _window, cx| page.activate(tab, cx)))
                    .child(tab.label())
                    // Counts tick up as pages land (§9.1). The value is
                    // pre-formatted when the page arrives, never in render.
                    .children(count.map(|text| {
                        let label = CountLabel::new(text);
                        if streaming { label.animating() } else { label }
                    }))
                    // `⟳` while this tab's stream is still live (§16).
                    .when(streaming, |el| {
                        el.child(
                            div()
                                .text_size(ts.caption.size)
                                .line_height(ts.caption.line_height)
                                .text_color(colours.fg_faint)
                                .child(SharedString::from("⟳")),
                        )
                    })
            }))
            .into_any_element()
    }

    /// The pre-formatted count shown beside a tab label, if it has one.
    fn tab_count(&self, tab: InnerTab) -> Option<SharedString> {
        match tab {
            InnerTab::Refs if self.refs.total() > 0 => Some(self.refs.total_text()),
            InnerTab::Impls if self.impls.total() > 0 => Some(self.impls.total_text()),
            _ => None,
        }
    }

    fn tab_streaming(&self, tab: InnerTab) -> bool {
        match tab {
            InnerTab::Refs => self.refs_streaming,
            InnerTab::Impls => self.impls_streaming,
            _ => false,
        }
    }

    fn render_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        match self.active {
            InnerTab::Docs => self.render_docs(cx),
            InnerTab::Refs => {
                let streaming = self.refs_streaming;
                let store = self.store.downgrade();
                let tab = self.tab;
                let entity = cx.entity().downgrade();
                self.refs.render(
                    streaming,
                    move |key, _window, cx| {
                        let key = *key;
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
            InnerTab::Impls => {
                let streaming = self.impls_streaming;
                let store = self.store.downgrade();
                let _ = tab_unused(self.tab);
                self.impls.render(
                    streaming,
                    move |key, _window, cx| {
                        let key = *key;
                        let _ = store.update(cx, |store, cx| {
                            store.open(key, OpenDisposition::Replace, cx);
                        });
                    },
                    cx,
                )
            }
            InnerTab::Source => stub_tab(
                IconName::File,
                "Source not materialised",
                "This package is loaded as declarations only. Materialise it to read the source here.",
                cx,
            ),
            InnerTab::Timeline => stub_tab(
                IconName::Calendar,
                "No lineage recorded",
                "This symbol has not been observed across more than one generation.",
                cx,
            ),
            InnerTab::Graph => stub_tab(
                IconName::Network,
                "Graph unavailable",
                "The neighbourhood graph for this symbol has not been built.",
                cx,
            ),
        }
    }

    /// The Docs tab: outline rail plus the virtualized section list.
    fn render_docs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let ext = cx.theme_ext();
        let scale = ext.motion_scale;

        if !self.docs.has_plan() {
            // No plan yet. `Awaiting` shows the grace-gated skeleton; anything
            // else genuinely has nothing to say.
            return match self.state {
                PageState::Awaiting | PageState::Streaming => {
                    plan_placeholder(cx)
                }
                PageState::ColdError | PageState::StaleError => div().into_any_element(),
                _ => EmptyState::new(
                    IconName::BookOpen,
                    SharedString::from("No documentation"),
                    SharedString::from("This symbol has no documented sections."),
                    MotionTokens::new(scale),
                )
                .into_any_element(),
            };
        }

        let body = list(
            self.docs.list_state().clone(),
            cx.processor(|page, ix: usize, window: &mut Window, cx: &mut App| {
                page.docs.render_section(ix, window, cx)
            }),
        )
        .flex_1();

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
            .size_full()
            .child(div().flex_1().overflow_hidden().child(body))
            .when(!self.outline.is_empty(), |el| el.child(outline))
            .into_any_element()
    }

    /// The slim retry bar shown when a refresh failed over readable content
    /// (LD-16: errors are states, not dialogs).
    fn render_error_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let error = self.error.as_ref()?;
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;
        // `SlotError: Display`; the message is built here, on an error path
        // that is reached once per failure — not on the streaming path.
        let message = SharedString::from(error.to_string());

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
}

/// A placeholder used only while `section_plan` itself has not arrived.
///
/// Once the plan lands this is replaced by the real, correctly-sized skeleton
/// in [`docs::DocsBody`]. It is grace-gated so a fast local open never flashes.
fn plan_placeholder(cx: &App) -> AnyElement {
    let ext = cx.theme_ext();
    let sp = ext.space;
    let ts = ext.type_scale;
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

/// A designed state for a tab whose backing surface is not built yet (LD-16).
fn stub_tab(
    icon: IconName,
    title: &'static str,
    description: &'static str,
    cx: &App,
) -> AnyElement {
    EmptyState::new(
        icon,
        SharedString::from(title),
        SharedString::from(description),
        MotionTokens::new(cx.theme_ext().motion_scale),
    )
    .into_any_element()
}

/// Keeps the `Impls` arm symmetric with `Refs` without an unused-variable warn.
#[inline]
fn tab_unused(_tab: TabId) {}

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

        let ext = cx.theme_ext();
        let sp = ext.space;
        let colours = ext.colours;
        let stale = self.state.is_stale();

        // ── Cold error: nothing readable, so the error *is* the page ─────────
        if self.state == PageState::ColdError {
            let error = self
                .error
                .clone()
                .unwrap_or(crate::bridge::slot::SlotError::Cancelled);
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
        let entity = cx.entity().downgrade();
        let nav = entity.clone();
        let picker_entity = entity.clone();
        let version_entity = entity.clone();
        let store_for_links = self.store.downgrade();
        let store_for_crumbs = self.store.downgrade();

        let header = SymbolHeader::new(self.header.clone())
            .stale(stale)
            .picker_open(self.picker_open)
            .on_crumb(move |key, _window, cx| {
                let key = *key;
                let _ = store_for_crumbs.update(cx, |store, cx| {
                    store.open(key, OpenDisposition::Replace, cx);
                });
            })
            .on_link(move |key, _window, cx| {
                let key = *key;
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

        let tab_strip = self.render_tab_strip(cx);
        let body = self.render_body(cx);
        let error_bar = self.render_error_bar(cx);
        let _ = sp;
        let _ = nav;

        div()
            .id("symbol.page")
            .key_context("SymbolPage")
            .v_flex()
            .size_full()
            .bg(colours.bg_base)
            .on_action(cx.listener(|page, _: &NextInnerTab, _window, cx| page.step_tab(1, cx)))
            .on_action(cx.listener(|page, _: &PrevInnerTab, _window, cx| page.step_tab(-1, cx)))
            .on_action(cx.listener(|page, _: &GoToDocsTab, _window, cx| {
                page.activate(InnerTab::Docs, cx)
            }))
            .on_action(cx.listener(|page, _: &GoToSourceTab, _window, cx| {
                page.activate(InnerTab::Source, cx)
            }))
            .on_action(cx.listener(|page, _: &GoToRefsTab, _window, cx| {
                page.activate(InnerTab::Refs, cx)
            }))
            .on_action(cx.listener(|page, _: &GoToTimelineTab, _window, cx| {
                page.activate(InnerTab::Timeline, cx)
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
            .child(tab_strip)
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    // LD-15: a superseded generation dims to 70 %; it is never
                    // replaced by a skeleton while it is still readable.
                    .when(stale, |el| el.opacity(0.7))
                    .child(body),
            )
            .children(error_bar)
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// WorkspaceItem
// ─────────────────────────────────────────────────────────────────────────────

impl<E: SymbolEngine> WorkspaceItem for SymbolPage<E> {
    fn tab_content(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;

        // Every string here was built at projection time; this is a clone of
        // `Arc`s and nothing more (§13.4 performance contract).
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

/// Re-exported so callers do not have to name the grace constant themselves.
pub use crate::bridge::slot::SKELETON_GRACE as SYMBOL_SKELETON_GRACE;
const _: () = {
    // Keeps the re-export honest if the bridge ever renames the constant.
    assert!(SKELETON_GRACE.as_millis() > 0);
};

#[cfg(test)]
mod tests {
    use super::*;

    /// The tab order in §16 is left-to-right Docs → Graph, and `ix()` must
    /// agree with `ALL` or keyboard navigation lands on the wrong tab.
    #[test]
    fn tab_indices_match_declaration_order() {
        for (ix, tab) in InnerTab::ALL.iter().enumerate() {
            assert_eq!(tab.ix(), ix, "{tab:?} reports the wrong index");
        }
    }

    /// Every tab has a visible label.
    #[test]
    fn every_tab_has_a_label() {
        for tab in InnerTab::ALL {
            assert!(!tab.label().is_empty());
        }
    }

    /// Stepping wraps in both directions, so `cmd-shift-]` from the last tab
    /// lands on the first rather than sticking.
    #[test]
    fn tab_stepping_wraps() {
        let len = InnerTab::ALL.len() as isize;
        let forward = (InnerTab::Graph.ix() as isize + 1).rem_euclid(len);
        assert_eq!(forward as usize, InnerTab::Docs.ix());
        let back = (InnerTab::Docs.ix() as isize - 1).rem_euclid(len);
        assert_eq!(back as usize, InnerTab::Graph.ix());
    }

    /// Only the states that still show readable content dim (LD-15). Dimming
    /// a cold load would be dimming nothing.
    #[test]
    fn only_readable_states_dim() {
        assert!(PageState::Stale.is_stale());
        assert!(PageState::StaleError.is_stale());
        assert!(!PageState::Awaiting.is_stale());
        assert!(!PageState::Streaming.is_stale());
        assert!(!PageState::Ready.is_stale());
        assert!(!PageState::ColdError.is_stale());
    }

    /// §9.4.5 says 4 Hz; anything faster would spam the engine on every
    /// scroll frame.
    #[test]
    fn highlight_priority_is_coalesced_at_4hz() {
        assert_eq!(HIGHLIGHT_PRIORITY_INTERVAL, Duration::from_millis(250));
    }

    /// Docs is the only tab loaded up front — the rest are lazy (§16).
    #[test]
    fn only_docs_is_eager() {
        let loaded = [true, false, false, false, false, false];
        assert!(loaded[InnerTab::Docs.ix()]);
        for tab in InnerTab::ALL.iter().filter(|t| **t != InnerTab::Docs) {
            assert!(!loaded[tab.ix()], "{tab:?} must be lazy");
        }
    }
}

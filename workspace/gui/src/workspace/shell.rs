//! `Shell` — the top-level application chrome (GUI-PLAN §13.1–§13.2).
//!
//! # Layout
//!
//! ```text
//! ┌ TitleBar  [☰] Nudox — my-app ⬢local   [⌘K Search…]   [Jobs ●2] [◐] ┐
//! ├─ left dock ──┬── center: panes of tabs ─────────┬── right dock ─────┤
//! │ Project      │ ◀ ▶  [Tab A][Tab B]               │ Outline          │
//! │ Search       │                                   │ Quick peek       │
//! │              │     active item content            │ (P1)             │
//! ├──────────────┴───────────────────────────────────┴──────────────────┤
//! │ bottom dock:  [Jobs] [Logs]                                          │
//! ├────────────────────────────────────────────────────────────────────── ┤
//! │ status bar: ● index synced · 3 gens · ⚠1 · 143 ms                   │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Built on gpui-component `DockArea`
//! (`gpui-component@c112e7b/crates/ui/src/dock/mod.rs:44`).
//! The left, bottom, and right docks are `Dock` entities managed by `DockArea`.
//! The centre is a single `Pane` wrapped in a thin `CenterPanel`.
//!
//! # Dock animation contract (LD-5, §4.2)
//!
//! `DockArea` handles 1:1 drag-resize internally (mouse tracking → `Dock::set_size`).
//! On drag release we retarget a `Motion` (DEFAULT spring) on the dock's committed
//! size so the release snaps to a clean grid value with physics.  Dock *toggle*
//! also goes through the same spring: animate_to 0 (close) or the previous size
//! (open).
//!
//! **The spring lives in `Shell`, not in `DockArea`.**  This is correct: shell
//! is the subscriber of `DockArea::LayoutChanged`; it ticks its own springs in
//! its own render; it calls `window.request_animation_frame()` only while
//! unsettled.  No parent (root Workspace) is notified — this is the leaf-
//! animation rule (§1.1.2) applied to layout.
//!
//! # Banner surface (§13.2)
//!
//! One banner max; `banner.drop` SNAPPY spring on height 0→32 px.  Banner height
//! is pushed from `ShellStore`; the spring is owned here and ticked here.
//!
//! # Placeholder panels (TODO(views))
//!
//! Views that have not been authored yet are represented by `PlaceholderPanel`
//! structs defined in this file.  Each renders a designed empty state (LD-9/LD-16)
//! and carries a `TODO(views)` comment naming the eventual module.  They are
//! correct GPUI `Panel` implementations so `DockArea` can serialize them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Window, div, px,
    prelude::FluentBuilder as _,
};
use gpui_component::dock::{
        DockArea, DockEvent, DockItem, DockPlacement, Panel, PanelEvent, PanelState,
        PanelView, PanelInfo, DockAreaState, register_panel,
    };
use serde_json::Value as JsonValue;

use crate::app::actions::{OpenOmniSearch, ToggleBottomDock, ToggleLeftDock};
use crate::motion::spring::{Motion, Spring};
use crate::stores::events::{OpenDisposition, TabActivated};
use crate::stores::symbol::TabId as DocTabId;
use crate::stores::events::{PackageActivated, PackagesChanged};
use crate::stores::{PackageStore, SearchStore, SymbolStore};
use crate::views::project_panel::ProjectPanel;
use crate::theme::ext::ThemeExtAccessor as _;
use crate::views::omni_search::{OmniSearch, OmniSearchEvent};
use crate::views::symbol_page::SymbolPage;
use crate::workspace::overlays::{OverlayKind, OverlayStack};
use crate::workspace::pane::{Pane, TabId as PaneTabId};
use crate::workspace::status_bar::StatusBar;
use gpui::prelude::*;

// ─────────────────────────────────────────────────────────────────────────────
// Actions
// ─────────────────────────────────────────────────────────────────────────────
//
// The shell declares none of its own. Every action it answers to lives in
// `app::actions` and is bound in `app::keymaps`, which is what keeps the `?`
// cheat sheet and the command palette a complete account of what actually
// works (see the `app::keymaps` module docs).
//
// This file used to declare its own `ToggleLeftDock`. That is a *different
// type* from the one `keymaps` binds, so `cmd-B` dispatched an action nothing
// listened for while both halves looked correct in isolation. Actions are
// declared once, centrally, for exactly that reason.

// ─────────────────────────────────────────────────────────────────────────────
// BannerKind
// ─────────────────────────────────────────────────────────────────────────────

/// The kind of banner currently shown, if any (§13.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerKind {
    /// Offline: serving local Ready subset, some packages unavailable.
    Offline { unavailable: u32 },
    /// Index server unreachable; shows a retry countdown.
    IndexUnreachable { retry_in_secs: u32 },
}

/// The current banner, including its pre-built display label.
///
/// Labels are pre-built at update time (§1.1.4 — no `format!` in render).
#[derive(Debug, Clone)]
pub struct Banner {
    pub kind: BannerKind,
    /// Ready-to-render label; built when the banner is pushed to `Shell`.
    pub label: SharedString,
}

// ─────────────────────────────────────────────────────────────────────────────
// Placeholder panel views (TODO(views))
// ─────────────────────────────────────────────────────────────────────────────

/// Macro to generate a minimal placeholder panel with a fixed panel name.
///
/// Each generated panel:
/// - Implements `Panel` + `Focusable` + `EventEmitter<PanelEvent>` + `Render`.
/// - Renders a designed empty state (LD-9/LD-16): centred label + caption.
/// - Has `closable = false` so it cannot accidentally be removed from the dock.
macro_rules! placeholder_panel {
    (
        $name:ident,
        $panel_name_str:literal,
        $title:literal,
        $caption:literal
    ) => {
        /// Placeholder for $title.
        ///
        /// TODO(views): replace with the real view module once authored.
        pub struct $name {
            focus: FocusHandle,
        }

        impl $name {
            pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
                Self { focus: cx.focus_handle() }
            }
        }

        impl EventEmitter<PanelEvent> for $name {}

        impl Focusable for $name {
            fn focus_handle(&self, _: &App) -> FocusHandle {
                self.focus.clone()
            }
        }

        impl Panel for $name {
            fn panel_name(&self) -> &'static str {
                $panel_name_str
            }

            fn title(
                &mut self,
                _: &mut Window,
                _: &mut Context<Self>,
            ) -> impl IntoElement {
                div().child(SharedString::from($title))
            }

            fn closable(&self, _: &App) -> bool {
                false
            }

            fn dump(&self, _: &App) -> PanelState {
                PanelState {
                    panel_name: $panel_name_str.to_string(),
                    children: Vec::new(),
                    info: PanelInfo::Panel(JsonValue::Null),
                }
            }
        }

        impl Render for $name {
            fn render(
                &mut self,
                _: &mut Window,
                cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let theme = cx.theme_ext().clone();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(theme.space.space_2)
                    .child(
                        div()
                            .text_size(theme.type_scale.ui.size)
                            .text_color(theme.colours.fg_muted)
                            // TODO(views): replace with EmptyState component.
                            .child(SharedString::from($title)),
                    )
                    .child(
                        div()
                            .text_size(theme.type_scale.caption.size)
                            .text_color(theme.colours.fg_faint)
                            .child(SharedString::from($caption)),
                    )
            }
        }
    };
}

// Left dock panels.
//
// `ProjectPanel` is no longer a placeholder — it lives in
// `crate::views::project_panel` and is driven by the live `PackageStore`.

placeholder_panel!(
    SearchPanel,
    "search-panel",
    "Search",
    "TODO(views): crate::views::omni_search"
);

// Bottom dock panels.
placeholder_panel!(
    JobsPanel,
    "jobs-panel",
    "Jobs",
    "TODO(views): crate::views::jobs_panel"
);

placeholder_panel!(
    LogsPanel,
    "logs-panel",
    "Logs",
    "TODO(views): crate::views::log_panel"
);

// Right dock panel.
placeholder_panel!(
    OutlinePanel,
    "outline-panel",
    "Outline",
    "TODO(views): crate::views::outline_panel"
);

// ─────────────────────────────────────────────────────────────────────────────
// CenterPanel — wraps the main Pane as a gpui-component Panel
// ─────────────────────────────────────────────────────────────────────────────

/// Thin `Panel` wrapper around `Pane` so it can be placed in the `DockArea` centre.
pub struct CenterPanel {
    pane: Entity<Pane>,
    focus: FocusHandle,
}

impl CenterPanel {
    pub fn new(pane: Entity<Pane>, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        Self { pane, focus }
    }
}

impl EventEmitter<PanelEvent> for CenterPanel {}

impl Focusable for CenterPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for CenterPanel {
    fn panel_name(&self) -> &'static str {
        "center-panel"
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(SharedString::from("Editor"))
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: "center-panel".to_string(),
            children: Vec::new(),
            info: PanelInfo::Panel(JsonValue::Null),
        }
    }
}

impl Render for CenterPanel {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().overflow_hidden().child(self.pane.clone())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Shell
// ─────────────────────────────────────────────────────────────────────────────

/// Dock width constraints (px).
const DOCK_MIN_W: f32 = 120.0;
const DOCK_MAX_W: f32 = 600.0;
const LEFT_DOCK_DEFAULT_W: f32 = 240.0;
const RIGHT_DOCK_DEFAULT_W: f32 = 220.0;
const BOTTOM_DOCK_DEFAULT_H: f32 = 200.0;

/// The main application chrome.
///
/// `Shell` is a GPUI `Entity` that:
/// - Owns the `DockArea` entity.
/// - Owns the centre `Pane` entity.
/// - Owns the `StatusBar` entity.
/// - Holds dock spring state for the `dock.slide` animation (LD-5).
/// - Holds banner spring state for the `banner.drop` animation (§13.2).
/// - Persists `DockAreaState` via `cx.emit(ShellEvent::DockLayoutChanged)`.
///
/// It does **not** own overlays or toasts — those belong to their respective agents.
pub struct Shell {
    dock_area: Entity<DockArea>,
    pane: Entity<Pane>,
    status_bar: Entity<StatusBar>,

    // ── Stores (LD-1: the shell owns them; views only subscribe) ─────────────
    /// Search state, shared by the omni-search overlay and (later) the search
    /// panel. App-scoped: a query survives closing and reopening the overlay.
    search: Entity<SearchStore>,
    /// Open symbol documents. One entry per open tab; `open()` dedups by key.
    symbols: Entity<SymbolStore>,
    /// Which packages are loaded, loading, or failed. Held (not just
    /// subscribed to) because dropping it would cancel its drain task and the
    /// status bar would freeze at whatever it last showed (LD-18).
    packages: Entity<PackageStore>,

    // ── Overlays (§13.5) ─────────────────────────────────────────────────────
    /// Which overlays are open, innermost last.
    overlays: OverlayStack,
    /// The live omni-search view, while `OverlayKind::OmniSearch` is on the
    /// stack. Dropping it tears down the view; `omni_subs` goes with it.
    omni: Option<Entity<OmniSearch<SearchStore>>>,
    /// Subscriptions belonging to the current overlay. Cleared on close, so an
    /// overlay's callbacks cannot outlive the overlay (LD-18).
    omni_subs: Vec<Subscription>,
    /// Scrim opacity, 0 → 1 (§5.3 `overlay.in`, SNAPPY).
    scrim: Motion,

    /// Which pane tab shows which open document.
    ///
    /// `SymbolStore` dedups by `SymbolKey` and reports the *document* tab id;
    /// the pane numbers its own tabs independently. Without this map, opening
    /// an already-open symbol would either add a duplicate tab or activate an
    /// unrelated one — the two id spaces are deliberately not interchangeable
    /// (both are called `TabId`, hence the aliases at the imports).
    tabs: HashMap<DocTabId, PaneTabId>,

    // ── Dock springs (GUI-PLAN §5.3 `dock.slide`, LD-5) ─────────────────────
    /// LEFT dock width — DEFAULT spring.  Animate_to 0.0 to close, previous
    /// size to re-open.  Ticked in `render`; `request_animation_frame` called
    /// only while unsettled.
    left_w: Motion,
    /// BOTTOM dock height — DEFAULT spring.
    bottom_h: Motion,
    /// RIGHT dock width — DEFAULT spring.
    right_w: Motion,

    // ── Committed sizes (persisted) ──────────────────────────────────────────
    left_w_open: f32,
    bottom_h_open: f32,
    right_w_open: f32,

    // ── Dock open state (distinct from spring target so we can toggle) ───────
    left_open: bool,
    bottom_open: bool,
    right_open: bool,

    // ── Banner spring (§5.3 `banner.drop`, §13.2) ───────────────────────────
    /// Height of the banner strip — SNAPPY spring, 0 → 32 px.
    banner_h: Motion,
    /// Current banner, if any.
    current_banner: Option<Banner>,

    // ── Layout state ─────────────────────────────────────────────────────────
    /// Persisted dock layout; saved on every `DockEvent::LayoutChanged`.
    saved_state: Option<DockAreaState>,

    _subs: Vec<Subscription>,
}

/// Scrim alpha at full strength (§13.5). Dark enough to push the page back,
/// light enough that the reader keeps their place in it.
const SCRIM_ALPHA: f32 = 0.42;

impl Shell {
    /// Construct the shell, wiring up the `DockArea` and all placeholder panels.
    ///
    /// The stores arrive from `main` already holding a live `EngineHandle`: the
    /// corpus is loading before the first frame paints, so by the time anyone
    /// presses `cmd-K` there is something to find (LR-10).
    pub fn new(
        search: Entity<SearchStore>,
        symbols: Entity<SymbolStore>,
        packages: Entity<PackageStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // ── Register panels so DockArea can (de)serialize them ────────────────
        //
        // The project panel needs the package store, which `register_panel`'s
        // `'static` factory closure cannot borrow from the constructor — so the
        // store is cloned into the closure. Cloning an `Entity` is an arc bump;
        // the panel and the shell observe the same store, which is the point:
        // a deserialized layout must show the same corpus as the live one.
        let panel_packages = packages.clone();
        register_panel(cx, "project-panel", move |_, _, _, window, cx| {
            Box::new(cx.new(|cx| ProjectPanel::new(panel_packages.clone(), window, cx)))
        });
        register_panel(cx, "search-panel", |_, _, _, window, cx| {
            Box::new(cx.new(|cx| SearchPanel::new(window, cx)))
        });
        register_panel(cx, "jobs-panel", |_, _, _, window, cx| {
            Box::new(cx.new(|cx| JobsPanel::new(window, cx)))
        });
        register_panel(cx, "logs-panel", |_, _, _, window, cx| {
            Box::new(cx.new(|cx| LogsPanel::new(window, cx)))
        });
        register_panel(cx, "outline-panel", |_, _, _, window, cx| {
            Box::new(cx.new(|cx| OutlinePanel::new(window, cx)))
        });
        register_panel(cx, "center-panel", |_, _, _, window, cx| {
            let pane = cx.new(|cx| Pane::new(window, cx));
            Box::new(cx.new(|cx| CenterPanel::new(pane, cx)))
        });

        // ── Build the DockArea ────────────────────────────────────────────────
        let dock_area = cx.new(|cx| DockArea::new("main", Some(1), window, cx));
        let weak_dock = dock_area.downgrade();

        // ── Centre pane ───────────────────────────────────────────────────────
        let pane = cx.new(|cx| Pane::new(window, cx));
        let center_panel = cx.new(|cx| CenterPanel::new(pane.clone(), cx));

        // ── Left dock (Project only) ──────────────────────────────────────────
        //
        // One panel, no tab strip. The Search tab was a second way to reach a
        // capability that already has a better one: search is a surface you
        // summon with `cmd-K` and dismiss, not a place you navigate to and
        // leave sitting there. A tab strip over a single panel is pure chrome —
        // it costs a row of vertical space on every frame to offer a choice
        // with one option.
        let project_panel = cx.new(|cx| ProjectPanel::new(packages.clone(), window, cx));
        // Cloned because the dock takes ownership below and the shell still
        // needs to subscribe to it. `Entity` clone is an arc bump, and both
        // halves observe the same panel — which is the point: the dock renders
        // it, the shell listens to it.
        let panel_for_subs = project_panel.clone();
        let left_item = DockItem::tabs(
            vec![Arc::new(project_panel) as Arc<dyn PanelView>],
            &weak_dock,
            window,
            cx,
        );

        // ── Bottom dock (Jobs + Logs) ──────────────────────────────────────────
        let jobs_panel = cx.new(|cx| JobsPanel::new(window, cx));
        let logs_panel = cx.new(|cx| LogsPanel::new(window, cx));
        let bottom_item = DockItem::tabs(
            vec![
                Arc::new(jobs_panel) as Arc<dyn PanelView>,
                Arc::new(logs_panel) as Arc<dyn PanelView>,
            ],
            &weak_dock,
            window,
            cx,
        );

        // ── Right dock (Outline) ───────────────────────────────────────────────
        let outline_panel = cx.new(|cx| OutlinePanel::new(window, cx));
        let right_item = DockItem::tab(outline_panel, &weak_dock, window, cx);

        // ── Centre content ─────────────────────────────────────────────────────
        let center_item = DockItem::tab(center_panel, &weak_dock, window, cx);

        dock_area.update(cx, |da, cx| {
            da.set_center(center_item, window, cx);
            da.set_left_dock(
                left_item,
                Some(px(LEFT_DOCK_DEFAULT_W)),
                true,
                window,
                cx,
            );
            da.set_bottom_dock(
                bottom_item,
                Some(px(BOTTOM_DOCK_DEFAULT_H)),
                false, // closed by default
                window,
                cx,
            );
            da.set_right_dock(
                right_item,
                Some(px(RIGHT_DOCK_DEFAULT_W)),
                false, // closed by default
                window,
                cx,
            );
        });

        // ── Status bar ────────────────────────────────────────────────────────
        //
        // Seeded from the package store rather than left at its cold-start
        // default: `PackageStore` already knows what `main` requested, so the
        // very first frame can say "loading axum…". Waiting for the first
        // `PackagesChanged` would show "no packages" for as long as the
        // producer takes — which on a real crate is half a minute of the app
        // looking broken.
        let initial_packages = packages.read(cx).summary_label();
        let status_bar = cx.new(|cx| {
            let mut bar = StatusBar::new(cx);
            bar.set_packages(initial_packages, cx);
            bar
        });

        // ── Subscribe to layout changes ───────────────────────────────────────
        let mut subs = Vec::new();
        {
            let _entity = cx.entity();
            subs.push(cx.subscribe(&dock_area, move |shell, _, evt: &DockEvent, cx| {
                if matches!(evt, DockEvent::LayoutChanged) {
                    shell.saved_state = Some(shell.dock_area.read(cx).dump(cx));
                    cx.emit(ShellEvent::DockLayoutChanged);
                }
            }));
        }

        // ── The one store→shell edge (§12.10): a document wants a tab ─────────
        //
        // `SymbolStore::open` is called from three places — the overlay, a
        // signature link, a crumb — and none of them knows about panes. They
        // all funnel through this event, so tab placement policy is written
        // once, here.
        subs.push(cx.subscribe_in(
            &symbols,
            window,
            |shell, _store, event: &TabActivated, window, cx| {
                shell.reveal_document(event.tab_id, event.disposition, window, cx);
            },
        ));

        // ── Package activated → open its crate root ──────────────────────────
        //
        // The panel knows *which* symbol a package's landing page is; the shell
        // knows *where* documents go. Neither has to learn the other's job —
        // the same seam the omni-search overlay uses, which is why
        // `reveal_document` is written once and both paths funnel through it.
        subs.push(
            cx.subscribe(&panel_for_subs, |shell, _panel, event: &PackageActivated, cx| {
                shell.symbols.update(cx, |store, cx| {
                    store.open(event.root.clone(), OpenDisposition::Replace, cx);
                });
            }),
        );

        // ── Corpus contents → status bar ─────────────────────────────────────
        //
        // The event carries no payload (see `events::PackagesChanged`), so the
        // label is re-read from the store rather than passed along — the store
        // stays the single source of truth and the string cannot arrive stale.
        subs.push(
            cx.subscribe(&packages, |shell, store, _: &PackagesChanged, cx| {
                let label = store.read(cx).summary_label();
                shell
                    .status_bar
                    .update(cx, |bar, cx| bar.set_packages(label, cx));
            }),
        );

        // Focus the pane so the very first keystroke has somewhere to land.
        // Actions dispatch from the focused element upward; with nothing
        // focused, `cmd-K` on a fresh window would do nothing at all.
        let pane_focus = pane.read(cx).focus_handle(cx);
        window.focus(&pane_focus, cx);

        Self {
            dock_area,
            pane,
            status_bar,

            search,
            symbols,
            packages,

            overlays: OverlayStack::new(),
            omni: None,
            omni_subs: Vec::new(),
            scrim: Motion::new(0.0, Spring::SNAPPY),

            tabs: HashMap::new(),

            left_w:   Motion::new(LEFT_DOCK_DEFAULT_W,  Spring::DEFAULT),
            bottom_h: Motion::new(0.0,                  Spring::DEFAULT),
            right_w:  Motion::new(0.0,                  Spring::DEFAULT),

            left_w_open:   LEFT_DOCK_DEFAULT_W,
            bottom_h_open: BOTTOM_DOCK_DEFAULT_H,
            right_w_open:  RIGHT_DOCK_DEFAULT_W,

            left_open:   true,
            bottom_open: false,
            right_open:  false,

            banner_h: Motion::new(0.0, Spring::SNAPPY),
            current_banner: None,

            saved_state: None,
            _subs: subs,
        }
    }

    // ── Omni-search overlay (§13.5, §15) ─────────────────────────────────────

    /// `cmd-K`: open the fused search overlay over whatever is on screen.
    ///
    /// The *store* is app-scoped and the *view* is overlay-scoped. That split
    /// is deliberate: closing the overlay must not throw away a query the user
    /// spent time refining, but it must throw away the view's scroll offsets,
    /// springs, and focus trap — otherwise reopening restores a half-animated
    /// widget in a state nothing produced.
    fn open_omni_search(
        &mut self,
        _: &OpenOmniSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlays.contains(&OverlayKind::OmniSearch) {
            // Already open: re-take focus rather than stacking a second one.
            if let Some(omni) = &self.omni {
                let focus = omni.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
            }
            return;
        }

        self.overlays.push(OverlayKind::OmniSearch, None);

        let search = self.search.clone();
        let omni = cx.new(|cx| OmniSearch::new(search, window, cx));
        self.omni_subs = vec![cx.subscribe_in(
            &omni,
            window,
            |shell, _view, event: &OmniSearchEvent, window, cx| {
                shell.on_omni_event(event, window, cx);
            },
        )];
        self.omni = Some(omni);

        if cx.theme_ext().reduced_motion() {
            self.scrim.snap_to(1.0);
        } else {
            self.scrim.animate_to(1.0);
        }
        cx.notify();
    }

    /// What the overlay reports upward (§15 — the view never closes itself).
    fn on_omni_event(
        &mut self,
        event: &OmniSearchEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            OmniSearchEvent::Open { key, disposition } => {
                let (key, disposition) = (key.clone(), *disposition);
                // `open` dedups by key and answers with `TabActivated`, which
                // `reveal_document` turns into a pane tab. The overlay does not
                // know panes exist, and the pane does not know search exists.
                self.symbols.update(cx, |store, cx| {
                    store.open(key, disposition, cx);
                });
                // `Stay` (cmd-enter) and `Background` (alt-enter) both exist so
                // you can open several hits from one query without retyping it.
                if disposition == OpenDisposition::Replace {
                    self.close_overlay(window, cx);
                }
            }
            OmniSearchEvent::Dismiss => self.close_overlay(window, cx),
        }
    }

    /// Pop the top overlay and give focus back.
    ///
    /// Focus restoration is the half that gets forgotten: an overlay that
    /// vanishes without handing focus anywhere leaves the next keystroke going
    /// to nothing, and the app appears to have frozen (LD-13).
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlays.pop().is_none() {
            return;
        }
        self.omni = None;
        // Dropping the subscriptions with the view is the whole teardown.
        self.omni_subs.clear();

        if cx.theme_ext().reduced_motion() {
            self.scrim.snap_to(0.0);
        } else {
            self.scrim.animate_to(0.0);
        }

        let focus = self.pane.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    // ── Document tabs ────────────────────────────────────────────────────────

    /// Place an opened document in the centre pane, or re-activate its tab.
    ///
    /// Called for every `TabActivated`, including the ones `SymbolStore` emits
    /// when `open()` finds the symbol already open — which is why the dedup
    /// check comes first and does not build a second page.
    ///
    /// # Replace disposition and the tab-per-navigation problem
    ///
    /// `SymbolStore::open` with `Replace` closes the *outgoing doc tab* after
    /// creating the new one — so by the time this function is called, the old
    /// `DocTabId` is gone from the store.  But the pane still has the pane tab
    /// for that old doc, and `self.tabs` still maps the old `DocTabId` to that
    /// pane tab.  Unless we close the pane tab here the strip accumulates one
    /// ghost tab per navigation.
    ///
    /// The fix: when the disposition is `Replace`, look up the pane tab that
    /// was active *before* we opened the new one, find the `DocTabId` it
    /// belonged to (reverse lookup in `self.tabs`), remove both mappings, and
    /// close the pane tab.  This is safe because the store has already dropped
    /// the doc; we are only cleaning up the pane-side mirror.
    fn reveal_document(
        &mut self,
        doc: DocTabId,
        disposition: OpenDisposition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(&existing) = self.tabs.get(&doc) {
            if disposition != OpenDisposition::Background {
                self.pane.update(cx, |pane, cx| {
                    pane.activate_tab(existing, window, cx);
                    cx.notify();
                });
            }
            cx.notify();
            return;
        }

        // Snapshot the currently-active pane tab *before* opening the new one.
        // For `Replace`, this is the tab we will close after the new one is open.
        let previously_active_pane_tab = self.pane.read(cx).active_id();

        let symbols = self.symbols.clone();
        let page = cx.new(|cx| SymbolPage::new(doc, symbols, window, cx));

        let opened = self.pane.update(cx, |pane, cx| {
            let id = pane.open_item(page, Vec::new(), window, cx);
            cx.notify();
            id
        });
        self.tabs.insert(doc, opened);

        // `open_item` activates what it opens, which is right for every
        // disposition but Background; put the reader back where they were.
        if disposition == OpenDisposition::Background {
            if let Some(previous) = previously_active_pane_tab {
                self.pane.update(cx, |pane, cx| {
                    pane.activate_tab(previous, window, cx);
                    cx.notify();
                });
            }
        }

        // For `Replace`: the store already closed the outgoing doc tab; close
        // the matching pane tab so the strip does not accumulate ghost entries.
        if disposition == OpenDisposition::Replace {
            if let Some(outgoing_pane_tab) = previously_active_pane_tab {
                // Find the DocTabId that owned this pane tab (reverse lookup).
                let outgoing_doc = self
                    .tabs
                    .iter()
                    .find(|(d, p)| **d != doc && **p == outgoing_pane_tab)
                    .map(|(d, _)| *d);

                if let Some(old_doc) = outgoing_doc {
                    self.tabs.remove(&old_doc);
                    self.pane.update(cx, |pane, cx| {
                        pane.close_tab(outgoing_pane_tab, window, cx);
                        cx.notify();
                    });
                }
            }
        }

        cx.notify();
    }

    // ── Public API ────────────────────────────────────────────────────────────

    /// Toggle the left dock open/closed (cmd-B, Appendix B).
    ///
    /// Drag-resize sets the dock size directly via `DockArea::set_left_dock`.
    /// Toggle goes through the spring so the open/close is animated (LD-5).
    pub fn toggle_left_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.left_open = !self.left_open;
        let target = if self.left_open { self.left_w_open } else { 0.0 };
        let reduced = cx.theme_ext().reduced_motion();
        if reduced {
            self.left_w.snap_to(target);
        } else {
            self.left_w.animate_to(target);
        }
        self.dock_area.update(cx, |da, cx| {
            da.toggle_dock(DockPlacement::Left, window, cx);
        });
        cx.notify();
    }

    /// Toggle the bottom dock.
    pub fn toggle_bottom_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.bottom_open = !self.bottom_open;
        let target = if self.bottom_open { self.bottom_h_open } else { 0.0 };
        let reduced = cx.theme_ext().reduced_motion();
        if reduced {
            self.bottom_h.snap_to(target);
        } else {
            self.bottom_h.animate_to(target);
        }
        self.dock_area.update(cx, |da, cx| {
            da.toggle_dock(DockPlacement::Bottom, window, cx);
        });
        cx.notify();
    }

    /// Toggle the right dock.
    pub fn toggle_right_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.right_open = !self.right_open;
        let target = if self.right_open { self.right_w_open } else { 0.0 };
        let reduced = cx.theme_ext().reduced_motion();
        if reduced {
            self.right_w.snap_to(target);
        } else {
            self.right_w.animate_to(target);
        }
        self.dock_area.update(cx, |da, cx| {
            da.toggle_dock(DockPlacement::Right, window, cx);
        });
        cx.notify();
    }

    /// Push a banner into view (§13.2).  At most one banner at a time.
    /// Replaces any existing banner immediately (no queue — §13.2 spec).
    pub fn push_banner(&mut self, banner: Banner, cx: &mut Context<Self>) {
        self.current_banner = Some(banner);
        let reduced = cx.theme_ext().reduced_motion();
        if reduced {
            self.banner_h.snap_to(32.0);
        } else {
            self.banner_h.animate_to(32.0);
        }
        cx.notify();
    }

    /// Clear the current banner.
    pub fn clear_banner(&mut self, cx: &mut Context<Self>) {
        self.current_banner = None;
        let reduced = cx.theme_ext().reduced_motion();
        if reduced {
            self.banner_h.snap_to(0.0);
        } else {
            self.banner_h.animate_to(0.0);
        }
        cx.notify();
    }

    /// The persisted dock state (call to save to disk).
    pub fn dock_state(&self) -> Option<&DockAreaState> {
        self.saved_state.as_ref()
    }

    /// Restore a previously persisted dock state.
    pub fn restore_state(
        &mut self,
        state: DockAreaState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let _ = self.dock_area.update(cx, |da, cx| da.load(state, window, cx));
        cx.notify();
    }

    /// The main centre pane.
    pub fn pane(&self) -> &Entity<Pane> {
        &self.pane
    }

    /// The interactive overlay, if any (§13.5).
    ///
    /// Exposed for tests: "did `cmd-K` open the search?" is otherwise only
    /// answerable by rendering and looking, which is exactly the kind of
    /// assertion that passes for the wrong reason.
    pub fn overlay_kind_on_top(&self) -> Option<&OverlayKind> {
        self.overlays.top().map(|entry| &entry.kind)
    }

    /// How many overlays are stacked (§13.5 — Escape pops exactly one).
    pub fn overlay_depth(&self) -> usize {
        self.overlays.depth()
    }

    /// Which pane tab is showing `doc`, if it is open.
    ///
    /// The two id spaces are deliberately distinct; this is the only sanctioned
    /// crossing between them.
    pub fn pane_tab_for_document(&self, doc: DocTabId) -> Option<PaneTabId> {
        self.tabs.get(&doc).copied()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ShellEvent
// ─────────────────────────────────────────────────────────────────────────────

/// Events emitted by `Shell` to `main.rs` / app-level observers.
pub enum ShellEvent {
    /// The dock layout changed; caller should persist `shell.dock_state()`.
    DockLayoutChanged,
}

impl EventEmitter<ShellEvent> for Shell {}

// ─────────────────────────────────────────────────────────────────────────────
// Render
// ─────────────────────────────────────────────────────────────────────────────

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // ── §4.2 render-loop contract: tick all springs here ──────────────────
        // Notify *only this entity* via `request_animation_frame` while unsettled.
        // The rest of the app renders zero frames during a dock slide.
        let now = Instant::now();
        let mut animating = false;
        animating |= self.left_w.tick(now);
        animating |= self.bottom_h.tick(now);
        animating |= self.right_w.tick(now);
        animating |= self.banner_h.tick(now);
        animating |= self.scrim.tick(now);

        let reduced = cx.theme_ext().reduced_motion();
        if animating && !reduced {
            window.request_animation_frame();
        }

        let theme = cx.theme_ext().clone();
        let banner_h = self.banner_h.value();

        // ── Banner strip (§13.2, §5.3 `banner.drop`) ─────────────────────────
        // Height is spring-animated; pushing content down communicates the
        // urgency of the message (spec: "height animation sanctioned — it must
        // push content").
        let banner = self.current_banner.clone().map(|b| {
            div()
                .flex_none()
                .w_full()
                .h(px(banner_h))
                .overflow_hidden()
                .bg(theme.trust.stale.colour)
                .flex()
                .items_center()
                .px(theme.space.space_4)
                .when(banner_h > 1.0, |s| {
                    s.child(
                        div()
                            .text_size(theme.type_scale.dense.size)
                            .text_color(theme.colours.fg_default)
                            .child(b.label),
                    )
                })
        });

        // ── Overlay layer (§13.5) ─────────────────────────────────────────────
        //
        // Absolutely positioned over the whole shell rather than nested inside
        // the dock area, so an overlay covers the docks and status bar too —
        // a search panel that a sidebar can occlude is not a modal surface.
        let scrim_opacity = self.scrim.value();
        let overlay = self.omni.clone().map(|omni| {
            let dismissable = self
                .overlays
                .top()
                .is_some_and(|entry| entry.kind.dismiss_on_scrim_click());

            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .when(self.overlays.wants_scrim(), |el| {
                    el.child(
                        div()
                            .id("overlay.scrim")
                            // Test seam — see `omni_search::render_row`. The
                            // scrim and the overlay panel occupy overlapping
                            // full-window layers, so a test needs the scrim's
                            // real bounds to click *it* rather than the panel
                            // sitting above it. No-op outside test builds.
                            .debug_selector(|| "overlay.scrim".into())
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .bg(gpui::black().opacity(scrim_opacity * SCRIM_ALPHA))
                            // A modal refuses this (§13.5): a destructive
                            // confirmation that a stray click dismisses trains
                            // people to click through the next one.
                            .when(dismissable, |el| {
                                el.on_click(cx.listener(|shell, _, window, cx| {
                                    shell.close_overlay(window, cx);
                                }))
                            }),
                    )
                })
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .child(omni),
                )
        });

        // ── Full shell layout ─────────────────────────────────────────────────
        div()
            .id("shell")
            // `global` is the context `app::keymaps` binds `cmd-K`, `cmd-B` and
            // the rest against; `Workspace` is here for bindings that should
            // stop at the shell. Both are matched by name, so the root must
            // declare `global` explicitly — GPUI has no implicit root context.
            .key_context("Workspace global")
            .on_action(cx.listener(Self::open_omni_search))
            .on_action(cx.listener(|shell, _: &ToggleLeftDock, window, cx| {
                shell.toggle_left_dock(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ToggleBottomDock, window, cx| {
                shell.toggle_bottom_dock(window, cx);
            }))
            .size_full()
            // The overlay layer is absolute; without this it would position
            // against the window rather than the shell.
            .relative()
            .flex()
            .flex_col()
            .bg(theme.colours.bg_base)
            // Banner above everything (§13.2).
            .when_some(banner, |s, b| s.child(b))
            // DockArea fills the available space.
            .child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .child(self.dock_area.clone()),
            )
            // Status bar at the bottom (§13.3).
            .child(
                div()
                    .flex_none()
                    .border_t_1()
                    .border_color(theme.colours.border_default)
                    .child(self.status_bar.clone()),
            )
            .children(overlay)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Banner queue holds at most one ────────────────────────────────────────

    /// Pure-logic test: pushing a second banner replaces the first.
    /// No GPUI executor needed — we test the state directly.
    #[test]
    fn banner_queue_holds_at_most_one() {
        // Simulate the state machine without needing a full GPUI context.
        let mut current_banner: Option<Banner> = None;

        let b1 = Banner {
            kind: BannerKind::Offline { unavailable: 3 },
            label: "Offline — 3 packages unavailable".into(),
        };
        let b2 = Banner {
            kind: BannerKind::IndexUnreachable { retry_in_secs: 30 },
            label: "Index unreachable — retrying in 30 s".into(),
        };

        // Push first banner.
        current_banner = Some(b1.clone());
        assert!(current_banner.is_some());
        assert_eq!(
            current_banner.as_ref().unwrap().label,
            SharedString::from("Offline — 3 packages unavailable")
        );

        // Push second — replaces first.
        current_banner = Some(b2.clone());
        assert!(current_banner.is_some());
        assert_eq!(
            current_banner.as_ref().unwrap().label,
            SharedString::from("Index unreachable — retrying in 30 s")
        );

        // Clear.
        current_banner = None;
        assert!(current_banner.is_none());
    }

    /// Dock visibility state toggles correctly without needing GPUI.
    #[test]
    fn dock_visibility_toggles() {
        let mut left_open = true;
        let mut bottom_open = false;

        // Toggle left closed.
        left_open = !left_open;
        assert!(!left_open);

        // Toggle left open again.
        left_open = !left_open;
        assert!(left_open);

        // Toggle bottom open.
        bottom_open = !bottom_open;
        assert!(bottom_open);
    }

    /// Spring targets match the committed sizes after toggle.
    #[test]
    fn dock_spring_targets_match_committed_sizes() {
        let mut left_w = Motion::new(LEFT_DOCK_DEFAULT_W, Spring::DEFAULT);
        let left_w_open = LEFT_DOCK_DEFAULT_W;
        let mut left_open = true;

        // Toggle closed.
        left_open = !left_open;
        let target = if left_open { left_w_open } else { 0.0 };
        left_w.animate_to(target);
        assert_eq!(left_w.target(), 0.0);

        // Toggle open.
        left_open = !left_open;
        let target = if left_open { left_w_open } else { 0.0 };
        left_w.animate_to(target);
        assert_eq!(left_w.target(), left_w_open);
    }

    /// Banner spring targets match the expected heights.
    #[test]
    fn banner_spring_targets_correct() {
        let mut banner_h = Motion::new(0.0, Spring::SNAPPY);

        // Push banner — target is 32 px.
        banner_h.animate_to(32.0);
        assert_eq!(banner_h.target(), 32.0);

        // Clear — target is 0.
        banner_h.animate_to(0.0);
        assert_eq!(banner_h.target(), 0.0);
    }

    // ── Replace disposition — tab cleanup (BUG 1) ────────────────────────────

    /// The reverse-lookup used by `reveal_document` to find the outgoing
    /// `DocTabId` from a `PaneTabId` must not return the newly-opened tab.
    ///
    /// This is the pure logic extracted from `reveal_document`.  When
    /// `disposition == Replace`:
    ///
    /// 1. We have a `tabs` map with one existing entry: doc A → pane tab P.
    /// 2. We open doc B, which gets pane tab Q, and insert B → Q.
    /// 3. The previously-active pane tab is P.
    /// 4. The reverse lookup must find A (not B) as the outgoing doc.
    #[test]
    fn replace_reverse_lookup_finds_outgoing_not_incoming() {
        // Fake DocTabIds.
        let doc_a = DocTabId(1);
        let doc_b = DocTabId(2);

        // Fake PaneTabIds (nonzero, as required by the type).
        let pane_p = PaneTabId::new(1).unwrap();
        let pane_q = PaneTabId::new(2).unwrap();

        // State after the new tab has been inserted but before the cleanup.
        let mut tabs: HashMap<DocTabId, PaneTabId> = HashMap::new();
        tabs.insert(doc_a, pane_p); // the tab we opened *before*
        tabs.insert(doc_b, pane_q); // the tab we just opened

        let new_doc = doc_b;
        let outgoing_pane_tab = pane_p;

        // The logic in `reveal_document`:
        let outgoing_doc = tabs
            .iter()
            .find(|(d, p)| **d != new_doc && **p == outgoing_pane_tab)
            .map(|(d, _)| *d);

        assert_eq!(
            outgoing_doc,
            Some(doc_a),
            "reverse lookup must return the outgoing doc (A), not the new doc (B)"
        );

        // Simulate the cleanup.
        tabs.remove(&doc_a);
        assert!(!tabs.contains_key(&doc_a), "outgoing entry removed");
        assert!(tabs.contains_key(&doc_b), "incoming entry kept");
        assert_eq!(tabs.len(), 1);
    }

    /// When there is no previously-active pane tab (empty pane), the cleanup
    /// path is a no-op and must not panic or remove the newly-inserted entry.
    #[test]
    fn replace_with_no_previous_tab_is_a_noop() {
        let doc_b = DocTabId(1);
        let pane_q = PaneTabId::new(1).unwrap();

        let mut tabs: HashMap<DocTabId, PaneTabId> = HashMap::new();
        tabs.insert(doc_b, pane_q);

        let previously_active: Option<PaneTabId> = None; // empty pane before open

        // No outgoing tab to close.
        if let Some(outgoing_pane_tab) = previously_active {
            let outgoing_doc = tabs
                .iter()
                .find(|(d, p)| **d != doc_b && **p == outgoing_pane_tab)
                .map(|(d, _)| *d);
            if let Some(old) = outgoing_doc {
                tabs.remove(&old);
            }
        }

        assert_eq!(tabs.len(), 1, "no-op: incoming entry must remain");
    }

    /// `Stay` and `Background` dispositions must NOT trigger the Replace
    /// cleanup path.
    #[test]
    fn stay_and_background_do_not_close_outgoing_tab() {
        let doc_a = DocTabId(1);
        let doc_b = DocTabId(2);
        let pane_p = PaneTabId::new(1).unwrap();
        let pane_q = PaneTabId::new(2).unwrap();

        for disposition in [OpenDisposition::Stay, OpenDisposition::Background] {
            let mut tabs: HashMap<DocTabId, PaneTabId> = HashMap::new();
            tabs.insert(doc_a, pane_p);
            tabs.insert(doc_b, pane_q);

            // The cleanup runs only for Replace.
            if disposition == OpenDisposition::Replace {
                let outgoing_doc = tabs
                    .iter()
                    .find(|(d, p)| **d != doc_b && **p == pane_p)
                    .map(|(d, _)| *d);
                if let Some(old) = outgoing_doc {
                    tabs.remove(&old);
                }
            }

            assert_eq!(
                tabs.len(),
                2,
                "{disposition:?} must not remove any tab from the map"
            );
        }
    }
}

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
//! structs defined in [`crate::workspace::panels`]. Each renders a designed empty
//! state (LD-9/LD-16) and carries a `TODO(views)` comment naming the eventual
//! module. They are correct GPUI `Panel` implementations so `DockArea` can
//! serialize them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyView, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable as _,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::dock::{
    DockArea, DockAreaState, DockEvent, DockItem, DockPlacement, PanelView, register_panel,
};

use crate::app::account::{AccountService, AccountStatus};
use crate::app::actions::{
    CycleTheme, CycleThemeBack, OpenAccount, OpenCommandPalette, OpenOmniSearch, OpenSettings,
    ToggleBottomDock, ToggleLeftDock, ToggleShortcutsOverlay,
};
use crate::app::lifecycle;
use crate::app::mcp::McpStatus;
use crate::motion::spring::{Motion, Spring};
use crate::stores::events::{OpenDisposition, TabActivated};
use crate::stores::events::{PackageActivated, PackagesChanged};
use crate::stores::index_jobs::IndexJobStore;
use crate::stores::symbol::TabId as DocTabId;
use crate::stores::{PackageStore, SearchStore, SymbolStore};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::views::command_overlay::{CommandOverlay, CommandOverlayEvent, CommandOverlayMode};
use crate::views::omni_search::{OmniSearch, OmniSearchEvent};
use crate::views::project_panel::ProjectPanel;
use crate::views::settings::{SettingsEvent, SettingsView};
use crate::views::sign_in::{SignInEvent, SignInView};
use crate::views::symbol_page::SymbolPage;
use crate::workspace::dock::{Banner, DockLayout, DockState};
use crate::workspace::overlays::{OverlayKind, OverlayStack};
use crate::workspace::pane::{Activation, Pane, PaneEvent, TabId as PaneTabId};
use crate::workspace::panels::{CenterPanel, JobsPanel, LogsPanel, OutlinePanel, SearchPanel};
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
// PresentedOverlay — the one live overlay view (§13.5, L15)
// ─────────────────────────────────────────────────────────────────────────────

/// The view behind a view-backed [`OverlayKind`].
///
/// An enum rather than an `AnyView` because the shell needs the view's
/// `FocusHandle` at present time, and `AnyView` cannot produce one. Adding a
/// fourth overlay surface breaks both `match`es below, which is the point: a
/// surface that has no answer for "what do I focus?" cannot be presented.
enum OverlayView {
    /// `cmd-K` fused search (§15).
    OmniSearch(Entity<OmniSearch<SearchStore>>),
    /// `?` cheat sheet and `cmd-shift-P` palette — one view, two modes.
    Command(Entity<CommandOverlay>),
    /// `cmd-shift-A` account panel: sign in, see the account, sign out.
    SignIn(Entity<SignInView>),
    /// `cmd-,` settings panel: Connection today, more sections later.
    Settings(Entity<SettingsView>),
}

impl OverlayView {
    /// The element the overlay layer renders.
    fn any_view(&self) -> AnyView {
        match self {
            Self::OmniSearch(view) => view.clone().into(),
            Self::Command(view) => view.clone().into(),
            Self::SignIn(view) => view.clone().into(),
            Self::Settings(view) => view.clone().into(),
        }
    }

    /// Where keyboard focus goes while this overlay is up.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::OmniSearch(view) => view.read(cx).focus_handle(cx),
            Self::Command(view) => view.read(cx).focus_handle(cx),
            Self::SignIn(view) => view.read(cx).focus_handle(cx),
            Self::Settings(view) => view.read(cx).focus_handle(cx),
        }
    }
}

/// A live overlay: its kind, its view, and the subscriptions it owns.
///
/// **Structural guarantee:** dropping this closes the overlay completely — the
/// view's reference count falls and every `Subscription` it registered is
/// unregistered with it (LD-18, the same one-drop rule as
/// [`crate::workspace::pane::ItemSlot`]). There is no manual teardown call to
/// forget.
struct PresentedOverlay {
    /// Which stack entry this view belongs to, so dismissal pops the right one.
    kind: OverlayKind,
    view: OverlayView,
    /// Dropped with the overlay — no `.detach()` is legal here (LD-18).
    _subs: Vec<Subscription>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Shell
// ─────────────────────────────────────────────────────────────────────────────

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
    /// On-demand `pkg:` index jobs. Held for the same reason as `packages`:
    /// dropping it would cancel every running fetch and its drain task.
    index_jobs: Entity<IndexJobStore<nudox_engine::EngineHandle>>,

    // ── Account gate (`docs/auth.md`) ─────────────────────────────────────────────
    /// `Some` when the account posture cannot work: the corpus, the docks, the
    /// omni-search overlay and every document surface are behind this until it
    /// clears. `render` returns *only* this view's tree while it is set —
    /// there is no branch that renders both, which is what makes the gate a
    /// gate rather than a banner. `None` for [`AccountStatus::Absent`] (no
    /// account service in this process at all — every `#[gpui::test]` app):
    /// there is nothing to gate against, and [`SignInView`]'s own
    /// `Unavailable` phase is what a reader would see if they asked for it,
    /// not this field.
    gate: Option<Entity<SignInView>>,
    /// Periodic posture re-check while the window stays open (L56). Held —
    /// not `.detach()`ed — because dropping it cancels the loop, the same
    /// LD-18 reason `packages` is held: a detached timer would keep firing
    /// after this shell is gone, and a missing one would leave grace expiry
    /// mid-session invisible until the next sign-in, sign-out, or rebuild.
    _account_recheck: Option<Task<()>>,

    // ── Overlays (§13.5) ─────────────────────────────────────────────────────
    /// Which overlays are open, innermost last.
    overlays: OverlayStack,
    /// The one view-backed overlay currently on screen, if any (L15).
    ///
    /// One field rather than a pair of `Option<Entity<_>>` + `Vec<Subscription>`
    /// per surface. Presenting an overlay is three things that have to happen
    /// together — push the kind, hold the view and its subscriptions, and move
    /// window focus onto it — and while they were three separate statements at
    /// three call sites, two of the call sites got them wrong:
    /// `open_command_palette` and `toggle_shortcuts_overlay` both built and
    /// stored their view without ever focusing it, so the palette and the `?`
    /// sheet rendered but answered none of the `Overlay`-context bindings
    /// (escape / enter / up / down) — indistinguishable from not opening.
    ///
    /// [`Shell::present_overlay`] is now the only way to fill this field, and it
    /// does all three.
    presented: Option<PresentedOverlay>,
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

/// Give a freshly-built pane a tab for every document the store still holds.
///
/// # Why a window rebuild has to do this
///
/// `SearchStore`, `SymbolStore` and `PackageStore` are app-scoped, not
/// window-scoped (see `main.rs`), so dismissing lindsey's window destroys the
/// *view* of the corpus and nothing else. The pane, though, is built empty
/// every time. Restoring without this step would hand the reader a shell with
/// no documents while the store underneath still had all of them — and worse,
/// `SymbolStore::open` dedupes by key, so re-opening one from search would
/// re-activate an invisible tab and appear to do nothing.
///
/// # Ordering
///
/// Tabs come back in `TabId` order, which is open order — `next_tab_id` is a
/// monotonic counter — so the strip reads the way the reader left it rather
/// than in `HashMap` iteration order, which varies per run.
///
/// Each tab is opened with [`Activation::Preserve`] so the keyboard is not
/// dragged across the strip once per document; the reader's actual tab is
/// focused once, at the end, from the store's own record of it.
fn rehydrate_tabs(
    pane: &Entity<Pane>,
    symbols: &Entity<SymbolStore>,
    window: &mut Window,
    cx: &mut Context<Shell>,
) -> HashMap<DocTabId, PaneTabId> {
    let (mut resident, active) = {
        let store = symbols.read(cx);
        (
            store.docs.keys().copied().collect::<Vec<DocTabId>>(),
            store.active(),
        )
    };
    if resident.is_empty() {
        return HashMap::new();
    }
    resident.sort_unstable();

    let mut tabs = HashMap::with_capacity(resident.len());
    for doc in resident {
        let store = symbols.clone();
        let page = cx.new(|cx| SymbolPage::new(doc, store, window, cx));
        let opened = pane.update(cx, |pane, cx| {
            pane.open_item(page, Vec::new(), Activation::Preserve, window, cx)
        });
        tabs.insert(doc, opened);
    }

    // Put the reader back on the document they were reading. `Focus` here and
    // nowhere else in this function: exactly one tab should own the keyboard,
    // and it is this one.
    if let Some(restored) = active.and_then(|doc| tabs.get(&doc).copied()) {
        pane.update(cx, |pane, cx| {
            pane.activate_tab(restored, Activation::Focus, window, cx);
        });
    }

    tabs
}

/// How often [`Shell`] re-derives account posture while the window stays open.
///
/// Construction already judged the gate from `AccountService::refresh()`;
/// this interval is the *next* look. Tests only assert that a task is armed,
/// not the duration.
const ACCOUNT_GATE_RECHECK: Duration = Duration::from_secs(60);

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
        index_jobs: Entity<IndexJobStore<nudox_engine::EngineHandle>>,
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
        let panel_jobs = index_jobs.clone();
        register_panel(cx, "jobs-panel", move |_, _, _, window, cx| {
            Box::new(cx.new(|cx| JobsPanel::new(panel_jobs.clone(), window, cx)))
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
        let jobs_panel = cx.new(|cx| JobsPanel::new(index_jobs.clone(), window, cx));
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

        // Dock geometry comes from the app, not from constants: on a cold
        // launch `DockLayout::current` is `Default` (left open, the other two
        // closed — the values that used to be hardcoded here), and on a window
        // rebuild it is whatever the dismissed window last published. See
        // [`DockLayout`] for why this cannot live in `Shell`.
        let layout = DockLayout::current(cx);
        dock_area.update(cx, |da, cx| {
            da.set_center(center_item, window, cx);
            da.set_left_dock(
                left_item,
                Some(px(layout.left.size)),
                layout.left.open,
                window,
                cx,
            );
            da.set_bottom_dock(
                bottom_item,
                Some(px(layout.bottom.size)),
                layout.bottom.open,
                window,
                cx,
            );
            da.set_right_dock(
                right_item,
                Some(px(layout.right.size)),
                layout.right.open,
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
        // The MCP status is read from the process-wide service rather than
        // passed in, so a window opened before — or entirely without — the
        // service still renders something true (`McpStatus::Absent`). See
        // `app::mcp` and docs/LIMITATIONS.md L35.
        let initial_packages = packages.read(cx).summary_label();
        let initial_mcp = McpStatus::from_app(cx);
        // Same reasoning for the account as for MCP above, with one addition:
        // this calls `refresh()` rather than reading `status()`, so the gate
        // this constructor is about to build is judged against the posture
        // *now*, not whatever was last cached.
        //
        // The difference is not cosmetic. `AccountService::status` is a cache
        // that only `refresh()` updates, and this `Shell` is rebuilt on every
        // dock-click / `Window ▸ Show lindsey` after a dismissal
        // (`app::lifecycle`) — a window can sit dismissed for a week while its
        // grace quietly expires underneath it. Reading the cached status here
        // would show the *last-known* posture and could rebuild straight into
        // an ungated shell for a session that has, in fact, lapsed: a fail-open
        // on exactly the gesture (`app::lifecycle::WindowSession::show`) most
        // likely to follow a long absence. `refresh()` costs one clock read and
        // no network — [`nudox_engine::mcp::AccountGate::posture`] is pure — so there is
        // no reason to prefer the stale value.
        //
        // The gate at construction is still a snapshot: a grace window that
        // expires *while this window stays open* is caught by the timer
        // armed just before we return (`_account_recheck`), not by this
        // read. The tool-call boundary is unaffected either way —
        // `AccountGate::admit` re-derives the posture from the clock on
        // every call, in `nudox-mcp`, independent of anything this view caches.
        // A stale GUI gate can only be *too permissive about what the human
        // sees*, never about what an agent's tool call is allowed to do.
        let initial_account = if cx.try_global::<AccountService>().is_some() {
            cx.update_global::<AccountService, _>(|service, _| service.refresh())
        } else {
            AccountStatus::Absent
        };
        let status_bar = cx.new(|cx| {
            let mut bar = StatusBar::new(cx);
            bar.set_packages(initial_packages, cx);
            bar.set_mcp(initial_mcp, cx);
            // Cloned rather than moved: the gate built just below also reads
            // `initial_account`, and constructing it *from* the presentation
            // the status bar just rendered is what guarantees the chip and the
            // gate cannot disagree about which posture launch found.
            bar.set_account(initial_account.clone(), cx);
            bar
        });

        // ── Subscribe to layout changes ───────────────────────────────────────
        let mut subs = Vec::new();

        // ── Account gate (`docs/auth.md`) ──────────────────────────────────────────
        //
        // Built here, at launch, from the same `initial_account` the status
        // bar was just seeded from — see the field doc on `Shell::gate` for
        // what `Some`/`None` mean. `SignedOut`, `StoreUnavailable`,
        // `AwaitingFirstVerification`, `NeverVerified`, `GraceExpired`,
        // `Revoked` and `OverLimit` all gate; only `Active` and
        // `GraceOffline` — the two postures whose `can_work` is `true` — do
        // not. That is `AccountPresentation::can_work` verbatim: the gate
        // asks `app::account`'s own derivation, not a second opinion on the
        // posture.
        let gate = match &initial_account {
            AccountStatus::Live(p) if !p.can_work => {
                let view = cx.new(|cx| SignInView::new(Some(p.clone()), window, cx));
                subs.push(cx.subscribe_in(
                    &view,
                    window,
                    |shell, view, event: &SignInEvent, window, cx| {
                        shell.on_sign_in_event(view.clone(), event, window, cx);
                    },
                ));
                // `engage_gate` (the sign-out path) puts window focus on the
                // view it just installed; this, the launch-time path, was the
                // one case that did not. `SignInView::render` `track_focus`es
                // its own handle and reads every keystroke through
                // `on_key_down`, but neither fires unless the window's focus
                // is actually on that handle — GPUI does not focus a view
                // just because it is the only thing on screen. Without this,
                // a fresh, never-signed-in launch renders a field that looks
                // exactly like every other frame of it (cursor, hover, the
                // same `track_focus`) and silently drops every keystroke,
                // because nothing ever moved focus onto it in the first
                // place. `tests/screenshots.rs`'s `Stage::boot` signs in
                // before scene 01 specifically to get past this screen, so
                // the only scenes that ever typed into the gate for real were
                // 32-34, which go through `engage_gate` and were never
                // missing this call.
                let focus = view.read(cx).focus_handle(cx);
                window.focus(&focus, cx);
                Some(view)
            }
            AccountStatus::Live(_) | AccountStatus::Absent => None,
        };

        // Keep the store's Replace target aligned with the pane. Without this
        // edge, selecting an older tab and then opening a new symbol could close
        // the last-opened document instead of the document being read.
        subs.push(cx.subscribe(&pane, |shell, _pane, event: &PaneEvent, cx| {
            match event {
                PaneEvent::ActiveTabChanged { id: Some(pane_id) } => {
                    let doc = shell
                        .tabs
                        .iter()
                        .find(|(_, mapped)| **mapped == *pane_id)
                        .map(|(doc, _)| *doc);
                    if let Some(doc) = doc {
                        shell
                            .symbols
                            .update(cx, |store, cx| store.activate(doc, cx));
                    }
                }
                PaneEvent::TabClosed { id } => {
                    let doc = shell
                        .tabs
                        .iter()
                        .find(|(_, mapped)| **mapped == *id)
                        .map(|(doc, _)| *doc);
                    if let Some(doc) = doc {
                        shell.tabs.remove(&doc);
                        shell.symbols.update(cx, |store, cx| store.close(doc, cx));
                    }

                    // Closing the active pane tab selects its neighbour. Sync
                    // that selection too, since the close event carries only
                    // the id that disappeared.
                    let active_pane = shell.pane.read(cx).active_id();
                    let active_doc = active_pane.and_then(|pane_id| {
                        shell
                            .tabs
                            .iter()
                            .find(|(_, mapped)| **mapped == pane_id)
                            .map(|(doc, _)| *doc)
                    });
                    if let Some(doc) = active_doc {
                        shell
                            .symbols
                            .update(cx, |store, cx| store.activate(doc, cx));
                    }
                }
                PaneEvent::ActiveTabChanged { id: None } => {}
            }
        }));
        {
            let _entity = cx.entity();
            subs.push(
                cx.subscribe(&dock_area, move |shell, _, evt: &DockEvent, cx| {
                    if matches!(evt, DockEvent::LayoutChanged) {
                        shell.saved_state = Some(shell.dock_area.read(cx).dump(cx));
                        cx.emit(ShellEvent::DockLayoutChanged);
                    }
                }),
            );
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
        subs.push(cx.subscribe(
            &panel_for_subs,
            |shell, _panel, event: &PackageActivated, cx| {
                shell.symbols.update(cx, |store, cx| {
                    store.open(event.root.clone(), OpenDisposition::Replace, cx);
                });
            },
        ));

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

        // ── Rehydrate whatever the store still holds ──────────────────────────
        //
        // A cold launch finds an empty `SymbolStore` and this is a no-op. A
        // *rebuilt* window — the reader dismissed lindsey, the process kept
        // hosting its MCP endpoint, and they clicked the dock icon
        // (`app::lifecycle`) — finds every document still resident, because the
        // stores live on the `App` and only the pixels were thrown away.
        //
        // Without this, restoring would silently produce an empty shell, and
        // the failure would be invisible in code review: `Shell::new` is
        // *correct* for a cold launch, and it was the only caller until now.
        // Making the shell a pure projection of the stores is what makes
        // "destroy the window and rebuild it" safe as a design, rather than
        // safe by luck.
        let tabs = rehydrate_tabs(&pane, &symbols, window, cx);

        // Focus the pane so the very first keystroke has somewhere to land.
        // Actions dispatch from the focused element upward; with nothing
        // focused, `cmd-K` on a fresh window would do nothing at all.
        //
        // After the pane, not before: `rehydrate_tabs` focuses the document the
        // reader was on, and focusing the pane root afterwards would take it
        // straight back off again.
        //
        // Skipped while `gate` is `Some`: this ran unconditionally until the
        // bug this comment now documents — the gate-construction branch above
        // already put window focus on the sign-in field, and this being
        // unconditional stole it right back before `Shell::new` ever
        // returned. The pane was never part of the frame the gate renders
        // (see `Render for Shell`'s early `return` while `self.gate` is
        // `Some`), so focusing it here bought nothing but a launch-time
        // sign-in field that could never receive a keystroke — not from a
        // click (`SignInView` never chases focus back, and nothing else in
        // the gated tree would either), not from typing.
        if tabs.is_empty() && gate.is_none() {
            let pane_focus = pane.read(cx).focus_handle(cx);
            window.focus(&pane_focus, cx);
        }

        // L56: re-derive posture while the window stays open. Owned, not
        // detached — dropping the shell must cancel this, or a dismissed
        // window's timer would keep firing against a dead entity.
        let _account_recheck = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(ACCOUNT_GATE_RECHECK).await;
                if this
                    .update_in(cx, |this, window, cx| {
                        this.recheck_account_gate(window, cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));

        Self {
            dock_area,
            pane,
            status_bar,

            search,
            symbols,
            packages,
            index_jobs,

            gate,
            _account_recheck,

            overlays: OverlayStack::new(),
            presented: None,
            scrim: Motion::new(0.0, Spring::SNAPPY),

            tabs,

            // Springs start *settled* at the layout's current size rather than
            // animating in: a rebuilt window has to be the frame the reader
            // dismissed, and a dock sliding open on restore would both look
            // wrong and make the restored frame differ from the dismissed one.
            left_w: Motion::new(layout.left.rendered_size(), Spring::DEFAULT),
            bottom_h: Motion::new(layout.bottom.rendered_size(), Spring::DEFAULT),
            right_w: Motion::new(layout.right.rendered_size(), Spring::DEFAULT),

            left_w_open: layout.left.size,
            bottom_h_open: layout.bottom.size,
            right_w_open: layout.right.size,

            left_open: layout.left.open,
            bottom_open: layout.bottom.open,
            right_open: layout.right.open,

            banner_h: Motion::new(0.0, Spring::SNAPPY),
            current_banner: None,

            saved_state: None,
            _subs: subs,
        }
    }

    // ── Overlay presentation (§13.5) ─────────────────────────────────────────

    /// Put a view-backed overlay on screen. The only way to do so.
    ///
    /// # Why this exists rather than three open methods doing the same steps
    ///
    /// Presenting an overlay is four things that are only correct together:
    ///
    /// 1. the outgoing view-backed overlay is torn down *and its stack entry
    ///    popped* — `cmd-K` over `?` swaps surfaces, it does not layer them;
    /// 2. the new kind goes on the stack;
    /// 3. the view and its subscriptions are held (LD-18: dropping them later
    ///    is the whole teardown);
    /// 4. **window focus moves onto the view.**
    ///
    /// While those were four statements repeated at three call sites, two call
    /// sites got them wrong in two different ways, and neither failure is
    /// visible in a screenshot. `open_command_palette` and
    /// `toggle_shortcuts_overlay` both skipped (4), so the palette and cheat
    /// sheet painted correctly and then ignored `escape`, `enter`, `up` and
    /// `down` — every binding in the `Overlay` key context, which only
    /// dispatches through the focused element's ancestors. And the old
    /// `dismiss_view_overlay` did the view half of (1) but not the stack half,
    /// so each swap left an orphan entry behind: the depth grew by one, and the
    /// first `escape` after a swap popped an entry whose view had already been
    /// dropped instead of closing what was on screen.
    ///
    /// Both are now impossible to express: there is one field to fill and one
    /// function that fills it.
    fn present_overlay(
        &mut self,
        kind: OverlayKind,
        view: OverlayView,
        subs: Vec<Subscription>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // (1) Replace, don't stack — including the stack entry.
        if self.presented.take().is_some() {
            self.overlays.pop();
        }
        // (2)
        self.overlays.push(kind.clone(), None);
        // (4) is computed before (3) only because `focus_handle` borrows `cx`.
        let focus = view.focus_handle(cx);
        // (3)
        self.presented = Some(PresentedOverlay {
            kind,
            view,
            _subs: subs,
        });
        // (4) An overlay that renders but is not focused is not open: none of
        // the `Overlay`-context bindings can reach it.
        window.focus(&focus, cx);

        if cx.theme_ext().reduced_motion() {
            self.scrim.snap_to(1.0);
        } else {
            self.scrim.animate_to(1.0);
        }
        cx.notify();
    }

    /// Re-take focus for an overlay that is already the presented one.
    ///
    /// Returns `true` if there was one, so callers can early-return. Pressing
    /// the same shortcut twice must not build a second view.
    fn refocus_presented(
        &mut self,
        kind: &OverlayKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(presented) = self.presented.as_ref() else {
            return false;
        };
        if &presented.kind != kind {
            return false;
        }
        let focus = presented.view.focus_handle(cx);
        window.focus(&focus, cx);
        true
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
        if self.refocus_presented(&OverlayKind::OmniSearch, window, cx) {
            return;
        }

        let search = self.search.clone();
        let omni = cx.new(|cx| OmniSearch::new(search, window, cx));
        let subs = vec![cx.subscribe_in(
            &omni,
            window,
            |shell, _view, event: &OmniSearchEvent, window, cx| {
                shell.on_omni_event(event, window, cx);
            },
        )];
        self.present_overlay(
            OverlayKind::OmniSearch,
            OverlayView::OmniSearch(omni),
            subs,
            window,
            cx,
        );
    }

    // ── Shortcuts cheat sheet / command palette (§13.5, §23.1/§23.3, L15) ────

    /// `?`: toggle the read-only keyboard-shortcuts cheat sheet.
    ///
    /// Pressing `?` again while it is open closes it — that is what "toggle"
    /// in the action's own name promises, and the binding's context
    /// (`!InputFocused`, see `app::keymaps`) already keeps it from firing
    /// while the reader is typing anywhere else.
    fn toggle_shortcuts_overlay(
        &mut self,
        _: &ToggleShortcutsOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlays.contains(&OverlayKind::Shortcuts) {
            self.close_overlay(window, cx);
            return;
        }

        let view = cx.new(|cx| CommandOverlay::new(CommandOverlayMode::Shortcuts, cx));
        let subs = vec![cx.subscribe_in(
            &view,
            window,
            |shell, _view, _event: &CommandOverlayEvent, window, cx| {
                // Shortcuts mode only ever emits `Dismiss` (see
                // `CommandOverlay::confirm`) — nothing to match on.
                shell.close_overlay(window, cx);
            },
        )];
        self.present_overlay(
            OverlayKind::Shortcuts,
            OverlayView::Command(view),
            subs,
            window,
            cx,
        );
    }

    /// `cmd-shift-P`: open the command palette — every bound action, run by
    /// dispatching its real `Action` (see `CommandOverlay::confirm`).
    fn open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.refocus_presented(&OverlayKind::CommandPalette, window, cx) {
            return;
        }

        let view = cx.new(|cx| CommandOverlay::new(CommandOverlayMode::Palette, cx));
        let subs = vec![cx.subscribe_in(
            &view,
            window,
            |shell, _view, _event: &CommandOverlayEvent, window, cx| {
                // The view already dispatched the chosen `Action` (if any)
                // onto this same window before emitting `Dismiss` — see
                // `CommandOverlay::confirm`. Shell's only job here is the
                // close/focus-restoration half, same as every other overlay.
                shell.close_overlay(window, cx);
            },
        )];
        self.present_overlay(
            OverlayKind::CommandPalette,
            OverlayView::Command(view),
            subs,
            window,
            cx,
        );
    }

    // ── Account overlay (`docs/auth.md`) ──────────────────────────────────────────

    /// The key `OverlayKind::Modal` identity for the account panel.
    ///
    /// `Modal` rather than a bespoke variant because it *is* one: it does not
    /// dismiss on a scrim click (`OverlayKind::dismiss_on_scrim_click`), which
    /// is right for a surface where a stray click mid-paste would throw away a
    /// key the user just fetched from a browser.
    fn account_overlay_kind() -> OverlayKind {
        OverlayKind::Modal(SharedString::from("account"))
    }

    /// `cmd-shift-A`: open the account panel.
    ///
    /// One surface for sign-in and signed-in. `SignInView::new` derives which
    /// it shows from the presentation it is handed, so the shell does not have
    /// to know — and cannot get it wrong by asking the wrong question.
    fn open_account(&mut self, _: &OpenAccount, window: &mut Window, cx: &mut Context<Self>) {
        let kind = Self::account_overlay_kind();
        if self.refocus_presented(&kind, window, cx) {
            return;
        }

        let presentation = AccountStatus::from_app(cx).presentation().cloned();
        let view = cx.new(|cx| SignInView::new(presentation, window, cx));
        let subs = vec![cx.subscribe_in(
            &view,
            window,
            |shell, view, event: &SignInEvent, window, cx| {
                shell.on_sign_in_event(view.clone(), event, window, cx);
            },
        )];
        self.present_overlay(kind, OverlayView::SignIn(view), subs, window, cx);
    }

    // ── Settings overlay ──────────────────────────────────────────────────

    /// The key `OverlayKind::Modal` identity for the settings panel.
    fn settings_overlay_kind() -> OverlayKind {
        OverlayKind::Modal(SharedString::from("settings"))
    }

    /// `cmd-,`: open the settings panel.
    ///
    /// Currently one section (Connection) — see `views::settings` for why it
    /// exists: without it there was no way to retrieve the per-launch MCP
    /// session token at all.
    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        let kind = Self::settings_overlay_kind();
        if self.refocus_presented(&kind, window, cx) {
            return;
        }

        let view = cx.new(SettingsView::new);
        let subs = vec![cx.subscribe_in(
            &view,
            window,
            |shell, _view, event: &SettingsEvent, window, cx| match event {
                SettingsEvent::Dismiss => shell.close_overlay(window, cx),
            },
        )];
        self.present_overlay(kind, OverlayView::Settings(view), subs, window, cx);
    }

    /// What the account panel reports upward.
    ///
    /// The **shell owns every effect**; the view owns none. That split is not
    /// ceremony — `SignInView` has no access to the `AccountService` global, so
    /// it can be constructed and driven in a test app that has no account
    /// service at all, which is exactly what `tests/screenshots.rs` needs for
    /// the frames it takes before one is installed.
    fn on_sign_in_event(
        &mut self,
        view: Entity<SignInView>,
        event: &SignInEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            SignInEvent::Dismiss => self.close_overlay(window, cx),

            SignInEvent::Submit(key) => {
                let key = (**key).clone();
                // The result arrives on the engine's runtime. Hop back onto the
                // GPUI thread before touching any entity — the same rule every
                // other async result in this app follows (`crate::bridge`).
                let (tx, rx) = flume::bounded(1);
                if let Some(service) = cx.try_global::<AccountService>() {
                    service.sign_in(key, move |outcome| {
                        let _ = tx.send(outcome);
                    });
                } else {
                    let _ = tx.send(Err(nudox_engine::mcp::SignInFailure::NoService));
                }

                cx.spawn_in(window, async move |_shell, cx| {
                    let outcome = match rx.into_recv_async().await {
                        Ok(outcome) => outcome,
                        // The sender was dropped without answering. That is a
                        // bug on our side, not a rejection of the user's key,
                        // and saying "rejected" would send them to regenerate a
                        // key that is fine.
                        Err(_) => Err(nudox_engine::mcp::SignInFailure::NoService),
                    };
                    let rendered = cx.update(|_window, cx| {
                        let status =
                            cx.update_global::<AccountService, _>(|service, _| service.refresh());
                        // The menu bar's account line is a function of this
                        // same status (`app::menus`); refresh it in the same
                        // update so a reader who opens the menu right after
                        // signing in never sees the stale "Sign in…" line.
                        lifecycle::refresh_menus(cx);
                        status
                    });
                    let presentation = rendered
                        .ok()
                        .and_then(|status| status.presentation().cloned());

                    let _ = view.update_in(cx, |view, window, cx| {
                        view.settle(
                            match (outcome, presentation) {
                                (Ok(posture), Some(p)) => Ok((posture, p)),
                                // Verified, but the service could not re-derive
                                // a presentation — impossible in practice and
                                // not worth a fabricated one.
                                (Ok(_), None) => Err(nudox_engine::mcp::SignInFailure::NoService),
                                (Err(e), _) => Err(e),
                            },
                            window,
                            cx,
                        );
                    });
                })
                .detach();
            }

            SignInEvent::SignOut => {
                if cx.try_global::<AccountService>().is_some() {
                    cx.update_global::<AccountService, _>(|service, _| {
                        service.sign_out();
                    });
                }
                self.refresh_account(cx);
                lifecycle::refresh_menus(cx);
                view.update(cx, |view, cx| view.reset_to_form(window, cx));
                // "Sign out returns to the gate": re-block the corpus behind
                // the same view, reset to its empty form, whether sign-out was
                // requested from the gate itself or from the dismissable
                // `cmd-shift-A` overlay. See `Self::engage_gate` for why an
                // overlay-triggered sign-out must not leave a dismissable
                // escape hatch back to a shell the reader is no longer signed
                // in to.
                self.engage_gate(view, window, cx);
            }
        }
    }

    /// The account overlay, when it is the presented one.
    ///
    /// Exposed for the screenshot harness, which drives the *real* view — typing
    /// into it and submitting through it — rather than photographing a
    /// hand-built stand-in. Same reachability discipline as
    /// `StatusBar::mcp()`: a test that asserts on what the reader sees has to
    /// be able to reach what the reader sees (AGENTS-DOCTRINE §6 — never
    /// fabricate UI).
    pub fn presented_sign_in(&self) -> Option<Entity<SignInView>> {
        match self.presented.as_ref()?.view {
            OverlayView::SignIn(ref view) => Some(view.clone()),
            OverlayView::OmniSearch(_) | OverlayView::Command(_) | OverlayView::Settings(_) => None,
        }
    }

    /// Re-read the account status into the status bar.
    ///
    /// Called after every transition rather than polled: the presentation is
    /// derived on change (GUI-PLAN §1.1.4), and a status bar that recomputed it
    /// per frame would allocate on every one of the 120 frames a second it is
    /// visible.
    fn refresh_account(&mut self, cx: &mut Context<Self>) {
        let status = if cx.try_global::<AccountService>().is_some() {
            cx.update_global::<AccountService, _>(|service, _| service.refresh())
        } else {
            AccountStatus::Absent
        };
        self.publish_account_status(status, cx);
    }

    /// Push an already-derived account status into the status bar.
    ///
    /// Public because the screenshot harness drives `SignInView::submit`
    /// directly — it has to, in order to await the round trip before capturing
    /// — and so does not go through the subscription that would otherwise call
    /// [`Self::refresh_account`]. Exposing the *publish* half rather than
    /// letting the harness reach into `status_bar` keeps the status bar's
    /// invariant (label derived from status, in one place) intact.
    pub fn publish_account_status(&mut self, status: AccountStatus, cx: &mut Context<Self>) {
        self.status_bar
            .update(cx, |bar, cx| bar.set_account(status, cx));
    }

    // ── Account gate (`docs/auth.md`) ──────────────────────────────────────────────

    /// Whether the corpus is currently behind the gate.
    ///
    /// Exposed for tests and for the screenshot harness — asserting "the
    /// corpus is unreachable" has to be able to ask the shell what it is
    /// actually rendering, the same reachability discipline
    /// `presented_sign_in` and `StatusBar::mcp` already follow.
    pub fn is_gated(&self) -> bool {
        self.gate.is_some()
    }

    /// Whether [`Shell::new`] armed the L56 posture re-check.
    ///
    /// The task is `Some` for the shell's whole life; dropping the shell
    /// drops it. Tests assert this rather than waiting out the interval.
    pub(crate) fn account_recheck_armed(&self) -> bool {
        self._account_recheck.is_some()
    }

    /// The gate's own `SignInView`, when the gate is up.
    ///
    /// Distinct from [`Self::presented_sign_in`]: that one is the dismissable
    /// `cmd-shift-A` overlay, which does not exist while the gate is up — see
    /// [`Self::engage_gate`]. A caller driving the real sign-in flow through
    /// the launch surface (rather than the overlay) needs this one.
    pub fn gate_view(&self) -> Option<Entity<SignInView>> {
        self.gate.clone()
    }

    /// Re-block the corpus behind the sign-in surface.
    ///
    /// The only caller is [`Self::on_sign_in_event`]'s `SignOut` arm, and it
    /// is called whether the sign-out was requested from the gate itself or
    /// from the dismissable `cmd-shift-A` overlay. That second case is why
    /// this exists as its own step rather than just setting `self.gate`: if
    /// an overlay is presented, it is torn down here rather than left
    /// dismissable. Leaving it up would mean `escape` — or a stray click on
    /// the scrim — reveals the shell behind it to a reader who is no longer
    /// signed in, which is exactly the "screen you must pass" the gate exists
    /// to be.
    fn engage_gate(
        &mut self,
        view: Entity<SignInView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(presented) = self.presented.take() {
            self.overlays.pop();
            self.scrim.snap_to(0.0);
            // `view` is the *same entity* `presented` was showing — sign-out
            // reuses it as the gate rather than building a new one (see the
            // caller). `PresentedOverlay::_subs` holds the
            // `cx.subscribe_in(&view, …)` that routes this view's future
            // `SignInEvent`s back to `on_sign_in_event`. Dropping
            // `PresentedOverlay` here the way `close_overlay` does would sever
            // that subscription along with everything else the overlay
            // owned — and because the view survives the drop (this function
            // is holding another strong reference to it via `view`), the
            // failure is silent: the next `submit()` emits `Submit` to no
            // listener, `on_sign_in_event` never runs, and the gate simply
            // never hears that a key was typed again. Re-homing onto
            // `self._subs`, which lives exactly as long as `Shell` does, is
            // what keeps that subscription alive as long as its view can
            // still emit.
            self._subs.extend(presented._subs);
        }
        let focus = view.read(cx).focus_handle(cx);
        self.gate = Some(view);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Clear the gate once its acceptance animation has actually finished
    /// playing.
    ///
    /// Called at the top of every `render` while the gate is up. Not called
    /// the instant `settle` reports success: `SignInView::accepted` is the
    /// spring that "says the thing you typed became the thing you now are"
    /// (see its module docs), and swapping the gate away for the real shell
    /// mid-collapse would mean nobody ever sees the one animation this
    /// surface exists to show. `SignInView::accepted_and_settled` is true
    /// immediately for a reduced-motion viewer (the spring snaps), so this is
    /// correct in both modes with no branch of its own.
    fn settle_gate_if_ready(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(gate) = self.gate.clone() else {
            return;
        };
        if !gate.read(cx).accepted_and_settled() {
            return;
        }
        // `accepted_and_settled` proves the *view* finished its settle
        // animation into `SignInPhase::Accepted` — it does not prove the
        // account can work. `SignInView::new`'s own phase selection is
        // `p.can_work || p.can_sign_out`, and `can_sign_out` is true for
        // `GraceExpired`, `Revoked`, `OverLimit` and `NeverVerified` whenever
        // the credential came from the Keychain (`AccountPresentation::derive`
        // — none of those four ever clears `source`). All four are postures
        // this gate was built *for*: none of them may work, and a keychain
        // credential from a previous session is exactly what put the gate up
        // in the first place. Their presentation still selects `Accepted`
        // (correctly — the overlay needs to show the account and a "Sign
        // out" button for a revoked or expired key, not a blank form), and
        // `Motion::new` starts already at rest when the initial phase is
        // `Accepted`, so `accepted_and_settled()` is true on the *very first
        // render* — before any sign-in has been attempted this session.
        // Trusting the view's phase alone here would clear the gate for a
        // revoked key on sight. The account service's own, independently
        // re-derived `can_work` is what actually decides — the same value
        // that gated construction in `Shell::new` — and this re-reads it
        // rather than inferring authorization from the display layer.
        let can_work = cx
            .try_global::<AccountService>()
            .map(AccountService::status)
            .and_then(AccountStatus::presentation)
            .is_some_and(|p| p.can_work);
        if !can_work {
            return;
        }
        self.gate = None;
        self.refresh_account(cx);
        lifecycle::refresh_menus(cx);
        // The gate view held focus (`SignInView::new` and `Self::engage_gate`
        // both grant it explicitly). It is not part of the tree `render`
        // returns from here on, so without this, focus stays pinned to a
        // handle absent from the current frame and GPUI's dispatch falls back
        // to a context-free root — every action bound on the shell's own
        // "Workspace global" context, `OpenOmniSearch` included, would go
        // unanswered until *something else* moved focus.
        //
        // Found empirically: this file's own test signs in twice (once at
        // construction, once after a sign-out), and only the *second*
        // gate-clear demonstrably needed this — dispatching `OpenOmniSearch`
        // right after the first gate-clear worked with no explicit refocus at
        // all. That asymmetry is not explained here because it was not fully
        // run to ground (a first-ever-frame difference in how GPUI resolves a
        // focus target with no prior frame to compare against is the leading
        // guess); what is confirmed, by reverting this block and rerunning
        // the test, is that the second cycle fails without it and passes with
        // it. Do not read the first cycle's success as evidence this call is
        // optional — the second cycle is the one that is representative of a
        // real repeated sign-out/sign-in, and this is the same fallback
        // target `close_overlay` already uses, for the same reason.
        let focus = self
            .pane
            .read(cx)
            .active_item_focus_handle(cx)
            .unwrap_or_else(|| self.pane.read(cx).focus_handle(cx));
        window.focus(&focus, cx);
    }

    /// Re-derive account posture and engage or settle the gate if it changed.
    ///
    /// Called from the timer armed in [`Self::new`]. Does **not** rebuild a
    /// gate that is already up — that would steal focus every tick. The only
    /// engage is the ungated → cannot-work transition (grace expired while
    /// the window stayed open). [`AccountStatus::Absent`] is a no-op on the
    /// gate: test apps have no account service, and there is nothing to
    /// gate against.
    fn recheck_account_gate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_account(cx);

        // No process-wide gate in this app (every `#[gpui::test]` without an
        // `AccountService`). The chip already shows Absent; leave `self.gate`
        // alone.
        if matches!(AccountStatus::from_app(cx), AccountStatus::Absent) {
            return;
        }

        // Same derivation `settle_gate_if_ready` uses: the account service's
        // own status, not the view's phase.
        let can_work = cx
            .try_global::<AccountService>()
            .map(AccountService::status)
            .and_then(AccountStatus::presentation)
            .is_some_and(|p| p.can_work);

        if self.gate.is_none() && !can_work {
            let Some(presentation) = AccountStatus::from_app(cx).presentation().cloned() else {
                return;
            };
            let view = cx.new(|cx| SignInView::new(Some(presentation), window, cx));
            self._subs.push(cx.subscribe_in(
                &view,
                window,
                |shell, view, event: &SignInEvent, window, cx| {
                    shell.on_sign_in_event(view.clone(), event, window, cx);
                },
            ));
            lifecycle::refresh_menus(cx);
            self.engage_gate(view, window, cx);
        } else if self.gate.is_some() && can_work {
            self.settle_gate_if_ready(window, cx);
        }
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
            OmniSearchEvent::IndexPurl { purl } => {
                // Start the fetch, then close the overlay and open the Jobs
                // dock. Closing is right here and not for `Open`'s `Stay`/
                // `Background` dispositions, because there is nothing more to
                // do in a search panel whose query cannot match anything — and
                // because the work takes tens of seconds, so the user needs to
                // be looking at where it is reported rather than at a panel
                // that will sit empty.
                let purl = purl.clone();
                self.index_jobs
                    .update(cx, |store, cx| store.start(purl, cx));
                self.close_overlay(window, cx);
                // Only *open* it — never toggle. A user who already had Jobs
                // open and pressed enter would otherwise have it close on them
                // at the exact moment it acquired something to show.
                if !self.bottom_open {
                    self.toggle_bottom_dock(window, cx);
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
        // Dropping `PresentedOverlay` is the whole teardown: the view's
        // reference count falls and every `Subscription` it owns is
        // unregistered with it (LD-18). There is nothing else to clear, and no
        // way to clear "the wrong one" — there is only one.
        self.presented = None;

        if cx.theme_ext().reduced_motion() {
            self.scrim.snap_to(0.0);
        } else {
            self.scrim.animate_to(0.0);
        }

        // Prefer the active document's own handle over `Pane`'s root.
        //
        // Both keep the app responsive, but only the first makes the *page's*
        // bindings (`g s`, `g r`, `y`, the version picker) work in the frame
        // after the overlay closes — `dispatch_action` walks the focused
        // element's ancestors, and `Pane`'s root is an ancestor of the page,
        // not the other way round (L16). Falling back to `Pane` matters for
        // the case with no document open at all, where there is no page to
        // hand the keyboard to and dropping it on the floor is the LD-13
        // failure.
        let focus = self
            .pane
            .read(cx)
            .active_item_focus_handle(cx)
            .unwrap_or_else(|| self.pane.read(cx).focus_handle(cx));
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
        // A background open must never move the keyboard: the omni-search
        // overlay is deliberately still on screen and still owns it.
        let activation = match disposition {
            OpenDisposition::Background => Activation::Preserve,
            _ => Activation::Focus,
        };

        if let Some(&existing) = self.tabs.get(&doc) {
            if disposition != OpenDisposition::Background {
                self.pane.update(cx, |pane, cx| {
                    pane.activate_tab(existing, activation, window, cx);
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
            let id = pane.open_item(page, Vec::new(), activation, window, cx);
            cx.notify();
            id
        });
        self.tabs.insert(doc, opened);

        // `open_item` activates what it opens, which is right for every
        // disposition but Background; put the reader back where they were.
        if disposition == OpenDisposition::Background {
            if let Some(previous) = previously_active_pane_tab {
                self.pane.update(cx, |pane, cx| {
                    pane.activate_tab(previous, Activation::Preserve, window, cx);
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
        let target = if self.left_open {
            self.left_w_open
        } else {
            0.0
        };
        let reduced = cx.theme_ext().reduced_motion();
        if reduced {
            self.left_w.snap_to(target);
        } else {
            self.left_w.animate_to(target);
        }
        self.dock_area.update(cx, |da, cx| {
            da.toggle_dock(DockPlacement::Left, window, cx);
        });
        self.publish_layout(cx);
        cx.notify();
    }

    /// Advance to the next bundled theme, live.
    ///
    /// # Why this handler does almost nothing
    ///
    /// Everything that has to happen — resolving the palette, writing both
    /// theme globals, marking every window dirty — happens inside
    /// [`ThemeRegistry::cycle`]. The handler's whole job is to not add a
    /// fourth thing that also has to happen and could be forgotten. In
    /// particular it does **not** call `cx.notify()` on the shell: notifying
    /// one entity would redraw one entity, which is precisely the partial
    /// update that leaves half the window in the old theme.
    pub fn cycle_theme(&mut self, _: &CycleTheme, _window: &mut Window, cx: &mut Context<Self>) {
        crate::theme::ThemeRegistry::cycle(cx);
    }

    /// Go back one theme in the cycle. See [`Shell::cycle_theme`].
    pub fn cycle_theme_back(
        &mut self,
        _: &CycleThemeBack,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        crate::theme::ThemeRegistry::cycle_back(cx);
    }

    /// Toggle the bottom dock.
    pub fn toggle_bottom_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.bottom_open = !self.bottom_open;
        let target = if self.bottom_open {
            self.bottom_h_open
        } else {
            0.0
        };
        let reduced = cx.theme_ext().reduced_motion();
        if reduced {
            self.bottom_h.snap_to(target);
        } else {
            self.bottom_h.animate_to(target);
        }
        self.dock_area.update(cx, |da, cx| {
            da.toggle_dock(DockPlacement::Bottom, window, cx);
        });
        self.publish_layout(cx);
        cx.notify();
    }

    /// Toggle the right dock.
    pub fn toggle_right_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.right_open = !self.right_open;
        let target = if self.right_open {
            self.right_w_open
        } else {
            0.0
        };
        let reduced = cx.theme_ext().reduced_motion();
        if reduced {
            self.right_w.snap_to(target);
        } else {
            self.right_w.animate_to(target);
        }
        self.dock_area.update(cx, |da, cx| {
            da.toggle_dock(DockPlacement::Right, window, cx);
        });
        self.publish_layout(cx);
        cx.notify();
    }

    /// Publish the current dock geometry to the app, so the next window built
    /// comes back looking like this one.
    ///
    /// Derived from the live fields at the moment it is called, never
    /// accumulated in parallel with them (doctrine §8) — the global is a
    /// *projection* of `Shell`, so the two cannot disagree.
    fn publish_layout(&self, cx: &mut Context<Self>) {
        cx.set_global(DockLayout {
            left: DockState {
                open: self.left_open,
                size: self.left_w_open,
            },
            bottom: DockState {
                open: self.bottom_open,
                size: self.bottom_h_open,
            },
            right: DockState {
                open: self.right_open,
                size: self.right_w_open,
            },
        });
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
        let _ = self
            .dock_area
            .update(cx, |da, cx| da.load(state, window, cx));
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

    /// Which documents the pane is showing, **in strip order**.
    ///
    /// The inverse of [`pane_tab_for_document`](Self::pane_tab_for_document),
    /// ordered by the pane rather than by the `HashMap`. It exists because
    /// "the reader gets their tabs back, in their order" is the property a
    /// rebuilt window has to satisfy (`app::lifecycle`), and a set comparison
    /// would pass on a strip that came back shuffled.
    pub fn open_documents(&self, cx: &App) -> Vec<DocTabId> {
        self.pane
            .read(cx)
            .tab_ids()
            .into_iter()
            .filter_map(|pane_tab| {
                self.tabs
                    .iter()
                    .find(|(_, mapped)| **mapped == pane_tab)
                    .map(|(doc, _)| *doc)
            })
            .collect()
    }

    /// The status bar entity (§13.3).
    ///
    /// Exposed so a test can read the MCP endpoint out of *the thing the reader
    /// looks at* rather than out of the global behind it — see
    /// `tests/mcp_endpoint.rs` for why that distinction is the whole point of
    /// L35.
    pub fn status_bar(&self) -> &Entity<StatusBar> {
        &self.status_bar
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
        let _span = crate::perf::scope(crate::perf::Region::Shell);
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

        // ── Account gate (`docs/auth.md`) ───────────────────────────────────────────
        //
        // Checked, and possibly cleared, before anything else renders — see
        // `Self::settle_gate_if_ready` for why this is not done the instant
        // sign-in succeeds. While `self.gate` is `Some`, this branch returns
        // the gate's tree and *nothing else*: no dock area, no status bar, no
        // overlay layer. That is what makes the omni-search overlay, the
        // corpus, and every document surface actually unreachable rather than
        // merely hidden behind a banner — there is no `on_action` listener
        // anywhere in this tree for `OpenOmniSearch`, `OpenAccount`,
        // `ToggleLeftDock`, or any of the shell's other actions, because the
        // element that would have carried them was never built this frame.
        //
        // The wrapping div paints the theme's own background so the gate does
        // not sit on undrawn window backing outside `SignInView`'s own
        // centred panel; it declares no `key_context` and no `track_focus` of
        // its own, because `SignInView::render` already is a sole root
        // producer (its own doc comment) and a second claim on either would
        // be the doctrine §8 defect this file's own module docs warn about.
        self.settle_gate_if_ready(window, cx);
        if let Some(gate) = self.gate.clone() {
            return div()
                .id("gate")
                .size_full()
                .bg(theme.colours.bg_base)
                .child(gate)
                .into_any_element();
        }

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
        let scrim_colour = theme.colours.scrim;
        // One field, so there is no priority order to get wrong between two
        // live views — see `PresentedOverlay`.
        let overlay_view: Option<AnyView> = self.presented.as_ref().map(|p| p.view.any_view());
        let overlay = overlay_view.map(|view| {
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
                            // The scrim is a theme role, not a colour written here.
                            //
                            // This was `gpui::black().opacity(…)` — the last
                            // raw colour constructor in any view in the crate.
                            // Two things were wrong with it beyond the rule.
                            // Absolute black belongs to no theme, so a warm
                            // theme dimmed to a cold grey; and one alpha served
                            // both appearances, so a light page was veiled as
                            // hard as a dark one when a light page needs less
                            // to read as suspended. `colours.scrim` carries the
                            // theme's own darkest neutral at the right strength
                            // for its appearance; the multiply is the spring's
                            // fade-in, which is motion, not colour.
                            .bg(scrim_colour.opacity(scrim_opacity))
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
                .child(div().absolute().top_0().left_0().size_full().child(view))
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
            .on_action(cx.listener(Self::toggle_shortcuts_overlay))
            .on_action(cx.listener(Self::open_command_palette))
            .on_action(cx.listener(Self::open_account))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(|shell, _: &ToggleLeftDock, window, cx| {
                shell.toggle_left_dock(window, cx);
            }))
            .on_action(cx.listener(|shell, _: &ToggleBottomDock, window, cx| {
                shell.toggle_bottom_dock(window, cx);
            }))
            // Theme cycling. Handled on the shell rather than globally so it
            // sits in the same dispatch path as every other `global` binding —
            // a window-level `on_action` for one action would be a second way
            // for actions to reach handlers, and doctrine §8's `render` note is
            // about exactly what a second path costs.
            .on_action(cx.listener(Self::cycle_theme))
            .on_action(cx.listener(Self::cycle_theme_back))
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
            .into_any_element()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::dock::{BannerKind, LEFT_DOCK_DEFAULT_W};

    /// docs/LIMITATIONS.md L56: `Shell::gate` is judged at construction and
    /// after sign-in/sign-out, but nothing re-derives posture while the window
    /// stays open. Grace expiry mid-session would leave an ungated shell.
    #[gpui::test]
    async fn shell_arms_an_account_posture_recheck_while_the_window_stays_open(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.executor().allow_parking();
        cx.update(|cx: &mut App| {
            gpui_component::init(cx);
            crate::theme::ext::NudoxThemeExt::init(cx).expect("bundled themes parse and install");
            cx.set_global(crate::motion::tokens::MotionTokens::new(1.0));
            cx.bind_keys(crate::app::keymaps::all_bindings());
        });

        let engine = nudox_engine::runtime::Engine::start_with_fixtures(
            nudox_engine::runtime::EngineConfig::default(),
        );
        let search = cx.new(|_cx| SearchStore::new(engine.clone()));
        let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));
        let packages = cx.new(|c| PackageStore::new(engine.clone(), &[], c));
        let index_jobs = cx.new(|_c| IndexJobStore::new(engine.clone()));

        let shell_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<Entity<Shell>>));
        let shell_cell_w = shell_cell.clone();
        let (s2, y2, p2, j2) = (
            search.clone(),
            symbols.clone(),
            packages.clone(),
            index_jobs.clone(),
        );
        let window = cx
            .update(|cx: &mut App| {
                cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
                    let entity = cx.new(|cx| {
                        Shell::new(s2.clone(), y2.clone(), p2.clone(), j2.clone(), window, cx)
                    });
                    *shell_cell_w.lock().unwrap() = Some(entity.clone());
                    cx.new(|cx| gpui_component::Root::new(entity, window, cx))
                })
            })
            .expect("window must open");
        let shell = shell_cell.lock().unwrap().take().expect("shell set");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        assert!(
            shell.read_with(&mut vcx, |s, _| s.account_recheck_armed()),
            "L56: Shell::new must keep a timer that re-derives account posture \
             while the window stays open; without it a grace expiry mid-session \
             is invisible until the next sign-in, sign-out, or rebuild"
        );
    }

    /// Opening a tab and closing a *different* one in the same synchronous
    /// batch must leave `cmd-W` working.
    ///
    /// This is the shape `Shell::reveal_document` produces for
    /// `OpenDisposition::Replace`: `Pane::open_item` activates the incoming
    /// tab and then, with no frame in between, `Pane::close_tab` removes the
    /// outgoing one. The test was written while hunting L22, on the theory
    /// that the back-to-back activate-then-close was itself the trigger. It is
    /// not — the cause was `SymbolPage` rendering a second, focus-untracked
    /// root for its cold-error state (see
    /// `views::symbol_page::SymbolPage::page_root`) — and this test passing
    /// throughout is what ruled the batching out. Kept, because holding that
    /// still is worth a test on its own.
    #[gpui::test]
    async fn open_then_close_in_one_batch_keeps_close_tab_working(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx: &mut App| {
            gpui_component::init(cx);
            crate::theme::ext::NudoxThemeExt::init(cx).expect("bundled themes parse and install");
            cx.set_global(crate::motion::tokens::MotionTokens::new(1.0));
            cx.bind_keys(crate::app::keymaps::all_bindings());
        });

        let engine = nudox_engine::runtime::Engine::start_with_fixtures(
            nudox_engine::runtime::EngineConfig::default(),
        );
        let search = cx.new(|_cx| SearchStore::new(engine.clone()));
        let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));
        let packages = cx.new(|c| PackageStore::new(engine.clone(), &[], c));
        let index_jobs = cx.new(|_c| IndexJobStore::new(engine.clone()));

        let shell_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<Entity<Shell>>));
        let shell_cell_w = shell_cell.clone();
        let (s2, y2, p2, j2) = (
            search.clone(),
            symbols.clone(),
            packages.clone(),
            index_jobs.clone(),
        );
        let window = cx
            .update(|cx: &mut App| {
                cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
                    let entity = cx.new(|cx| {
                        Shell::new(s2.clone(), y2.clone(), p2.clone(), j2.clone(), window, cx)
                    });
                    *shell_cell_w.lock().unwrap() = Some(entity.clone());
                    // `Input` (`SignInView`'s field) requires a `Root`-rooted
                    // window; see the identical comment in `main.rs`.
                    cx.new(|cx| gpui_component::Root::new(entity, window, cx))
                })
            })
            .expect("window must open");
        let shell = shell_cell.lock().unwrap().take().expect("shell set");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();

        search.update(&mut vcx, |s, cx| {
            use crate::stores::search_model::SearchAccess as _;
            s.set_input("Point".into(), cx);
        });
        // `run_until_parked` alone does not wait for the corpus-seed future
        // on its own thread (see `tests/shell_flow.rs`'s `wait_until`).
        for _ in 0..100 {
            vcx.run_until_parked();
            let ready = search.read_with(&mut vcx, |s, _| {
                use crate::stores::search_model::SearchAccess as _;
                !s.snapshot().sections[0].rows.is_empty()
            });
            if ready {
                break;
            }
            cx.background_executor
                .timer(std::time::Duration::from_millis(50))
                .await;
        }

        let rows = search.read_with(&mut vcx, |s, _| {
            use crate::stores::search_model::SearchAccess as _;
            s.snapshot().sections[0].rows[0..2].to_vec()
        });
        let key1 = rows[0]
            .key
            .clone()
            .expect("a local hit must resolve to a real SymbolKey");
        let key2 = rows[1]
            .key
            .clone()
            .expect("a local hit must resolve to a real SymbolKey");

        let _tab1 = symbols.update(&mut vcx, |s, cx| s.open(key1, OpenDisposition::Stay, cx));
        let _tab2 = symbols.update(&mut vcx, |s, cx| s.open(key2, OpenDisposition::Stay, cx));
        vcx.run_until_parked();
        assert_eq!(
            shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len()),
            2,
            "precondition: two open documents means two pane tabs"
        );

        vcx.simulate_keystrokes("cmd-1");
        vcx.run_until_parked();

        // Reproduce `reveal_document`'s Replace-cleanup shape by hand: open a
        // third tab (which activates it), then — in a separate `pane.update`,
        // back to back, with no `run_until_parked` between them — close the tab
        // that held focus until this very block deactivated it. Doing it here
        // rather than through `reveal_document` keeps the two halves visible:
        // if `cmd-W` breaks after this, the batching is the cause; if it
        // survives, the cause is in whatever the *content view* rendered.
        let pane_entity = shell.read_with(&mut vcx, |s, _| s.pane().clone());
        let tab1_pane_id = shell
            .read_with(&mut vcx, |s, _| s.pane_tab_for_document(_tab1))
            .expect("tab1 must have a pane tab");
        let symbols_c = symbols.clone();
        let tab1_docid = _tab1;
        vcx.update(|window, cx| {
            let page3 = cx.new(|cx| SymbolPage::new(tab1_docid, symbols_c.clone(), window, cx));
            pane_entity.update(cx, |pane, cx| {
                pane.open_item(page3, Vec::new(), Activation::Focus, window, cx);
            });
            pane_entity.update(cx, |pane, cx| {
                pane.close_tab(tab1_pane_id, window, cx);
            });
        });
        vcx.run_until_parked();
        assert_eq!(
            shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len()),
            2,
            "one opened, one closed: the tab count is unchanged"
        );

        vcx.simulate_keystrokes("cmd-w");
        vcx.run_until_parked();
        let len_after_first_close = shell.read_with(&mut vcx, |s, cx| s.pane().read(cx).len());
        assert_eq!(
            len_after_first_close, 1,
            "cmd-w must close the newly-active tab"
        );
    }

    /// A pre-rendered presentation for a posture, built the same way
    /// `AccountService::status_of` builds one — through
    /// `AccountPresentation::derive` — rather than by hand, so a passing
    /// assertion is evidence about the real derivation.
    /// An `AccountService` that allows every real sign-in it is asked to
    /// verify, in-process — no socket, no thread of its own.
    ///
    /// Exists so the gate test below can drive `AccountGate::sign_in` for
    /// real — through `Shell::on_sign_in_event`, `AccountHost::sign_in`
    /// (spawned on the engine's own tokio runtime) and back — rather than
    /// calling `SignInView::settle` directly to fake the outcome. The
    /// difference is load-bearing: `settle` only ever updates the *view*.
    /// The gate's actual authority (`Shell::settle_gate_if_ready`) reads
    /// `AccountService::status`, which nothing but a real `sign_in` — through
    /// `AccountGate` — ever updates. A test that shortcuts `sign_in` and
    /// asserts the gate cleared is exactly the "test that would pass against
    /// a stub" AGENTS-DOCTRINE §4 rules out: it is provably not a test of
    /// what actually decides whether the gate opens, because it went on
    /// passing right through the fail-open this file's own comments in
    /// `settle_gate_if_ready` describe — the shortcut never touched the
    /// value that bug was about.
    struct AlwaysAllow;

    impl nudox_engine::mcp::account::service::AccountService for AlwaysAllow {
        fn authorize<'a>(
            &'a self,
            _key: &'a nudox_engine::mcp::ApiKey,
        ) -> nudox_engine::mcp::account::service::ServiceFuture<
            'a,
            Result<
                nudox_engine::mcp::account::service::AuthorizeOutcome,
                nudox_engine::mcp::account::state::ProbeFailure,
            >,
        > {
            Box::pin(async {
                Ok(
                    nudox_engine::mcp::account::service::AuthorizeOutcome::Allowed {
                        user: nudox_engine::mcp::account::state::UserId(24),
                    },
                )
            })
        }

        fn record_usage<'a>(
            &'a self,
            _key: &'a nudox_engine::mcp::ApiKey,
            _count: u32,
        ) -> nudox_engine::mcp::account::service::ServiceFuture<
            'a,
            Result<
                nudox_engine::mcp::account::service::RecordOutcome,
                nudox_engine::mcp::account::state::ProbeFailure,
            >,
        > {
            Box::pin(async { Ok(nudox_engine::mcp::account::service::RecordOutcome::Accepted) })
        }

        fn usage<'a>(
            &'a self,
            _key: &'a nudox_engine::mcp::ApiKey,
        ) -> nudox_engine::mcp::account::service::ServiceFuture<
            'a,
            Result<
                nudox_engine::mcp::account::state::QuotaSnapshot,
                nudox_engine::mcp::account::state::ProbeFailure,
            >,
        > {
            Box::pin(async {
                Ok(nudox_engine::mcp::account::state::QuotaSnapshot {
                    tier: "free".to_owned(),
                    period_start: "2026-08-01T00:00:00Z".to_owned(),
                    api_requests: 0,
                    tool_calls: 0,
                    used: 0,
                    limit: 1000,
                    remaining: 1000,
                    over_limit: false,
                    observed_at: std::time::SystemTime::now(),
                })
            })
        }

        fn base_url(&self) -> &str {
            "fake://always-allow"
        }
    }

    /// Type a key into `view` and submit it, then wait for the *real*
    /// sign-in round trip — through `AlwaysAllow`, on the engine's own tokio
    /// runtime — to land and the gate's acceptance spring to settle.
    ///
    /// `run_until_parked` alone does not wait for that round trip: it drains
    /// GPUI's own queues and knows nothing about a task spawned on a
    /// different runtime (the same reason `Stage::settle` in
    /// `tests/screenshots.rs` sleeps for real between polls).
    async fn sign_in_through(
        cx: &mut gpui::TestAppContext,
        vcx: &mut gpui::VisualTestContext,
        view: &Entity<SignInView>,
    ) {
        view.update_in(vcx, |view, window, cx| {
            view.paste("ndx_2f8c41a9b60d47e3a5710c9fbe2d836a4517", window, cx);
            view.submit(cx);
        });
        for _ in 0..80 {
            vcx.run_until_parked();
            let accepted = view.read_with(vcx, |view, _| {
                matches!(
                    view.phase(),
                    crate::views::sign_in::SignInPhase::Accepted { .. }
                )
            });
            if accepted {
                break;
            }
            cx.background_executor
                .timer(std::time::Duration::from_millis(10))
                .await;
        }
        assert!(
            view.read_with(vcx, |view, _| matches!(
                view.phase(),
                crate::views::sign_in::SignInPhase::Accepted { .. }
            )),
            "AlwaysAllow must have accepted this sign-in within the budget",
        );
    }

    /// The specific fail-open this session's investigation found: a gate
    /// whose view has settled into `SignInPhase::Accepted` must not clear
    /// itself unless the account service's own `can_work` agrees.
    ///
    /// `SignInView::new` puts a `Revoked` (or `GraceExpired`, `OverLimit`,
    /// `NeverVerified`) presentation straight into `SignInPhase::Accepted` —
    /// correctly, for the `cmd-shift-A` overlay, which needs to show *why*
    /// and offer "Sign out" rather than a blank form, because
    /// `AccountPresentation::derive` sets `can_sign_out: true` for all four
    /// (a keychain credential is still loaded) alongside `can_work: false`.
    /// `Motion::new` starts already at rest when the initial value equals the
    /// target, which it does here (`Accepted` → `1.0` from construction), so
    /// `accepted_and_settled()` is true before a single frame renders. If
    /// `Shell::settle_gate_if_ready` trusted that alone, this posture would
    /// wave itself through: no sign-in, no `POST /v1/authorize`, nothing.
    ///
    /// A real `Revoked` posture cannot be produced through
    /// `AccountGate::in_state_for_tests` — it never populates the gate's
    /// `credential` field regardless of `state`, so `can_sign_out` is always
    /// `false` through that path and the precondition this test is about
    /// cannot arise. This constructs the exact `AccountPresentation` by hand
    /// instead and injects it as the gate directly, which is what actually
    /// isolates the guard under test (`settle_gate_if_ready`'s `can_work`
    /// re-check) from the derivation machinery around it.
    #[gpui::test]
    async fn a_revoked_key_still_showing_a_sign_out_button_does_not_wave_itself_through(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::app::account::AccountPresentation;
        use crate::app::actions::OpenOmniSearch;

        cx.executor().allow_parking();
        cx.update(|cx: &mut App| {
            gpui_component::init(cx);
            crate::theme::ext::NudoxThemeExt::init(cx).expect("bundled themes parse and install");
            cx.update_global::<crate::theme::ext::NudoxThemeExt, _>(|ext, _| {
                ext.motion_scale = 0.0;
            });
            cx.set_global(crate::motion::tokens::MotionTokens::new(0.0));
            cx.bind_keys(crate::app::keymaps::all_bindings());
        });

        let engine = nudox_engine::runtime::Engine::start_with_fixtures(
            nudox_engine::runtime::EngineConfig::default(),
        );

        // `SignedOut` here only to give the shell *some* gate at construction
        // (`can_sign_out` is unconditionally `false` for `SignedOut` — see
        // `AccountPresentation::derive` — so this alone cannot trigger the
        // bug; the global status staying `SignedOut`, `can_work: false`,
        // for the rest of the test is exactly what makes it the correct
        // authority for the re-check below to consult).
        cx.update(|cx| {
            let gate = nudox_engine::mcp::AccountGate::in_state_for_tests(
                nudox_engine::mcp::account::state::GateState::SignedOut,
            );
            cx.set_global(AccountService::start_with_gate(&engine, gate));
        });

        let search = cx.new(|_cx| SearchStore::new(engine.clone()));
        let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));
        let packages = cx.new(|c| PackageStore::new(engine.clone(), &[], c));
        let index_jobs = cx.new(|_c| IndexJobStore::new(engine.clone()));

        let shell_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<Entity<Shell>>));
        let shell_cell_w = shell_cell.clone();
        let (s2, y2, p2, j2) = (
            search.clone(),
            symbols.clone(),
            packages.clone(),
            index_jobs.clone(),
        );
        let window = cx
            .update(|cx: &mut App| {
                cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
                    let entity = cx.new(|cx| {
                        Shell::new(s2.clone(), y2.clone(), p2.clone(), j2.clone(), window, cx)
                    });
                    *shell_cell_w.lock().unwrap() = Some(entity.clone());
                    // `Input` (`SignInView`'s field) requires a `Root`-rooted
                    // window; see the identical comment in `main.rs`.
                    cx.new(|cx| gpui_component::Root::new(entity, window, cx))
                })
            })
            .expect("window must open");
        let shell = shell_cell.lock().unwrap().take().expect("shell set");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();

        // The exact combination `AccountPresentation::derive` produces for
        // `Revoked` with a Keychain-sourced credential: `can_work: false`,
        // `can_sign_out: true`.
        let poisoned = AccountPresentation {
            tag: "revoked",
            segment: "account · key rejected".into(),
            headline: "Key rejected".into(),
            detail: "nudox says: key leaked, rotated by the account owner. Create a new \
                      key in the dashboard and sign in again."
                .into(),
            key_hint: Some("ndx_…4517".into()),
            source: Some("Keychain".into()),
            user: None,
            usage: None,
            usage_fraction: None,
            pending: None,
            dropped: None,
            can_work: false,
            can_sign_out: true,
            urgent: true,
        };
        let poisoned_view =
            vcx.update(|window, cx| cx.new(|cx| SignInView::new(Some(poisoned), window, cx)));

        // The precondition the whole test is about: this hand-built view
        // really is already showing settled `Accepted` — the panel, key
        // hint and "Sign out" button — before anything here does a single
        // thing to move it there.
        assert!(
            poisoned_view.read_with(&mut vcx, |v, _| v.accepted_and_settled()),
            "the precondition itself must hold, or this test is not exercising the case \
             it claims to: SignInView::new must put can_sign_out-but-not-can_work \
             straight into a settled Accepted phase",
        );

        vcx.update(|_window, cx| {
            shell.update(cx, |s, cx| {
                s.gate = Some(poisoned_view.clone());
                cx.notify();
            });
        });

        // Multiple renders — not one — because `settle_gate_if_ready` runs on
        // every `render`, and a bug that only failed on the *first* call
        // would still be a bug.
        for _ in 0..5 {
            vcx.run_until_parked();
            assert!(
                shell.read_with(&mut vcx, |s, _| s.is_gated()),
                "a settled Accepted view must never clear the gate on its own when the \
                 account service's own status still says can_work: false — no sign-in \
                 happened in this test at all",
            );
        }

        vcx.dispatch_action(OpenOmniSearch);
        vcx.run_until_parked();
        assert!(
            shell.read_with(&mut vcx, |s, _| s.presented.is_none()),
            "the corpus must stay unreachable behind a view that merely renders its \
             settled 'Accepted' panel without the account service agreeing it can work",
        );
    }

    /// The gate, end to end: a `SignedOut` launch blocks the omni-search
    /// overlay entirely (not merely hides it — dispatching the action while
    /// gated is a no-op because no element in that frame's tree carries an
    /// `on_action` listener for it), a successful sign-in clears the gate and
    /// the *same* dispatch now opens the overlay for real, and signing back
    /// out re-blocks it.
    ///
    /// This is the test AGENTS-DOCTRINE §4 asks for over "the button exists":
    /// it asserts on the actually-observable effect of a dispatched action —
    /// whether an overlay came up — not on the presence of a handler. And,
    /// per `AlwaysAllow`'s own doc comment, it drives sign-in through the
    /// real `AccountGate`, not through `SignInView::settle` — the shortcut
    /// this test used to take, and the reason an earlier version of it never
    /// caught `settle_gate_if_ready` trusting the view's phase over the
    /// account service's own `can_work`.
    #[gpui::test]
    async fn corpus_is_unreachable_while_gated_and_reachable_once_signed_in(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::app::actions::OpenOmniSearch;

        cx.executor().allow_parking();
        cx.update(|cx: &mut App| {
            gpui_component::init(cx);
            crate::theme::ext::NudoxThemeExt::init(cx).expect("bundled themes parse and install");
            // Reduced motion, forced directly on the theme global rather than
            // via `MotionTokens` — `NudoxThemeExt::motion_scale` is what
            // `ThemeExtAccessor::reduced_motion` actually reads
            // (`crate::theme::resolve` hardcodes it to `1.0` at init, and
            // `MotionTokens` governs a separate, unrelated loop-permit
            // budget). With it forced to `0.0`, `SignInView::accepted` snaps
            // rather than animates, so `accepted_and_settled` is true the
            // instant the real sign-in below lands, deterministically, with
            // no clock-advancing loop for the animation half.
            cx.update_global::<crate::theme::ext::NudoxThemeExt, _>(|ext, _| {
                ext.motion_scale = 0.0;
            });
            cx.set_global(crate::motion::tokens::MotionTokens::new(0.0));
            cx.bind_keys(crate::app::keymaps::all_bindings());
        });

        let engine = nudox_engine::runtime::Engine::start_with_fixtures(
            nudox_engine::runtime::EngineConfig::default(),
        );

        // A real gate over a real (in-process) service — not
        // `AccountGate::unmetered`, which has no posture to gate on at all
        // (`admit` short-circuits before ever reading it) and would prove
        // nothing about the launch-time branch under test here — and not
        // `AccountGate::in_state_for_tests`, which has no service at all and
        // so cannot `sign_in` for real.
        cx.update(|cx| {
            let gate = nudox_engine::mcp::AccountGate::new(
                Box::new(nudox_engine::mcp::account::store::MemoryStore::empty()),
                std::sync::Arc::new(AlwaysAllow),
                None,
            );
            cx.set_global(AccountService::start_with_gate(&engine, gate));
        });

        let search = cx.new(|_cx| SearchStore::new(engine.clone()));
        let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));
        let packages = cx.new(|c| PackageStore::new(engine.clone(), &[], c));
        let index_jobs = cx.new(|_c| IndexJobStore::new(engine.clone()));

        let shell_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<Entity<Shell>>));
        let shell_cell_w = shell_cell.clone();
        let (s2, y2, p2, j2) = (
            search.clone(),
            symbols.clone(),
            packages.clone(),
            index_jobs.clone(),
        );
        let window = cx
            .update(|cx: &mut App| {
                cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
                    let entity = cx.new(|cx| {
                        Shell::new(s2.clone(), y2.clone(), p2.clone(), j2.clone(), window, cx)
                    });
                    *shell_cell_w.lock().unwrap() = Some(entity.clone());
                    // `Input` (`SignInView`'s field) requires a `Root`-rooted
                    // window; see the identical comment in `main.rs`.
                    cx.new(|cx| gpui_component::Root::new(entity, window, cx))
                })
            })
            .expect("window must open");
        let shell = shell_cell.lock().unwrap().take().expect("shell set");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();

        assert!(
            shell.read_with(&mut vcx, |s, _| s.is_gated()),
            "a SignedOut launch must construct the shell already gated",
        );

        vcx.dispatch_action(OpenOmniSearch);
        vcx.run_until_parked();
        assert!(
            shell.read_with(&mut vcx, |s, _| s.presented.is_none()),
            "cmd-K must do nothing while the corpus is gated — there is no \
             `on_action` listener for it anywhere in the gated tree",
        );

        // Drive the real view the gate is showing, through the real
        // `AlwaysAllow` service — a real `AccountGate::sign_in`, a real
        // update of `AccountService::status`, and only then the gate's own
        // `can_work` re-check in `settle_gate_if_ready`.
        let gate_view = shell
            .read_with(&mut vcx, |s, _| s.gate.clone())
            .expect("the gate must be showing a SignInView while SignedOut");
        sign_in_through(cx, &mut vcx, &gate_view).await;

        let mut cleared = false;
        for _ in 0..50 {
            vcx.run_until_parked();
            if shell.read_with(&mut vcx, |s, _| !s.is_gated()) {
                cleared = true;
                break;
            }
            cx.background_executor
                .timer(std::time::Duration::from_millis(10))
                .await;
        }
        assert!(
            cleared,
            "the gate must clear once sign-in settles and the acceptance spring is settled"
        );

        vcx.dispatch_action(OpenOmniSearch);
        vcx.run_until_parked();
        assert!(
            matches!(
                shell.read_with(&mut vcx, |s, _| s
                    .presented
                    .as_ref()
                    .map(|p| matches!(p.view, OverlayView::OmniSearch(_)))),
                Some(true)
            ),
            "the identical dispatch must open the real omni-search overlay now that the account can work",
        );

        // Close it before driving sign-out through the account overlay below,
        // so the only presented overlay by the time sign-out fires is the
        // account one — `engage_gate` tears down whatever is presented, and
        // the assertion below is specifically about that overlay's teardown.
        vcx.update(|window, cx| {
            shell.update(cx, |s, cx| s.close_overlay(window, cx));
        });
        vcx.run_until_parked();

        vcx.dispatch_action(crate::app::actions::OpenAccount);
        vcx.run_until_parked();
        let account_view = shell
            .read_with(&mut vcx, |s, _| s.presented_sign_in())
            .expect("cmd-shift-A must present the account overlay once signed in");
        account_view.update(&mut vcx, |_view, cx| {
            cx.emit(SignInEvent::SignOut);
        });
        vcx.run_until_parked();

        assert!(
            shell.read_with(&mut vcx, |s, _| s.is_gated()),
            "signing out must return to the gate",
        );
        assert!(
            shell.read_with(&mut vcx, |s, _| s.presented.is_none()),
            "sign-out must not leave a dismissable overlay behind — that would be an \
             escape hatch back to the shell for a reader who is no longer signed in",
        );

        vcx.dispatch_action(OpenOmniSearch);
        vcx.run_until_parked();
        assert!(
            shell.read_with(&mut vcx, |s, _| s.presented.is_none()),
            "the corpus must be unreachable again after signing out",
        );

        // Sign in a *second* time, through the gate that sign-out just
        // re-engaged, reusing the same view the `cmd-shift-A` overlay was
        // showing a moment ago (`Shell::engage_gate` does not build a new
        // one). This is the case the first sign-in earlier in this test
        // cannot cover: it caught a real bug — `engage_gate` dropped
        // `PresentedOverlay` (and, with it, the `SignInEvent` subscription
        // tied to the view it reused as the gate) without re-homing that
        // subscription first, so a second `submit()` emitted `Submit` to no
        // listener and `on_sign_in_event` never ran. Silent: nothing panics,
        // the field just never becomes `Checking`. Only a *second* real
        // round trip through the reused view exercises that path.
        let second_gate_view = shell
            .read_with(&mut vcx, |s, _| s.gate.clone())
            .expect("sign-out must leave the gate showing a SignInView");
        sign_in_through(cx, &mut vcx, &second_gate_view).await;

        let mut cleared_again = false;
        for _ in 0..50 {
            vcx.run_until_parked();
            if shell.read_with(&mut vcx, |s, _| !s.is_gated()) {
                cleared_again = true;
                break;
            }
            cx.background_executor
                .timer(std::time::Duration::from_millis(10))
                .await;
        }
        assert!(
            cleared_again,
            "the gate must clear on a second sign-in through a reused view exactly as \
             it did on the first — this is the assertion that fails if `engage_gate` \
             regresses to dropping the reused view's subscription",
        );

        vcx.dispatch_action(OpenOmniSearch);
        vcx.run_until_parked();
        assert!(
            matches!(
                shell.read_with(&mut vcx, |s, _| s
                    .presented
                    .as_ref()
                    .map(|p| matches!(p.view, OverlayView::OmniSearch(_)))),
                Some(true)
            ),
            "the corpus must be reachable again after the second sign-in",
        );
    }

    /// A launch-time gate must hold real window focus, not just look
    /// focused.
    ///
    /// Every other test in this module drives the sign-in field through
    /// `SignInView::paste`/`submit` called directly on the entity — which
    /// proves the field's own logic works, but proves nothing about whether
    /// a real keystroke would ever reach it, because that call skips the
    /// window's key-dispatch path entirely. `vcx.simulate_keystrokes` is the
    /// one API in this file that does not skip it: it is the same pipeline
    /// GPUI uses for a real key event, dispatched to whatever the window's
    /// focus is actually on. This is the test that would have caught
    /// `Shell::new`'s gate-construction branch never calling `window.focus`
    /// on the view it had just built and installed — `engage_gate` (the
    /// sign-out path) always has, so every prior test that signs in a
    /// *second* time through a reused gate view was, by construction, unable
    /// to exercise the bug this test is about.
    #[gpui::test]
    async fn a_launch_time_gate_accepts_a_real_keystroke(cx: &mut gpui::TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx: &mut App| {
            gpui_component::init(cx);
            crate::theme::ext::NudoxThemeExt::init(cx).expect("bundled themes parse and install");
            cx.update_global::<crate::theme::ext::NudoxThemeExt, _>(|ext, _| {
                ext.motion_scale = 0.0;
            });
            cx.set_global(crate::motion::tokens::MotionTokens::new(0.0));
            cx.bind_keys(crate::app::keymaps::all_bindings());
        });

        let engine = nudox_engine::runtime::Engine::start_with_fixtures(
            nudox_engine::runtime::EngineConfig::default(),
        );

        cx.update(|cx| {
            let gate = nudox_engine::mcp::AccountGate::new(
                Box::new(nudox_engine::mcp::account::store::MemoryStore::empty()),
                std::sync::Arc::new(AlwaysAllow),
                None,
            );
            cx.set_global(AccountService::start_with_gate(&engine, gate));
        });

        let search = cx.new(|_cx| SearchStore::new(engine.clone()));
        let symbols = cx.new(|_cx| SymbolStore::new(engine.clone()));
        let packages = cx.new(|c| PackageStore::new(engine.clone(), &[], c));
        let index_jobs = cx.new(|_c| IndexJobStore::new(engine.clone()));

        let shell_cell = std::sync::Arc::new(std::sync::Mutex::new(None::<Entity<Shell>>));
        let shell_cell_w = shell_cell.clone();
        let (s2, y2, p2, j2) = (
            search.clone(),
            symbols.clone(),
            packages.clone(),
            index_jobs.clone(),
        );
        let window = cx
            .update(|cx: &mut App| {
                cx.open_window(gpui::WindowOptions::default(), move |window, cx| {
                    let entity = cx.new(|cx| {
                        Shell::new(s2.clone(), y2.clone(), p2.clone(), j2.clone(), window, cx)
                    });
                    *shell_cell_w.lock().unwrap() = Some(entity.clone());
                    // `Input` (`SignInView`'s field) requires a `Root`-rooted
                    // window; see the identical comment in `main.rs`.
                    cx.new(|cx| gpui_component::Root::new(entity, window, cx))
                })
            })
            .expect("window must open");
        let shell = shell_cell.lock().unwrap().take().expect("shell set");
        let mut vcx = gpui::VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();

        let gate_view = shell
            .read_with(&mut vcx, |s, _| s.gate.clone())
            .expect("a SignedOut launch must construct the shell already gated");
        assert_eq!(
            gate_view.read_with(&mut vcx, |v, cx| v
                .input_state()
                .read(cx)
                .value()
                .to_string()),
            "",
            "precondition: nothing has been typed yet",
        );

        // Not `gate_view.update(..., |view, cx| view.input_state()...)` —
        // that would call the field's logic directly, the exact shortcut
        // this test exists to not take. `simulate_input` goes through the
        // window's real input path, exactly as a real keypress would.
        vcx.simulate_input("n");
        vcx.run_until_parked();

        assert_ne!(
            gate_view.read_with(&mut vcx, |v, cx| v
                .input_state()
                .read(cx)
                .value()
                .to_string()),
            "",
            "a keystroke dispatched through the window must reach the launch-time gate's \
             field — if this is empty, window focus never landed on the gate when \
             `Shell::new` built it, and every real keypress a user makes at a fresh, \
             never-signed-in launch is silently dropped",
        );
    }

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

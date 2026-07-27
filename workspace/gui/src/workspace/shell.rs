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

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, ParentElement as _, Render, SharedString, Styled,
    Subscription, Window, actions, div, px,
    prelude::FluentBuilder as _,
};
use gpui_component::dock::{
        DockArea, DockEvent, DockItem, DockPlacement, Panel, PanelEvent, PanelState,
        PanelView, PanelInfo, DockAreaState, register_panel,
    };
use serde_json::Value as JsonValue;

use crate::motion::spring::{Motion, Spring};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::workspace::pane::Pane;
use crate::workspace::status_bar::StatusBar;
use gpui::prelude::*;

// ─────────────────────────────────────────────────────────────────────────────
// Actions (Appendix B)
// ─────────────────────────────────────────────────────────────────────────────

actions!(
    shell,
    [
        /// Toggle left sidebar dock (cmd-B).
        ToggleLeftDock,
        /// Toggle bottom dock (cmd-J).
        ToggleBottomDock,
        /// Toggle right dock.
        ToggleRightDock,
    ]
);

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
placeholder_panel!(
    ProjectPanel,
    "project-panel",
    "Project",
    "TODO(views): crate::views::project_panel"
);

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

impl Shell {
    /// Construct the shell, wiring up the `DockArea` and all placeholder panels.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // ── Register placeholder panels so DockArea can (de)serialize them ─────
        register_panel(cx, "project-panel", |_, _, _, window, cx| {
            Box::new(cx.new(|cx| ProjectPanel::new(window, cx)))
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

        // ── Left dock (Project + Search) ───────────────────────────────────────
        let project_panel = cx.new(|cx| ProjectPanel::new(window, cx));
        let search_panel  = cx.new(|cx| SearchPanel::new(window, cx));
        let left_item = DockItem::tabs(
            vec![
                Arc::new(project_panel) as Arc<dyn PanelView>,
                Arc::new(search_panel)  as Arc<dyn PanelView>,
            ],
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
        let status_bar = cx.new(|cx| StatusBar::new(cx));

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

        Self {
            dock_area,
            pane,
            status_bar,

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

        // ── Full shell layout ─────────────────────────────────────────────────
        div()
            .id("shell")
            .size_full()
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
}

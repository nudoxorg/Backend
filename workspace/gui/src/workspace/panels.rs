//! The dock `Panel` implementations: placeholders, jobs, and the centre pane.

use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement as _,
    Render, SharedString, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use gpui_component::dock::{Panel, PanelEvent, PanelInfo, PanelState, TitleStyle};
use gpui_component::{IconName, h_flex, v_flex};
use serde_json::Value as JsonValue;

use crate::stores::index_jobs::{IndexJobState, IndexJobStore, IndexJobsChanged};
use crate::theme::ext::ThemeExtAccessor as _;
use crate::workspace::pane::Pane;

// ─────────────────────────────────────────────────────────────────────────────
// Dock panels that have no data source yet
// ─────────────────────────────────────────────────────────────────────────────

/// A dock panel whose data source does not exist yet, rendered as a designed
/// empty state.
///
/// # What this used to say, and why that was a defect
///
/// The generated panels used to render their own `TODO(views):
/// crate::views::jobs_panel` scaffolding note, centred, in the product's own
/// type, in a user-visible dock (GUI-WORKORDER-2 F9,
/// `tests/shots/memchr/17-bottom-dock-open.png`). A placeholder is a claim too: it
/// is read by a user, not by the author who wrote it, and a Rust module path
/// is not an answer to "what would be here?".
///
/// The empty states below therefore say what *the reader* would see once the
/// surface is populated — the LD-9 / LD-16 rule that an empty state is a
/// designed state, not an absence. They deliberately do not promise a date, a
/// version, or a feature name.
///
/// # Why the tab label and the empty-state title are two parameters
///
/// They answer two different questions. The dock tab has to say what the
/// surface *is* (`Jobs`) so the reader can find it again; the body has to say
/// what the reader is looking at *right now* (`No running jobs`). Collapsing
/// them is how a body ends up repeating its own tab label and then needing a
/// second line to carry any content — which is the shape the `TODO(views)`
/// line grew in.
///
/// # Why the `$icon` and `$caption` are macro parameters and not a default
///
/// So that adding a panel forces the author to answer "what does the reader see
/// here when it works?" at the definition site. There is no fallback caption to
/// inherit, which is what let one of these ship naming a module path.
///
/// Each generated panel:
/// - Implements `Panel` + `Focusable` + `EventEmitter<PanelEvent>` + `Render`.
/// - Renders `ui::EmptyState` (icon, title, caption) — the same component every
///   other empty surface in the app uses, so these do not become a second,
///   divergent look.
/// - Has `closable = false` so it cannot accidentally be removed from the dock.
macro_rules! placeholder_panel {
    (
        $name:ident,
        $panel_name_str:literal,
        $tab_label:literal,
        $empty_title:literal,
        $icon:expr,
        $caption:literal
    ) => {
        /// A dock panel for $tab_label whose data source is not wired yet.
        pub struct $name {
            focus: FocusHandle,
        }

        impl $name {
            pub fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
                Self {
                    focus: cx.focus_handle(),
                }
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

            fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                div().child(SharedString::from($tab_label))
            }

            // Without this override, `TabPanel::render_title_bar` never calls
            // `.text_color(..)` on the header row (it only does so `when_some`
            // a `TitleStyle` is returned) and the title falls back to GPUI's
            // unthemed black default — invisible on a dark base surface. See
            // `NudoxThemeExt::panel_title_style` for why this is a role gap,
            // not a wrong-colour pick.
            fn title_style(&self, cx: &App) -> Option<TitleStyle> {
                Some(cx.theme_ext().panel_title_style())
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
            fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
                let motion = crate::motion::tokens::MotionTokens::new(cx.theme_ext().motion_scale);
                div().size_full().child(crate::ui::EmptyState::new(
                    $icon,
                    SharedString::from($empty_title),
                    SharedString::from($caption),
                    motion,
                ))
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
    "Nothing searched yet",
    IconName::Search,
    "Press ⌘K to search symbols, signatures and prose across the loaded corpus."
);

// Bottom dock panels.
//
// `JobsPanel` used to be a placeholder, and its comment said why:
// "`EngineHandle::jobs()` hands back an already-closed receiver
// (docs/LIMITATIONS.md L37), so the engine's job stream is a documented stub and
// there is nothing here for `lindsey` to subscribe to."
//
// `EngineHandle::jobs()` is still `Err(Unimplemented)` — that gap is unchanged.
// What changed is that there is now one kind of long-running work `lindsey`
// starts itself, and therefore owns a stream for: an on-demand `pkg:` index
// (`EngineHandle::index_purl`). So this panel renders exactly that set and says
// so in its empty state. "No package has been indexed this session" is a claim
// about a set `IndexJobStore` is authoritative over; "no jobs are running"
// would have been a claim about a plane that still does not exist.

placeholder_panel!(
    LogsPanel,
    "logs-panel",
    "Logs",
    "No log output",
    IconName::SquareTerminal,
    "Diagnostics from the engine and its producers appear here."
);

/// The Jobs dock: on-demand package indexing, and how far each one has got.
///
/// Reads [`IndexJobStore`] and computes nothing (§13.3) — every string on
/// screen was formatted in the store when the event arrived, not here on every
/// frame. A download emits a byte-count update per chunk; formatting
/// "2.3 MB of 4.1 MB" in `render` would be that work multiplied by the frame
/// rate.
pub struct JobsPanel {
    focus: FocusHandle,
    store: Entity<IndexJobStore<nudox_engine::EngineHandle>>,
    /// Held so the panel repaints when the store changes. Dropping it would
    /// leave the dock frozen at whatever it last drew (LD-18).
    _subscription: gpui::Subscription,
}

impl JobsPanel {
    pub fn new(
        store: Entity<IndexJobStore<nudox_engine::EngineHandle>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let subscription = cx.subscribe(&store, |_this, _store, _e: &IndexJobsChanged, cx| {
            cx.notify();
        });
        Self {
            focus: cx.focus_handle(),
            store,
            _subscription: subscription,
        }
    }
}

impl EventEmitter<PanelEvent> for JobsPanel {}

impl Focusable for JobsPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Panel for JobsPanel {
    fn panel_name(&self) -> &'static str {
        "jobs-panel"
    }

    fn title(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().child(SharedString::from("Jobs"))
    }

    fn title_style(&self, cx: &App) -> Option<TitleStyle> {
        Some(cx.theme_ext().panel_title_style())
    }

    fn closable(&self, _: &App) -> bool {
        false
    }

    fn dump(&self, _: &App) -> PanelState {
        PanelState {
            panel_name: "jobs-panel".to_string(),
            children: Vec::new(),
            info: PanelInfo::Panel(JsonValue::Null),
        }
    }
}

impl Render for JobsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let colours = ext.colours;
        let motion = crate::motion::tokens::MotionTokens::new(ext.motion_scale);

        let store = self.store.read(cx);
        if store.is_empty() {
            return div().size_full().child(crate::ui::EmptyState::new(
                IconName::LayoutDashboard,
                SharedString::from("No package indexed this session"),
                SharedString::from(
                    "Type a package URL such as pkg:cargo/serde@1.0.196 into ⌘K to fetch and \
                     document a package that is not in this corpus. Its progress appears here.",
                ),
                motion,
            ));
        }

        let rows: Vec<_> = store.rows().to_vec();
        div()
            .size_full()
            .p(sp.space_3)
            .child(
                v_flex()
                    .w_full()
                    .gap(sp.space_2)
                    .children(rows.into_iter().map(move |row| {
                        let (headline, detail, tone) = match &row.state {
                            IndexJobState::Running { stage, detail } => {
                                (stage.clone(), detail.clone(), colours.fg_muted)
                            }
                            IndexJobState::Done { detail, .. } => (
                                SharedString::from("Indexed"),
                                detail.clone(),
                                colours.fg_muted,
                            ),
                            IndexJobState::Failed { detail, .. } => {
                                (SharedString::from("Failed"), detail.clone(), colours.danger)
                            }
                        };
                        // The `help` line is rendered *below* the failure and in a
                        // quieter role: "what happened" and "what to do next" are
                        // different sentences and the panel must not run them
                        // together (the same split the MCP error vocabulary makes).
                        let help = match &row.state {
                            IndexJobState::Failed { help, .. } => help.clone(),
                            _ => None,
                        };

                        v_flex()
                            .w_full()
                            .gap(sp.space_1)
                            .px(sp.space_2)
                            .py(sp.space_2)
                            .rounded(sp.r_md)
                            .bg(colours.bg_raised)
                            .child(
                                h_flex()
                                    .w_full()
                                    .items_center()
                                    .gap(sp.space_2)
                                    .child(
                                        div()
                                            .text_size(ts.ui.size)
                                            .line_height(ts.ui.line_height)
                                            .text_color(colours.fg_default)
                                            .child(row.purl.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(ts.dense.size)
                                            .line_height(ts.dense.line_height)
                                            .text_color(tone)
                                            .child(headline),
                                    ),
                            )
                            .when(!detail.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_size(ts.dense.size)
                                        .line_height(ts.dense.line_height)
                                        .text_color(colours.fg_muted)
                                        .child(detail),
                                )
                            })
                            .children(help.map(|help| {
                                div()
                                    .text_size(ts.dense.size)
                                    .line_height(ts.dense.line_height)
                                    .text_color(colours.fg_faint)
                                    .child(help)
                            }))
                    })),
            )
    }
}

// Right dock panel.
placeholder_panel!(
    OutlinePanel,
    "outline-panel",
    "Outline",
    "No document open",
    IconName::GalleryVerticalEnd,
    "Open a symbol and its sections are listed here."
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

    // See the identical comment in `placeholder_panel!` — without this the
    // "Editor" header renders unthemed black text on a near-black surface.
    fn title_style(&self, cx: &App) -> Option<TitleStyle> {
        Some(cx.theme_ext().panel_title_style())
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

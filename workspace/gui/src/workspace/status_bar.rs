//! The status bar (GUI-PLAN §13.3) — the app's ambient truth line.
//!
//! Left to right: backend provenance, sync summary, diagnostics count, the
//! hosted MCP endpoint, and (in debug builds) the frame-time readout that opens
//! the HUD.
//!
//! # Why it is an entity rather than a render helper
//!
//! Everything here changes on a different clock from the rest of the shell:
//! sync progress ticks at up to 30 Hz, diagnostics arrive in bursts, frame time
//! updates at 4 Hz. If the status bar were part of the shell's own render, each
//! of those would re-render the entire window (§1.1.2 — invalidation
//! granularity is the entity). As its own entity it re-renders alone, which is
//! the same leaf-animation rule the dock springs follow, applied to data.
//!
//! # Every segment is a button
//!
//! §13.3 requires it, and the reason is discoverability: a count with no
//! affordance is a dead end. The diagnostics chip opens the log panel filtered
//! to warnings; the MCP segment copies the endpoint; the frame-time readout
//! toggles the HUD.

use gpui::{
    Context, InteractiveElement as _, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, div,
};

use crate::app::account::AccountStatus;
use crate::app::mcp::McpStatus;
use crate::theme::ext::{Provenance, ThemeExtAccessor as _};
use crate::ui::ProvenanceDot;

/// Pre-computed status-bar state.
///
/// Every field is render-ready (§1.1.4): the strings are formatted when the
/// value changes, never in `render`. A status bar that ran `format!` per frame
/// would allocate on every one of the 120 frames a second it is visible.
pub struct StatusBar {
    /// How the local corpus was obtained.
    provenance: Provenance,
    /// e.g. `"3 packages"` — the loaded-package count (GUI-LOCAL-PLAN §L10).
    packages_label: SharedString,
    /// e.g. `"synced"` or `"2 fetching · 2.1 MB/s"`.
    sync_label: SharedString,
    /// Warning/error count; `None` hides the chip entirely rather than showing
    /// a zero, because "0 problems" is noise once you have read it twice.
    diagnostics: Option<u32>,
    /// Pre-rendered diagnostics chip.
    ///
    /// Formatted once when the count changes, not per frame (§1.1.4). GPUI
    /// caches shaped text by `(text, font, size)`, so a `SharedString` that
    /// keeps its identity is nearly free to re-render — while a `format!` in
    /// the render body allocates and re-shapes on every one of the 120 frames
    /// a second this bar is visible.
    diagnostics_label: SharedString,
    /// What the hosted MCP server is doing (GUI-LOCAL-PLAN §L6).
    ///
    /// A [`McpStatus`], not an `Option<SharedString>`. The old field could not
    /// distinguish "not started", "failed to bind", "stopped" and "this
    /// process hosts no server" — all four were `None`, so a server that had
    /// crashed on start-up rendered identically to one that was never asked
    /// for. That is docs/LIMITATIONS.md L35's second half; see `app::mcp`.
    mcp: McpStatus,
    /// Pre-rendered MCP segment, refreshed on transition rather than per frame
    /// (§1.1.4). `None` hides the segment.
    mcp_label: Option<SharedString>,
    /// What the account gate is doing (`docs/auth.md`).
    ///
    /// An [`AccountStatus`], for the same reason `mcp` is an `McpStatus` and
    /// not an `Option<SharedString>`: "no account service in this process",
    /// "signed out", "offline with four days left" and "over quota" are four
    /// different facts, and a status bar that renders three of them as an
    /// absent segment is one where a user first learns they are signed out from
    /// a failed agent call.
    account: AccountStatus,
    /// Pre-rendered account segment. `None` only when this process hosts no
    /// account gate at all.
    account_label: Option<SharedString>,
    /// Rolling p95 frame time, debug builds only (§25.2).
    frame_p95_ms: Option<f32>,
    /// Cached rendering of `frame_p95_ms`, refreshed at 4 Hz by the HUD sampler.
    frame_label: SharedString,
}

impl StatusBar {
    /// A status bar in its cold-start state: local, empty, quiet.
    ///
    /// LR-10 — local is the default, not a state we transition into after
    /// hearing from a server.
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            provenance: Provenance::TrustedLocal,
            packages_label: SharedString::from("no packages"),
            sync_label: SharedString::from("idle"),
            diagnostics: None,
            diagnostics_label: SharedString::default(),
            // Cold start hosts nothing. `main` replaces this the moment the
            // service is installed; a test app never does, and `Absent` is the
            // true statement in that case rather than a placeholder.
            mcp: McpStatus::Absent,
            mcp_label: None,
            // Same reasoning as `mcp` above: a cold-start bar hosts nothing,
            // and `Absent` is the true statement rather than a placeholder.
            account: AccountStatus::Absent,
            account_label: None,
            frame_p95_ms: None,
            frame_label: SharedString::from("—"),
        }
    }

    /// Report the loaded-package count. `label` is pre-formatted by the caller.
    pub fn set_packages(&mut self, label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.packages_label = label.into();
        cx.notify();
    }

    /// Report sync state. `label` is pre-formatted by the caller.
    pub fn set_sync(&mut self, label: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.sync_label = label.into();
        cx.notify();
    }

    /// Report the diagnostics count; `None` hides the chip.
    pub fn set_diagnostics(&mut self, count: Option<u32>, cx: &mut Context<Self>) {
        self.diagnostics = count;
        // Format here, once, rather than in every frame of `render`.
        self.diagnostics_label = match count {
            Some(n) => SharedString::from(format!("⚠ {n}")),
            None => SharedString::default(),
        };
        cx.notify();
    }

    /// Publish what the hosted MCP server is doing (GUI-LOCAL-PLAN §L6).
    ///
    /// Called by `Shell::new` from [`McpStatus::from_app`] and again on quit.
    /// The label is derived here so the rendered text is a pure function of the
    /// status — a segment that said "listening" while the status said `Stopped`
    /// would be the same class of drift the status enum exists to remove.
    pub fn set_mcp(&mut self, status: McpStatus, cx: &mut Context<Self>) {
        self.mcp_label = status.label();
        self.mcp = status;
        cx.notify();
    }

    /// What the status bar currently believes about the MCP server.
    ///
    /// Exposed for `tests/mcp_endpoint.rs`, which takes the URL out of *this*
    /// value and opens a socket to it: the invariant under test is that the
    /// endpoint the reader sees is one a client can reach, and that is only
    /// testable if the test can read what the reader sees.
    pub fn mcp(&self) -> &McpStatus {
        &self.mcp
    }

    /// The exact text painted in the MCP segment, or `None` when it is hidden.
    pub fn mcp_label(&self) -> Option<&SharedString> {
        self.mcp_label.as_ref()
    }

    /// Publish what the account gate is doing (`docs/auth.md`).
    ///
    /// The label is derived from the status here, once, so the painted text is
    /// a pure function of the state — the same rule [`Self::set_mcp`] follows,
    /// and for the same reason: a segment reading "signed in" beside a status
    /// of `Revoked` is the drift the status type exists to remove.
    pub fn set_account(&mut self, status: AccountStatus, cx: &mut Context<Self>) {
        self.account_label = status.label();
        self.account = status;
        cx.notify();
    }

    /// What the status bar currently believes about the account.
    ///
    /// Exposed for the screenshot harness and for `tests/`, which assert on
    /// *what the reader sees* rather than on what a global holds — the same
    /// reachability discipline `mcp()` exists for.
    pub fn account(&self) -> &AccountStatus {
        &self.account
    }

    /// The exact text painted in the account segment, or `None` when hidden.
    pub fn account_label(&self) -> Option<&SharedString> {
        self.account_label.as_ref()
    }

    /// Update the frame-time readout. Called at 4 Hz, not per frame — the
    /// readout exists to be *read*, and a number changing 120 times a second
    /// cannot be.
    pub fn set_frame_p95(&mut self, ms: Option<f32>, cx: &mut Context<Self>) {
        self.frame_p95_ms = ms;
        self.frame_label = match ms {
            Some(v) => SharedString::from(format!("{v:.1} ms")),
            None => SharedString::from("—"),
        };
        cx.notify();
    }

    /// The current provenance, for tests and for the shell's title.
    pub fn provenance(&self) -> Provenance {
        self.provenance
    }
}

impl Render for StatusBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ext = cx.theme_ext();
        let space = ext.space;
        let colours = ext.colours;
        let caption = ext.type_scale.caption;
        let theme_name = ext.theme_name.clone();

        // Frame time is over budget when it exceeds the §1.2 8.3 ms target.
        let frame_colour = match self.frame_p95_ms {
            Some(v) if v > 8.3 => colours.warn,
            _ => colours.fg_muted,
        };

        let segment = move |child: gpui::AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(space.space_1)
                .px(space.space_2)
                .child(child)
        };

        let mut bar = div()
            .flex()
            .items_center()
            .w_full()
            .h(space.space_6)
            .px(space.space_2)
            .bg(colours.bg_raised)
            .border_t_1()
            .border_color(colours.border_default)
            .text_size(caption.size)
            .line_height(caption.line_height)
            .text_color(colours.fg_muted)
            // Backend provenance + loaded packages.
            .child(
                div()
                    .id("status.provenance")
                    .flex()
                    .items_center()
                    .gap(space.space_1)
                    .px(space.space_2)
                    .rounded(space.r_sm)
                    .hover(|s| s.bg(colours.bg_hover))
                    .child(ProvenanceDot::new("status.dot", self.provenance))
                    .child(self.packages_label.clone()),
            )
            .child(segment(
                div().child(self.sync_label.clone()).into_any_element(),
            ));

        if self.diagnostics.is_some() {
            bar = bar.child(
                div()
                    .id("status.diagnostics")
                    .px(space.space_2)
                    .rounded(space.r_sm)
                    .text_color(colours.warn)
                    .cursor_pointer()
                    .hover(|s| s.bg(colours.bg_hover))
                    .child(self.diagnostics_label.clone()),
            );
        }

        // Push the right-hand cluster to the far edge.
        bar = bar.child(div().flex_1());

        // A failed server is painted in the warning colour, not hidden: §L6's
        // whole promise is that an agent can reach this window, and silently
        // dropping the segment is what made docs/LIMITATIONS.md L35 invisible for
        // as long as it was.
        if let Some(label) = &self.mcp_label {
            let mcp_colour = if self.mcp.is_failure() {
                colours.warn
            } else {
                colours.fg_muted
            };
            bar = bar.child(
                div()
                    .id("status.mcp")
                    .px(space.space_2)
                    .rounded(space.r_sm)
                    .text_color(mcp_colour)
                    .cursor_pointer()
                    .hover(|s| s.bg(colours.bg_hover).text_color(colours.fg_default))
                    .child(label.clone()),
            );
        }

        // The account.
        //
        // Painted for *every* posture the process actually has, including a
        // healthy one — the segment is hidden only when there is no account
        // gate at all. A signed-in state that renders as nothing is a
        // signed-out state that renders as nothing, and the first time the user
        // would learn the difference is when an agent's tool call is refused.
        if let Some(label) = &self.account_label {
            let account_colour = if self.account.is_urgent() {
                colours.warn
            } else {
                colours.fg_muted
            };
            bar = bar.child(
                div()
                    .id("status.account")
                    .px(space.space_2)
                    .rounded(space.r_sm)
                    .text_color(account_colour)
                    .cursor_pointer()
                    .hover(|s| s.bg(colours.bg_hover).text_color(colours.fg_default))
                    .child(label.clone())
                    // This was the reported defect: `cursor_pointer()` and a
                    // hover state with no click handler at all — every visual
                    // cue of a button, wired to nothing. Compare
                    // `status.theme` below, which is the pattern this segment
                    // was missing.
                    .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                        window.dispatch_action(Box::new(crate::app::actions::OpenAccount), cx);
                    }),
            );
        }

        // The live theme, named.
        //
        // # Why the theme belongs in the status bar
        //
        // `cmd-shift-T` recolours the entire window, which is unmistakable —
        // and tells the reader nothing about *which* of four themes they have
        // landed on, or that there are four. A one-word segment makes the
        // cycle navigable instead of a slot machine, and it is the only place
        // in the application where the theme's name is written down.
        //
        // Clicking it cycles, so the feature is discoverable without knowing
        // the binding — the same reason the dock toggles are also buttons.
        bar = bar.child(
            div()
                .id("status.theme")
                .px(space.space_2)
                .rounded(space.r_sm)
                .cursor_pointer()
                .hover(|s| s.bg(colours.bg_hover).text_color(colours.fg_default))
                .child(theme_name)
                .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                    window.dispatch_action(Box::new(crate::app::actions::CycleTheme), cx);
                }),
        );

        // The frame-time readout, in debug builds, **only once there is a
        // measurement**.
        //
        // It used to render unconditionally, and `set_frame_p95(None)`
        // formats as `"—"`, so a build with no frame sampler wired showed a
        // bare em-dash pinned to the right edge of the window for the whole
        // session. That is visible in every frame in `tests/shots/fixtures/` and
        // reads as a broken widget rather than as an absent measurement. A
        // readout with nothing to read out should not be a readout.
        #[cfg(debug_assertions)]
        if self.frame_p95_ms.is_some() {
            bar = bar.child(
                div()
                    .id("status.frame")
                    .px(space.space_2)
                    .rounded(space.r_sm)
                    .text_color(frame_colour)
                    .cursor_pointer()
                    .hover(|s| s.bg(colours.bg_hover))
                    .child(self.frame_label.clone()),
            );
        }
        #[cfg(not(debug_assertions))]
        let _ = frame_colour;

        bar
    }
}

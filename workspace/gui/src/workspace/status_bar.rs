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
    Context, InteractiveElement as _, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
};

use crate::theme::ext::{Provenance, ThemeExtAccessor as _};
use crate::ui::ProvenanceDot;
use gpui::prelude::*;

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
    /// The loopback endpoint of the hosted MCP server (GUI-LOCAL-PLAN §L6),
    /// or `None` before it has bound its port.
    mcp_endpoint: Option<SharedString>,
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
            mcp_endpoint: None,
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

    /// Publish the MCP endpoint once the server has bound its ephemeral port.
    pub fn set_mcp_endpoint(
        &mut self,
        endpoint: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.mcp_endpoint = endpoint;
        cx.notify();
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
            .child(segment(div().child(self.sync_label.clone()).into_any_element()));

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

        if let Some(endpoint) = &self.mcp_endpoint {
            bar = bar.child(
                div()
                    .id("status.mcp")
                    .px(space.space_2)
                    .rounded(space.r_sm)
                    .cursor_pointer()
                    .hover(|s| s.bg(colours.bg_hover).text_color(colours.fg_default))
                    .child(endpoint.clone()),
            );
        }

        #[cfg(debug_assertions)]
        {
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

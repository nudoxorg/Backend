//! The Settings surface (`cmd-,`) — one section today: Connection.
//!
//! # Why this exists
//!
//! `nudox-mcp` requires a per-launch bearer token on every request (§L6's
//! threat model: the listener is loopback-only, so the token defends against
//! *other local processes*, not a network attacker). The token is generated
//! fresh each launch and deliberately never logged
//! (`nudox_engine::mcp::session::SessionToken`'s `Debug` impl redacts it). Until this
//! view existed, there was **no way to retrieve it** — `Copy MCP Endpoint`
//! only ever copied the bare URL, and nothing in this crate rendered
//! `McpStatus::Listening`'s `client_config`. An agent could see the app
//! logging "mcp server hosted" and still have no way to authenticate to it.
//!
//! This view closes that gap by rendering the same [`crate::app::mcp::McpStatus`]
//! the status bar already reads, so the URL and the ready-to-paste
//! `mcpServers` config displayed here can never drift from what the server
//! actually accepts — both come from the live endpoint (see
//! `nudox_engine::mcp::endpoint::ClientConfig::for_endpoint`).

use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, Focusable, IntoElement, Render,
    SharedString, Task, Window, div, px,
};

use crate::app::actions::DismissOverlay;
use crate::app::mcp::McpStatus;
use crate::theme::ext::ThemeExtAccessor as _;

/// How long the "Copied" confirmation stays up after a copy action fires.
///
/// Not a spring — a plain timestamp and a plain threshold, matching
/// `symbol_page`'s identical `COPY_FEEDBACK_DURATION` convention: motion
/// tokens belong in `motion/tokens.rs`, time-based *visibility* windows live
/// beside the state they gate.
const COPY_FEEDBACK_DURATION: Duration = Duration::from_millis(1600);

/// Which control's "Copied" confirmation is currently showing.
///
/// Mutually exclusive by construction (a single field holds this, not two
/// independent timestamps) because copying the config and copying the URL are
/// never both the most recent action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CopyTarget {
    Config,
    Url,
}

/// What the shell needs to hear from this view.
#[derive(Clone, Debug)]
pub enum SettingsEvent {
    Dismiss,
}

/// The Settings overlay.
pub struct SettingsView {
    focus: FocusHandle,
    copied: Option<(CopyTarget, Instant)>,
    /// Clears `copied` after [`COPY_FEEDBACK_DURATION`]. Replacing this
    /// cancels a still-pending clear from an earlier copy, the same
    /// `debounce_task` pattern `stores/search.rs` uses.
    _feedback_task: Option<Task<()>>,
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            copied: None,
            _feedback_task: None,
        }
    }

    fn copy(&mut self, target: CopyTarget, text: &SharedString, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        self.copied = Some((target, Instant::now()));
        self._feedback_task = Some(cx.spawn(async move |view, cx| {
            cx.background_executor().timer(COPY_FEEDBACK_DURATION).await;
            let _ = view.update(cx, |view, cx| {
                view.copied = None;
                cx.notify();
            });
        }));
        cx.notify();
    }
}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Render for SettingsView {
    /// Sole producer of the root element — same split as `SignInView::render`
    /// (doctrine §8's GPUI note): `id`, `key_context`, `track_focus` and every
    /// `.on_action` live only here.
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (sp, ts, colours) = {
            let ext = cx.theme_ext();
            (ext.space, ext.type_scale, ext.colours)
        };

        let status = McpStatus::from_app(cx);

        let mut panel = div()
            .id("settings.panel")
            .w(px(560.0))
            .max_h(px(560.0))
            .flex()
            .flex_col()
            .gap(sp.space_3)
            .p(sp.space_5)
            .rounded(sp.r_xl)
            .bg(colours.bg_raised)
            .border_1()
            .border_color(colours.border_default)
            .shadow_lg()
            // A click inside the panel must not reach the scrim behind it.
            .occlude()
            .child(
                div()
                    .text_size(ts.title.size)
                    .line_height(ts.title.line_height)
                    .text_color(colours.fg_default)
                    .child("Settings"),
            )
            .child(
                div()
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .text_color(colours.fg_muted)
                    .child("Connection"),
            );

        panel = match &status {
            McpStatus::Listening { url, client_config } => panel
                .child(
                    div()
                        .text_size(ts.prose.size)
                        .line_height(ts.prose.line_height)
                        .text_color(colours.fg_muted)
                        .child(
                            "This process is hosting an MCP endpoint. Paste the config below \
                             into an agent's MCP client settings to connect it — the token is \
                             generated fresh for this launch and stops working the moment the \
                             app quits.",
                        ),
                )
                .child(self.field_row("URL", url, CopyTarget::Url, sp, ts, colours, cx))
                .child(self.field_row(
                    "Client config",
                    client_config,
                    CopyTarget::Config,
                    sp,
                    ts,
                    colours,
                    cx,
                )),
            McpStatus::Absent => panel.child(
                div()
                    .text_size(ts.prose.size)
                    .line_height(ts.prose.line_height)
                    .text_color(colours.fg_muted)
                    .child("This process is not hosting an MCP endpoint."),
            ),
            McpStatus::Failed { reason } => panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(sp.space_1)
                    .child(
                        div()
                            .text_size(ts.prose.size)
                            .line_height(ts.prose.line_height)
                            .text_color(colours.danger)
                            .child("The MCP endpoint failed to start."),
                    )
                    .child(
                        div()
                            .text_size(ts.caption.size)
                            .line_height(ts.caption.line_height)
                            .text_color(colours.fg_muted)
                            .child(reason.clone()),
                    ),
            ),
            McpStatus::Stopped => panel.child(
                div()
                    .text_size(ts.prose.size)
                    .line_height(ts.prose.line_height)
                    .text_color(colours.fg_muted)
                    .child("The MCP endpoint was stopped."),
            ),
        };

        div()
            .id("settings")
            .track_focus(&self.focus)
            .key_context("Settings Overlay")
            .on_action(cx.listener(|_view, _: &DismissOverlay, _window, cx| {
                cx.emit(SettingsEvent::Dismiss);
            }))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(panel)
    }
}

impl SettingsView {
    /// A labelled, monospace, copyable field — the URL row and the config row
    /// are the same shape, so this is the one place either can go wrong.
    #[allow(clippy::too_many_arguments)]
    fn field_row(
        &self,
        label: &'static str,
        value: &SharedString,
        target: CopyTarget,
        sp: crate::theme::tokens::SpaceTokens,
        ts: crate::theme::tokens::TypeScale,
        colours: crate::theme::tokens::ColourRoles,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let is_config = matches!(target, CopyTarget::Config);
        let just_copied = self.copied.is_some_and(|(t, _)| t == target);
        let value = value.clone();
        let value_for_click = value.clone();

        div()
            .flex()
            .flex_col()
            .gap(sp.space_1)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(sp.space_2)
                    .child(
                        div()
                            .text_size(ts.caption.size)
                            .line_height(ts.caption.line_height)
                            .text_color(colours.fg_muted)
                            .child(label),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(SharedString::from(format!("settings.copy.{label}")))
                            .px(sp.space_2)
                            .py(sp.space_1 / 2.0)
                            .rounded(sp.r_sm)
                            .text_size(ts.caption.size)
                            .line_height(ts.caption.line_height)
                            .text_color(if just_copied {
                                colours.ok
                            } else {
                                colours.accent
                            })
                            .cursor_pointer()
                            .hover(|s| s.bg(colours.bg_hover))
                            .on_click(cx.listener(move |view, _event, _window, cx| {
                                view.copy(target, &value_for_click, cx);
                            }))
                            .child(if just_copied { "Copied" } else { "Copy" }),
                    ),
            )
            .child(
                div()
                    .id(SharedString::from(format!("settings.value.{label}")))
                    .w_full()
                    .max_h(px(160.0))
                    .overflow_y_scroll()
                    .px(sp.space_3)
                    .py(sp.space_2)
                    .rounded(sp.r_sm)
                    .bg(colours.bg_sunken)
                    .border_1()
                    .border_color(colours.border_default)
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .text_color(colours.fg_default)
                    .when(is_config, |el| el.whitespace_normal())
                    .when(!is_config, |el| el.whitespace_nowrap())
                    .child(value),
            )
    }
}

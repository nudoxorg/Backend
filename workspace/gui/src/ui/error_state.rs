//! `ErrorState` — a full-page error state with a retry affordance.
//!
//! # Guarantees
//!
//! Every slot has a designed error state (LD-16).  This component provides it.
//! The error message comes from [`SlotError`]; callers supply a `on_retry`
//! callback when the error is transient (retryable).
//!
//! **No motion on errors** (§5.2: "errors must be calm").  The component
//! renders immediately without any entrance animation.

use gpui::{
    App, IntoElement, ParentElement, RenderOnce, Window, prelude::FluentBuilder as _,
};
use gpui_component::{ActiveTheme as _, IconName, Icon, v_flex};

use crate::bridge::slot::SlotError;
use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;
use gpui_component::Sizable as _;

/// Full-page error state with a retry affordance (LD-16).
///
/// No entrance motion — errors must be calm (§5.2).
///
/// The component renders:
/// - A danger-coloured icon.
/// - The error message from [`SlotError`].
/// - A "Retry" button when `on_retry` is `Some` (for transient errors).
/// - For `Permanent` errors, the message itself is the explanation; no
///   separate "details" disclosure is needed at this abstraction level.
#[derive(IntoElement)]
pub struct ErrorState {
    /// The terminal error to display.
    error: SlotError,
    /// Callback invoked when the user clicks "Retry".  `None` for permanent
    /// errors where a retry cannot help.
    on_retry: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
}

impl ErrorState {
    /// Create an error state.
    ///
    /// Pass `on_retry: None` for permanent errors (auth failures, missing
    /// packages, schema mismatches — anything a retry cannot resolve).
    pub fn new(error: SlotError, on_retry: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>) -> Self {
        Self { error, on_retry }
    }

    /// Convenience: build from a transient error with a retry handler.
    pub fn transient(
        message: impl Into<String>,
        on_retry: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            error: SlotError::Transient { message: message.into() },
            on_retry: Some(Box::new(on_retry)),
        }
    }

    /// Convenience: build from a permanent error (no retry).
    pub fn permanent(message: impl Into<String>) -> Self {
        Self {
            error: SlotError::Permanent { message: message.into() },
            on_retry: None,
        }
    }
}

impl RenderOnce for ErrorState {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let theme = cx.theme();

        // Error message — pre-computed via Display impl on SlotError (not format! in render).
        let message: gpui::SharedString = self.error.to_string().into();

        // Whether to show a retry button.
        let has_retry = self.on_retry.is_some();
        let on_retry = self.on_retry;

        v_flex()
            .id("ui.error_state")
            .w_full()
            .items_center()
            .justify_center()
            .gap(sp.space_3)
            .p(sp.space_6)
            // Danger icon
            .child(
                Icon::new(IconName::CircleX)
                    .text_color(theme.danger)
                    .with_size(gpui_component::Size::Large),
            )
            // Error message
            .child(
                gpui::div()
                    .text_color(theme.danger)
                    .text_size(ts.ui.size)
                    .line_height(ts.ui.line_height)
                    .text_align(gpui::TextAlign::Center)
                    .child(message),
            )
            // Retry button (only for retryable errors)
            .when(has_retry, |el| {
                let on_click = on_retry.unwrap();
                el.child(
                    gpui::div()
                        .id("ui.error_state.retry")
                        .cursor_pointer()
                        .bg(theme.danger.opacity(0.1))
                        .text_color(theme.danger)
                        .rounded(sp.r_md)
                        .px(sp.space_4)
                        .py(sp.space_2)
                        .text_size(ts.ui.size)
                        .border_1()
                        .border_color(theme.danger.opacity(0.4))
                        .hover(|s| s.bg(theme.danger.opacity(0.15)))
                        .active(|s| s.bg(theme.danger.opacity(0.2)))
                        .on_click(move |_, window, cx| on_click(window, cx))
                        .child(gpui::SharedString::from("Retry")),
                )
            })
    }
}

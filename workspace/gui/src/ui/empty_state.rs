//! `EmptyState` — a designed empty screen for every list and panel.
//!
//! # Guarantees
//!
//! Every empty state is a designed screen, not an absent element (LD-16).
//! This component provides the canonical layout: icon + title + description +
//! optional primary action, with the `empty.state` entrance motion (§5.3).
//!
//! All callers must supply pre-computed `SharedString`s — no `format!` inside
//! render.  The action callback is executed on click; it never fires from
//! render.

use gpui::{
    App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window,
    prelude::FluentBuilder as _,
};
use gpui_component::{ActiveTheme as _, Icon, IconName, v_flex};

use crate::motion::declarative::empty_state_enter;
use crate::motion::tokens::MotionTokens;
use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;
use gpui_component::Sizable as _;

/// A designed empty state screen (LD-16 / §5.3 `empty.state`).
///
/// Renders centred in its parent container.  Callers are responsible for
/// giving the parent enough height that the centred content is visible.
///
/// # LD-6 note
///
/// This component does not render rows.  Callers that place a list *below*
/// this (or alongside it) and that list can exceed 32 rows must use
/// `uniform_list`.
#[derive(IntoElement)]
pub struct EmptyState {
    /// Icon to display above the title.
    icon: IconName,
    /// Short title (e.g. "No results").
    title: SharedString,
    /// Longer description or call-to-action sentence.
    description: SharedString,
    /// Label for the primary action button.  When `None`, no button is shown.
    action_label: Option<SharedString>,
    /// Callback for the primary action button.
    action: Option<Box<dyn Fn(&mut Window, &mut App) + 'static>>,
    /// Motion tokens — needed to pass into `empty_state_enter`.
    /// Callers must provide this from `cx.global::<MotionTokens>()` at
    /// update time (before render).
    motion: MotionTokens,
}

impl EmptyState {
    /// Create an empty state with the given icon, title, and description.
    ///
    /// Use `motion` from `cx.global::<MotionTokens>()` at the call site.
    pub fn new(
        icon: IconName,
        title: impl Into<SharedString>,
        description: impl Into<SharedString>,
        motion: MotionTokens,
    ) -> Self {
        Self {
            icon,
            title: title.into(),
            description: description.into(),
            action_label: None,
            action: None,
            motion,
        }
    }

    /// Attach a primary action button.
    pub fn action(
        mut self,
        label: impl Into<SharedString>,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        self.action_label = Some(label.into());
        self.action = Some(Box::new(on_click));
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let theme = cx.theme();

        let action_label = self.action_label;
        let action = self.action;

        let inner = v_flex()
            .items_center()
            .gap(sp.space_3)
            .p(sp.space_6)
            // Icon — large, faint
            .child(
                Icon::new(self.icon)
                    .text_color(theme.muted_foreground.opacity(0.5))
                    .with_size(gpui_component::Size::Large),
            )
            // Title
            .child(
                gpui::div()
                    .text_color(theme.foreground)
                    .text_size(ts.title.size)
                    .line_height(ts.title.line_height)
                    .font_weight(gpui::FontWeight(ts.title.weight as f32))
                    .text_align(gpui::TextAlign::Center)
                    .child(self.title),
            )
            // Description
            .child(
                gpui::div()
                    .text_color(theme.muted_foreground)
                    .text_size(ts.ui.size)
                    .line_height(ts.ui.line_height)
                    .text_align(gpui::TextAlign::Center)
                    .child(self.description),
            )
            // Optional action button
            .when(action_label.is_some() && action.is_some(), |el| {
                let label = action_label.unwrap();
                let on_click = action.unwrap();
                el.child(
                    gpui::div()
                        .id("ui.empty_state.action")
                        .cursor_pointer()
                        .bg(theme.accent)
                        .text_color(theme.accent_foreground)
                        .rounded(sp.r_md)
                        .px(sp.space_4)
                        .py(sp.space_2)
                        .text_size(ts.ui.size)
                        .hover(|s| s.opacity(0.85))
                        .active(|s| s.opacity(0.7))
                        .on_click(move |_, window, cx| on_click(window, cx))
                        .child(label),
                )
            });

        // Wrap with entrance motion — `empty_state_enter` handles scale-0.
        empty_state_enter(
            gpui::div()
                .id("ui.empty_state")
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .child(inner),
            "ui.empty_state",
            &self.motion,
        )
    }
}

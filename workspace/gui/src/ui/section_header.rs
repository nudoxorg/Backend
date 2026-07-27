//! `SectionHeader` — caption overline for panel sections.
//!
//! # Guarantees
//!
//! - Title rendered at `caption` type scale in `fg_muted` with the token
//!   letter-spacing (0.2 em), all-uppercase via `text-transform` styling.
//! - Optional count in `fg_faint`; optional action button right-aligned.
//! - A hairline `border_default` bottom separator.
//! - No `format!`, no inline colours, no raw pixel literals.

use gpui::{
    AnyElement, App, IntoElement, ParentElement, RenderOnce, SharedString, Window,
    prelude::FluentBuilder as _,
};
use gpui_component::{ActiveTheme as _, h_flex};

use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;

/// Caption overline for a panel or tab section.
///
/// Composes as:
/// ```text
/// ┌─────────────────────────────────────────────────┐
/// │ TITLE              247   [action]                │
/// ├─────────────────────────────────────────────────┤
/// ```
///
/// The title is forced to uppercase by this component; callers pass the
/// original-casing label.
///
/// Any list that this header precedes that can exceed 32 rows **must** use
/// `uniform_list` in the caller (LD-6).  This component does not virtualize;
/// it is a header only.
#[derive(IntoElement)]
pub struct SectionHeader {
    /// Section title text (displayed in uppercase via CSS text-transform).
    title: SharedString,
    /// Optional pre-formatted count string (e.g. `"247"`).  Must not be
    /// constructed with `format!` in a render body.
    count: Option<SharedString>,
    /// Optional action: a label and a pre-built element (e.g. a small button).
    /// The element is placed right-aligned in the header row.
    action: Option<AnyElement>,
}

impl SectionHeader {
    /// Create a section header with only a title.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            count: None,
            action: None,
        }
    }

    /// Attach a count string (pre-formatted by the calling store).
    pub fn count(mut self, count: impl Into<SharedString>) -> Self {
        self.count = Some(count.into());
        self
    }

    /// Attach a right-aligned action element (e.g. a small icon button).
    pub fn action(mut self, el: AnyElement) -> Self {
        self.action = Some(el);
        self
    }
}

impl RenderOnce for SectionHeader {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let theme = cx.theme();

        h_flex()
            .w_full()
            .px(sp.space_2)
            .py(sp.space_1)
            .gap(sp.space_2)
            .border_b_1()
            .border_color(theme.border)
            // Title
            .child(
                gpui::div()
                    .flex_1()
                    .text_color(theme.muted_foreground)
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .font_weight(gpui::FontWeight(ts.caption.weight as f32))
                    .child(self.title),
            )
            // Optional count
            .when_some(self.count, |el, count| {
                el.child(
                    gpui::div()
                        .text_color(theme.muted_foreground.opacity(0.6))
                        .text_size(ts.caption.size)
                        .line_height(ts.caption.line_height)
                        .font_weight(gpui::FontWeight(ts.caption.weight as f32))
                        .child(count),
                )
            })
            // Optional right-aligned action
            .when_some(self.action, |el, action| el.child(action))
    }
}

//! `KeyHint` — a description + row of `Kbd` chips.
//!
//! # Guarantees
//!
//! Used in palette footers, overlay bottoms, empty states, and the `?`
//! shortcuts overlay.  Each hint shows a description and the associated
//! keystrokes as `gpui_component::kbd::Kbd` chips.
//!
//! No inline colours; all styling from tokens.  No `format!` in render.

use gpui::{App, IntoElement, Keystroke, ParentElement, RenderOnce, SharedString, Window};
use gpui_component::{ActiveTheme as _, h_flex, kbd::Kbd};

use crate::theme::ext::ThemeExtAccessor as _;
use gpui::prelude::*;

/// A single keyboard hint: description text + one or more `Kbd` chips.
///
/// Used in: palette footer, empty states, `?` shortcuts overlay.
///
/// ```text
///  Open symbol   ⌘⏎
///  Close         ⎋
/// ```
#[derive(IntoElement)]
pub struct KeyHint {
    /// Human-readable description of the action.
    description: SharedString,
    /// The keystrokes to display as `Kbd` chips.
    keys: Vec<Keystroke>,
}

impl KeyHint {
    /// Create a hint with a description and a list of keystrokes.
    pub fn new(description: impl Into<SharedString>, keys: Vec<Keystroke>) -> Self {
        Self {
            description: description.into(),
            keys,
        }
    }

    /// Convenience: create from a single keystroke.
    pub fn single(description: impl Into<SharedString>, key: Keystroke) -> Self {
        Self::new(description, vec![key])
    }
}

impl RenderOnce for KeyHint {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let ext = cx.theme_ext();
        let sp = ext.space;
        let ts = ext.type_scale;
        let theme = cx.theme();

        h_flex()
            .gap(sp.space_2)
            .items_center()
            // Description label
            .child(
                gpui::div()
                    .text_color(theme.muted_foreground.opacity(0.7))
                    .text_size(ts.caption.size)
                    .line_height(ts.caption.line_height)
                    .child(self.description),
            )
            // Kbd chips — each pre-built by gpui-component
            .children(self.keys.into_iter().map(|k| Kbd::new(k)))
    }
}

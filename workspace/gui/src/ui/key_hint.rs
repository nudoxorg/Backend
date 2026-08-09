//! `KeyHint` — a description + row of `Kbd` chips.
//!
//! # Guarantees
//!
//! Used in palette footers, overlay bottoms, empty states, and the `?`
//! shortcuts overlay.  Each hint shows a description and the associated
//! keystrokes as `gpui_component::kbd::Kbd` chips.
//!
//! No inline colours; all styling from tokens.  No `format!` in render.

use std::rc::Rc;

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
    ///
    /// `Rc<[Keystroke]>`, not `Vec<Keystroke>`, because every caller holds its
    /// keystrokes for the lifetime of a view and hands the *same* ones to a new
    /// `KeyHint` on every frame. With a `Vec` that hand-off is a heap
    /// allocation and a `String` clone per chip per row per frame — the search
    /// footer paid five of them a frame and the command palette paid seventy-
    /// nine. A refcount bump is the whole cost now, and the shape makes the
    /// cheap form the default rather than something each caller must remember.
    keys: Rc<[Keystroke]>,
}

impl KeyHint {
    /// Create a hint with a description and a list of keystrokes.
    ///
    /// Takes `impl Into<Rc<[Keystroke]>>` so an owner that has already built
    /// its keystrokes once can pass a clone of the `Rc` (free), while a caller
    /// with a literal `vec![…]` is unchanged.
    pub fn new(description: impl Into<SharedString>, keys: impl Into<Rc<[Keystroke]>>) -> Self {
        Self {
            description: description.into(),
            keys: keys.into(),
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
            // `Kbd::new` takes an owned `Keystroke`, so the clone here is
            // unavoidable at this boundary; what the `Rc` removes is the
            // *slice* allocation that used to wrap it.
            .children(self.keys.iter().map(|k| Kbd::new(k.clone())))
    }
}

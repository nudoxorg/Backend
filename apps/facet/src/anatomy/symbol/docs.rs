//! Native semantics for the same values the documentation page paints.
//! Hosts own callback admission; these controls do not invent navigation,
//! register an alternate text layout, or revive a retained/inert subtree.

use crate::reading::{self, Intent, ReadingRole};
use gpui::{Div, ElementId, SharedString, Stateful};

pub use crate::reading::{ControlKind, control};

/// The reading role of words already present in the document.
#[derive(Clone, Copy)]
pub enum TextKind {
    Text,
    Heading,
}

/// Paint and read the same text leaf. Native words follow its actual layout,
/// including any clamp or ellipsis supplied by the surrounding builder.
pub fn text(
    id: impl Into<ElementId>,
    words: impl Into<SharedString>,
    kind: TextKind,
    paint: impl FnOnce(reading::Text) -> Div,
) -> Div {
    let role = match kind {
        TextKind::Text => ReadingRole::Paragraph,
        TextKind::Heading => ReadingRole::Heading,
    };
    paint(reading::text(id, words, Intent::Reading(role)))
}

/// When a painted control activates. PointerDown is reserved for openers
/// whose press transfers focus into a popup; touch still activates on tap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Activation {
    Release,
    PointerDown,
}
impl Activation {
    /// Mouse release must not repeat an opener's down action. GPUI owns
    /// keyboard down/up; native AX Click is normalized to Keyboard.
    pub fn admits_click(self, event: &gpui::ClickEvent) -> bool {
        self == Self::Release || !matches!(event, gpui::ClickEvent::Mouse(_))
    }
    /// Attach the opener's one pointer-down action to the same painted leaf.
    /// Stop propagation so a press cannot take focus back from the menu.
    pub fn pointer_down(self, control: Stateful<Div>, act: super::host::Act) -> Stateful<Div> {
        use gpui::InteractiveElement as _;
        if self == Self::PointerDown {
            control.on_mouse_down(gpui::MouseButton::Left, move |_, window, cx| {
                act(window, cx);
                window.prevent_default();
                cx.stop_propagation();
            })
        } else {
            control
        }
    }
}

mod rich;
pub use rich::{Link, RichText, rich_text};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod link_admission_tests;

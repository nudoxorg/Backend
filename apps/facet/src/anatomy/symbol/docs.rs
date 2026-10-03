//! Native semantics for the same values the documentation page paints.
//! Hosts own callback admission; these controls do not invent navigation,
//! register an alternate text layout, or revive a retained/inert subtree.

use gpui::{
    Div, ElementId, ParentElement as _, Role, SharedString, Stateful,
    StatefulInteractiveElement as _,
};

/// The reading role of words already present in the document.
#[derive(Clone, Copy)]
pub enum TextKind {
    Text,
    Heading,
}

/// Paint and name one owned string. The builder receives exactly the value
/// exposed to AccessKit; wrapping and font scaling stay with native layout.
pub fn text(
    id: impl Into<ElementId>,
    words: impl Into<SharedString>,
    kind: TextKind,
    paint: impl FnOnce(SharedString) -> Div,
) -> Stateful<Div> {
    let words = words.into();
    paint(words.clone())
        .id(id)
        .role(match kind {
            TextKind::Text => Role::Label,
            TextKind::Heading => Role::Heading,
        })
        .aria_label(words)
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

/// The semantics of a real page action.
#[derive(Clone, Copy)]
pub enum ControlKind {
    Button,
    Link,
    Disclosure(bool),
    Filter(bool),
}

/// Decorate the actual clickable element before its host attaches native
/// focus. No second control, callback, hitbox, or hidden fallback is added.
pub fn control(
    words: impl Into<SharedString>,
    kind: ControlKind,
    element: Stateful<Div>,
) -> Stateful<Div> {
    let element = element
        .role(match kind {
            ControlKind::Link => Role::Link,
            _ => Role::Button,
        })
        .aria_label(words);
    match kind {
        ControlKind::Disclosure(open) => element.aria_expanded(open),
        ControlKind::Filter(chosen) => element.aria_selected(chosen),
        ControlKind::Button | ControlKind::Link => element,
    }
}

mod rich;
pub use rich::{Link, RichText, rich_text};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod link_admission_tests;

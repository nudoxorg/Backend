//! Native reading semantics over the text layout that actually paints.
//!
//! Callers supply their existing, owner-qualified element key and semantic
//! intent. This module does not infer meaning from a font, the probe ledger,
//! or a string, and does not own actions, focus, or resource admission.

use gpui::{
    App, Bounds, Div, Element, ElementId, GlobalElementId, InspectorElementId,
    InteractiveElement as _, IntoElement, LayoutId, Pixels, Role, SharedString, Stateful,
    StatefulInteractiveElement as _, StyledText, TextLayout, Window,
};

/// The meaning of words the current surface already paints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadingRole {
    Paragraph,
    Heading,
    Code,
    Fact,
    Status,
}

/// Whether a painted text leaf owns a native reading node.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Intent {
    Reading(ReadingRole),
    /// The enclosing, real control already owns these words as its name.
    NameOfExistingControl,
    /// Punctuation or decoration that is not a separate reading unit.
    Decoration,
}

impl Intent {
    /// All reading leaves are noneditable; a heading adds only its role.
    #[must_use]
    pub const fn native_role(self) -> Option<Role> {
        match self {
            Self::Reading(ReadingRole::Heading) => Some(Role::Heading),
            Self::Reading(
                ReadingRole::Paragraph
                | ReadingRole::Code
                | ReadingRole::Fact
                | ReadingRole::Status,
            ) => Some(Role::Label),
            Self::NameOfExistingControl | Self::Decoration => None,
        }
    }
}

/// One existing styled text layout, with an explicit reading intent.
pub struct Text {
    id: ElementId,
    text: StyledText,
    intent: Intent,
}

/// Paint and read one owned string with GPUI's normal text layout.
#[must_use]
pub fn text(id: impl Into<ElementId>, words: impl Into<SharedString>, intent: Intent) -> Text {
    styled(id, StyledText::new(words.into()), intent)
}

/// Preserve prepared runs and inline glyph geometry in one existing layout.
#[must_use]
pub fn styled(id: impl Into<ElementId>, text: StyledText, intent: Intent) -> Text {
    Text {
        id: id.into(),
        text,
        intent,
    }
}

/// Name the same measured text GPUI will paint. Measurement has already
/// applied the inherited ellipsis and line clamp; hidden tails are absent.
/// No value, SetValue, focus, or activation action is registered.
pub(crate) fn write_layout(layout: &TextLayout, node: &mut gpui::accesskit::Node) {
    node.set_label(layout.text());
}

impl IntoElement for Text {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Text {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        self.intent.native_role().map(|_| self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn a11y_role(&self) -> Option<Role> {
        self.intent.native_role()
    }

    fn write_a11y_info(&self, node: &mut gpui::accesskit::Node) {
        write_layout(self.text.layout(), node);
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.text.request_layout(None, inspector, window, cx)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .prepaint(None, inspector, bounds, state, window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut (),
        prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.text
            .paint(None, inspector, bounds, state, prepaint, window, cx);
    }
}

/// The semantics of an existing, actionable control.
#[derive(Clone, Copy)]
pub enum ControlKind {
    Button,
    Link,
    Disclosure(bool),
    Filter(bool),
}

/// Decorate the actual control before its host attaches native activation.
/// Its host retains the one callback, admission check, and focus handle.
pub fn control(
    words: impl Into<SharedString>,
    kind: ControlKind,
    element: Stateful<Div>,
) -> Stateful<Div> {
    let element = element
        .role(match kind {
            ControlKind::Link => Role::Link,
            ControlKind::Button | ControlKind::Disclosure(_) | ControlKind::Filter(_) => {
                Role::Button
            }
        })
        .aria_label(words);
    match kind {
        ControlKind::Disclosure(open) => element.aria_expanded(open),
        ControlKind::Filter(chosen) => element.aria_selected(chosen),
        ControlKind::Button | ControlKind::Link => element,
    }
}

#[cfg(test)]
mod tests;

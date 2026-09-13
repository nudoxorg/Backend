//! Buttons and key hints.
//! Three weights, one shape, no filled rectangles competing with the content.
//! The keyboard hint beside a button is part of the button, not decoration.
//!
//! Almost every action in this application has a key, and showing that key on
//! the affordance is what turns a mouse user into a keyboard user. The hint is
//! drawn in the specimen face at the faintest ink so it reads as an annotation
//! rather than as a second label.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, Space, TypeScale, hairline, radius, space, type_size};
use gpui::{
    Div, ElementId, FontWeight, InteractiveElement, ParentElement, SharedString,
    StatefulInteractiveElement, Stateful, Styled, div, px,
};

/// How loudly a button asks to be pressed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Weight {
    /// The one action a region exists for.
    Primary,
    /// An ordinary action.
    Regular,
    /// An action that should not compete with the content beside it.
    Quiet,
}

/// Returns a button shell that a view attaches its own click handler to.
pub(crate) fn button(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: &str,
    weight: Weight,
) -> Stateful<Div> {
    let (ink, ground, edge) = paints(theme, weight);
    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .flex_none()
        .items_center()
        .gap(space(Space::Tight))
        .px(space(Space::Base))
        .py(px(4.0))
        .rounded(radius(Radius::Small))
        .bg(ground)
        .border(hairline())
        .border_color(edge)
        .text_size(type_size(TypeScale::Small))
        .font_weight(FontWeight::MEDIUM)
        .text_color(ink)
        .cursor_pointer()
        .hover(|style| style.bg(theme.paint(Paint::Hover)))
        .active(|style| style.bg(theme.paint(Paint::Selected)))
        .child(label.to_owned())
}

/// Returns a compact square button holding one icon.
pub(crate) fn icon_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    mark: super::icon::Icon,
) -> Stateful<Div> {
    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .flex_none()
        .w(px(22.0))
        .h(px(22.0))
        .items_center()
        .justify_center()
        .rounded(radius(Radius::Small))
        .text_color(theme.paint(Paint::TextDim))
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme.paint(Paint::Hover))
                .text_color(theme.paint(Paint::TextStrong))
        })
        .child(super::icon::sized(theme, mark, 13.0, Paint::TextDim))
}

/// Returns a compact square button holding a single glyph.
pub(crate) fn glyph_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    glyph: &str,
) -> Stateful<Div> {
    div()
        .id(ElementId::Name(id.into()))
        .flex()
        .flex_none()
        .w(px(22.0))
        .h(px(22.0))
        .items_center()
        .justify_center()
        .rounded(radius(Radius::Small))
        .text_size(type_size(TypeScale::Small))
        .text_color(theme.paint(Paint::TextDim))
        .cursor_pointer()
        .hover(|style| {
            style
                .bg(theme.paint(Paint::Hover))
                .text_color(theme.paint(Paint::TextStrong))
        })
        .child(glyph.to_owned())
}

/// Returns the keyboard hint drawn beside an affordance.
pub(crate) fn key_hint(theme: &Theme, keys: &str) -> Div {
    div()
        .flex_none()
        .px(px(4.0))
        .py(px(1.0))
        .rounded(radius(Radius::Hair))
        .bg(theme.paint(Paint::Hover))
        .font_family(theme.specimen())
        .text_size(type_size(TypeScale::Micro))
        .text_color(theme.paint(Paint::TextFaint))
        .child(keys.to_owned())
}

fn paints(theme: &Theme, weight: Weight) -> (gpui::Hsla, gpui::Hsla, gpui::Hsla) {
    match weight {
        Weight::Primary => (
            theme.paint(Paint::Gilt),
            theme.paint(Paint::GiltWash),
            theme.paint(Paint::GiltDim),
        ),
        Weight::Regular => (
            theme.paint(Paint::Text),
            theme.paint(Paint::Panel),
            theme.paint(Paint::Hairline),
        ),
        Weight::Quiet => (
            theme.paint(Paint::TextDim),
            gpui::transparent_black(),
            gpui::transparent_black(),
        ),
    }
}

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
use crate::theme::tokens::{Radius, TypeScale, radius, type_size};
use gpui::{Div, ParentElement, SharedString, Styled, div, px};

/// The single button API used by every product view.
pub(crate) use super::components::Weight;

/// Returns a button shell that a view attaches its own click handler to.
pub(crate) fn button(
    theme: &Theme,
    id: impl Into<SharedString>,
    label: &str,
    weight: Weight,
) -> gpui_component::button::Button {
    super::components::button(theme, id, label, weight)
}

/// Returns a compact square button holding one icon.
pub(crate) fn icon_button(
    theme: &Theme,
    id: impl Into<SharedString>,
    mark: super::icon::Icon,
) -> gpui_component::button::Button {
    icon_button_labeled(theme, id, mark, icon_accessibility_label(mark))
}

/// Returns an icon button whose semantic name can differ from the icon's
/// generic label. Compact chrome uses this to keep the header truthful while
/// preserving the same CE action-tree registration as its wide text button.
pub(crate) fn icon_button_labeled(
    theme: &Theme,
    id: impl Into<SharedString>,
    mark: super::icon::Icon,
    label: impl Into<SharedString>,
) -> gpui_component::button::Button {
    super::components::icon_button(theme, id, label).child(super::icon::sized(
        theme,
        mark,
        13.0,
        Paint::TextDim,
    ))
}

/// Returns a CE icon button with the disabled state projected into both the
/// component and the rendered semantic frame.
pub(crate) fn icon_button_state(
    theme: &Theme,
    id: impl Into<SharedString>,
    mark: super::icon::Icon,
    disabled: bool,
) -> gpui_component::button::Button {
    super::components::icon_button_with_state(
        theme,
        id,
        icon_accessibility_label(mark),
        disabled,
        true,
    )
    .child(super::icon::sized(
        theme,
        mark,
        13.0,
        if disabled {
            Paint::TextFaint
        } else {
            Paint::TextDim
        },
    ))
}

fn icon_accessibility_label(icon: super::icon::Icon) -> &'static str {
    match icon {
        super::icon::Icon::Search => "Search",
        super::icon::Icon::Command => "Command palette",
        super::icon::Icon::Plus => "Add",
        super::icon::Icon::Folder => "Choose folder",
        super::icon::Icon::Close => "Close",
        super::icon::Icon::ChevronLeft => "Back",
        super::icon::Icon::ChevronRight => "Forward",
        super::icon::Icon::ChevronDown => "Expand",
        super::icon::Icon::Copy => "Copy",
        super::icon::Icon::Gear => "Settings",
        super::icon::Icon::Sun => "Vellum appearance",
        super::icon::Icon::Moon => "Ink appearance",
        super::icon::Icon::Refresh => "Refresh",
        super::icon::Icon::External => "Open externally",
        super::icon::Icon::Home => "Home",
        super::icon::Icon::ArrowLeft => "Previous",
        super::icon::Icon::ArrowRight => "Next",
        super::icon::Icon::Minimize => "Minimize",
        super::icon::Icon::Maximize => "Maximize",
        super::icon::Icon::Restore => "Restore",
        super::icon::Icon::Link => "Open link",
        super::icon::Icon::Code => "Source",
        super::icon::Icon::Spark => "Agent",
        super::icon::Icon::Play => "Run",
    }
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

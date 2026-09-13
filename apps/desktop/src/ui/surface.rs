//! Surfaces and edges: panels, raised sheets, sunken wells, and hairlines.
//! Elevation is reserved for things that float; everything else is an edge.
//! There are exactly four grounds, and no view may invent a fifth.
//!
//! Depth in this interface is carried by one hairline and one step of
//! lightness, not by shadows stacked on shadows. A shadow means "this is
//! temporarily above the page and will go away" — a sheet, a hover card, a
//! menu — so a reader can tell at a glance what is part of the document and
//! what is an interruption.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::{Radius, hairline, radius};
use gpui::{Div, Styled, div, px};

/// Returns the window ground.
pub(crate) fn ground(theme: &Theme) -> Div {
    div().bg(theme.paint(Paint::Ground))
}

/// Returns a panel resting on the ground.
pub(crate) fn panel(theme: &Theme) -> Div {
    div().bg(theme.paint(Paint::Panel))
}

/// Returns a floating surface: a sheet, a hover card, or a menu.
pub(crate) fn raised(theme: &Theme) -> Div {
    div()
        .bg(theme.paint(Paint::Raised))
        .rounded(radius(Radius::Large))
        .border(hairline())
        .border_color(theme.paint(Paint::HairlineStrong))
        .shadow_lg()
}

/// Returns a recessed well: a signature specimen or a source block.
pub(crate) fn sunken(theme: &Theme) -> Div {
    div()
        .bg(theme.paint(Paint::Sunken))
        .rounded(radius(Radius::Medium))
        .border(hairline())
        .border_color(theme.paint(Paint::Hairline))
}

/// Returns a full-width horizontal hairline.
pub(crate) fn rule(theme: &Theme) -> Div {
    div()
        .h(hairline())
        .w_full()
        .flex_none()
        .bg(theme.paint(Paint::Hairline))
}

/// Returns a full-height vertical hairline.
pub(crate) fn spine(theme: &Theme) -> Div {
    div()
        .w(hairline())
        .h_full()
        .flex_none()
        .bg(theme.paint(Paint::Hairline))
}

/// Returns the scrim drawn behind a floating surface.
pub(crate) fn scrim(theme: &Theme) -> Div {
    div().absolute().inset_0().bg(theme.paint(Paint::Scrim))
}

/// Returns a focus ring drawn around the region that owns the keyboard.
pub(crate) fn focus_ring(theme: &Theme, focused: bool) -> Div {
    let ink = if focused {
        theme.paint(Paint::Focus)
    } else {
        theme.paint(Paint::Hairline)
    };
    div().border(px(1.0)).border_color(ink)
}

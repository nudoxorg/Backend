//! Surfaces and edges: panels, floating sheets, recessed wells, and rules.
//! Elevation is reserved for things that float; everything else is an edge.
//! Every route uses the same abyss steps and cut edge grammar.
//!
//! Depth in this interface is carried by one hairline and one step of
//! lightness, not by shadows stacked on shadows. A shadow means "this is
//! temporarily above the page and will go away" — a sheet, a hover card, a
//! menu — so a reader can tell at a glance what is part of the document and
//! what is an interruption.

use crate::theme::Theme;
use crate::theme::palette::Paint;
use crate::theme::tokens::hairline;
use gpui::{Div, ParentElement, Styled, div};

/// Returns the window ground.
pub(crate) fn ground(theme: &Theme) -> Div {
    let mut facet_ink = theme.paint(Paint::Focus);
    facet_ink.alpha = 0.075;
    div().relative().bg(theme.paint(Paint::Abyss0)).child(
        crate::ui::icon::asset(crate::ui::icon::FACETS_PATH, 1440.0, facet_ink)
            .absolute()
            .inset_0()
            .w_full()
            .h_full(),
    )
}

/// Returns a panel resting on the abyss ground.
pub(crate) fn panel(theme: &Theme) -> Div {
    div().bg(theme.paint(Paint::Abyss1))
}

/// Returns a flat plate with the hard edge language used by package headers,
/// section rails, and state specimens. The diagonal edge markers are kept as
/// children so this primitive remains available to every route without asking
/// a view to hand-build the bevel.
pub(crate) fn cut(theme: &Theme, accent: Paint) -> Div {
    div()
        .relative()
        .bg(theme.paint(Paint::Abyss1))
        .border(hairline())
        .border_color(theme.paint(Paint::Rule3))
        .child(
            div()
                .absolute()
                .top_0()
                .right_0()
                .w(gpui::px(18.0))
                .h(hairline())
                .bg(theme.paint(accent)),
        )
        .child(
            div()
                .absolute()
                .bottom_0()
                .left_0()
                .w(gpui::px(18.0))
                .h(hairline())
                .bg(theme.paint(accent)),
        )
}

/// Returns a floating surface: a sheet, a hover card, or a menu.
pub(crate) fn raised(theme: &Theme) -> Div {
    div()
        .bg(theme.paint(Paint::Abyss2))
        .border(hairline())
        .border_color(theme.paint(Paint::Rule3))
        .shadow_lg()
}

/// Returns a recessed well: a signature specimen or a source block.
pub(crate) fn sunken(theme: &Theme) -> Div {
    div()
        .bg(theme.paint(Paint::Abyss0))
        .border(hairline())
        .border_color(theme.paint(Paint::Rule1))
}

/// Returns the veil drawn behind a floating surface.
pub(crate) fn scrim(theme: &Theme) -> Div {
    div().absolute().inset_0().bg(theme.paint(Paint::Veil))
}

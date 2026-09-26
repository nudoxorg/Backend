//! Holds: a bracket of fields. The heading notes how many are private; a
//! private field's name is quieter. A type with no fields "holds nothing — a
//! marker".

use super::fork::{gutter, part_row};
use super::text::{Line, Links, TypeInk};
use super::{heading, roles};
use crate::measure::{Measure, Set};
use crate::semantics::model::Holds;
use crate::theme::ActiveFacet;
use gpui::{App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px};
use std::sync::Arc;

/// Holds. Build with [`holds`].
#[derive(IntoElement)]
pub struct HoldsView {
    id: ElementId,
    holds: Holds,
    measure: Measure,
    links: Links,
}

/// The holds bracket for `holds` at `measure`.
#[must_use]
pub fn holds(id: impl Into<ElementId>, holds: Holds, measure: &Measure, links: &Links) -> HoldsView {
    HoldsView { id: id.into(), holds, measure: *measure, links: links.clone() }
}

/// A `[`-shaped bracket down the left of a group of rows (solid, or dashed
/// for what you write).
pub(crate) fn bracket(measure: &Measure, color: gpui::Hsla, dashed: bool) -> gpui::Div {
    let s = measure.scale();
    let b = div()
        .absolute()
        .left(px(4.0 * s))
        .top(px(8.0 * s))
        .bottom(px(8.0 * s))
        .w(px(7.0 * s))
        .border_l_1()
        .border_t_1()
        .border_b_1()
        .border_color(color);
    if dashed { b.border_dashed() } else { b }
}

impl RenderOnce for HoldsView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let xray = m.reveal().xray;
        let ink = TypeInk::new(roles::TYPE, palette);
        let gutter = gutter(&m);
        let rows_measure = m.within(m.width() - gutter);
        let mut root = div().id(self.id.clone()).flex().flex_col().child(heading(self.holds.heading(), &m, palette));
        if self.holds.fields.is_empty() {
            return root;
        }
        let rows = self.holds.fields.iter().enumerate().map(|(n, field)| {
            let name_role = if field.public { roles::NAME } else { roles::NAME_QUIET };
            let name_ink = if field.public { palette.ink0 } else { palette.ink2 };
            let name = div()
                .set(name_role, &rows_measure)
                .text_color(name_ink.hsla())
                .children(field.name.clone())
                .into_any_element();
            let mut line = Line::new();
            line.spelled(&field.ty, &ink, &self.links, xray);
            let id = ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(format!("{n}-ty")));
            let ty = line.element(id, roles::TYPE, &rows_measure, &self.links, palette);
            part_row(name, Some(ty), field.doc.clone(), &rows_measure, palette)
        });
        root = root.child(
            div()
                .relative()
                .pl(gutter)
                .flex()
                .flex_col()
                .child(bracket(&m, palette.line3.hsla(), false))
                .children(rows),
        );
        root
    }
}

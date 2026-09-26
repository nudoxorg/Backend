//! The fork: "one of" — a rail with a branch per variant.
//!
//! A vertical rail runs down the left of the rows; each branch leaves it with
//! a short tick and a small open diamond, then the variant's name, what it
//! carries (in plain words: `SemanticLinkKind × RelationDirection`, or its
//! named parts) and its one sentence. In a narrow room each row stacks.

use super::text::{Line, Links, TypeInk};
use super::{heading, k, roles, row_pad, stacked};
use crate::measure::{Measure, Set};
use crate::semantics::model::{Fork, Payload};
use crate::theme::ActiveFacet;
use crate::tokens::Palette;
use gpui::{
    InteractiveElement,
    AnyElement, App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px,
};
use std::sync::Arc;

/// A fork. Build with [`fork`].
#[derive(IntoElement)]
pub struct ForkView {
    id: ElementId,
    fork: Fork,
    measure: Measure,
    links: Links,
}

/// The fork for `fork` at `measure`.
#[must_use]
pub fn fork(id: impl Into<ElementId>, fork: Fork, measure: &Measure, links: &Links) -> ForkView {
    ForkView { id: id.into(), fork, measure: *measure, links: links.clone() }
}

/// The left gutter every bracketed or railed part leaves for its mark.
pub(crate) fn gutter(measure: &Measure) -> gpui::Pixels {
    k(measure, 22.0)
}

/// A row's name / type / sentence, side by side or stacked.
pub(crate) fn part_row(
    name: AnyElement,
    ty: Option<AnyElement>,
    say: Option<SharedString>,
    measure: &Measure,
    palette: &Palette,
) -> gpui::Div {
    let say = say.map(|s| {
        let d = div().set(roles::SAY, measure).text_color(palette.ink3.hsla()).min_w_0();
        if stacked(measure) { d.child(s) } else { d.flex_1().truncate().child(s) }
    });
    let pad = row_pad(measure, 7.0);
    let mut row = div().relative().py(pad).min_h(row_pad(measure, 32.0));
    if stacked(measure) {
        row = row.flex().flex_col().gap(px(2.0 * measure.scale())).child(name).children(ty).children(say);
    } else {
        row = row
            .flex()
            .items_baseline()
            .gap(k(measure, 18.0))
            .child(div().flex_none().child(name))
            .children(ty.map(|t| div().flex_shrink_1().min_w_0().child(t)))
            .children(say);
    }
    row
}

fn diamond(size: f32, color: gpui::Hsla) -> impl IntoElement {
    gpui::canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let c = bounds.center();
            let r = px(size / 2.0);
            let mut path = gpui::PathBuilder::stroke(px(1.0));
            path.move_to(gpui::point(c.x, c.y - r));
            path.line_to(gpui::point(c.x + r, c.y));
            path.line_to(gpui::point(c.x, c.y + r));
            path.line_to(gpui::point(c.x - r, c.y));
            path.close();
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        },
    )
    .size(px(size + 2.0))
}

impl RenderOnce for ForkView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let xray = m.reveal().xray;
        let ink = TypeInk::new(roles::TYPE, palette);
        let line3 = palette.line3.hsla();
        let gutter = gutter(&m);
        let rows_measure = m.within(m.width() - gutter);
        let tick = px(9.0 * m.scale());
        let rows = self.fork.branches.iter().enumerate().map(|(n, branch)| {
            let id = |part: &str| ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(format!("{n}-{part}")));
            let name = div()
                .set(roles::NAME, &rows_measure)
                .text_color(palette.ink0.hsla())
                .child(branch.name.clone())
                .into_any_element();
            let ty = match &branch.payload {
                Payload::Unit => None,
                Payload::Tuple(parts) => {
                    let mut line = Line::new();
                    for (q, part) in parts.iter().enumerate() {
                        if q > 0 {
                            line.push(" × ", roles::TYPE, palette.ink4.hsla());
                        }
                        line.spelled(part, &ink, &self.links, xray);
                    }
                    Some(line.element(id("ty"), roles::TYPE, &rows_measure, &self.links, palette))
                }
                Payload::Record(parts) => {
                    let mut line = Line::new();
                    for (q, (field, ty)) in parts.iter().enumerate() {
                        if q > 0 {
                            line.push(", ", roles::TYPE, palette.ink4.hsla());
                        }
                        line.push(field, roles::TYPE, palette.ink2.hsla());
                        line.push(" ", roles::TYPE, palette.ink4.hsla());
                        line.spelled(ty, &ink, &self.links, xray);
                    }
                    Some(line.element(id("ty"), roles::TYPE, &rows_measure, &self.links, palette))
                }
            };
            // The branch: a tick from the rail and an open diamond, centred on
            // the name's line.
            let mid = row_pad(&m, 7.0) + px(roles::NAME.line * m.scale() / 2.0);
            let branch_mark = div()
                .absolute()
                .left(-gutter + px(5.0 * m.scale()))
                .top(mid)
                .flex()
                .items_center()
                .child(div().w(tick).h(px(1.0)).bg(line3))
                .child(diamond(5.0 * m.scale(), palette.ink3.hsla()))
                .mt(px(-(5.0 * m.scale() + 2.0) / 2.0));
            part_row(name, ty, branch.doc.clone(), &rows_measure, palette).child(branch_mark)
        });
        let rail_inset = row_pad(&m, 7.0) + px(roles::NAME.line * m.scale() / 2.0);
        div()
            .id(self.id.clone())
            .flex()
            .flex_col()
            .child(heading(self.fork.heading(), &m, palette))
            .child(
                div()
                    .relative()
                    .pl(gutter)
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .absolute()
                            .left(px(5.0 * m.scale()))
                            .top(rail_inset)
                            .bottom(rail_inset)
                            .w(px(1.0))
                            .bg(line3),
                    )
                    .children(rows),
            )
    }
}

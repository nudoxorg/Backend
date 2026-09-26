//! The static prism: the same groups the graph gathers beside a focused
//! symbol, at rest on the page. What it comes from sits in named groups on
//! the left, what it goes into on the right; a quiet curve runs from each
//! row to the gem in the middle, and the gem opens the graph (G). Below
//! [`ONE_COLUMN_BELOW`] effective px the prism becomes one column on a rail:
//! the left groups, the symbol, the right groups.

use super::text::{Line, Links};
use super::{k, roles};
use crate::graph::Kind;
use crate::measure::{Measure, Set};
use crate::semantics::model::{Column, PrismRow};
use crate::semantics::types::Target;
use crate::theme::ActiveFacet;
use crate::tokens::Palette;
use gpui::{
    App, Bounds, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement, PathBuilder,
    Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div, point, px,
};
use std::sync::Arc;

/// Below this effective width the prism is one column.
pub const ONE_COLUMN_BELOW: f32 = 620.0;

/// Open the graph at a symbol: dispatched by the prism's gem.
#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = facet, no_json)]
pub struct ToGraph {
    /// The symbol to gather the prism around.
    pub target: Target,
}

/// A prism. Build with [`prism`].
#[derive(IntoElement)]
pub struct PrismView {
    id: ElementId,
    node: Target,
    name: SharedString,
    left: Vec<Column>,
    right: Vec<Column>,
    measure: Measure,
    links: Links,
}

/// The prism around `node` (named `name`) with its columns at `measure`.
#[must_use]
pub fn prism(
    id: impl Into<ElementId>,
    node: Target,
    name: impl Into<SharedString>,
    columns: (Vec<Column>, Vec<Column>),
    measure: &Measure,
    links: &Links,
) -> PrismView {
    PrismView {
        id: id.into(),
        node,
        name: name.into(),
        left: columns.0,
        right: columns.1,
        measure: *measure,
        links: links.clone(),
    }
}

/// The shape a row's mark takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mark {
    /// A filled diamond: a type or value.
    Solid,
    /// A square: a callable.
    Square,
    /// An open diamond: a trait.
    Open,
}

fn mark_of(kind: Option<Kind>) -> Mark {
    match kind {
        Some(Kind::Function | Kind::Method | Kind::Macro | Kind::Constant) => Mark::Square,
        Some(Kind::Trait) | None => Mark::Open,
        _ => Mark::Solid,
    }
}

fn diamond(builder: &mut PathBuilder, c: gpui::Point<Pixels>, r: Pixels) {
    builder.move_to(point(c.x, c.y - r));
    builder.line_to(point(c.x + r, c.y));
    builder.line_to(point(c.x, c.y + r));
    builder.line_to(point(c.x - r, c.y));
    builder.close();
}

fn row_mark(kind: Option<Kind>, measure: &Measure, color: gpui::Hsla) -> impl IntoElement {
    let s = measure.scale();
    let shape = mark_of(kind);
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let c = bounds.center();
            match shape {
                Mark::Square => {
                    let h = px(3.0 * s);
                    window.paint_quad(gpui::fill(Bounds::from_corners(point(c.x - h, c.y - h), point(c.x + h, c.y + h)), color));
                }
                Mark::Solid | Mark::Open => {
                    let mut b = if shape == Mark::Open { PathBuilder::stroke(px(1.2)) } else { PathBuilder::fill() };
                    diamond(&mut b, c, px(3.8 * s));
                    if let Ok(path) = b.build() {
                        window.paint_path(path, color);
                    }
                }
            }
        },
    )
    .flex_none()
    .size(px(9.0 * s))
}

/// Row and head heights and the gap between groups, px at 100 %.
const ROW: f32 = 24.0;
const HEAD: f32 = 24.0;
const GAP: f32 = 8.0;
/// Columns end and start this far either side of the gem, px at 100 %.
const REACH: f32 = 150.0;

fn column_height(groups: &[Column]) -> f32 {
    let h: f32 = groups
        .iter()
        .map(|g| {
            #[allow(clippy::cast_precision_loss)]
            let rows = (g.rows.len() + usize::from(g.more > 0)) as f32;
            HEAD + ROW * rows + GAP
        })
        .sum();
    (h - GAP).max(0.0)
}

/// Each row's centre, from the top of its column, px at 100 %.
fn row_centres(groups: &[Column]) -> Vec<f32> {
    let mut out = Vec::new();
    let mut y = 0.0;
    for g in groups {
        y += HEAD;
        for _ in &g.rows {
            out.push(y + ROW / 2.0);
            y += ROW;
        }
        if g.more > 0 {
            y += ROW;
        }
        y += GAP;
    }
    out
}

struct Ctx<'a> {
    id: &'a ElementId,
    measure: &'a Measure,
    links: &'a Links,
    palette: &'static Palette,
}

impl Ctx<'_> {
    fn key(&self, part: String) -> ElementId {
        ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(part))
    }

    fn row(&self, n: String, row: &PrismRow, left: bool) -> gpui::Div {
        let m = self.measure;
        let palette = self.palette;
        let target = row.node.map(Target::Node);
        let yours = target.as_ref().is_some_and(|t| (self.links.yours)(t));
        let mut line = Line::new();
        let ink = if yours { palette.mint.base } else { palette.ink1 };
        match target {
            Some(t) => {
                line.link(&row.text, roles::PRISM_ROW, ink.hsla(), t);
            }
            None => {
                line.push(&row.text, roles::PRISM_ROW, palette.ink2.hsla());
            }
        }
        let name = line.element(self.key(format!("row-{n}")), roles::PRISM_ROW, m, self.links, palette);
        let note = row.note.clone().map(|note| div().flex_none().set(roles::NOTE, m).text_color(palette.ink4.hsla()).child(note));
        let mark_ink = if yours { palette.mint.base } else { palette.ink2 };
        let mark = row_mark(row.kind, m, mark_ink.hsla());
        let base = div().h(px(ROW * m.scale())).flex().items_center().gap(k(m, 9.0)).whitespace_nowrap();
        if left {
            base.justify_end().children(note).child(name).child(mark)
        } else {
            base.child(mark).child(name).children(note)
        }
    }

    fn column(&self, side: &str, groups: &[Column], left: bool) -> gpui::Div {
        let m = self.measure;
        let palette = self.palette;
        let mut col = div().flex().flex_col().gap(px(GAP * m.scale()));
        for (g, group) in groups.iter().enumerate() {
            let head = div()
                .h(px(HEAD * m.scale()))
                .flex()
                .items_end()
                .pb(px(5.0 * m.scale()))
                .set(roles::HEAD, m)
                .text_color(palette.ink3.hsla())
                .child(group.word.text());
            let head = if left { head.justify_end() } else { head };
            let mut block = div().flex().flex_col().child(head);
            for (r, row) in group.rows.iter().enumerate() {
                block = block.child(self.row(format!("{side}-{g}-{r}"), row, left));
            }
            if group.more > 0 {
                let more = div()
                    .h(px(ROW * m.scale()))
                    .flex()
                    .items_center()
                    .set(roles::QUIET, m)
                    .text_color(palette.ink4.hsla())
                    .child(SharedString::from(format!("and {} more", group.more)));
                block = block.child(if left { more.justify_end() } else { more });
            }
            col = col.child(block);
        }
        col
    }
}

fn gem(measure: &Measure, palette: &Palette) -> impl IntoElement {
    let s = measure.scale();
    let fill = palette.peri.base.hsla();
    let ring = palette.peri.base.hsla().opacity(0.45);
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let c = bounds.center();
            let mut b = PathBuilder::fill();
            diamond(&mut b, c, px(9.9 * s));
            if let Ok(path) = b.build() {
                window.paint_path(path, fill);
            }
            let mut b = PathBuilder::stroke(px(1.0));
            diamond(&mut b, c, px(17.0 * s));
            if let Ok(path) = b.build() {
                window.paint_path(path, ring);
            }
        },
    )
    .size(px(36.0 * s))
}

impl RenderOnce for PrismView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let s = m.scale();
        let ctx = Ctx { id: &self.id, measure: &m, links: &self.links, palette };
        let root = div().id(self.id.clone());
        if self.left.is_empty() && self.right.is_empty() {
            return root;
        }
        let to_graph = self.node.clone();
        let gem_el = div()
            .id(ctx.key("gem".into()))
            .flex_none()
            .cursor_pointer()
            .child(gem(&m, palette))
            .on_mouse_up(MouseButton::Left, move |_, window, cx| {
                window.dispatch_action(Box::new(ToGraph { target: to_graph.clone() }), cx);
                cx.stop_propagation();
            });
        if m.effective() < ONE_COLUMN_BELOW {
            // One column on a rail: comes from, the symbol, goes into.
            let name = div().set(roles::NAME, &m).text_color(palette.ink0.hsla()).child(self.name.clone());
            let here = div()
                .flex()
                .items_center()
                .gap(k(&m, 4.0))
                .ml(-px(16.0 * s) - px(18.0 * s))
                .child(gem_el)
                .child(name);
            return root
                .flex()
                .flex_col()
                .gap(k(&m, 6.0))
                .pl(px(16.0 * s))
                .border_l_1()
                .border_color(palette.line3.hsla())
                .child(ctx.column("l", &self.left, false))
                .child(here)
                .child(ctx.column("r", &self.right, false));
        }
        let hl = column_height(&self.left);
        let hr = column_height(&self.right);
        let h = hl.max(hr).max(40.0);
        let width = f32::from(m.width()) / s;
        let cx0 = width / 2.0;
        let yl: Vec<f32> = row_centres(&self.left).into_iter().map(|y| y + (h - hl) / 2.0).collect();
        let yr: Vec<f32> = row_centres(&self.right).into_iter().map(|y| y + (h - hr) / 2.0).collect();
        let curve = palette.line3.hsla();
        let curves = canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let at = |x: f32, y: f32| point(bounds.left() + px(x * s), bounds.top() + px(y * s));
                let mid = h / 2.0;
                let mut draw = |x0: f32, y0: f32, x1: f32, y1: f32| {
                    let c = (x1 - x0) * 0.5;
                    let mut b = PathBuilder::stroke(px(1.0));
                    b.move_to(at(x0, y0));
                    b.cubic_bezier_to(at(x1, y1), at(x0 + c, y0), at(x1 - c, y1));
                    if let Ok(path) = b.build() {
                        window.paint_path(path, curve);
                    }
                };
                for &y in &yl {
                    draw(cx0 - REACH + 6.0, y, cx0 - 14.0, mid);
                }
                for &y in &yr {
                    draw(cx0 + 14.0, mid, cx0 + REACH - 6.0, y);
                }
            },
        )
        .absolute()
        .inset_0();
        let left = ctx
            .column("l", &self.left, true)
            .absolute()
            .right(px((width - cx0 + REACH) * s))
            .top(px((h - hl) / 2.0 * s));
        let right = ctx
            .column("r", &self.right, false)
            .absolute()
            .left(px((cx0 + REACH) * s))
            .top(px((h - hr) / 2.0 * s));
        let gem_box = div()
            .absolute()
            .left(px(cx0 * s - 18.0 * s))
            .top(px(h / 2.0 * s - 18.0 * s))
            .child(gem_el);
        root.relative()
            .w_full()
            .h(px(h * s))
            .my(k(&m, 6.0))
            .child(curves)
            .child(left)
            .child(right)
            .child(gem_box)
    }
}

/// The prism's row rectangles, for tests: `(left, right)` row centres in px
/// at 100 %.
#[must_use]
pub fn centres(columns: &(Vec<Column>, Vec<Column>)) -> (Vec<f32>, Vec<f32>) {
    (row_centres(&columns.0), row_centres(&columns.1))
}

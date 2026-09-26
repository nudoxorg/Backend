//! Getting one / Calling it: each route as a rail read left to right like a
//! transit line — where it starts in plain words (`from text what`), each
//! step a boxed link, each intermediate type a station ◇, the target's ◆ at
//! the end; inputs that are not on the spine ride along (`+ a
//! DeclarationKind kind`); `?` marks a step that may fail. The best route
//! is at full strength, the others quieter until touched. Holding ⌥ swaps
//! every rail for its code.

use super::text::{Line, Links, TypeInk};
use super::{k, roles, section_title};
use crate::measure::{Measure, Set};
use crate::semantics::model::{RailView, RecipeView, StepView};
use crate::semantics::types::{Piece, Spelled};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole};
use gpui::{
    AnyElement, App, ElementId, InteractiveElement, IntoElement, ParentElement, PathBuilder, RenderOnce, SharedString,
    Styled, Window, canvas, div, point, px,
};
use std::sync::Arc;

/// Getting one / Calling it. Build with [`recipe`].
#[derive(IntoElement)]
pub struct RecipeSection {
    id: ElementId,
    view: RecipeView,
    measure: Measure,
    links: Links,
}

/// The section for `view` at `measure`.
#[must_use]
pub fn recipe(id: impl Into<ElementId>, view: RecipeView, measure: &Measure, links: &Links) -> RecipeSection {
    RecipeSection { id: id.into(), view, measure: *measure, links: links.clone() }
}

const STEP: TypeRole = TypeRole { weight: 500.0, ..roles::PRISM_ROW };
const RIDER: TypeRole = TypeRole { size: 11.5, line: 16.0, ..roles::PRISM_ROW };
const MARK: TypeRole = TypeRole { weight: 600.0, size: 11.0, line: 16.0, ..roles::PRISM_ROW };
const LEAD: TypeRole = TypeRole { size: 12.5, line: 18.0, ..roles::PRISM_ROW };
const FOOT: TypeRole = TypeRole { size: 11.5, line: 16.0, ..roles::QUIET };
const SENTENCE: TypeRole = TypeRole { size: 13.0, line: 19.0, ..roles::QUIET };
const CODE: TypeRole = TypeRole { line: 20.0, ..roles::CODE };

/// Pieces as one line: words quiet, names links, variables italic.
fn pieces(line: &mut Line, pieces: &[Piece], base: TypeRole, words: gpui::Hsla, links: &Links, palette: &Palette) {
    let mut ink = TypeInk::new(base, palette);
    ink.words = words;
    ink.var = palette.ink4.hsla();
    ink.prim = words;
    line.spelled(&Spelled { pieces: pieces.to_vec(), source: SharedString::default() }, &ink, links, false);
}

fn connector(measure: &Measure, palette: &Palette) -> impl IntoElement {
    div().flex_none().w(px(14.0 * measure.scale())).h(px(1.0)).bg(palette.peri.line.hsla())
}

fn diamond(size: f32, color: gpui::Hsla, filled: bool, ring: Option<gpui::Hsla>) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let c = bounds.center();
            let shape = |r: gpui::Pixels, b: &mut PathBuilder| {
                b.move_to(point(c.x, c.y - r));
                b.line_to(point(c.x + r, c.y));
                b.line_to(point(c.x, c.y + r));
                b.line_to(point(c.x - r, c.y));
                b.close();
            };
            if let Some(ring) = ring {
                let mut b = PathBuilder::fill();
                shape(px(size * 0.5 + 3.5), &mut b);
                if let Ok(path) = b.build() {
                    window.paint_path(path, ring);
                }
            }
            let mut b = if filled { PathBuilder::fill() } else { PathBuilder::stroke(px(1.3)) };
            shape(px(size * 0.5), &mut b);
            if let Ok(path) = b.build() {
                window.paint_path(path, color);
            }
        },
    )
    .flex_none()
    .size(px(size + 8.0))
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

    fn step(&self, key: String, step: &StepView, first: bool, best: bool) -> gpui::Div {
        let m = self.measure;
        let p = self.palette;
        let mut verb = Line::new();
        // The best route's steps speak; the others are a step quieter.
        verb.link(&step.verb, STEP, if best { p.ink1 } else { p.ink2 }.hsla(), step.target.clone());
        let boxed = div()
            .flex_none()
            .px(px(6.0 * m.scale()))
            .py(px(1.0 * m.scale()))
            .border_1()
            .border_color(p.line2.hsla())
            .child(verb.element(self.key(format!("{key}-verb")), STEP, m, self.links, p));
        let mut unit = div().flex().flex_none().items_center();
        if !first {
            unit = unit.child(connector(m, p));
        }
        unit = unit.child(boxed);
        if step.fails || step.maybe {
            let ink = if step.fails { p.ink3 } else { p.ink4 };
            unit = unit.child(div().ml(px(2.0 * m.scale())).set(MARK, m).text_color(ink.hsla()).child("?"));
        }
        for (r, rider) in step.riders.iter().enumerate() {
            let mut line = Line::new();
            pieces(&mut line, rider, RIDER, p.ink4.hsla(), self.links, p);
            unit = unit.child(div().ml(k(m, 6.0)).child(line.element(self.key(format!("{key}-ride-{r}")), RIDER, m, self.links, p)));
        }
        unit.child(connector(m, p))
    }

    fn rail(&self, n: usize, rail: &RailView) -> AnyElement {
        let m = self.measure;
        let p = self.palette;
        if m.reveal().xray {
            let mut code = div().flex().flex_col().mt(px(2.0 * m.scale())).px(k(m, 12.0)).py(k(m, 7.0)).bg(p.well.hsla());
            for (q, text) in rail.code.split('\n').enumerate() {
                let mut line = Line::new();
                line.push(text, CODE, p.ink1.hsla());
                code = code.child(line.element(self.key(format!("{n}-code-{q}")), CODE, &m.inset(k(m, 12.0)), self.links, p));
            }
            return code.into_any_element();
        }
        let mut row = div().flex().flex_wrap().items_center().gap_y(k(m, 8.0)).min_w_0();
        if !rail.lead.is_empty() {
            let mut line = Line::new();
            pieces(&mut line, &rail.lead, LEAD, p.ink3.hsla(), self.links, p);
            row = row.child(div().flex_none().mr(px(2.0 * m.scale())).child(line.element(self.key(format!("{n}-lead")), LEAD, m, self.links, p)));
        }
        for (s, step) in rail.steps.iter().enumerate() {
            row = row.child(self.step(format!("{n}-{s}"), step, s == 0 && rail.lead.is_empty(), n == 0));
            if let Some(station) = &step.station {
                let mut line = Line::new();
                pieces(&mut line, station, STEP, p.ink3.hsla(), self.links, p);
                row = row.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.0 * m.scale()))
                        .child(diamond(5.0 * m.scale(), p.ink3.hsla(), false, None))
                        .child(line.element(self.key(format!("{n}-{s}-station")), STEP, m, self.links, p)),
                );
            }
        }
        let peri = p.peri.base.hsla();
        row = row.child(diamond(8.0 * m.scale(), peri, !rail.call, (!rail.call).then(|| p.peri.soft.hsla())));
        if !rail.also.is_empty() {
            let mut line = Line::new();
            pieces(&mut line, &rail.also, RIDER, p.ink4.hsla(), self.links, p);
            row = row.child(div().ml(k(m, 10.0)).min_w_0().child(line.element(self.key(format!("{n}-also")), FOOT, m, self.links, p)));
        }
        row.into_any_element()
    }
}

impl RenderOnce for RecipeSection {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let ctx = Ctx { id: &self.id, measure: &m, links: &self.links, palette };
        let mut root = div().id(self.id.clone()).flex().flex_col().gap(k(&m, 10.0)).child(section_title(self.view.heading, &m, palette));
        if let Some(sentence) = &self.view.sentence {
            root = root.child(div().set(SENTENCE, &m).text_color(palette.ink3.hsla()).child(sentence.clone()));
        }
        for (n, rail) in self.view.rails.iter().enumerate() {
            // The prototype dims the other routes to 62 %; that takes their
            // words under 4.5:1, so they are quieter by ink instead.
            root = root.child(div().id(ctx.key(format!("route-{n}"))).flex().flex_col().child(ctx.rail(n, rail)));
        }
        if let Some(foot) = &self.view.foot {
            root = root.child(div().set(FOOT, &m).text_color(palette.ink4.hsla()).child(foot.clone()));
        }
        root
    }
}

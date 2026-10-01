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
use crate::semantics::model::{RailView, RecipePort, RecipeView, StepView};
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
    title: bool,
    foot: bool,
}

/// The section for `view` at `measure`.
#[must_use]
pub fn recipe(id: impl Into<ElementId>, view: RecipeView, measure: &Measure, links: &Links) -> RecipeSection {
    RecipeSection { id: id.into(), view, measure: *measure, links: links.clone(), title: true, foot: true }
}

impl RecipeSection {
    /// The page owns its tracked heading.
    #[must_use]
    pub fn without_title(mut self) -> Self { self.title = false; self }

    /// The page can supply this same producer summary beside its exact
    /// relation group, while the standalone recipe still keeps its foot.
    #[must_use]
    pub fn without_foot(mut self) -> Self { self.foot = false; self }
}

const STEP: TypeRole = TypeRole { weight: 600.0, size: 13.5, line: 20.0, ..roles::PRISM_ROW };
const RIDER: TypeRole = TypeRole { size: 12.5, line: 18.0, ..roles::PRISM_ROW };
const MARK: TypeRole = TypeRole { weight: 600.0, size: 11.0, line: 16.0, ..roles::PRISM_ROW };
const LEAD: TypeRole = TypeRole { size: 13.5, line: 20.0, ..roles::PRISM_ROW };
const FOOT: TypeRole = TypeRole { size: 12.5, line: 18.0, ..roles::QUIET };
const SENTENCE: TypeRole = TypeRole { size: 13.0, line: 19.0, ..roles::QUIET };
const CODE: TypeRole = TypeRole { line: 20.0, ..roles::CODE };

/// Pieces as one line: words quiet, names links, variables italic.
fn pieces(line: &mut Line, pieces: &[Piece], base: TypeRole, words: gpui::Hsla, links: &Links, palette: &Palette) {
    let mut ink = TypeInk::new(base, palette);
    ink.words = words;
    ink.var = palette.ink1.hsla();
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

    fn port(&self, key: String, port: &RecipePort) -> gpui::Div {
        let m = self.measure;
        let p = self.palette;
        let mut line = Line::new();
        pieces(&mut line, &port.ty, LEAD, p.ink1.hsla(), self.links, p);
        let mut row = div().flex().items_center().min_w_0().gap(k(m, 6.0))
            .py(k(m, 1.0))
            .child(div().flex_none().w(px(3.0)).h(k(m, 14.0)).bg(p.peri.base.hsla()))
            .child(line.element(self.key(format!("{key}-type")), LEAD, m, self.links, p));
        if let Some(name) = &port.name {
            row = row.child(div().set(RIDER, m).text_color(p.ink1.hsla()).child(name.clone()));
        }
        row
    }

    fn step(&self, key: String, step: &StepView, first: bool) -> gpui::Div {
        let m = self.measure;
        let p = self.palette;
        let mut verb = Line::new();
        // The verb is the only underlined stop on this route.
        verb.link(&step.verb, STEP, p.ink1.hsla(), step.target.clone());
        let action = div()
            .flex_none().py(k(m, 1.0)).px(k(m, 4.0))
            .border_b_1().border_color(p.peri.base.hsla())
            .child(verb.element(self.key(format!("{key}-verb")), STEP, m, self.links, p));
        let mut head = div().flex().flex_none().items_center();
        if !first {
            head = head.child(connector(m, p));
        }
        head = head.child(action).child(connector(m, p));
        let mut unit = div().flex().flex_col().gap(k(m, 3.0)).child(head);
        if step.fails || step.maybe {
            let label = if step.fails { "may fail" } else { "may give nothing" };
            let color = if step.fails { p.coral.base.hsla() } else { p.peri.base.hsla() };
            unit = unit.child(div().flex().items_center().ml(k(m, 16.0))
                .child(super::operation::connector(m, color, true))
                .child(div().set(MARK, m).text_color(p.ink1.hsla()).child(label)));
        }
        for (r, rider) in step.side_inputs.iter().enumerate() {
            unit = unit.child(div().flex().items_center().ml(k(m, 8.0))
                .child(div().set(MARK, m).text_color(p.ink1.hsla()).child("+"))
                .child(self.port(format!("{key}-ride-{r}"), rider)));
        }
        unit
    }

    fn rail(&self, n: usize, rail: &RailView, outcome: Option<&[Piece]>) -> AnyElement {
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
        let mut row = div().flex().flex_wrap().items_start().gap_y(k(m, 10.0)).min_w_0();
        if !rail.starts.is_empty() {
            let mut start = div().flex().flex_col().gap(k(m, 4.0));
            for (i, port) in rail.starts.iter().enumerate() {
                start = start.child(self.port(format!("{n}-start-{i}"), port));
            }
            row = row.child(start.mr(k(m, 4.0)));
        }
        for (s, step) in rail.steps.iter().enumerate() {
            row = row.child(self.step(format!("{n}-{s}"), step, s == 0 && rail.starts.is_empty()));
            if let Some(station) = &step.station {
                let mut line = Line::new();
                pieces(&mut line, station, STEP, p.ink1.hsla(), self.links, p);
                row = row.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(2.0 * m.scale()))
                        .child(diamond(5.0 * m.scale(), p.peri.base.hsla(), false, None))
                        .child(line.element(self.key(format!("{n}-{s}-station")), STEP, m, self.links, p)),
                );
            }
        }
        let peri = p.peri.base.hsla();
        let mut end = div().flex().items_center().gap(k(m, 5.0)).py(k(m, 1.0))
            .child(diamond(8.0 * m.scale(), peri, !rail.call, (!rail.call).then(|| p.peri.soft.hsla())));
        if let Some(outcome) = outcome {
            let mut line = Line::new();
            pieces(&mut line, outcome, STEP, p.ink1.hsla(), self.links, p);
            end = end.child(line.element(self.key(format!("{n}-outcome")), STEP, m, self.links, p));
        }
        row = row.child(end);
        let mut body = div().flex().flex_col().gap(k(m, 6.0)).pl(k(m, 12.0))
            .border_l_1().border_color(if n == 0 { p.peri.base.hsla() } else { p.line2.hsla() })
            .child(row);
        if !rail.also.is_empty() {
            let mut line = Line::new();
            pieces(&mut line, &rail.also, RIDER, p.ink1.hsla(), self.links, p);
            body = body.child(div().min_w_0().child(line.element(self.key(format!("{n}-also")), FOOT, m, self.links, p)));
        }
        body.into_any_element()
    }
}

impl RenderOnce for RecipeSection {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let ctx = Ctx { id: &self.id, measure: &m, links: &self.links, palette };
        let mut root = div().id(self.id.clone()).flex().flex_col().gap(k(&m, 10.0)).children(self.title.then(|| section_title(self.view.heading, &m, palette)));
        if let Some(sentence) = &self.view.sentence {
            root = root.child(div().set(SENTENCE, &m).text_color(palette.ink1.hsla()).child(sentence.clone()));
        }
        for (n, rail) in self.view.rails.iter().enumerate() {
            // The prototype dims the other routes to 62 %; that takes their
            // words under 4.5:1, so they are quieter by ink instead.
            root = root.child(div().id(ctx.key(format!("route-{n}"))).flex().flex_col().child(ctx.rail(n, rail, self.view.outcome.as_deref())));
        }
        if self.foot {
            if let Some(foot) = &self.view.foot {
                let label = div().set(FOOT, &m).text_color(palette.ink1.hsla()).child(foot.clone());
                root = root.child(crate::probe::text(
                    ctx.key("foot".to_owned()),
                    foot.clone(),
                    m.role(FOOT),
                    1.0,
                    crate::probe::TextOverflow::Wrap,
                    label,
                ));
            }
        }
        root
    }
}

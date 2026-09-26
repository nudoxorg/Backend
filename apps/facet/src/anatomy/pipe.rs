//! The pipe: a callable's inputs stacked on the left, an arrow, the output
//! on the right with "or fails with E" as its own exit below it; generic
//! bounds follow as sentences (`T is any Deserialize`), then qualifier words.
//! In a narrow room the inputs sit above the output and the arrow goes.

use super::text::{Line, Links, TypeInk};
use super::{k, roles, stacked};
use crate::measure::{Measure, Set};
use crate::semantics::model::Pipe;
use crate::semantics::types::Piece;
use crate::theme::ActiveFacet;
use gpui::{
    InteractiveElement,
    App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, canvas, div, point, px,
};
use std::sync::Arc;

/// A pipe. Build with [`pipe`].
#[derive(IntoElement)]
pub struct PipeView {
    id: ElementId,
    pipe: Pipe,
    measure: Measure,
    links: Links,
}

/// The pipe for `pipe` at `measure`.
#[must_use]
pub fn pipe(id: impl Into<ElementId>, pipe: Pipe, measure: &Measure, links: &Links) -> PipeView {
    PipeView { id: id.into(), pipe, measure: *measure, links: links.clone() }
}

fn arrow(measure: &Measure, color: gpui::Hsla, head: gpui::Hsla) -> impl IntoElement {
    let s = measure.scale();
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let y = bounds.center().y;
            let right = bounds.right() - px(1.0 * s);
            let mut shaft = gpui::PathBuilder::stroke(px(1.0));
            shaft.move_to(point(bounds.left(), y));
            shaft.line_to(point(right - px(1.0), y));
            if let Ok(path) = shaft.build() {
                window.paint_path(path, color);
            }
            let r = px(4.5 * s);
            let mut chevron = gpui::PathBuilder::stroke(px(1.0));
            chevron.move_to(point(right - r, y - r));
            chevron.line_to(point(right, y));
            chevron.line_to(point(right - r, y + r));
            if let Ok(path) = chevron.build() {
                window.paint_path(path, head);
            }
        },
    )
    .flex_none()
    .w(k(measure, 64.0))
    .h(px(12.0 * s))
}

impl RenderOnce for PipeView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.facet().palette();
        let m = self.measure;
        let xray = m.reveal().xray;
        let narrow = stacked(&m);
        let id = |part: String| ElementId::NamedChild(Arc::new(self.id.clone()), SharedString::from(part));
        let ink = TypeInk::new(roles::TYPE, palette);
        // Inputs: the receiver's words, then each parameter's name and type.
        let mut pins = div().flex().flex_col().gap(k(&m, 8.0)).min_w_0();
        if self.pipe.inputs.is_empty() {
            pins = pins.child(div().set(roles::INPUT, &m).text_color(palette.ink3.hsla()).child("takes nothing"));
        }
        for (n, input) in self.pipe.inputs.iter().enumerate() {
            let mut row = div()
                .flex()
                .items_baseline()
                .gap(k(&m, 14.0))
                .child(div().flex_none().set(roles::INPUT, &m).text_color(palette.ink3.hsla()).child(input.name.clone()));
            if let Some(ty) = &input.ty {
                let mut line = Line::new();
                line.spelled(ty, &ink, &self.links, xray);
                row = row.child(div().min_w_0().child(line.element(id(format!("in-{n}")), roles::TYPE, &m, &self.links, palette)));
            }
            pins = pins.child(row);
        }
        // The output and its failure exit.
        let mut out_ink = TypeInk::new(roles::OUTPUT, palette);
        out_ink.link = palette.ink0.hsla();
        out_ink.prim = palette.ink0.hsla();
        let mut output = Line::new();
        match &self.pipe.output {
            Some(ty) => output.spelled(ty, &out_ink, &self.links, xray),
            None => {
                output.push("nothing", roles::words(roles::OUTPUT), palette.ink3.hsla());
            }
        }
        let mut outs = div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(output.element(id("out".into()), roles::OUTPUT, &m, &self.links, palette));
        if let Some(fails) = &self.pipe.fails {
            let mut fail_ink = TypeInk::new(roles::TYPE, palette);
            fail_ink.link = palette.ink1.hsla();
            let mut line = Line::new();
            line.push("or fails with ", roles::words(roles::TYPE), palette.ink3.hsla());
            match fails {
                Some(err) => line.spelled(err, &fail_ink, &self.links, xray),
                None => {
                    line.push("an error", roles::words(roles::TYPE), palette.ink3.hsla());
                }
            }
            outs = outs.child(div().mt(k(&m, 8.0)).child(line.element(id("fails".into()), roles::TYPE, &m, &self.links, palette)));
        }
        let body = if narrow {
            div()
                .flex()
                .flex_col()
                .gap(k(&m, 10.0))
                .child(pins.pb(k(&m, 10.0)).border_b_1().border_color(palette.line3.hsla()))
                .child(outs)
        } else {
            div()
                .flex()
                .items_center()
                .gap(k(&m, 10.0))
                .child(pins.py(k(&m, 6.0)).pr(k(&m, 18.0)).border_r_1().border_color(palette.line3.hsla()))
                .child(arrow(&m, palette.line3.hsla(), palette.ink3.hsla()))
                .child(outs.flex_1())
        };
        let mut root = div().id(self.id.clone()).flex().flex_col().gap(k(&m, 12.0)).child(body);
        if !self.pipe.wheres.is_empty() {
            let mut wheres = div().flex().flex_col().gap(k(&m, 4.0)).pt(px(2.0 * m.scale()));
            for (n, w) in self.pipe.wheres.iter().enumerate() {
                let mut line = Line::new();
                for piece in &w.sentence {
                    match piece {
                        Piece::Word(t) => {
                            line.push(t, roles::words(roles::TYPE), palette.ink3.hsla());
                        }
                        Piece::Space => {
                            line.push(" ", roles::TYPE, palette.ink3.hsla());
                        }
                        _ => {
                            let spelled = crate::semantics::types::Spelled { pieces: vec![piece.clone()], source: SharedString::default() };
                            line.spelled(&spelled, &ink, &self.links, false);
                        }
                    }
                }
                if xray && !w.source.is_empty() {
                    line.push("  ", roles::TYPE, palette.ink4.hsla());
                    line.push(&format!("{}: {}", w.name, w.source), roles::TYPE, palette.ink4.hsla());
                }
                wheres = wheres.child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(k(&m, 10.0))
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(18.0 * m.scale()))
                                .set(roles::italic(roles::INPUT), &m)
                                .text_color(palette.ink1.hsla())
                                .child(w.name.clone()),
                        )
                        .child(div().min_w_0().child(line.element(id(format!("where-{n}")), roles::TYPE, &m, &self.links, palette))),
                );
            }
            root = root.child(wheres);
        }
        if !self.pipe.flags.is_empty() {
            root = root.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(k(&m, 14.0))
                    .set(roles::QUIET, &m)
                    .text_color(palette.ink3.hsla())
                    .children(self.pipe.flags.iter().map(|f| div().child(*f))),
            );
        }
        root
    }
}

//! One small drawn vocabulary for the page: what a place does with the
//! symbol (twelve verbs), how a call can end, the joints on the rail, the
//! capability marks. Each is a few strokes in a 14-unit box, painted in the
//! palette's voices by one canvas, so a mark is the same shape and colour
//! wherever it appears.

use super::view::{CapMark, Change, Effect, Verb};
use crate::tokens::Palette;
use gpui::{AnyElement, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled, canvas, point, px};

/// One stroke or fill, in viewbox units.
#[derive(Clone, Debug)]
enum Prim {
    /// An open polyline.
    Line(Vec<(f32, f32)>, Hsla, f32),
    /// A closed outline.
    Poly(Vec<(f32, f32)>, Hsla, f32),
    /// A filled polygon.
    Fill(Vec<(f32, f32)>, Hsla),
    /// A dashed closed outline.
    Dashed(Vec<(f32, f32)>, Hsla, f32, [f32; 2]),
}

fn circle(cx: f32, cy: f32, r: f32) -> Vec<(f32, f32)> {
    (0..32)
        .map(|k| {
            let a = k as f32 / 32.0 * std::f32::consts::TAU;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

fn rect(x: f32, y: f32, w: f32, h: f32) -> Vec<(f32, f32)> {
    vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
}

fn diamond(cx: f32, cy: f32, r: f32) -> Vec<(f32, f32)> {
    vec![(cx, cy - r), (cx + r, cy), (cx, cy + r), (cx - r, cy)]
}

/// An arc of a circle as a polyline: from `from` to `to` radians.
fn arc(cx: f32, cy: f32, r: f32, from: f32, to: f32) -> Vec<(f32, f32)> {
    let steps = 18;
    (0..=steps)
        .map(|k| {
            let a = from + (to - from) * k as f32 / steps as f32;
            (cx + r * a.cos(), cy + r * a.sin())
        })
        .collect()
}

/// The colours a mark draws with.
#[derive(Clone, Copy)]
struct Hue {
    ink1: Hsla,
    ink2: Hsla,
    ink3: Hsla,
    bg: Hsla,
    deep: Hsla,
    make: Hsla,
    con: Hsla,
    peri: Hsla,
    peri_hi: Hsla,
    coral: Hsla,
    amber: Hsla,
    slate: Hsla,
}

fn hue(p: &Palette) -> Hue {
    Hue {
        ink1: p.ink1.hsla(),
        ink2: p.ink2.hsla(),
        ink3: p.ink3.hsla(),
        bg: p.g1.hsla(),
        deep: p.g0.hsla(),
        make: p.f_type.hue.hsla(),
        con: p.f_con.hue.hsla(),
        peri: p.peri.base.hsla(),
        peri_hi: p.peri_hi.hsla(),
        coral: p.coral.base.hsla(),
        amber: p.amber.base.hsla(),
        slate: p.f_ns.hue.hsla(),
    }
}

/// Every mark the page draws.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum G {
    /// What a place does with the symbol.
    Verb(Verb),
    /// The rail: a required input.
    In,
    /// The rail: an optional input.
    Opt,
    /// The rail: any number of inputs.
    Rest,
    /// The rail: the receiver, by what the call does to it.
    Recv(Option<Effect>),
    /// The rail's end: it gives.
    Out,
    /// The rail's end: it gives each of many.
    Many,
    /// The rail's end: it fails.
    Fail,
    /// A kind of failure that cannot happen here: the failure block, drawn
    /// in ink.
    Impossible,
    /// The rail's end: it may give nothing.
    None,
    /// The rail: it answers later.
    Later,
    /// A fork's case.
    Case,
    /// A bracket's field.
    Field,
    /// Holds more of itself.
    Loop,
    /// Opens the source.
    Open,
    /// A disclosure caret, closed.
    Caret,
    /// A capability.
    Cap(CapMark),
    /// A method group's verb: makes one.
    Makes,
    /// A method group's verb: reads it.
    Reads,
    /// A method group's verb: changes it.
    Changes,
    /// A method group's verb: uses it up.
    UsesUp,
}

fn glyph(g: G, h: Hue) -> (f32, f32, Vec<Prim>) {
    use Prim::{Dashed, Fill, Line, Poly};
    let w = 1.5;
    match g {
        G::Verb(Verb::Makes) | G::Makes => (14.0, 14.0, vec![Poly(rect(1.5, 1.5, 11.0, 11.0), h.make, w), Line(vec![(7.0, 4.0), (7.0, 10.0)], h.make, w), Line(vec![(4.0, 7.0), (10.0, 7.0)], h.make, w)]),
        G::Verb(Verb::Reads) | G::Reads => (14.0, 14.0, vec![Poly(circle(7.0, 7.0, 4.6), h.ink2, w)]),
        G::Verb(Verb::Changes) | G::Changes => (14.0, 14.0, vec![Poly(circle(7.0, 7.0, 4.6), h.amber, w), Fill(circle(7.0, 7.0, 1.8), h.amber)]),
        G::Verb(Verb::UsesUp) | G::UsesUp => (14.0, 14.0, vec![Fill(circle(7.0, 7.0, 5.0), h.peri), Line(vec![(4.5, 7.0), (9.5, 7.0)], h.deep, w), Line(vec![(7.5, 5.0), (9.5, 7.0), (7.5, 9.0)], h.deep, w)]),
        G::Verb(Verb::Holds) => (14.0, 14.0, vec![Line(vec![(4.5, 2.0), (2.5, 2.0), (2.5, 12.0), (4.5, 12.0)], h.ink2, w), Line(vec![(9.5, 2.0), (11.5, 2.0), (11.5, 12.0), (9.5, 12.0)], h.ink2, w)]),
        G::Verb(Verb::Matches) => (14.0, 14.0, vec![Line(vec![(2.0, 7.0), (5.0, 7.0)], h.make, w), Line(vec![(5.0, 7.0), (9.0, 3.0), (12.0, 3.0)], h.make, w), Line(vec![(5.0, 7.0), (9.0, 11.0), (12.0, 11.0)], h.make, w)]),
        G::Verb(Verb::Names) => (14.0, 14.0, vec![Fill(circle(7.0, 7.0, 1.8), h.ink3)]),
        G::Verb(Verb::Imports) => (14.0, 14.0, vec![Line(vec![(7.0, 2.0), (7.0, 9.0)], h.ink3, w), Line(vec![(4.0, 6.0), (7.0, 9.0), (10.0, 6.0)], h.ink3, w), Line(vec![(3.0, 12.0), (11.0, 12.0)], h.ink3, w)]),
        G::Verb(Verb::Calls) => (14.0, 14.0, vec![Line(vec![(1.5, 7.0), (8.5, 7.0)], h.peri, w), Line(vec![(6.0, 4.5), (8.5, 7.0), (6.0, 9.5)], h.peri, w), Poly(rect(9.5, 3.5, 3.0, 7.0), h.peri, w)]),
        G::Verb(Verb::Derives) => (14.0, 14.0, vec![Line(vec![(3.0, 11.0), (11.0, 3.0)], h.make, w), Line(vec![(7.0, 2.5), (7.0, 5.5)], h.make, w), Line(vec![(5.5, 4.0), (8.5, 4.0)], h.make, w), Line(vec![(10.0, 8.0), (10.0, 11.0)], h.make, w), Line(vec![(8.5, 9.5), (11.5, 9.5)], h.make, w)]),
        G::Verb(Verb::Implements) => (14.0, 14.0, vec![Poly(rect(2.5, 2.5, 9.0, 9.0), h.con, w), Line(vec![(5.0, 2.5), (5.0, 6.0), (9.0, 6.0), (9.0, 2.5)], h.con, w)]),
        G::Verb(Verb::AsksFor) => (14.0, 14.0, vec![Poly(circle(5.0, 7.0, 2.8), h.con, w), Line(vec![(7.8, 7.0), (12.5, 7.0)], h.con, w), Line(vec![(11.0, 7.0), (11.0, 9.0)], h.con, w)]),
        G::In => (12.0, 12.0, vec![Fill(circle(6.0, 6.0, 4.0), h.ink1)]),
        G::Opt => (12.0, 12.0, vec![Fill(circle(6.0, 6.0, 3.8), h.bg), Poly(circle(6.0, 6.0, 3.8), h.ink2, 1.5)]),
        G::Rest => (14.0, 12.0, vec![Fill(circle(4.5, 6.0, 3.0), h.ink1), Fill(circle(10.0, 6.0, 3.0), h.bg), Poly(circle(10.0, 6.0, 3.0), h.ink1, 1.3)]),
        G::Recv(effect) => {
            let fill = match effect {
                Some(Effect::Changes) => h.amber,
                Some(Effect::UsesUp) => h.peri,
                _ => h.ink2,
            };
            (12.0, 12.0, vec![Fill(rect(1.5, 1.5, 9.0, 9.0), fill)])
        }
        G::Out => (14.0, 14.0, vec![Fill(vec![(2.0, 3.5), (12.0, 7.0), (2.0, 10.5)], h.peri_hi)]),
        G::Many => (16.0, 14.0, vec![Fill(vec![(1.0, 3.5), (8.0, 7.0), (1.0, 10.5)], h.peri_hi), Fill(vec![(7.0, 3.5), (14.0, 7.0), (7.0, 10.5)], h.peri_hi)]),
        G::Fail => (14.0, 14.0, vec![Fill(rect(1.0, 1.0, 12.0, 12.0), h.coral), Line(vec![(4.4, 4.4), (9.6, 9.6)], h.deep, 1.9), Line(vec![(9.6, 4.4), (4.4, 9.6)], h.deep, 1.9)]),
        G::Impossible => (14.0, 14.0, vec![Poly(rect(1.0, 1.0, 12.0, 12.0), h.ink3, 1.4), Line(vec![(4.4, 4.4), (9.6, 9.6)], h.ink3, 1.5), Line(vec![(9.6, 4.4), (4.4, 9.6)], h.ink3, 1.5)]),
        G::None => (14.0, 14.0, vec![Fill(circle(7.0, 7.0, 5.2), h.bg), Dashed(circle(7.0, 7.0, 5.2), h.slate, 1.8, [2.6, 1.9])]),
        G::Later => (16.0, 16.0, vec![Fill(circle(8.0, 8.0, 6.2), h.bg), Poly(circle(8.0, 8.0, 6.2), h.peri, 1.5), Line(vec![(8.0, 4.6), (8.0, 8.0), (10.3, 9.5)], h.peri, 1.5)]),
        G::Case => (12.0, 12.0, vec![Fill(diamond(6.0, 6.0, 4.5), h.bg), Poly(diamond(6.0, 6.0, 4.5), h.make, 1.5)]),
        G::Field => (10.0, 10.0, vec![Fill(rect(1.0, 1.0, 8.0, 8.0), h.ink2)]),
        G::Loop => {
            let mut points = arc(7.0, 7.0, 4.0, -0.35, 4.4);
            points.dedup();
            (14.0, 14.0, vec![Line(points, h.make, w), Line(vec![(11.0, 2.5), (11.0, 4.9), (8.6, 4.9)], h.make, w)])
        }
        G::Open => (12.0, 12.0, vec![Line(vec![(5.0, 2.0), (2.0, 2.0), (2.0, 10.0), (10.0, 10.0), (10.0, 7.0)], h.peri, 1.4), Line(vec![(7.0, 2.0), (10.0, 2.0), (10.0, 5.0)], h.peri, 1.4), Line(vec![(10.0, 2.0), (5.5, 6.5)], h.peri, 1.4)]),
        G::Caret => (10.0, 10.0, vec![Fill(vec![(3.0, 1.5), (8.0, 5.0), (3.0, 8.5)], h.peri)]),
        G::Cap(mark) => cap(mark, h),
    }
}

fn cap(mark: CapMark, h: Hue) -> (f32, f32, Vec<Prim>) {
    use Prim::{Line, Poly};
    let (c, w) = (h.ink2, 1.4);
    let prims = match mark {
        CapMark::Copy => vec![Poly(rect(1.5, 3.5, 7.0, 7.0), c, w), Poly(rect(5.0, 1.5, 7.0, 7.0), c, w)],
        CapMark::Eq => vec![Line(vec![(2.5, 5.0), (11.5, 5.0)], c, w), Line(vec![(2.5, 9.0), (11.5, 9.0)], c, w)],
        CapMark::Hash => vec![Line(vec![(5.0, 1.5), (4.0, 12.5)], c, w), Line(vec![(10.0, 1.5), (9.0, 12.5)], c, w), Line(vec![(2.0, 5.0), (12.5, 5.0)], c, w), Line(vec![(1.5, 9.0), (12.0, 9.0)], c, w)],
        CapMark::Debug => vec![Line(vec![(5.0, 2.0), (3.6, 3.0), (3.6, 6.2), (2.0, 7.0), (3.6, 7.8), (3.6, 11.0), (5.0, 12.0)], c, w), Line(vec![(9.0, 2.0), (10.4, 3.0), (10.4, 6.2), (12.0, 7.0), (10.4, 7.8), (10.4, 11.0), (9.0, 12.0)], c, w)],
        CapMark::Print => vec![Line(vec![(2.0, 4.0), (12.0, 4.0)], c, w), Line(vec![(2.0, 7.0), (12.0, 7.0)], c, w), Line(vec![(2.0, 10.0), (8.0, 10.0)], c, w)],
        CapMark::Default => vec![Poly(circle(7.0, 7.0, 4.5), c, w), Prim::Fill(circle(7.0, 7.0, 1.1), c)],
        CapMark::Ser => vec![Poly(rect(1.5, 3.5, 6.0, 7.0), c, w), Line(vec![(5.0, 7.0), (12.0, 7.0)], c, w), Line(vec![(10.0, 5.0), (12.0, 7.0), (10.0, 9.0)], c, w)],
        CapMark::De => vec![Poly(rect(6.5, 3.5, 6.0, 7.0), c, w), Line(vec![(1.5, 7.0), (8.5, 7.0)], c, w), Line(vec![(6.5, 5.0), (8.5, 7.0), (6.5, 9.0)], c, w)],
        CapMark::FromStr => vec![Line(vec![(2.0, 3.5), (7.0, 3.5)], c, w), Line(vec![(2.0, 7.0), (7.0, 7.0)], c, w), Line(vec![(2.0, 10.5), (5.0, 10.5)], c, w), Line(vec![(9.0, 7.0), (12.5, 7.0)], c, w), Line(vec![(11.0, 5.0), (13.0, 7.0), (11.0, 9.0)], c, w)],
        CapMark::Index => vec![Line(vec![(4.0, 2.0), (2.0, 2.0), (2.0, 12.0), (4.0, 12.0)], c, w), Line(vec![(10.0, 2.0), (12.0, 2.0), (12.0, 12.0), (10.0, 12.0)], c, w), Line(vec![(7.0, 4.5), (7.0, 9.5)], c, w)],
        CapMark::Reader => vec![Poly(rect(3.0, 2.5, 8.0, 9.0), c, w), Line(vec![(5.0, 5.5), (9.0, 5.5)], c, w), Line(vec![(5.0, 8.5), (9.0, 8.5)], c, w)],
        CapMark::Error => vec![Poly(rect(2.0, 2.0, 10.0, 10.0), c, w), Line(vec![(5.0, 5.0), (9.0, 9.0)], c, w), Line(vec![(9.0, 5.0), (5.0, 9.0)], c, w)],
        CapMark::Order => vec![Line(vec![(10.0, 2.5), (3.5, 7.0), (10.0, 11.5)], c, w)],
        CapMark::Iter => vec![Line(vec![(2.0, 4.0), (9.0, 4.0)], c, w), Line(vec![(2.0, 7.0), (9.0, 7.0)], c, w), Line(vec![(2.0, 10.0), (9.0, 10.0)], c, w), Line(vec![(10.5, 5.0), (12.5, 7.0), (10.5, 9.0)], c, w)],
        CapMark::Other => vec![Poly(rect(3.0, 2.5, 8.0, 9.0), h.con, w), Line(vec![(5.5, 2.5), (5.5, 5.5), (8.5, 5.5), (8.5, 2.5)], h.con, w)],
    };
    (14.0, 14.0, prims)
}

fn points(pts: &[(f32, f32)], origin: Point<Pixels>, k: f32) -> Vec<Point<Pixels>> {
    pts.iter().map(|&(x, y)| point(origin.x + px(x * k), origin.y + px(y * k))).collect()
}

/// A mark `size` px on its long side.
#[must_use]
pub fn mark(g: G, palette: &Palette, size: f32) -> AnyElement {
    let (vw, vh, prims) = glyph(g, hue(palette));
    let k = size / vw.max(vh);
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let origin = bounds.origin;
            for prim in &prims {
                match prim {
                    Prim::Fill(pts, color) => {
                        let mut path = PathBuilder::fill();
                        path.add_polygon(&points(pts, origin, k), true);
                        if let Ok(path) = path.build() {
                            window.paint_path(path, *color);
                        }
                    }
                    Prim::Line(pts, color, width) => {
                        let mut path = PathBuilder::stroke(px(width * k));
                        let pts = points(pts, origin, k);
                        if let Some((first, rest)) = pts.split_first() {
                            path.move_to(*first);
                            for p in rest {
                                path.line_to(*p);
                            }
                        }
                        if let Ok(path) = path.build() {
                            window.paint_path(path, *color);
                        }
                    }
                    Prim::Poly(pts, color, width) => {
                        let mut path = PathBuilder::stroke(px(width * k));
                        path.add_polygon(&points(pts, origin, k), true);
                        if let Ok(path) = path.build() {
                            window.paint_path(path, *color);
                        }
                    }
                    Prim::Dashed(pts, color, width, dash) => {
                        let mut path = PathBuilder::stroke(px(width * k)).dash_array(&[px(dash[0] * k), px(dash[1] * k)]);
                        path.add_polygon(&points(pts, origin, k), true);
                        if let Ok(path) = path.build() {
                            window.paint_path(path, *color);
                        }
                    }
                }
            }
        },
    )
    .w(px(size * vw / vw.max(vh)))
    .h(px(size * vh / vw.max(vh)))
    .flex_none()
    .into_any_element()
}

/// The glyph for an option's change of outcome (`fails → nothing`).
#[must_use]
pub fn change_marks(change: Change) -> (G, G) {
    match change {
        Change::FailsToNone => (G::Fail, G::None),
        Change::OneToMany => (G::Out, G::Many),
        Change::NoneToFails => (G::None, G::Fail),
    }
}

/// The verb a method group's heading wears.
#[must_use]
pub const fn group_mark(verb: super::view::Do) -> G {
    match verb {
        super::view::Do::Makes => G::Makes,
        super::view::Do::Reads => G::Reads,
        super::view::Do::Changes => G::Changes,
        super::view::Do::UsesUp => G::UsesUp,
    }
}

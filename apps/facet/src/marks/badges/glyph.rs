//! The badge glyphs: twelve-unit drawings, cut not drawn (square caps,
//! mitred corners, no round pucks: a circle is a twelve-sided polygon).
//! Every badge on a symbol card and every chip in the heads-up stack wears
//! one; the padlock of a locked feature is the same family, with its
//! shackle a parameter so it can close.
//!
//! Geometry is data ([`Glyph::prims`]); one painter turns it into filled
//! quads and triangles in a single path, so a glyph is one draw and the
//! colour is whatever the caller says (opaque: overlapping strokes must not
//! double-blend).

use crate::paint::geom::{Fill, Poly, Pt, pt};
use gpui::{Bounds, Hsla, IntoElement, Pixels, Styled, Window, canvas, px};

/// Every glyph a badge or a chip can wear.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Glyph {
    /// A warning triangle.
    Unsafe,
    /// Two arrows passing.
    Async,
    /// A disc with an exclamation.
    Fail,
    /// A disc, half filled.
    Maybe,
    /// A chain link: it reads what it borrows.
    Reads,
    /// A pencil: it changes what it is given.
    Changes,
    /// An arrow into a bracket: it takes the value.
    Consumes,
    /// Angle brackets.
    Generic,
    /// A padlock.
    Const,
    /// A bang.
    Macro,
    /// Three lines and an arrow.
    Iter,
    /// A square with a cross.
    Error,
    /// A wrench.
    Build,
    /// A four-point star.
    MacroPkg,
    /// A shield.
    Shield,
    /// A globe.
    Net,
    /// A folder.
    Files,
    /// A terminal.
    Process,
    /// A key.
    Env,
    /// A plug.
    Ffi,
    /// A person.
    You,
    /// A page with nothing on it.
    Undoc,
    /// A caret: belongs to, wraps, is.
    Owner,
    /// A plus: makes one.
    Makes,
    /// A flag: a marker.
    Marker,
    /// An arrow in: arguments.
    Takes,
    /// A dashed box: nobody fills it yet.
    Abstract,
    /// A box over a box: it replaces.
    Override,
}

type P = (f32, f32);

/// One drawing primitive on the twelve-unit grid.
#[derive(Clone, Copy)]
enum Prim {
    /// An open polyline.
    Line(&'static [P]),
    /// A closed outline.
    Loop(&'static [P]),
    /// A filled convex polygon.
    Solid(&'static [P]),
    /// A circle outline: centre, radius.
    Ring(P, f32),
    /// An arc: centre, radius, from and to in degrees (0 = right, clockwise
    /// on screen).
    Arc(P, f32, f32, f32),
    /// A filled half disc: centre, radius, facing degrees.
    Half(P, f32, f32),
    /// A filled square dot: centre, half side.
    Dot(P, f32),
}

impl Glyph {
    fn prims(self) -> &'static [Prim] {
        use Prim::{Arc, Dot, Half, Line, Loop, Ring, Solid};
        match self {
            Self::Unsafe => &[Loop(&[(6.0, 1.4), (11.0, 10.6), (1.0, 10.6)]), Line(&[(6.0, 4.8), (6.0, 7.4)]), Dot((6.0, 9.1), 0.6)],
            Self::Async => &[
                Line(&[(1.5, 4.2), (8.0, 4.2)]),
                Line(&[(6.2, 2.4), (8.0, 4.2), (6.2, 6.0)]),
                Line(&[(10.5, 7.8), (4.0, 7.8)]),
                Line(&[(5.8, 6.0), (4.0, 7.8), (5.8, 9.6)]),
            ],
            Self::Fail => &[Ring((6.0, 6.0), 4.6), Line(&[(6.0, 3.5), (6.0, 6.4)]), Dot((6.0, 8.3), 0.6)],
            Self::Maybe => &[Ring((6.0, 6.0), 4.6), Half((6.0, 6.0), 4.6, 0.0)],
            Self::Reads => &[
                Loop(&[(1.6, 6.6), (3.6, 4.6), (6.0, 7.0), (4.0, 9.0)]),
                Loop(&[(6.0, 5.0), (8.4, 2.6), (10.4, 4.6), (8.0, 7.0)]),
                Line(&[(4.6, 7.4), (7.4, 4.6)]),
            ],
            Self::Changes => &[Loop(&[(2.0, 10.0), (2.6, 7.6), (8.4, 1.8), (10.2, 3.6), (4.4, 9.4)]), Line(&[(7.0, 3.2), (8.8, 5.0)])],
            Self::Consumes => &[Line(&[(1.2, 6.0), (7.0, 6.0)]), Line(&[(4.6, 3.6), (7.0, 6.0), (4.6, 8.4)]), Line(&[(8.6, 2.2), (10.6, 2.2), (10.6, 9.8), (8.6, 9.8)])],
            Self::Generic => &[Line(&[(4.6, 2.0), (1.8, 6.0), (4.6, 10.0)]), Line(&[(7.4, 2.0), (10.2, 6.0), (7.4, 10.0)])],
            Self::Const => &[Loop(&[(2.4, 5.2), (9.6, 5.2), (9.6, 10.6), (2.4, 10.6)]), Line(&[(4.0, 5.2), (4.0, 3.8), (5.0, 1.8), (7.0, 1.8), (8.0, 3.8), (8.0, 5.2)])],
            Self::Macro => &[Line(&[(6.0, 1.6), (6.0, 7.2)]), Dot((6.0, 9.6), 0.7)],
            Self::Iter => &[
                Line(&[(1.6, 3.2), (8.2, 3.2)]),
                Line(&[(1.6, 6.0), (8.2, 6.0)]),
                Line(&[(1.6, 8.8), (8.2, 8.8)]),
                Line(&[(9.2, 5.4), (10.6, 6.8), (9.2, 8.2)]),
            ],
            Self::Error => &[Loop(&[(2.0, 2.0), (10.0, 2.0), (10.0, 10.0), (2.0, 10.0)]), Line(&[(4.2, 4.2), (7.8, 7.8)]), Line(&[(7.8, 4.2), (4.2, 7.8)])],
            Self::Build => &[Line(&[(2.0, 10.0), (6.2, 5.8)]), Loop(&[(5.3, 2.4), (9.6, 6.7), (8.2, 8.1), (3.9, 3.8)])],
            Self::MacroPkg => &[Loop(&[(6.0, 1.2), (7.1, 4.9), (10.8, 6.0), (7.1, 7.1), (6.0, 10.8), (4.9, 7.1), (1.2, 6.0), (4.9, 4.9)])],
            Self::Shield => &[Loop(&[(6.0, 1.2), (10.2, 2.7), (10.2, 6.2), (8.6, 9.0), (6.0, 10.8), (3.4, 9.0), (1.8, 6.2), (1.8, 2.7)])],
            Self::Net => &[
                Ring((6.0, 6.0), 4.6),
                Line(&[(1.4, 6.0), (10.6, 6.0)]),
                Arc((6.0, 6.0), 4.6, -55.0, 55.0),
                Arc((6.0, 6.0), 4.6, 125.0, 235.0),
                Line(&[(6.0, 1.4), (6.0, 10.6)]),
            ],
            Self::Files => &[Loop(&[(1.6, 2.8), (4.9, 2.8), (5.9, 4.0), (10.4, 4.0), (10.4, 9.6), (1.6, 9.6)])],
            Self::Process => &[Loop(&[(1.4, 2.2), (10.6, 2.2), (10.6, 9.8), (1.4, 9.8)]), Line(&[(3.3, 4.6), (5.0, 6.0), (3.3, 7.4)]), Line(&[(6.2, 7.6), (8.6, 7.6)])],
            Self::Env => &[Ring((4.2, 6.0), 2.4), Line(&[(6.6, 6.0), (10.6, 6.0)]), Line(&[(9.0, 6.0), (9.0, 8.0)])],
            Self::Ffi => &[
                Line(&[(4.0, 1.4), (4.0, 4.4)]),
                Line(&[(8.0, 1.4), (8.0, 4.4)]),
                Loop(&[(2.6, 4.4), (9.4, 4.4), (9.4, 6.2), (7.8, 8.4), (4.2, 8.4), (2.6, 6.2)]),
                Line(&[(6.0, 8.4), (6.0, 10.8)]),
            ],
            Self::You => &[Ring((6.0, 4.0), 2.0), Line(&[(2.2, 10.6), (2.8, 8.6), (4.4, 7.4), (7.6, 7.4), (9.2, 8.6), (9.8, 10.6)])],
            Self::Undoc => &[
                Line(&[(2.8, 1.6), (7.2, 1.6), (9.2, 3.6), (9.2, 10.4), (2.8, 10.4), (2.8, 1.6)]),
                Line(&[(4.6, 6.0), (7.6, 6.0)]),
                Line(&[(4.6, 8.0), (6.8, 8.0)]),
            ],
            Self::Owner => &[Line(&[(2.2, 9.6), (6.0, 2.4), (9.8, 9.6)])],
            Self::Makes => &[Line(&[(6.0, 2.0), (6.0, 10.0)]), Line(&[(2.0, 6.0), (10.0, 6.0)])],
            Self::Marker => &[Line(&[(3.0, 10.6), (3.0, 1.6)]), Loop(&[(3.0, 2.2), (9.4, 2.2), (7.6, 4.4), (9.4, 6.6), (3.0, 6.6)])],
            Self::Takes => &[Line(&[(1.4, 6.0), (7.4, 6.0)]), Line(&[(5.0, 3.6), (7.4, 6.0), (5.0, 8.4)]), Line(&[(9.6, 2.0), (9.6, 10.0)])],
            Self::Abstract => &[
                Line(&[(2.0, 2.0), (4.2, 2.0)]),
                Line(&[(7.8, 2.0), (10.0, 2.0)]),
                Line(&[(10.0, 2.0), (10.0, 4.2)]),
                Line(&[(10.0, 7.8), (10.0, 10.0)]),
                Line(&[(10.0, 10.0), (7.8, 10.0)]),
                Line(&[(4.2, 10.0), (2.0, 10.0)]),
                Line(&[(2.0, 10.0), (2.0, 7.8)]),
                Line(&[(2.0, 4.2), (2.0, 2.0)]),
                Solid(&[(5.0, 5.0), (7.0, 5.0), (7.0, 7.0), (5.0, 7.0)]),
            ],
            Self::Override => &[Loop(&[(1.8, 1.8), (8.0, 1.8), (8.0, 8.0), (1.8, 8.0)]), Loop(&[(4.4, 4.4), (10.2, 4.4), (10.2, 10.2), (4.4, 10.2)])],
        }
    }

    /// The glyph's name, for tests and captions.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Unsafe => "unsafe",
            Self::Async => "async",
            Self::Fail => "fail",
            Self::Maybe => "maybe",
            Self::Reads => "reads",
            Self::Changes => "changes",
            Self::Consumes => "consumes",
            Self::Generic => "generic",
            Self::Const => "const",
            Self::Macro => "macro",
            Self::Iter => "iter",
            Self::Error => "error",
            Self::Build => "build",
            Self::MacroPkg => "macro-pkg",
            Self::Shield => "shield",
            Self::Net => "net",
            Self::Files => "files",
            Self::Process => "process",
            Self::Env => "env",
            Self::Ffi => "ffi",
            Self::You => "you",
            Self::Undoc => "undoc",
            Self::Owner => "owner",
            Self::Makes => "makes",
            Self::Marker => "marker",
            Self::Takes => "takes",
            Self::Abstract => "abstract",
            Self::Override => "override",
        }
    }

    /// Every glyph, for the gallery.
    pub const ALL: [Self; 28] = [
        Self::Unsafe,
        Self::Async,
        Self::Fail,
        Self::Maybe,
        Self::Reads,
        Self::Changes,
        Self::Consumes,
        Self::Generic,
        Self::Const,
        Self::Macro,
        Self::Iter,
        Self::Error,
        Self::Build,
        Self::MacroPkg,
        Self::Shield,
        Self::Net,
        Self::Files,
        Self::Process,
        Self::Env,
        Self::Ffi,
        Self::You,
        Self::Undoc,
        Self::Owner,
        Self::Makes,
        Self::Marker,
        Self::Takes,
        Self::Abstract,
        Self::Override,
    ];
}

fn arc_points(centre: P, r: f32, from: f32, to: f32) -> Vec<Pt> {
    let steps = (((to - from).abs() / 15.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let t = (from + (to - from) * i as f32 / steps as f32).to_radians();
            pt(centre.0 + r * t.cos(), centre.1 + r * t.sin())
        })
        .collect()
}

fn ring_points(centre: P, r: f32) -> Vec<Pt> {
    (0..12)
        .map(|i| {
            let t = (i as f32 * 30.0).to_radians();
            pt(centre.0 + r * t.cos(), centre.1 + r * t.sin())
        })
        .collect()
}

/// One segment as a quad with square caps, in positive orientation.
fn segment(a: Pt, b: Pt, w: f32) -> Option<Poly> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = dx.hypot(dy);
    if len < 1e-4 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy * w * 0.5, ux * w * 0.5);
    let (ex, ey) = (ux * w * 0.5, uy * w * 0.5);
    let (a, b) = (pt(a.x - ex, a.y - ey), pt(b.x + ex, b.y + ey));
    let quad = [pt(a.x + nx, a.y + ny), pt(b.x + nx, b.y + ny), pt(b.x - nx, b.y - ny), pt(a.x - nx, a.y - ny)];
    // Positive (clockwise on screen) orientation.
    let area: f32 = (0..4)
        .map(|i| {
            let (p, q) = (quad[i], quad[(i + 1) % 4]);
            p.x * q.y - q.x * p.y
        })
        .sum();
    Some(if area >= 0.0 { Poly::new(quad) } else { Poly::new([quad[3], quad[2], quad[1], quad[0]]) })
}

fn positive(points: &[Pt]) -> Poly {
    let area: f32 = (0..points.len())
        .map(|i| {
            let (p, q) = (points[i], points[(i + 1) % points.len()]);
            p.x * q.y - q.x * p.y
        })
        .sum();
    if area >= 0.0 {
        Poly::new(points.iter().copied())
    } else {
        Poly::new(points.iter().rev().copied())
    }
}

/// Paints `glyph` into `bounds` (its box is square: the shorter side) in
/// opaque `color`.
pub fn paint(window: &mut Window, bounds: Bounds<Pixels>, glyph: Glyph, color: Hsla) {
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    let k = side / 12.0;
    let ox = f32::from(bounds.origin.x) + (f32::from(bounds.size.width) - side) * 0.5;
    let oy = f32::from(bounds.origin.y) + (f32::from(bounds.size.height) - side) * 0.5;
    let w = (1.25 * k).max(1.0);
    let at = |p: P| pt(ox + p.0 * k, oy + p.1 * k);
    let mut fill = Fill::new();
    let mut stroke = |points: &[Pt], closed: bool, fill: &mut Fill| {
        for pair in points.windows(2) {
            if let Some(quad) = segment(pair[0], pair[1], w) {
                fill.poly(&quad);
            }
        }
        if closed && points.len() > 2 {
            if let Some(quad) = segment(points[points.len() - 1], points[0], w) {
                fill.poly(&quad);
            }
        }
    };
    for prim in glyph.prims() {
        match *prim {
            Prim::Line(points) => stroke(&points.iter().map(|p| at(*p)).collect::<Vec<_>>(), false, &mut fill),
            Prim::Loop(points) => stroke(&points.iter().map(|p| at(*p)).collect::<Vec<_>>(), true, &mut fill),
            Prim::Solid(points) => fill.poly(&positive(&points.iter().map(|p| at(*p)).collect::<Vec<_>>())),
            Prim::Ring(c, r) => {
                let ring: Vec<Pt> = ring_points(c, r).into_iter().map(|p| at((p.x, p.y))).collect();
                stroke(&ring, true, &mut fill);
            }
            Prim::Arc(c, r, from, to) => {
                let arc: Vec<Pt> = arc_points(c, r, from, to).into_iter().map(|p| at((p.x, p.y))).collect();
                stroke(&arc, false, &mut fill);
            }
            Prim::Half(c, r, facing) => {
                let half: Vec<Pt> = arc_points(c, r, facing - 90.0, facing + 90.0).into_iter().map(|p| at((p.x, p.y))).collect();
                fill.poly(&positive(&half));
            }
            Prim::Dot(c, h) => {
                let dot = Poly::rect(ox + (c.0 - h) * k, oy + (c.1 - h) * k, h * 2.0 * k, h * 2.0 * k);
                fill.poly(&dot);
            }
        }
    }
    fill.paint(window, color);
}

/// A glyph as an element, `size` px square, in `color`.
pub fn glyph(glyph: Glyph, size: f32, color: impl Into<Hsla>) -> impl IntoElement {
    let color = color.into();
    canvas(|_, _, _| {}, move |bounds, (), window, _| paint(window, bounds, glyph, color))
        .flex_none()
        .size(px(size))
}

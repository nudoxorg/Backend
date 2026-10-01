//! The marks' glyphs, painted: each registry's stone, each license family's
//! ring, and the dependency diamond. Geometry is authored in a 24-unit box
//! (the boards' SVG viewBox) and scaled to the size asked for.
//!
//! - **Stones.** Each registry is a different cut: crates.io an octagon,
//!   npm a square, PyPI two overlapping diamonds, Go a parallelogram, Maven
//!   a kite, NuGet a hexagon, and C/C++ an uncut rough stone (no registry).
//!   Facets take the house light from the top-left; [`stone`] can turn the
//!   geometry by part of one symmetry step and every facet takes the light
//!   of where it now faces, so a stone that turns a whole step ends looking
//!   exactly as it started (MOTION.md's *Turn*).
//! - **Rings.** An octagon whose edges say the family: permissive is open
//!   (one edge missing), weak copyleft is open with a core, strong copyleft
//!   is closed with a core, public domain is only its corners, and unknown
//!   terms are dashed.

use super::eco::Eco;
use super::spdx::Family;
use crate::paint::geom::{Fill, Poly, Pt, pt};
use gpui::{Bounds, Hsla, Pixels, Window};

type P = (f32, f32);

struct StoneCut {
    outer: &'static [P],
    /// The table's scale about `centre` (0: an apex cut, no table).
    k: f32,
    centre: P,
    apex: Option<P>,
    /// Drawn behind, dimmer (PyPI's second stone).
    back: bool,
}

struct Cut {
    stones: &'static [StoneCut],
    /// One symmetry step, degrees (0: the rough stone wobbles instead).
    step: f32,
}

const CRATES: &[StoneCut] = &[StoneCut {
    outer: &[(6.5, 4.0), (17.5, 4.0), (21.5, 8.0), (21.5, 16.0), (17.5, 20.0), (6.5, 20.0), (2.5, 16.0), (2.5, 8.0)],
    k: 0.5,
    centre: (12.0, 12.0),
    apex: None,
    back: false,
}];
const NPM: &[StoneCut] = &[StoneCut {
    outer: &[(3.5, 3.5), (20.5, 3.5), (20.5, 20.5), (3.5, 20.5)],
    k: 0.46,
    centre: (12.0, 12.0),
    apex: None,
    back: false,
}];
const PYPI: &[StoneCut] = &[
    StoneCut {
        outer: &[(14.5, 7.5), (21.5, 14.5), (14.5, 21.5), (7.5, 14.5)],
        k: 0.42,
        centre: (14.5, 14.5),
        apex: None,
        back: true,
    },
    StoneCut {
        outer: &[(9.5, 2.5), (16.5, 9.5), (9.5, 16.5), (2.5, 9.5)],
        k: 0.42,
        centre: (9.5, 9.5),
        apex: None,
        back: false,
    },
];
const GO: &[StoneCut] = &[StoneCut {
    outer: &[(8.0, 5.0), (22.0, 5.0), (16.0, 19.0), (2.0, 19.0)],
    k: 0.45,
    centre: (12.0, 12.0),
    apex: None,
    back: false,
}];
const MAVEN: &[StoneCut] = &[StoneCut {
    outer: &[(12.0, 1.5), (18.5, 12.0), (12.0, 22.5), (5.5, 12.0)],
    k: 0.42,
    centre: (12.0, 12.0),
    apex: None,
    back: false,
}];
const NUGET: &[StoneCut] = &[StoneCut {
    // A regular hexagon, radius 10 about (12, 12.2), a vertex at the top.
    outer: &[(12.0, 2.2), (20.66, 7.2), (20.66, 17.2), (12.0, 22.2), (3.34, 17.2), (3.34, 7.2)],
    k: 0.5,
    centre: (12.0, 12.2),
    apex: None,
    back: false,
}];
const CPP: &[StoneCut] = &[StoneCut {
    outer: &[(5.0, 8.5), (12.5, 2.5), (21.0, 8.0), (18.5, 19.5), (6.5, 20.0)],
    k: 0.0,
    centre: (12.0, 12.0),
    apex: Some((11.0, 11.5)),
    back: false,
}];

fn cut(eco: Eco) -> Cut {
    match eco {
        Eco::Crates => Cut { stones: CRATES, step: 180.0 },
        Eco::Npm => Cut { stones: NPM, step: 90.0 },
        Eco::Pypi => Cut { stones: PYPI, step: 90.0 },
        Eco::Go => Cut { stones: GO, step: 180.0 },
        Eco::Maven => Cut { stones: MAVEN, step: 180.0 },
        Eco::Nuget => Cut { stones: NUGET, step: 60.0 },
        Eco::Cpp => Cut { stones: CPP, step: 0.0 },
    }
}

/// Light from the top-left, a little more from the top.
const LIGHT: P = (-0.6, -0.8);

/// Each facet's light for an outline: the edge facing the light is lit; the
/// rest take light by how much they face it.
fn facet_light(outer: &[P]) -> Vec<f32> {
    let n = outer.len();
    let dots: Vec<f32> = (0..n)
        .map(|i| {
            let (a, b) = (outer[i], outer[(i + 1) % n]);
            let (nx, ny) = (b.1 - a.1, -(b.0 - a.0));
            let l = nx.hypot(ny).max(1e-6);
            (nx * LIGHT.0 + ny * LIGHT.1) / l
        })
        .collect();
    let lit = dots
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i);
    dots.iter()
        .enumerate()
        .map(|(i, d)| if i == lit { 0.95 } else { 0.1 + 0.42 * d.max(0.0).powf(1.4) })
        .collect()
}

fn rotate(p: P, c: P, radians: f32) -> P {
    let (s, co) = radians.sin_cos();
    let (dx, dy) = (p.0 - c.0, p.1 - c.1);
    (c.0 + dx * co - dy * s, c.1 + dx * s + dy * co)
}

fn scale_about(p: P, c: P, k: f32) -> P {
    (c.0 + (p.0 - c.0) * k, c.1 + (p.1 - c.1) * k)
}

/// Where the 24-unit box lands in `bounds`.
fn to_px(bounds: Bounds<Pixels>) -> impl Fn(P) -> Pt {
    let x0 = f32::from(bounds.origin.x);
    let y0 = f32::from(bounds.origin.y);
    let s = f32::from(bounds.size.width.min(bounds.size.height)) / 24.0;
    move |(x, y)| pt(x0 + x * s, y0 + y * s)
}

/// Paints `eco`'s stone in `bounds` in `ink`, turned `turn` of one symmetry
/// step (0 at rest; the copy tick runs it from 1 back to 0). A `local` stone
/// has a mint table (`mine`).
pub(crate) fn stone(eco: Eco, bounds: Bounds<Pixels>, ink: Hsla, table: Option<Hsla>, turn: f32, window: &mut Window) {
    let cut = cut(eco);
    let map = to_px(bounds);
    let scale = f32::from(bounds.size.width) / 24.0;
    // The rough stone has no symmetry: it rocks a little instead.
    let angle = if cut.step > 0.0 { -cut.step * turn } else { -18.0 * turn };
    let radians = angle.to_radians();
    for s in cut.stones {
        let outer: Vec<P> = s.outer.iter().map(|p| rotate(*p, s.centre, radians)).collect();
        let light = facet_light(&outer);
        let n = outer.len();
        let inner: Option<Vec<P>> = (s.k > 0.0).then(|| outer.iter().map(|p| scale_about(*p, s.centre, s.k)).collect());
        let apex = s.apex.map(|a| rotate(a, s.centre, radians));
        let dim = if s.back { 0.6 } else { 1.0 };
        for i in 0..n {
            let (a, b) = (outer[i], outer[(i + 1) % n]);
            let poly = match (&inner, apex) {
                (Some(inner), _) => Poly::new([map(a), map(b), map(inner[(i + 1) % n]), map(inner[i])]),
                (None, Some(apex)) => Poly::new([map(a), map(b), map(apex)]),
                (None, None) => continue,
            };
            let mut fill = Fill::new();
            fill.poly(&poly);
            fill.paint(window, crate::controls::with_alpha(ink, ink.alpha * light[i] * dim));
        }
        if let Some(inner) = &inner {
            let table_poly = Poly::new(inner.iter().map(|p| map(*p)));
            let mut fill = Fill::new();
            fill.poly(&table_poly);
            match table {
                Some(mine) => fill.paint(window, mine),
                None => fill.paint(window, crate::controls::with_alpha(ink, ink.alpha * 0.4 * dim)),
            }
        }
        let outline = Poly::new(outer.iter().map(|p| map(*p)));
        let mut ring = Fill::new();
        for piece in outline.stroke_ring(1.15 * scale) {
            ring.poly(&piece);
        }
        ring.paint(window, crate::controls::with_alpha(ink, ink.alpha * dim));
    }
}

/// The ring's eight corners, turned `radians` about the centre.
fn octagon(radians: f32) -> [P; 8] {
    let mut v = [(0.0, 0.0); 8];
    for (i, slot) in v.iter_mut().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let a = (-112.5 + 45.0 * i as f32).to_radians() + radians;
        *slot = (12.0 + 9.0 * a.cos(), 12.0 + 9.0 * a.sin());
    }
    v
}

/// A straight stroke from `a` to `b`, `w` wide, square caps.
fn segment(a: Pt, b: Pt, w: f32) -> Poly {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l = dx.hypot(dy).max(1e-6);
    let (ux, uy) = (dx / l * w * 0.5, dy / l * w * 0.5);
    let (nx, ny) = (-uy, ux);
    Poly::new([
        pt(a.x - ux + nx, a.y - uy + ny),
        pt(b.x + ux + nx, b.y + uy + ny),
        pt(b.x + ux - nx, b.y + uy - ny),
        pt(a.x - ux - nx, a.y - uy - ny),
    ])
}

/// Paints the ring of `family` in `bounds`, turned `turn` degrees.
pub(crate) fn ring(family: Family, bounds: Bounds<Pixels>, ink: Hsla, turn: f32, window: &mut Window) {
    let map = to_px(bounds);
    let scale = f32::from(bounds.size.width) / 24.0;
    let v = octagon(turn.to_radians());
    let mut edges = Fill::new();
    let mut core = Fill::new();
    let mut core_alpha = 0.3;
    match family {
        Family::Public => {
            for c in v {
                let d = 1.1 * std::f32::consts::SQRT_2;
                edges.poly(&Poly::new([map((c.0, c.1 - d)), map((c.0 + d, c.1)), map((c.0, c.1 + d)), map((c.0 - d, c.1))]));
            }
        }
        Family::Unknown => {
            for i in 0..8 {
                let (a, b) = (v[i], v[(i + 1) % 8]);
                let len = (b.0 - a.0).hypot(b.1 - a.1);
                let (dash, gap) = (1.6, 1.9);
                let mut at = 0.0;
                while at < len {
                    let to = (at + dash).min(len);
                    let p = |t: f32| (a.0 + (b.0 - a.0) * t / len, a.1 + (b.1 - a.1) * t / len);
                    edges.poly(&segment(map(p(at)), map(p(to)), 1.3 * scale));
                    at += dash + gap;
                }
            }
        }
        _ => {
            for i in 0..8 {
                let open = matches!(family, Family::Permissive | Family::Weak) && i == 1;
                if !open {
                    edges.poly(&segment(map(v[i]), map(v[(i + 1) % 8]), 1.55 * scale));
                }
            }
            match family {
                Family::Strong => {
                    core.poly(&Poly::new(v.iter().map(|p| map(scale_about(*p, (12.0, 12.0), 0.52)))));
                }
                Family::Weak => {
                    let r = 2.4 * std::f32::consts::SQRT_2;
                    core.poly(&Poly::new([map((12.0, 12.0 - r)), map((12.0 + r, 12.0)), map((12.0, 12.0 + r)), map((12.0 - r, 12.0))]));
                    core_alpha = 0.9;
                }
                _ => {}
            }
        }
    }
    core.paint(window, crate::controls::with_alpha(ink, ink.alpha * core_alpha));
    edges.paint(window, ink);
}

/// How a dependency's diamond is drawn.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum Diamond {
    /// A required dependency.
    Solid,
    /// An optional one.
    Hollow,
    /// A dev or build dependency (⌥): smaller, fainter, hollow.
    Dev,
}

/// Paints the dependency diamond centred in `bounds`.
pub(crate) fn diamond(kind: Diamond, bounds: Bounds<Pixels>, ink: Hsla, window: &mut Window) {
    let c = bounds.center();
    let (cx, cy) = (f32::from(c.x), f32::from(c.y));
    let side = f32::from(bounds.size.width.min(bounds.size.height));
    // A square of `side` turned 45°: its half-diagonal (it overhangs its
    // box as the board's rotated square does).
    let r = side * 0.5 * std::f32::consts::SQRT_2 * if kind == Diamond::Dev { 0.8 } else { 1.0 };
    let shape = Poly::new([pt(cx, cy - r), pt(cx + r, cy), pt(cx, cy + r), pt(cx - r, cy)]);
    match kind {
        Diamond::Solid => {
            let mut fill = Fill::new();
            fill.poly(&shape);
            fill.paint(window, ink);
        }
        Diamond::Hollow | Diamond::Dev => {
            let w = if kind == Diamond::Dev { 1.0 } else { 1.2 } * side / 6.5;
            let mut fill = Fill::new();
            for piece in shape.offset(-w * 0.5).stroke_ring(w) {
                fill.poly(&piece);
            }
            fill.paint(window, ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{CRATES, NUGET, facet_light, rotate};

    #[test]
    fn a_stone_turned_one_whole_step_takes_the_light_it_started_with() {
        // Turning the octagon a half turn (its step) lights the same facets.
        let turned: Vec<(f32, f32)> =
            CRATES[0].outer.iter().map(|p| rotate(*p, (12.0, 12.0), 180_f32.to_radians())).collect();
        let (a, b) = (facet_light(CRATES[0].outer), facet_light(&turned));
        // The facet that faces the light after a half turn is the one
        // opposite: the lights are the same set, shifted by half the edges.
        let n = a.len();
        for i in 0..n {
            assert!((a[i] - b[(i + n / 2) % n]).abs() < 1e-3, "facet {i}: {} vs {}", a[i], b[(i + n / 2) % n]);
        }
        // Half a step in, the light has moved: it is a turn, not a still.
        let half: Vec<(f32, f32)> = NUGET[0].outer.iter().map(|p| rotate(*p, (12.0, 12.2), 30_f32.to_radians())).collect();
        assert_ne!(facet_light(NUGET[0].outer), facet_light(&half));
    }
}

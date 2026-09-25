//! Strands: the cubic relation lines the rose draws between its hub and its
//! members. Three voices — **written** (solid: a fact that holds still),
//! **via** (dashed 2·4: arrives through a blanket or auto impl), **flow**
//! (dashed 3·9, the dashes marching on the ambient pulse: a value in
//! flight right now).
//!
//! GPUI paths are triangle fans, not stroked curves, so a strand is sampled
//! into a polyline and stroked as a chain of mitre-free quads with round-ish
//! overlap. A strand draws in by arc length (`reveal`), so a spoke grows at
//! a constant visual speed even where the curve bends.

use crate::paint::geom::{Fill, Poly, Pt, pt};
use gpui::{Hsla, Window};

/// How a strand is drawn.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Voice {
    /// Solid.
    #[default]
    Written,
    /// Dashed 2 on, 4 off.
    Via,
    /// Dashed 3 on, 9 off, marching (drive `phase` from the pulse).
    Flow,
}

/// A cubic from `from` through `c0`, `c1` to `to`, in window px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strand {
    /// Start.
    pub from: Pt,
    /// First control point.
    pub c0: Pt,
    /// Second control point.
    pub c1: Pt,
    /// End.
    pub to: Pt,
}

impl Strand {
    /// The point at parameter `t`.
    #[must_use]
    pub fn at(&self, t: f32) -> Pt {
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        pt(
            a * self.from.x + b * self.c0.x + c * self.c1.x + d * self.to.x,
            a * self.from.y + b * self.c0.y + c * self.c1.y + d * self.to.y,
        )
    }

    /// The curve as `n` straight pieces (`n + 1` points).
    #[must_use]
    pub fn polyline(&self, n: usize) -> Vec<Pt> {
        #[allow(clippy::cast_precision_loss)]
        (0..=n).map(|i| self.at(i as f32 / n.max(1) as f32)).collect()
    }

    /// Distance from `p` to the curve (sampled), px.
    #[must_use]
    pub fn distance(&self, p: Pt) -> f32 {
        let line = self.polyline(24);
        line.windows(2)
            .map(|w| segment_distance(p, w[0], w[1]))
            .fold(f32::MAX, f32::min)
    }
}

fn segment_distance(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (qx, qy) = (a.x + dx * t, a.y + dy * t);
    ((p.x - qx).powi(2) + (p.y - qy).powi(2)).sqrt()
}

/// A quad for `a → b`, `w` wide, extended by `w / 2` at both ends so the
/// chain of a sampled curve has no seams.
fn piece(a: Pt, b: Pt, w: f32) -> Poly {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    let (ux, uy) = (dx / len, dy / len);
    let (a, b) = (
        pt(a.x - ux * w * 0.25, a.y - uy * w * 0.25),
        pt(b.x + ux * w * 0.25, b.y + uy * w * 0.25),
    );
    let (nx, ny) = (-uy * w * 0.5, ux * w * 0.5);
    let quad = Poly::new([
        pt(a.x - nx, a.y - ny),
        pt(b.x - nx, b.y - ny),
        pt(b.x + nx, b.y + ny),
        pt(a.x + nx, a.y + ny),
    ]);
    if quad.area() < 0.0 {
        Poly::new(quad.points().iter().rev().copied())
    } else {
        quad
    }
}

/// The dashes (or the one solid run) of `strand`, `width` px wide, drawn
/// in to `reveal` of its arc length, in `voice` (`phase` in periods marches
/// a flow's dashes; `scale` scales the dash pattern with the text).
#[must_use]
pub fn pieces(strand: &Strand, width: f32, reveal: f32, voice: Voice, phase: f32, scale: f32) -> Vec<Poly> {
    let mut out = Vec::new();
    let line = strand.polyline(40);
    let lengths: Vec<f32> = line
        .windows(2)
        .map(|w| ((w[1].x - w[0].x).powi(2) + (w[1].y - w[0].y).powi(2)).sqrt())
        .collect();
    let total: f32 = lengths.iter().sum();
    let limit = total * reveal.clamp(0.0, 1.0);
    let dash = match voice {
        Voice::Written => None,
        Voice::Via => Some((2.0 * scale, 4.0 * scale)),
        Voice::Flow => Some((3.0 * scale, 9.0 * scale)),
    };
    let mut start = 0.0_f32;
    for (w, len) in line.windows(2).zip(&lengths) {
        let (a, b) = (w[0], w[1]);
        let end = start + len;
        if start >= limit {
            break;
        }
        let stop = end.min(limit);
        let point_at = |d: f32| {
            let t = if *len > 0.0 { (d - start) / len } else { 0.0 };
            pt(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
        };
        match dash {
            None => out.push(piece(a, point_at(stop), width)),
            Some((on, off)) => {
                let period = on + off;
                let shift = phase.rem_euclid(1.0) * period;
                let mut k = ((start - shift) / period).floor();
                loop {
                    let d0 = k * period + shift;
                    if d0 >= stop {
                        break;
                    }
                    let (lo, hi) = (d0.max(start), (d0 + on).min(stop));
                    if hi > lo {
                        out.push(piece(point_at(lo), point_at(hi), width));
                    }
                    k += 1.0;
                }
            }
        }
        start = end;
    }
    out
}

/// Adds [`pieces`] to `fill`.
pub fn stroke(fill: &mut Fill, strand: &Strand, width: f32, reveal: f32, voice: Voice, phase: f32, scale: f32) {
    for poly in pieces(strand, width, reveal, voice, phase, scale) {
        fill.poly(&poly);
    }
}

/// Paints one strand in one colour.
pub fn paint(window: &mut Window, strand: &Strand, width: f32, reveal: f32, voice: Voice, phase: f32, scale: f32, color: Hsla) {
    let mut fill = Fill::new();
    stroke(&mut fill, strand, width, reveal, voice, phase, scale);
    fill.paint(window, color);
}

#[cfg(test)]
mod tests {
    use super::{Strand, Voice, pieces};
    use crate::paint::geom::pt;

    fn line() -> Strand {
        Strand {
            from: pt(0.0, 0.0),
            c0: pt(33.0, 0.0),
            c1: pt(66.0, 0.0),
            to: pt(100.0, 0.0),
        }
    }

    #[test]
    fn a_straight_cubic_is_its_chord() {
        let s = line();
        assert!((s.at(0.5).x - 50.0).abs() < 1.0);
        assert!(s.distance(pt(50.0, 3.0)) - 3.0 < 0.01);
        assert!(s.distance(pt(-4.0, 0.0)) - 4.0 < 0.01);
    }

    #[test]
    fn reveal_and_dashes_cover_what_they_should() {
        let s = line();
        // Painted length: each piece is extended by w/2 in total, so subtract it.
        let painted = |voice: Voice, reveal: f32| -> f32 {
            pieces(&s, 1.0, reveal, voice, 0.0, 1.0)
                .iter()
                .map(|p| p.area() - 0.5)
                .sum::<f32>()
        };
        assert!(pieces(&s, 1.0, 0.0, Voice::Written, 0.0, 1.0).is_empty(), "a zero reveal drew something");
        assert!((painted(Voice::Written, 1.0) - 100.0).abs() < 2.0, "{}", painted(Voice::Written, 1.0));
        assert!((painted(Voice::Written, 0.5) - 50.0).abs() < 2.0);
        // Via: 2 on / 4 off → a third of the length.
        let via = painted(Voice::Via, 1.0);
        assert!((via - 100.0 / 3.0).abs() < 3.0, "{via}");
        assert!((painted(Voice::Via, 0.5) - via / 2.0).abs() < 3.0);
        // Flow: 3 on / 9 off → a quarter, wherever the march is.
        for phase in [0.0, 0.3, 0.77] {
            let flow: f32 = pieces(&s, 1.0, 1.0, Voice::Flow, phase, 1.0).iter().map(|p| p.area() - 0.5).sum();
            assert!((flow - 25.0).abs() < 4.0, "{phase}: {flow}");
        }
    }
}

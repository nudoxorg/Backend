//! Convex geometry for the cut language.
//!
//! Every shape FACET paints is convex: the chamfered plate, its bevel halves,
//! a running-light wedge, a hatch stripe, a gem facet. So one small toolkit
//! covers all of it: a convex polygon ([`Poly`]), a Sutherland–Hodgman clip
//! against a half-plane or another convex polygon, a mitred offset, a stroke
//! ring built from edge quads, and point-in-polygon.
//!
//! Polygons are emitted straight into GPUI as triangle fans through
//! [`gpui::Path::push_triangle`] (no tessellator): a fan of a convex polygon
//! never overlaps itself, and the renderer's 4x MSAA gives every straight
//! edge its antialiasing.
//!
//! Coordinates are logical pixels in window space, y pointing down. A polygon
//! is kept in *positive* orientation: clockwise on screen, which makes
//! [`side`] positive for every interior point of every edge.

use gpui::{Background, Path, Pixels, Point, Window, point, px};
use smallvec::SmallVec;

/// A point in logical pixels.
pub type Pt = Point<f32>;

/// Shorthand constructor for a [`Pt`].
#[must_use]
pub const fn pt(x: f32, y: f32) -> Pt {
    Point { x, y }
}

/// Twice the signed area of triangle `a b p`; positive when `p` lies on the
/// interior side of the directed edge `a -> b` of a positively oriented
/// polygon (to the right of travel, on screen).
#[must_use]
pub fn side(a: Pt, b: Pt, p: Pt) -> f32 {
    (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x)
}

fn lerp_pt(a: Pt, b: Pt, t: f32) -> Pt {
    pt(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// A convex polygon, positively oriented.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Poly(pub SmallVec<[Pt; 8]>);

impl Poly {
    /// A polygon from points already in positive (screen-clockwise) order.
    #[must_use]
    pub fn new(points: impl IntoIterator<Item = Pt>) -> Self {
        Self(points.into_iter().collect())
    }

    /// An axis-aligned rectangle.
    #[must_use]
    pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self::new([pt(x, y), pt(x + w, y), pt(x + w, y + h), pt(x, y + h)])
    }

    /// The FACET cut: one 45° chamfer of `c` at the top-left corner and one
    /// at the bottom-right, exactly the boards' `clip-path`:
    /// `polygon(c 0, 100% 0, 100% calc(100% - c), calc(100% - c) 100%, 0 100%, 0 c)`.
    ///
    /// `c` is clamped to the shorter side so the shape stays convex.
    #[must_use]
    #[allow(clippy::many_single_char_names)]
    pub fn chamfer(x: f32, y: f32, w: f32, h: f32, c: f32) -> Self {
        let c = c.clamp(0.0, w.min(h).max(0.0));
        if c <= 0.0 {
            return Self::rect(x, y, w, h);
        }
        Self::new([
            pt(x + c, y),
            pt(x + w, y),
            pt(x + w, y + h - c),
            pt(x + w - c, y + h),
            pt(x, y + h),
            pt(x, y + c),
        ])
    }

    /// The points.
    #[must_use]
    pub fn points(&self) -> &[Pt] {
        &self.0
    }

    /// Whether this polygon has no area to paint.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.len() < 3 || self.area() <= 1e-4
    }

    /// Signed area (positive for positive orientation).
    #[must_use]
    pub fn area(&self) -> f32 {
        let n = self.0.len();
        if n < 3 {
            return 0.0;
        }
        let mut twice = 0.0;
        for i in 0..n {
            let a = self.0[i];
            let b = self.0[(i + 1) % n];
            twice += a.x * b.y - b.x * a.y;
        }
        twice * 0.5
    }

    /// The same polygon moved by `(dx, dy)`.
    #[must_use]
    pub fn translate(&self, dx: f32, dy: f32) -> Self {
        Self(self.0.iter().map(|p| pt(p.x + dx, p.y + dy)).collect())
    }

    /// The same polygon scaled about the origin, then moved.
    #[must_use]
    pub fn transform(&self, scale: f32, dx: f32, dy: f32) -> Self {
        Self(
            self.0
                .iter()
                .map(|p| pt(p.x * scale + dx, p.y * scale + dy))
                .collect(),
        )
    }

    /// Keeps the part of the polygon on the interior side of the directed
    /// line `a -> b` (where [`side`] is `>= 0`). Sutherland–Hodgman, one edge.
    #[must_use]
    pub fn clip_half_plane(&self, a: Pt, b: Pt) -> Self {
        let n = self.0.len();
        let mut out = SmallVec::new();
        if n == 0 {
            return Self(out);
        }
        for i in 0..n {
            let cur = self.0[i];
            let next = self.0[(i + 1) % n];
            let sc = side(a, b, cur);
            let sn = side(a, b, next);
            if sc >= 0.0 {
                out.push(cur);
            }
            if (sc >= 0.0) != (sn >= 0.0) {
                let t = sc / (sc - sn);
                out.push(lerp_pt(cur, next, t));
            }
        }
        Self(out)
    }

    /// The intersection with another convex polygon (positively oriented).
    #[must_use]
    pub fn clip(&self, window: &Self) -> Self {
        let n = window.0.len();
        let mut out = self.clone();
        for i in 0..n {
            if out.0.is_empty() {
                break;
            }
            out = out.clip_half_plane(window.0[i], window.0[(i + 1) % n]);
        }
        out
    }

    /// Keeps the band `lo <= p·dir < hi` (`dir` need not be unit length;
    /// the band is measured in its units).
    #[must_use]
    pub fn clip_band(&self, dir: Pt, lo: f32, hi: f32) -> Self {
        // p·dir >= lo  <=>  interior of the line through lo*dir/|dir|² with
        // direction perpendicular to dir, oriented so dir points inside.
        let len2 = dir.x * dir.x + dir.y * dir.y;
        if len2 <= 0.0 {
            return Self::default();
        }
        let perp = pt(-dir.y, dir.x);
        let a_lo = pt(dir.x * lo / len2, dir.y * lo / len2);
        let a_hi = pt(dir.x * hi / len2, dir.y * hi / len2);
        // side(a, a + perp, a + dir) = perp.x*dir.y - perp.y*dir.x = -(dir.y² + dir.x²) < 0,
        // so the interior (towards +dir) of `a -> a + perp` is negative: flip it.
        let lower = self.clip_half_plane(pt(a_lo.x + perp.x, a_lo.y + perp.y), a_lo);
        lower.clip_half_plane(a_hi, pt(a_hi.x + perp.x, a_hi.y + perp.y))
    }

    /// Whether `p` lies inside (or on the boundary of) the polygon.
    #[must_use]
    pub fn contains(&self, p: Pt) -> bool {
        let n = self.0.len();
        if n < 3 {
            return false;
        }
        (0..n).all(|i| side(self.0[i], self.0[(i + 1) % n], p) >= -1e-4)
    }

    /// The polygon grown outward by `d` (inward for negative `d`) with
    /// mitred corners: every edge moves `d` along its outward normal.
    #[must_use]
    pub fn offset(&self, d: f32) -> Self {
        let n = self.0.len();
        if n < 3 || d == 0.0 {
            return self.clone();
        }
        let normal = |i: usize| {
            let a = self.0[i];
            let b = self.0[(i + 1) % n];
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let len = (dx * dx + dy * dy).sqrt().max(1e-6);
            // Outward normal of a positively oriented polygon.
            pt(dy / len, -dx / len)
        };
        let mut out = SmallVec::new();
        for i in 0..n {
            let na = normal((i + n - 1) % n);
            let nb = normal(i);
            let dot = na.x * nb.x + na.y * nb.y;
            let k = d / (1.0 + dot).max(1e-3);
            out.push(pt(
                self.0[i].x + (na.x + nb.x) * k,
                self.0[i].y + (na.y + nb.y) * k,
            ));
        }
        Self(out)
    }

    /// A closed stroke of width `w` centred on the outline, as one convex
    /// quad per edge (mitred joins, no overlaps).
    #[must_use]
    pub fn stroke_ring(&self, w: f32) -> SmallVec<[Self; 8]> {
        let outer = self.offset(w * 0.5);
        let inner = self.offset(-w * 0.5);
        let n = self.0.len();
        (0..n)
            .map(|i| {
                let j = (i + 1) % n;
                Self::new([outer.0[i], outer.0[j], inner.0[j], inner.0[i]])
            })
            .collect()
    }

    /// The bounding box `(min, max)`.
    #[must_use]
    pub fn bounds(&self) -> (Pt, Pt) {
        let mut min = pt(f32::MAX, f32::MAX);
        let mut max = pt(f32::MIN, f32::MIN);
        for p in &self.0 {
            min.x = min.x.min(p.x);
            min.y = min.y.min(p.y);
            max.x = max.x.max(p.x);
            max.y = max.y.max(p.y);
        }
        (min, max)
    }
}

/// A wedge of the plane around `centre`, from angle `from` to `to` (radians,
/// clockwise from 12 o'clock like a CSS conic gradient). Spans of 180° or
/// more are split into convex pieces. Returns the pieces of `poly` inside it.
#[must_use]
pub fn clip_wedge(poly: &Poly, centre: Pt, from: f32, to: f32) -> SmallVec<[Poly; 4]> {
    let mut pieces = SmallVec::new();
    let span = to - from;
    if span <= 0.0 {
        return pieces;
    }
    // At most 120° per piece keeps each piece a strict (convex) cone.
    let steps = (span / (std::f32::consts::TAU / 3.0)).ceil().max(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let count = steps as usize;
    let step = span / steps;
    for k in 0..count {
        #[allow(clippy::cast_precision_loss)]
        let a0 = from + step * k as f32;
        let a1 = a0 + step;
        let d0 = pt(a0.sin(), -a0.cos());
        let d1 = pt(a1.sin(), -a1.cos());
        let piece = poly
            .clip_half_plane(centre, pt(centre.x + d0.x, centre.y + d0.y))
            .clip_half_plane(pt(centre.x + d1.x, centre.y + d1.y), centre);
        if !piece.is_empty() {
            pieces.push(piece);
        }
    }
    pieces
}

/// Accumulates convex polygons into one GPUI path (a triangle fan per
/// polygon) so a whole family of same-coloured shapes is one draw.
#[derive(Default)]
pub struct Fill {
    path: Option<Path<Pixels>>,
}

impl Fill {
    /// An empty batch.
    #[must_use]
    pub const fn new() -> Self {
        Self { path: None }
    }

    /// Adds a convex polygon.
    pub fn poly(&mut self, poly: &Poly) {
        let pts = poly.points();
        if pts.len() < 3 {
            return;
        }
        let first = gp(pts[0]);
        let path = self.path.get_or_insert_with(|| Path::new(first));
        let st = (point(0.0, 1.0), point(0.0, 1.0), point(0.0, 1.0));
        for i in 1..pts.len() - 1 {
            path.push_triangle((first, gp(pts[i]), gp(pts[i + 1])), st);
        }
    }

    /// Adds one triangle.
    pub fn triangle(&mut self, a: Pt, b: Pt, c: Pt) {
        let first = gp(a);
        let path = self.path.get_or_insert_with(|| Path::new(first));
        let st = (point(0.0, 1.0), point(0.0, 1.0), point(0.0, 1.0));
        path.push_triangle((first, gp(b), gp(c)), st);
    }

    /// Whether anything was added.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.path.is_none()
    }

    /// Paints the batch (a no-op when empty or fully transparent).
    pub fn paint(self, window: &mut Window, color: impl Into<Background>) {
        if let Some(path) = self.path {
            window.paint_path(path, color);
        }
    }
}

fn gp(p: Pt) -> Point<Pixels> {
    point(px(p.x), px(p.y))
}

/// Paints one convex polygon in one colour.
pub fn fill_poly(window: &mut Window, poly: &Poly, color: impl Into<Background>) {
    let mut fill = Fill::new();
    fill.poly(poly);
    fill.paint(window, color);
}

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function evaluated at `t`.
#[must_use]
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, s: f32| {
        let u = 1.0 - s;
        3.0 * u * u * s * a + 3.0 * u * s * s * b + s * s * s
    };
    // Solve x(s) = t by bisection (monotone for 0 <= x1, x2 <= 1).
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    let mut s = t;
    for _ in 0..24 {
        let x = bez(x1, x2, s);
        if (x - t).abs() < 1e-5 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = (lo + hi) * 0.5;
    }
    bez(y1, y2, s)
}

/// CSS `ease-in-out`.
#[must_use]
pub fn ease_in_out(t: f32) -> f32 {
    cubic_bezier(0.42, 0.0, 0.58, 1.0, t)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::float_cmp
)]
mod tests {
    use super::*;

    /// A tiny deterministic generator for property tests.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> f32 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            #[allow(clippy::cast_precision_loss)]
            let v = (self.0 >> 40) as f32 / (1u64 << 24) as f32;
            v
        }
    }

    /// A random convex polygon: points on an ellipse at sorted angles.
    fn random_convex(rng: &mut Lcg) -> Poly {
        let n = 3 + (rng.next() * 9.0) as usize;
        let mut angles: Vec<f32> = (0..n).map(|_| rng.next() * std::f32::consts::TAU).collect();
        angles.sort_by(f32::total_cmp);
        let (cx, cy) = (rng.next() * 200.0 - 100.0, rng.next() * 200.0 - 100.0);
        let (rx, ry) = (10.0 + rng.next() * 150.0, 10.0 + rng.next() * 150.0);
        // Increasing angle with y down is clockwise on screen: positive.
        Poly::new(
            angles
                .iter()
                .map(|a| pt(cx + rx * a.cos(), cy + ry * a.sin())),
        )
    }

    #[test]
    fn chamfer_matches_the_css_polygon() {
        let p = Poly::chamfer(0.0, 0.0, 100.0, 40.0, 14.0);
        assert_eq!(
            p.points(),
            &[
                pt(14.0, 0.0),
                pt(100.0, 0.0),
                pt(100.0, 26.0),
                pt(86.0, 40.0),
                pt(0.0, 40.0),
                pt(0.0, 14.0)
            ]
        );
        // Two 14px right triangles cut from the rectangle.
        assert!((p.area() - (4000.0 - 2.0 * 98.0)).abs() < 1e-3);
    }

    #[test]
    fn half_planes_partition_area() {
        let mut rng = Lcg(7);
        for _ in 0..500 {
            let poly = random_convex(&mut rng);
            let a = pt(rng.next() * 300.0 - 150.0, rng.next() * 300.0 - 150.0);
            let b = pt(rng.next() * 300.0 - 150.0, rng.next() * 300.0 - 150.0);
            if (a.x - b.x).abs() + (a.y - b.y).abs() < 1e-3 {
                continue;
            }
            let left = poly.clip_half_plane(a, b);
            let right = poly.clip_half_plane(b, a);
            let sum = left.area() + right.area();
            assert!(
                (sum - poly.area()).abs() <= poly.area() * 1e-3 + 1e-2,
                "{sum} vs {}",
                poly.area()
            );
            assert!(left.area() >= -1e-3 && right.area() >= -1e-3);
        }
    }

    #[test]
    fn clipping_is_contained_and_symmetric() {
        let mut rng = Lcg(11);
        for _ in 0..500 {
            let p = random_convex(&mut rng);
            let q = random_convex(&mut rng);
            let pq = p.clip(&q);
            let qp = q.clip(&p);
            assert!(pq.area() <= p.area() + 1e-2 && pq.area() <= q.area() + 1e-2);
            assert!((pq.area() - qp.area()).abs() <= p.area().max(q.area()) * 1e-3 + 1e-2);
            // Clipping by itself is the identity (in area).
            assert!((p.clip(&p).area() - p.area()).abs() <= p.area() * 1e-3 + 1e-2);
        }
    }

    #[test]
    fn bands_tile_the_polygon() {
        let mut rng = Lcg(3);
        for _ in 0..200 {
            let p = random_convex(&mut rng);
            let dir = pt(rng.next() - 0.5, rng.next() - 0.5);
            let mut total = 0.0;
            let mut k = -40.0;
            while k < 40.0 {
                total += p.clip_band(dir, k * 10.0, (k + 1.0) * 10.0).area();
                k += 1.0;
            }
            assert!(
                (total - p.area()).abs() <= p.area() * 2e-3 + 1e-2,
                "{total} vs {}",
                p.area()
            );
        }
    }

    #[test]
    fn wedges_tile_the_polygon() {
        let mut rng = Lcg(5);
        for _ in 0..200 {
            let p = random_convex(&mut rng);
            let c = pt(rng.next() * 40.0 - 20.0, rng.next() * 40.0 - 20.0);
            let start = rng.next() * std::f32::consts::TAU;
            let cut = start + rng.next() * std::f32::consts::TAU;
            let a: f32 = clip_wedge(&p, c, start, cut).iter().map(Poly::area).sum();
            let b: f32 = clip_wedge(&p, c, cut, start + std::f32::consts::TAU)
                .iter()
                .map(Poly::area)
                .sum();
            assert!(
                (a + b - p.area()).abs() <= p.area() * 2e-3 + 1e-2,
                "{} vs {}",
                a + b,
                p.area()
            );
        }
    }

    #[test]
    fn contains_agrees_with_clipping() {
        let mut rng = Lcg(9);
        for _ in 0..200 {
            let p = random_convex(&mut rng);
            for _ in 0..20 {
                let q = pt(rng.next() * 400.0 - 200.0, rng.next() * 400.0 - 200.0);
                // A 0.1px square around q: fully inside or fully outside
                // unless q sits on the boundary, which we skip.
                let tiny = Poly::rect(q.x - 0.05, q.y - 0.05, 0.1, 0.1);
                let covered = tiny.clip(&p).area() / tiny.area();
                if covered > 0.999 {
                    assert!(p.contains(q));
                } else if covered < 0.001 {
                    assert!(!p.contains(q));
                }
            }
        }
    }

    #[test]
    fn stroke_ring_has_the_expected_area() {
        let sq = Poly::rect(0.0, 0.0, 10.0, 10.0);
        let ring: f32 = sq.stroke_ring(2.0).iter().map(Poly::area).sum();
        // Outer 12x12 minus inner 8x8.
        assert!((ring - (144.0 - 64.0)).abs() < 1e-3);
    }

    #[test]
    fn ease_in_out_is_symmetric() {
        assert!(ease_in_out(0.0).abs() < 1e-4);
        assert!((ease_in_out(1.0) - 1.0).abs() < 1e-4);
        assert!((ease_in_out(0.5) - 0.5).abs() < 1e-3);
        assert!((ease_in_out(0.25) + ease_in_out(0.75) - 1.0).abs() < 1e-3);
    }
}

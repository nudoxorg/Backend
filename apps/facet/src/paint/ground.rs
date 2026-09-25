//! The faceted ground: the low-poly field behind every window.
//!
//! This is the whole atmosphere of v3 — no glow, no gradient: a sparse field
//! of flat triangles in the ground hue at 0.6–9 % opacity, densest in the
//! top-left (where the light comes from), a smaller drift in the
//! bottom-right, a few strays between. A handful twinkle (opacity × 3.2 at
//! the top of a 7 s ease-in-out breath), a rarer few in mint.
//!
//! The lattice is hashed from integer grid coordinates and a seed, anchored
//! at the window's top-left, so resizing never reshuffles it: density is a
//! smooth function of position relative to the window, so triangles fade in
//! and out at the field's edges instead of popping. Geometry is cached per
//! (size, seed); a repaint only rebuilds triangle lists (a few hundred
//! triangles, bucketed by opacity into a few dozen paths).

use super::geom::{Fill, Pt, ease_in_out, pt};
use crate::theme::ActiveFacet;
use gpui::{
    App, Bounds, ColorExt, Element, ElementId, Global, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Refineable, Style, StyleRefinement, Styled, Window,
};
use std::sync::Arc;

/// Lattice pitch in px.
const CELL: f32 = 104.0;
/// Opacity range of the field.
const O_MIN: f32 = 0.006;
const O_MAX: f32 = 0.088;
/// Twinkle: the peak multiplies opacity by this much.
const TWINKLE_PEAK: f32 = 3.2;
/// Opacity quantum for batching (1/1000: invisible steps, few buckets).
const QUANTUM: f32 = 1000.0;

/// One triangle of the field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shard {
    /// Corners, px relative to the field's top-left.
    pub points: [Pt; 3],
    /// Resting opacity.
    pub opacity: f32,
    /// Twinkle delay as a fraction of the 7 s loop, if it twinkles.
    pub twinkle: Option<f32>,
    /// Twinkles in mint instead of the ground hue.
    pub mint: bool,
}

/// A generated field: every visible shard, plus the resting ones bucketed
/// by quantized opacity.
#[derive(Debug, PartialEq)]
pub struct Field {
    /// Every shard (resting and twinkling).
    pub shards: Vec<Shard>,
    /// Resting shards by opacity level (`level / 1000`), ascending.
    buckets: Vec<(u16, Vec<usize>)>,
    /// Twinkling shards.
    twinklers: Vec<usize>,
}

/// `SplitMix64`: a small, well-mixed hash for lattice coordinates.
fn hash(seed: u64, i: i64, j: i64, salt: u64) -> f32 {
    #[allow(clippy::cast_sign_loss)]
    let mut z = seed
        ^ (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (j as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ salt.wrapping_mul(0x1656_67B1_9E37_79F9);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    #[allow(clippy::cast_precision_loss)]
    let unit = (z >> 40) as f32 / (1u64 << 24) as f32;
    unit
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// How present the field is at `c` in a `w × h` window (0 = bare ground).
fn presence(c: Pt, w: f32, h: f32) -> f32 {
    let tl = ((c.x / (0.6 * w)).powi(2) + (c.y / (0.7 * h)).powi(2)).sqrt();
    let br = (((w - c.x) / (0.42 * w)).powi(2) + ((h - c.y) / (0.34 * h)).powi(2)).sqrt();
    (1.0 - tl).max(0.0).max((1.0 - br).max(0.0) * 0.85)
}

/// Generates the field for a `w × h` window.
#[must_use]
#[allow(clippy::many_single_char_names, clippy::too_many_lines)]
pub fn field(w: f32, h: f32, seed: u64) -> Field {
    #[allow(clippy::cast_possible_truncation)]
    let cols = (w / CELL).ceil() as i64 + 1;
    #[allow(clippy::cast_possible_truncation)]
    let rows = (h / CELL).ceil() as i64 + 1;
    #[allow(clippy::cast_precision_loss)]
    let vertex = |i: i64, j: i64| {
        let jx = (hash(seed, i, j, 1) - 0.5) * CELL * 0.72;
        let jy = (hash(seed, i, j, 2) - 0.5) * CELL * 0.72;
        pt(i as f32 * CELL + jx, j as f32 * CELL + jy)
    };
    let mut shards = Vec::new();
    for j in -1..rows {
        for i in -1..cols {
            let (a, b, c, d) = (
                vertex(i, j),
                vertex(i + 1, j),
                vertex(i + 1, j + 1),
                vertex(i, j + 1),
            );
            // Clockwise on screen either way the cell is split.
            let pair = if hash(seed, i, j, 3) < 0.5 {
                [[a, b, c], [a, c, d]]
            } else {
                [[a, b, d], [b, c, d]]
            };
            for (n, tri) in pair.into_iter().enumerate() {
                #[allow(clippy::cast_possible_wrap)]
                let salt = 10 + n as u64 * 10;
                let centre = pt(
                    (tri[0].x + tri[1].x + tri[2].x) / 3.0,
                    (tri[0].y + tri[1].y + tri[2].y) / 3.0,
                );
                let here = presence(centre, w, h);
                // Strays: a rare triangle anywhere. Holes: a fixed fraction
                // of the cluster is left bare. Both are per-cell constants,
                // so resizing only fades the cluster's edge, never reshuffles.
                let stray = hash(seed, i, j, salt + 2) < 0.03;
                let density = if stray { here.max(0.3) } else { here };
                let hole = hash(seed, i, j, salt + 1) < 0.12;
                let keep = if hole {
                    0.0
                } else {
                    smoothstep(0.05, 0.15, density)
                };
                if keep <= 0.0 {
                    continue;
                }
                let bright = hash(seed, i, j, salt + 3);
                let strength = density.powf(1.25) * (0.12 + 0.88 * bright.powf(1.6));
                let opacity = (O_MIN + (O_MAX - O_MIN) * strength) * keep;
                if opacity < O_MIN * 0.5 {
                    continue;
                }
                let twinkles = hash(seed, i, j, salt + 4) < 0.075;
                shards.push(Shard {
                    points: tri,
                    opacity,
                    twinkle: twinkles.then(|| hash(seed, i, j, salt + 5)),
                    mint: twinkles && here > 0.25 && hash(seed, i, j, salt + 6) < 0.6,
                });
            }
        }
    }
    let mut buckets: Vec<(u16, Vec<usize>)> = Vec::new();
    let mut twinklers = Vec::new();
    for (index, shard) in shards.iter().enumerate() {
        if shard.twinkle.is_some() {
            twinklers.push(index);
            continue;
        }
        let level = quantize(shard.opacity);
        match buckets.binary_search_by_key(&level, |(l, _)| *l) {
            Ok(at) => buckets[at].1.push(index),
            Err(at) => buckets.insert(at, (level, vec![index])),
        }
    }
    Field {
        shards,
        buckets,
        twinklers,
    }
}

fn quantize(opacity: f32) -> u16 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let level = (opacity * QUANTUM).round().clamp(1.0, 1000.0) as u16;
    level
}

/// A twinkling shard's opacity at `phase` of the 7 s loop.
#[must_use]
pub fn twinkle(opacity: f32, delay: f32, phase: f32) -> f32 {
    // keyframes twinkle { 0%,100% { o } 50% { o * 3.2 } }, ease-in-out.
    let local = (phase + delay).rem_euclid(1.0);
    let rise = if local < 0.5 {
        ease_in_out(local / 0.5)
    } else {
        1.0 - ease_in_out((local - 0.5) / 0.5)
    };
    opacity * (1.0 + (TWINKLE_PEAK - 1.0) * rise)
}

impl Field {
    /// The paint batches at `phase`: `(opacity, mint, triangles)`, with the
    /// triangles offset by `origin`.
    #[must_use]
    pub fn batches(&self, origin: Pt, phase: f32) -> Vec<Batch> {
        let shift = |p: Pt| pt(p.x + origin.x, p.y + origin.y);
        let mut out: Vec<(u16, bool, Fill)> =
            Vec::with_capacity(self.buckets.len() + self.twinklers.len());
        for (level, members) in &self.buckets {
            let mut fill = Fill::new();
            for &index in members {
                let [a, b, c] = self.shards[index].points;
                fill.triangle(shift(a), shift(b), shift(c));
            }
            out.push((*level, false, fill));
        }
        for &index in &self.twinklers {
            let shard = &self.shards[index];
            let level = quantize(twinkle(shard.opacity, shard.twinkle.unwrap_or(0.0), phase));
            let [a, b, c] = shard.points;
            if let Some((_, _, fill)) = out
                .iter_mut()
                .find(|(l, mint, _)| *l == level && *mint == shard.mint)
            {
                fill.triangle(shift(a), shift(b), shift(c));
            } else {
                let mut fill = Fill::new();
                fill.triangle(shift(a), shift(b), shift(c));
                out.push((level, shard.mint, fill));
            }
        }
        out.into_iter()
            .map(|(level, mint, fill)| (f32::from(level) / QUANTUM, mint, fill))
            .collect()
    }
}

/// One paint batch: opacity, whether it is mint, and its triangles.
pub type Batch = (f32, bool, Fill);

/// Generated fields, most recent first.
#[derive(Default)]
struct GroundCache(Vec<(FieldKey, Arc<Field>)>);

/// `(width, height, seed)`.
type FieldKey = (u32, u32, u64);

impl Global for GroundCache {}

fn cached_field(cx: &mut App, w: f32, h: f32, seed: u64) -> Arc<Field> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let key = (w.round() as u32, h.round() as u32, seed);
    let cache = cx.default_global::<GroundCache>();
    if let Some(at) = cache.0.iter().position(|(k, _)| *k == key) {
        let entry = cache.0.remove(at);
        let found = entry.1.clone();
        cache.0.insert(0, entry);
        return found;
    }
    let made = Arc::new(field(w, h, seed));
    cache.0.insert(0, (key, made.clone()));
    cache.0.truncate(4);
    made
}

/// The ground element. Build with [`ground`]; by default it fills its
/// parent absolutely (put it first inside a `relative` window root).
pub struct Ground {
    style: StyleRefinement,
    seed: u64,
    phase: f32,
}

/// The faceted ground, absolutely filling its parent.
#[must_use]
pub fn ground() -> Ground {
    Ground {
        style: StyleRefinement::default(),
        seed: 0x00FA_CE7E,
        phase: 0.0,
    }
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

impl Ground {
    /// A different field.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// Position in the 7 s twinkle loop (`0..1`); drive it from the pulse
    /// clock at ≤ 12 fps.
    #[must_use]
    pub const fn phase(mut self, phase: f32) -> Self {
        self.phase = phase;
        self
    }
}

impl Styled for Ground {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for Ground {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Ground {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let palette = cx.palette();
        let hue: Hsla = palette.ground.into();
        let mint: Hsla = palette.mint.base.into();
        let opacity = self.style.opacity.unwrap_or(1.0);
        let field = cached_field(cx, w, h, self.seed);
        let origin = pt(f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for (alpha, is_mint, fill) in field.batches(origin, self.phase) {
                let color = if is_mint { mint } else { hue };
                fill.paint(window, color.opacity(alpha * opacity));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_field() {
        assert_eq!(field(1440.0, 900.0, 7), field(1440.0, 900.0, 7));
        assert_ne!(
            field(1440.0, 900.0, 7).shards,
            field(1440.0, 900.0, 8).shards
        );
    }

    #[test]
    fn resizing_keeps_the_lattice() {
        // The same lattice cell yields the same triangle at any window size:
        // the top-left cluster survives a resize untouched in shape.
        let a = field(1440.0, 900.0, 3);
        let b = field(1500.0, 940.0, 3);
        let near = |f: &Field| -> Vec<[Pt; 3]> {
            f.shards
                .iter()
                .filter(|s| s.points.iter().all(|p| p.x < 300.0 && p.y < 200.0))
                .map(|s| s.points)
                .collect()
        };
        let (na, nb) = (near(&a), near(&b));
        assert!(!na.is_empty());
        assert!(na.iter().filter(|t| nb.contains(t)).count() * 10 >= na.len() * 8);
    }

    #[test]
    fn opacities_stay_in_the_boards_range_and_the_light_comes_from_the_top_left() {
        let f = field(1440.0, 900.0, 1);
        assert!(f.shards.len() > 40, "{}", f.shards.len());
        for s in &f.shards {
            assert!(
                s.opacity > 0.0 && s.opacity <= O_MAX + 1e-6,
                "{}",
                s.opacity
            );
        }
        let mean = |pred: &dyn Fn(Pt) -> bool| {
            let v: Vec<f32> = f
                .shards
                .iter()
                .filter(|s| pred(s.points[0]))
                .map(|s| s.opacity)
                .collect();
            (v.iter().sum::<f32>(), v.len())
        };
        let (tl_sum, tl_n) = mean(&|p| p.x < 480.0 && p.y < 300.0);
        let (mid_sum, mid_n) =
            mean(&|p| (600.0..900.0).contains(&p.x) && (350.0..600.0).contains(&p.y));
        assert!(tl_n > 0);
        // Denser and brighter at the top-left than mid-window.
        assert!(
            tl_sum > mid_sum * 3.0 && tl_n > mid_n,
            "{tl_sum}/{tl_n} vs {mid_sum}/{mid_n}"
        );
        let twinklers = f.shards.iter().filter(|s| s.twinkle.is_some()).count();
        assert!(twinklers >= 2 && twinklers * 6 < f.shards.len());
    }

    #[test]
    fn twinkle_peaks_at_three_point_two() {
        assert!((twinkle(0.01, 0.0, 0.0) - 0.01).abs() < 1e-6);
        assert!((twinkle(0.01, 0.0, 0.5) - 0.032).abs() < 1e-5);
        assert!((twinkle(0.01, 0.25, 0.25) - 0.032).abs() < 1e-5);
    }

    #[test]
    fn paint_batches_are_few_and_cheap() {
        for (w, h) in [(1440.0, 900.0), (2560.0, 1440.0)] {
            let f = field(w, h, 5);
            let start = std::time::Instant::now();
            let mut total = 0;
            for frame in 0..120 {
                #[allow(clippy::cast_precision_loss)]
                let batches = f.batches(pt(0.0, 0.0), frame as f32 / 120.0);
                total += batches.len();
            }
            let per_frame = start.elapsed() / 120;
            eprintln!(
                "ground {w}x{h}: {} shards, {} paths/frame, {:?} per frame (batches)",
                f.shards.len(),
                total / 120,
                per_frame
            );
            assert!(total / 120 < 120);
        }
    }
}

#[cfg(test)]
mod stats {
    #[test]
    #[ignore = "prints the field's statistics"]
    fn field_stats() {
        let f = super::field(1440.0, 900.0, 0x00FA_CE7E);
        let mut o: Vec<f32> = f.shards.iter().map(|s| s.opacity).collect();
        o.sort_by(f32::total_cmp);
        let tw = f.shards.iter().filter(|s| s.twinkle.is_some()).count();
        let mint = f.shards.iter().filter(|s| s.mint).count();
        #[allow(clippy::cast_precision_loss)]
        let mean = o.iter().sum::<f32>() / o.len() as f32;
        let area: f32 = f
            .shards
            .iter()
            .map(|s| super::super::geom::Poly::new(s.points).area().abs())
            .sum();
        eprintln!(
            "n={} tw={tw} mint={mint} min={} median={} mean={mean} max={} area={area}",
            o.len(),
            o[0],
            o[o.len() / 2],
            o[o.len() - 1]
        );
    }
}

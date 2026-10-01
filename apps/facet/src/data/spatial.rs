//! Spatial indexing and culling for marks with many parts: the substrate a
//! graph view can stand on.
//!
//! - [`Grid`]: a uniform bucket grid over rectangles (compressed rows, no
//!   per-cell allocation). A point query looks at one cell; a rect query
//!   visits only the cells it overlaps. Built once per layout, O(n).
//! - [`visible`]: the part of a mark's bounds that can actually be seen —
//!   its bounds clipped by the window's current content mask (every scroll
//!   container and the window edge). Marks iterate only what falls inside.
//! - [`Painted`]: a per-window count of what marks painted this frame, so
//!   tests can prove that paint cost follows what is visible, not what
//!   exists.

use gpui::{App, Bounds, Global, Pixels, Window};

/// An axis-aligned rectangle in any consistent space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aabb {
    /// Left.
    pub x0: f32,
    /// Top.
    pub y0: f32,
    /// Right.
    pub x1: f32,
    /// Bottom.
    pub y1: f32,
}

impl Aabb {
    /// From origin and size.
    #[must_use]
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x + w.max(0.0),
            y1: y + h.max(0.0),
        }
    }

    /// Whether the point is inside (half-open on the far edges).
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }

    /// Whether the two overlap.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }
}

/// A uniform grid of buckets over rectangles.
#[derive(Clone, Debug, Default)]
pub struct Grid {
    x0: f32,
    y0: f32,
    cell: f32,
    cols: usize,
    rows: usize,
    /// Bucket `c` holds `items[starts[c]..starts[c + 1]]`.
    starts: Vec<u32>,
    items: Vec<u32>,
    boxes: Vec<Aabb>,
}

impl Grid {
    /// Indexes `boxes` with square cells of `cell` units (`0` picks one from
    /// the boxes' mean size).
    #[must_use]
    pub fn build(boxes: &[Aabb], cell: f32) -> Self {
        if boxes.is_empty() {
            return Self::default();
        }
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        let mut mean = 0.0;
        for b in boxes {
            x0 = x0.min(b.x0);
            y0 = y0.min(b.y0);
            x1 = x1.max(b.x1);
            y1 = y1.max(b.y1);
            mean += (b.x1 - b.x0).max(b.y1 - b.y0);
        }
        #[allow(clippy::cast_precision_loss)]
        let cell = if cell > 0.0 { cell } else { (mean / boxes.len() as f32).max(1.0) };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (cols, rows) = (
            (((x1 - x0) / cell).ceil() as usize).clamp(1, 4096),
            (((y1 - y0) / cell).ceil() as usize).clamp(1, 4096),
        );
        let mut grid = Self {
            x0,
            y0,
            cell,
            cols,
            rows,
            starts: vec![0; cols * rows + 1],
            items: Vec::new(),
            boxes: boxes.to_vec(),
        };
        // Count, prefix-sum, fill: two passes, no per-cell vectors.
        let mut counts = vec![0_u32; cols * rows];
        for b in boxes {
            let (c0, r0, c1, r1) = grid.span(b);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    counts[r * cols + c] += 1;
                }
            }
        }
        let mut acc = 0;
        for (i, n) in counts.iter().enumerate() {
            grid.starts[i] = acc;
            acc += n;
        }
        grid.starts[cols * rows] = acc;
        grid.items = vec![0; acc as usize];
        let mut next: Vec<u32> = grid.starts[..cols * rows].to_vec();
        for (i, b) in boxes.iter().enumerate() {
            let (c0, r0, c1, r1) = grid.span(b);
            for r in r0..=r1 {
                for c in c0..=c1 {
                    let slot = &mut next[r * cols + c];
                    #[allow(clippy::cast_possible_truncation)]
                    {
                        grid.items[*slot as usize] = i as u32;
                    }
                    *slot += 1;
                }
            }
        }
        grid
    }

    /// The cell range `(c0, r0, c1, r1)` a box covers (clamped).
    fn span(&self, b: &Aabb) -> (usize, usize, usize, usize) {
        let cell = |v: f32, o: f32, n: usize| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let i = ((v - o) / self.cell).floor().max(0.0) as usize;
            i.min(n - 1)
        };
        (
            cell(b.x0, self.x0, self.cols),
            cell(b.y0, self.y0, self.rows),
            cell((b.x1 - 1e-4).max(b.x0), self.x0, self.cols),
            cell((b.y1 - 1e-4).max(b.y0), self.y0, self.rows),
        )
    }

    /// The first box containing `(x, y)`: one bucket, a handful of tests.
    #[must_use]
    pub fn at(&self, x: f32, y: f32) -> Option<usize> {
        if self.boxes.is_empty() || x < self.x0 || y < self.y0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (c, r) = (((x - self.x0) / self.cell) as usize, ((y - self.y0) / self.cell) as usize);
        if c >= self.cols || r >= self.rows {
            return None;
        }
        let cell = r * self.cols + c;
        self.items[self.starts[cell] as usize..self.starts[cell + 1] as usize]
            .iter()
            .map(|i| *i as usize)
            .find(|i| self.boxes[*i].contains(x, y))
    }

    /// Every box overlapping `view`, each once, in index order.
    #[must_use]
    pub fn query(&self, view: &Aabb) -> Vec<usize> {
        if self.boxes.is_empty() {
            return Vec::new();
        }
        #[allow(clippy::cast_precision_loss)]
        let bounds = Aabb {
            x0: self.x0,
            y0: self.y0,
            x1: self.x0 + self.cell * self.cols as f32,
            y1: self.y0 + self.cell * self.rows as f32,
        };
        if !bounds.overlaps(view) {
            return Vec::new();
        }
        let (c0, r0, c1, r1) = self.span(view);
        let mut out = Vec::new();
        let mut seen = vec![false; self.boxes.len()];
        for r in r0..=r1 {
            for c in c0..=c1 {
                let cell = r * self.cols + c;
                for &i in &self.items[self.starts[cell] as usize..self.starts[cell + 1] as usize] {
                    let i = i as usize;
                    if !seen[i] && self.boxes[i].overlaps(view) {
                        seen[i] = true;
                        out.push(i);
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// Boxes indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    /// Whether nothing is indexed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }
}

/// The part of `bounds` the window can show right now: clipped by the
/// current content mask (scroll containers, the window edge).
#[must_use]
pub fn visible(bounds: Bounds<Pixels>, window: &Window) -> Aabb {
    let mask = window.content_mask().bounds;
    let clip = bounds.intersect(&mask);
    Aabb::new(
        f32::from(clip.origin.x),
        f32::from(clip.origin.y),
        f32::from(clip.size.width),
        f32::from(clip.size.height),
    )
}

/// What the marks painted since the last [`take_painted`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Painted {
    /// Stones (mosaic, territory).
    pub stones: u64,
    /// Ticks (combs).
    pub ticks: u64,
    /// Regions (territory).
    pub regions: u64,
}

impl Global for Painted {}

/// Adds to this frame's counts.
pub(crate) fn count(cx: &mut App, f: impl FnOnce(&mut Painted)) {
    f(cx.default_global::<Painted>());
}

/// The counts since the last call, and resets them.
pub fn take_painted(cx: &mut App) -> Painted {
    std::mem::take(cx.default_global::<Painted>())
}

#[cfg(test)]
mod tests {
    use super::{Aabb, Grid};

    /// A deterministic scatter of boxes.
    fn boxes(n: usize) -> Vec<Aabb> {
        let mut seed = 42_u64;
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                #[allow(clippy::cast_precision_loss)]
                let (x, y) = (((seed >> 33) % 4000) as f32, ((seed >> 13) % 3000) as f32);
                #[allow(clippy::cast_precision_loss)]
                let (w, h) = (5.0 + ((seed >> 7) % 60) as f32, 5.0 + ((seed >> 3) % 40) as f32);
                Aabb::new(x, y, w, h)
            })
            .collect()
    }

    #[test]
    fn point_queries_agree_with_a_scan() {
        let all = boxes(3000);
        let grid = Grid::build(&all, 0.0);
        let mut seed = 7_u64;
        for _ in 0..4000 {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            #[allow(clippy::cast_precision_loss)]
            let (x, y) = (((seed >> 33) % 4100) as f32 - 50.0, ((seed >> 17) % 3100) as f32 - 50.0);
            let got = grid.at(x, y);
            match got {
                Some(i) => assert!(all[i].contains(x, y), "({x}, {y}) -> {i} does not contain it"),
                None => assert!(!all.iter().any(|b| b.contains(x, y)), "({x}, {y}) missed a box"),
            }
        }
    }

    #[test]
    fn rect_queries_return_exactly_the_overlapping_boxes() {
        let all = boxes(3000);
        let grid = Grid::build(&all, 64.0);
        for view in [
            Aabb::new(0.0, 0.0, 400.0, 300.0),
            Aabb::new(1500.0, 1000.0, 900.0, 700.0),
            Aabb::new(-100.0, -100.0, 50.0, 50.0),
            Aabb::new(3900.0, 2900.0, 400.0, 400.0),
        ] {
            let expected: Vec<usize> = (0..all.len()).filter(|i| all[*i].overlaps(&view)).collect();
            assert_eq!(grid.query(&view), expected, "{view:?}");
        }
    }
}

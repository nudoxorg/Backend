//! The mosaic: one stone per public item, one custom element for all of
//! them — cheap at 2 000. Calm at rest: one quiet ink, only yours lit mint;
//! the stone you rest on shows its family's hue.
//!
//! Stones batch into one path per ink, so a whole package paints in a dozen
//! draws. The pointer finds its stone by grid arithmetic (column and row from
//! one division each). The hover is a small 2-D wave — the rested stone lifts
//! and swells, its neighbours lift a little — driven by three motion tracks
//! (centre column, centre row, strength) however many stones there are.
//! Under ⌥ x-ray (or [`Mosaic::dimmed`]) everything that is not yours fades
//! to texture. A mosaic seen for the first time lays its stones in, a quick
//! stagger in reading order.

use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use crate::measure::Measure;
use crate::motion::{Spec, spec};
use crate::paint::geom::{Fill, Poly};
use crate::paint::hatch::Hatch;
use crate::theme::ActiveFacet;
use crate::tokens::{Family, Palette, motion as dur};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, Style, Window, point, px, size,
};
use std::rc::Rc;

/// What a stone says.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum StoneState {
    /// Private or undocumented: quiet.
    #[default]
    Private,
    /// Public: full strength.
    Public,
    /// Arrived in the release you are reading: mint.
    New,
    /// Removed: a coral hatch.
    Gone,
    /// Behind a feature or cfg gate: a quiet hatch.
    Gated,
}

/// One stone.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct Stone {
    /// The family (hue).
    pub family: Family,
    /// What it says.
    pub state: StoneState,
    /// Yours: your code reaches it (mint; stays lit when dimmed).
    pub yours: bool,
}

impl Stone {
    /// A stone of `family` at `state`.
    #[must_use]
    pub const fn new(family: Family, state: StoneState) -> Self {
        Self {
            family,
            state,
            yours: false,
        }
    }

    /// Your code reaches it.
    #[must_use]
    pub const fn yours(mut self) -> Self {
        self.yours = true;
        self
    }
}

/// Stone geometry: cell edge, gap and chamfer, px at 100 %.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Edge.
    pub edge: f32,
    /// Gap.
    pub gap: f32,
    /// Chamfer.
    pub cut: f32,
}

impl Cell {
    /// The board mosaic: 11 px stones, 3 px apart.
    pub const BOARD: Self = Self {
        edge: 11.0,
        gap: 3.0,
        cut: 3.0,
    };
    /// The territory map's stones: 13 px on a 16 px pitch.
    pub const TERRITORY: Self = Self {
        edge: 13.0,
        gap: 3.0,
        cut: 3.5,
    };

    fn scaled(self, s: f32) -> Self {
        Self {
            edge: self.edge * s,
            gap: self.gap * s,
            cut: self.cut * s,
        }
    }

    /// Distance from one stone's origin to the next.
    #[must_use]
    pub fn pitch(self) -> f32 {
        self.edge + self.gap
    }

    /// Columns that fit `width` px (at least one).
    #[must_use]
    pub fn columns(self, width: f32) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let cols = ((width + self.gap) / self.pitch()).floor().max(1.0) as usize;
        cols
    }

    /// The height `count` stones need at `columns`.
    #[must_use]
    pub fn height(self, count: usize, columns: usize) -> f32 {
        let rows = count.div_ceil(columns.max(1));
        #[allow(clippy::cast_precision_loss)]
        let h = rows as f32 * self.pitch() - self.gap;
        h.max(0.0)
    }

    /// The stone under `(x, y)` (relative to the grid's origin), counting the
    /// gap after a stone as its own so a sweep never falls through.
    #[must_use]
    pub fn at(self, x: f32, y: f32, columns: usize, count: usize) -> Option<usize> {
        if x < 0.0 || y < 0.0 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (col, row) = ((x / self.pitch()) as usize, (y / self.pitch()) as usize);
        if col >= columns {
            return None;
        }
        let i = row * columns + col;
        (i < count).then_some(i)
    }
}

/// The ink a stone paints in, and how strongly.
/// Calm (gui-plan §6.2): one quiet ink at rest — public a step brighter
/// than private — mint for yours and for what just arrived, coral for what
/// left. A stone takes its family's hue only while it is rested on.
fn ink(stone: Stone, palette: &Palette) -> (Hsla, f32) {
    let quiet: Hsla = palette.ground.into();
    match stone.state {
        _ if stone.yours => (palette.mint.base.into(), 0.85),
        StoneState::Private => (quiet, 0.10),
        StoneState::Public => (quiet, 0.22),
        StoneState::New => (palette.mint.base.into(), 1.0),
        StoneState::Gone => (palette.coral.base.into(), 0.8),
        StoneState::Gated => (quiet, 0.3),
    }
}

/// `(lift px, swell)` at a grid distance from the rested stone.
#[must_use]
pub fn stone_wave(distance: f32) -> (f32, f32) {
    let d = distance.abs();
    if d <= 1.0 {
        (3.0 - 1.5 * d, 1.55 - 0.55 * d)
    } else if d <= 2.0 {
        (1.5 * (2.0 - d), 1.0)
    } else {
        (0.0, 1.0)
    }
}

/// A mosaic. Build with [`mosaic`].
pub struct Mosaic {
    id: ElementId,
    stones: Rc<[Stone]>,
    measure: Measure,
    cell: Cell,
    door: Option<Door>,
    rest: Option<usize>,
    dimmed: Option<bool>,
    arrive: bool,
    max_rows: Option<usize>,
}

/// A mosaic of `stones` wrapping to the width `measure` gives it.
#[must_use]
pub fn mosaic(id: impl Into<ElementId>, stones: impl Into<Rc<[Stone]>>, measure: &Measure) -> Mosaic {
    Mosaic {
        id: id.into(),
        stones: stones.into(),
        measure: *measure,
        cell: Cell::BOARD,
        door: None,
        rest: None,
        dimmed: None,
        arrive: false,
        max_rows: None,
    }
}

impl Mosaic {
    /// Stone geometry.
    #[must_use]
    pub const fn cell(mut self, cell: Cell) -> Self {
        self.cell = cell;
        self
    }

    /// Stones open through `door` (part = stone index).
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a stone as rested (scenes; a live pointer overrides it).
    #[must_use]
    pub const fn rest(mut self, stone: Option<usize>) -> Self {
        self.rest = stone;
        self
    }

    /// Fades everything that is not yours (x-ray does this while ⌥ is held).
    #[must_use]
    pub const fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = Some(dimmed);
        self
    }

    /// Lays the stones in the first time this mosaic is seen.
    #[must_use]
    pub const fn arrive(mut self, arrive: bool) -> Self {
        self.arrive = arrive;
        self
    }

    /// Caps the rows drawn (the rest are counted, not drawn).
    #[must_use]
    pub const fn max_rows(mut self, rows: usize) -> Self {
        self.max_rows = Some(rows);
        self
    }

    fn shown(&self, columns: usize) -> usize {
        self.max_rows
            .map_or(self.stones.len(), |rows| self.stones.len().min(rows * columns))
    }
}

impl IntoElement for Mosaic {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[doc(hidden)]
pub struct MosaicLayout {
    live: Entity<Live>,
    keys: AnyElement,
}

impl Element for Mosaic {
    type RequestLayoutState = MosaicLayout;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
    ) -> (LayoutId, MosaicLayout) {
        let live = live::live(window, cx);
        let (keys, kid) = live::keys(&live, window, cx);
        let cell = self.cell.scaled(self.measure.scale());
        let width = f32::from(self.measure.width());
        let cols = cell.columns(width);
        let mut style = Style::default();
        style.size.width = px(width).into();
        style.size.height = px(cell.height(self.shown(cols), cols)).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [kid], cx), MosaicLayout { live, keys })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut MosaicLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        live::prepaint(bounds, &mut layout.keys, window, cx)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut MosaicLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let cell = self.cell.scaled(s);
        let cols = cell.columns(f32::from(bounds.size.width));
        let shown = self.shown(cols);
        let live = layout.live.clone();
        let motion = live.read(cx).motion.clone();
        let hover = live.read(cx).hover.or(self.rest);
        let walk = live::walking(&live, window, cx);
        let active = hover.or(walk);

        let dim_target = if self.dimmed.unwrap_or(self.measure.reveal().xray) {
            1.0
        } else {
            0.0
        };
        let dim = motion.animate(live::key(&self.id, "dim"), dim_target, spec::REVEAL, window, cx);
        let arrive = if self.arrive {
            motion.animate_from(
                live::key(&self.id, "arrive"),
                0.0,
                1.0,
                Spec::tween(dur::EMPH, dur::GLIDE),
                window,
                cx,
            )
        } else {
            1.0
        };
        // The 2-D wave: its centre springs in px, so a move to the next row
        // goes down, not along the row.
        #[allow(clippy::cast_precision_loss)]
        let (wx, wy, strength) = live::wave(
            &self.id,
            &live,
            active.map(|i| ((i % cols) as f32 * cell.pitch(), (i / cols) as f32 * cell.pitch())),
            window,
            cx,
        );
        let (col, row) = (wx / cell.pitch(), wy / cell.pitch());

        let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        // Batches keyed by (ink, opacity); stones near the wave paint alone.
        let mut batches: Vec<((Hsla, f32, bool), Fill)> = Vec::new();
        let mut lifted: Vec<(Poly, Hsla, bool, bool)> = Vec::new();
        #[allow(clippy::cast_precision_loss)]
        let reveal = |i: usize| -> f32 {
            // Staggered in reading order across the first 70 % of the arrival.
            let start = i as f32 / shown.max(1) as f32 * 0.7;
            ((arrive - start) / 0.3).clamp(0.0, 1.0)
        };
        // Only the rows the window can show are drawn (one row of slack for
        // the wave's lift): paint cost follows what is visible.
        let view = super::spatial::visible(bounds, window);
        let (first, last) = if view.y1 <= view.y0 || view.x1 <= view.x0 {
            (0, 0)
        } else {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let r0 = (((view.y0 - y0) / cell.pitch()).floor() - 1.0).max(0.0) as usize;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let r1 = (((view.y1 - y0) / cell.pitch()).ceil() + 1.0).max(0.0) as usize;
            ((r0 * cols).min(shown), (r1 * cols).min(shown))
        };
        super::spatial::count(cx, |p| p.stones += (last - first) as u64);
        for (i, stone) in self.stones.iter().enumerate().take(last).skip(first) {
            let appear = reveal(i);
            if appear <= 0.0 {
                continue;
            }
            #[allow(clippy::cast_precision_loss)]
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            let d = ((c - col).powi(2) + (r - row).powi(2)).sqrt();
            let (lift, swell) = stone_wave(d);
            let lift = lift * strength * s + (1.0 - appear) * 4.0 * s;
            let swell = 1.0 + (swell - 1.0) * strength;
            let (rest_ink, mut opacity) = ink(*stone, palette);
            // The rested stone (and, fading, its neighbours) takes its hue.
            let light = ((1.0 - d).clamp(0.0, 1.0) * strength).min(1.0);
            let ink = if stone.yours || light <= 0.0 {
                rest_ink
            } else {
                crate::paint::mix(rest_ink.opacity(opacity), palette.family(stone.family).hue.into(), light)
            };
            if light > 0.0 && !stone.yours {
                opacity = 1.0;
            }
            if !stone.yours {
                opacity *= 1.0 - 0.72 * dim;
            }
            opacity *= appear;
            let e = cell.edge * swell;
            let (sx, sy) = (
                x0 + c * cell.pitch() + (cell.edge - e) * 0.5,
                y0 + r * cell.pitch() + (cell.edge - e) * 0.5 - lift,
            );
            let poly = Poly::chamfer(sx, sy, e, e, cell.cut * swell);
            let hatched = matches!(stone.state, StoneState::Gone | StoneState::Gated) && !stone.yours;
            if lift.abs() > 0.01 || swell > 1.001 || hatched {
                lifted.push((poly, ink.opacity(opacity), hatched, walk == Some(i)));
                continue;
            }
            let key = (ink, opacity, false);
            match batches.iter_mut().find(|(k, _)| k.0 == key.0 && (k.1 - key.1).abs() < 1e-3) {
                Some((_, fill)) => fill.poly(&poly),
                None => {
                    let mut fill = Fill::new();
                    fill.poly(&poly);
                    batches.push((key, fill));
                }
            }
        }
        for ((color, opacity, _), fill) in batches {
            fill.paint(window, color.opacity(opacity));
        }
        // Lifted stones last, so a swelling stone covers its neighbours.
        lifted.sort_by(|a, b| a.0.area().total_cmp(&b.0.area()));
        for (poly, color, hatched, walked) in lifted {
            if hatched {
                let (min, max) = poly.bounds();
                let frame = Bounds::new(point(px(min.x), px(min.y)), size(px(max.x - min.x), px(max.y - min.y)));
                Hatch::vertical(1.5 * s, 3.5 * s).paint(window, &poly, frame, color);
            } else {
                let mut fill = Fill::new();
                fill.poly(&poly);
                fill.paint(window, color);
            }
            if walked {
                let mut ring = Fill::new();
                for quad in poly.offset(1.5 * s).stroke_ring(1.5 * s) {
                    ring.poly(&quad);
                }
                ring.paint(window, Hsla::from(palette.peri_hi));
            }
        }

        let count = shown;
        let pitch = cell.pitch();
        let edge = cell.edge;
        live::paint(
            Hooks {
                mark: self.id.clone(),
                live,
                door: self.door.clone(),
                side: Side::Above,
                count,
                hit: Rc::new(move |p: Point<Pixels>| {
                    cell.at(f32::from(p.x) - x0, f32::from(p.y) - y0, cols, count)
                }),
                anchor: Rc::new(move |i| {
                    (i < count).then(|| {
                        #[allow(clippy::cast_precision_loss)]
                        let (c, r) = ((i % cols) as f32, (i / cols) as f32);
                        Bounds::new(
                            point(px(x0 + c * pitch), px(y0 + r * pitch)),
                            size(px(edge), px(edge)),
                        )
                    })
                }),
                step: live::grid(cols),
            },
            &mut layout.keys,
            hitbox,
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Cell, stone_wave};

    #[test]
    fn grid_arithmetic_finds_every_stone_and_nothing_else() {
        let cell = Cell::BOARD;
        let cols = cell.columns(300.0);
        assert_eq!(cols, 21);
        let count = 2000;
        for i in [0usize, 1, 20, 21, 999, 1999] {
            #[allow(clippy::cast_precision_loss)]
            let (x, y) = (
                (i % cols) as f32 * cell.pitch() + 5.0,
                (i / cols) as f32 * cell.pitch() + 5.0,
            );
            assert_eq!(cell.at(x, y, cols, count), Some(i));
        }
        // Past the last stone, past the last column, before the origin.
        #[allow(clippy::cast_precision_loss)]
        let past = (count / cols + 1) as f32 * cell.pitch() + 1.0;
        assert_eq!(cell.at(1.0, past, cols, count), None);
        #[allow(clippy::cast_precision_loss)]
        let right = cols as f32 * cell.pitch() + 1.0;
        assert_eq!(cell.at(right, 1.0, cols, count), None);
        assert_eq!(cell.at(-1.0, 1.0, cols, count), None);
    }

    #[test]
    fn height_fits_the_rows() {
        let cell = Cell::BOARD;
        assert!((cell.height(21, 21) - 11.0).abs() < 1e-4);
        assert!((cell.height(22, 21) - 25.0).abs() < 1e-4);
        assert!(cell.height(0, 21).abs() < 1e-4);
    }

    #[test]
    fn the_wave_peaks_on_the_stone_and_is_gone_by_two() {
        let near = |d: f32, lift: f32, swell: f32| {
            let (l, s) = stone_wave(d);
            (l - lift).abs() < 1e-5 && (s - swell).abs() < 1e-5
        };
        assert!(near(0.0, 3.0, 1.55));
        assert!(near(1.0, 1.5, 1.0));
        assert!(near(2.0, 0.0, 1.0));
        assert!(near(5.0, 0.0, 1.0));
        // Continuous across the neighbour ring.
        assert!((stone_wave(0.999).0 - stone_wave(1.001).0).abs() < 0.01);
    }
}

//! The territory map: a package as one squarified treemap. A region per
//! module, sized by what it holds; a stone per public item inside it.
//!
//! Calm at rest (gui-plan §6.2): the stones are one quiet ink; only the
//! stones your code reaches are lit (mint); a region's name is a quiet mono
//! label, drawn only where the region can hold it. Resting on a region
//! brightens its outline and opens the module lens; resting on a stone
//! swells it and opens its peek. Under ⌥ x-ray, everything that is not yours
//! fades further, so what you reach stands alone.
//!
//! One custom element paints every region and every stone. The squarified
//! layout is a pure function of the modules and the box. Regions are keyed
//! by module name: when a resize changes the layout's shape (a region moves
//! to another row), each region springs from where it was painted to where
//! it now is (FLIP); a continuous drag that only stretches the rows tracks
//! directly.

use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use super::spatial::{self, Aabb, Grid};
use super::text::shape_fit;
use crate::measure::Measure;
use crate::motion::{Motion, spec};
use crate::paint::geom::{Fill, Poly};
use crate::theme::ActiveFacet;
use crate::tokens::{TypeRole, ty};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, SharedString, Style, Window, point,
    px, size,
};
use std::collections::HashMap;
use std::rc::Rc;

/// One module on the map.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// The module path ("de::value"): its key and its label.
    pub name: SharedString,
    /// Public items (stones), in the order they are drawn.
    pub items: usize,
    /// Which of them your code reaches (indices into the items).
    pub reached: Vec<usize>,
    /// Private or doc-hidden: drawn quieter.
    pub private: bool,
}

impl Region {
    /// A region of `items` stones, none reached.
    #[must_use]
    pub fn new(name: impl Into<SharedString>, items: usize) -> Self {
        Self {
            name: name.into(),
            items,
            reached: Vec::new(),
            private: false,
        }
    }

    /// The stones your code reaches.
    #[must_use]
    pub fn reached(mut self, reached: impl Into<Vec<usize>>) -> Self {
        self.reached = reached.into();
        self
    }

    /// Private or doc-hidden.
    #[must_use]
    pub const fn private(mut self) -> Self {
        self.private = true;
        self
    }
}

/// A laid-out rectangle, relative to the map's origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

/// Squarified treemap (Bruls, Huizing, van Wijk): `values` in any order,
/// one rectangle per value in the same order, tiling `w × h` exactly.
#[must_use]
#[allow(clippy::many_single_char_names)]
pub fn squarify(values: &[f32], w: f32, h: f32) -> Vec<Rect> {
    let mut out = vec![
        Rect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: 0.0,
        };
        values.len()
    ];
    let total: f32 = values.iter().map(|v| v.max(0.0)).sum();
    if total <= 0.0 || w <= 0.0 || h <= 0.0 {
        return out;
    }
    let scale = w * h / total;
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|a, b| values[*b].total_cmp(&values[*a]));
    let area = |i: usize| values[i].max(0.0) * scale;
    let worst = |row: &[usize], side: f32| {
        let sum: f32 = row.iter().map(|i| area(*i)).sum();
        row.iter()
            .map(|i| {
                let a = area(*i).max(1e-6);
                (side * side * a / (sum * sum)).max(sum * sum / (side * side * a))
            })
            .fold(0.0_f32, f32::max)
    };
    let (mut x, mut y, mut w, mut h) = (0.0_f32, 0.0_f32, w, h);
    let mut row: Vec<usize> = Vec::new();
    let mut rest = order.into_iter().peekable();
    let lay = |row: &[usize], x: &mut f32, y: &mut f32, w: &mut f32, h: &mut f32, out: &mut Vec<Rect>| {
        let sum: f32 = row.iter().map(|i| area(*i)).sum();
        if *w >= *h {
            let cw = if *h > 0.0 { sum / *h } else { 0.0 };
            let mut cy = *y;
            for i in row {
                let ch = if cw > 0.0 { area(*i) / cw } else { 0.0 };
                out[*i] = Rect { x: *x, y: cy, w: cw, h: ch };
                cy += ch;
            }
            *x += cw;
            *w -= cw;
        } else {
            let ch = if *w > 0.0 { sum / *w } else { 0.0 };
            let mut cx = *x;
            for i in row {
                let cw = if ch > 0.0 { area(*i) / ch } else { 0.0 };
                out[*i] = Rect { x: cx, y: *y, w: cw, h: ch };
                cx += cw;
            }
            *y += ch;
            *h -= ch;
        }
    };
    while let Some(&next) = rest.peek() {
        let side = w.min(h);
        let mut with = row.clone();
        with.push(next);
        if row.is_empty() || worst(&with, side) <= worst(&row, side) {
            row.push(next);
            rest.next();
        } else {
            lay(&row, &mut x, &mut y, &mut w, &mut h, &mut out);
            row.clear();
        }
    }
    if !row.is_empty() {
        lay(&row, &mut x, &mut y, &mut w, &mut h, &mut out);
    }
    out
}

/// Stone pitch and edge, px at 100 %.
const PITCH: f32 = 15.5;
const STONE: f32 = 10.0;
const GAP: f32 = 3.0;
const PAD: f32 = 8.0;
const LABEL: TypeRole = TypeRole {
    weight: 500.0,
    size: 11.5,
    line: 15.0,
    ..ty::MONO_SMALL
};

/// What the pointer is on: a region, or one of its stones.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Spot {
    /// A module region (its index).
    Region(usize),
    /// A stone: `(region, item)`.
    Stone(usize, usize),
}

/// The map. Build with [`territory`].
pub struct Territory {
    id: ElementId,
    regions: Rc<[Region]>,
    measure: Measure,
    region_door: Option<Door>,
    stone_door: Option<Door>,
    rest: Option<Spot>,
    canvas: Option<f32>,
}

/// A territory map of `regions` as wide as `measure` gives it; its height
/// follows from how many stones there are.
#[must_use]
pub fn territory(id: impl Into<ElementId>, regions: impl Into<Rc<[Region]>>, measure: &Measure) -> Territory {
    Territory {
        id: id.into(),
        regions: regions.into(),
        measure: *measure,
        region_door: None,
        stone_door: None,
        rest: None,
        canvas: None,
    }
}

impl Territory {
    /// Regions open through `door` (part = region index): the module lens.
    #[must_use]
    pub fn region_door(mut self, door: Door) -> Self {
        self.region_door = Some(door);
        self
    }

    /// Stones open through `door` (part = [`Territory::stone_part`]): the peek.
    #[must_use]
    pub fn stone_door(mut self, door: Door) -> Self {
        self.stone_door = Some(door);
        self
    }

    /// Shows a spot as rested (scenes; a live pointer overrides it).
    #[must_use]
    pub const fn rest(mut self, spot: Option<Spot>) -> Self {
        self.rest = spot;
        self
    }

    /// Lays the map out on a canvas this tall (px at 100 %) instead of the
    /// height its stones need: a map larger than the window, of which the
    /// window shows a part (the graph view's substrate).
    #[must_use]
    pub const fn canvas(mut self, height: f32) -> Self {
        self.canvas = Some(height);
        self
    }

    /// The part index a stone reports: regions come first, then every
    /// stone in reading order across regions.
    #[must_use]
    pub fn stone_part(regions: &[Region], region: usize, item: usize) -> usize {
        regions.len() + regions.iter().take(region).map(|r| r.items).sum::<usize>() + item
    }

    /// The spot a part index names.
    #[must_use]
    pub fn spot(regions: &[Region], part: usize) -> Option<Spot> {
        if part < regions.len() {
            return Some(Spot::Region(part));
        }
        let mut at = part - regions.len();
        for (i, region) in regions.iter().enumerate() {
            if at < region.items {
                return Some(Spot::Stone(i, at));
            }
            at -= region.items;
        }
        None
    }

    fn height(&self) -> f32 {
        let s = self.measure.scale();
        if let Some(h) = self.canvas {
            return h * s;
        }
        let w = f32::from(self.measure.width()).max(1.0);
        #[allow(clippy::cast_precision_loss)]
        let stones: f32 = self.regions.iter().map(|r| r.items as f32).sum();
        let area = stones * (PITCH * s) * (PITCH * s) * 1.25;
        (area / w).max(260.0 * s)
    }
}

impl IntoElement for Territory {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// Where each region was last laid out (FLIP memory), keyed by name.
#[derive(Default)]
pub struct Flip {
    targets: HashMap<SharedString, Rect>,
    /// The squarified layout and its spatial index, rebuilt only when the
    /// box or the data changes.
    cache: Option<(LayoutKey, Rc<Vec<Rect>>, Rc<Grid>)>,
}

/// What a cached layout depends on.
#[derive(Clone, Copy, Debug, PartialEq)]
struct LayoutKey {
    w: f32,
    h: f32,
    regions: usize,
    items: usize,
    data: usize,
}

#[doc(hidden)]
pub struct TerritoryLayout {
    live: Entity<Live>,
    keys: AnyElement,
    flip: Entity<Flip>,
}

/// A laid-out region, window coordinates, painted where its FLIP puts it.
#[derive(Clone, Copy, Debug)]
struct Placed {
    rect: Rect,
    label: bool,
    columns: usize,
    rows: usize,
    top: f32,
}

fn place(rect: Rect, s: f32) -> Placed {
    let label = rect.w > 70.0 * s && rect.h > 44.0 * s;
    let top = if label { PAD * s + LABEL.line * s + 6.0 * s } else { PAD * s };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let columns = (((rect.w - PAD * 2.0 * s) + (PITCH - STONE) * s) / (PITCH * s)).floor().max(0.0) as usize;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rows = (((rect.h - top - PAD * s) + (PITCH - STONE) * s) / (PITCH * s)).floor().max(0.0) as usize;
    Placed {
        rect,
        label,
        columns,
        rows,
        top,
    }
}

/// The spot under `p` (relative to the map), by arithmetic per region.
fn spot_at(placed: &[Placed], grid: &Grid, regions: &[Region], x: f32, y: f32, s: f32) -> Option<Spot> {
    // The region: one bucket of the spatial index, not a scan.
    let i = grid.at(x, y)?;
    let r = &placed[i];
    let (lx, ly) = (x - r.rect.x - PAD * s, y - r.rect.y - r.top);
    if lx >= 0.0 && ly >= 0.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (c, row) = ((lx / (PITCH * s)) as usize, (ly / (PITCH * s)) as usize);
        let within = lx - c as f32 * PITCH * s <= STONE * s + 1.0 && ly - row as f32 * PITCH * s <= STONE * s + 1.0;
        if within && c < r.columns && row < r.rows {
            let item = row * r.columns + c;
            if item < regions[i].items {
                return Some(Spot::Stone(i, item));
            }
        }
    }
    Some(Spot::Region(i))
}

impl Element for Territory {
    type RequestLayoutState = TerritoryLayout;
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
    ) -> (LayoutId, TerritoryLayout) {
        let live = live::live(window, cx);
        let (keys, kid) = live::keys(&live, window, cx);
        let flip = window.use_keyed_state("flip", cx, |_, _| Flip::default());
        let mut style = Style::default();
        style.size.width = px(f32::from(self.measure.width())).into();
        style.size.height = px(self.height()).into();
        style.flex_shrink = 0.0;
        (
            window.request_layout(style, [kid], cx),
            TerritoryLayout { live, keys, flip },
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut TerritoryLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        live::prepaint(bounds, &mut layout.keys, window, cx)
    }

    #[allow(clippy::too_many_lines)]
    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut TerritoryLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        let live = layout.live.clone();
        let motion = live.read(cx).motion.clone();
        let regions = self.regions.clone();

        let (targets, grid) = cached_layout(&layout.flip, &regions, w, h, cx);
        let (rects, moving) = flip(&self.id, &layout.flip, &motion, &regions, &targets, s, window, cx);
        let placed: Rc<Vec<Placed>> = Rc::new(rects.iter().map(|r| place(*r, s)).collect());
        // What the window can show, in the map's own coordinates. While a
        // FLIP runs, regions travel, so every region is drawn.
        let view = spatial::visible(bounds, window);
        let view = Aabb {
            x0: view.x0 - ox,
            y0: view.y0 - oy,
            x1: view.x1 - ox,
            y1: view.y1 - oy,
        };
        let shown_regions: Vec<usize> = if view.x1 <= view.x0 || view.y1 <= view.y0 {
            Vec::new()
        } else if moving {
            (0..regions.len()).collect()
        } else {
            grid.query(&view)
        };
        spatial::count(cx, |p| p.regions += shown_regions.len() as u64);

        let spot_of = |part: Option<usize>| part.and_then(|p| Territory::spot(&regions, p));
        let hover = spot_of(live.read(cx).hover).or(self.rest);
        let walk = spot_of(live::walking(&live, window, cx));
        let dim = motion.animate(
            live::key(&self.id, "dim"),
            if self.measure.reveal().xray { 1.0 } else { 0.0 },
            spec::REVEAL,
            window,
            cx,
        );
        let lit_region = match hover.or(walk) {
            Some(Spot::Region(i) | Spot::Stone(i, _)) => Some(i),
            None => None,
        };

        // Regions: a whisper of fill and a hairline; the rested one brightens.
        let mut fills = Fill::new();
        let mut lines = Fill::new();
        let mut bright = Fill::new();
        for &i in &shown_regions {
            let r = &placed[i];
            let rw = (r.rect.w - GAP * s).max(0.0);
            let rh = (r.rect.h - GAP * s).max(0.0);
            let outline = Poly::rect(ox + r.rect.x, oy + r.rect.y, rw, rh);
            fills.poly(&outline);
            let ring = outline.offset(-0.5).stroke_ring(1.0);
            for q in ring {
                if lit_region == Some(i) {
                    bright.poly(&q);
                } else {
                    lines.poly(&q);
                }
            }
        }
        fills.paint(window, Hsla::from(palette.tint).opacity(0.3));
        lines.paint(window, Hsla::from(palette.line1));
        bright.paint(window, Hsla::from(palette.line3));

        // Stones: one quiet batch, one mint batch, the rested stone alone.
        let quiet: Hsla = Hsla::from(palette.ground).opacity(0.10 * (1.0 - 0.6 * dim));
        let hidden: Hsla = Hsla::from(palette.ground).opacity(0.06 * (1.0 - 0.6 * dim));
        let mint: Hsla = Hsla::from(palette.mint.base).opacity(0.85);
        let mut stones = Fill::new();
        let mut private = Fill::new();
        let mut reached = Fill::new();
        let mut single: Option<(Poly, Hsla)> = None;
        let mut drawn = 0_u64;
        for &i in &shown_regions {
            let (r, region) = (&placed[i], &regions[i]);
            let shown = region.items.min(r.columns * r.rows);
            // Only the rows inside the view.
            let pitch = PITCH * s;
            let top = r.rect.y + r.top;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let row0 = ((view.y0 - top) / pitch).floor().max(0.0) as usize;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let row1 = ((view.y1 - top) / pitch).ceil().max(0.0) as usize;
            let (first, last) = ((row0 * r.columns).min(shown), (row1 * r.columns).min(shown));
            drawn += (last - first) as u64;
            let reach: std::collections::HashSet<usize> = region.reached.iter().copied().collect();
            for item in first..last {
                #[allow(clippy::cast_precision_loss)]
                let (c, row) = ((item % r.columns) as f32, (item / r.columns) as f32);
                let (sx, sy) = (
                    ox + r.rect.x + PAD * s + c * PITCH * s,
                    oy + r.rect.y + r.top + row * PITCH * s,
                );
                let rested = matches!(hover, Some(Spot::Stone(a, b)) if a == i && b == item)
                    || matches!(walk, Some(Spot::Stone(a, b)) if a == i && b == item);
                if rested {
                    let e = STONE * s * 1.4;
                    let poly = Poly::rect(sx - (e - STONE * s) * 0.5, sy - (e - STONE * s) * 0.5, e, e);
                    let ink = if reach.contains(&item) { palette.mint.base } else { palette.ink2 };
                    single = Some((poly, ink.into()));
                    continue;
                }
                let poly = Poly::rect(sx, sy, STONE * s, STONE * s);
                if reach.contains(&item) {
                    reached.poly(&poly);
                } else if region.private {
                    private.poly(&poly);
                } else {
                    stones.poly(&poly);
                }
            }
        }
        spatial::count(cx, |p| p.stones += drawn);
        stones.paint(window, quiet);
        private.paint(window, hidden);
        reached.paint(window, mint);
        if let Some((poly, ink)) = single {
            let mut fill = Fill::new();
            fill.poly(&poly);
            fill.paint(window, ink);
        }

        // Labels where they fit.
        for &i in &shown_regions {
            let (r, region) = (&placed[i], &regions[i]);
            if !r.label {
                continue;
            }
            let role = self.measure.role(LABEL);
            let label = shape_fit(
                &region.name,
                role,
                palette.ink2.into(),
                r.rect.w - GAP * s - PAD * 2.0 * s,
                window,
            );
            label.paint(ox + r.rect.x + PAD * s, oy + r.rect.y + PAD * s + label.ascent(), window, cx);
        }

        // Doors: regions and stones share one mark, so one lens at a time.
        let count = Territory::stone_part(&regions, regions.len(), 0);
        let door = TerritoryDoor {
            region: self.region_door.clone(),
            stone: self.stone_door.clone(),
            regions: regions.clone(),
        };
        let hit_placed = placed.clone();
        let hit_grid = grid.clone();
        let hit_regions = regions.clone();
        let anchor_regions = regions.clone();
        live::paint(
            Hooks {
                mark: self.id.clone(),
                live,
                door: door.merged(),
                side: Side::Right,
                count,
                hit: Rc::new(move |p: Point<Pixels>| {
                    let spot = spot_at(
                        &hit_placed,
                        &hit_grid,
                        &hit_regions,
                        f32::from(p.x) - ox,
                        f32::from(p.y) - oy,
                        s,
                    )?;
                    Some(match spot {
                        Spot::Region(i) => i,
                        Spot::Stone(i, item) => Territory::stone_part(&hit_regions, i, item),
                    })
                }),
                anchor: Rc::new(move |part| {
                    Some(match Territory::spot(&anchor_regions, part)? {
                        Spot::Region(i) => {
                            let r = placed.get(i)?;
                            Bounds::new(
                                point(px(ox + r.rect.x), px(oy + r.rect.y)),
                                size(px(r.rect.w - GAP * s), px(r.rect.h - GAP * s)),
                            )
                        }
                        Spot::Stone(i, item) => {
                            let r = placed.get(i)?;
                            let cols = r.columns.max(1);
                            #[allow(clippy::cast_precision_loss)]
                            let (c, row) = ((item % cols) as f32, (item / cols) as f32);
                            Bounds::new(
                                point(
                                    px(ox + r.rect.x + PAD * s + c * PITCH * s),
                                    px(oy + r.rect.y + r.top + row * PITCH * s),
                                ),
                                size(px(STONE * s), px(STONE * s)),
                            )
                        }
                    })
                }),
                step: Rc::new(live::linear),
            },
            &mut layout.keys,
            hitbox,
            window,
            cx,
        );
    }
}

/// Region and stone doors behind one [`Door`] (the part index says which).
struct TerritoryDoor {
    region: Option<Door>,
    stone: Option<Door>,
    regions: Rc<[Region]>,
}

impl TerritoryDoor {
    fn merged(self) -> Option<Door> {
        if self.region.is_none() && self.stone.is_none() {
            return None;
        }
        let Self {
            region,
            stone,
            regions,
        } = self;
        let opens = region.as_ref().or(stone.as_ref()).map(Door::opens);
        let build = move |part: usize, m: &Measure, w: &mut Window, cx: &mut App| -> AnyElement {
            match Territory::spot(&regions, part) {
                Some(Spot::Region(i)) => region.as_ref().map(|d| d.build(i, m, w, cx)),
                Some(Spot::Stone(..)) => stone.as_ref().map(|d| d.build(part, m, w, cx)),
                None => None,
            }
            .unwrap_or_else(|| IntoElement::into_any_element(gpui::Empty))
        };
        Some(match opens {
            Some(super::door::Opens::Peek) => Door::peek(build),
            Some(super::door::Opens::Tip) => Door::tip(build),
            _ => Door::lens(build),
        })
    }
}

/// The squarified layout for `w × h` and its spatial index, from the cache
/// when nothing it depends on changed.
fn cached_layout(memory: &Entity<Flip>, regions: &Rc<[Region]>, w: f32, h: f32, cx: &mut App) -> (Rc<Vec<Rect>>, Rc<Grid>) {
    let key = LayoutKey {
        w,
        h,
        regions: regions.len(),
        items: regions.iter().map(|r| r.items).sum(),
        data: Rc::as_ptr(regions).cast::<u8>() as usize,
    };
    if let Some((k, targets, grid)) = &memory.read(cx).cache
        && *k == key
    {
        return (targets.clone(), grid.clone());
    }
    #[allow(clippy::cast_precision_loss)]
    let values: Vec<f32> = regions.iter().map(|r| r.items as f32).collect();
    let targets = Rc::new(squarify(&values, w, h));
    let boxes: Vec<Aabb> = targets.iter().map(|r| Aabb::new(r.x, r.y, r.w, r.h)).collect();
    let grid = Rc::new(Grid::build(&boxes, 0.0));
    memory.update(cx, |flip, _| flip.cache = Some((key, targets.clone(), grid.clone())));
    (targets, grid)
}

/// FLIP: each region's painted rect, and whether any region is travelling.
/// A rect that jumps (the squarified layout changed shape) springs from where
/// it was painted; a rect that moves a little (a continuous drag) tracks
/// directly. At rest this touches no motion track at all, so a map of many
/// regions costs nothing extra per frame.
#[allow(clippy::too_many_arguments)]
fn flip(
    mark: &ElementId,
    memory: &Entity<Flip>,
    motion: &Motion,
    regions: &[Region],
    targets: &[Rect],
    s: f32,
    window: &mut Window,
    cx: &mut App,
) -> (Vec<Rect>, bool) {
    const JUMP: f32 = 24.0;
    let key = |name: &SharedString, channel: &'static str| {
        ElementId::NamedChild(
            std::sync::Arc::new(ElementId::NamedChild(std::sync::Arc::new(mark.clone()), name.clone())),
            SharedString::new_static(channel),
        )
    };
    let previous: Vec<Option<Rect>> = {
        let flip = memory.read(cx);
        regions.iter().map(|r| flip.targets.get(&r.name).copied()).collect()
    };
    let jumped: Vec<bool> = targets
        .iter()
        .zip(&previous)
        .map(|(t, p)| {
            p.is_some_and(|p| {
                (p.x - t.x).abs() > JUMP * s
                    || (p.y - t.y).abs() > JUMP * s
                    || (p.w - t.w).abs() > JUMP * s
                    || (p.h - t.h).abs() > JUMP * s
            })
        })
        .collect();
    let travelling = motion.is_live(cx) && memory.read(cx).targets.len() == regions.len();
    let out = if jumped.iter().any(|j| *j) || travelling {
        let mut out = Vec::with_capacity(targets.len());
        for ((region, target), (prev, jump)) in regions.iter().zip(targets).zip(previous.iter().zip(&jumped)) {
            let mut rect = [0.0_f32; 4];
            let from = prev.unwrap_or(*target);
            for (k, (channel, value, was)) in [
                ("x", target.x, from.x),
                ("y", target.y, from.y),
                ("w", target.w, from.w),
                ("h", target.h, from.h),
            ]
            .into_iter()
            .enumerate()
            {
                if *jump && !travelling {
                    // Start from where it was painted.
                    motion.set(key(&region.name, channel), was);
                }
                rect[k] = motion.animate(key(&region.name, channel), value, spec::SETTLE, window, cx);
            }
            out.push(Rect {
                x: rect[0],
                y: rect[1],
                w: rect[2],
                h: rect[3],
            });
        }
        out
    } else {
        targets.to_vec()
    };
    memory.update(cx, |flip, _| {
        flip.targets = regions
            .iter()
            .zip(targets)
            .map(|(r, t)| (r.name.clone(), *t))
            .collect();
    });
    let moving = out.iter().zip(targets).any(|(a, b)| (a.x - b.x).abs() + (a.y - b.y).abs() > 0.5);
    (out, moving)
}

#[cfg(test)]
mod tests {
    use super::{Placed, Rect, Region, Spot, Territory, place, spot_at, squarify};
    use crate::data::spatial::{Aabb, Grid};

    #[test]
    fn squarify_tiles_the_box_exactly_and_keeps_proportions() {
        let values = [96.0, 72.0, 58.0, 44.0, 38.0, 9.0, 12.0, 41.0, 6.0, 7.0];
        let (w, h) = (780.0, 420.0);
        let rects = squarify(&values, w, h);
        let total: f32 = values.iter().sum();
        let covered: f32 = rects.iter().map(|r| r.w * r.h).sum();
        assert!((covered - w * h).abs() < 1.0, "{covered} vs {}", w * h);
        for (v, r) in values.iter().zip(&rects) {
            let share = r.w * r.h / (w * h);
            assert!((share - v / total).abs() < 1e-3, "{v}: {share}");
            assert!(r.x >= -0.01 && r.y >= -0.01 && r.x + r.w <= w + 0.01 && r.y + r.h <= h + 0.01);
        }
        // No two regions overlap.
        for (i, a) in rects.iter().enumerate() {
            for b in rects.iter().skip(i + 1) {
                let ox = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
                let oy = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
                assert!(ox <= 0.01 || oy <= 0.01, "{a:?} overlaps {b:?}");
            }
        }
        // Squarified: no sliver worse than 1:6 for this data.
        for r in &rects {
            let ratio = (r.w / r.h).max(r.h / r.w);
            assert!(ratio < 6.0, "{r:?}");
        }
    }

    #[test]
    fn parts_name_regions_first_then_every_stone() {
        let regions = vec![Region::new("a", 3), Region::new("b", 2)];
        assert_eq!(Territory::spot(&regions, 0), Some(Spot::Region(0)));
        assert_eq!(Territory::spot(&regions, 1), Some(Spot::Region(1)));
        assert_eq!(Territory::spot(&regions, 2), Some(Spot::Stone(0, 0)));
        assert_eq!(Territory::spot(&regions, 4), Some(Spot::Stone(0, 2)));
        assert_eq!(Territory::spot(&regions, 5), Some(Spot::Stone(1, 0)));
        assert_eq!(Territory::spot(&regions, 7), None);
        for (i, region) in regions.iter().enumerate() {
            for item in 0..region.items {
                let part = Territory::stone_part(&regions, i, item);
                assert_eq!(Territory::spot(&regions, part), Some(Spot::Stone(i, item)));
            }
        }
    }

    #[test]
    fn the_pointer_finds_a_stone_or_falls_back_to_its_region() {
        let regions = vec![Region::new("de", 30)];
        let placed: Vec<Placed> = vec![place(Rect { x: 0.0, y: 0.0, w: 200.0, h: 200.0 }, 1.0)];
        let grid = Grid::build(&[Aabb::new(0.0, 0.0, 200.0, 200.0)], 0.0);
        let p = placed[0];
        assert!(p.label);
        // The first stone's centre.
        assert_eq!(spot_at(&placed, &grid, &regions, 8.0 + 5.0, p.top + 5.0, 1.0), Some(Spot::Stone(0, 0)));
        // The next row's first stone.
        assert_eq!(
            spot_at(&placed, &grid, &regions, 8.0 + 5.0, p.top + 15.5 + 5.0, 1.0),
            Some(Spot::Stone(0, p.columns))
        );
        // The label and the padding are the region.
        assert_eq!(spot_at(&placed, &grid, &regions, 20.0, 4.0, 1.0), Some(Spot::Region(0)));
        // Past the last stone is the region too.
        assert_eq!(spot_at(&placed, &grid, &regions, 190.0, 190.0, 1.0), Some(Spot::Region(0)));
        assert_eq!(spot_at(&placed, &grid, &regions, 250.0, 10.0, 1.0), None);
    }
}

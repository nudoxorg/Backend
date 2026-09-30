//! The shingles: a package's territory, one region per module and one
//! chamfered shingle per public name, in the hue of what the name is (types
//! teal, callables periwinkle, contracts violet, values neutral). Mint is
//! yours: the names your code reaches. The map is the intro of a package's
//! page: every name at a glance, and each shingle is a real, clickable item.
//!
//! - **Layout.** Justified rows of regions in the order given: a region
//!   holds its full label (wrapped inside the map at narrow widths) and its
//!   shingles on one grid; oversized regions fit the map rather than leaking
//!   past its clip. A row's regions share its height and the row is stretched
//!   to the full width in proportion to what they hold. The height follows from
//!   the rows. The shingle size follows the container ([`Measure::fluid`]),
//!   so a 2560 px window has larger, not lonelier, shingles.
//! - **Hover.** A region brightens its outline and reads itself in the foot
//!   (its own words, what it holds, what you use); a shingle swells and
//!   wears its name on a small plate. All of it inside the map.
//! - **Click** a region (or a shingle in it) to open the module; **the
//!   keyboard** is the host's (one system: every region is a target it walks
//!   with `j`/`k`), and the region it stands on is rested on with [`Shingles::rest`].
//! - **The past.** A shingle whose name changed, went or is not yet there at
//!   the release being read wears amber, coral or a ghost, and the regions
//!   take the warmer tint.

use super::cards::Change;
use super::flight::{Carrying, Stone};
use super::state::{Extent, Time, Use};
use crate::data::text::{font, shape, shape_fit};
use crate::measure::Measure;
use crate::paint::geom::{Fill, Poly};
use crate::probe::{self, TextOverflow, TextSample};
use crate::theme::ActiveFacet;
use crate::tokens::{Family, TypeRole, ty};
use gpui::{
    App, Bounds, DispatchPhase, Element, ElementId, Entity, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, Pixels, SharedString, Style, TextAlign, TextRun, Window, px,
};
use std::rc::Rc;

/// One shingle: one public name.
#[derive(Clone, Debug, PartialEq)]
pub struct ShingleFacts {
    /// The name.
    pub name: SharedString,
    /// What it is, which picks the hue.
    pub family: Family,
    /// Your code reaches it.
    pub yours: Use,
    /// What the release being read did to it.
    pub state: Option<Change>,
}

/// One module: a region of shingles.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleFacts {
    /// The module's path (`sync::mpsc`).
    pub name: SharedString,
    /// Its own first sentence, when it has one.
    pub doc: Option<SharedString>,
    /// Its public names.
    pub shingles: Vec<ShingleFacts>,
    /// It has a page of its own (rather than unfurling inline).
    pub extent: Extent,
}

impl ModuleFacts {
    /// A module of `shingles`.
    #[must_use]
    pub fn new(name: impl Into<SharedString>, shingles: Vec<ShingleFacts>) -> Self {
        Self {
            name: name.into(),
            doc: None,
            shingles,
            extent: Extent::Inline,
        }
    }
}

/// Shingle edge, pitch, region padding, label band and gap, px at 100 %.
pub const STONE: f32 = 10.0;
/// The shingle grid's pitch.
pub const PITCH: f32 = 14.0;
const PAD: f32 = 8.0;
const LABEL_BAND: f32 = 20.0;
/// The widest a region grows, in shingle columns: past this it grows in rows.
const MAX_COLUMNS: usize = 30;
const GAP: f32 = 3.0;
/// The mono advance of the label face, in em.
const ADVANCE: f32 = 0.6;
const LABEL: TypeRole = TypeRole {
    weight: 500.0,
    size: 11.5,
    line: 15.0,
    ..ty::MONO_SMALL
};
const PLATE: TypeRole = TypeRole {
    weight: 500.0,
    size: 12.0,
    line: 16.0,
    ..ty::MONO_SMALL
};
const FOOT_NAME: TypeRole = TypeRole {
    weight: 600.0,
    size: 12.5,
    line: 18.0,
    ..ty::MONO_SMALL
};
const FOOT_DOC: TypeRole = TypeRole {
    size: 13.5,
    line: 18.0,
    ..ty::CAPTION
};
const FOOT_MORE: TypeRole = TypeRole {
    size: 12.0,
    line: 16.0,
    ..ty::SMALL
};

/// A region laid out: its rect and where its shingles sit.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    /// The region's rect, relative to the map.
    pub rect: (f32, f32, f32, f32),
    /// Shingle columns.
    pub columns: usize,
    /// Wrapped label lines in this region.
    pub label_lines: usize,
    /// The first shingle's origin, relative to the map.
    pub origin: (f32, f32),
}

/// The laid-out map.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The width it was laid out for.
    pub width: f32,
    /// Its height (regions only; the foot is added by the element).
    pub height: f32,
    /// One region per module, in the order given.
    pub regions: Vec<Region>,
    /// The scale the geometry was made at (text scale × fluid growth).
    pub k: f32,
}

impl Layout {
    /// The native measured target bounds for each region, in map-local coordinates.
    #[must_use]
    pub fn region_bounds(&self) -> Vec<Bounds<Pixels>> {
        self.regions
            .iter()
            .map(|region| {
                Bounds::new(
                    gpui::point(px(region.rect.0), px(region.rect.1)),
                    gpui::size(px(region.rect.2), px(region.rect.3)),
                )
            })
            .collect()
    }
}

/// Lays modules (`label characters`, `shingles`) out in `width` px at scale
/// `k`. Production layout uses the same row solver with text-system measured
/// label widths and wrap counts.
#[must_use]
pub fn layout(modules: &[(usize, usize)], width: f32, k: f32) -> Layout {
    layout_sized(modules, None, None, LABEL.line * k, width, k)
}

fn layout_sized(
    modules: &[(usize, usize)],
    label_widths: Option<&[f32]>,
    labels: Option<(&[SharedString], TypeRole, &Window)>,
    label_line_height: f32,
    width: f32,
    k: f32,
) -> Layout {
    let width = width.max(0.0);
    let (pitch, stone) = (PITCH * k, STONE * k);
    let pad = (PAD * k).min((width - stone).max(0.0) * 0.5);
    let gap = (GAP * k).min(width);
    let char_w = LABEL.size * ADVANCE * k * 1.02;
    #[allow(clippy::cast_precision_loss)]
    let want: Vec<f32> = modules
        .iter()
        .enumerate()
        .map(|(index, &(chars, items))| {
            let cols = items.div_ceil(3).clamp(1, MAX_COLUMNS);
            let label = label_widths.map_or(chars as f32 * char_w, |widths| widths[index]);
            ((label).max(cols as f32 * pitch - (pitch - stone)) + 2.0 * pad).min(width)
        })
        .collect();
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut used = 0.0_f32;
    for (i, w) in want.iter().enumerate() {
        if !current.is_empty() && used + w + gap > width {
            rows.push(std::mem::take(&mut current));
            used = 0.0;
        }
        used += w + if current.is_empty() { 0.0 } else { gap };
        current.push(i);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    let mut regions = vec![
        Region {
            rect: (0.0, 0.0, 0.0, 0.0),
            columns: 1,
            label_lines: 1,
            origin: (0.0, 0.0),
        };
        modules.len()
    ];
    let mut y = 0.0_f32;
    let last = rows.len().saturating_sub(1);
    for (row_index, row) in rows.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let base: f32 = row.iter().map(|&i| want[i]).sum::<f32>() + gap * (row.len() as f32 - 1.0);
        let slack = if row_index == last && base < width * 0.7 {
            0.0
        } else {
            (width - base).max(0.0)
        };
        #[allow(clippy::cast_precision_loss)]
        let weight: f32 = row.iter().map(|&i| modules[i].1 as f32).sum::<f32>().max(1.0);
        let mut x = 0.0_f32;
        let mut placed = Vec::new();
        let mut tallest = 0usize;
        for &i in row {
            // Spare room widens a region, but a big module grows down, not
            // into one long strip.
            #[allow(clippy::cast_precision_loss)]
            let w = (want[i] + slack * modules[i].1 as f32 / weight).min((width - x).max(0.0));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let columns = (((w - 2.0 * pad + (pitch - stone)).max(0.0) / pitch).floor() as usize).max(1);
            let lines = modules[i].1.div_ceil(columns);
            let label_lines = if let Some((labels, role, window)) = labels {
                wrapped_line_count(&labels[i], role, (w - 2.0 * pad).max(1.0), window)
            } else {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                ((label_widths.map_or(modules[i].0 as f32 * char_w, |widths| widths[i]) / (w - 2.0 * pad).max(1.0)).ceil() as usize).max(1)
            };
            tallest = tallest.max(lines);
            placed.push((i, x, w, columns, label_lines));
            x += w + gap;
        }
        #[allow(clippy::cast_precision_loss)]
        let label_band = (LABEL_BAND * k).max(
            placed
                .iter()
                .map(|(_, _, _, _, lines)| *lines as f32 * label_line_height + 4.0 * k)
                .fold(0.0, f32::max),
        );
        #[allow(clippy::cast_precision_loss)]
        let h = label_band + 2.0 * pad + tallest.max(1) as f32 * pitch - (pitch - stone);
        for (i, x, w, columns, label_lines) in placed {
            regions[i] = Region {
                rect: (x, y, w, h),
                columns,
                label_lines,
                origin: (x + pad, y + pad + label_band),
            };
        }
        y += h + gap;
    }
    Layout {
        width,
        height: (y - gap).max(0.0),
        regions,
        k,
    }
}

fn wrap_label(text: SharedString, role: TypeRole, ink: Hsla, width: f32, window: &Window) -> Vec<gpui::WrappedLine> {
    let run = TextRun {
        len: text.len(),
        font: font(role),
        color: ink,
        background_color: None,
        underline: None,
        strikethrough: None,
        letter_spacing: (role.tracking.abs() > f32::EPSILON).then(|| px(role.tracking * role.size)),
    };
    window
        .text_system()
        .shape_text(text, px(role.size), &[run], Some(px(width.max(1.0))), None)
        .unwrap_or_default()
        .into_iter()
        .collect()
}

fn paint_wrapped(text: SharedString, role: TypeRole, ink: Hsla, width: f32, x: f32, y: f32, window: &mut Window, cx: &mut App) -> (f32, f32) {
    let natural = shape(text.clone(), role, ink, window).width();
    let wrapped = wrap_label(text, role, ink, width, window);
    let mut cursor = y;
    for line in &wrapped {
        line.paint(gpui::point(px(x), px(cursor)), px(role.line), TextAlign::Left, None, window, cx)
            .ok();
        #[allow(clippy::cast_precision_loss)]
        {
            cursor += (1 + line.wrap_boundaries().len()) as f32 * role.line;
        }
    }
    (natural, (cursor - y).max(role.line))
}

fn wrapped_line_count(text: &str, role: TypeRole, width: f32, window: &Window) -> usize {
    wrap_label(text.to_owned().into(), role, Hsla::default(), width, window)
        .iter()
        .map(|line| 1 + line.wrap_boundaries().len())
        .sum::<usize>()
        .max(1)
}

/// Measures one immutable map layout for both its native paint element and
/// the host's keyboard targets.
#[must_use]
pub fn measured(modules: &[ModuleFacts], measure: &Measure, window: &Window) -> Rc<Layout> {
    let k = measure.scale() * (0.94 + 0.42 * measure.t());
    let sizes: Vec<(usize, usize)> = modules.iter().map(|module| (module.name.chars().count(), module.shingles.len())).collect();
    let role = scaled(measure.role(LABEL), k / measure.scale());
    let widths: Vec<f32> = modules
        .iter()
        .map(|module| shape(module.name.clone(), role, Hsla::default(), window).width())
        .collect();
    let labels: Vec<SharedString> = modules.iter().map(|module| module.name.clone()).collect();
    Rc::new(layout_sized(
        &sizes,
        Some(&widths),
        Some((&labels, role, window)),
        role.line,
        f32::from(measure.width()),
        k,
    ))
}

/// What the pointer or the walk is on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Spot {
    /// A region (module index).
    Region(usize),
    /// A shingle: `(module, shingle)`.
    Shingle(usize, usize),
}

impl Layout {
    /// The spot at `(x, y)`, relative to the map.
    #[must_use]
    pub fn at(&self, modules: &[ModuleFacts], x: f32, y: f32) -> Option<Spot> {
        let (pitch, stone) = (PITCH * self.k, STONE * self.k);
        for (i, region) in self.regions.iter().enumerate() {
            let (rx, ry, rw, rh) = region.rect;
            if x < rx || x > rx + rw || y < ry || y > ry + rh {
                continue;
            }
            let (lx, ly) = (x - region.origin.0, y - region.origin.1);
            if lx >= -2.0 && ly >= -2.0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let (c, r) = ((lx.max(0.0) / pitch) as usize, (ly.max(0.0) / pitch) as usize);
                let (fx, fy) = (lx - c as f32 * pitch, ly - r as f32 * pitch);
                let index = r * region.columns + c;
                if c < region.columns && index < modules[i].shingles.len() && fx <= stone + 2.0 && fy <= stone + 2.0 {
                    return Some(Spot::Shingle(i, index));
                }
            }
            return Some(Spot::Region(i));
        }
        None
    }

    /// The origin of one shingle, relative to the map.
    #[must_use]
    pub fn shingle(&self, module: usize, index: usize) -> Option<(f32, f32)> {
        let region = self.regions.get(module)?;
        let pitch = PITCH * self.k;
        #[allow(clippy::cast_precision_loss)]
        Some((
            region.origin.0 + (index % region.columns) as f32 * pitch,
            region.origin.1 + (index / region.columns) as f32 * pitch,
        ))
    }
}

/// What the map does when something is opened: `(module, shingle)`.
pub type Open = Rc<dyn Fn(usize, Option<usize>, &mut Window, &mut App)>;

struct State {
    hover: Option<Spot>,
    /// The semantic focus seen when the pointer last moved. A changed
    /// keyboard target makes an old pointer position stale until it moves.
    rest: Option<Spot>,
}

/// The map (see [`shingles`]).
pub struct Shingles {
    id: ElementId,
    modules: Rc<[ModuleFacts]>,
    measure: Measure,
    time: Time,
    open: Option<usize>,
    rest: Option<Spot>,
    on_open: Option<Open>,
    on_carry: Option<Rc<dyn Fn(Carrying, &mut Window, &mut App)>>,
    geometry: Option<Rc<Layout>>,
}

/// A shingle map of `modules` as wide as `measure` gives it.
#[must_use]
pub fn shingles(id: impl Into<ElementId>, modules: Rc<[ModuleFacts]>, measure: &Measure) -> Shingles {
    Shingles {
        id: id.into(),
        modules,
        measure: *measure,
        time: Time::Now,
        open: None,
        rest: None,
        on_open: None,
        on_carry: None,
        geometry: None,
    }
}

impl Shingles {
    /// Reading a release other than the pin: the regions take the warmer tint.
    #[must_use]
    pub const fn time(mut self, time: Time) -> Self {
        self.time = time;
        self
    }

    /// The module that is open (its region stays lit).
    #[must_use]
    pub const fn open(mut self, open: Option<usize>) -> Self {
        self.open = open;
        self
    }

    /// Shows a spot as rested (scenes; a live pointer overrides it).
    #[must_use]
    pub const fn rest(mut self, spot: Option<Spot>) -> Self {
        self.rest = spot;
        self
    }

    /// Reuses the measured geometry that also positions the host's keyboard targets.
    #[must_use]
    pub fn geometry(mut self, geometry: Rc<Layout>) -> Self {
        self.geometry = Some(geometry);
        self
    }

    /// Called when a region, or a shingle in it, is clicked (or Enter walks
    /// onto it).
    #[must_use]
    pub fn on_open(mut self, on_open: impl Fn(usize, Option<usize>, &mut Window, &mut App) + 'static) -> Self {
        self.on_open = Some(Rc::new(on_open));
        self
    }

    /// Called just before `on_open` when a region is clicked, with the
    /// shingles of the module as they are on screen (window coordinates), so
    /// the host can carry them to the cards.
    #[must_use]
    pub fn on_carry(mut self, on_carry: impl Fn(Carrying, &mut Window, &mut App) + 'static) -> Self {
        self.on_carry = Some(Rc::new(on_carry));
        self
    }

    fn k(&self) -> f32 {
        self.measure.scale() * (0.94 + 0.42 * self.measure.t())
    }

    fn layout(&self, window: &Window) -> Rc<Layout> {
        self.geometry.clone().unwrap_or_else(|| measured(&self.modules, &self.measure, window))
    }

    fn foot(&self, window: &Window) -> f32 {
        let width = f32::from(self.measure.width());
        let scale = self.measure.scale();
        let name_role = self.measure.role(FOOT_NAME);
        let more_role = self.measure.role(FOOT_MORE);
        let mut height = 26.0 * scale;
        for module in self.modules.iter() {
            let name = shape(module.name.clone(), name_role, Hsla::default(), window);
            let more: SharedString = foot_more(module).into();
            let more_width = shape(more.clone(), more_role, Hsla::default(), window).width();
            let go: SharedString = if module.extent == Extent::Page {
                "opens its own page".into()
            } else {
                "opens here".into()
            };
            let go_width = shape(go.clone(), more_role, Hsla::default(), window).width();
            let room = width - name.width() - more_width - go_width - 52.0 * scale;
            if room < 0.0 {
                #[allow(clippy::cast_precision_loss)]
                let rows = wrapped_line_count(&module.name, name_role, width, window) as f32 * name_role.line
                    + wrapped_line_count(&more, more_role, width, window) as f32 * more_role.line
                    + wrapped_line_count(&go, more_role, width, window) as f32 * more_role.line;
                height = height.max(rows + 8.0 * scale);
            }
        }
        height
    }
}

impl IntoElement for Shingles {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// What the map keeps between layout and paint.
pub struct MapLayout {
    state: Entity<State>,
    layout: Rc<Layout>,
}

impl Element for Shingles {
    type RequestLayoutState = MapLayout;
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
    ) -> (LayoutId, MapLayout) {
        let state = window.use_keyed_state("shingles", cx, |_, _| State { hover: None, rest: None });
        let layout = self.layout(window);
        let foot = self.foot(window);
        let mut style = Style::default();
        style.size.width = px(f32::from(self.measure.width())).into();
        style.size.height = px(layout.height + foot + 8.0 * self.measure.scale()).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), MapLayout { state, layout })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut MapLayout,
        window: &mut Window,
        _cx: &mut App,
    ) -> Hitbox {
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    #[allow(clippy::too_many_lines)]
    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        map: &mut MapLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let scale = self.measure.scale();
        let k = map.layout.k;
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let width = f32::from(bounds.size.width);
        // What is lit: what the pointer is on, else what the host rests it on
        // (the keyboard's focus, a scene).
        let current_state = map.state.read(cx);
        let hover = crate::hover::visible_target(current_state.hover, current_state.rest, self.rest);
        let lit_region = match hover {
            Some(Spot::Region(i) | Spot::Shingle(i, _)) => Some(i),
            None => None,
        };
        let modules = self.modules.clone();
        let stone = STONE * k;
        let chamfer = (stone * 0.3).max(1.5);

        // Regions: a whisper of fill and a hairline; the lit one brightens.
        let tint: Hsla = if self.time == Time::Past {
            palette.amber.base.alpha(0.06).into()
        } else {
            palette.peri.base.alpha(0.05).into()
        };
        let mut fills = Fill::new();
        let mut lines = Fill::new();
        let mut bright = Fill::new();
        let mut open_line = Fill::new();
        for (i, region) in map.layout.regions.iter().enumerate() {
            let (rx, ry, rw, rh) = region.rect;
            let outline = Poly::rect(ox + rx, oy + ry, rw, rh);
            fills.poly(&outline);
            let width = if lit_region == Some(i) || self.open == Some(i) { 1.5 } else { 1.0 };
            for ring in outline.offset(-width * 0.5).stroke_ring(width) {
                if lit_region == Some(i) {
                    bright.poly(&ring);
                } else if self.open == Some(i) {
                    open_line.poly(&ring);
                } else {
                    lines.poly(&ring);
                }
            }
        }
        fills.paint(window, tint);
        lines.paint(window, Hsla::from(palette.line3));
        open_line.paint(window, Hsla::from(palette.peri.base));
        bright.paint(window, Hsla::from(palette.peri_hi));

        // Shingles, batched by colour.
        let mut batches: Vec<(Hsla, Fill)> = Vec::new();
        let mut batch = |color: Hsla, poly: &Poly| {
            let at = batches.iter().position(|(c, _)| *c == color).unwrap_or_else(|| {
                batches.push((color, Fill::new()));
                batches.len() - 1
            });
            batches[at].1.poly(poly);
        };
        let mut swelled: Option<(Poly, Hsla, usize, usize)> = None;
        for (i, module) in modules.iter().enumerate() {
            for (j, shingle) in module.shingles.iter().enumerate() {
                let Some((sx, sy)) = map.layout.shingle(i, j) else {
                    continue;
                };
                let colour = shingle_ink(shingle, self.time, palette);
                if hover == Some(Spot::Shingle(i, j)) {
                    let e = stone * 1.5;
                    let poly = Poly::chamfer(ox + sx - (e - stone) * 0.5, oy + sy - (e - stone) * 0.5, e, e, chamfer * 1.4);
                    let full = if shingle.yours.is_yours() {
                        palette.mint.base
                    } else {
                        palette.family(shingle.family).hue
                    };
                    swelled = Some((poly, full.into(), i, j));
                    continue;
                }
                batch(colour, &Poly::chamfer(ox + sx, oy + sy, stone, stone, chamfer));
            }
        }
        for (color, fill) in batches {
            fill.paint(window, color);
        }

        // Labels: always drawn, always whole (the layout gave them room).
        let label_role = scaled(self.measure.role(LABEL), k / scale);
        let mut painted: Vec<(SharedString, Bounds<Pixels>, f32)> = Vec::new();
        for (i, module) in modules.iter().enumerate() {
            let region = &map.layout.regions[i];
            let (rx, ry, rw, _) = region.rect;
            let pad = region.origin.0 - rx;
            let label_width = (rw - 2.0 * pad).max(1.0);
            let ink: Hsla = if lit_region == Some(i) || self.open == Some(i) {
                palette.ink0.into()
            } else {
                palette.ink1.into()
            };
            let natural = shape(module.name.clone(), label_role, ink, window);
            let wrapped = wrap_label(module.name.clone(), label_role, ink, label_width, window);
            let (lx, mut ly) = (ox + rx + pad, oy + ry + pad);
            for line in &wrapped {
                line.paint(gpui::point(px(lx), px(ly)), px(label_role.line), TextAlign::Left, None, window, cx)
                    .ok();
                let line_count = 1 + line.wrap_boundaries().len();
                #[allow(clippy::cast_precision_loss)]
                let line_count = line_count as f32;
                ly += line_count * label_role.line;
            }
            let label_height = (ly - (oy + ry + pad)).max(label_role.line);
            painted.push((
                module.name.clone(),
                Bounds::new(gpui::point(px(lx), px(oy + ry + pad)), gpui::size(px(label_width), px(label_height))),
                natural.width(),
            ));
        }
        if probe::enabled(cx) {
            for (name, at, natural) in painted {
                let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), SharedString::from(format!("region-{name}")));
                publish(cx, &key, at, &name, label_role, natural, TextOverflow::Wrap, label_role.size * 2.0, window);
            }
            // Where every shingle stands, for tests that rest on one.
            for (i, module) in modules.iter().enumerate() {
                for j in 0..module.shingles.len() {
                    if let Some((sx, sy)) = map.layout.shingle(i, j) {
                        let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), SharedString::from(format!("shingle-{i}-{j}")));
                        probe::record_bounds(cx, &key, Bounds::new(gpui::point(px(ox + sx), px(oy + sy)), gpui::size(px(stone), px(stone))));
                    }
                }
            }
        }

        // The swollen shingle and its plate.
        if let Some((poly, colour, i, j)) = swelled {
            let mut fill = Fill::new();
            fill.poly(&poly);
            fill.paint(window, colour);
            if let Some((sx, sy)) = map.layout.shingle(i, j) {
                let name = modules[i].shingles[j].name.clone();
                let plate_role = self.measure.role(PLATE);
                let text = shape(name.clone(), plate_role, palette.ink0.into(), window);
                let (pw, ph) = (text.width() + 14.0 * scale, text.line_height() + 5.0 * scale);
                let mut lx = sx + stone * 1.6;
                if lx + pw + 4.0 > width {
                    lx = sx - pw - stone * 0.6;
                }
                let ly = sy - ph * 0.5 + stone * 0.4;
                let plate = Poly::chamfer(ox + lx, oy + ly, pw, ph, 3.0 * scale);
                let mut back = Fill::new();
                back.poly(&plate);
                back.paint(window, Hsla::from(palette.plate3));
                let mut edge = Fill::new();
                for ring in plate.offset(-0.5).stroke_ring(1.0) {
                    edge.poly(&ring);
                }
                edge.paint(window, Hsla::from(palette.line3));
                let baseline = oy + ly + (ph - text.line_height()) * 0.5 + text.ascent();
                text.paint(ox + lx + 7.0 * scale, baseline, window, cx);
                if probe::enabled(cx) {
                    let at = Bounds::new(
                        gpui::point(px(ox + lx + 7.0 * scale), px(baseline - text.ascent())),
                        gpui::size(px(text.width()), px(text.line_height())),
                    );
                    let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "plate".into());
                    publish(cx, &key, at, &name, plate_role, text.width(), TextOverflow::Clip, text.width(), window);
                }
            }
        }

        // The foot: the lit module reads itself.
        if let Some(i) = lit_region {
            let module = &modules[i];
            let foot_y = oy + map.layout.height + 8.0 * scale;
            let name_role = self.measure.role(FOOT_NAME);
            let more_role = self.measure.role(FOOT_MORE);
            let name = shape(module.name.clone(), name_role, palette.ink0.into(), window);
            let more_words: SharedString = foot_more(module).into();
            let more = shape(more_words.clone(), more_role, palette.ink2.into(), window);
            let go_words: SharedString = if module.extent == Extent::Page {
                "opens its own page".into()
            } else {
                "opens here".into()
            };
            let go = shape(go_words.clone(), more_role, palette.peri_hi.into(), window);
            let room = width - name.width() - more.width() - go.width() - 52.0 * scale;
            if room < 0.0 {
                let (natural, height) = paint_wrapped(module.name.clone(), name_role, palette.ink0.into(), width, ox, foot_y, window, cx);
                if probe::enabled(cx) {
                    let at = Bounds::new(gpui::point(px(ox), px(foot_y)), gpui::size(px(width), px(height)));
                    let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "foot-name".into());
                    publish(cx, &key, at, &module.name, name_role, natural, TextOverflow::Wrap, name_role.size * 2.0, window);
                }
                let mut y = foot_y + height + 4.0 * scale;
                let (natural, height) = paint_wrapped(more_words.clone(), more_role, palette.ink2.into(), width, ox, y, window, cx);
                if probe::enabled(cx) {
                    let at = Bounds::new(gpui::point(px(ox), px(y)), gpui::size(px(width), px(height)));
                    let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "foot-more".into());
                    publish(cx, &key, at, &more_words, more_role, natural, TextOverflow::Wrap, more_role.size * 2.0, window);
                }
                y += height + 4.0 * scale;
                let (natural, height) = paint_wrapped(go_words.clone(), more_role, palette.peri_hi.into(), width, ox, y, window, cx);
                if probe::enabled(cx) {
                    let at = Bounds::new(gpui::point(px(ox), px(y)), gpui::size(px(width), px(height)));
                    let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "foot-go".into());
                    publish(cx, &key, at, &go_words, more_role, natural, TextOverflow::Wrap, more_role.size * 2.0, window);
                }
            } else {
                let mut x = ox;
                let base = foot_y + name.ascent() + 2.0 * scale;
                name.paint(x, base, window, cx);
                if probe::enabled(cx) {
                    let at = Bounds::new(
                        gpui::point(px(x), px(base - name.ascent())),
                        gpui::size(px(name.width()), px(name.line_height())),
                    );
                    let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "foot-name".into());
                    publish(cx, &key, at, &module.name, name_role, name.width(), TextOverflow::Clip, name.width(), window);
                }
                x += name.width() + 12.0 * scale;
                if let Some(doc) = module.doc.as_ref().filter(|_| room > 60.0 * scale) {
                    let text = shape_fit(doc, self.measure.role(FOOT_DOC), palette.ink1.into(), room, window);
                    text.paint(x, base, window, cx);
                    if probe::enabled(cx) {
                        let at = Bounds::new(
                            gpui::point(px(x), px(base - text.ascent())),
                            gpui::size(px(text.width()), px(text.line_height())),
                        );
                        let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "foot-doc".into());
                        publish(
                            cx,
                            &key,
                            at,
                            doc,
                            self.measure.role(FOOT_DOC),
                            text.width(),
                            TextOverflow::Clip,
                            text.width(),
                            window,
                        );
                    }
                    x += text.width() + 14.0 * scale;
                }
                more.paint(x, base, window, cx);
                if probe::enabled(cx) {
                    let at = Bounds::new(
                        gpui::point(px(x), px(base - more.ascent())),
                        gpui::size(px(more.width()), px(more.line_height())),
                    );
                    let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "foot-more".into());
                    publish(cx, &key, at, &more_words, more_role, more.width(), TextOverflow::Clip, more.width(), window);
                }
                let gx = ox + width - go.width();
                go.paint(gx, base, window, cx);
                if probe::enabled(cx) {
                    let at = Bounds::new(gpui::point(px(gx), px(base - go.ascent())), gpui::size(px(go.width()), px(go.line_height())));
                    let key = ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "foot-go".into());
                    publish(cx, &key, at, &go_words, more_role, go.width(), TextOverflow::Clip, go.width(), window);
                }
            }
        }

        // The pointer.
        let layout = map.layout.clone();
        let hit_modules = modules.clone();
        {
            let (state, hitbox, rest) = (map.state.clone(), hitbox.clone(), self.rest);
            let layout = layout.clone();
            let modules = hit_modules.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let next = hitbox
                    .is_hovered(window)
                    .then(|| layout.at(&modules, f32::from(event.position.x) - ox, f32::from(event.position.y) - oy))
                    .flatten();
                if state.read(cx).hover != next || state.read(cx).rest != rest {
                    state.update(cx, |state, cx| {
                        state.hover = next;
                        state.rest = rest;
                        cx.notify();
                    });
                }
            });
        }
        {
            let state = map.state.clone();
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _window, cx| {
                if phase == DispatchPhase::Capture && state.read(cx).hover.is_some() {
                    state.update(cx, |state, cx| {
                        state.hover = None;
                        cx.notify();
                    });
                }
            });
        }
        if let Some(on_open) = self.on_open.clone() {
            let hitbox = hitbox.clone();
            let on_carry = self.on_carry.clone();
            let (time, k) = (self.time, map.layout.k);
            let hit_layout = map.layout.clone();
            let carried = self.modules.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hitbox.is_hovered(window) {
                    return;
                }
                let spot = layout.at(&hit_modules, f32::from(event.position.x) - ox, f32::from(event.position.y) - oy);
                if let Some(spot) = spot {
                    let (module, shingle) = match spot {
                        Spot::Region(i) => (i, None),
                        Spot::Shingle(i, j) => (i, Some(j)),
                    };
                    if let Some(on_carry) = &on_carry {
                        let stone = STONE * k;
                        let stones = carried[module]
                            .shingles
                            .iter()
                            .enumerate()
                            .filter_map(|(index, shingle)| {
                                let (x, y) = hit_layout.shingle(module, index)?;
                                Some(Stone {
                                    from: Bounds::new(gpui::point(px(ox + x), px(oy + y)), gpui::size(px(stone), px(stone))),
                                    ink: shingle_ink(shingle, time, palette),
                                })
                            })
                            .collect();
                        on_carry(Carrying { module, stones }, window, cx);
                    }
                    on_open(module, shingle, window, cx);
                }
            });
        }
    }
}

/// `role` scaled by `factor` (the map's own growth on a wide container).
fn scaled(role: TypeRole, factor: f32) -> TypeRole {
    TypeRole {
        size: role.size * factor,
        line: role.line * factor,
        ..role
    }
}

fn shingle_ink(shingle: &ShingleFacts, time: Time, palette: &crate::tokens::Palette) -> Hsla {
    match shingle.state {
        Some(Change::Gone) => palette.coral.base.alpha(0.95).into(),
        Some(Change::Changed) => palette.amber.base.into(),
        Some(Change::New) => palette.mint.base.into(),
        Some(Change::Absent) => palette.ink4.alpha(0.35).into(),
        None if shingle.yours.is_yours() => palette.mint.base.into(),
        None => {
            let hue = palette.family(shingle.family).hue;
            hue.alpha(if time == Time::Past { 0.34 } else { 0.86 }).into()
        }
    }
}

/// What a module holds, in words: `4 types · 9 functions · 1 trait`.
#[must_use]
pub fn foot_more(module: &ModuleFacts) -> String {
    let mut counts = [0usize; 4];
    for shingle in &module.shingles {
        counts[match shingle.family {
            Family::Type => 0,
            Family::Callable => 1,
            Family::Contract => 2,
            Family::Value | Family::Namespace => 3,
        }] += 1;
    }
    let words = [("type", "types"), ("function", "functions"), ("trait", "traits"), ("value", "values")];
    let mut parts: Vec<String> = counts
        .iter()
        .zip(words)
        .filter(|(n, _)| **n > 0)
        .map(|(n, (one, many))| format!("{n} {}", if *n == 1 { one } else { many }))
        .collect();
    let yours = module.shingles.iter().filter(|s| s.yours.is_yours()).count();
    if yours > 0 {
        parts.push(format!("you use {yours}"));
    }
    if parts.is_empty() { "nothing public".to_owned() } else { parts.join(" · ") }
}

fn publish(
    cx: &mut App,
    key: &ElementId,
    at: Bounds<Pixels>,
    content: &str,
    role: TypeRole,
    natural: f32,
    overflow: TextOverflow,
    min_width: f32,
    window: &Window,
) {
    probe::record_text_in(
        cx,
        key,
        at,
        TextSample {
            key: String::new(),
            bounds: probe::BoundsSample {
                key: String::new(),
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 0.0,
            },
            paint_clip: None,
            scroll_ancestors: probe::current_scroll_ancestors(),
            natural_width: natural,
            overflow,
            content: content.to_owned(),
            min_width,
            line_height: role.line,
            size: role.size,
            weight: role.weight,
            region: probe::current_region(),
        },
        window,
    );
}

#[cfg(test)]
mod tests {
    use super::{Layout, ModuleFacts, ShingleFacts, Spot, foot_more, layout};
    use crate::folio::state::Use;
    use crate::tokens::Family;

    fn modules(counts: &[usize]) -> Vec<(usize, usize)> {
        counts.iter().enumerate().map(|(i, n)| (4 + i, *n)).collect()
    }

    fn overlap(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
        a.0 < b.0 + b.2 - 0.01 && b.0 < a.0 + a.2 - 0.01 && a.1 < b.1 + b.3 - 0.01 && b.1 < a.1 + a.3 - 0.01
    }

    #[test]
    fn regions_tile_rows_without_overlap_and_fill_the_width() {
        let counts = [15, 14, 12, 10, 8, 7, 6, 6, 5, 5, 3, 3, 2, 2, 2, 1];
        for width in [800.0, 1000.0, 1440.0, 2200.0] {
            let l = layout(&modules(&counts), width, 1.0);
            assert_eq!(l.regions.len(), counts.len());
            for (i, a) in l.regions.iter().enumerate() {
                assert!(
                    a.rect.0 >= -0.01 && a.rect.0 + a.rect.2 <= width + 0.5,
                    "{width}: region {i} leaves the map: {:?}",
                    a.rect
                );
                for b in l.regions.iter().skip(i + 1) {
                    assert!(!overlap(a.rect, b.rect), "{width}: {:?} overlaps {:?}", a.rect, b.rect);
                }
            }
            // Every shingle sits inside its own region.
            for (i, region) in l.regions.iter().enumerate() {
                for j in 0..counts[i] {
                    let (x, y) = l.shingle(i, j).expect("shingle");
                    assert!(
                        x >= region.rect.0 && x + 10.0 <= region.rect.0 + region.rect.2 + 0.01,
                        "{width}: shingle {i}/{j} escapes its region in x"
                    );
                    assert!(
                        y >= region.rect.1 && y + 10.0 <= region.rect.1 + region.rect.3 + 0.01,
                        "{width}: shingle {i}/{j} escapes its region in y"
                    );
                }
            }
        }
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn long_unbreakable_labels_wrap_inside_narrow_map_geometry() {
        for width in [96.0, 128.0, 192.0, 256.0] {
            for k in [1.0, 2.0] {
                let counts = [1, 3, 60];
                let input = [(180, counts[0]), (24, counts[1]), (8, counts[2])];
                let l = layout(&input, width, k);
                assert_eq!(l.regions.len(), input.len());
                let mut wrapped = false;
                for (i, region) in l.regions.iter().enumerate() {
                    assert!(region.rect.0 >= -0.01, "{width}@{k}: region {i} starts outside the map: {:?}", region.rect);
                    assert!(
                        region.rect.0 + region.rect.2 <= width + 0.01,
                        "{width}@{k}: region {i} exceeds the map: {:?}",
                        region.rect
                    );
                    wrapped |= region.label_lines > 1;
                    let pad = region.origin.0 - region.rect.0;
                    let band = region.origin.1 - region.rect.1 - pad;
                    assert!(
                        band + 0.01 >= region.label_lines as f32 * super::LABEL.line * k,
                        "label band is too short: {region:?}"
                    );
                    for index in 0..counts[i] {
                        let (x, y) = l.shingle(i, index).expect("shingle");
                        assert!(
                            x >= region.rect.0 && x + super::STONE * k <= region.rect.0 + region.rect.2 + 0.01,
                            "{width}@{k}: shingle {i}/{index} escapes its region"
                        );
                        assert!(
                            y + super::STONE * k <= region.rect.1 + region.rect.3 + 0.01,
                            "{width}@{k}: shingle {i}/{index} escapes its region"
                        );
                    }
                    for other in l.regions.iter().skip(i + 1) {
                        assert!(
                            !overlap(region.rect, other.rect),
                            "{width}@{k}: regions overlap: {:?} / {:?}",
                            region.rect,
                            other.rect
                        );
                    }
                }
                assert!(wrapped, "{width}@{k}: no long label wrapped");
            }
        }
    }

    #[test]
    fn keyboard_region_bounds_are_the_map_layout_bounds() {
        let layout = layout(&[(12, 2), (8, 3), (16, 1)], 256.0, 2.0);
        let bounds = layout.region_bounds();
        assert_eq!(bounds.len(), layout.regions.len());
        for (region, bounds) in layout.regions.iter().zip(bounds) {
            assert!((f32::from(bounds.origin.x) - region.rect.0).abs() <= 0.01);
            assert!((f32::from(bounds.origin.y) - region.rect.1).abs() <= 0.01);
            assert!((f32::from(bounds.size.width) - region.rect.2).abs() <= 0.01);
            assert!((f32::from(bounds.size.height) - region.rect.3).abs() <= 0.01);
        }
    }

    #[test]
    fn a_wider_window_makes_fewer_taller_rows_never_more() {
        let counts = [15, 14, 12, 10, 8, 7, 6, 6, 5, 5, 3, 3, 2, 2, 2, 1];
        let heights: Vec<f32> = [800.0, 1000.0, 1440.0, 2200.0]
            .iter()
            .map(|w| layout(&modules(&counts), *w, 1.0).height)
            .collect();
        for pair in heights.windows(2) {
            assert!(pair[1] <= pair[0] + 0.01, "the map grew taller as the window widened: {heights:?}");
        }
    }

    #[test]
    fn the_pointer_finds_a_shingle_or_falls_back_to_its_region() {
        let l = layout(&modules(&[9, 3]), 600.0, 1.0);
        let mods: Vec<ModuleFacts> = [9usize, 3]
            .iter()
            .map(|n| {
                ModuleFacts::new(
                    "m",
                    (0..*n)
                        .map(|i| ShingleFacts {
                            name: format!("s{i}").into(),
                            family: Family::Type,
                            yours: Use::Elsewhere,
                            state: None,
                        })
                        .collect(),
                )
            })
            .collect();
        let (x, y) = l.shingle(0, 4).expect("shingle");
        assert_eq!(l.at(&mods, x + 4.0, y + 4.0), Some(Spot::Shingle(0, 4)));
        // The label band is the region.
        let r = &l.regions[0];
        assert_eq!(l.at(&mods, r.rect.0 + 12.0, r.rect.1 + 6.0), Some(Spot::Region(0)));
        // Off the map is nothing.
        assert_eq!(l.at(&mods, -20.0, -20.0), None);
        let _: &Layout = &l;
    }

    #[test]
    fn a_foot_counts_what_a_module_holds_in_words() {
        let mk = |family| ShingleFacts {
            name: "x".into(),
            family,
            yours: Use::Elsewhere,
            state: None,
        };
        let mut module = ModuleFacts::new("sync", vec![mk(Family::Type), mk(Family::Type), mk(Family::Callable), mk(Family::Contract)]);
        assert_eq!(foot_more(&module), "2 types · 1 function · 1 trait");
        module.shingles[0].yours = Use::Yours;
        assert_eq!(foot_more(&module), "2 types · 1 function · 1 trait · you use 1");
        assert_eq!(foot_more(&ModuleFacts::new("e", vec![])), "nothing public");
    }
}

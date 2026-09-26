//! The version comb: a package's releases as a row of ticks under its name
//! (`v4/shots/VersionComb.png`).
//!
//! - **At rest** every release is a 1 px `ink4` tick standing on one
//!   baseline: 18 px for a new major, 12 px for a new minor, 6 px for a
//!   patch. The release your lockfile pins is mint, 2 px wide, 20 px tall.
//!   No labels.
//! - **Hover** turns a tick `ink0` and raises a tip *below* the comb — the
//!   version in mono and its age — so it never covers the name above.
//! - **Scrub** (press and drag, ←/→ one release, PgUp/PgDn one minor,
//!   Home/End the first/newest): the chosen tick is periwinkle, 2 px wide,
//!   22 px tall, in a soft 2 px ring, and one quiet line appears under the
//!   comb — "viewing **0.3.0** · you pin **0.4.2**" — with `esc` to come
//!   home. The control only emits [`VersionSelected`]; the shell decides
//!   what it means (it re-scopes the page to that release).
//! - **Hundreds of releases.** Where ticks would stand closer than 2 px the
//!   patches run together into a hairline band and the minors still stand
//!   up out of it. Under the pointer a lens opens: releases near the pointer
//!   spread to at least 5 px each, the ones around them crowd into the
//!   lens's rim, and the comb outside the lens (both ends included) never
//!   moves. The lens stays put while the pointer moves over its plateau — so
//!   every release in it has its own hover target — and slides along the
//!   comb when the pointer pushes past the plateau.
//!
//! Keyboard-complete, text-scale aware (everything is sized from the
//! [`Measure`]); reduced motion snaps every spring.

use super::GRIP;
use super::state::{pointer_away, track, watch_pointer};
use super::kbd::{KbdSize, KbdVoice, kbd};
use crate::Set;
use crate::measure::Measure;
use crate::motion::{Motion, SNAPPY, Spec, spec};
use crate::overlay::float::{self, FloatKind, FloatRequest, Side};
use crate::paint::geom::{Fill, Poly};
use crate::paint::mix;
use crate::theme::ActiveFacet;
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{
    AnyElement, App, Bounds, DispatchPhase, ElementId, FocusHandle, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window,
    canvas, div, point, px, size,
};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

/// A release's identity, as the shell knows it.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ReleaseId(pub SharedString);

/// How big a release is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Step {
    /// `x.0.0`.
    Major,
    /// `x.y.0`.
    Minor,
    /// `x.y.z`.
    Patch,
}

impl Step {
    /// Reads a version's step from its text (`2.0.0` major, `1.4.0` minor,
    /// anything else a patch; pre-release and build tags are ignored).
    #[must_use]
    pub fn of(version: &str) -> Self {
        let core = version.split(['-', '+']).next().unwrap_or(version);
        let mut parts = core.split('.').skip(1);
        let minor = parts.next().unwrap_or("0");
        let patch = parts.next().unwrap_or("0");
        match (minor == "0", patch == "0") {
            (true, true) => Self::Major,
            (_, true) => Self::Minor,
            _ => Self::Patch,
        }
    }
}

/// One release on the comb.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Its identity.
    pub id: ReleaseId,
    /// The version, as written (`1.0.180`).
    pub version: SharedString,
    /// How big it is.
    pub step: Step,
    /// How long ago (`7 months ago`).
    pub age: SharedString,
}

/// What the comb emits: the reader chose this release.
#[derive(Clone, Debug, PartialEq)]
pub struct VersionSelected(pub ReleaseId);

type OnSelect = Rc<dyn Fn(&VersionSelected, &mut Window, &mut App)>;

/// A version comb over `releases` (oldest first).
#[derive(IntoElement)]
pub struct VersionComb {
    id: ElementId,
    releases: Rc<[Release]>,
    pinned: Option<usize>,
    selected: Option<usize>,
    focus_look: bool,
    hover_look: Option<usize>,
    lens_look: Option<f32>,
    touches: Rc<[bool]>,
    measure: Measure,
    on_select: Option<OnSelect>,
}

/// A version comb for `releases` (oldest first), as wide as `measure`.
#[must_use]
pub fn version_comb(
    id: impl Into<ElementId>,
    releases: impl Into<Rc<[Release]>>,
    measure: &Measure,
) -> VersionComb {
    VersionComb {
        id: id.into(),
        releases: releases.into(),
        pinned: None,
        selected: None,
        focus_look: false,
        hover_look: None,
        lens_look: None,
        touches: Rc::from(Vec::new()),
        measure: *measure,
        on_select: None,
    }
}

impl VersionComb {
    /// The release your lockfile pins (the mint tick).
    #[must_use]
    pub const fn pinned(mut self, index: usize) -> Self {
        self.pinned = Some(index);
        self
    }

    /// The release being read. Defaults to the pin, else the newest.
    #[must_use]
    pub const fn selected(mut self, index: usize) -> Self {
        self.selected = Some(index);
        self
    }

    /// Shows the keyboard focus outline regardless of focus (state sheets).
    #[must_use]
    pub const fn focus_look(mut self) -> Self {
        self.focus_look = true;
        self
    }

    /// The releases whose changes reach your code (by index): their ticks
    /// are a quieter mint. W-Data's `data::release` knows which they are
    /// (a release touches you when `Crate::impact(pinned, release)` is not
    /// empty); the comb only draws it.
    #[must_use]
    pub fn touches(mut self, releases: impl IntoIterator<Item = usize>) -> Self {
        let mut flags = vec![false; self.releases.len()];
        for index in releases {
            if let Some(flag) = flags.get_mut(index) {
                *flag = true;
            }
        }
        self.touches = Rc::from(flags);
        self
    }

    /// Shows release `index` hovered, with its tip (state sheets).
    #[must_use]
    pub const fn hover_look(mut self, index: usize) -> Self {
        self.hover_look = Some(index);
        self
    }

    /// Shows the lens open at `x` px from the comb's left (state sheets).
    #[must_use]
    pub const fn lens_look(mut self, x: f32) -> Self {
        self.lens_look = Some(x);
        self
    }

    /// Called whenever the chosen release changes.
    #[must_use]
    pub fn on_select(
        mut self,
        handler: impl Fn(&VersionSelected, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

// ---------------------------------------------------------------- geometry

/// Closer than this (px at 100 %), patches run together into a band.
const BAND: f32 = 2.0;
/// Below this pitch (px at 100 %) a lens opens under the pointer.
const HIT: f32 = 5.0;
/// Every release on the lens's plateau gets this much pitch.
const LENS_PITCH: f32 = 6.0;
/// The lens's reach either side of its centre (the target's 90 px).
const LENS_RADIUS: f32 = 90.0;
/// The share of the reach that is plateau; the rest is the rim.
const PLATEAU: f32 = 0.5;

/// Where the ticks of an `n`-release comb `width` px wide sit.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Layout {
    pub n: usize,
    pub width: f32,
    pub pitch: f32,
    pub scale: f32,
}

/// An open lens, anchored at `centre` px from the comb's left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Lens {
    pub centre: f32,
}

impl Layout {
    pub(crate) fn new(n: usize, width: f32, scale: f32) -> Self {
        let width = width.max(0.0);
        #[allow(clippy::cast_precision_loss)]
        let pitch = if n > 1 { width / (n - 1) as f32 } else { 0.0 };
        Self {
            n,
            width,
            pitch,
            scale,
        }
    }

    fn last(&self) -> f32 {
        #[allow(clippy::cast_precision_loss)]
        let last = self.n.saturating_sub(1) as f32;
        last
    }

    /// Whether releases are too close to hit one by one (a lens opens).
    pub(crate) fn needs_lens(&self) -> bool {
        self.n > 1 && self.pitch < HIT * self.scale
    }

    /// Whether the patches read as a band.
    pub(crate) fn banded(&self) -> bool {
        self.n > 1 && self.pitch < BAND * self.scale
    }

    fn uniform(&self, i: f32) -> f32 {
        if self.n <= 1 {
            return self.width * 0.5;
        }
        self.pitch * i
    }

    fn uniform_index(&self, x: f32) -> f32 {
        if self.pitch <= 0.0 {
            return 0.0;
        }
        (x / self.pitch).clamp(0.0, self.last())
    }

    /// The lens: reach `r`, plateau half-width `p`, plateau pitch, and the
    /// releases the plateau holds either side of its centre (`q`).
    fn lens_shape(&self) -> (f32, f32, f32, f32) {
        let s = self.scale;
        let r = (LENS_RADIUS * s).min(self.width * 0.5).max(1.0);
        let pitch = (LENS_PITCH * s).max(self.pitch);
        let q = ((r * PLATEAU) / pitch).min(r / self.pitch.max(1e-4) * 0.8);
        (r, q * pitch, pitch, q)
    }

    /// Where release `i` sits under a fully open lens.
    pub(crate) fn lensed(&self, i: f32, lens: &Lens) -> f32 {
        let x = self.uniform(i);
        let (r, p, pitch, q) = self.lens_shape();
        let c = lens.centre;
        if (x - c).abs() >= r {
            return x;
        }
        let ic = self.uniform_index(c);
        let d = i - ic;
        if d.abs() <= q {
            return (c + d * pitch).clamp(0.0, self.width);
        }
        // The rim: the releases between the plateau and the lens's edge
        // crowd into what is left of the reach.
        let sign = d.signum();
        let edge_x = (c + sign * r).clamp(0.0, self.width);
        let edge_i = self.uniform_index(edge_x);
        let from_i = ic + sign * q;
        let from_x = (c + sign * p).clamp(0.0, self.width);
        let span = edge_i - from_i;
        if span.abs() < 1e-4 {
            return from_x;
        }
        from_x + (edge_x - from_x) * ((i - from_i) / span)
    }

    /// Every tick's position under `lens` opened `strength` of the way.
    pub(crate) fn positions(&self, lens: Option<&Lens>, strength: f32) -> Vec<f32> {
        (0..self.n)
            .map(|i| {
                #[allow(clippy::cast_precision_loss)]
                let i = i as f32;
                let flat = self.uniform(i);
                match lens {
                    Some(lens) if strength > 0.0 => flat + (self.lensed(i, lens) - flat) * strength,
                    _ => flat,
                }
            })
            .collect()
    }

    /// The lens after the pointer moved to `x`: it opens where the pointer
    /// enters, stays while the pointer moves over its plateau, and slides
    /// when the pointer pushes past the plateau.
    pub(crate) fn follow(&self, lens: Option<Lens>, x: f32) -> Lens {
        let (_, p, _, _) = self.lens_shape();
        let Some(mut lens) = lens else {
            return Lens {
                centre: x.clamp(0.0, self.width),
            };
        };
        let inner = p * 0.85;
        let d = x - lens.centre;
        if d.abs() > inner {
            lens.centre += d - inner * d.signum();
        }
        // Near an end the lens gives way faster than the pointer moves, so
        // the end's own releases come to the pointer (at the very end the
        // lens is centred on it).
        let give_left = x + inner * (x / p).clamp(0.0, 1.0);
        let give_right = x - inner * ((self.width - x) / p).clamp(0.0, 1.0);
        lens.centre = lens.centre.min(give_left).max(give_right).clamp(0.0, self.width);
        lens
    }
}

/// The tick nearest `x` among ascending `positions`.
pub(crate) fn nearest(positions: &[f32], x: f32) -> Option<usize> {
    if positions.is_empty() {
        return None;
    }
    let at = positions.partition_point(|p| *p < x);
    let candidates = [at.saturating_sub(1), at.min(positions.len() - 1)];
    candidates
        .into_iter()
        .min_by(|a, b| (positions[*a] - x).abs().total_cmp(&(positions[*b] - x).abs()))
}

/// A move along a release history (the comb's keys, and `[` / `]` from
/// anywhere on a page through the shell).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ReleaseStep {
    /// One release older (`←`, `[`).
    Older,
    /// One release newer (`→`, `]`).
    Newer,
    /// Back to the release that opened this minor, or the one before
    /// (`PgDn`).
    OlderMinor,
    /// On to the next release that opens a minor or major (`PgUp`).
    NewerMinor,
    /// The first release (`Home`).
    First,
    /// The newest release (`End`).
    Newest,
}

impl ReleaseStep {
    /// The move a key makes on the comb (`left`, `]`, `pageup`, `end`…).
    #[must_use]
    pub fn of_key(key: &str) -> Option<Self> {
        Some(match key {
            "left" | "[" => Self::Older,
            "right" | "]" => Self::Newer,
            "pagedown" => Self::OlderMinor,
            "pageup" => Self::NewerMinor,
            "home" => Self::First,
            "end" => Self::Newest,
            _ => return None,
        })
    }
}

/// Where `step` lands from release `from` in `releases` (oldest first);
/// `None` only for an empty history. The comb's keys and a page's `[` / `]`
/// both go through here.
#[must_use]
pub fn step_release(releases: &[Release], from: usize, step: ReleaseStep) -> Option<usize> {
    let last = releases.len().checked_sub(1)?;
    let from = from.min(last);
    Some(match step {
        ReleaseStep::Older => from.saturating_sub(1),
        ReleaseStep::Newer => (from + 1).min(last),
        ReleaseStep::First => 0,
        ReleaseStep::Newest => last,
        ReleaseStep::NewerMinor => (from + 1..=last)
            .find(|&i| releases[i].step != Step::Patch)
            .unwrap_or(last),
        ReleaseStep::OlderMinor => (0..from)
            .rev()
            .find(|&i| releases[i].step != Step::Patch)
            .unwrap_or(0),
    })
}

/// The release a key moves to from `from`.
pub(crate) fn step_key(releases: &[Release], from: usize, key: &str) -> Option<usize> {
    step_release(releases, from, ReleaseStep::of_key(key)?)
}

// ------------------------------------------------------------------- state

struct CombState {
    focus: FocusHandle,
    lens: Option<Lens>,
    hot: Option<usize>,
    dragging: bool,
    tip: Option<ElementId>,
    /// The release whose tip is open (the tip is re-anchored every frame,
    /// so it moves down as the "viewing" line opens under the comb).
    tip_at: Option<usize>,
    /// The open tip was opened by a key (it stays until closed, where a
    /// hover tip leaves by itself).
    tip_keyboard: bool,
    /// A state sheet's tip was opened (once; writing it never notifies).
    sheet_tip: Rc<Cell<bool>>,
    /// The selection the comb last drew, and the one it last chose itself:
    /// a change it did not make (`[` / `]` from the page) is echoed.
    drawn: Rc<Cell<Option<usize>>>,
    chose: Rc<Cell<Option<usize>>>,
    /// Where the comb's ticks were laid out last frame.
    last_bounds: Rc<Cell<Bounds<Pixels>>>,
}

const NOTE: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 400.0,
    size: 12.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};
const NOTE_MONO: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 600.0,
    size: 11.5,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};
const TIP_TEXT: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 400.0,
    size: 11.5,
    line: 15.0,
    tracking: 0.0,
    italic: false,
};
const TIP_MONO: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 600.0,
    ..TIP_TEXT
};

/// Whether the comb shows anything the pointer put there (a hovered tick,
/// the lens, a hover tip) outside a drag.
fn live_hover(state: &gpui::Entity<CombState>, cx: &App) -> bool {
    let st = state.read(cx);
    !st.dragging && (st.hot.is_some() || st.lens.is_some() || st.tip.as_ref().is_some_and(|_| !st.tip_keyboard))
}

/// The pointer is gone: the hovered tick, the lens and a hover tip let go;
/// a tip keys opened stays unless `all` (the pointer moved off the comb).
fn let_go(state: &gpui::Entity<CombState>, all: bool, window: &mut Window, cx: &mut App) {
    let (tip, keyboard) = {
        let st = state.read(cx);
        (st.tip.clone(), st.tip_keyboard)
    };
    let drop = all || !keyboard;
    state.update(cx, |st, cx| {
        st.hot = None;
        st.lens = None;
        if drop {
            st.tip = None;
            st.tip_at = None;
        }
        cx.notify();
    });
    if let (Some(tip), true) = (tip, drop) {
        drop_tip(&tip, keyboard, window, cx);
    }
}

/// How long the comb's echo of an outside step stays.
const ECHO: std::time::Duration = std::time::Duration::from_millis(1400);

/// The "viewing" line's height with its gap, px at 100 %.
const NOTE_HEIGHT: f32 = 28.0;

/// Everything one frame of the comb paints.
#[derive(Clone)]
struct Frame {
    releases: Rc<[Release]>,
    layout: Layout,
    lens: Option<Lens>,
    strength: f32,
    pinned: Option<usize>,
    /// The chosen tick's glide (fractional index).
    ring: f32,
    /// How strongly the chosen tick shows (away from the pin).
    away: f32,
    focus_t: f32,
    hot: Option<usize>,
    hot_t: f32,
    /// Per release: its changes reach your code.
    touches: Rc<[bool]>,
}

fn tick_height(step: Step) -> f32 {
    match step {
        Step::Major => 18.0,
        Step::Minor => 12.0,
        Step::Patch => 6.0,
    }
}

fn alpha(color: Hsla, a: f32) -> Hsla {
    let mut color = color;
    color.alpha *= a.clamp(0.0, 1.0);
    color
}

fn paint_comb(frame: &Frame, bounds: Bounds<Pixels>, palette: &Palette, window: &mut Window) -> Vec<f32> {
    let s = frame.layout.scale;
    let x0 = f32::from(bounds.origin.x);
    let base = f32::from(bounds.origin.y + bounds.size.height);
    let positions = frame.layout.positions(frame.lens.as_ref(), frame.strength);
    let n = positions.len();
    let ink4: Hsla = palette.ink4.into();
    let ink0: Hsla = palette.ink0.into();
    let hair = s.max(1.0);
    let band_h = tick_height(Step::Patch) * s;
    let close = |i: usize| {
        let left = if i > 0 { positions[i] - positions[i - 1] } else { f32::MAX };
        let right = if i + 1 < n { positions[i + 1] - positions[i] } else { f32::MAX };
        left.min(right) < BAND * s
    };

    // Past pixel density (at the comb's own pitch) the patches are one
    // continuous hairline band from the first release to the last, and the
    // lens only moves ticks above it: the comb stays one object. In a
    // sparser comb, patches the lens's rim crowds run together locally.
    let banded = frame.layout.banded();
    let mut band = Fill::new();
    let mut ticks = Fill::new();
    let mut lifted = Fill::new();
    let mut touched = Fill::new();
    if banded && n > 1 {
        let (from, to) = (positions[0], positions[n - 1]);
        band.poly(&Poly::rect(x0 + from - hair * 0.5, base - band_h, to - from + hair, band_h));
    }
    let mut run: Option<(f32, f32)> = None;
    let flush = |run: &mut Option<(f32, f32)>, band: &mut Fill| {
        if let Some((from, to)) = run.take() {
            band.poly(&Poly::rect(x0 + from - hair * 0.5, base - band_h, to - from + hair, band_h));
        }
    };
    for (i, x) in positions.iter().copied().enumerate() {
        let step = frame.releases[i].step;
        let crowded = close(i);
        let touches = frame.touches.get(i).copied().unwrap_or(false);
        if step == Step::Patch && crowded && !touches {
            if !banded {
                *run.get_or_insert((x, x)) = (run.map_or(x, |(from, _)| from), x);
            }
            continue;
        }
        flush(&mut run, &mut band);
        let h = tick_height(step) * s;
        let bar = Poly::rect(x0 + x - hair * 0.5, base - h, hair, h);
        if touches {
            touched.poly(&bar);
        } else if banded && step == Step::Patch {
            // A patch the lens spread, standing on the band.
            lifted.poly(&bar);
        } else {
            ticks.poly(&bar);
        }
    }
    flush(&mut run, &mut band);
    band.paint(window, alpha(ink4, 0.85));
    ticks.paint(window, ink4);
    lifted.paint(window, Hsla::from(palette.ink3));
    // Releases whose changes reach your code: mint, quieter than the pin.
    touched.paint(window, alpha(palette.mint.base.into(), 0.55));

    // The tick under the pointer, over the band if it is in it.
    if let Some(hot) = frame.hot
        && frame.hot_t > 0.0
        && Some(hot) != frame.pinned
    {
        let h = tick_height(frame.releases[hot].step) * s;
        let mut lit = Fill::new();
        lit.poly(&Poly::rect(x0 + positions[hot] - hair * 0.5, base - h, hair, h));
        lit.paint(window, mix(ink4, ink0, frame.hot_t));
    }
    // Your pin, the one "you are here" mark: a mint capsule, 2 px by
    // 20 px, in a soft mint halo.
    if let Some(pin) = frame.pinned {
        let x = x0 + positions[pin];
        let (w, h) = (2.0 * s, 20.0 * s);
        let mut halo = Fill::new();
        halo.poly(&capsule(x, base - h - 3.0 * s, base + 3.0 * s, w + 6.0 * s));
        halo.paint(window, alpha(palette.mint.base.into(), 0.14));
        let mut fill = Fill::new();
        fill.poly(&capsule(x, base - h, base, w));
        fill.paint(window, Hsla::from(palette.mint.base));
    }
    // The release being read: periwinkle, 2 px, 22 px, in a soft 2 px ring;
    // keyboard focus adds a crisp periwinkle outline around the ring.
    let shown = frame.away.max(frame.focus_t);
    if shown > 0.001 {
        let x = interpolate(&positions, frame.ring);
        let (w, h) = (2.0 * s, 22.0 * s);
        let ring = Poly::rect(x0 + x - w * 0.5 - 2.0 * s, base - h - 2.0 * s, w + 4.0 * s, h + 2.0 * s);
        let peri: Hsla = palette.peri.base.into();
        let mut soft = Fill::new();
        for piece in ring.offset(-s).stroke_ring(2.0 * s) {
            soft.poly(&piece);
        }
        soft.paint(window, alpha(peri, 0.18 * shown));
        if frame.focus_t > 0.0 {
            let mut crisp = Fill::new();
            for piece in ring.offset(0.5 * s).stroke_ring(s) {
                crisp.poly(&piece);
            }
            crisp.paint(window, alpha(palette.peri_hi.into(), frame.focus_t));
        }
        let mut fill = Fill::new();
        fill.poly(&Poly::rect(x0 + x - w * 0.5, base - h, w, h));
        fill.paint(window, alpha(peri, shown));
    }
    positions
}

/// A capsule (a stadium): `width` wide from `top` to `bottom`, round ends.
fn capsule(cx: f32, top: f32, bottom: f32, width: f32) -> Poly {
    let r = (width * 0.5).min((bottom - top) * 0.5).max(0.0);
    let mut points = Vec::with_capacity(20);
    // Clockwise on screen: the top arc left to right, then the bottom arc.
    for k in 0..=8_u8 {
        let a = std::f32::consts::PI * (1.0 + f32::from(k) / 8.0);
        points.push(crate::paint::geom::pt(cx + r * a.cos(), top + r + r * a.sin()));
    }
    for k in 0..=8_u8 {
        let a = std::f32::consts::PI * (f32::from(k) / 8.0);
        points.push(crate::paint::geom::pt(cx + r * a.cos(), bottom - r + r * a.sin()));
    }
    Poly::new(points)
}

/// The position of fractional release `at` among `positions`.
fn interpolate(positions: &[f32], at: f32) -> f32 {
    if positions.is_empty() {
        return 0.0;
    }
    let last = positions.len() - 1;
    #[allow(clippy::cast_precision_loss)]
    let at = at.clamp(0.0, last as f32);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lo = at.floor() as usize;
    let hi = (lo + 1).min(last);
    #[allow(clippy::cast_precision_loss)]
    let t = at - lo as f32;
    positions[lo] + (positions[hi] - positions[lo]) * t
}

/// Where a release's tip hangs: from the tick, below the comb — and below
/// the "viewing" line when it shows (`below` px) — so a tip never covers the
/// name and version above, nor the line that says where you are.
fn tip_anchor(bounds: Bounds<Pixels>, x: f32, below: f32) -> Bounds<Pixels> {
    Bounds::new(
        point(bounds.origin.x + px(x - 1.0), bounds.origin.y),
        size(px(2.0), bounds.size.height + px(below)),
    )
}

/// Lets the comb's tip go: a hover tip leaves (the float layer's own
/// intent), a key's tip closes.
fn drop_tip(key: &ElementId, keyboard: bool, window: &mut Window, cx: &mut App) {
    float::leave(key, window, cx);
    if keyboard {
        float::close(key, window, cx);
    }
}

/// The tip's content: the version in mono, then its age.
fn tip_content(release: &Release) -> impl Fn(&Measure, &mut Window, &mut App) -> AnyElement + 'static {
    let version = release.version.clone();
    let age = release.age.clone();
    move |measure: &Measure, _window: &mut Window, cx: &mut App| {
        let palette = cx.facet().palette();
        let s = measure.scale();
        div()
            .flex()
            .items_baseline()
            .gap(px(5.0 * s))
            .px(px(9.0 * s))
            .py(px(5.0 * s))
            .whitespace_nowrap()
            .child(div().set(TIP_MONO, measure).text_color(palette.ink0.hsla()).child(version.clone()))
            .child(div().set(TIP_TEXT, measure).text_color(palette.ink4.hsla()).child("·"))
            .child(div().set(TIP_TEXT, measure).text_color(palette.ink2.hsla()).child(age.clone()))
            .into_any_element()
    }
}

impl RenderOnce for VersionComb {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let s = measure.scale();
        let id = self.id.clone();
        let releases = self.releases.clone();
        let n = releases.len();
        let state = window.use_keyed_state(id.clone(), cx, |_, cx| CombState {
            focus: cx.focus_handle().tab_stop(true),
            lens: None,
            hot: None,
            dragging: false,
            tip: None,
            tip_at: None,
            tip_keyboard: false,
            sheet_tip: Rc::new(Cell::new(false)),
            drawn: Rc::new(Cell::new(None)),
            chose: Rc::new(Cell::new(None)),
            last_bounds: Rc::new(Cell::new(Bounds::default())),
        });
        let motion = Motion::scoped(ElementId::View(state.entity_id()), cx);
        let (focus, lens, hot, dragging) = {
            let st = state.read(cx);
            (
                st.focus.clone(),
                st.lens.or(self.lens_look.map(|centre| Lens { centre })),
                st.hot.or(self.hover_look).filter(|i| *i < n),
                st.dragging,
            )
        };
        let pinned = self.pinned.filter(|i| *i < n);
        let selected = self
            .selected
            .or(pinned)
            .unwrap_or(n.saturating_sub(1))
            .min(n.saturating_sub(1));
        let focused = self.focus_look || (focus.is_focused(window) && window.last_input_was_keyboard());
        // A live comb (no pinned look) tells the probe what it shows.
        let live = !self.focus_look && self.hover_look.is_none() && self.lens_look.is_none();
        let claim = live.then(|| {
            (id.clone(), crate::probe::Target {
                // A drag scrubs wherever the pointer goes: that is the
                // press, not a hover.
                hovered: hot.is_some() && !dragging,
                pressed: dragging,
                focused,
                focusable: true,
                clickable: true,
            })
        });
        let layout = Layout::new(n, f32::from(measure.width()), s);
        let strength = motion.animate(
            track(&id, "lens"),
            if layout.needs_lens() && lens.is_some() { 1.0 } else { 0.0 },
            Spec::Spring(SNAPPY),
            window,
            cx,
        );
        #[allow(clippy::cast_precision_loss)]
        let ring = motion.animate(
            track(&id, "ring"),
            selected as f32,
            // Following a drag it is a spring; stepping by keys or landing
            // after a click it glides and stops exactly on its tick.
            if dragging { Spec::Spring(GRIP) } else { spec::REVEAL },
            window,
            cx,
        );
        let hot_t = motion.animate(
            track(&id, "hot"), if hot.is_some() { 1.0 } else { 0.0 }, spec::HOVER, window, cx);
        let focus_t = motion.animate(
            track(&id, "focus"), if focused { 1.0 } else { 0.0 }, spec::HOVER, window, cx);
        let is_away = pinned.is_some_and(|pin| pin != selected);
        let away = motion.animate(
            track(&id, "away"), if is_away { 1.0 } else { 0.0 }, spec::REVEAL, window, cx);

        let frame = Frame {
            releases: releases.clone(),
            layout: layout.clone(),
            lens,
            strength,
            pinned,
            ring,
            away,
            focus_t,
            hot,
            hot_t,
            touches: self.touches.clone(),
        };
        let (painted, drawn, chose) = {
            let st = state.read(cx);
            (st.last_bounds.clone(), st.drawn.clone(), st.chose.clone())
        };
        // Whatever the comb chooses itself it marks, so the next frame can
        // tell its own moves from the page's.
        let on_select: Option<OnSelect> = self.on_select.clone().map(|handler| {
            let chose = chose.clone();
            let releases = releases.clone();
            Rc::new(move |event: &VersionSelected, window: &mut Window, cx: &mut App| {
                chose.set(releases.iter().position(|r| r.id == event.0));
                handler(event, window, cx);
            }) as OnSelect
        });
        let sheet_tip = self.hover_look.filter(|i| *i < n);
        let sheet_opened = state.read(cx).sheet_tip.clone();
        // Echo: the selection moved and the comb did not move it — the page
        // stepped with `[` / `]` — so the comb says where you are now: the
        // chosen release's tip, for a moment.
        let moved = drawn.get().is_some_and(|was| was != selected);
        let echo = moved && chose.take() != Some(selected) && n > 0 && self.hover_look.is_none();
        drawn.set(Some(selected));
        if echo {
            let key = ElementId::NamedChild(Arc::new(id.clone()), format!("t{selected}").into());
            let release = releases[selected].clone();
            let echo_state = state.clone();
            let bounds = painted.get();
            let layout = Layout::new(n, f32::from(bounds.size.width), s);
            #[allow(clippy::cast_precision_loss)]
            let x = layout.uniform(selected as f32);
            let below = if pinned.is_some_and(|pin| pin != selected) { NOTE_HEIGHT * s } else { 0.0 };
            window.defer(cx, move |window, cx| {
                let old = {
                    let st = echo_state.read(cx);
                    st.tip.clone().zip(Some(st.tip_keyboard))
                };
                if let Some((old, keyboard)) = old.filter(|(old, _)| *old != key) {
                    drop_tip(&old, keyboard, window, cx);
                }
                echo_state.update(cx, |st, cx| {
                    st.tip = Some(key.clone());
                    st.tip_at = Some(selected);
                    st.tip_keyboard = true;
                    cx.notify();
                });
                float::open(
                    FloatRequest::new(key.clone(), tip_anchor(bounds, x, below), FloatKind::Tip, tip_content(&release))
                        .side(Side::Below),
                    window,
                    cx,
                );
                // Let it go after a moment unless something else took over.
                let timer = cx.background_executor().timer(ECHO);
                let closing = echo_state.downgrade();
                window
                    .spawn(cx, async move |cx| {
                        timer.await;
                        let _ = cx.update(|window, cx| {
                            let Some(state) = closing.upgrade() else { return };
                            let current = state.read(cx).tip.clone();
                            if current.as_ref() == Some(&key) && state.read(cx).hot.is_none() {
                                drop_tip(&key, true, window, cx);
                                state.update(cx, |st, cx| {
                                    st.tip = None;
                                    st.tip_at = None;
                                    cx.notify();
                                });
                            }
                        });
                    })
                    .detach();
            });
        }
        let comb = canvas(
            {
                let painted = painted.clone();
                move |bounds, _, cx| {
                    painted.set(bounds);
                    if let Some((key, target)) = &claim {
                        crate::probe::record_target(cx, key, bounds, *target);
                    }
                }
            },
            {
                let state = state.clone();
                let releases = releases.clone();
                let on_select = on_select.clone();
                let id = id.clone();
                move |bounds, (), window, cx| {
                    let positions = Rc::new(paint_comb(&frame, bounds, palette, window));
                    let x0 = f32::from(bounds.origin.x);
                    let below = NOTE_HEIGHT * frame.layout.scale * frame.away;
                    // Keep an open tip hanging from its tick as the comb and
                    // the line under it move.
                    let open = {
                        let st = state.read(cx);
                        st.tip.clone().zip(st.tip_at)
                    };
                    if let Some((key, i)) = open.filter(|(_, i)| *i < positions.len()) {
                        float::anchor(&key, tip_anchor(bounds, positions[i], below), window, cx);
                    }
                    if let Some(i) = sheet_tip {
                        // A state sheet shows the hovered state with its tip.
                        let key = ElementId::NamedChild(Arc::new(id.clone()), format!("t{i}").into());
                        let anchor = tip_anchor(bounds, positions[i], below);
                        float::anchor(&key, anchor, window, cx);
                        if !sheet_opened.replace(true) {
                            let request =
                                FloatRequest::new(key, anchor, FloatKind::Tip, tip_content(&releases[i]))
                                    .side(Side::Below);
                            window.defer(cx, move |window, cx| float::open(request, window, cx));
                        }
                    }
                    // Hover is reconciled, not only event-driven: when the
                    // comb moved from under a still pointer (a resize, a
                    // scroll) or the pointer left the window, let go.
                    watch_pointer(window);
                    let pointer_gone = pointer_away(window, cx) || !bounds.contains(&window.mouse_position());
                    if pointer_gone && live_hover(&state, cx) {
                        let state = state.clone();
                        window.defer(cx, move |window, cx| let_go(&state, false, window, cx));
                    }
                    let left = state.clone();
                    window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble && live_hover(&left, cx) {
                            let_go(&left, false, window, cx);
                        }
                    });
                    let moved = state.clone();
                    let rel = releases.clone();
                    let select = on_select.clone();
                    let comb_id = id.clone();
                    let layout = frame.layout.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let inside = bounds.contains(&event.position);
                        let st = moved.read(cx);
                        if !inside && !st.dragging {
                            if st.hot.is_some() || st.lens.is_some() || st.tip.is_some() {
                                let_go(&moved, true, window, cx);
                            }
                            return;
                        }
                        let x = f32::from(event.position.x) - x0;
                        let lens = layout.needs_lens().then(|| layout.follow(st.lens, x));
                        // Hit against where the ticks stand under the lens
                        // as it now is (fully open).
                        let targets = lens.map(|lens| layout.positions(Some(&lens), 1.0));
                        let hot = nearest(targets.as_deref().unwrap_or(&positions), x);
                        let (was, dragging, old_tip, old_keyboard) =
                            (st.hot, st.dragging, st.tip.clone(), st.tip_keyboard);
                        let tip_key = hot.map(|i| {
                            ElementId::NamedChild(Arc::new(comb_id.clone()), format!("t{i}").into())
                        });
                        moved.update(cx, |st, cx| {
                            st.lens = lens;
                            st.hot = hot;
                            st.tip = tip_key.clone().filter(|_| !dragging);
                            st.tip_at = hot.filter(|_| !dragging);
                            st.tip_keyboard = false;
                            cx.notify();
                        });
                        if was != hot {
                            if let Some(old) = old_tip {
                                drop_tip(&old, old_keyboard, window, cx);
                            }
                            // While dragging the header and the "viewing" line
                            // already name the release: no tip over them.
                            if let (Some(i), Some(key), false) = (hot, tip_key, dragging) {
                                let at = targets.as_ref().map_or(positions[i], |t| t[i]);
                                let anchor = tip_anchor(bounds, at, below);
                                float::rest(
                                    FloatRequest::new(key, anchor, FloatKind::Tip, tip_content(&rel[i]))
                                        .side(Side::Below),
                                    window,
                                    cx,
                                );
                            }
                            if dragging
                                && let (Some(i), Some(select)) = (hot, &select)
                            {
                                select(&VersionSelected(rel[i].id.clone()), window, cx);
                            }
                        }
                    });
                    let released = state.clone();
                    window.on_mouse_event(move |_event: &MouseUpEvent, phase, _window, cx| {
                        if phase == DispatchPhase::Bubble && released.read(cx).dragging {
                            released.update(cx, |st, cx| {
                                st.dragging = false;
                                cx.notify();
                            });
                        }
                    });
                }
            },
        )
        .w_full()
        .h(px(24.0 * s));

        // The quiet line while you read a release you do not pin.
        let note = pinned.filter(|_| away > 0.001).map(|pin| {
            let viewing = releases[selected].version.clone();
            let pinned_version = releases[pin].version.clone();
            let back_to = releases[pin].id.clone();
            let select = on_select.clone();
            div()
                .id("away")
                .flex()
                .items_center()
                .gap(px(6.0 * s))
                .mt(px(10.0 * s * away))
                .h(px((NOTE_HEIGHT - 10.0) * s * away))
                .overflow_hidden()
                .opacity(away)
                .child(div().set(NOTE, &measure).text_color(palette.ink3.hsla()).child("viewing"))
                .child(
                    div()
                        .set(NOTE_MONO, &measure)
                        .text_color(palette.peri_hi.hsla())
                        .child(viewing),
                )
                .child(div().set(NOTE, &measure).text_color(palette.ink4.hsla()).child("·"))
                .child(
                    div()
                        .id("pin")
                        .flex()
                        .items_center()
                        .gap(px(6.0 * s))
                        .cursor_pointer()
                        .child(div().set(NOTE, &measure).text_color(palette.ink3.hsla()).child("you pin"))
                        .child(
                            div()
                                .set(NOTE_MONO, &measure)
                                .text_color(palette.mint.base.hsla())
                                .child(pinned_version),
                        )
                        .on_click(move |_, window, cx| {
                            if let Some(select) = &select {
                                select(&VersionSelected(back_to.clone()), window, cx);
                            }
                        }),
                )
                .child(div().flex_1())
                .child(kbd("esc", &measure).voice(KbdVoice::Quiet).size(KbdSize::Small))
        });

        let down_state = state.clone();
        let down_releases = releases.clone();
        let down_select = on_select.clone();
        let down_painted = painted.clone();
        let key_releases = releases.clone();
        let key_select = on_select.clone();
        let key_state = state.clone();
        let key_painted = painted.clone();
        let key_id = id.clone();
        div()
            .id(id.clone())
            .flex()
            .flex_col()
            .w(measure.width())
            .track_focus(&focus)
            .tab_index(0)
            .child(
                div()
                    .id("ticks")
                    .w_full()
                    .cursor_pointer()
                    .child(comb)
                    .on_mouse_down(MouseButton::Left, move |event: &MouseDownEvent, window, cx| {
                        let bounds = down_painted.get();
                        let x = f32::from(event.position.x - bounds.origin.x);
                        let layout = Layout::new(down_releases.len(), f32::from(bounds.size.width), s);
                        let lens = down_state.read(cx).lens.filter(|_| layout.needs_lens());
                        let at = nearest(&layout.positions(lens.as_ref(), 1.0), x);
                        let (tip, keyboard) = {
                            let st = down_state.read(cx);
                            (st.tip.clone(), st.tip_keyboard)
                        };
                        if let Some(tip) = tip {
                            drop_tip(&tip, keyboard, window, cx);
                        }
                        down_state.update(cx, |st, cx| {
                            st.dragging = true;
                            st.hot = at;
                            st.tip = None;
                            st.tip_at = None;
                            cx.notify();
                        });
                        if let (Some(i), Some(select)) = (at, &down_select) {
                            select(&VersionSelected(down_releases[i].id.clone()), window, cx);
                        }
                    }),
            )
            .children(note)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let key = event.keystroke.key.as_str();
                let next = if key == "escape" {
                    let back = pinned.filter(|pin| *pin != selected);
                    if back.is_none() {
                        // Already home: Esc lets the tip go and carries on
                        // (the float layer's step back, the shell's).
                        let (tip, keyboard) = {
                            let st = key_state.read(cx);
                            (st.tip.clone(), st.tip_keyboard)
                        };
                        if let Some(tip) = tip {
                            drop_tip(&tip, keyboard, window, cx);
                            key_state.update(cx, |st, _| {
                                st.tip = None;
                                st.tip_at = None;
                            });
                        }
                    }
                    back
                } else {
                    step_key(&key_releases, selected, key)
                };
                if let Some(next) = next {
                    if next != selected
                        && let Some(select) = &key_select
                    {
                        select(&VersionSelected(key_releases[next].id.clone()), window, cx);
                    }
                    // Keys scrub without a pointer: the tip follows the
                    // chosen release (no hover delay), and the pointer's
                    // hover lets go.
                    let bounds = key_painted.get();
                    let layout = Layout::new(key_releases.len(), f32::from(bounds.size.width), s);
                    #[allow(clippy::cast_precision_loss)]
                    let x = layout.uniform(next as f32);
                    let away = pinned.is_some_and(|pin| pin != next);
                    let below = if away { NOTE_HEIGHT * s } else { 0.0 };
                    let tip = ElementId::NamedChild(Arc::new(key_id.clone()), format!("t{next}").into());
                    let (old, old_keyboard) = {
                        let st = key_state.read(cx);
                        (st.tip.clone(), st.tip_keyboard)
                    };
                    if let Some(old) = old.filter(|old| *old != tip) {
                        drop_tip(&old, old_keyboard, window, cx);
                    }
                    key_state.update(cx, |st, cx| {
                        st.hot = None;
                        st.lens = None;
                        st.tip = Some(tip.clone());
                        st.tip_at = Some(next);
                        st.tip_keyboard = true;
                        cx.notify();
                    });
                    float::open(
                        FloatRequest::new(tip, tip_anchor(bounds, x, below), FloatKind::Tip, tip_content(&key_releases[next]))
                            .side(Side::Below),
                        window,
                        cx,
                    );
                    cx.stop_propagation();
                }
            })
    }
}

/// The releases a person cannot reach by pointer on `layout`. A person goes to a
/// release the way one reaches anything in a fisheye: sweep towards where
/// it sits (the lens slides along), then steer by eye — half a px at a time
/// towards where the tick is now drawn — until its tip shows. Every release
/// must be reachable that way from either side, within a short steer.
#[cfg(test)]
pub(crate) fn unreachable(layout: &Layout) -> Vec<usize> {
    let width = layout.width;
    let reach = |target: usize, from_left: bool| {
        #[allow(clippy::cast_precision_loss)]
        let goal = layout.uniform(target as f32);
        let mut lens: Option<Lens> = None;
        let mut x = if from_left { 0.0 } else { width };
        let step = if from_left { 1.0 } else { -1.0 };
        while (from_left && x < goal) || (!from_left && x > goal) {
            lens = layout.needs_lens().then(|| layout.follow(lens, x));
            x += step;
        }
        for _ in 0..(width as usize * 2).max(240) {
            lens = layout.needs_lens().then(|| layout.follow(lens, x));
            let positions = layout.positions(lens.as_ref(), 1.0);
            if nearest(&positions, x) == Some(target) {
                return true;
            }
            x = (x + (positions[target] - x).signum() * 0.5).clamp(0.0, width);
        }
        false
    };
    (0..layout.n).filter(|&r| !reach(r, true) || !reach(r, false)).collect()
}

#[cfg(test)]
mod tests {
    use super::{Layout, Lens, Release, ReleaseId, ReleaseStep, Step, nearest, step_key, step_release};

    fn releases(n: usize) -> Vec<Release> {
        (0..n)
            .map(|i| Release {
                id: ReleaseId(format!("r{i}").into()),
                version: format!("1.{}.{}", i / 10, i % 10).into(),
                step: if i % 10 == 0 { Step::Minor } else { Step::Patch },
                age: "".into(),
            })
            .collect()
    }

    #[test]
    fn steps_read_from_versions() {
        assert_eq!(Step::of("2.0.0"), Step::Major);
        assert_eq!(Step::of("1.4.0"), Step::Minor);
        assert_eq!(Step::of("1.4.2"), Step::Patch);
        assert_eq!(Step::of("1.4.0-rc.1"), Step::Minor);
    }

    #[test]
    fn a_short_history_spans_the_comb_end_to_end() {
        let layout = Layout::new(17, 236.0, 1.0);
        let positions = layout.positions(None, 0.0);
        assert!(positions[0].abs() < 1e-4 && (positions[16] - 236.0).abs() < 1e-3);
        assert!(!layout.needs_lens() && !layout.banded());
    }

    #[test]
    fn four_hundred_releases_band_and_every_one_is_reachable_through_the_lens() {
        let layout = Layout::new(400, 236.0, 1.0);
        assert!(layout.banded() && layout.needs_lens(), "pitch {}", layout.pitch);
        let missed = super::unreachable(&layout);
        assert!(missed.is_empty(), "unreachable releases: {missed:?}");
    }

    #[test]
    fn the_lens_spreads_its_plateau_keeps_order_and_never_moves_the_ends() {
        let layout = Layout::new(400, 236.0, 1.0);
        let lens = layout.follow(None, 118.0);
        let positions = layout.positions(Some(&lens), 1.0);
        assert!(positions.windows(2).all(|w| w[1] >= w[0] - 1e-3), "not monotonic");
        let at = nearest(&positions, 118.0).unwrap();
        let gap = positions[at + 1] - positions[at];
        assert!(gap >= 5.0, "plateau pitch {gap}");
        assert!(positions[0].abs() < 1e-3 && (positions[399] - 236.0).abs() < 1e-3, "ends moved");
        // Outside the lens's reach nothing moves.
        let flat = layout.positions(None, 0.0);
        assert!((positions[5] - flat[5]).abs() < 1e-3);
    }

    #[test]
    fn keys_step_releases_minors_and_ends() {
        let rs = releases(35);
        assert_eq!(step_key(&rs, 12, "right"), Some(13));
        assert_eq!(step_key(&rs, 0, "left"), Some(0));
        assert_eq!(step_key(&rs, 12, "pageup"), Some(20), "next minor");
        assert_eq!(step_key(&rs, 12, "pagedown"), Some(10), "previous minor");
        assert_eq!(step_key(&rs, 12, "home"), Some(0));
        assert_eq!(step_key(&rs, 12, "end"), Some(34));
        assert_eq!(step_key(&rs, 31, "pageup"), Some(34), "no minor ahead: newest");
        assert_eq!(step_key(&[], 0, "right"), None);
        // The page's `[` / `]` are the same steps as the comb's arrows.
        assert_eq!(step_key(&rs, 12, "["), Some(11));
        assert_eq!(step_key(&rs, 12, "]"), Some(13));
        assert_eq!(step_key(&rs, 34, "]"), Some(34), "newest stays newest");
        assert_eq!(step_key(&rs, 12, "x"), None, "other keys pass through");
        assert_eq!(step_release(&rs, 99, ReleaseStep::Older), Some(33), "a stale index clamps first");
    }
}

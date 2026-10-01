//! The styled combs: Rider, Baseline and Band (see the parent module's docs).
//!
//! They share the plain comb's state, lens, keys, tips and "viewing" line;
//! what differs is where the ticks stand (a [`Geometry`]), how they are
//! painted, and the number, which is an element over the comb: an odometer
//! whose changed semver wheels roll, with a mint (pin), periwinkle (viewing)
//! or quiet (not in your tree) tooth at its left edge.

use super::super::GRIP;
use super::super::kbd::{KbdSize, KbdVoice, kbd};
use super::super::state::{pointer_away, track, track_n, watch_pointer};
use super::{
    CombState, CombStyle, Layout, Lens, NOTE, NOTE_HEIGHT, NOTE_MONO, OnSelect, Release,
    VersionComb, VersionSelected, alpha, drop_tip, interpolate, let_go, live_hover, nearest,
    step_key, tip_anchor, tip_content,
};
use crate::Set;
use crate::marks::semver::{self, Tick};
use crate::measure::Measure;
use crate::motion::{Motion, SNAPPY, Spec, Spring, spec};
use crate::overlay::float::{self, FloatKind, FloatRequest, Side};
use crate::paint::geom::{Fill, Poly};
use crate::paint::mix;
use crate::probe::{self, TextOverflow};
use crate::theme::ActiveFacet;
use crate::tokens::motion::Bezier;
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{
    AnyElement, App, Bounds, DispatchPhase, ElementId, Hsla, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled, Window, canvas, div,
    px,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// The scrub head's curve: steady through the middle, so every release it
/// crosses is felt (MOTION.md's *timeline*).
const TIMELINE: Bezier = Bezier {
    x1: 0.4,
    y1: 0.0,
    x2: 0.2,
    y2: 1.0,
};

/// Odometer wheels (MOTION.md's REEL).
const REEL: Spring = Spring {
    response: 0.24,
    damping: 0.92,
};

/// The number on the comb.
const NUMBER: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 500.0,
    size: 13.0,
    line: 14.0,
    tracking: 0.0,
    italic: false,
};

/// The newest release, faint at the comb's end.
const LATEST: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 400.0,
    size: 11.0,
    line: 12.0,
    tracking: 0.0,
    italic: false,
};

/// How long the head takes to walk `releases` releases.
#[must_use]
pub(crate) fn walk(releases: usize) -> Duration {
    #[allow(clippy::cast_possible_truncation)]
    let ms = (18 * releases as u64 + 120).clamp(240, 640);
    Duration::from_millis(ms)
}

/// Where the ticks of a styled comb stand, relative to the comb's left.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Geometry {
    /// The releases' own layout (lens included), without the number.
    pub layout: Layout,
    /// The head: the number's release, fractional while it walks.
    pub head: f32,
    /// The room the riding number takes out of the comb.
    pub gap: f32,
    /// Where the ticks start (after a number that sits still).
    pub offset: f32,
}

impl Geometry {
    pub(crate) fn new(
        style: CombStyle,
        n: usize,
        width: f32,
        scale: f32,
        number: f32,
        head: f32,
    ) -> Self {
        match style {
            CombStyle::Rider => Self {
                layout: Layout::new(n, width - number, scale),
                head,
                gap: number,
                offset: 0.0,
            },
            CombStyle::Baseline | CombStyle::Band => {
                let offset = number + 12.0 * scale;
                Self {
                    layout: Layout::new(n, width - offset, scale),
                    head,
                    gap: 0.0,
                    offset,
                }
            }
            CombStyle::Plain => Self {
                layout: Layout::new(n, width, scale),
                head,
                gap: 0.0,
                offset: 0.0,
            },
        }
    }

    /// Every release's x. After the head they stand a number's width on;
    /// the release the head is crossing moves across under the number.
    pub(crate) fn xs(&self, lens: Option<&Lens>, strength: f32) -> Vec<f32> {
        self.layout
            .positions(lens, strength)
            .iter()
            .enumerate()
            .map(|(i, x)| {
                #[allow(clippy::cast_precision_loss)]
                let past = (i as f32 - self.head).clamp(0.0, 1.0);
                self.offset + x + self.gap * past
            })
            .collect()
    }

    /// The number's left edge (the rider's tooth).
    pub(crate) fn head_x(&self, lens: Option<&Lens>, strength: f32) -> f32 {
        if self.gap > 0.0 {
            self.offset + interpolate(&self.layout.positions(lens, strength), self.head)
        } else {
            0.0
        }
    }

    /// Whether `x` is under the riding number (its ticks are not drawn).
    pub(crate) fn under_number(&self, x: f32, head_x: f32) -> bool {
        self.gap > 0.0 && x > head_x + 0.5 && x < head_x + self.gap - 0.5
    }

    /// The pointer at `x` in the layout's own coordinates (for the lens).
    pub(crate) fn to_layout(&self, x: f32, head_x: f32) -> f32 {
        let x = x - self.offset;
        let head = head_x - self.offset;
        if self.gap <= 0.0 || x <= head {
            x
        } else if x >= head + self.gap {
            x - self.gap
        } else {
            head
        }
    }

    /// The release the pointer at `x` means: the head itself on the number.
    pub(crate) fn hit(&self, xs: &[f32], head_x: f32, x: f32) -> Option<usize> {
        if self.gap > 0.0 && x >= head_x && x <= head_x + self.gap {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            return Some((self.head.round().max(0.0) as usize).min(xs.len().saturating_sub(1)));
        }
        nearest(xs, x)
    }
}

/// One frame of a styled comb.
#[derive(Clone)]
struct Frame {
    style: CombStyle,
    xs: Rc<[f32]>,
    kinds: Rc<[Tick]>,
    yanked: Rc<[bool]>,
    also: Rc<[usize]>,
    pinned: Option<usize>,
    head: usize,
    head_x: f32,
    geometry: Geometry,
    hot: Option<usize>,
    hot_t: f32,
    away: f32,
    focus_t: f32,
    ring: f32,
    dense: bool,
    scale: f32,
}

fn paint(frame: &Frame, bounds: Bounds<Pixels>, palette: &Palette, window: &mut Window) {
    let s = frame.scale;
    let x0 = f32::from(bounds.origin.x);
    let base = f32::from(bounds.origin.y + bounds.size.height) - 3.0 * s;
    let n = frame.xs.len();
    let ink2: Hsla = palette.ink2.into();
    let ink3: Hsla = palette.ink3.into();
    let ink1: Hsla = palette.ink1.into();
    let ink0: Hsla = palette.ink0.into();
    let mint: Hsla = palette.mint.base.into();
    let peri: Hsla = palette.peri.base.into();
    let rect = |fill: &mut Fill, x: f32, w: f32, h: f32| {
        fill.poly(&Poly::rect(x0 + x - w * 0.5, base - h, w, h))
    };
    if frame.style == CombStyle::Band {
        // One hairline from the first release to the newest.
        if n > 1 {
            let mut line = Fill::new();
            line.poly(&Poly::rect(
                x0 + frame.xs[0],
                base - s,
                frame.xs[n - 1] - frame.xs[0],
                s,
            ));
            line.paint(window, alpha(palette.ink4.into(), 0.85));
        }
        for i in 0..n {
            if frame.kinds[i] != Tick::Breaking
                || Some(i) == frame.pinned
                || frame.also.contains(&i)
            {
                continue;
            }
            let after = frame.pinned.is_some_and(|p| i > p);
            let mut fill = Fill::new();
            rect(&mut fill, frame.xs[i], s, 6.0 * s);
            fill.paint(
                window,
                alpha(
                    if after { ink1 } else { ink3 },
                    if after { 0.9 } else { 0.7 },
                ),
            );
        }
        for &i in frame.also.iter() {
            hollow(window, x0 + frame.xs[i], base, 3.0 * s, 8.0 * s, s, mint);
        }
        if let Some(pin) = frame.pinned {
            let mut fill = Fill::new();
            rect(&mut fill, frame.xs[pin], 2.0 * s, 10.0 * s);
            fill.paint(window, mint);
        }
        if frame.away > 0.001 {
            let x = interpolate(&frame.xs, frame.ring);
            let mut fill = Fill::new();
            rect(&mut fill, x, 2.0 * s, 12.0 * s);
            fill.paint(window, alpha(peri, frame.away));
        }
        return;
    }
    let after_span = frame.pinned.map_or(1.0, |p| {
        (n.saturating_sub(1).saturating_sub(p)).max(1) as f32
    });
    let hair = if frame.dense { s.max(1.0) } else { 1.5 * s };
    for i in 0..n {
        let x = frame.xs[i];
        // The number is this release's tooth; the also teeth draw their own.
        let is_head = frame.style == CombStyle::Rider && i == frame.head;
        if is_head || frame.geometry.under_number(x, frame.head_x) || frame.also.contains(&i) {
            continue;
        }
        if frame.style == CombStyle::Baseline && Some(i) == frame.pinned {
            continue;
        }
        let kind = frame.kinds[i];
        let after = frame.pinned.is_some_and(|p| i > p);
        #[allow(clippy::cast_precision_loss)]
        let fade = frame.pinned.map_or(0.62, |p| {
            0.62 - 0.4 * ((i.saturating_sub(p)) as f32 / after_span)
        });
        let (w, h, mut ink, mut a) = match kind {
            Tick::Pre => (hair, 5.0 * s, ink3, 0.6),
            Tick::Patch => (hair, 4.0 * s, ink3, if frame.dense { 0.22 } else { 0.42 }),
            Tick::Minor => (hair, 8.0 * s, ink3, 0.62),
            Tick::Breaking if after => (2.0 * s, 17.0 * s, ink1, 0.9),
            Tick::Breaking => (hair, 13.0 * s, ink2, 0.9),
        };
        if after && kind != Tick::Breaking {
            a = fade * if frame.dense { 0.7 } else { 1.0 };
        }
        if frame.hot == Some(i) {
            ink = mix(ink, ink0, frame.hot_t);
            a = a + (1.0 - a) * frame.hot_t;
        }
        if frame.yanked.get(i).copied().unwrap_or(false) {
            hollow(window, x0 + x, base, 4.0 * s, 4.0 * s, s, alpha(ink3, 0.8));
            continue;
        }
        let mut fill = Fill::new();
        if kind == Tick::Pre {
            // Hatched: a pre-release is only half a release.
            let mut y = 0.0;
            while y < h {
                fill.poly(&Poly::rect(x0 + x - w * 0.5, base - y - s, w, s));
                y += 2.0 * s;
            }
        } else {
            rect(&mut fill, x, w, h);
        }
        fill.paint(window, alpha(ink, a));
    }
    for &i in frame.also.iter() {
        hollow(window, x0 + frame.xs[i], base, 4.0 * s, 15.0 * s, s, mint);
    }
    // The pin stays home while you read another release.
    if let Some(pin) = frame.pinned {
        let away_from_pin = frame.style == CombStyle::Baseline || pin != frame.head;
        if away_from_pin && !frame.also.contains(&pin) {
            let mut fill = Fill::new();
            rect(&mut fill, frame.xs[pin], 2.0 * s, 17.0 * s);
            fill.paint(window, mint);
        }
    }
    if frame.style == CombStyle::Baseline {
        // The release being read: periwinkle, in a soft ring.
        let shown = frame.away.max(frame.focus_t);
        if shown > 0.001 {
            let x = interpolate(&frame.xs, frame.ring);
            let (w, h) = (2.0 * s, 17.0 * s);
            let ring = Poly::rect(
                x0 + x - w * 0.5 - 2.0 * s,
                base - h - 2.0 * s,
                w + 4.0 * s,
                h + 2.0 * s,
            );
            let mut soft = Fill::new();
            for piece in ring.offset(-s).stroke_ring(2.0 * s) {
                soft.poly(&piece);
            }
            soft.paint(window, alpha(peri, 0.18 * shown));
            let mut fill = Fill::new();
            rect(&mut fill, x, w, h);
            fill.paint(window, alpha(peri, shown));
        }
    }
}

/// A hollow tooth `w` by `h`, standing on `base`, centred on `x`.
fn hollow(window: &mut Window, x: f32, base: f32, w: f32, h: f32, s: f32, ink: Hsla) {
    let outline = Poly::rect(x - w * 0.5, base - h, w, h);
    let mut fill = Fill::new();
    for piece in outline.offset(-0.5 * s).stroke_ring(s) {
        fill.poly(&piece);
    }
    fill.paint(window, ink);
}

/// The version text split into wheels and separators (`0`, `.`, `8`, …).
fn wheels(version: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut run = String::new();
    for c in version.chars() {
        if matches!(c, '.' | '-' | '+') {
            if !run.is_empty() {
                out.push(std::mem::take(&mut run));
            }
            out.push(c.to_string());
        } else {
            run.push(c);
        }
    }
    if !run.is_empty() {
        out.push(run);
    }
    out
}

/// The odometer's memory: what it shows, and the wheels still rolling
/// (their old text) with the direction they roll in.
#[derive(Default)]
struct Odometer {
    shown: String,
    at: usize,
    rolling: Vec<Option<String>>,
    /// Later (1): the new text comes up from below; earlier (-1): down.
    up: bool,
}

/// The number: tooth and odometer, `advance` px per character.
#[allow(clippy::too_many_arguments)]
fn number(
    id: &ElementId,
    version: &str,
    at: usize,
    tooth: Hsla,
    ink: Hsla,
    advance: f32,
    measure: &Measure,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let s = measure.scale();
    let odo = window.use_keyed_state(track(id, "odometer"), cx, |_, _| {
        RefCell::new(Odometer::default())
    });
    let motion = Motion::scoped(ElementId::View(odo.entity_id()), cx);
    let parts = wheels(version);
    {
        let odo = odo.read(cx);
        let mut o = odo.borrow_mut();
        if o.shown != version {
            let old = wheels(&o.shown);
            if !o.shown.is_empty() {
                o.up = at > o.at;
                if old.len() == parts.len() {
                    o.rolling = old
                        .iter()
                        .zip(&parts)
                        .map(|(a, b)| (a != b).then(|| a.clone()))
                        .collect();
                } else {
                    // A different shape (a pre-release tag): one wheel.
                    o.rolling = vec![Some(o.shown.clone())];
                }
                for (j, wheel) in o.rolling.iter().enumerate() {
                    if wheel.is_some() {
                        motion.set(track_n(id, "wheel", j), 0.0);
                    }
                }
            }
            o.shown = version.to_owned();
            o.at = at;
        }
    }
    let line = 14.0 * s;
    let (rolling, up) = {
        let odo = odo.read(cx);
        let o = odo.borrow();
        (o.rolling.clone(), o.up)
    };
    let whole = rolling.len() == 1 && parts.len() > 1;
    let wheel = |j: usize,
                 new: &str,
                 old: Option<&String>,
                 color: Hsla,
                 window: &mut Window,
                 cx: &mut App|
     -> AnyElement {
        let Some(old) = old else {
            return div()
                .h(px(line))
                .text_color(color)
                .child(new.to_owned())
                .into_any_element();
        };
        let v = motion
            .animate(track_n(id, "wheel", j), 1.0, Spec::Spring(REEL), window, cx)
            .clamp(0.0, 1.0);
        if v >= 0.999 {
            return div()
                .h(px(line))
                .text_color(color)
                .child(new.to_owned())
                .into_any_element();
        }
        #[allow(clippy::cast_precision_loss)]
        let width = advance * new.chars().count().max(old.chars().count()) as f32;
        let sign = if up { 1.0 } else { -1.0 };
        div()
            .relative()
            .overflow_hidden()
            .h(px(line))
            .w(px(width))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(-sign * v * line))
                    .text_color(color)
                    .child(old.clone()),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(sign * (1.0 - v) * line))
                    .text_color(color)
                    .child(new.to_owned()),
            )
            .into_any_element()
    };
    let mut digits = div().flex().items_end().set(NUMBER, measure);
    if whole {
        digits = digits.child(wheel(0, version, rolling[0].as_ref(), ink, window, cx));
    } else {
        for (j, part) in parts.iter().enumerate() {
            let separator = matches!(part.as_str(), "." | "-" | "+");
            let color = if separator { palette.ink3.into() } else { ink };
            let old = rolling.get(j).and_then(Option::as_ref);
            digits = digits.child(wheel(j, part, old, color, window, cx));
        }
    }
    let content: SharedString = version.to_owned().into();
    div()
        .relative()
        .h(px(17.0 * s))
        .flex()
        .items_end()
        .pl(px(advance * 0.75))
        .child(
            div()
                .absolute()
                .left_0()
                .bottom_0()
                .w(px(2.0 * s))
                .h(px(17.0 * s))
                .bg(tooth),
        )
        .child(probe::text(
            track(id, "number"),
            content,
            measure.role(NUMBER),
            1.0,
            TextOverflow::Clip,
            digits,
        ))
        .into_any_element()
}

#[allow(clippy::too_many_lines)]
pub(super) fn render(comb: VersionComb, window: &mut Window, cx: &mut App) -> AnyElement {
    let palette = cx.palette();
    let measure = comb.measure;
    let s = measure.scale();
    let id = comb.id.clone();
    let releases = comb.releases.clone();
    let n = releases.len();
    let style = comb.style;
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
    if n == 0 {
        return div().id(id).into_any_element();
    }
    let motion = Motion::scoped(ElementId::View(state.entity_id()), cx);
    let (focus, lens, hot, dragging, drawn, sheet_opened, painted) = {
        let st = state.read(cx);
        (
            st.focus.clone(),
            st.lens,
            st.hot.filter(|i| *i < n),
            st.dragging,
            st.drawn.clone(),
            st.sheet_tip.clone(),
            st.last_bounds.clone(),
        )
    };
    let pinned = comb.pinned.filter(|i| *i < n);
    let selected = comb.selected.or(pinned).unwrap_or(n - 1).min(n - 1);
    let focused = comb.focus_look || (focus.is_focused(window) && window.last_input_was_keyboard());
    // The head walks from where it was to the release chosen.
    let was = drawn.get();
    let distance = was.map_or(0, |w: usize| w.abs_diff(selected));
    let head_spec = if dragging {
        Spec::Spring(GRIP)
    } else {
        Spec::tween(walk(distance), TIMELINE)
    };
    #[allow(clippy::cast_precision_loss)]
    let head = motion.animate(track(&id, "head"), selected as f32, head_spec, window, cx);
    drawn.set(Some(selected));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let head_at = (head.round().max(0.0) as usize).min(n - 1);
    let shown = releases[head_at].version.clone();
    let advance = f32::from(probe::natural_width(
        &SharedString::new_static("0"),
        measure.role(NUMBER),
        1.0,
        window,
    ));
    #[allow(clippy::cast_precision_loss)]
    let number_w = motion.animate(
        track(&id, "number-w"),
        advance * (shown.chars().count() as f32 + 1.4),
        Spec::Spring(SNAPPY),
        window,
        cx,
    );
    let latest = comb
        .latest
        .filter(|l| *l < n && style == CombStyle::Rider && pinned.is_some_and(|p| *l > p))
        .map(|l| releases[l].version.clone());
    let latest_w = latest.as_ref().map_or(0.0, |v| {
        f32::from(probe::natural_width(v, measure.role(LATEST), 1.0, window)) + 10.0 * s
    });
    let width = (f32::from(measure.width()) - latest_w).max(40.0 * s);
    let height = if style == CombStyle::Band {
        20.0 * s
    } else {
        28.0 * s
    };
    let geometry = Geometry::new(style, n, width, s, number_w, head);
    let strength = motion.animate(
        track(&id, "lens"),
        if geometry.layout.needs_lens() && lens.is_some() {
            1.0
        } else {
            0.0
        },
        Spec::Spring(SNAPPY),
        window,
        cx,
    );
    let xs: Rc<[f32]> = geometry.xs(lens.as_ref(), strength).into();
    let head_x = geometry.head_x(lens.as_ref(), strength);
    let hot_t = motion.animate(
        track(&id, "hot"),
        if hot.is_some() { 1.0 } else { 0.0 },
        spec::HOVER,
        window,
        cx,
    );
    let focus_t = motion.animate(
        track(&id, "focus"),
        if focused { 1.0 } else { 0.0 },
        spec::HOVER,
        window,
        cx,
    );
    let is_away = pinned.is_some_and(|pin| pin != selected);
    let away = motion.animate(
        track(&id, "away"),
        if is_away { 1.0 } else { 0.0 },
        spec::REVEAL,
        window,
        cx,
    );
    let versions: Vec<&str> = releases.iter().map(|r| r.version.as_ref()).collect();
    let kinds: Rc<[Tick]> = semver::kinds(&versions).into();
    let also_idx: Rc<[usize]> = comb.also.iter().map(|t| t.index).collect();
    #[allow(clippy::cast_precision_loss)]
    let dense = n as f32 > width / (3.2 * s);
    let frame = Frame {
        style,
        xs: xs.clone(),
        kinds,
        yanked: comb.yanked.clone(),
        also: also_idx.clone(),
        pinned,
        head: head_at,
        head_x,
        geometry: geometry.clone(),
        hot,
        hot_t,
        away,
        focus_t,
        ring: head,
        dense,
        scale: s,
    };
    let live = !comb.number_look && comb.also_look.is_none();
    let claim = live.then(|| {
        (
            id.clone(),
            crate::probe::Target {
                hovered: hot.is_some() && !dragging,
                pressed: dragging,
                focused,
                focusable: true,
                clickable: true,
            },
        )
    });
    let on_select: Option<OnSelect> = comb.on_select.clone();
    // The tick canvas, with the pointer's hover, lens, tip and drag.
    let canvas_el = canvas(
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
            let geometry = geometry.clone();
            let also_idx = also_idx.clone();
            move |bounds, (), window, cx| {
                paint(&frame, bounds, palette, window);
                let x0 = f32::from(bounds.origin.x);
                let below = NOTE_HEIGHT * s * frame.away;
                let open = {
                    let st = state.read(cx);
                    st.tip.clone().zip(st.tip_at)
                };
                if let Some((key, i)) = open.filter(|(_, i)| *i < frame.xs.len()) {
                    float::anchor(&key, tip_anchor(bounds, frame.xs[i], below), window, cx);
                }
                watch_pointer(window);
                let pointer_gone =
                    pointer_away(window, cx) || !bounds.contains(&window.mouse_position());
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
                let geo = geometry.clone();
                let head_x = frame.head_x;
                let teeth = also_idx.clone();
                let xs_now = frame.xs.clone();
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
                    let lens = geo
                        .layout
                        .needs_lens()
                        .then(|| geo.layout.follow(st.lens, geo.to_layout(x, head_x)));
                    let targets: Vec<f32> =
                        lens.map_or_else(|| xs_now.to_vec(), |lens| geo.xs(Some(&lens), 1.0));
                    let hot = geo.hit(&targets, head_x, x);
                    // On the number or an also tooth, their own cards speak:
                    // no tick tip over them.
                    let on_number = geo.gap > 0.0 && x >= head_x && x <= head_x + geo.gap
                        || geo.offset > 0.0 && x < geo.offset;
                    let on_tooth = teeth
                        .iter()
                        .any(|&t| (targets.get(t).copied().unwrap_or(-1e9) - x).abs() < 5.0 * s);
                    let quiet = on_number || on_tooth;
                    let (was, dragging, old_tip, old_keyboard) =
                        (st.hot, st.dragging, st.tip.clone(), st.tip_keyboard);
                    let tip_key = hot.map(|i| {
                        ElementId::NamedChild(Arc::new(comb_id.clone()), format!("t{i}").into())
                    });
                    moved.update(cx, |st, cx| {
                        st.lens = lens;
                        st.hot = hot;
                        st.tip = tip_key.clone().filter(|_| !dragging && !quiet);
                        st.tip_at = hot.filter(|_| !dragging && !quiet);
                        st.tip_keyboard = false;
                        cx.notify();
                    });
                    if was != hot || quiet {
                        if let Some(old) = old_tip {
                            drop_tip(&old, old_keyboard, window, cx);
                        }
                        if let (Some(i), Some(key), false, false) = (hot, tip_key, dragging, quiet)
                        {
                            let anchor = tip_anchor(bounds, targets[i], below);
                            float::rest(
                                FloatRequest::new(
                                    key,
                                    anchor,
                                    FloatKind::Tip,
                                    tip_content(&rel[i]),
                                )
                                .side(Side::Below),
                                window,
                                cx,
                            );
                        }
                        if dragging
                            && was != hot
                            && let (Some(i), Some(select)) = (hot, &select)
                        {
                            select(&VersionSelected(rel[i].id.clone()), window, cx);
                        }
                    }
                });
                let released = state.clone();
                let up_releases = releases.clone();
                let up_select = on_select.clone();
                let up_geo = geometry.clone();
                let up_xs = frame.xs.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble || !released.read(cx).dragging {
                        return;
                    }
                    let x = f32::from(event.position.x) - x0;
                    let at = up_geo.hit(&up_xs, head_x, x);
                    released.update(cx, |st, cx| {
                        st.dragging = false;
                        cx.notify();
                    });
                    if let (Some(i), Some(select)) = (at, &up_select) {
                        select(&VersionSelected(up_releases[i].id.clone()), window, cx);
                    }
                });
            }
        },
    )
    .w(px(width))
    .h(px(height));

    // The number, riding (Rider) or still at the start.
    let viewing = pinned.is_some_and(|p| p != head_at);
    let tooth: Hsla = match (pinned, viewing) {
        (Some(_), false) => palette.mint.base.into(),
        (Some(_), true) => palette.peri.base.into(),
        (None, _) => palette.ink2.into(),
    };
    let ink: Hsla = if viewing {
        palette.peri_hi.into()
    } else {
        palette.ink0.into()
    };
    let number_el = number(
        &id, &shown, head_at, tooth, ink, advance, &measure, palette, window, cx,
    );
    let number_left = if style == CombStyle::Rider {
        head_x
    } else {
        0.0
    };
    let number_key = track(&id, "number-card");
    let number_el: AnyElement = match comb.number_card.clone() {
        Some(card) => {
            let key = number_key.clone();
            float::trigger(
                number_key.clone(),
                move |bounds| {
                    let card = card.clone();
                    FloatRequest::new(key.clone(), bounds, FloatKind::Peek, move |m, w, cx| {
                        card(m, w, cx)
                    })
                    .unfurl()
                    .hang_from_start()
                },
                number_el,
            )
            .into_any_element()
        }
        None => number_el,
    };
    let mut comb_box = div()
        .id("ticks")
        .relative()
        .w(px(width))
        .h(px(height))
        .cursor_pointer()
        .child(canvas_el)
        .child(
            div()
                .absolute()
                .left(px(number_left))
                .bottom(px(3.0 * s))
                .child(number_el),
        );
    for tooth in comb.also.iter() {
        let key =
            ElementId::NamedChild(Arc::new(id.clone()), format!("also-{}", tooth.index).into());
        let card = tooth.card.clone();
        let request_key = key.clone();
        let x = xs[tooth.index];
        comb_box = comb_box.child(
            div()
                .absolute()
                .left(px(x - 5.0 * s))
                .bottom(px(3.0 * s))
                .child(float::trigger(
                    key,
                    move |bounds| {
                        let card = card.clone();
                        FloatRequest::new(
                            request_key.clone(),
                            bounds,
                            FloatKind::Peek,
                            move |m, w, cx| card(m, w, cx),
                        )
                        .unfurl()
                        .hang_from_start()
                    },
                    div().w(px(10.0 * s)).h(px(18.0 * s)),
                )),
        );
    }
    // State sheets: open the number's or a tooth's card on the first paint.
    let sheet_card: Option<(ElementId, CardAt)> = if comb.number_look {
        comb.number_card.clone().map(|card| {
            (
                number_key.clone(),
                CardAt {
                    card,
                    x: number_left,
                    w: number_w,
                },
            )
        })
    } else {
        comb.also_look.and_then(|i| {
            comb.also.iter().find(|t| t.index == i).map(|t| {
                (
                    ElementId::NamedChild(Arc::new(id.clone()), format!("also-{i}").into()),
                    CardAt {
                        card: t.card.clone(),
                        x: xs[i] - 5.0 * s,
                        w: 10.0 * s,
                    },
                )
            })
        })
    };
    if let Some((key, at)) = sheet_card {
        let opened = sheet_opened.clone();
        comb_box = comb_box.child(
            canvas(
                |_, _, _| {},
                move |bounds, (), window, cx| {
                    if !opened.replace(true) {
                        let anchor = Bounds::new(
                            gpui::point(
                                bounds.origin.x + px(at.x),
                                bounds.origin.y + bounds.size.height - px(20.0 * s),
                            ),
                            gpui::size(px(at.w), px(17.0 * s)),
                        );
                        let card = at.card.clone();
                        let request = FloatRequest::new(
                            key.clone(),
                            anchor,
                            FloatKind::Peek,
                            move |m, w, cx| card(m, w, cx),
                        )
                        .unfurl()
                        .hang_from_start();
                        window.defer(cx, move |window, cx| float::rest(request, window, cx));
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        );
    }
    let down_state = state.clone();
    let down_releases = releases.clone();
    let down_select = on_select.clone();
    let down_painted = painted.clone();
    let down_geo = geometry.clone();
    let down_xs = xs.clone();
    let comb_box = comb_box.on_mouse_down(
        MouseButton::Left,
        move |event: &MouseDownEvent, window, cx| {
            let bounds = down_painted.get();
            let x = f32::from(event.position.x - bounds.origin.x);
            let at = down_geo.hit(&down_xs, head_x, x);
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
        },
    );
    let mut row = div().flex().items_end().child(comb_box);
    if let Some(latest) = latest {
        row = row.child(
            div()
                .pl(px(10.0 * s))
                .pb(px(3.0 * s))
                .set(LATEST, &measure)
                .text_color(palette.ink4.hsla())
                .child(latest),
        );
    }
    // The quiet line while you read a release you do not pin.
    let note = pinned.filter(|_| away > 0.001).map(|pin| {
        // Away, it names where you went (the number rides there); on the way
        // home, while the line rolls away, the release the head is leaving.
        let viewing = releases[if selected != pin { selected } else { head_at }]
            .version
            .clone();
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
            .child(
                div()
                    .set(NOTE, &measure)
                    .text_color(palette.ink3.hsla())
                    .child("viewing"),
            )
            .child(
                div()
                    .set(NOTE_MONO, &measure)
                    .text_color(palette.peri_hi.hsla())
                    .child(viewing),
            )
            .child(
                div()
                    .set(NOTE, &measure)
                    .text_color(palette.ink4.hsla())
                    .child("·"),
            )
            .child(
                div()
                    .id("pin")
                    .flex()
                    .items_center()
                    .gap(px(6.0 * s))
                    .cursor_pointer()
                    .child(
                        div()
                            .set(NOTE, &measure)
                            .text_color(palette.ink3.hsla())
                            .child("you pin"),
                    )
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
            .child(div().w(px(8.0 * s)))
            .child(
                kbd("esc", &measure)
                    .voice(KbdVoice::Quiet)
                    .size(KbdSize::Small),
            )
    });
    let key_releases = releases.clone();
    let key_select = on_select.clone();
    let key_state = state.clone();
    let key_painted = painted.clone();
    let key_id = id.clone();
    let key_xs = xs.clone();
    div()
        .id(id.clone())
        .flex()
        .flex_col()
        .w(measure.width())
        .track_focus(&focus)
        .tab_index(0)
        .child(row)
        .children(note)
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            let key = event.keystroke.key.as_str();
            let next = if key == "escape" {
                let back = pinned.filter(|pin| *pin != selected);
                if back.is_none() {
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
                let bounds = key_painted.get();
                let away = pinned.is_some_and(|pin| pin != next);
                let below = if away { NOTE_HEIGHT * s } else { 0.0 };
                let tip =
                    ElementId::NamedChild(Arc::new(key_id.clone()), format!("t{next}").into());
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
                let x = key_xs.get(next).copied().unwrap_or_default();
                float::open(
                    FloatRequest::new(
                        tip,
                        tip_anchor(bounds, x, below),
                        FloatKind::Tip,
                        tip_content(&key_releases[next]),
                    )
                    .side(Side::Below),
                    window,
                    cx,
                );
                cx.stop_propagation();
            }
        })
        .into_any_element()
}

struct CardAt {
    card: super::CardContent,
    x: f32,
    w: f32,
}

#[allow(dead_code)]
fn _release(_: &Release) {}

#[cfg(test)]
mod tests {
    use super::{Geometry, walk, wheels};
    use crate::controls::comb::CombStyle;
    use std::time::Duration;

    #[test]
    fn the_rider_parts_the_ticks_around_its_number() {
        // Ten releases on 290 px, a 50 px number riding at release 4.
        let g = Geometry::new(CombStyle::Rider, 10, 290.0, 1.0, 50.0, 4.0);
        let xs = g.xs(None, 0.0);
        let head_x = g.head_x(None, 0.0);
        assert!(
            (head_x - xs[4]).abs() < 1e-3,
            "the number is release 4's tooth"
        );
        assert!(
            xs[5] >= head_x + 50.0 - 1e-3,
            "later releases stand clear of the number"
        );
        assert!(
            (xs[9] - 290.0).abs() < 1e-3 && xs[0].abs() < 1e-3,
            "the comb spans its width"
        );
        assert_eq!(
            g.hit(&xs, head_x, head_x + 20.0),
            Some(4),
            "the number means its own release"
        );
        // Half-way to release 5, release 5 is crossing under the number.
        let mid = Geometry::new(CombStyle::Rider, 10, 290.0, 1.0, 50.0, 4.5);
        let (xs, hx) = (mid.xs(None, 0.0), mid.head_x(None, 0.0));
        assert!(
            mid.under_number(xs[5], hx),
            "the crossing tick hides under the number"
        );
    }

    #[test]
    fn the_head_walks_eighteen_ms_a_release_within_bounds() {
        assert_eq!(walk(28), Duration::from_millis(624));
        assert_eq!(walk(1), Duration::from_millis(240));
        assert_eq!(walk(200), Duration::from_millis(640));
        assert_eq!(wheels("1.1.5"), vec!["1", ".", "1", ".", "5"]);
    }
}

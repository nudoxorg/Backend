//! The release ticker: the one place a package's versions live. Every
//! release is a bar on a time line (tall is breaking, mid a feature, short a
//! fix; coral is yanked, mint your pin, amber the newest). Move along it and
//! the bars under the pointer widen apart (a fisheye), the nearest one lifts,
//! and its label rides inside the ticker. Click or drag to travel: the page
//! is read at that release.
//!
//! Releases without a date are spaced evenly between the dated ones around
//! them (all of them evenly when none is dated), so the ticker is honest
//! about what it does not know: a bar with no date never claims a year.
//!
//! The ticker only emits the release chosen ([`Ticker::on_travel`]); the
//! shell decides what it means. Keyboard: ←/→ one release, Home/End the
//! first and newest.

use super::state::{Names, Standing};
use crate::data::text::{Shaped, shape};
use crate::marks::semver::{self, Tick as Kind};
use crate::measure::Measure;
use crate::motion::{Motion, spec};
use crate::paint::geom::{Fill, Poly, pt};
use crate::probe::{self, TextOverflow, TextSample};
use crate::theme::ActiveFacet;
use crate::tokens::{Palette, TypeRole, ty};
use gpui::{
    App, Bounds, DispatchPhase, Element, ElementId, Entity, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseExitEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Style, Window, px,
};
use std::rc::Rc;
use std::sync::Arc;

/// One release on the ticker.
#[derive(Clone, Debug, PartialEq)]
pub struct Tick {
    /// The version as the registry spells it.
    pub version: SharedString,
    /// Its publish date (`2026-01-31`), when known.
    pub date: Option<SharedString>,
    /// Whether the publisher withdrew it.
    pub standing: Standing,
    /// Whether its names are read (in the index); a release that is not is
    /// drawn quieter and says so.
    pub names: Names,
    /// How big a step it was.
    pub kind: Kind,
    /// Days since 1970-01-01, when it has a date.
    pub day: Option<f64>,
}

/// A ticker's data.
#[derive(Clone, Debug, PartialEq)]
pub struct TickerFacts {
    /// The releases, oldest first.
    pub ticks: Vec<Tick>,
    /// The release you pin.
    pub pin: Option<usize>,
    /// The newest release.
    pub latest: Option<usize>,
    /// The release being read, when it is not the pin.
    pub reading: Option<usize>,
    /// Today, `YYYY-MM-DD`.
    pub today: SharedString,
    today_day: f64,
}

/// One release as the registry gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// The version as the registry spells it.
    pub version: String,
    /// Its publish date (`2026-01-31`), when known.
    pub date: Option<String>,
    /// Whether the publisher withdrew it.
    pub standing: Standing,
    /// Whether its names are read.
    pub names: Names,
}

impl TickerFacts {
    /// A ticker from `releases` in any order, your `pin`, and `today`.
    #[must_use]
    pub fn new(releases: &[Release], pin: Option<&str>, today: &str) -> Self {
        let mut sorted: Vec<&Release> = releases.iter().collect();
        sorted.sort_by(|a, b| semver::cmp(&a.version, &b.version));
        let versions: Vec<&str> = sorted.iter().map(|r| r.version.as_str()).collect();
        let kinds = semver::kinds(&versions);
        let ticks: Vec<Tick> = sorted
            .iter()
            .zip(kinds)
            .map(|(r, kind)| Tick {
                version: r.version.clone().into(),
                date: r.date.clone().map(Into::into),
                standing: r.standing,
                names: r.names,
                kind,
                day: r.date.as_deref().and_then(semver::days),
            })
            .collect();
        let pin = pin.and_then(|pin| {
            ticks
                .iter()
                .position(|t| t.version == pin || semver::short(&t.version) == semver::short(pin))
        });
        // The newest is the highest release that is not yanked.
        let latest = ticks
            .iter()
            .rposition(|t| t.standing == Standing::Available && t.kind != Kind::Pre)
            .or_else(|| ticks.len().checked_sub(1));
        Self {
            ticks,
            pin,
            latest,
            reading: None,
            today: today.to_owned().into(),
            today_day: semver::days(today).unwrap_or(0.0),
        }
    }

    /// The release being read (`None`: the pin).
    #[must_use]
    pub fn reading(mut self, version: Option<&str>) -> Self {
        self.reading = version.and_then(|v| {
            self.ticks
                .iter()
                .position(|t| t.version == v || semver::short(&t.version) == semver::short(v))
        });
        self
    }

    /// Whether at least two releases carry a date, so the axis is time.
    #[must_use]
    pub fn dated(&self) -> bool {
        self.ticks.iter().filter(|t| t.day.is_some()).count() >= 2
    }

    /// Each release's x on a line `width` wide, before the fisheye.
    #[must_use]
    pub fn positions(&self, width: f32, pad: f32) -> Vec<f32> {
        let n = self.ticks.len();
        if n == 0 {
            return Vec::new();
        }
        let span = (width - 2.0 * pad).max(1.0);
        if !self.dated() {
            #[allow(clippy::cast_precision_loss)]
            return (0..n)
                .map(|i| {
                    pad + if n == 1 {
                        span * 0.5
                    } else {
                        span * i as f32 / (n - 1) as f32
                    }
                })
                .collect();
        }
        let dated: Vec<f64> = self.ticks.iter().filter_map(|t| t.day).collect();
        let t0 = dated.iter().copied().fold(f64::INFINITY, f64::min);
        let t1 = dated
            .iter()
            .copied()
            .fold(self.today_day, f64::max)
            .max(t0 + 1.0);
        #[allow(clippy::cast_possible_truncation)]
        let at = |d: f64| pad + ((d - t0) / (t1 - t0)) as f32 * span;
        let mut xs: Vec<Option<f32>> = self.ticks.iter().map(|t| t.day.map(at)).collect();
        // Undated releases sit evenly between the dated ones around them.
        for i in 0..n {
            if xs[i].is_some() {
                continue;
            }
            let before = (0..i).rev().find(|j| xs[*j].is_some());
            let after = (i + 1..n).find(|j| xs[*j].is_some());
            #[allow(clippy::cast_precision_loss)]
            let x = match (before, after) {
                (Some(a), Some(b)) => {
                    let (xa, xb) = (xs[a].unwrap_or(pad), xs[b].unwrap_or(pad));
                    xa + (xb - xa) * (i - a) as f32 / (b - a) as f32
                }
                (Some(a), None) => (xs[a].unwrap_or(pad) + 3.0 * (i - a) as f32).min(pad + span),
                (None, Some(b)) => (xs[b].unwrap_or(pad) - 3.0 * (b - i) as f32).max(pad),
                (None, None) => pad,
            };
            xs[i] = Some(x);
        }
        let mut out: Vec<f32> = xs.into_iter().map(|x| x.unwrap_or(pad)).collect();
        for i in 1..n {
            out[i] = out[i].max(out[i - 1]);
        }
        out
    }

    /// The years to mark on the axis: `(year, x)`.
    #[must_use]
    pub fn years(&self, width: f32, pad: f32) -> Vec<(i32, f32)> {
        if !self.dated() {
            return Vec::new();
        }
        let span = (width - 2.0 * pad).max(1.0);
        let dated: Vec<f64> = self.ticks.iter().filter_map(|t| t.day).collect();
        let t0 = dated.iter().copied().fold(f64::INFINITY, f64::min);
        let t1 = dated
            .iter()
            .copied()
            .fold(self.today_day, f64::max)
            .max(t0 + 1.0);
        let year_of = |day: f64| civil_year(day);
        let (first, last) = (year_of(t0), year_of(t1));
        let step = if last - first > 9 { 2 } else { 1 };
        (first + 1..=last)
            .step_by(step)
            .filter_map(|y| {
                let d = semver::days(&format!("{y}-01-01"))?;
                #[allow(clippy::cast_possible_truncation)]
                Some((y, pad + ((d - t0) / (t1 - t0)) as f32 * span))
            })
            .collect()
    }
}

/// The civil year of a day count since 1970-01-01.
fn civil_year(day: f64) -> i32 {
    #[allow(clippy::cast_possible_truncation)]
    let z = day.floor() as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    #[allow(clippy::cast_possible_truncation)]
    let year = (if m <= 2 { y + 1 } else { y }) as i32;
    year
}

/// The fisheye: where a bar at `x` is drawn with the pointer at `p`.
/// Everything within `radius` of the pointer is pushed outward so bars near
/// it have room; what is farther does not move.
#[must_use]
pub fn fisheye(x: f32, p: f32, radius: f32, distortion: f32) -> f32 {
    let d = x - p;
    let r = d.abs();
    if r >= radius {
        return x;
    }
    let u = r / radius;
    p + d.signum() * radius * (((distortion + 1.0) * u) / (distortion * u + 1.0))
}

const DISTORTION: f32 = 3.2;
const RADIUS: f32 = 110.0;
const PAD: f32 = 8.0;
/// Top band (the riding label and the flags), bar band, axis band: px at 100 %.
const TOP: f32 = 24.0;
const BARS: f32 = 44.0;
const AXIS: f32 = 20.0;
const TEXT: TypeRole = TypeRole {
    size: 11.0,
    line: 15.0,
    ..ty::MONO_SMALL
};
const LABEL: TypeRole = TypeRole {
    size: 12.0,
    line: 16.0,
    ..ty::MONO_SMALL
};

struct State {
    pointer: Option<f32>,
    dragging: bool,
    motion: Motion,
}

/// The ticker (see [`ticker`]).
pub struct Ticker {
    id: ElementId,
    facts: Rc<TickerFacts>,
    measure: Measure,
    on_travel: Option<Rc<dyn Fn(usize, &mut Window, &mut App)>>,
    rest: Option<f32>,
    stand: Option<usize>,
}

/// A ticker for `facts`, as wide as `measure` gives it.
#[must_use]
pub fn ticker(id: impl Into<ElementId>, facts: Rc<TickerFacts>, measure: &Measure) -> Ticker {
    Ticker {
        id: id.into(),
        facts,
        measure: *measure,
        on_travel: None,
        rest: None,
        stand: None,
    }
}

/// Where the bar of release `tick` sits at rest, relative to the ticker's own
/// corner, as a box a host can put a keyboard door on.
#[must_use]
pub fn door(facts: &TickerFacts, measure: &Measure, tick: usize) -> Bounds<Pixels> {
    let s = measure.scale();
    let xs = facts.positions(f32::from(measure.width()), PAD * s);
    let x = xs.get(tick).copied().unwrap_or(0.0);
    // At least 24 px wide, centred on its bar: a target the pointer can hit
    // (the bar itself is 10 px).
    Bounds::new(
        gpui::point(px(x - 12.0 * s), px(TOP * s)),
        gpui::size(px(24.0 * s), px(BARS * s)),
    )
}

impl Ticker {
    /// The release the host's keyboard stands on: its bar is hot, its label
    /// rides it (one keyboard system: the host's targets, not the element's).
    #[must_use]
    pub const fn stand(mut self, tick: Option<usize>) -> Self {
        self.stand = tick;
        self
    }

    /// Called with the release index chosen: on press and on every release the
    /// drag crosses.
    #[must_use]
    pub fn on_travel(mut self, on_travel: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_travel = Some(Rc::new(on_travel));
        self
    }

    /// Shows the pointer at `x` px from the ticker's left (scenes).
    #[must_use]
    pub fn rest(mut self, x: Option<f32>) -> Self {
        self.rest = x;
        self
    }

    fn height(&self) -> f32 {
        (TOP + BARS + AXIS) * self.measure.scale()
    }
}

impl IntoElement for Ticker {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

/// What the ticker keeps between layout and paint.
pub struct TickerLayout {
    state: Entity<State>,
}

impl Element for Ticker {
    type RequestLayoutState = TickerLayout;
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
    ) -> (LayoutId, TickerLayout) {
        let state = window.use_keyed_state("ticker", cx, |_, _| State {
            pointer: None,
            dragging: false,
            motion: Motion::new(),
        });
        let mut style = Style::default();
        style.size.width = px(f32::from(self.measure.width())).into();
        style.size.height = px(self.height()).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [], cx), TickerLayout { state })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _layout: &mut TickerLayout,
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
        layout: &mut TickerLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let (ox, oy) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let width = f32::from(bounds.size.width);
        let facts = self.facts.clone();
        let base_y = oy + (TOP + BARS) * s;
        let pad = PAD * s;
        let (radius, distortion) = (RADIUS * s, DISTORTION);
        let xs = facts.positions(width, pad);

        let (pointer, motion) = {
            let state = layout.state.read(cx);
            (state.pointer, state.motion.clone())
        };
        let pointer = pointer.or(self.rest);
        let magnify = motion.animate(
            ElementId::NamedChild(Arc::new(self.id.clone()), "magnify".into()),
            if pointer.is_some() { 1.0 } else { 0.0 },
            spec::REVEAL,
            window,
            cx,
        );
        let anchor = pointer.unwrap_or(0.0);
        let shown = |x: f32| x + (fisheye(x, anchor, radius, distortion) - x) * magnify;
        let sx: Vec<f32> = xs.iter().map(|x| shown(*x)).collect();

        // Which bar is hot: the nearest within reach of the pointer, or the
        // one the host's keyboard stands on.
        let walk = self.stand;
        let hot = pointer
            .and_then(|p| {
                sx.iter()
                    .enumerate()
                    .map(|(i, x)| (i, (x - p).abs()))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .filter(|(_, d)| *d < 24.0 * s)
                    .map(|(i, _)| i)
            })
            .or(walk);

        // The axis.
        let mut axis = Fill::new();
        axis.poly(&Poly::rect(ox, base_y, width, 1.0));
        for (_, x) in facts.years(width, pad) {
            let x = ox + shown(x);
            axis.poly(&Poly::rect(x, base_y, 1.0, 4.0 * s));
        }
        axis.paint(window, Hsla::from(palette.line3));
        let text_role = self.measure.role(TEXT);
        let mut texts: Vec<(Shaped, f32, f32, SharedString, TypeRole)> = Vec::new();
        for (year, x) in facts.years(width, pad) {
            let words: SharedString = year.to_string().into();
            let t = shape(words.clone(), text_role, palette.ink3.into(), window);
            let at = ox + shown(x) - t.width() * 0.5;
            texts.push((t, at, base_y + 5.0 * s + text_role.size, words, text_role));
        }
        if !facts.dated() && facts.ticks.len() > 1 {
            for (i, x) in [(0, xs[0]), (facts.ticks.len() - 1, xs[xs.len() - 1])] {
                let words = facts.ticks[i].version.clone();
                let t = shape(words.clone(), text_role, palette.ink3.into(), window);
                let at = if i == 0 {
                    ox + x - 2.0
                } else {
                    ox + x - t.width() + 2.0
                };
                texts.push((t, at, base_y + 5.0 * s + text_role.size, words, text_role));
            }
        }

        // The bars, batched by colour.
        let mut batches: Vec<(Hsla, Fill)> = Vec::new();
        for (i, tick) in facts.ticks.iter().enumerate() {
            let near = pointer.map_or(0.0, |p| (1.0 - (xs[i] - p).abs() / radius).clamp(0.0, 1.0))
                * magnify;
            let w = (1.5 + 4.5 * near * near) * s;
            let h0 = match tick.kind {
                Kind::Breaking => 34.0,
                Kind::Minor => 21.0,
                Kind::Patch | Kind::Pre => 12.0,
            };
            let h = h0 * s * if hot == Some(i) { 1.18 } else { 1.0 };
            let colour = bar_ink(&facts, i, hot == Some(i), palette);
            let poly = Poly::rect(ox + sx[i] - w * 0.5, base_y - h, w, h);
            let at = batches
                .iter()
                .position(|(c, _)| *c == colour)
                .unwrap_or_else(|| {
                    batches.push((colour, Fill::new()));
                    batches.len() - 1
                });
            batches[at].1.poly(&poly);
        }
        for (colour, fill) in batches {
            fill.paint(window, colour);
        }

        // The caret under the release being read.
        if let Some(i) = facts.reading {
            let x = ox + sx[i];
            let mut caret = Fill::new();
            caret.triangle(
                pt(x - 5.0 * s, base_y + 1.0),
                pt(x, base_y - 5.0 * s),
                pt(x + 5.0 * s, base_y + 1.0),
            );
            caret.paint(window, Hsla::from(palette.peri_hi));
        }

        // Flags: the pin and the newest wear small words that stay put. A
        // flag ends on its stem (one near the left edge begins on it), and
        // when two would touch the later one keeps only its first word.
        let flag_y = base_y - (BARS - 1.0) * s - 2.0 * s;
        let mut spans: Vec<(f32, f32)> = Vec::new();
        for (index, full, short, ink) in [
            facts.pin.map(|i| {
                (
                    i,
                    format!("your pin {}", facts.ticks[i].version),
                    "pin".to_owned(),
                    palette.mint.base,
                )
            }),
            facts.latest.filter(|l| Some(*l) != facts.pin).map(|i| {
                (
                    i,
                    format!("newest {}", facts.ticks[i].version),
                    "newest".to_owned(),
                    palette.amber.base,
                )
            }),
        ]
        .into_iter()
        .flatten()
        {
            if hot.is_some_and(|h| (sx[h] - sx[index]).abs() < 70.0 * s) && hot != Some(index) {
                continue;
            }
            let x = ox + sx[index];
            let place = |words: String, window: &Window| {
                let words: SharedString = words.into();
                let t = shape(words.clone(), text_role, ink.into(), window);
                let at = if x + 3.0 * s - t.width() < ox {
                    x - 3.0 * s
                } else {
                    x + 3.0 * s - t.width()
                };
                (t, at, words)
            };
            let (mut t, mut at, mut words) = place(full, window);
            let touches = |at: f32, w: f32, spans: &[(f32, f32)]| {
                spans
                    .iter()
                    .any(|(a, b)| at < *b + 6.0 * s && at + w > *a - 6.0 * s)
            };
            if touches(at, t.width(), &spans) {
                (t, at, words) = place(short, window);
            }
            if touches(at, t.width(), &spans) {
                continue;
            }
            spans.push((at, at + t.width()));
            texts.push((t, at, flag_y, words, text_role));
            let mut stem = Fill::new();
            stem.poly(&Poly::rect(x, flag_y + 2.0 * s, 1.0, 4.0 * s));
            stem.paint(window, Hsla::from(ink));
        }

        // The riding label: the hot release reads itself, on a plate at the top.
        let mut plate: Option<(Poly, Vec<(Shaped, SharedString)>, f32, f32)> = None;
        if let Some(i) = hot {
            let tick = &facts.ticks[i];
            let mut parts: Vec<(SharedString, Hsla)> =
                vec![(tick.version.clone(), palette.ink0.into())];
            if let Some(date) = &tick.date {
                parts.push((date.clone(), palette.ink2.into()));
            }
            parts.push(if tick.standing == Standing::Yanked {
                ("yanked".into(), palette.coral.base.into())
            } else {
                (
                    match tick.kind {
                        Kind::Breaking => "breaking",
                        Kind::Minor => "features",
                        Kind::Patch => "fixes",
                        Kind::Pre => "pre-release",
                    }
                    .into(),
                    palette.ink1.into(),
                )
            });
            if facts.pin == Some(i) {
                parts.push(("your pin".into(), palette.mint.base.into()));
            } else if facts.latest == Some(i) {
                parts.push(("newest".into(), palette.amber.base.into()));
            } else if let Some(ago) = tick
                .date
                .as_deref()
                .and_then(|d| semver::ago(d, &facts.today))
            {
                parts.push((format!("{ago} ago").into(), palette.ink2.into()));
            }
            parts.push(if tick.names == Names::Read {
                ("read".into(), palette.peri_hi.into())
            } else {
                ("not read yet".into(), palette.ink2.into())
            });
            let label_role = self.measure.role(LABEL);
            let shaped: Vec<(Shaped, SharedString)> = parts
                .into_iter()
                .map(|(w, ink)| (shape(w.clone(), label_role, ink, window), w))
                .collect();
            let sep = 12.0 * s;
            let total: f32 = shaped.iter().map(|(t, _)| t.width()).sum::<f32>()
                + sep * (shaped.len() as f32 - 1.0);
            let (pw, ph) = (total + 18.0 * s, label_role.line + 6.0 * s);
            let x = (ox + sx[i] - pw * 0.5).clamp(ox, (ox + width - pw).max(ox));
            let y = oy + 1.0;
            plate = Some((
                Poly::chamfer(x, y, pw, ph, 3.0 * s),
                shaped,
                x + 9.0 * s,
                y + 3.0 * s + label_role.size,
            ));
        }
        if let Some((poly, _, _, _)) = &plate {
            let mut back = Fill::new();
            back.poly(poly);
            back.paint(window, Hsla::from(palette.plate3));
            let mut edge = Fill::new();
            for ring in poly.offset(-0.5).stroke_ring(1.0) {
                edge.poly(&ring);
            }
            edge.paint(window, Hsla::from(palette.line3));
        }
        let mut published: Vec<(Bounds<Pixels>, String, TypeRole, f32, &'static str)> = Vec::new();
        if let Some((_, shaped, mut x, baseline)) = plate {
            let mut joined = Vec::new();
            let start = x;
            for (n, (t, words)) in shaped.iter().enumerate() {
                if n > 0 {
                    x += 6.0 * s;
                    let mut dot = Fill::new();
                    dot.poly(&Poly::rect(x, baseline - 5.0 * s, 1.5 * s, 1.5 * s));
                    dot.paint(window, Hsla::from(palette.ink3));
                    x += 6.0 * s;
                }
                t.paint(x, baseline, window, cx);
                x += t.width();
                joined.push(words.to_string());
            }
            let label_role = self.measure.role(LABEL);
            published.push((
                Bounds::new(
                    gpui::point(px(start), px(baseline - label_role.size)),
                    gpui::size(px(x - start), px(label_role.line)),
                ),
                joined.join(" · "),
                label_role,
                x - start,
                "label",
            ));
        }
        for (t, x, baseline, words, role) in &texts {
            t.paint(*x, *baseline, window, cx);
            published.push((
                Bounds::new(
                    gpui::point(px(*x), px(baseline - role.size)),
                    gpui::size(px(t.width()), px(role.line)),
                ),
                words.to_string(),
                *role,
                t.width(),
                "text",
            ));
        }
        if probe::enabled(cx) {
            for (n, (at, content, role, natural, kind)) in published.into_iter().enumerate() {
                let key = ElementId::NamedChild(
                    Arc::new(self.id.clone()),
                    SharedString::from(format!("{kind}-{n}")),
                );
                probe::record_text(
                    cx,
                    &key,
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
                        natural_width: natural,
                        overflow: TextOverflow::Clip,
                        content,
                        min_width: natural,
                        line_height: role.line,
                        size: role.size,
                        weight: role.weight,
                        region: probe::current_region(),
                    },
                );
                let _ = kind;
            }
            // Where each bar stands, for tests that click one.
            for (i, x) in sx.iter().enumerate() {
                let key = ElementId::NamedChild(
                    Arc::new(self.id.clone()),
                    SharedString::from(format!("bar-{}", facts.ticks[i].version)),
                );
                probe::record_bounds(
                    cx,
                    &key,
                    Bounds::new(
                        gpui::point(px(ox + x - 1.0), px(oy)),
                        gpui::size(px(2.0), px(self.height())),
                    ),
                );
            }
        }

        // The pointer.
        let state = layout.state.clone();
        {
            let (state, hitbox) = (state.clone(), hitbox.clone());
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let inside = hitbox.is_hovered(window);
                let next = inside.then(|| f32::from(event.position.x) - ox);
                let changed = {
                    let current = state.read(cx);
                    current.pointer != next
                        && (next.is_some() || current.dragging || current.pointer.is_some())
                };
                if changed {
                    state.update(cx, |state, cx| {
                        state.pointer = next;
                        cx.notify();
                    });
                }
            });
        }
        {
            let state = state.clone();
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _window, cx| {
                if phase == DispatchPhase::Capture && state.read(cx).pointer.is_some() {
                    state.update(cx, |state, cx| {
                        state.pointer = None;
                        cx.notify();
                    });
                }
            });
        }
        if let Some(travel) = self.on_travel.clone() {
            let facts = facts.clone();
            let travelling = Rc::new(std::cell::Cell::new(None::<usize>));
            {
                let (state, hitbox, travel, travelling, facts) = (
                    state.clone(),
                    hitbox.clone(),
                    travel.clone(),
                    travelling.clone(),
                    facts.clone(),
                );
                let xs = xs.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || event.button != MouseButton::Left
                        || !hitbox.is_hovered(window)
                    {
                        return;
                    }
                    let p = f32::from(event.position.x) - ox;
                    let magnify = if state.read(cx).pointer.is_some() {
                        1.0
                    } else {
                        0.0
                    };
                    let hit = xs
                        .iter()
                        .enumerate()
                        .map(|(i, x)| {
                            (
                                i,
                                (x + (fisheye(*x, p, radius, distortion) - x) * magnify - p).abs(),
                            )
                        })
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .filter(|(_, d)| *d < 24.0 * s)
                        .map(|(i, _)| i);
                    state.update(cx, |state, cx| {
                        state.dragging = true;
                        state.pointer = Some(p);
                        cx.notify();
                    });
                    if let Some(i) = hit {
                        travelling.set(Some(i));
                        let _ = &facts;
                        travel(i, window, cx);
                    }
                });
            }
            {
                let (state, hitbox, travel, travelling) =
                    (state.clone(), hitbox.clone(), travel, travelling.clone());
                let xs = xs.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Bubble
                        || !state.read(cx).dragging
                        || !event.dragging()
                    {
                        return;
                    }
                    let p = f32::from(event.position.x) - ox;
                    let _ = &hitbox;
                    let hit = xs
                        .iter()
                        .enumerate()
                        .map(|(i, x)| (i, (fisheye(*x, p, radius, distortion) - p).abs()))
                        .min_by(|a, b| a.1.total_cmp(&b.1))
                        .filter(|(_, d)| *d < 24.0 * s)
                        .map(|(i, _)| i);
                    if let Some(i) = hit
                        && travelling.get() != Some(i)
                    {
                        travelling.set(Some(i));
                        travel(i, window, cx);
                    }
                });
            }
            {
                let state = state.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
                    if phase == DispatchPhase::Capture
                        && event.button == MouseButton::Left
                        && state.read(cx).dragging
                    {
                        state.update(cx, |state, cx| {
                            state.dragging = false;
                            cx.notify();
                        });
                        travelling.set(None);
                    }
                });
            }
        }
    }
}

/// A bar's colour: what it is beats what it was.
fn bar_ink(facts: &TickerFacts, i: usize, hot: bool, palette: &Palette) -> Hsla {
    let tick = &facts.ticks[i];
    if hot {
        return palette.ink0.into();
    }
    if facts.reading == Some(i) {
        return palette.peri_hi.into();
    }
    if facts.pin == Some(i) {
        return palette.mint.base.into();
    }
    if tick.standing == Standing::Yanked {
        return palette.coral.base.into();
    }
    if facts.latest == Some(i) {
        return palette.amber.base.into();
    }
    let base: Hsla = match tick.kind {
        Kind::Breaking => palette.ink1.into(),
        Kind::Minor => palette.ink2.into(),
        Kind::Patch | Kind::Pre => palette.ink3.into(),
    };
    if tick.names == Names::Read {
        base
    } else {
        crate::paint::mix(base, palette.g1.into(), 0.45)
    }
}

#[cfg(test)]
mod tests {
    use super::{Release, TickerFacts, civil_year, fisheye};
    use crate::folio::state::{Names, Standing};

    fn facts(releases: &[(&str, Option<&str>)], pin: &str) -> TickerFacts {
        let releases: Vec<Release> = releases
            .iter()
            .map(|(v, d)| Release {
                version: (*v).to_owned(),
                date: d.map(str::to_owned),
                standing: Standing::Available,
                names: Names::Read,
            })
            .collect();
        TickerFacts::new(&releases, Some(pin), "2026-09-28")
    }

    #[test]
    fn releases_sort_oldest_first_and_the_pin_and_newest_are_found() {
        let f = facts(
            &[
                ("1.0.0", None),
                ("0.8.23", None),
                ("1.1.6", None),
                ("0.5.11", None),
            ],
            "0.8.23",
        );
        let order: Vec<&str> = f.ticks.iter().map(|t| t.version.as_ref()).collect();
        assert_eq!(order, ["0.5.11", "0.8.23", "1.0.0", "1.1.6"]);
        assert_eq!(f.pin, Some(1));
        assert_eq!(f.latest, Some(3));
    }

    /// A release's door is a target a pointer can hit: at least 24 px each
    /// way, centred on its 10 px bar (J1 linted every door 10 x 44 px).
    #[test]
    fn a_release_door_is_at_least_24_px_and_centred_on_its_bar() {
        let f = facts(
            &[("0.5.11", None), ("0.8.23", None), ("1.0.0", None)],
            "0.8.23",
        );
        let measure = crate::Measure::new(gpui::px(600.0), &crate::theme::Facet::default());
        let xs = f.positions(f32::from(measure.width()), super::PAD * measure.scale());
        for (tick, x) in xs.iter().enumerate() {
            let door = super::door(&f, &measure, tick);
            assert!(
                f32::from(door.size.width) >= 24.0 && f32::from(door.size.height) >= 24.0,
                "{tick}: {door:?}"
            );
            assert!(
                (f32::from(door.center().x) - x).abs() < 0.01,
                "{tick}: centred on its bar at {x}: {door:?}"
            );
        }
    }

    #[test]
    fn undated_releases_are_spaced_evenly() {
        let f = facts(
            &[
                ("0.1.0", None),
                ("0.2.0", None),
                ("0.3.0", None),
                ("0.4.0", None),
                ("0.5.0", None),
            ],
            "0.3.0",
        );
        assert!(!f.dated());
        let xs = f.positions(408.0, 4.0);
        let gaps: Vec<f32> = xs.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps.iter().all(|g| (g - gaps[0]).abs() < 0.001), "{gaps:?}");
        assert!((xs[0] - 4.0).abs() < 0.001 && (xs[4] - 404.0).abs() < 0.001);
        assert!(f.years(408.0, 4.0).is_empty(), "no dates, no years");
    }

    #[test]
    fn dated_releases_sit_on_a_time_axis_and_undated_ones_between_their_neighbours() {
        let f = facts(
            &[
                ("0.1.0", Some("2020-01-01")),
                ("0.2.0", None),
                ("0.3.0", Some("2024-01-01")),
                ("0.4.0", Some("2026-01-01")),
            ],
            "0.3.0",
        );
        assert!(f.dated());
        let xs = f.positions(1008.0, 8.0);
        assert!(xs[0] < xs[1] && xs[1] < xs[2] && xs[2] < xs[3]);
        // 2020 -> 2024 is a much longer stretch than 2024 -> 2026.
        assert!(xs[2] - xs[0] > (xs[3] - xs[2]) * 1.5, "{xs:?}");
        // The undated one is midway between its dated neighbours.
        assert!(((xs[1] - xs[0]) - (xs[2] - xs[1])).abs() < 0.01);
        let years: Vec<i32> = f.years(1008.0, 8.0).iter().map(|(y, _)| *y).collect();
        assert_eq!(years, [2021, 2022, 2023, 2024, 2025, 2026]);
    }

    #[test]
    fn the_fisheye_pushes_neighbours_apart_and_leaves_the_far_ones() {
        let p = 300.0;
        // Far from the pointer nothing moves.
        assert!((fisheye(500.0, p, 110.0, 3.2) - 500.0).abs() < 0.001);
        // Two bars 2 px apart near the pointer end up much farther apart.
        let (a, b) = (fisheye(306.0, p, 110.0, 3.2), fisheye(308.0, p, 110.0, 3.2));
        assert!(b - a > 2.5 * 2.0, "{a} {b}");
        // Order and side are kept; the edge of the lens is continuous.
        assert!(fisheye(250.0, p, 110.0, 3.2) < p && fisheye(350.0, p, 110.0, 3.2) > p);
        assert!((fisheye(409.9, p, 110.0, 3.2) - 409.9).abs() < 0.2);
    }

    #[test]
    fn years_come_from_the_day_count() {
        assert_eq!(civil_year(0.0), 1970);
        assert_eq!(
            civil_year(crate::marks::semver::days("2026-09-28").unwrap_or(0.0)),
            2026
        );
        assert_eq!(
            civil_year(crate::marks::semver::days("2015-12-31").unwrap_or(0.0)),
            2015
        );
    }
}

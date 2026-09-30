//! Its history: the releases as bars on one instrument (the board's
//! `histHtml`, in `ticker.js`'s grammar): tall is a major, short a patch,
//! coral a yank. A release the index has read wears a cap that says what the
//! symbol was there against your pin (mint: your pin, ink: the same, amber:
//! different, hollow: not here yet, coral: gone). Resting on a bar lifts it
//! and puts that release's words in the caption, inside the instrument.

use super::lazy::lazy;
use super::said;
use crate::anatomy::history::{History, Was, Weight};
use crate::data::release::RegistryFact;
use crate::measure::{Measure, Set};
use crate::paint::geom::{Poly, fill_poly};
use crate::tokens::{Palette, scale};
use gpui::{
    AnyElement, App, Bounds, ColorExt, Element, ElementId, Global, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, MouseMoveEvent, ParentElement, Pixels, Refineable,
    SharedString, Style, StyleRefinement, Styled, Window, div, px,
};
use std::rc::Rc;

/// Which release the pointer is on, per instrument.
#[derive(Default)]
struct Scrub {
    key: Option<SharedString>,
    at: Option<usize>,
}

impl Global for Scrub {}

fn scrubbed(key: &str, cx: &App) -> Option<usize> {
    cx.try_global::<Scrub>()
        .filter(|scrub| scrub.key.as_deref() == Some(key))
        .and_then(|scrub| scrub.at)
}

fn exact_date(release: &crate::anatomy::history::Release) -> Option<&str> {
    match &release.at {
        RegistryFact::Known(date) => Some(date.as_ref()),
        RegistryFact::Missing | RegistryFact::Ambiguous => None,
    }
}

/// Where each release's bar sits along `width`. Exact dates set the time
/// scale when every date is known; otherwise releases keep their registry
/// order at equal spacing, without inventing dates for missing or conflicting
/// facts.
fn positions(history: &History, width: f32) -> Vec<f32> {
    let days = |at: &str| -> Option<f64> {
        let mut parts = at
            .split(['-', 'T'])
            .take(3)
            .map(|part| part.parse::<f64>().ok());
        let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
        Some(y * 372.0 + m * 31.0 + d)
    };
    let dated: Option<Vec<f64>> = history
        .releases
        .iter()
        .map(|release| exact_date(release).and_then(days))
        .collect();
    if let Some(dated) = dated {
        let (lo, hi) = dated
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), t| (lo.min(*t), hi.max(*t)));
        dated
            .iter()
            .map(|t| 6.0 + ((t - lo) / (hi - lo).max(1.0)) as f32 * (width - 12.0))
            .collect()
    } else {
        let denominator = history.releases.len().saturating_sub(1).max(1) as f32;
        (0..history.releases.len())
            .map(|index| 6.0 + index as f32 / denominator * (width - 12.0))
            .collect()
    }
}

/// The instrument and its pointer-scrub behavior.
struct HistoryStrip {
    history: Rc<History>,
    xs: Vec<f32>,
    key: SharedString,
    at: Option<usize>,
    scale: f32,
    inks: [Hsla; 6],
    style: StyleRefinement,
}

impl Styled for HistoryStrip {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for HistoryStrip {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for HistoryStrip {
    type RequestLayoutState = Style;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Style) {
        let mut style = Style::default();
        style.refine(&self.style);
        let layout = window.request_layout(style.clone(), [], cx);
        (layout, style)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Style,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        style: &mut Style,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let (history, xs, at, scale, inks, key) = (
            Rc::clone(&self.history),
            self.xs.clone(),
            self.at,
            self.scale,
            self.inks,
            self.key.clone(),
        );
        style.paint(bounds, window, cx, |window, cx| {
            paint(&history, &xs, at, bounds, scale, inks, window);
            // The pointer scrubs: the nearest bar takes the caption.
            let (key, xs, span) = (key, xs, bounds);
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Bubble {
                    return;
                }
                let inside = span.contains(&event.position);
                let near = inside
                    .then(|| {
                        let x = f32::from(event.position.x - span.origin.x);
                        xs.iter()
                            .enumerate()
                            .map(|(index, position)| (index, (position - x).abs()))
                            .min_by(|a, b| a.1.total_cmp(&b.1))
                            .map(|(index, _)| index)
                    })
                    .flatten();
                let now = scrubbed(&key, cx);
                if near != now
                    || (near.is_none()
                        && cx.try_global::<Scrub>().is_some_and(|scrub| {
                            scrub.key.as_deref() == Some(&*key) && scrub.at.is_some()
                        }))
                {
                    let scrub = cx.default_global::<Scrub>();
                    match near {
                        Some(_) => {
                            *scrub = Scrub {
                                key: Some(key.clone()),
                                at: near,
                            }
                        }
                        None if scrub.key.as_deref() == Some(&*key) => *scrub = Scrub::default(),
                        None => {}
                    }
                    window.refresh();
                }
            });
        });
    }
}

/// The instrument, `width` wide, when there is a history to draw.
#[must_use]
pub fn history(
    history: &History,
    width: Pixels,
    m: &Measure,
    palette: &Palette,
) -> Option<AnyElement> {
    if !history.drawn() {
        return None;
    }
    let (history, m, palette) = (Rc::new(history.clone()), *m, *palette);
    let key = SharedString::from("page-history");
    Some(
        lazy(move |_, cx| {
            let s = m.scale();
            let at = scrubbed(&key, cx);
            let caption = at.map_or_else(|| history.caption(), |index| history.label(index));
            let w = f32::from(width);
            let h = 40.0 * s;
            let xs = positions(&history, w);
            let ink = palette.ink2.hsla();
            let inks = [
                ink,
                palette.mint.base.hsla(),
                palette.coral.base.hsla(),
                palette.amber.base.hsla(),
                palette.ink3.hsla(),
                palette.line2.hsla(),
            ];
            let strip = HistoryStrip {
                history: Rc::clone(&history),
                xs: xs.clone(),
                key: key.clone(),
                at,
                scale: s,
                inks,
                style: StyleRefinement::default(),
            }
            .w(px(w))
            .h(px(h))
            .cursor_pointer()
            .into_any_element();

            let mut years = div().relative().w(px(w)).h(px(14.0 * s));
            // A year ruler only makes sense when every bar is anchored by an exact date.
            let all_dates_known = history
                .releases
                .iter()
                .all(|release| exact_date(release).is_some_and(|date| days(date).is_some()));
            let dated: Vec<f32> = xs.clone();
            if all_dates_known {
                if let (Some(first), Some(last)) =
                    (history.releases.first(), history.releases.last())
                {
                    let year = |release: &crate::anatomy::history::Release| {
                        exact_date(release)
                            .and_then(|date| date.get(..4))
                            .and_then(|y| y.parse::<i32>().ok())
                    };
                    if let (Some(a), Some(b)) = (year(first), year(last)) {
                        let per_year = (w - 12.0) / (b - a + 1).max(1) as f32;
                        let step = ((46.0 * s / per_year).ceil() as i32).max(2);
                        let step = step + step % 2;
                        let mut y = a + step / 2;
                        while y <= b {
                            let frac = (f64::from(y - a) + 0.5) / f64::from((b - a + 1).max(1));
                            let x = (6.0 + frac as f32 * (w - 12.0))
                                .min(dated.last().copied().unwrap_or(w));
                            years =
                                years.child(div().absolute().left(px(x - 10.0 * s)).child(said(
                                    format!("page-history-year-{y}"),
                                    y.to_string(),
                                    scale::LABEL_MONO,
                                    palette.ink3,
                                    &m,
                                )));
                            y += step;
                        }
                    }
                }
            }
            let caption_ink = if at.is_some() {
                palette.ink1
            } else {
                palette.ink2
            };
            let caption = SharedString::from(caption);
            div()
                .flex()
                .flex_col()
                .gap(px(3.0 * s))
                .w(px(w))
                .child(said(
                    "page-history-kicker",
                    "Its history",
                    scale::LABEL,
                    palette.ink3,
                    &m,
                ))
                .child(
                    div().w(px(w)).min_h(px(16.0 * s)).child(crate::probe::text(
                        gpui::ElementId::Name("page-history-caption".into()),
                        caption.clone(),
                        m.role(scale::LABEL),
                        1.0,
                        crate::probe::TextOverflow::Wrap,
                        div()
                            .set(scale::LABEL, &m)
                            .text_color(caption_ink.hsla())
                            .child(caption),
                    )),
                )
                .child(strip)
                .child(years)
                .into_any_element()
        })
        .into_any_element(),
    )
}

fn days(at: &str) -> Option<f64> {
    let mut parts = at
        .split(['-', 'T'])
        .take(3)
        .map(|part| part.parse::<f64>().ok());
    let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
    Some(y * 372.0 + m * 31.0 + d)
}

fn paint(
    history: &History,
    xs: &[f32],
    at: Option<usize>,
    bounds: Bounds<Pixels>,
    s: f32,
    inks: [Hsla; 6],
    window: &mut Window,
) {
    let [_ink, mint, coral, amber, quiet, line] = inks;
    let (x0, y1) = (
        f32::from(bounds.origin.x),
        f32::from(bounds.origin.y) + f32::from(bounds.size.height),
    );
    let base = Poly::rect(x0, y1 - 1.0, f32::from(bounds.size.width), 1.0);
    fill_poly(window, &base, line);
    for (i, (release, x)) in history.releases.iter().zip(xs).enumerate() {
        let lifted = at == Some(i);
        let height = match release.weight {
            Weight::Major => 24.0,
            Weight::Minor => 14.0,
            Weight::Patch => 8.0,
        } * s
            + if lifted { 4.0 * s } else { 0.0 };
        let colour = if release.yanked == RegistryFact::Known(true) {
            coral
        } else if lifted {
            mint
        } else {
            quiet
        };
        fill_poly(
            window,
            &Poly::rect(x0 + x - 0.7 * s, y1 - 1.0 - height, 1.4 * s, height),
            colour,
        );
        // The cap: what the symbol was there.
        let cap = 9.0 * s;
        let (cx, cy) = (x0 + x - cap / 2.0, y1 - 1.0 - height - cap - 2.0 * s);
        let (fill, ring): (Option<Hsla>, Hsla) = match &release.was {
            Was::Unread => continue,
            Was::Pinned => (Some(mint), mint),
            Was::Same => (Some(palette_ink(quiet)), quiet),
            Was::Differs(_) => (Some(amber), amber),
            Was::NotYet => (None, quiet),
            Was::Gone => (Some(coral), coral),
        };
        let square = Poly::rect(cx, cy, cap, cap);
        if let Some(fill) = fill {
            fill_poly(
                window,
                &square,
                fill.opacity(if lifted { 1.0 } else { 0.75 }),
            );
        } else {
            for edge in square.stroke_ring(1.0) {
                fill_poly(window, &edge, ring);
            }
        }
    }
}

/// The cap of a release that is the same: the quiet ink, a step up.
fn palette_ink(quiet: Hsla) -> Hsla {
    quiet.opacity(0.9)
}

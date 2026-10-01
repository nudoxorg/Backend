//! Its history: the releases as bars on one instrument (the board's
//! `histHtml`, in `ticker.js`'s grammar): tall is a major, short a patch,
//! coral a yank. A release the index has read wears a cap that says what the
//! symbol was there against your pin (mint: your pin, ink: the same, amber:
//! different, hollow: not here yet, coral: gone). Resting on a bar lifts it
//! and puts that release's words in the caption, inside the instrument.

use super::lazy::lazy;
use super::said;
use crate::anatomy::history::{History, Was, Weight};
use crate::measure::{Measure, Set};
use crate::paint::geom::{Poly, fill_poly};
use crate::tokens::{Palette, scale};
use gpui::{AnyElement, App, Bounds, ColorExt, Global, Hsla, IntoElement, MouseMoveEvent, ParentElement, Pixels, SharedString, Styled, Window, canvas, div, px};
use std::rc::Rc;

/// Which release the pointer is on, per instrument.
#[derive(Default)]
struct Scrub {
    key: Option<SharedString>,
    at: Option<usize>,
}

impl Global for Scrub {}

fn scrubbed(key: &str, cx: &App) -> Option<usize> {
    cx.try_global::<Scrub>().filter(|scrub| scrub.key.as_deref() == Some(key)).and_then(|scrub| scrub.at)
}

/// Where each release's bar sits along `width`, by its date (equally spaced
/// when the registry gave no dates).
fn positions(history: &History, width: f32) -> Vec<Option<f32>> {
    let days = |at: &str| -> Option<f64> {
        let mut parts = at.split(['-', 'T']).take(3).map(|part| part.parse::<f64>().ok());
        let (y, m, d) = (parts.next()??, parts.next()??, parts.next()??);
        Some(y * 372.0 + m * 31.0 + d)
    };
    let dated: Vec<Option<f64>> = history.releases.iter().map(|release| days(&release.at)).collect();
    let (lo, hi) = dated.iter().flatten().fold((f64::MAX, f64::MIN), |(lo, hi), t| (lo.min(*t), hi.max(*t)));
    dated.iter().map(|t| t.map(|t| 6.0 + ((t - lo) / (hi - lo).max(1.0)) as f32 * (width - 12.0))).collect()
}

/// The instrument, `width` wide, when there is a history to draw.
#[must_use]
pub fn history(history: &History, width: Pixels, m: &Measure, palette: &Palette) -> Option<AnyElement> {
    if !history.drawn() {
        return None;
    }
    let (history, m, palette) = (Rc::new(history.clone()), *m, *palette);
    let key = SharedString::from("page-history");
    Some(lazy(move |_, cx| {
        let s = m.scale();
        let at = scrubbed(&key, cx);
        let caption = at.map_or_else(|| history.caption(), |index| history.label(index));
        let w = f32::from(width);
        let h = 40.0 * s;
        let xs = positions(&history, w);
        let (bars, key_paint) = (Rc::clone(&history), key.clone());
        let ink = palette.ink2.hsla();
        let (mint, coral, amber, quiet, line) = (palette.mint.base.hsla(), palette.coral.base.hsla(), palette.amber.base.hsla(), palette.ink3.hsla(), palette.line2.hsla());
        let xs_paint = xs.clone();
        let strip = canvas(
            |_, _, _| {},
            move |bounds, (), window, cx| {
                paint(&bars, &xs_paint, at, bounds, s, [ink, mint, coral, amber, quiet, line], window);
                // The pointer scrubs: the nearest bar takes the caption.
                let (key, xs, span) = (key_paint.clone(), xs_paint.clone(), bounds);
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase != gpui::DispatchPhase::Bubble {
                        return;
                    }
                    let inside = span.contains(&event.position);
                    let near = inside.then(|| {
                        let x = f32::from(event.position.x - span.origin.x);
                        xs.iter().enumerate().filter_map(|(i, p)| p.map(|p| (i, (p - x).abs()))).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
                    }).flatten();
                    let now = scrubbed(&key, cx);
                    if near != now || (near.is_none() && cx.try_global::<Scrub>().is_some_and(|scrub| scrub.key.as_deref() == Some(&*key) && scrub.at.is_some())) {
                        let scrub = cx.default_global::<Scrub>();
                        match near {
                            Some(_) => *scrub = Scrub { key: Some(key.clone()), at: near },
                            None if scrub.key.as_deref() == Some(&*key) => *scrub = Scrub::default(),
                            None => {}
                        }
                        window.refresh();
                    }
                });
                let _ = cx;
            },
        )
        .w(px(w))
        .h(px(h))
        .into_any_element();
        let mut years = div().relative().w(px(w)).h(px(14.0 * s));
        let dated: Vec<f32> = xs.iter().flatten().copied().collect();
        if let (Some(first), Some(last)) = (history.releases.iter().find(|r| !r.at.is_empty()), history.releases.iter().rev().find(|r| !r.at.is_empty())) {
            let year = |at: &str| at.get(..4).and_then(|y| y.parse::<i32>().ok());
            if let (Some(a), Some(b)) = (year(&first.at), year(&last.at)) {
                let per_year = (w - 12.0) / (b - a + 1).max(1) as f32;
                let step = ((46.0 * s / per_year).ceil() as i32).max(2);
                let step = step + step % 2;
                let mut y = a + step / 2;
                while y <= b {
                    let frac = (f64::from(y - a) + 0.5) / f64::from((b - a + 1).max(1));
                    let x = (6.0 + frac as f32 * (w - 12.0)).min(dated.last().copied().unwrap_or(w));
                    years = years.child(div().absolute().left(px(x - 10.0 * s)).child(said(format!("page-history-year-{y}"), y.to_string(), scale::LABEL_MONO, palette.ink3, &m)));
                    y += step;
                }
            }
        }
        let caption_ink = if at.is_some() { palette.ink1 } else { palette.ink2 };
        let caption = SharedString::from(caption);
        div().flex().flex_col().gap(px(3.0 * s)).w(px(w))
            .child(said("page-history-kicker", "Its history", scale::LABEL, palette.ink3, &m))
            .child(div().w(px(w)).min_h(px(16.0 * s)).child(crate::probe::text(
                gpui::ElementId::Name("page-history-caption".into()),
                caption.clone(),
                m.role(scale::LABEL),
                1.0,
                crate::probe::TextOverflow::Wrap,
                div().set(scale::LABEL, &m).text_color(caption_ink.hsla()).child(caption),
            )))
            .child(strip)
            .child(years)
            .into_any_element()
    }).into_any_element())
}

fn paint(history: &History, xs: &[Option<f32>], at: Option<usize>, bounds: Bounds<Pixels>, s: f32, inks: [Hsla; 6], window: &mut Window) {
    let [_ink, mint, coral, amber, quiet, line] = inks;
    let (x0, y1) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y) + f32::from(bounds.size.height));
    let base = Poly::rect(x0, y1 - 1.0, f32::from(bounds.size.width), 1.0);
    fill_poly(window, &base, line);
    for (i, (release, x)) in history.releases.iter().zip(xs).enumerate() {
        let Some(x) = x else { continue };
        let lifted = at == Some(i);
        let height = match release.weight {
            Weight::Major => 24.0,
            Weight::Minor => 14.0,
            Weight::Patch => 8.0,
        } * s
            + if lifted { 4.0 * s } else { 0.0 };
        let colour = if release.yanked {
            coral
        } else if lifted {
            mint
        } else {
            quiet
        };
        fill_poly(window, &Poly::rect(x0 + x - 0.7 * s, y1 - 1.0 - height, 1.4 * s, height), colour);
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
            fill_poly(window, &square, fill.opacity(if lifted { 1.0 } else { 0.75 }));
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

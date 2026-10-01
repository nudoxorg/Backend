//! Motion scenes, made to be filmed: every one drives the real input path
//! (scripted pointer moves on the virtual clock) or real data changes
//! (scripted steps), so a filmstrip is the product moving, not a mock-up.
//!
//! - `film-comb`: the pointer sweeps a comb tick by tick, then leaves.
//! - `film-rose`: rests on the "to" strands, then on a member, then leaves.
//! - `film-mosaic`: sweeps a row of stones.
//! - `film-gem`: stages advance under a gem and a seam (fill flows facet by
//!   facet; working flashes; a stall turns amber; done).
//! - `film-strands`: a small rose whose last strand is a value in flight.
//! - `film-rung`: a comb's room shrinks step by step (card → row → tag → mark).

use super::super::{
    Dir, Door, Member, Stage, StageState, Tick, TickTone, comb, gem_progress, mosaic, rose, seam,
};
use super::stage::step;
use crate::icons::Kind;
use crate::measure::Rung;
use crate::theme::ActiveFacet;
use gpui::{AnyElement, App, IntoElement, ParentElement, Styled, Window, div, px};

/// A pointer script: `n` moves `every` ms apart from `(x0, y)` stepping `dx`,
/// then one move off to `(x0, y + 200)`.
const fn sweep<const N: usize>(start: u64, every: u64, x0: f32, dx: f32, y: f32) -> [(u64, f32, f32); N] {
    let mut out = [(0_u64, 0.0_f32, 0.0_f32); N];
    let mut i = 0;
    while i < N {
        #[allow(clippy::cast_precision_loss)]
        let k = i as f32;
        out[i] = (start + every * i as u64, x0 + dx * k, y);
        i += 1;
    }
    // The last move leaves.
    out[N - 1] = (start + every * (N as u64 - 1) + 400, x0, y + 200.0);
    out
}

/// Comb: 64 ticks over 900 px at x = 50; tick i is at 50 + 1 + i * 898 / 63.
pub(crate) static COMB_SWEEP: [(u64, f32, f32); 17] = sweep(0, 50, 50.0 + 1.0 + 20.0 * (898.0 / 63.0), 898.0 / 63.0, 90.0);

pub(crate) fn comb_scene(_w: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    comb_with(true, cx)
}

/// The same sweep with no door: nothing floats, so the motion report sees
/// the comb's own tracks alone.
pub(crate) fn bare_comb_scene(_w: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    comb_with(false, cx)
}

fn comb_with(door: bool, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let m = facet.measure(px(900.0 * facet.text_scale));
    let mut c = comb("film-comb", super::releases(), &m).thickness(40.0).rung(Rung::Row);
    if door {
        c = c.door(Door::tip(|_, _, _, _| div().into_any_element()));
    }
    div().size_full().p(px(50.0)).pt(px(60.0)).child(c).into_any_element()
}

/// Rose (the symbol page at 1176): the "to" strands, then `group_head`, then off.
pub(crate) static ROSE_POKES: [(u64, f32, f32); 3] = [(0, 720.0, 566.0), (700, 905.0, 521.0), (1400, 1100.0, 950.0)];

/// Mosaic: 96 stones, 18 columns of 14 px at x = 50, y = 60; sweep row 2.
pub(crate) static MOSAIC_SWEEP: [(u64, f32, f32); 13] = sweep(0, 40, 50.0 + 5.0, 14.0, 60.0 + 2.0 * 14.0 + 5.0);

pub(crate) fn mosaic_scene(_w: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    div()
        .size_full()
        .p(px(50.0))
        .pt(px(60.0))
        .child(
            mosaic("film-mosaic", super::stones(96, 3), &facet.measure(px(250.0 * facet.text_scale)))
                .door(Door::tip(|_, _, _, _| div().into_any_element())),
        )
        .into_any_element()
}

pub(crate) static GEM_STEPS: [u64; 5] = [300, 900, 1500, 2100, 2700];

pub(crate) fn gem_scene(_w: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let m = facet.measure(px(420.0 * facet.text_scale));
    let n = step(cx);
    use StageState::{Done, Now, Stall, Todo};
    let states: [[(StageState, f32); 4]; 6] = [
        [(Now, 0.5), (Todo, 0.0), (Todo, 0.0), (Todo, 0.0)],
        [(Done, 1.0), (Now, 0.4), (Todo, 0.0), (Todo, 0.0)],
        [(Done, 1.0), (Done, 1.0), (Now, 0.3), (Todo, 0.0)],
        [(Done, 1.0), (Done, 1.0), (Stall, 0.6), (Todo, 0.0)],
        [(Done, 1.0), (Done, 1.0), (Done, 1.0), (Now, 0.5)],
        [(Done, 1.0), (Done, 1.0), (Done, 1.0), (Done, 1.0)],
    ];
    let names = ["resolve", "fetch", "index", "seal"];
    let stages: Vec<Stage> = states[n.min(5)]
        .iter()
        .zip(names)
        .map(|((state, done), name)| Stage::new(name, *state).done(*done).weight(if name == "index" { 2.0 } else { 1.0 }))
        .collect();
    div()
        .size_full()
        .p(px(50.0))
        .flex()
        .flex_col()
        .gap(px(24.0))
        .child(gem_progress("film-gem", Kind::Struct, stages.clone(), &m).size(64.0))
        .child(div().w(px(420.0 * facet.text_scale)).child(seam("film-seam", stages, &m)))
        .into_any_element()
}

pub(crate) fn strands_scene(_w: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let m = facet.measure(px(620.0 * facet.text_scale));
    div()
        .size_full()
        .p(px(20.0))
        .child(
            rose("film-strands", Kind::Enum, &m)
                .list(false)
                .members(Dir::Is, vec![Member::new("Display", Kind::Trait).via()])
                .members(
                    Dir::To,
                    vec![
                        Member::new("as_str", Kind::Method),
                        Member::new("serialize()", Kind::Method).flow(),
                    ],
                ),
        )
        .into_any_element()
}

pub(crate) static RUNG_STEPS: [u64; 6] = [200, 500, 800, 1100, 1400, 1700];

pub(crate) fn rung_scene(_w: f32, _window: &mut Window, cx: &mut App) -> AnyElement {
    let facet = cx.facet();
    let widths = [900.0, 640.0, 440.0, 300.0, 170.0, 120.0, 60.0];
    let w = widths[step(cx).min(widths.len() - 1)];
    let m = facet.measure(px(w * facet.text_scale));
    let mut ticks = super::releases();
    ticks[46] = Tick::new(18.0).tone(TickTone::Current).label("1.0.193");
    div()
        .size_full()
        .p(px(50.0))
        .pt(px(60.0))
        .child(
            comb("film-rung", ticks, &m).caption([
                ("19 since your pin, ", false),
                ("2", true),
                (" touch your code", false),
            ]),
        )
        .into_any_element()
}

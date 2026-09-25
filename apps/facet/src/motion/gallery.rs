//! Motion scenes: the curve tokens plotted and travelled, the drop-in with a
//! list making room, and the ambient pulse.

use super::keys::{self, Pose};
use super::{LINEAR, Motion, Spec, offset, posed, pulse};
use crate::fonts::Typeset;
use crate::gallery::Scene;
use crate::probe;
use crate::theme::{ActiveFacet, Facet};
use crate::tokens::motion::{BOUNCE, Bezier, DROP, GLIDE, SNAP, SPRING};
use crate::tokens::{Face, Palette, Tone, TypeRole, ty};
use gpui::{
    AnyView, App, AppContext, Bounds, Context, InteractiveElement, IntoElement, ParentElement,
    PathBuilder, Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Window, canvas,
    div, fill, point, px, size,
};
use std::time::Duration;

pub(crate) const SCENES: &[Scene] = &[
    Scene {
        id: "motion-curves",
        title: "The five curve tokens (and linear) plotted, each travelled by a dot over 600 ms",
        size: (1240, 660),
        build: |_, cx| cx.new(|_| Curves::default()).into(),
    },
    Scene {
        id: "motion-drop-in",
        title: "drop-in (squash, overshoot, rebound, settle) with the list making room",
        size: (480, 420),
        build: |_, cx| cx.new(|_| DropIn::default()).into(),
    },
    Scene {
        id: "pulse",
        title: "The leased 12 fps ambient pulse: a twinkling facet row and a running bevel",
        size: (640, 320),
        build: build_pulse,
    },
];

const META: TypeRole = TypeRole {
    face: Face::Mono,
    weight: 400.0,
    size: 11.0,
    line: 14.0,
    tracking: 0.0,
    italic: false,
};

/// The plotted curves' shared duration.
const CURVE_TIME: Duration = Duration::from_millis(600);

const CURVES: [(&str, &str, Bezier); 6] = [
    ("glide", "cubic-bezier(.22, 1, .36, 1)", GLIDE),
    ("snap", "cubic-bezier(.3, 0, 0, 1)", SNAP),
    ("spring", "cubic-bezier(.2, .9, .25, 1.18)", SPRING),
    ("bounce", "cubic-bezier(.34, 1.56, .64, 1)", BOUNCE),
    ("drop", "cubic-bezier(.5, 0, .9, .6)", DROP),
    ("linear", "cubic-bezier(0, 0, 1, 1)", LINEAR),
];

#[derive(Default)]
struct Curves {
    motion: Motion,
}

/// Maps curve space (x 0..1, y -0.25..1.25) into a plot box.
fn plot_point(bounds: Bounds<Pixels>, x: f32, y: f32) -> gpui::Point<Pixels> {
    let (low, high) = (-0.25, 1.25);
    point(
        bounds.origin.x + bounds.size.width * x,
        bounds.origin.y + bounds.size.height * (1.0 - (y - low) / (high - low)),
    )
}

/// The colours a curve panel paints with.
#[derive(Clone, Copy)]
struct Ink {
    line: Tone,
    dot: Tone,
    grid: Tone,
    faint: Tone,
}

fn square(centre: gpui::Point<Pixels>, radius: Pixels) -> Bounds<Pixels> {
    Bounds {
        origin: point(centre.x - radius, centre.y - radius),
        size: size(radius * 2.0, radius * 2.0),
    }
}

/// The curve, its 0 / 0.5 / 1 rules, and the dot at (progress, value).
fn paint_plot(
    bounds: Bounds<Pixels>,
    curve: Bezier,
    progress: f32,
    value: f32,
    ink: Ink,
    window: &mut Window,
) {
    let inner = Bounds {
        origin: point(bounds.origin.x + px(12.0), bounds.origin.y + px(8.0)),
        size: size(bounds.size.width - px(24.0), bounds.size.height - px(16.0)),
    };
    for (level, tone) in [(0.0, ink.grid), (1.0, ink.grid), (0.5, ink.faint)] {
        let mut rule = PathBuilder::stroke(px(1.0));
        rule.move_to(plot_point(inner, 0.0, level));
        rule.line_to(plot_point(inner, 1.0, level));
        if let Ok(path) = rule.build() {
            window.paint_path(path, tone);
        }
    }
    let mut path = PathBuilder::stroke(px(2.0));
    path.move_to(plot_point(inner, 0.0, 0.0));
    for step in 1..=120_u8 {
        let x = f32::from(step) / 120.0;
        path.line_to(plot_point(inner, x, curve.ease(x)));
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, ink.line);
    }
    let radius = px(6.0);
    window.paint_quad(
        fill(square(plot_point(inner, progress, value), radius), ink.dot).corner_radii(radius),
    );
}

/// The value alone on a rail (what a moving element would do), with ticks at
/// 0 and 1 and room for overshoot either side.
fn paint_rail(bounds: Bounds<Pixels>, value: f32, ink: Ink, window: &mut Window) {
    let middle = bounds.center().y;
    let left = bounds.origin.x + px(12.0);
    let width = bounds.size.width - px(24.0);
    window.paint_quad(fill(
        Bounds {
            origin: point(left, middle),
            size: size(width, px(1.0)),
        },
        ink.faint,
    ));
    let at = |v: f32| left + width * ((v + 0.25) / 1.5);
    for tick in [0.0, 1.0] {
        window.paint_quad(fill(
            Bounds {
                origin: point(at(tick), middle - px(5.0)),
                size: size(px(1.0), px(11.0)),
            },
            ink.grid,
        ));
    }
    window
        .paint_quad(fill(square(point(at(value), middle), px(5.0)), ink.dot).corner_radii(px(2.0)));
}

fn curve_panel(
    facet: Facet,
    palette: &'static Palette,
    (name, formula, curve): (&'static str, &'static str, Bezier),
    progress: f32,
    value: f32,
) -> gpui::Div {
    let ink = Ink {
        line: palette.peri.base,
        dot: palette.mint.base,
        grid: palette.line3,
        faint: palette.line1,
    };
    let plot = canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            paint_plot(bounds, curve, progress, value, ink, window);
        },
    )
    .w_full()
    .h(facet.px(190.0));
    let rail = canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            paint_rail(bounds, value, ink, window);
        },
    )
    .w_full()
    .h(facet.px(22.0));
    div()
        .flex()
        .flex_col()
        .gap(facet.px(6.0))
        .p(facet.px(14.0))
        .bg(palette.plate)
        .border_1()
        .border_color(palette.line2.hsla())
        .child(
            div()
                .flex()
                .justify_between()
                .child(
                    div()
                        .typeset(ty::HEAD, &facet)
                        .text_color(palette.ink0.hsla())
                        .child(name),
                )
                .child(
                    div()
                        .typeset(META, &facet)
                        .text_color(palette.ink3.hsla())
                        .child(formula),
                ),
        )
        .child(probe::measure(
            SharedString::from(format!("curve-plot.{name}")),
            plot,
        ))
        .child(rail)
}

impl Render for Curves {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let progress = self.motion.animate_from(
            "curves.progress",
            0.0,
            1.0,
            Spec::tween(CURVE_TIME, LINEAR),
            window,
            cx,
        );
        let [first, second, third, fourth, fifth, sixth] = CURVES.map(|(name, formula, curve)| {
            let value = self.motion.animate_from(
                SharedString::from(format!("curves.{name}")),
                0.0,
                1.0,
                Spec::tween(CURVE_TIME, curve),
                window,
                cx,
            );
            curve_panel(facet, palette, (name, formula, curve), progress, value).flex_1()
        });
        div()
            .id("motion-curves")
            .size_full()
            .bg(palette.g1)
            .p(facet.px(28.0))
            .flex()
            .flex_col()
            .gap(facet.px(16.0))
            .on_click(cx.listener(|this, _, _, cx| {
                this.motion = Motion::new();
                cx.notify();
            }))
            .child(
                div()
                    .typeset(META, &facet)
                    .text_color(palette.ink3.hsla())
                    .child(format!(
                        "t = {:.0} ms of {} ms",
                        progress * 600.0,
                        CURVE_TIME.as_millis()
                    )),
            )
            .child(
                div()
                    .flex()
                    .gap(facet.px(16.0))
                    .children([first, second, third]),
            )
            .child(
                div()
                    .flex()
                    .gap(facet.px(16.0))
                    .children([fourth, fifth, sixth]),
            )
    }
}

/// Row pitch in the drop-in list: 34 px rows and a 10 px gap.
const ROW: f32 = 34.0;
const GAP: f32 = 10.0;
const ROWS: [&str; 4] = ["serde_json", "serde_derive", "toml", "ron"];

#[derive(Default)]
struct DropIn {
    motion: Motion,
}

/// A flat plate with the 1 px two-tone bevel: light top-left, shade
/// bottom-right.
fn paint_bevelled(
    window: &mut Window,
    body: Bounds<Pixels>,
    plate: Tone,
    light: Tone,
    shade: Tone,
) {
    let (left, top) = (body.origin.x, body.origin.y);
    let (width, height) = (body.size.width, body.size.height);
    let edge = px(1.0);
    let strip = |x, y, w, h| Bounds {
        origin: point(x, y),
        size: size(w, h),
    };
    window.paint_quad(fill(body, plate));
    window.paint_quad(fill(strip(left, top, width, edge), light));
    window.paint_quad(fill(strip(left, top, edge, height), light));
    window.paint_quad(fill(strip(left, top + height - edge, width, edge), shade));
    window.paint_quad(fill(strip(left + width - edge, top, edge, height), shade));
}

/// The cut-less placeholder plate, painted under a pose so squash and
/// stretch read (F2's cut plate replaces it).
fn placeholder(palette: &'static Palette, pose: Pose) -> impl IntoElement {
    let (plate, light, shade, mark, bar) = (
        palette.plate2,
        palette.mint.base,
        palette.bevel_lo,
        palette.mint.base,
        palette.ink2,
    );
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let body = posed(bounds, pose);
            let tone = |tone: Tone| tone.alpha(pose.opacity);
            paint_bevelled(window, body, tone(plate), tone(light), tone(shade));
            // A diamond mark and two text bars, scaled with the plate.
            let unit_x = body.size.width / 360.0;
            let unit_y = body.size.height / ROW;
            let centre = point(body.origin.x + unit_x * 20.0, body.center().y);
            let mut diamond = PathBuilder::fill();
            diamond.move_to(point(centre.x, centre.y - unit_y * 7.0));
            diamond.line_to(point(centre.x + unit_x * 7.0, centre.y));
            diamond.line_to(point(centre.x, centre.y + unit_y * 7.0));
            diamond.line_to(point(centre.x - unit_x * 7.0, centre.y));
            diamond.close();
            if let Ok(path) = diamond.build() {
                window.paint_path(path, tone(mark));
            }
            for (start, length, strength) in [(38.0, 120.0, 0.9), (170.0, 60.0, 0.45)] {
                let text = Bounds {
                    origin: point(body.origin.x + unit_x * start, centre.y - unit_y * 3.0),
                    size: size(unit_x * length, unit_y * 6.0),
                };
                window.paint_quad(fill(text, tone(bar.alpha(strength))).corner_radii(unit_y * 3.0));
            }
        },
    )
    .w_full()
    .h(px(ROW))
}

impl Render for DropIn {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let pose = self.motion.play("drop-in.card", &keys::DROP_IN, window, cx);
        let room = keys::make_room(ROW + GAP);
        let rows = ROWS
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let shift = self.motion.play(("drop-in.room", index), &room, window, cx);
                // Outer: the laid-out slot. Inner: where the row is painted.
                probe::measure(
                    ("drop-in.slot", index),
                    offset(probe::measure(
                        ("drop-in.row", index),
                        div()
                            .h(px(ROW))
                            .px(px(14.0))
                            .flex()
                            .items_center()
                            .bg(palette.plate)
                            .border_1()
                            .border_color(palette.line2.hsla())
                            .typeset(ty::MONO_ROW, &facet)
                            .text_color(palette.ink1.hsla())
                            .child(*name),
                    ))
                    .pose(shift),
                )
            })
            .collect::<Vec<_>>();
        div()
            .id("motion-drop-in")
            .size_full()
            .bg(palette.g1)
            .px(px(60.0))
            .py(px(36.0))
            .flex()
            .flex_col()
            .gap(px(GAP))
            .on_click(cx.listener(|this, _, _, cx| {
                this.motion = Motion::new();
                cx.notify();
            }))
            .child(
                div()
                    .h(px(40.0))
                    .typeset(META, &facet)
                    .text_color(palette.ink3.hsla())
                    .child("drop-in \u{b7} make-room \u{b7} click to replay"),
            )
            .child(probe::measure("drop-in.card", placeholder(palette, pose)))
            .children(rows)
    }
}

fn build_pulse(_window: &mut Window, cx: &mut App) -> AnyView {
    pulse::thaw(cx);
    cx.new(|_| PulseDemo).into()
}

struct PulseDemo;

/// A 48 px light travelling a box's edge clockwise, `phase` of the way round.
fn paint_running_light(window: &mut Window, bounds: Bounds<Pixels>, phase: f32, light: Tone) {
    let (left, top) = (bounds.origin.x, bounds.origin.y);
    let (width, height) = (bounds.size.width, bounds.size.height);
    let perimeter = (width + height) * 2.0;
    let step = px(2.0);
    let start = perimeter * phase;
    let mut along = start;
    while along < start + px(48.0) {
        let at = along % perimeter;
        let (origin, piece) = if at < width {
            (point(left + at, top), size(step, px(2.0)))
        } else if at < width + height {
            (
                point(left + width - px(2.0), top + (at - width)),
                size(px(2.0), step),
            )
        } else if at < width * 2.0 + height {
            (
                point(left + width - (at - width - height), top + height - px(2.0)),
                size(step, px(2.0)),
            )
        } else {
            (
                point(left, top + height - (at - width * 2.0 - height)),
                size(px(2.0), step),
            )
        };
        window.paint_quad(fill(
            Bounds {
                origin,
                size: piece,
            },
            light,
        ));
        along += step;
    }
}

impl Render for PulseDemo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let facet = cx.facet();
        let palette = facet.palette();
        let pulse = pulse::lease(window, cx);
        let facets = (0..12_u8).map(|index| {
            let glow = pulse.wave(1.5, f32::from(index) / 12.0);
            div()
                .size(px(22.0))
                .bg(palette.peri.base.alpha(0.15 + 0.85 * glow))
                .border_1()
                .border_color(palette.line3.hsla())
        });
        let phase = pulse.phase(2.0);
        let (plate, shade, light, run) = (
            palette.plate2,
            palette.bevel_lo,
            palette.bevel_hi,
            palette.mint.base,
        );
        let bevel = canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                paint_bevelled(window, bounds, plate, light, shade);
                paint_running_light(window, bounds, phase, run);
            },
        )
        .w(px(360.0))
        .h(px(64.0));
        div()
            .size_full()
            .bg(palette.g1)
            .p(px(32.0))
            .flex()
            .flex_col()
            .gap(px(22.0))
            .child(
                div()
                    .typeset(META, &facet)
                    .text_color(palette.ink3.hsla())
                    .child(format!(
                        "pulse t = {:.3} s \u{b7} tick {} \u{b7} phase {:.3}",
                        pulse.seconds,
                        (pulse.seconds / pulse::TICK.as_secs_f32()).round(),
                        phase
                    )),
            )
            .child(div().flex().gap(px(8.0)).children(facets))
            .child(bevel)
    }
}

//! Switch, check and radio: "no pills, no rounded pucks — one geometry, the
//! diamond, for anything with two states."
//!
//! - **switch**: a diamond bead on a short cut track; on, it fills mint and
//!   travels on a spring, stretching along the way and leaning toward where
//!   it will go while pressed;
//! - **check**: the same diamond standing still — it fills, the tick lands
//!   with a bounce; *mixed* fills periwinkle with a bar;
//! - **radio**: a hollow diamond whose heart grows in when chosen.
//!
//! Each takes an optional label, so the whole row is the hit target (never
//! under 24 px) and hovering it lights the mark.

use super::button::{Handler, sunk, wire};
use super::state::{Look, Touch, hover_zone, track};
use super::{clear, muted};
use crate::Set;
use crate::measure::{Control, Measure, Space};
use crate::motion::{BOUNCY, Spec, spec};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::motion::{BOUNCE, QUICK};
use crate::tokens::{Palette, ty};
use gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, Styled, Window, div, px,
};
use std::rc::Rc;

/// A checkbox's three states.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Tri {
    /// Hollow.
    #[default]
    Off,
    /// Mint with a tick.
    On,
    /// Periwinkle with a bar: some of what it stands for is on.
    Mixed,
}

/// Which toggle a row holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Switch(bool),
    Check(Tri),
    Radio(bool),
}

/// A switch, check or radio, with an optional label and note.
#[derive(IntoElement)]
pub struct Toggle {
    id: ElementId,
    kind: Kind,
    label: Option<SharedString>,
    note: Option<SharedString>,
    disabled: bool,
    look: Look,
    measure: Measure,
    on_toggle: Option<Handler>,
}

fn toggle(id: impl Into<ElementId>, kind: Kind, measure: &Measure) -> Toggle {
    Toggle {
        id: id.into(),
        kind,
        label: None,
        note: None,
        disabled: false,
        look: Look::LIVE,
        measure: *measure,
        on_toggle: None,
    }
}

/// A diamond switch, on or off.
#[must_use]
pub fn switch(id: impl Into<ElementId>, on: bool, measure: &Measure) -> Toggle {
    toggle(id, Kind::Switch(on), measure)
}

/// A diamond check in one of three states.
#[must_use]
pub fn check(id: impl Into<ElementId>, state: Tri, measure: &Measure) -> Toggle {
    toggle(id, Kind::Check(state), measure)
}

/// One radio of a family, chosen or not.
#[must_use]
pub fn radio(id: impl Into<ElementId>, chosen: bool, measure: &Measure) -> Toggle {
    toggle(id, Kind::Radio(chosen), measure)
}

impl Toggle {
    /// The row's label (clicking it toggles too).
    #[must_use]
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// A quiet mono note after the label ("on by default").
    #[must_use]
    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Disabled: .4 opacity, deaf.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Pins an appearance on top of the live state.
    #[must_use]
    pub const fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    /// Called on click, Enter or Space; the caller owns the next state.
    #[must_use]
    pub fn on_toggle(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
        self
    }
}

/// Everything one frame of a toggle mark paints, as plain numbers.
#[derive(Clone, Copy)]
struct Paint {
    /// 0 = off, 1 = on (the switch's travel and fill, the check's fill).
    on: f32,
    /// The check's mixed state, 0..1.
    mixed: f32,
    /// The tick's scale, 0..1 (lands with a bounce).
    tick: f32,
    hover: f32,
    press: f32,
    focus: f32,
    /// Switch bead x, 0..1 of its travel (spring; may overshoot).
    x: f32,
    scale: f32,
}

fn tones(palette: &Palette) -> (Hsla, Hsla, Hsla, Hsla, Hsla, Hsla) {
    (
        palette.table.into(),
        palette.ink2.into(),
        palette.ink0.into(),
        palette.mint.base.into(),
        palette.peri.base.into(),
        palette.peri_hi.into(),
    )
}

/// The switch track: a cut plate painted behind the bead.
fn switch_mark(paint: Paint, palette: &'static Palette) -> AnyElement {
    let s = paint.scale;
    let (w, h) = (32.0 * s, 16.0 * s);
    let rest = Edge::of(Bevel::Rest, palette);
    let track_edge = rest
        .mix(sunk(rest), paint.press * 0.6)
        .mix(Edge::of(Bevel::Focus, palette), paint.focus);
    let fill = mix(palette.plate2.into(), palette.plate3.into(), paint.hover);
    let (table, ink2, ink0, mint, _, _) = tones(palette);
    let r = 7.07 * s;
    let inset = 8.0 * s;
    let travel = 17.0 * s;
    let lean = paint.press * 2.0 * s * if paint.on > 0.5 { -1.0 } else { 1.0 };
    let center_x = inset + travel * paint.x + lean;
    let stretch = 1.0 + 0.32 * (paint.x - paint.on).abs().min(1.0);
    let rx = r * stretch;
    let ry = r / stretch.sqrt();
    let body = mix(table, mint, paint.on);
    let line = mix(ink2, ink0, paint.hover);
    let bead_edge = mix(line, clear(mint), paint.on);
    let bead = div()
        .absolute()
        .left(px(center_x - rx))
        .top(px((h - 2.0 * ry) * 0.5))
        .w(px(2.0 * rx))
        .h(px(2.0 * ry))
        .child(
            crate::controls::diamond::diamond()
                .fill(body)
                .size(px(2.0 * rx))
                .h(px(2.0 * ry)),
        )
        .child(
            crate::controls::diamond::diamond()
                .inset(0.8 * s)
                .outline(bead_edge, 1.6 * s)
                .size(px(2.0 * rx))
                .h(px(2.0 * ry)),
        );
    cut()
        .chamfer(Chamfer::Px(3.0 * s))
        .edge(track_edge)
        .plate(Plate::Flat)
        .fill(fill)
        .relative()
        .flex_none()
        .w(px(w))
        .h(px(h))
        .child(div().absolute().inset_0().child(bead))
        .into_any_element()
}

/// The check and the radio: a standing diamond (and its tick or heart).
fn standing_mark(radio: bool, paint: Paint, palette: &'static Palette) -> AnyElement {
    let s = paint.scale;
    let (table, ink2, ink0, mint, peri, peri_hi) = tones(palette);
    let mark_size = 20.0 * s;
    let r = 8.49 * s * (1.0 - 0.06 * paint.press);
    let diameter = 2.0 * r;
    let edge = mix(ink2, ink0, paint.hover);
    let (fill, outline) = if radio {
        (table, mix(edge, mint, paint.on))
    } else {
        let voice = mix(mint, peri, paint.mixed);
        let lit = paint.on.max(paint.mixed);
        (mix(table, voice, lit), mix(edge, clear(voice), lit))
    };
    let left = (mark_size - diameter) * 0.5;
    let top = (mark_size - diameter) * 0.5;
    let mut mark = div()
        .relative()
        .flex_none()
        .size(px(mark_size))
        .child(
            crate::controls::diamond::diamond()
                .fill(fill)
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(diameter))
                .h(px(diameter)),
        )
        .child(
            crate::controls::diamond::diamond()
                .inset(0.8 * s)
                .outline(outline, 1.6 * s)
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(diameter))
                .h(px(diameter)),
        );
    if radio {
        let heart = r * 0.52 * paint.tick;
        if heart > 0.2 {
            let d = 2.0 * heart;
            mark = mark.child(
                crate::controls::diamond::diamond()
                    .fill(mint)
                    .absolute()
                    .left(px((mark_size - d) * 0.5))
                    .top(px((mark_size - d) * 0.5))
                    .w(px(d))
                    .h(px(d)),
            );
        }
    } else {
        if paint.tick > 0.02 && paint.mixed < 0.5 {
            let k = 10.0 / 24.0 * s * paint.tick;
            let center = mark_size * 0.5;
            let tick: Hsla = palette.mint_ink.into();
            mark = mark.child(
                crate::controls::native::native_paths(mark_size, mark_size)
                    .stroke(
                        &[
                            (center - 4.25 * k, center),
                            (center + 0.25 * k, center + 4.5 * k),
                            (center + 4.25 * k, center - 4.5 * k),
                        ],
                        1.7 * s,
                        false,
                        tick,
                    )
                    .absolute()
                    .inset_0(),
            );
        }
        if paint.mixed > 0.02 {
            let half = 3.6 * s * paint.mixed;
            mark = mark.child(
                div()
                    .absolute()
                    .left(px(mark_size * 0.5 - half))
                    .top(px(mark_size * 0.5 - 0.9 * s))
                    .w(px(half * 2.0))
                    .h(px(1.8 * s))
                    .bg(palette.table),
            );
        }
    }
    if paint.focus > 0.0 {
        let first = 2.0 * (r + 2.6 * s);
        let second = 2.0 * (r + 4.2 * s);
        mark = mark
            .child(
                crate::controls::diamond::diamond()
                    .outline(mix(clear(peri_hi), peri_hi, paint.focus), 1.2 * s)
                    .absolute()
                    .left(px((mark_size - first) * 0.5))
                    .top(px((mark_size - first) * 0.5))
                    .w(px(first))
                    .h(px(first)),
            )
            .child(
                crate::controls::diamond::diamond()
                    .outline(mix(clear(peri), peri, paint.focus * 0.7), 1.0 * s)
                    .absolute()
                    .left(px((mark_size - second) * 0.5))
                    .top(px((mark_size - second) * 0.5))
                    .w(px(second))
                    .h(px(second)),
            );
    }
    mark.into_any_element()
}

impl RenderOnce for Toggle {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let active = !self.disabled;
        let touch = Touch::read(&self.id, self.look, active, window, cx);
        let motion = touch.motion.clone();
        let id = self.id.clone();
        let scale = f32::from(measure.icon(16.0)) / 16.0;

        let (on, mixed) = match self.kind {
            Kind::Switch(on) | Kind::Radio(on) => (on, false),
            Kind::Check(tri) => (tri == Tri::On, tri == Tri::Mixed),
        };
        let hover = motion.animate(
            track(&id, "hover"),
            if touch.hovered { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let press = motion.animate(
            track(&id, "press"),
            if touch.pressed { 1.0 } else { 0.0 },
            spec::PRESS,
            window,
            cx,
        );
        let focus = motion.animate(
            track(&id, "focus"),
            if touch.focused { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let on_t = motion.animate(
            track(&id, "on"),
            if on { 1.0 } else { 0.0 },
            spec::REVEAL,
            window,
            cx,
        );
        let mixed_t = motion.animate(
            track(&id, "mixed"),
            if mixed { 1.0 } else { 0.0 },
            spec::REVEAL,
            window,
            cx,
        );
        let tick = motion.animate(
            track(&id, "tick"),
            if on || mixed { 1.0 } else { 0.0 },
            Spec::tween(QUICK, BOUNCE),
            window,
            cx,
        );
        let x = if matches!(self.kind, Kind::Switch(_)) {
            motion.animate(
                track(&id, "x"),
                if on { 1.0 } else { 0.0 },
                BOUNCY,
                window,
                cx,
            )
        } else {
            0.0
        };
        let paint = Paint {
            on: on_t,
            mixed: mixed_t,
            tick,
            hover,
            press,
            focus,
            x,
            scale,
        };
        let mark = match self.kind {
            Kind::Switch(_) => switch_mark(paint, palette),
            Kind::Check(_) => standing_mark(false, paint, palette),
            Kind::Radio(_) => standing_mark(true, paint, palette),
        };

        let mut ink: Hsla = mix(palette.ink1.into(), palette.ink0.into(), hover);
        if self.disabled {
            ink = muted(ink);
        }
        let mut row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(measure.space(Space::Snug) + measure.space(Space::Hair))
            .min_h(measure.control(Control::Small))
            .child(mark);
        if let Some(label) = self.label.clone() {
            row = row.child(
                div()
                    .set(ty::ROW, &measure)
                    .text_color(ink)
                    .whitespace_nowrap()
                    .child(label),
            );
        }
        if let Some(note) = self.note.clone() {
            row = row.child(
                div()
                    .set(ty::MONO_SMALL, &measure)
                    .text_color(palette.ink3.hsla())
                    .whitespace_nowrap()
                    .child(note),
            );
        }
        let row = row.id(id).opacity(if self.disabled { 0.4 } else { 1.0 });
        let row = if active {
            wire(row, &touch, self.on_toggle).into_any_element()
        } else {
            row.into_any_element()
        };
        hover_zone(row, &touch, 0.0, active)
    }
}

//! The segmented control: a row of choices — plain words at rest — and one
//! raised cut stone (`plate3`, the bevel) that *is* the selection. As a
//! **well** (the Controls board's lens switcher) the row sits in a recessed
//! plate whose bevel is inverted (shaded top-left, lit bottom-right). The plate glides between choices on a
//! spring (position and width both, so it reshapes as it travels), leans a
//! few px toward a choice under the pointer, sinks while pressed and takes
//! the doubled periwinkle bevel when the control has keyboard focus.
//! ←/→ (and Home/End) move the selection.
//!
//! Every choice is measured before layout, so the plate sits exactly under
//! its label from the first frame.
//!
//! [`density_toggle`] is the same control over the three densities (words by
//! default, or [`Seg::art`]: the same four rows painted at each density's
//! pitch); [`theme_toggle`] carries a swatch diamond per theme.

use super::button::sunk;
use super::kbd::key_badge_at;
use super::state::{Look, Touch, hover_zone, set_hot_item, set_key_pressed, set_pressed, track, track_n};
use super::text;
use crate::Set;
use crate::icons::{Icon, IconSize, ui};
use crate::measure::{Control, Density, Measure};
use crate::motion::spec;
use crate::paint::geom::{Fill, Poly};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::{ABYSS, Face, GLACIER, TypeRole};
use gpui::{
    AnyElement, App, ElementId, Hsla, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window,
    canvas, div, px,
};
use std::rc::Rc;
use std::sync::Arc;

/// A theme swatch: a small diamond in the theme's own plate tone.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Swatch {
    /// Follows the system: half Abyss, half Glacier.
    System,
    /// Abyss.
    Abyss,
    /// Glacier.
    Glacier,
}

/// What one choice shows.
#[derive(Clone, Debug)]
enum Face_ {
    Label(SharedString),
    Icon(Icon, Option<SharedString>),
    Density(Density, SharedString),
    Swatch(Swatch, SharedString),
}

/// How the row is set.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum SegStyle {
    /// Words on the ground; the selection is the only plate (Settings).
    #[default]
    Stones,
    /// Inside a recessed well, smaller (the lens switcher).
    Well,
}

/// One choice.
#[derive(Clone, Debug)]
struct Choice {
    face: Face_,
    /// The accessible name / tooltip.
    name: SharedString,
    key: Option<SharedString>,
}

type Select = Rc<dyn Fn(usize, &mut Window, &mut App)>;

/// A segmented control: `seg("lens", &measure).label("Map").label("Readme").selected(1)`.
#[derive(IntoElement)]
pub struct Seg {
    id: ElementId,
    choices: Vec<Choice>,
    style: SegStyle,
    art: bool,
    selected: usize,
    disabled: bool,
    look: Look,
    measure: Measure,
    on_select: Option<Select>,
}

/// An empty segmented control sized for `measure`.
#[must_use]
pub fn seg(id: impl Into<ElementId>, measure: &Measure) -> Seg {
    Seg {
        id: id.into(),
        choices: Vec::new(),
        style: SegStyle::Stones,
        art: false,
        selected: 0,
        disabled: false,
        look: Look::LIVE,
        measure: *measure,
        on_select: None,
    }
}

impl Seg {
    /// Adds a worded choice.
    #[must_use]
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        self.choices.push(Choice {
            face: Face_::Label(label.clone()),
            name: label,
            key: None,
        });
        self
    }

    /// Adds a choice shown as an icon (with an optional word beside it).
    #[must_use]
    pub fn icon(mut self, icon: Icon, name: impl Into<SharedString>, worded: bool) -> Self {
        let name = name.into();
        self.choices.push(Choice {
            face: Face_::Icon(icon, worded.then(|| name.clone())),
            name,
            key: None,
        });
        self
    }

    /// Adds a theme choice with its swatch.
    #[must_use]
    pub fn swatch(mut self, swatch: Swatch, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        self.choices.push(Choice {
            face: Face_::Swatch(swatch, label.clone()),
            name: label,
            key: None,
        });
        self
    }

    /// Sets the row inside a recessed well.
    #[must_use]
    pub const fn well(mut self) -> Self {
        self.style = SegStyle::Well;
        self
    }

    /// Density choices drawn as painted stones instead of words.
    #[must_use]
    pub const fn art(mut self) -> Self {
        self.art = true;
        self
    }

    /// Gives the last choice a key (its cap rises while ⌘ is held).
    #[must_use]
    pub fn key(mut self, key: impl Into<SharedString>) -> Self {
        if let Some(last) = self.choices.last_mut() {
            last.key = Some(key.into());
        }
        self
    }

    /// The chosen index.
    #[must_use]
    pub const fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Disabled: .42 opacity, deaf.
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

    /// Called with the chosen index (pointer, ←/→, Home/End).
    #[must_use]
    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// The accessible names of the choices, in order.
    #[must_use]
    pub fn names(&self) -> Vec<SharedString> {
        self.choices.iter().map(|choice| choice.name.clone()).collect()
    }
}

/// The three densities as one control of painted stones.
#[must_use]
pub fn density_toggle(id: impl Into<ElementId>, current: Density, measure: &Measure) -> Seg {
    let mut control = seg(id, measure);
    for (density, name) in [
        (Density::Comfortable, "Comfortable"),
        (Density::Compact, "Compact"),
        (Density::Dense, "Dense"),
    ] {
        control.choices.push(Choice {
            face: Face_::Density(density, name.into()),
            name: name.into(),
            key: None,
        });
    }
    control.selected = match current {
        Density::Comfortable => 0,
        Density::Compact => 1,
        Density::Dense => 2,
    };
    control
}

/// The three themes, each with its swatch.
#[must_use]
pub fn theme_toggle(id: impl Into<ElementId>, current: Swatch, measure: &Measure) -> Seg {
    seg(id, measure)
        .swatch(Swatch::System, "System")
        .swatch(Swatch::Abyss, "Abyss")
        .swatch(Swatch::Glacier, "Glacier")
        .selected(match current {
            Swatch::System => 0,
            Swatch::Abyss => 1,
            Swatch::Glacier => 2,
        })
}

/// `.seg>button`: Geist 550 12 px.
const WELL_LABEL: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 550.0,
    size: 12.0,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

/// `.stone`: Geist 500 12.5 px.
const STONE_LABEL: TypeRole = TypeRole {
    face: Face::Ui,
    weight: 500.0,
    size: 12.5,
    line: 16.0,
    tracking: 0.0,
    italic: false,
};

/// A swatch diamond: the theme's plate with its quiet outline (System is
/// split on the diagonal).
fn swatch_art(swatch: Swatch, size: Pixels) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let c = bounds.center();
            let (cx, cy) = (f32::from(c.x), f32::from(c.y));
            let r = f32::from(bounds.size.width) * 0.5;
            let poly = Poly::new([
                crate::paint::geom::pt(cx, cy - r),
                crate::paint::geom::pt(cx + r, cy),
                crate::paint::geom::pt(cx, cy + r),
                crate::paint::geom::pt(cx - r, cy),
            ]);
            let (dark, light) = (&ABYSS, &GLACIER);
            let paint = |window: &mut Window, poly: &Poly, color: Hsla| {
                let mut fill = Fill::new();
                fill.poly(poly);
                fill.paint(window, color);
            };
            let ring = |window: &mut Window, color: Hsla| {
                let mut fill = Fill::new();
                for piece in poly.offset(-0.5).stroke_ring(1.0) {
                    fill.poly(&piece);
                }
                fill.paint(window, color);
            };
            match swatch {
                Swatch::Abyss => {
                    paint(window, &poly, dark.plate.into());
                    ring(window, dark.ink4.into());
                }
                Swatch::Glacier => {
                    paint(window, &poly, light.g1.into());
                    ring(window, light.ink4.into());
                }
                Swatch::System => {
                    let top = poly.clip_half_plane(
                        crate::paint::geom::pt(cx - r, cy),
                        crate::paint::geom::pt(cx + r, cy),
                    );
                    let bottom = poly.clip_half_plane(
                        crate::paint::geom::pt(cx + r, cy),
                        crate::paint::geom::pt(cx - r, cy),
                    );
                    paint(window, &top, dark.plate.into());
                    paint(window, &bottom, light.g1.into());
                    ring(window, dark.ink3.into());
                }
            }
        },
    )
    .flex_none()
    .size(size)
    .into_any_element()
}

/// A painted density stone: four rows (a mark and a bar each) at the
/// density's row pitch, in `ink`.
fn density_art(density: Density, size: Pixels, ink: Hsla) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let s = f32::from(bounds.size.width) / 22.0;
            let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let pitch = 5.4 * density.row() * s;
            let total = pitch * 3.0 + 1.6 * s;
            let top = y0 + (f32::from(bounds.size.height) - total) * 0.5;
            let mut marks = Fill::new();
            let mut bars = Fill::new();
            for row in 0..4_u8 {
                let y = top + pitch * f32::from(row);
                marks.poly(&Poly::rect(x0 + 2.0 * s, y, 2.4 * s, 1.6 * s));
                let long = if row % 2 == 0 { 13.0 } else { 10.0 };
                bars.poly(&Poly::rect(x0 + 6.0 * s, y, long * s, 1.6 * s));
            }
            marks.paint(window, ink);
            bars.paint(window, Hsla { alpha: ink.alpha * 0.55, ..ink });
        },
    )
    .flex_none()
    .size(size)
    .into_any_element()
}

impl RenderOnce for Seg {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let active = !self.disabled;
        let touch = Touch::read(&self.id, self.look, active, window, cx);
        let motion = touch.motion.clone();
        let id = self.id.clone();
        let count = self.choices.len().max(1);
        let selected = self.selected.min(count - 1);

        let well = self.style == SegStyle::Well;
        let (item_h, label_role, spread, gap_px) = if well {
            (measure.control(Control::Small), WELL_LABEL, 11.0, 2.0)
        } else {
            (measure.control(Control::Medium), STONE_LABEL, 13.0, 4.0)
        };
        let s = f32::from(measure.control(Control::Small)) / 24.0;
        let pad = if well { px(2.0 * s) } else { px(0.0) };
        let gap = px(gap_px * s);
        let icon_px = measure.icon(14.0);
        let art_px = measure.icon(22.0);
        let swatch_px = measure.icon(12.0);
        let inner = px(8.0 * s);
        let widths: Vec<Pixels> = self
            .choices
            .iter()
            .map(|choice| match &choice.face {
                Face_::Label(label) => {
                    text::width(label, label_role, &measure, window) + px(2.0 * spread * s)
                }
                Face_::Icon(_, None) => item_h + px(4.0 * s),
                Face_::Icon(_, Some(label)) => {
                    text::width(label, label_role, &measure, window)
                        + icon_px
                        + inner
                        + px(2.0 * spread * s)
                }
                Face_::Density(_, label) if !self.art => {
                    text::width(label, label_role, &measure, window) + px(2.0 * spread * s)
                }
                Face_::Density(..) => art_px + px(14.0 * s),
                Face_::Swatch(_, label) => {
                    text::width(label, label_role, &measure, window)
                        + swatch_px
                        + inner
                        + px(2.0 * spread * s)
                }
            })
            .collect();
        let mut xs = Vec::with_capacity(widths.len());
        let mut at = pad;
        for width in &widths {
            xs.push(at);
            at += *width + gap;
        }
        let well_w = at - gap + pad;
        let well_h = item_h + pad * 2.0;

        let (target_x, target_w) = xs
            .get(selected)
            .copied()
            .zip(widths.get(selected).copied())
            .unwrap_or((pad, px(0.0)));
        let x = motion.animate(track(&id, "x"), f32::from(target_x), spec::FOLLOW, window, cx);
        let w = motion.animate(track(&id, "w"), f32::from(target_w), spec::FOLLOW, window, cx);
        // Lean toward a hovered choice: the plate wants to go there.
        let lean_to = match touch.hot_item {
            Some(hot) if hot != selected && active => {
                if hot > selected { 1.0 } else { -1.0 }
            }
            _ => 0.0,
        };
        let lean = motion.animate(track(&id, "lean"), lean_to, spec::HOVER, window, cx);
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
        let reach = 4.0 * s * lean;
        let plate_x = x + reach.min(0.0);
        let plate_w = w + reach.abs();

        let rest = Edge::of(Bevel::Rest, palette);
        let plate_edge = rest
            .mix(sunk(rest), press)
            .mix(Edge::of(Bevel::Focus, palette), focus);
        let chamfer = f32::from(item_h) * if well { 0.25 } else { 7.0 / 30.0 };
        let plate = cut()
            .chamfer(Chamfer::Px(chamfer))
            .edge(plate_edge)
            .plate(Plate::Flat)
            .fill(palette.plate3)
            .absolute()
            .top(pad)
            .left(px(plate_x))
            .w(px(plate_w.max(0.0)))
            .h(item_h);

        let mut row = div().absolute().top(pad).left(pad).flex().gap(gap);
        for (index, choice) in self.choices.iter().enumerate() {
            let hot = touch.hot_item == Some(index);
            let lit = motion.animate(
                track_n(&id, "lit", index),
                if index == selected || hot { 1.0 } else { 0.0 },
                spec::HOVER,
                window,
                cx,
            );
            let rest_ink = if well { palette.ink2 } else { palette.ink3 };
            let ink = mix(rest_ink.into(), palette.ink0.into(), lit);
            let word = |label: &SharedString| {
                div()
                    .set(label_role, &measure)
                    .text_color(ink)
                    .whitespace_nowrap()
                    .child(label.clone())
            };
            let face: AnyElement = match &choice.face {
                Face_::Density(density, _) if self.art => density_art(*density, art_px, ink),
                Face_::Label(label) | Face_::Density(_, label) => word(label).into_any_element(),
                Face_::Icon(icon, label) => div()
                    .flex()
                    .items_center()
                    .gap(inner)
                    .child(ui(*icon, IconSize::S14, ink).size(icon_px))
                    .children(label.as_ref().map(word))
                    .into_any_element(),
                Face_::Swatch(swatch, label) => div()
                    .flex()
                    .items_center()
                    .gap(inner)
                    .child(swatch_art(*swatch, swatch_px))
                    .child(word(label))
                    .into_any_element(),
            };
            let mut item = div()
                .id(("choice", index))
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .w(widths[index])
                .h(item_h)
                .child(face);
            if let Some(key) = &choice.key {
                let key_id = ElementId::NamedChild(Arc::new(id.clone()), format!("k{index}").into());
                item = item.child(key_badge_at(key, &motion, &key_id, &measure, active));
            }
            if active {
                let entity = touch.entity.clone();
                item = item.on_hover(move |inside, _window, cx| {
                    let now = entity.read(cx).hot_item;
                    if *inside {
                        set_hot_item(&entity, Some(index), cx);
                    } else if now == Some(index) {
                        set_hot_item(&entity, None, cx);
                    }
                });
                if let Some(select) = self.on_select.clone() {
                    item = item.on_click(move |_, window, cx| select(index, window, cx));
                }
            }
            row = row.child(item);
        }

        let well_edge = if well {
            sunk(rest)
        } else {
            Edge {
                hi: super::clear(rest.hi),
                lo: super::clear(rest.lo),
                ..rest
            }
        };
        let well_fill: Hsla = if well {
            palette.inset.into()
        } else {
            super::clear(palette.inset.into())
        };
        let well = cut()
            .chamfer(Chamfer::Px(f32::from(well_h) * 0.25))
            .edge(well_edge)
            .plate(Plate::Flat)
            .fill(well_fill)
            .relative()
            .flex_none()
            .w(well_w)
            .h(well_h)
            .child(plate)
            .child(row)
            .id(id)
            .opacity(if self.disabled { 0.42 } else { 1.0 });
        let well = if active {
            let entity = touch.entity.clone();
            let select = self.on_select.clone();
            well.track_focus(&touch.focus)
                .tab_index(0)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, {
                    let entity = entity.clone();
                    move |_, _window, cx| set_pressed(&entity, true, cx)
                })
                .on_mouse_up(MouseButton::Left, {
                    let entity = entity.clone();
                    move |_, _window, cx| set_pressed(&entity, false, cx)
                })
                .on_mouse_up_out(MouseButton::Left, {
                    let entity = entity.clone();
                    move |_, _window, cx| set_pressed(&entity, false, cx)
                })
                .on_key_up({
                    let entity = entity.clone();
                    move |_, _window, cx| set_key_pressed(&entity, false, cx)
                })
                .on_key_down(move |event: &KeyDownEvent, window, cx| {
                    let next = match event.keystroke.key.as_str() {
                        "left" => Some(selected.saturating_sub(1)),
                        "right" => Some((selected + 1).min(count - 1)),
                        "home" => Some(0),
                        "end" => Some(count - 1),
                        _ => None,
                    };
                    if let Some(next) = next {
                        set_key_pressed(&entity, true, cx);
                        if next != selected
                            && let Some(select) = &select
                        {
                            select(next, window, cx);
                        }
                        cx.stop_propagation();
                    }
                })
                .into_any_element()
        } else {
            well.into_any_element()
        };
        hover_zone(well, &touch, f32::from(well_h) * 0.25, active)
    }
}

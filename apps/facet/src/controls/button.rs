//! The button: a cut plate (8 px chamfer at 30 px, growing with the
//! height), five intents, four sizes.
//!
//! One control, its whole life, and the bevel is the only thing that
//! changes meaning:
//!
//! - **rest**: flat tone, the lit/shaded bevel;
//! - **hover**: the plate steps up a tone and lifts toward the light, and the
//!   facet sweep — the bevel's own light — crosses the face once per
//!   hover-enter (it finishes crossing if the pointer leaves mid-way);
//! - **press**: the plate sinks a pixel and the bevel *inverts* (shaded
//!   top-left, lit bottom-right): a stone pressed into the surface;
//! - **focus** (keyboard only): the doubled periwinkle bevel;
//! - **busy**: a running hatch fills the face, the label keeps its width;
//! - **disabled**: .42 opacity, desaturated, deaf.

use super::glyph::{Glyph, glyph};
use super::kbd::key_badge;
use super::state::{
    Look, Touch, hover_zone, key_press, set_key_pressed, set_pressed, sweep_done, track, track_n,
};
use super::sweep::sweep;
use super::{muted, with_alpha};
use crate::Set;
use crate::icons::{Icon, IconSize, ui};
use crate::measure::{Control, Measure};
use crate::motion::{Spec, pulse, spec};
use crate::paint::{Chamfer, Edge, Bevel, Hatch, Plate, cut, hatch_fill, mix};
use crate::theme::ActiveFacet;
use crate::motion::EASE;
use crate::tokens::motion::EMPH;
use crate::tokens::{Face, Palette, TypeRole};
use gpui::{
    App, ClickEvent, ColorExt, ElementId, Hsla, InteractiveElement, IntoElement, KeyDownEvent, KeyUpEvent,
    MouseButton, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled,
    Window, div, layer, px,
};
use std::rc::Rc;

/// What a button means.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum Intent {
    /// `plate2`, `ink0`.
    #[default]
    Default,
    /// Mint: the one action that advances the flow.
    Primary,
    /// Periwinkle: compare, select, the other half of a pair.
    Edge,
    /// No plate until touched.
    Ghost,
    /// Coral-tinted: destructive.
    Danger,
}

pub(crate) type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// A button: `button("add", "Add package", &measure).icon(Icon::Package)`.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    intent: Intent,
    size: Control,
    icon: Option<Icon>,
    glyph: Option<Glyph>,
    busy: bool,
    disabled: bool,
    full: bool,
    key: Option<SharedString>,
    look: Look,
    measure: Measure,
    on_click: Option<Handler>,
}

/// A default-intent, medium button reading `label`, sized for `measure`.
#[must_use]
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, measure: &Measure) -> Button {
    Button {
        id: id.into(),
        label: Some(label.into()),
        intent: Intent::Default,
        size: Control::Medium,
        icon: None,
        glyph: None,
        busy: false,
        disabled: false,
        full: false,
        key: None,
        look: Look::LIVE,
        measure: *measure,
        on_click: None,
    }
}

impl Button {
    /// The intent.
    #[must_use]
    pub const fn intent(mut self, intent: Intent) -> Self {
        self.intent = intent;
        self
    }

    /// Mint: the one action that advances the flow.
    #[must_use]
    pub const fn primary(self) -> Self {
        self.intent(Intent::Primary)
    }

    /// Periwinkle.
    #[must_use]
    pub const fn edge(self) -> Self {
        self.intent(Intent::Edge)
    }

    /// No plate until touched.
    #[must_use]
    pub const fn ghost(self) -> Self {
        self.intent(Intent::Ghost)
    }

    /// Coral: destructive.
    #[must_use]
    pub const fn danger(self) -> Self {
        self.intent(Intent::Danger)
    }

    /// The height (`Small` 24, `Medium` 30, `Large` 38, `Huge` 46 at 100 %).
    #[must_use]
    pub const fn size(mut self, size: Control) -> Self {
        self.size = size;
        self
    }

    /// A leading icon.
    #[must_use]
    pub const fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// A leading control glyph (plus, cross, minus).
    #[must_use]
    pub const fn glyph(mut self, glyph: Glyph) -> Self {
        self.glyph = Some(glyph);
        self
    }

    /// Only the icon (the label becomes the accessible name).
    #[must_use]
    pub fn icon_only(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self.label = None;
        self
    }

    /// Busy: a running hatch over the face, the label's width kept, deaf.
    #[must_use]
    pub const fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Disabled: .42 opacity, desaturated, deaf.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Stretches to the container's width.
    #[must_use]
    pub const fn full(mut self) -> Self {
        self.full = true;
        self
    }

    /// The key this button answers to; its hot cap rises while ⌘ is held.
    #[must_use]
    pub fn key(mut self, key: impl Into<SharedString>) -> Self {
        self.key = Some(key.into());
        self
    }

    /// Pins an appearance on top of the live state.
    #[must_use]
    pub const fn look(mut self, look: Look) -> Self {
        self.look = look;
        self
    }

    /// Click, Enter and Space.
    #[must_use]
    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

/// A board button role: Geist 550 at the size's px.
const fn role(size: f32) -> TypeRole {
    TypeRole {
        face: Face::Ui,
        weight: 550.0,
        size,
        line: size * 1.3,
        tracking: 0.0,
        italic: false,
    }
}

/// Per-size metrics at 100 %: type, icon, horizontal padding (as a fraction
/// of the height), and the chamfer (8 px at 30 px).
fn metrics(size: Control) -> (TypeRole, f32) {
    match size {
        Control::Small => (role(11.5), 12.0),
        Control::Medium => (role(12.5), 14.0),
        Control::Large => (role(13.5), 16.0),
        Control::Huge => (role(15.0), 18.0),
    }
}

/// Everything an intent paints with, at rest and hovered.
struct Tones {
    fill: Hsla,
    hover: Hsla,
    ink: Hsla,
    hover_ink: Hsla,
    edge: Edge,
    light: f32,
}


fn tones(intent: Intent, palette: &Palette) -> Tones {
    let rest = Edge::of(Bevel::Rest, palette);
    let hi: Hsla = palette.bevel_hi.into();
    let ink0: Hsla = palette.ink0.into();
    let voiced = |fill: Hsla, ink: Hsla| Tones {
        fill,
        hover: mix(fill, ink0, 0.14),
        ink,
        hover_ink: ink,
        edge: Edge {
            hi: with_alpha(hi, 0.62),
            ..rest
        },
        light: 0.35,
    };
    match intent {
        Intent::Default => Tones {
            fill: palette.plate2.into(),
            hover: palette.plate3.into(),
            ink: ink0,
            hover_ink: ink0,
            edge: rest,
            light: 0.1,
        },
        Intent::Primary => voiced(palette.mint.base.into(), palette.mint_ink.into()),
        Intent::Edge => voiced(palette.peri.base.into(), palette.table.into()),
        Intent::Ghost => {
            let clear = with_alpha(palette.plate2.into(), 0.0);
            Tones {
                fill: clear,
                hover: palette.line1.into(),
                ink: palette.ink2.into(),
                hover_ink: ink0,
                edge: Edge {
                    hi: with_alpha(hi, 0.0),
                    lo: with_alpha(palette.bevel_lo.into(), 0.0),
                    ..rest
                },
                light: 0.0,
            }
        }
        Intent::Danger => {
            let coral: Hsla = palette.coral.base.into();
            Tones {
                fill: palette.coral.soft.into(),
                hover: with_alpha(coral, 0.24),
                ink: mix(coral, ink0, 0.55),
                hover_ink: mix(coral, ink0, 0.7),
                edge: Edge {
                    hi: palette.coral.line.into(),
                    ..rest
                },
                light: 0.1,
            }
        }
    }
}

/// The pressed edge: the same bevel with its light and shade swapped.
pub(crate) fn sunk(edge: Edge) -> Edge {
    Edge {
        hi: edge.lo,
        lo: edge.hi,
        ..edge
    }
}


/// Wires pointer, keyboard and focus onto a control's plate: press on
/// pointer down, release on up anywhere, activate on click and on
/// Enter (down) / Space (up), flashing the press while the key is held.
pub(crate) fn wire<E>(element: E, touch: &Touch, activate: Option<Handler>) -> E
where
    E: StatefulInteractiveElement + InteractiveElement + Styled,
{
    let entity = touch.entity.clone();
    let mut element = element
        .track_focus(&touch.focus)
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
            let activate = activate.clone();
            move |event: &KeyUpEvent, window: &mut Window, cx: &mut App| {
                let key = event.keystroke.key.as_str();
                if key == "space" || key == "enter" {
                    let was = entity.read(cx).key_pressed;
                    set_key_pressed(&entity, false, cx);
                    if was
                        && key == "space"
                        && let Some(activate) = &activate
                    {
                        activate(window, cx);
                    }
                }
            }
        });
    element = element.on_key_down({
        let activate = activate.clone();
        move |event: &KeyDownEvent, window: &mut Window, cx: &mut App| {
            let key = event.keystroke.key.as_str();
            if (key == "space" || key == "enter") && !event.keystroke.modifiers.modified() {
                key_press(&entity, window, cx);
                if key == "enter"
                    && !event.is_held
                    && let Some(activate) = &activate
                {
                    activate(window, cx);
                }
                cx.stop_propagation();
            }
        }
    });
    if let Some(activate) = activate {
        element = element.on_click(move |_event: &ClickEvent, window: &mut Window, cx: &mut App| {
            // A keyboard "click" is handled by the key listeners above.
            if !window.last_input_was_keyboard() {
                activate(window, cx);
            }
        });
    }
    element
}

impl RenderOnce for Button {
    #[allow(clippy::too_many_lines)]
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let active = !self.disabled && !self.busy;
        let touch = Touch::read(&self.id, self.look, active, window, cx);
        let motion = touch.motion.clone();
        let id = self.id.clone();

        let height = measure.control(self.size);
        let (role, icon_px) = metrics(self.size);
        let h = f32::from(height);
        let chamfer = h * 8.0 / 30.0;
        let tones = tones(self.intent, palette);

        // Tone: hover steps the plate up; press keeps the hover tone.
        let hover_t = motion.animate(
            track(&id, "hover"),
            if touch.hovered { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let press_t = motion.animate(
            track(&id, "press"),
            if touch.pressed { 1.0 } else { 0.0 },
            spec::PRESS,
            window,
            cx,
        );
        let focus_t = motion.animate(
            track(&id, "focus"),
            if touch.focused { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        // Lift: towards the light on hover (-1 px), into the surface on
        // press (+1 px). The press is crisp; the release springs back.
        let lift_target = if touch.pressed {
            -1.0 / 3.0
        } else if touch.hovered && self.intent != Intent::Ghost {
            1.0 / 3.0
        } else {
            0.0
        };
        let lift = motion.animate(
            track(&id, "lift"),
            lift_target,
            if touch.pressed { spec::PRESS } else { spec::LIFT },
            window,
            cx,
        );

        // The sweep: one crossing per hover-enter, numbered so each starts
        // fresh; a leave lets it finish instead of reversing, and an enter
        // mid-crossing lets it finish instead of cutting it off. The one
        // before it has ended, so forgetting it loses nothing.
        let sweep_t = if touch.sweeps > 0 && tones.light > 0.0 && active {
            motion.replay(track_n(&id, "sweep", touch.sweeps.wrapping_sub(1) as usize));
            let t = motion.animate_from(
                track_n(&id, "sweep", touch.sweeps as usize),
                0.0,
                1.0,
                Spec::tween(EMPH, EASE),
                window,
                cx,
            );
            if t >= 1.0 && touch.sweeping {
                sweep_done(&touch.entity, window, cx);
            }
            t
        } else {
            if touch.sweeping {
                sweep_done(&touch.entity, window, cx);
            }
            1.0
        };

        let mut fill = mix(tones.fill, tones.hover, hover_t);
        let mut ink = mix(tones.ink, tones.hover_ink, hover_t);
        if self.disabled {
            fill = muted(fill);
            ink = muted(ink);
        }
        let edge = tones.edge;
        let edge = edge.mix(sunk(edge), press_t);
        let edge = edge.mix(Edge::of(Bevel::Focus, palette), focus_t);

        let gap = height * (7.0 / 30.0);
        let mut content = div()
            .flex()
            .items_center()
            .justify_center()
            .gap(gap)
            // Busy hides the label but keeps its width.
            .opacity(if self.busy { 0.0 } else { 1.0 });
        if let Some(icon) = self.icon {
            content = content.child(ui(icon, IconSize::S14, ink).size(measure.icon(icon_px)));
        }
        if let Some(mark) = self.glyph {
            content = content.child(glyph(mark, measure.icon(icon_px), ink));
        }
        if let Some(label) = self.label.clone() {
            content = content.child(
                div()
                    .set(role, &measure)
                    .text_color(ink)
                    .whitespace_nowrap()
                    .child(label),
            );
        }

        let pad = if self.label.is_some() {
            height * 0.43
        } else {
            px(0.0)
        };
        let mut plate = cut()
            .chamfer(Chamfer::Px(chamfer))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(fill)
            .lift(lift)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(height)
            .min_w(height)
            .px(pad)
            .child(content);
        if self.full {
            plate = plate.w_full();
        }
        if self.busy {
            let phase = pulse::lease(window, cx).phase(0.5);
            let hatch_ink = if matches!(self.intent, Intent::Primary | Intent::Edge) {
                ink
            } else {
                palette.ink2.into()
            };
            plate = plate.child(
                div()
                    .absolute()
                    .top(height * 0.3)
                    .bottom(height * 0.3)
                    .left(height * 0.45)
                    .right(height * 0.45)
                    .child(
                        hatch_fill(Hatch::pending().phase(phase), hatch_ink.opacity(0.9))
                            .size_full(),
                    ),
            );
        }
        if sweep_t > 0.0 && sweep_t < 1.0 {
            let light = with_alpha(palette.bevel_hi.into(), tones.light);
            plate = plate.child(
                div()
                    .absolute()
                    .inset_0()
                    .child(sweep(sweep_t, chamfer, light).size_full()),
            );
        }
        if let Some(key) = self.key.as_ref().filter(|_| active) {
            plate = plate.child(key_badge(key, &motion, &id, &measure));
        }

        let plate = plate
            .id(id)
            .opacity(if self.disabled { 0.42 } else { 1.0 });
        let plate = if active {
            wire(plate, &touch, self.on_click).into_any_element()
        } else {
            plate.into_any_element()
        };
        // Pressed, the stone gives a little (`scale(.97)` on the boards).
        let plate = layer(plate).scale(1.0 - 0.03 * press_t);
        hover_zone(plate, &touch, chamfer, active)
    }
}

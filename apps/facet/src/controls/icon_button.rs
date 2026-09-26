//! The icon button: one icon on a small square cut plate that is only there
//! when touched. At rest it is the icon alone (`ink2`); hover raises a
//! plate under it with the bevel; press sinks it (the bevel inverts);
//! **on** tints it mint (the shelf is open, the filter is set); keyboard
//! focus doubles the bevel in periwinkle.

use super::button::{Handler, sunk, wire};
use super::{clear, muted};
use super::kbd::key_badge;
use super::state::{Look, Touch, hover_zone, track};
use crate::icons::{Icon, IconSize, ui};
use crate::measure::Measure;
use crate::motion::{offset, spec};
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use gpui::{
    ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, px,
};
use std::rc::Rc;

/// The plate size.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub enum IconButtonSize {
    /// 22 px, 14 px icon.
    Small,
    /// 28 px, 16 px icon.
    #[default]
    Medium,
    /// 32 px, 18 px icon (the language dock).
    Large,
}

impl IconButtonSize {
    const fn metrics(self) -> (f32, f32) {
        match self {
            Self::Small => (22.0, 14.0),
            Self::Medium => (28.0, 16.0),
            Self::Large => (32.0, 18.0),
        }
    }
}

/// An icon button: `icon_button("shelf", Icon::SideL, "Toggle shelf", &measure)`.
#[derive(IntoElement)]
pub struct IconButton {
    id: ElementId,
    icon: Icon,
    label: SharedString,
    size: IconButtonSize,
    on: bool,
    disabled: bool,
    key: Option<SharedString>,
    look: Look,
    measure: Measure,
    on_click: Option<Handler>,
}

/// A medium, off icon button showing `icon`, named `label` (its tooltip and
/// accessible name), sized for `measure`.
#[must_use]
pub fn icon_button(
    id: impl Into<ElementId>,
    icon: Icon,
    label: impl Into<SharedString>,
    measure: &Measure,
) -> IconButton {
    IconButton {
        id: id.into(),
        icon,
        label: label.into(),
        size: IconButtonSize::Medium,
        on: false,
        disabled: false,
        key: None,
        look: Look::LIVE,
        measure: *measure,
        on_click: None,
    }
}

impl IconButton {
    /// The plate size.
    #[must_use]
    pub const fn size(mut self, size: IconButtonSize) -> Self {
        self.size = size;
        self
    }

    /// The small size.
    #[must_use]
    pub const fn small(self) -> Self {
        self.size(IconButtonSize::Small)
    }

    /// Mint: the thing it toggles is on.
    #[must_use]
    pub const fn on(mut self, on: bool) -> Self {
        self.on = on;
        self
    }

    /// Disabled: .42 opacity, deaf.
    #[must_use]
    pub const fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// The key this button answers to (its cap rises while ⌘ is held).
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

    /// The accessible name and tooltip text.
    #[must_use]
    pub fn label(&self) -> &SharedString {
        &self.label
    }

    /// Click, Enter and Space.
    #[must_use]
    pub fn on_click(mut self, handler: impl Fn(&mut Window, &mut gpui::App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let active = !self.disabled;
        let touch = Touch::read(&self.id, self.look, active, window, cx);
        let motion = touch.motion.clone();
        let id = self.id.clone();
        let (side_base, icon_base) = self.size.metrics();
        let side = measure.icon(side_base);
        let chamfer = f32::from(side) * 0.25;

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
        let on_t = motion.animate(
            track(&id, "on"),
            if self.on { 1.0 } else { 0.0 },
            spec::HOVER,
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

        // The plate fades in under the pointer (and stays, mint, when on).
        let rest = Edge::of(Bevel::Rest, palette);
        let hidden = Edge {
            hi: clear(rest.hi),
            lo: clear(rest.lo),
            ..rest
        };
        let lit = Edge {
            hi: palette.mint.line.into(),
            lo: clear(rest.lo),
            ..rest
        };
        let plate_fill: Hsla = palette.plate3.into();
        let mint_fill: Hsla = palette.mint.soft.into();
        let fill = mix(
            mix(clear(plate_fill), plate_fill, hover_t),
            mix(mint_fill, palette.mint.line.into(), hover_t * 0.5),
            on_t,
        );
        let edge = hidden.mix(rest, hover_t).mix(lit, on_t);
        let edge = edge.mix(sunk(edge), press_t);
        let edge = edge.mix(Edge::of(Bevel::Focus, palette), focus_t);
        let mut ink = mix(
            mix(palette.ink2.into(), palette.ink0.into(), hover_t),
            palette.mint.base.into(),
            on_t,
        );
        if self.disabled {
            ink = muted(ink);
        }

        let glyph = offset(ui(self.icon, IconSize::S16, ink).size(measure.icon(icon_base)))
            .y(px(press_t));
        let mut plate = cut()
            .chamfer(Chamfer::Px(chamfer))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(fill)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(side)
            .child(glyph);
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
        hover_zone(plate, &touch, chamfer, active)
    }
}

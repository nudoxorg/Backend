//! The module rail: every module of a package as a small chip with its
//! count, the open one lit, and a chip at the end that closes the view
//! (`fold`, or `back to tokio`). It is the territory's stand-in while a
//! module is open: one click moves to another module without folding first.

use super::text::{key, one};
use crate::controls::button::wire;
use crate::controls::state::{Touch, hover_zone, track};
use crate::marks::badges::{Glyph, glyph};
use crate::measure::{Measure, Space};
use crate::motion::spec;
use crate::paint::{Bevel, Chamfer, Edge, Plate, cut, mix};
use crate::theme::ActiveFacet;
use crate::tokens::{TypeRole, ty};
use gpui::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    Styled, Window, div, px,
};
use std::rc::Rc;

const NAME: TypeRole = TypeRole {
    weight: 500.0,
    size: 12.0,
    line: 16.0,
    ..ty::MONO_SMALL
};
const COUNT: TypeRole = TypeRole {
    size: 11.0,
    line: 16.0,
    ..ty::MONO_SMALL
};

/// What the rail does with a click.
pub type Pick = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// What the closing chip does.
pub type Close = Rc<dyn Fn(&mut Window, &mut App)>;

/// The rail (see [`rail`]).
#[derive(IntoElement)]
pub struct Rail {
    id: ElementId,
    chips: Vec<(SharedString, usize)>,
    current: Option<usize>,
    close: SharedString,
    measure: Measure,
    on_pick: Option<Pick>,
    on_close: Option<Close>,
}

/// A rail of `chips` (`(module, names)`), `close` the closing chip's words.
#[must_use]
pub fn rail(
    id: impl Into<ElementId>,
    chips: Vec<(SharedString, usize)>,
    close: impl Into<SharedString>,
    measure: &Measure,
) -> Rail {
    Rail {
        id: id.into(),
        chips,
        current: None,
        close: close.into(),
        measure: *measure,
        on_pick: None,
        on_close: None,
    }
}

impl Rail {
    /// The lit chip.
    #[must_use]
    pub const fn current(mut self, current: Option<usize>) -> Self {
        self.current = current;
        self
    }

    /// Called with the chip clicked.
    #[must_use]
    pub fn on_pick(mut self, pick: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_pick = Some(Rc::new(pick));
        self
    }

    /// Called when the closing chip is clicked.
    #[must_use]
    pub fn on_close(mut self, close: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(close));
        self
    }
}

#[derive(IntoElement)]
struct Chip {
    id: ElementId,
    name: SharedString,
    count: Option<usize>,
    on: bool,
    close: bool,
    measure: Measure,
    act: Option<Rc<dyn Fn(&mut Window, &mut App)>>,
}

impl RenderOnce for Chip {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let palette = cx.palette();
        let measure = self.measure;
        let scale = measure.scale();
        let touch = Touch::read(&self.id, crate::controls::Look::LIVE, true, window, cx);
        let motion = touch.motion.clone();
        let hover = motion.animate(
            track(&self.id, "hover"),
            if touch.hovered { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let on = motion.animate(
            track(&self.id, "on"),
            if self.on { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let focus = motion.animate(
            track(&self.id, "focus"),
            if touch.focused { 1.0 } else { 0.0 },
            spec::HOVER,
            window,
            cx,
        );
        let mut rest = Edge::of(Bevel::Rest, palette);
        rest.hi = palette.line3.into();
        rest.lo = palette.line2.into();
        let edge = rest
            .mix(Edge::of(Bevel::Peri, palette), hover.max(on))
            .mix(Edge::of(Bevel::Focus, palette), focus);
        let ink = mix(palette.ink2.into(), palette.ink0.into(), hover.max(on));
        let mut body = cut()
            .chamfer(Chamfer::Px(4.0 * scale))
            .edge(edge)
            .plate(Plate::Flat)
            .fill(mix(
                palette.plate.into(),
                palette.plate2.into(),
                on.max(hover * 0.6),
            ))
            .flex()
            .flex_none()
            .items_center()
            .gap(measure.space(Space::Snug))
            .h(px(26.0 * scale))
            .px(measure.space(Space::Base));
        if self.close {
            body = body.child(glyph(Glyph::Error, 12.0 * scale, ink));
        }
        body = body.child(one(
            key(&self.id, "name"),
            self.name.clone(),
            NAME,
            ink,
            &measure,
        ));
        if let Some(count) = self.count {
            body = body.child(one(
                key(&self.id, "count"),
                count.to_string(),
                COUNT,
                palette.ink3,
                &measure,
            ));
        }
        let body = wire(body.id(self.id.clone()), &touch, self.act.clone());
        hover_zone(body, &touch, 4.0 * scale, true)
    }
}

impl RenderOnce for Rail {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let measure = self.measure;
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(measure.space(Space::Tight));
        for (index, (name, count)) in self.chips.iter().enumerate() {
            let pick = self.on_pick.clone();
            row = row.child(Chip {
                id: key(&self.id, format!("chip-{index}")),
                name: name.clone(),
                count: Some(*count),
                on: self.current == Some(index),
                close: false,
                measure,
                act: pick.map(|pick| {
                    Rc::new(move |window: &mut Window, cx: &mut App| pick(index, window, cx))
                        as Rc<dyn Fn(&mut Window, &mut App)>
                }),
            });
        }
        let close = self.on_close.clone();
        row.child(div().flex_1()).child(Chip {
            id: key(&self.id, "close"),
            name: self.close.clone(),
            count: None,
            on: false,
            close: true,
            measure,
            act: close,
        })
    }
}

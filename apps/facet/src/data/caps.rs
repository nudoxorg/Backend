//! The caps row: what you can do with a type (clone it, compare it, hash
//! it, print it…) as a row of capability marks, one element.
//!
//! Quiet at rest: implemented capabilities in ink3, missing ones as closed
//! doors (ink4), ones that arrive through a blanket or auto impl dashed.
//! Resting on one lifts it and takes the contracts hue; its neighbours lift
//! a little (the same wave as the comb, two tracks for the whole row). Each
//! cap is a door: its tip names the trait(s) and where they come from.

use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use crate::icons::{Cap, Stroke, variant_path};
use crate::measure::Measure;
use crate::theme::ActiveFacet;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, Style, TransformationMatrix, Window,
    point, px, size,
};
use std::rc::Rc;

/// How a capability is present.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Has {
    /// Written or derived.
    On,
    /// Arrives through a blanket or auto impl.
    Via,
    /// Not implemented: a closed door.
    Off,
}

/// A caps row. Build with [`caps`].
pub struct Caps {
    id: ElementId,
    caps: Rc<[(Cap, Has)]>,
    measure: Measure,
    door: Option<Door>,
    rest: Option<usize>,
}

/// A row of `caps` at `measure`'s scale.
#[must_use]
pub fn caps(id: impl Into<ElementId>, caps: impl Into<Rc<[(Cap, Has)]>>, measure: &Measure) -> Caps {
    Caps {
        id: id.into(),
        caps: caps.into(),
        measure: *measure,
        door: None,
        rest: None,
    }
}

impl Caps {
    /// Caps open through `door` (part = cap index).
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a cap as rested (scenes).
    #[must_use]
    pub const fn rest(mut self, cap: Option<usize>) -> Self {
        self.rest = cap;
        self
    }
}

impl IntoElement for Caps {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

const CELL: f32 = 30.0;
const ICON: f32 = 18.0;
const GAP: f32 = 2.0;

/// `(lift px, swell)` at a cap distance from the rested one.
#[must_use]
pub fn cap_wave(distance: f32) -> (f32, f32) {
    let d = distance.abs();
    if d <= 1.0 {
        (3.0 - 1.5 * d, 1.15 - 0.15 * d)
    } else if d <= 2.0 {
        (1.5 * (2.0 - d), 1.0)
    } else {
        (0.0, 1.0)
    }
}

#[doc(hidden)]
pub struct CapsLayout {
    live: Entity<Live>,
    keys: AnyElement,
}

impl Element for Caps {
    type RequestLayoutState = CapsLayout;
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
    ) -> (LayoutId, CapsLayout) {
        let live = live::live(window, cx);
        let (keys, kid) = live::keys(&live, window, cx);
        let s = self.measure.scale();
        #[allow(clippy::cast_precision_loss)]
        let n = self.caps.len() as f32;
        let mut style = Style::default();
        style.size.width = px(((CELL + GAP) * n - GAP).max(0.0) * s).into();
        style.size.height = px(CELL * s).into();
        style.flex_shrink = 0.0;
        (window.request_layout(style, [kid], cx), CapsLayout { live, keys })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut CapsLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        live::prepaint(bounds, &mut layout.keys, window, cx)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut CapsLayout,
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let palette = cx.palette();
        let s = self.measure.scale();
        let (x0, y0) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let live = layout.live.clone();
        let active = live.read(cx).hover.or(self.rest).or(live::walking(&live, window, cx));
        let pitch = (CELL + GAP) * s;
        #[allow(clippy::cast_precision_loss)]
        let (at, _, strength) = live::wave(&self.id, &live, active.map(|i| (i as f32 * pitch, 0.0)), window, cx);
        let centre = at / pitch;
        for (i, (cap, has)) in self.caps.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let d = (i as f32 - centre).abs();
            let (lift, swell) = cap_wave(d);
            let (lift, swell) = (lift * strength * s, 1.0 + (swell - 1.0) * strength);
            let lit = (1.0 - d).clamp(0.0, 1.0) * strength;
            let rest: Hsla = match has {
                Has::On | Has::Via => palette.ink3.into(),
                Has::Off => palette.ink4.into(),
            };
            let ink = crate::paint::mix(rest, palette.f_con.hue.into(), lit);
            let e = ICON * s * swell;
            #[allow(clippy::cast_precision_loss)]
            let cx0 = x0 + i as f32 * pitch + CELL * s * 0.5;
            let at = Bounds::new(
                point(px(cx0 - e * 0.5), px(y0 + CELL * s * 0.5 - e * 0.5 - lift)),
                size(px(e), px(e)),
            );
            let stroke = Stroke {
                width: 1.5,
                facet: None,
                dashed: *has == Has::Via,
            };
            window
                .paint_svg(at, variant_path(cap.path(), stroke), None, TransformationMatrix::unit(), ink, cx)
                .ok();
        }
        let n = self.caps.len();
        live::paint(
            Hooks {
                mark: self.id.clone(),
                live,
                door: self.door.clone(),
                side: Side::Above,
                count: n,
                hit: Rc::new(move |p: Point<Pixels>| {
                    let along = f32::from(p.x) - x0;
                    if along < 0.0 {
                        return None;
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    let i = (along / pitch) as usize;
                    (i < n).then_some(i)
                }),
                anchor: Rc::new(move |i| {
                    #[allow(clippy::cast_precision_loss)]
                    (i < n).then(|| Bounds::new(point(px(x0 + i as f32 * pitch), px(y0)), size(px(CELL * s), px(CELL * s))))
                }),
                step: Rc::new(live::linear),
            },
            &mut layout.keys,
            hitbox,
            window,
            cx,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::cap_wave;

    #[test]
    fn the_rested_cap_lifts_most_and_neighbours_less() {
        assert!((cap_wave(0.0).0 - 3.0).abs() < 1e-6 && (cap_wave(0.0).1 - 1.15).abs() < 1e-6);
        assert!((cap_wave(1.0).0 - 1.5).abs() < 1e-6 && (cap_wave(1.0).1 - 1.0).abs() < 1e-6);
        assert!(cap_wave(2.0).0.abs() < 1e-6);
        assert!((cap_wave(0.999).0 - cap_wave(1.001).0).abs() < 0.01);
    }
}

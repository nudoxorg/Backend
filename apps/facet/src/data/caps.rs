//! The caps row: the compact, glyph-only form of a type's capabilities,
//! for rooms where words do not fit (a peek card, a row, a lens).
//!
//! There is one capability authority: [`semantics::caps`](crate::semantics::caps)
//! turns a type's derives and impls into [`Cap`]s — a plain word, the
//! trait, and how it arrives. The anatomy's `can` line spells them in
//! words; this row draws the same list as glyphs, in the same grammar:
//! hollow when derived (ink3), solid when written by hand (ink1), dashed
//! when given through another capability (ink4). Traits with no glyph are
//! left to the words.
//!
//! Missing capabilities — closed doors — are a question only ⌥ asks: at
//! rest the row shows what the type can do and nothing else. A glyph a
//! present one implies (`Copy` covers `Clone`, `Ord` covers `Eq`) is never
//! missing.
//!
//! Resting on a glyph lifts it and takes the contracts hue; its neighbours
//! lift a little (the comb's wave, two tracks for the whole row). Each glyph
//! is a door: its tip names the trait(s) and how they arrive.

use super::door::{Door, Side};
use super::live::{self, Hooks, Live};
use crate::icons::{Cap as Glyph, Stroke, variant_path};
use crate::measure::Measure;
use crate::semantics::caps::{Arrives, Cap};
use crate::theme::ActiveFacet;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, Entity, GlobalElementId, Hitbox, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, SharedString, Style, TransformationMatrix,
    Window, point, px, size,
};
use std::rc::Rc;

/// The glyph a trait draws as (`None`: words only).
#[must_use]
pub fn glyph(trait_name: &str) -> Option<Glyph> {
    Some(match trait_name {
        "Clone" => Glyph::Clone,
        "Copy" => Glyph::Copy,
        "PartialEq" | "Eq" => Glyph::Eq,
        "PartialOrd" | "Ord" => Glyph::Ord,
        "Hash" => Glyph::Hash,
        "Debug" => Glyph::Debug,
        "Display" => Glyph::Display,
        "From" | "Into" | "TryFrom" | "TryInto" | "ToString" | "FromStr" => Glyph::Convert,
        "Send" | "Sync" => Glyph::Thread,
        "Default" => Glyph::Default,
        "Serialize" | "Deserialize" => Glyph::Serde,
        "Iterator" | "IntoIterator" => Glyph::Iter,
        "Deref" | "DerefMut" => Glyph::Deref,
        "Error" => Glyph::Error,
        _ => return None,
    })
}

/// The glyphs a present glyph implies (never shown missing).
const fn implies(glyph: Glyph) -> &'static [Glyph] {
    match glyph {
        Glyph::Copy => &[Glyph::Clone],
        Glyph::Ord => &[Glyph::Eq],
        _ => &[],
    }
}

/// How one glyph is drawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Has {
    /// Present: how it arrives (the strongest of its traits: written, then
    /// derived, then via).
    Is(Arrives),
    /// Missing: a closed door (only while ⌥ is held).
    Missing,
}

/// One glyph of the row, with the traits it stands for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Slot {
    /// The glyph.
    pub glyph: Glyph,
    /// How it is drawn.
    pub has: Has,
    /// Its tip: `PartialEq, Eq — derived`.
    pub says: SharedString,
}

const fn strength(arrives: &Arrives) -> u8 {
    match arrives {
        Arrives::Written => 2,
        Arrives::Derived => 1,
        Arrives::Via(_) => 0,
    }
}

/// The row's glyphs for `caps`, in board order; with `xray`, the missing
/// ones too.
#[must_use]
pub fn slots(caps: &[Cap], xray: bool) -> Vec<Slot> {
    let mut present: Vec<(Glyph, Arrives, Vec<String>)> = Vec::new();
    for cap in caps {
        let Some(g) = glyph(&cap.trait_name) else { continue };
        match present.iter_mut().find(|(p, _, _)| *p == g) {
            Some((_, arrives, traits)) => {
                if strength(&cap.arrives) > strength(arrives) {
                    *arrives = cap.arrives.clone();
                }
                traits.push(cap.trait_name.to_string());
            }
            None => present.push((g, cap.arrives.clone(), vec![cap.trait_name.to_string()])),
        }
    }
    let implied = |g: Glyph| present.iter().any(|(p, _, _)| implies(*p).contains(&g));
    Glyph::ALL
        .iter()
        .filter_map(|&g| match present.iter().find(|(p, _, _)| *p == g) {
            Some((_, arrives, traits)) => Some(Slot {
                glyph: g,
                says: SharedString::from(format!("{} — {}", traits.join(", "), arrives.text())),
                has: Has::Is(arrives.clone()),
            }),
            None if xray && !implied(g) => Some(Slot {
                glyph: g,
                has: Has::Missing,
                says: SharedString::from(format!("not {}", g.traits())),
            }),
            None => None,
        })
        .collect()
}

/// A caps row. Build with [`caps`].
pub struct Caps {
    id: ElementId,
    slots: Rc<[Slot]>,
    measure: Measure,
    door: Option<Door>,
    rest: Option<usize>,
}

/// The glyph row for `caps` (from `semantics::caps`) at `measure`'s scale;
/// `xray` (⌥ held) adds the missing ones as closed doors.
#[must_use]
pub fn caps(id: impl Into<ElementId>, caps: &[Cap], xray: bool, measure: &Measure) -> Caps {
    Caps {
        id: id.into(),
        slots: slots(caps, xray).into(),
        measure: *measure,
        door: None,
        rest: None,
    }
}

impl Caps {
    /// Glyphs open through `door` (part = glyph index; [`Caps::says`] is
    /// what each tip says).
    #[must_use]
    pub fn door(mut self, door: Door) -> Self {
        self.door = Some(door);
        self
    }

    /// Shows a glyph as rested (scenes).
    #[must_use]
    pub const fn rest(mut self, glyph: Option<usize>) -> Self {
        self.rest = glyph;
        self
    }

    /// What each glyph's tip says, by index.
    #[must_use]
    pub fn says(&self) -> Vec<SharedString> {
        self.slots.iter().map(|s| s.says.clone()).collect()
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

/// `(lift px, swell)` at a glyph distance from the rested one.
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
        let n = self.slots.len() as f32;
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
        for (i, slot) in self.slots.iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let d = (i as f32 - centre).abs();
            let (lift, swell) = cap_wave(d);
            let (lift, swell) = (lift * strength * s, 1.0 + (swell - 1.0) * strength);
            let lit = (1.0 - d).clamp(0.0, 1.0) * strength;
            // The `can` line's grammar: hollow derived, solid written,
            // dashed via; a closed door in the quietest ink.
            let (rest, facet, dashed): (Hsla, f32, bool) = match &slot.has {
                Has::Is(Arrives::Written) => (palette.ink1.into(), 0.55, false),
                Has::Is(Arrives::Derived) => (palette.ink3.into(), 0.0, false),
                Has::Is(Arrives::Via(_)) => (palette.ink4.into(), 0.0, true),
                Has::Missing => (palette.ink4.into(), 0.0, false),
            };
            let ink = crate::paint::mix(rest, palette.f_con.hue.into(), lit);
            let e = ICON * s * swell;
            #[allow(clippy::cast_precision_loss)]
            let cx0 = x0 + i as f32 * pitch + CELL * s * 0.5;
            let at = Bounds::new(
                point(px(cx0 - e * 0.5), px(y0 + CELL * s * 0.5 - e * 0.5 - lift)),
                size(px(e), px(e)),
            );
            let stroke = Stroke { width: 1.5, facet: Some(facet), dashed };
            window
                .paint_svg(at, variant_path(slot.glyph.path(), stroke), None, TransformationMatrix::unit(), ink, cx)
                .ok();
        }
        let n = self.slots.len();
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
    use super::{Has, cap_wave, slots};
    use crate::icons::Cap as Glyph;
    use crate::semantics::caps::{Arrives, caps};
    use gpui::SharedString;

    #[test]
    fn the_rested_cap_lifts_most_and_neighbours_less() {
        assert!((cap_wave(0.0).0 - 3.0).abs() < 1e-6 && (cap_wave(0.0).1 - 1.15).abs() < 1e-6);
        assert!((cap_wave(1.0).0 - 1.5).abs() < 1e-6 && (cap_wave(1.0).1 - 1.0).abs() < 1e-6);
        assert!(cap_wave(2.0).0.abs() < 1e-6);
        assert!((cap_wave(0.999).0 - cap_wave(1.001).0).abs() < 0.01);
    }

    /// RelationLabel's capabilities, from the one authority: derives
    /// `Clone, Copy, Debug, Eq, PartialEq, Hash`, a hand-written `Display`
    /// (which gives `ToString`).
    fn relation_label() -> Vec<crate::semantics::caps::Cap> {
        caps(&["Clone", "Copy", "Debug", "Eq", "PartialEq", "Hash"], &[(SharedString::new_static("Display"), None)], &[] as &[&str])
    }

    #[test]
    fn glyphs_say_how_each_capability_arrives() {
        let row = slots(&relation_label(), false);
        let drawn: Vec<(Glyph, &Has, &str)> = row.iter().map(|s| (s.glyph, &s.has, s.says.as_ref())).collect();
        assert_eq!(
            drawn,
            [
                (Glyph::Copy, &Has::Is(Arrives::Derived), "Copy — derived"),
                (Glyph::Eq, &Has::Is(Arrives::Derived), "Eq — derived"),
                (Glyph::Hash, &Has::Is(Arrives::Derived), "Hash — derived"),
                (Glyph::Debug, &Has::Is(Arrives::Derived), "Debug — derived"),
                (Glyph::Display, &Has::Is(Arrives::Written), "Display — written"),
                // `ToString` arrives through `Display`: dashed.
                (Glyph::Convert, &Has::Is(Arrives::Via(SharedString::new_static("Display"))), "ToString — via Display"),
            ]
        );
    }

    #[test]
    fn missing_capabilities_are_closed_doors_only_under_the_option_key() {
        let at_rest = slots(&relation_label(), false);
        assert!(at_rest.iter().all(|s| s.has != Has::Missing), "{at_rest:?}");
        let xray = slots(&relation_label(), true);
        let missing: Vec<Glyph> = xray.iter().filter(|s| s.has == Has::Missing).map(|s| s.glyph).collect();
        // `Clone` is implied by `Copy`: never missing.
        assert_eq!(missing, [Glyph::Ord, Glyph::Thread, Glyph::Default, Glyph::Serde, Glyph::Iter, Glyph::Deref, Glyph::Error]);
    }
}

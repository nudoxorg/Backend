//! A module opens by carrying its shingles to its cards: each shingle of the
//! region flies on the CARRY spring (`facet::motion::Carry`) to the mark of
//! the card that stands for the same name, and the card's face fades in as
//! its shingle arrives. The territory stays what it is (one shingle per
//! name); the cards are what a name is once you are in the module.
//!
//! The host keeps the [`Carry`] (started at the click), hands the stones the
//! map reported ([`Carrying`]) and the cards' [`Marks`] to a [`Flight`], and
//! gives the cards the progress ([`arrival`]) so their marks stay hidden
//! until the shingle is over them.

use super::text::key;
use crate::motion::{Carry, band, now, reduced, request_frame};
use crate::paint::geom::{Fill, Poly};
use crate::probe;
use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Position, Style, Window, point, px, size,
};
use std::cell::RefCell;
use std::rc::Rc;

/// One shingle in the air: where it started (window coordinates) and its ink.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stone {
    /// Where the shingle was.
    pub from: Bounds<Pixels>,
    /// Its colour.
    pub ink: Hsla,
}

/// What a click on a region reports: the module, and its shingles as they
/// were on screen, in the module's order.
#[derive(Clone, Debug, PartialEq)]
pub struct Carrying {
    /// The module opened (its index in the map).
    pub module: usize,
    /// Its shingles, one per name.
    pub stones: Vec<Stone>,
}

/// Where the cards' marks are this frame (window coordinates, by card index):
/// filled while the cards prepaint, read when the stones are painted after
/// them in the same frame.
#[derive(Clone, Debug, Default)]
pub struct Marks(Rc<RefCell<Vec<Option<Bounds<Pixels>>>>>);

impl Marks {
    /// No marks yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The mark of card `index` is at `at`.
    pub fn set(&self, index: usize, at: Bounds<Pixels>) {
        let mut marks = self.0.borrow_mut();
        if marks.len() <= index {
            marks.resize(index + 1, None);
        }
        marks[index] = Some(at);
    }

    /// Where card `index`'s mark is, when it has been laid out.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<Bounds<Pixels>> {
        self.0.borrow().get(index).copied().flatten()
    }
}

/// How far the carry has come, `0..=1`: 1 under reduced motion (nothing
/// flies) and once it has landed.
#[must_use]
pub fn progress(carry: &Carry, cx: &App) -> f32 {
    if reduced(cx) {
        1.0
    } else {
        carry.value(now(cx)).clamp(0.0, 1.0)
    }
}

/// How visible a card is at carry progress `p`: its face and its mark stay
/// away until the shingles are nearly over them.
#[must_use]
pub fn arrival(p: f32) -> f32 {
    band(p, 0.55, 1.0)
}

/// A wrapper that reports where its child is laid out to [`Marks`].
pub struct Mark {
    marks: Marks,
    index: usize,
    child: AnyElement,
}

/// `child`, reporting its bounds as card `index`'s mark.
#[must_use]
pub fn mark(marks: &Marks, index: usize, child: impl IntoElement) -> Mark {
    Mark {
        marks: marks.clone(),
        index,
        child: child.into_any_element(),
    }
}

impl IntoElement for Mark {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Mark {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
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
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.marks.set(self.index, bounds);
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

/// The stones in the air (see [`flight`]).
pub struct Flight {
    id: ElementId,
    stones: Rc<[Stone]>,
    marks: Marks,
    carry: Carry,
}

/// The shingles of a module on their way to `marks`, along `carry`. `id` names
/// the flight in the probe (`{id}-stone-{index}` where each stone is, `{id}-mark-{index}` where
/// it is going, one pair per stone in the air).
#[must_use]
pub fn flight(id: ElementId, stones: Rc<[Stone]>, marks: Marks, carry: Carry) -> Flight {
    Flight {
        id,
        stones,
        marks,
        carry,
    }
}

impl IntoElement for Flight {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for Flight {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
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
    ) -> (LayoutId, ()) {
        // No room of its own: it paints in the air.
        let mut style = Style::default();
        style.position = Position::Absolute;
        style.size.width = px(0.0).into();
        style.size.height = px(0.0).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        _window: &mut Window,
        _cx: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        _prepaint: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let p = progress(&self.carry, cx);
        if p >= 1.0 {
            return;
        }
        request_frame(window, cx);
        let lerp = |a: Pixels, b: Pixels| a + (b - a) * p;
        let mut batches: Vec<(Hsla, Fill)> = Vec::new();
        for (index, stone) in self.stones.iter().enumerate() {
            // A name with no card yet stays where it was.
            let to = self.marks.get(index).unwrap_or(stone.from);
            let (x, y) = (
                lerp(stone.from.origin.x, to.origin.x),
                lerp(stone.from.origin.y, to.origin.y),
            );
            let (w, h) = (
                lerp(stone.from.size.width, to.size.width),
                lerp(stone.from.size.height, to.size.height),
            );
            if probe::enabled(cx) {
                probe::record_bounds(
                    cx,
                    &key(&self.id, format!("stone-{index}")),
                    Bounds::new(point(x, y), size(w, h)),
                );
                probe::record_bounds(cx, &key(&self.id, format!("mark-{index}")), to);
            }
            let (w, h) = (f32::from(w), f32::from(h));
            let poly = Poly::chamfer(f32::from(x), f32::from(y), w, h, (w.min(h) * 0.3).max(1.5));
            let at = batches
                .iter()
                .position(|(ink, _)| *ink == stone.ink)
                .unwrap_or_else(|| {
                    batches.push((stone.ink, Fill::new()));
                    batches.len() - 1
                });
            batches[at].1.poly(&poly);
        }
        for (ink, fill) in batches {
            fill.paint(window, ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_card_is_away_until_its_shingle_is_nearly_over_it_and_whole_when_it_lands() {
        assert!(
            arrival(0.0) == 0.0 && arrival(0.5) == 0.0,
            "nothing of a card shows while its shingle is far"
        );
        assert!(arrival(0.8) > 0.4 && arrival(0.8) < 0.7);
        assert!((arrival(1.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn marks_are_reported_by_index_and_a_missing_one_is_none() {
        let marks = Marks::new();
        assert_eq!(marks.get(2), None);
        marks.set(
            2,
            Bounds::new(point(px(4.0), px(5.0)), size(px(12.0), px(12.0))),
        );
        assert_eq!(marks.get(2).map(|b| f32::from(b.origin.x)), Some(4.0));
        assert_eq!(
            marks.get(0),
            None,
            "the ones before it are unreported, not zeros"
        );
    }
}

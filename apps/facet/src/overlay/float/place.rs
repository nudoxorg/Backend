//! Placement: where a card of a given size goes for an anchor, pure
//! geometry with no window.
//!
//! One function decides everything a card's position depends on: the
//! preferred side, the flip when that side has no room, the shift along the
//! edge that keeps the card [`MARGIN`] inside the viewport, the height cap
//! (the card scrolls inside when even the roomier side is too short), the
//! bottom sheet at narrow room, and the hairline connector back to the
//! exact anchor rect.

use super::Side;
use gpui::{Bounds, Pixels, Point, Size, point, px, size};

/// How far every card stays inside the viewport.
pub const MARGIN: f32 = 8.0;
/// The default gap between an anchor and a card beside it (kinds pass their
/// own: the hairline connector spans it).
pub const GAP: f32 = 6.0;
/// The gap between a parent card and its chained child.
pub const CHAIN_GAP: f32 = 16.0;
/// A connector is drawn only when the gap it bridges is at least this long.
const CONNECT_FROM: f32 = 3.0;

/// Where a card goes and how it hangs off its anchor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The card's plate, window coordinates.
    pub bounds: Bounds<Pixels>,
    /// The side it ended up on (after flipping).
    pub side: Side,
    /// The height the content was capped to (`None`: it fits).
    pub capped: Option<Pixels>,
    /// The most height this placement allows.
    pub room: Pixels,
    /// A hairline from the anchor's edge to the card's near edge.
    pub connector: Option<(Point<Pixels>, Point<Pixels>)>,
}

/// What a card hangs off.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hang {
    /// A trigger on the page (or a tip anywhere): beside the anchor rect.
    Anchor,
    /// A chained child: beside its parent card (right, or left without
    /// room), its top level with the anchor word inside the parent.
    Parent(Bounds<Pixels>),
    /// Narrow room: a full-width sheet pinned to the window bottom.
    Sheet,
}

fn f(value: Pixels) -> f32 {
    f32::from(value)
}

/// Places a card of `natural` size for `anchor`.
#[must_use]
pub fn place(
    anchor: Bounds<Pixels>,
    natural: Size<Pixels>,
    side: Side,
    hang: Hang,
    viewport: Size<Pixels>,
) -> Placement {
    place_with_gap(anchor, natural, side, hang, viewport, GAP)
}

/// [`place`] with an explicit anchor gap.
#[must_use]
pub fn place_with_gap(
    anchor: Bounds<Pixels>,
    natural: Size<Pixels>,
    side: Side,
    hang: Hang,
    viewport: Size<Pixels>,
    gap: f32,
) -> Placement {
    let (vw, vh) = (f(viewport.width), f(viewport.height));
    let (w, h) = (
        f(natural.width).min((vw - 2.0 * MARGIN).max(1.0)),
        f(natural.height),
    );
    match hang {
        Hang::Sheet => {
            let cap = (vh * 0.72).max(1.0);
            let height = h.min(cap);
            Placement {
                bounds: Bounds::new(point(px(0.0), px(vh - height)), size(px(vw), px(height))),
                side: Side::Above,
                capped: (h > cap).then_some(px(cap)),
                room: px(cap),
                connector: None,
            }
        }
        Hang::Parent(parent) => beside_parent(anchor, parent, w, h, vw, vh),
        Hang::Anchor => beside_anchor(anchor, w, h, side, vw, vh, gap),
    }
}

fn clamp_axis(start: f32, length: f32, limit: f32) -> f32 {
    let low = MARGIN;
    let high = limit - MARGIN - length;
    if high < low { low } else { start.clamp(low, high) }
}

fn beside_anchor(anchor: Bounds<Pixels>, w: f32, h: f32, side: Side, vw: f32, vh: f32, gap: f32) -> Placement {
    let (ax, ay) = (f(anchor.origin.x), f(anchor.origin.y));
    let (aw, ah) = (f(anchor.size.width), f(anchor.size.height));
    let room = |side: Side| match side {
        Side::Below => vh - MARGIN - (ay + ah + gap),
        Side::Above => ay - gap - MARGIN,
        Side::Right => vw - MARGIN - (ax + aw + gap),
        Side::Left => ax - gap - MARGIN,
    };
    let need = |side: Side| match side {
        Side::Below | Side::Above => h,
        Side::Right | Side::Left => w,
    };
    let opposite = side.opposite();
    let chosen = if room(side) >= need(side) || room(side) >= room(opposite) {
        side
    } else {
        opposite
    };
    let vertical = matches!(chosen, Side::Above | Side::Below);
    // Cap the extent along the placement axis to the room on that side
    // (vertical sides only: a side card is never taller than the viewport).
    let space = if vertical {
        room(chosen).max(40.0)
    } else {
        vh - 2.0 * MARGIN
    };
    let (height, capped) = if h > space { (space, Some(px(space))) } else { (h, None) };
    let (x, y) = match chosen {
        // Above and below: centred on the anchor's midpoint; beside: the
        // card's top level with the anchor's top. Then shifted inside.
        Side::Below => (clamp_axis(ax + aw / 2.0 - w / 2.0, w, vw), ay + ah + gap),
        Side::Above => (clamp_axis(ax + aw / 2.0 - w / 2.0, w, vw), ay - gap - height),
        Side::Right => (ax + aw + gap, clamp_axis(ay, height, vh)),
        Side::Left => (ax - gap - w, clamp_axis(ay, height, vh)),
    };
    // A side placement can still overflow when neither side had room: shift.
    let x = clamp_axis(x, w, vw);
    let y = clamp_axis(y, height, vh);
    let bounds = Bounds::new(point(px(x), px(y)), size(px(w), px(height)));
    Placement {
        bounds,
        side: chosen,
        capped,
        room: px(space),
        connector: connector(anchor, bounds, chosen),
    }
}

fn beside_parent(
    anchor: Bounds<Pixels>,
    parent: Bounds<Pixels>,
    w: f32,
    h: f32,
    vw: f32,
    vh: f32,
) -> Placement {
    let (px0, pw) = (f(parent.origin.x), f(parent.size.width));
    let right_room = vw - MARGIN - (px0 + pw + CHAIN_GAP);
    let left_room = px0 - CHAIN_GAP - MARGIN;
    let side = if right_room >= w || right_room >= left_room {
        Side::Right
    } else {
        Side::Left
    };
    let x = match side {
        Side::Left => px0 - CHAIN_GAP - w,
        _ => px0 + pw + CHAIN_GAP,
    };
    let x = clamp_axis(x, w, vw);
    let space = vh - 2.0 * MARGIN;
    let (height, capped) = if h > space { (space, Some(px(space))) } else { (h, None) };
    let y = clamp_axis(f(anchor.origin.y), height, vh);
    let bounds = Bounds::new(point(px(x), px(y)), size(px(w), px(height)));
    Placement {
        bounds,
        side,
        capped,
        room: px(space),
        connector: connector(anchor, bounds, side),
    }
}

/// The hairline from the anchor's edge to the card's near edge, straight
/// along the placement axis, clamped so it lands on the plate (not in a cut
/// corner).
pub fn connector(
    anchor: Bounds<Pixels>,
    card: Bounds<Pixels>,
    side: Side,
) -> Option<(Point<Pixels>, Point<Pixels>)> {
    let inset = 12.0_f32;
    let (cx0, cy0) = (f(card.origin.x), f(card.origin.y));
    let (cx1, cy1) = (cx0 + f(card.size.width), cy0 + f(card.size.height));
    let (ax0, ay0) = (f(anchor.origin.x), f(anchor.origin.y));
    let (ax1, ay1) = (ax0 + f(anchor.size.width), ay0 + f(anchor.size.height));
    let clamp = |v: f32, lo: f32, hi: f32| if hi < lo { (lo + hi) / 2.0 } else { v.clamp(lo, hi) };
    let (from, to) = match side {
        Side::Below => {
            let x = clamp((ax0 + ax1) / 2.0, cx0 + inset, cx1 - inset);
            ((x, ay1), (x, cy0))
        }
        Side::Above => {
            let x = clamp((ax0 + ax1) / 2.0, cx0 + inset, cx1 - inset);
            ((x, ay0), (x, cy1))
        }
        Side::Right => {
            let y = clamp((ay0 + ay1) / 2.0, cy0 + inset, cy1 - inset);
            ((ax1, y), (cx0, y))
        }
        Side::Left => {
            let y = clamp((ay0 + ay1) / 2.0, cy0 + inset, cy1 - inset);
            ((ax0, y), (cx1, y))
        }
    };
    let length = (to.0 - from.0).abs() + (to.1 - from.1).abs();
    // Only a connector that runs outward, across a real gap.
    let outward = match side {
        Side::Below => to.1 > from.1,
        Side::Above => to.1 < from.1,
        Side::Right => to.0 > from.0,
        Side::Left => to.0 < from.0,
    };
    (outward && length >= CONNECT_FROM)
        .then(|| (point(px(from.0), px(from.1)), point(px(to.0), px(to.1))))
}

#[cfg(test)]
mod tests {
    use super::{Hang, MARGIN, place};
    use crate::overlay::float::Side;
    use gpui::{Bounds, Pixels, point, px, size};

    fn b(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    fn inside(bounds: Bounds<Pixels>, vw: f32, vh: f32) -> bool {
        let (x, y) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
        let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        x >= MARGIN - 0.01 && y >= MARGIN - 0.01 && x + w <= vw - MARGIN + 0.01 && y + h <= vh - MARGIN + 0.01
    }

    #[test]
    fn below_by_default_and_flips_above_at_the_bottom_edge() {
        let vp = size(px(1000.0), px(700.0));
        let card = size(px(300.0), px(200.0));
        let top = place(b(100.0, 100.0, 80.0, 18.0), card, Side::Below, Hang::Anchor, vp);
        assert_eq!(top.side, Side::Below);
        assert!(f32::from(top.bounds.origin.y) > 118.0);
        let low = place(b(100.0, 620.0, 80.0, 18.0), card, Side::Below, Hang::Anchor, vp);
        assert_eq!(low.side, Side::Above, "no room below: flips");
        assert!(f32::from(low.bounds.origin.y) + 200.0 <= 620.0);
        assert!(inside(low.bounds, 1000.0, 700.0));
    }

    #[test]
    fn shifts_along_the_edge_to_stay_eight_px_inside() {
        let vp = size(px(800.0), px(600.0));
        let card = size(px(392.0), px(240.0));
        let right = place(b(760.0, 100.0, 30.0, 18.0), card, Side::Below, Hang::Anchor, vp);
        assert!((f32::from(right.bounds.origin.x) + 392.0 - (800.0 - MARGIN)).abs() < 0.01);
        let left = place(b(2.0, 100.0, 30.0, 18.0), card, Side::Below, Hang::Anchor, vp);
        assert!((f32::from(left.bounds.origin.x) - MARGIN).abs() < 0.01);
    }

    #[test]
    fn caps_height_to_the_roomier_side_and_never_leaves_the_viewport() {
        let vp = size(px(600.0), px(400.0));
        let tall = size(px(300.0), px(900.0));
        for y in [10.0_f32, 150.0, 380.0] {
            for side in [Side::Below, Side::Above, Side::Left, Side::Right] {
                let placed = place(b(250.0, y, 60.0, 16.0), tall, side, Hang::Anchor, vp);
                assert!(placed.capped.is_some(), "900 px never fits 400");
                assert!(inside(placed.bounds, 600.0, 400.0), "{side:?} at {y}: {:?}", placed.bounds);
            }
        }
    }

    #[test]
    fn above_and_below_centre_on_the_anchor_and_beside_aligns_tops() {
        let vp = size(px(1200.0), px(800.0));
        let anchor = b(500.0, 300.0, 40.0, 18.0);
        let below = place(anchor, size(px(300.0), px(120.0)), Side::Below, Hang::Anchor, vp);
        let centre = f32::from(below.bounds.origin.x) + 150.0;
        assert!((centre - 520.0).abs() < 0.01, "centred on 520, got {centre}");
        let above = place(anchor, size(px(300.0), px(120.0)), Side::Above, Hang::Anchor, vp);
        assert!((f32::from(above.bounds.origin.x) + 150.0 - 520.0).abs() < 0.01);
        let right = place(anchor, size(px(300.0), px(120.0)), Side::Right, Hang::Anchor, vp);
        assert_eq!(right.bounds.origin.y, px(300.0), "top level with the anchor's top");
        let gap = super::place_with_gap(anchor, size(px(300.0), px(120.0)), Side::Below, Hang::Anchor, vp, 14.0);
        let (from, to) = gap.connector.expect("a 14 px gap has a connector");
        assert_eq!(to.y - from.y, px(14.0));
    }

    #[test]
    fn a_wide_card_is_narrowed_to_the_viewport() {
        let placed = place(
            b(10.0, 10.0, 40.0, 16.0),
            size(px(900.0), px(100.0)),
            Side::Below,
            Hang::Anchor,
            size(px(420.0), px(800.0)),
        );
        assert!((f32::from(placed.bounds.size.width) - (420.0 - 2.0 * MARGIN)).abs() < 0.01);
        assert!(inside(placed.bounds, 420.0, 800.0));
    }

    #[test]
    fn children_go_right_of_the_parent_or_left_without_room() {
        let vp = size(px(1400.0), px(900.0));
        let parent = b(200.0, 100.0, 392.0, 300.0);
        let word = b(420.0, 220.0, 80.0, 18.0);
        let child = place(word, size(px(330.0), px(200.0)), Side::Right, Hang::Parent(parent), vp);
        assert_eq!(child.side, Side::Right);
        assert!(f32::from(child.bounds.origin.x) >= 592.0 + 15.9);
        assert!((f32::from(child.bounds.origin.y) - 220.0).abs() < 0.01, "top level with the word");
        let connector = child.connector.expect("a gap to bridge");
        assert!((f32::from(connector.0.x) - 500.0).abs() < 0.01, "starts at the word's right edge");
        let crowded = b(900.0, 100.0, 392.0, 300.0);
        let child = place(word, size(px(330.0), px(200.0)), Side::Right, Hang::Parent(crowded), vp);
        assert_eq!(child.side, Side::Left);
        assert!(f32::from(child.bounds.origin.x) + 330.0 <= 900.0 - 15.9);
    }

    #[test]
    fn narrow_sheet_is_full_width_on_the_bottom_edge() {
        let vp = size(px(420.0), px(800.0));
        let sheet = place(b(50.0, 100.0, 60.0, 16.0), size(px(392.0), px(300.0)), Side::Below, Hang::Sheet, vp);
        assert_eq!(sheet.bounds.size.width, px(420.0));
        assert_eq!(sheet.bounds.origin.y + sheet.bounds.size.height, px(800.0));
        assert!(sheet.connector.is_none());
    }

    #[test]
    fn connector_touches_the_anchor_and_the_card() {
        let vp = size(px(1000.0), px(800.0));
        let anchor = b(300.0, 200.0, 120.0, 18.0);
        let placed = place(anchor, size(px(392.0), px(260.0)), Side::Below, Hang::Anchor, vp);
        let (from, to) = placed.connector.expect("below with a gap");
        assert_eq!(from.y, px(218.0));
        assert_eq!(to.y, placed.bounds.origin.y);
        assert_eq!(from.x, px(360.0), "anchor centre");
    }
}

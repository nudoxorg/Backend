//! Print: content revealed in reading order by a travelling edge, and
//! [`masked`], a child cut to a window rectangle (W-Motion PLAN §2a).
//!
//! A page does not fade in; it *prints*. One horizontal edge travels down
//! the page and everything above it is drawn, everything below it is not:
//! reading order, by clip. At the storyboard's pace (a line every 8 ms) the
//! edge crosses a line of text in well under a frame, so no line is ever
//! part-drawn for more than two frames. As the edge enters a block the block
//! settles the last few pixels down (the 8 px settle), so each block lands
//! rather than appears. The fold of a Close is the same edge travelling up:
//! last lines first.
//!
//! ```ignore
//! // Every section of the arriving page, under one edge:
//! print(Some(Edge::down(y, px(8.0), px(136.0))), section)
//! ```
//!
//! Both elements keep their child's layout exactly: a settled print (no
//! edge, or an edge past the page) paints exactly what layout put there.

use gpui::{
    AnyElement, App, Bounds, ContentMask, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Window, point, px, size,
};

/// Where the print stands this frame, in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Edge {
    /// Everything above this y is drawn.
    pub y: Pixels,
    /// How far a block drops as the edge enters it (0 for a fold).
    pub settle: Pixels,
    /// How far the edge travels while a block settles.
    pub span: Pixels,
}

impl Edge {
    /// An edge printing downward at `y`: each block drops `settle` over the
    /// first `span` of its print.
    #[must_use]
    pub const fn down(y: Pixels, settle: Pixels, span: Pixels) -> Self {
        Self { y, settle, span }
    }

    /// An edge folding upward at `y` (nothing settles).
    #[must_use]
    pub const fn fold(y: Pixels) -> Self {
        Self {
            y,
            settle: Pixels::ZERO,
            span: Pixels::ZERO,
        }
    }

    /// How far a block whose top is at `top` sits below its place: the
    /// settle, closing as the edge travels `span` into it.
    #[must_use]
    pub fn drop_at(&self, top: Pixels) -> Pixels {
        if self.settle <= Pixels::ZERO || self.span <= Pixels::ZERO {
            return Pixels::ZERO;
        }
        let into = f32::from(self.y - top) / f32::from(self.span);
        self.settle * (1.0 - into.clamp(0.0, 1.0))
    }
}

/// Wraps `child` so only what lies above `edge` is drawn (all of it when
/// `edge` is `None`).
pub fn print(edge: Option<Edge>, child: impl IntoElement) -> Print {
    Print {
        edge,
        child: child.into_any_element(),
        mask: None,
    }
}

/// See [`print`].
pub struct Print {
    edge: Option<Edge>,
    child: AnyElement,
    mask: Option<ContentMask<Pixels>>,
}

/// A clip wide enough for any overhang sideways, cut at `bottom`.
fn above(bounds: Bounds<Pixels>, bottom: Pixels) -> Bounds<Pixels> {
    let reach = px(100_000.0);
    Bounds {
        origin: point(bounds.origin.x - reach, bounds.origin.y - reach),
        size: size(
            bounds.size.width + reach * 2.0,
            (bottom - (bounds.origin.y - reach)).max(Pixels::ZERO),
        ),
    }
}

impl IntoElement for Print {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Print {
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let Some(edge) = self.edge else {
            self.child.prepaint(window, cx);
            return;
        };
        // The edge is in window space; the bounds are too, before any layer
        // transform (a page drifting inside a transformed layer maps back).
        let edge_y = window
            .layer_transform()
            .inverse()
            .map_or(edge.y, |inverse| inverse.apply(point(bounds.origin.x, edge.y)).y);
        let mask = ContentMask {
            bounds: above(bounds, edge_y),
        };
        self.mask = Some(mask);
        let drop = Edge { y: edge_y, ..edge }.drop_at(bounds.origin.y);
        let child = &mut self.child;
        window.with_content_mask(Some(mask), |window| {
            window.with_element_offset(point(Pixels::ZERO, drop), |window| child.prepaint(window, cx));
        });
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_content_mask(self.mask.take(), |window| child.paint(window, cx));
    }
}

/// Wraps `child` so it is drawn only inside `bounds` (window coordinates):
/// the plate a page is drawn on, or the part of a page a plate has not yet
/// covered.
pub fn masked(bounds: Bounds<Pixels>, child: impl IntoElement) -> Masked {
    Masked {
        bounds,
        child: child.into_any_element(),
        mask: None,
    }
}

/// See [`masked`].
pub struct Masked {
    bounds: Bounds<Pixels>,
    child: AnyElement,
    mask: Option<ContentMask<Pixels>>,
}

impl IntoElement for Masked {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Masked {
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
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let bounds = window
            .layer_transform()
            .inverse()
            .map_or(self.bounds, |inverse| inverse.apply_bounds(self.bounds));
        let mask = ContentMask { bounds };
        self.mask = Some(mask);
        let child = &mut self.child;
        window.with_content_mask(Some(mask), |window| child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_content_mask(self.mask.take(), |window| child.paint(window, cx));
    }
}

#[cfg(test)]
mod tests {
    use super::Edge;
    use gpui::px;

    /// A block settles 8 px as the edge enters it and none once the edge is
    /// `span` inside; a fold never settles.
    #[test]
    fn a_block_drops_as_the_edge_enters_and_lands_within_its_span() {
        let edge = |y: f32| Edge::down(px(y), px(8.0), px(128.0));
        assert_eq!(edge(100.0).drop_at(px(100.0)), px(8.0));
        assert_eq!(edge(164.0).drop_at(px(100.0)), px(4.0));
        assert_eq!(edge(228.0).drop_at(px(100.0)), px(0.0));
        assert_eq!(Edge::fold(px(100.0)).drop_at(px(50.0)), px(0.0));
    }
}

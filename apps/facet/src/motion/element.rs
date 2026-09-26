//! Layout-neutral motion elements: [`Offset`] moves a child (and optionally
//! fades it) without moving anything else; [`Reveal`] clips a child to a
//! part of its own box. Both keep the child's layout exactly, so a settled
//! motion leaves the element precisely where layout put it.

use super::keys::Pose;
use gpui::{
    AnyElement, App, Bounds, ContentMask, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Point, Window, point, px, size,
};

/// Translates (and optionally fades) a child at paint time.
pub struct Offset {
    child: AnyElement,
    shift: Point<Pixels>,
    opacity: Option<f32>,
}

/// Wraps `child` in an [`Offset`] of zero.
pub fn offset(child: impl IntoElement) -> Offset {
    Offset {
        child: child.into_any_element(),
        shift: Point::default(),
        opacity: None,
    }
}

impl Offset {
    /// Horizontal translation.
    #[must_use]
    pub fn x(mut self, x: Pixels) -> Self {
        self.shift.x = x;
        self
    }

    /// Vertical translation.
    #[must_use]
    pub fn y(mut self, y: Pixels) -> Self {
        self.shift.y = y;
        self
    }

    /// Subtree opacity (multiplies with any ancestor's).
    #[must_use]
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = Some(opacity.clamp(0.0, 1.0));
        self
    }

    /// A pose's translation and opacity (its scale belongs to painted
    /// primitives: see [`posed`]).
    #[must_use]
    pub fn pose(self, pose: Pose) -> Self {
        self.x(px(pose.x)).y(px(pose.y)).opacity(pose.opacity)
    }
}

impl IntoElement for Offset {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Offset {
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
        let (child, shift, opacity) = (&mut self.child, self.shift, self.opacity);
        window.with_element_offset(shift, |window| {
            window.with_element_opacity(opacity, |window| child.prepaint(window, cx));
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
        window.with_element_opacity(self.opacity, |window| child.paint(window, cx));
    }
}

/// Clips a child to part of its own box, like CSS `clip-path: inset(…)` in
/// fractions of the box: `reveal(x).right(0.4)` hides the right 40 %.
pub struct Reveal {
    child: AnyElement,
    top: f32,
    right: f32,
    bottom: f32,
    left: f32,
    mask: Option<ContentMask<Pixels>>,
}

/// Wraps `child` in a fully open [`Reveal`].
pub fn reveal(child: impl IntoElement) -> Reveal {
    Reveal {
        child: child.into_any_element(),
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
        mask: None,
    }
}

impl Reveal {
    /// Hides this fraction of the box from the top.
    #[must_use]
    pub fn top(mut self, fraction: f32) -> Self {
        self.top = fraction.clamp(0.0, 1.0);
        self
    }

    /// Hides this fraction of the box from the right.
    #[must_use]
    pub fn right(mut self, fraction: f32) -> Self {
        self.right = fraction.clamp(0.0, 1.0);
        self
    }

    /// Hides this fraction of the box from the bottom.
    #[must_use]
    pub fn bottom(mut self, fraction: f32) -> Self {
        self.bottom = fraction.clamp(0.0, 1.0);
        self
    }

    /// Hides this fraction of the box from the left.
    #[must_use]
    pub fn left(mut self, fraction: f32) -> Self {
        self.left = fraction.clamp(0.0, 1.0);
        self
    }

    fn clip(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        let (width, height) = (bounds.size.width, bounds.size.height);
        let left = width * self.left;
        let top = height * self.top;
        let right = width * self.right;
        let bottom = height * self.bottom;
        Bounds {
            origin: point(bounds.origin.x + left, bounds.origin.y + top),
            size: size(
                (width - left - right).max(Pixels::ZERO),
                (height - top - bottom).max(Pixels::ZERO),
            ),
        }
    }
}

impl IntoElement for Reveal {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Reveal {
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
        let mask = ContentMask {
            bounds: self.clip(bounds),
        };
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

/// Where a painted primitive laid out at `bounds` draws under `pose`: scaled
/// about the box centre, then translated. Custom-painted primitives (the cut
/// plate, the gem) take this instead of a CSS transform, which GPUI lacks.
#[must_use]
pub fn posed(bounds: Bounds<Pixels>, pose: Pose) -> Bounds<Pixels> {
    let centre = bounds.center();
    let width = bounds.size.width * pose.sx;
    let height = bounds.size.height * pose.sy;
    Bounds {
        origin: point(
            centre.x - width / 2.0 + px(pose.x),
            centre.y - height / 2.0 + px(pose.y),
        ),
        size: size(width, height),
    }
}

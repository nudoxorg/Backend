//! NUDOX: compositing layers. A subtree painted under a 2D transform (scale
//! about an origin, then translate) and at a group opacity.
//!
//! The transform is applied where primitives enter the scene
//! ([`Window::with_layer_transform`]): quads, borders, shadows, paths,
//! underlines, images and SVGs are placed and sized in window space, and
//! glyphs are *rasterized* at the layer's scale, so text under a scaled layer
//! is as sharp as text laid out at that size. Hitboxes and content masks are
//! transformed too, so hover and clicks follow what is painted. An identity
//! transform takes the exact untransformed code path: a layer at rest paints
//! bit-identical pixels to no layer at all.
//!
//! Group opacity ([`Window::with_group_opacity`]) renders the subtree into an
//! offscreen target and composites it once, so overlapping children fade as
//! one surface rather than showing through each other. At opacity 1 no group
//! is created.

use crate::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Point, Size, Window, point,
};

/// An axis-aligned 2D transform for a painted subtree: `p' = p * scale + offset`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayerTransform {
    /// Per-axis scale.
    pub scale: Size<f32>,
    /// Added after scaling, in window pixels.
    pub offset: Point<Pixels>,
}

impl Default for LayerTransform {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl LayerTransform {
    /// No transform.
    pub const IDENTITY: Self = Self {
        scale: Size {
            width: 1.0,
            height: 1.0,
        },
        offset: Point {
            x: Pixels::ZERO,
            y: Pixels::ZERO,
        },
    };

    /// Scales by `scale` about `origin` (window coordinates).
    pub fn scale_about(origin: Point<Pixels>, scale: Size<f32>) -> Self {
        Self {
            scale,
            offset: point(
                origin.x - origin.x * scale.width,
                origin.y - origin.y * scale.height,
            ),
        }
    }

    /// Translates by `offset`.
    pub fn translation(offset: Point<Pixels>) -> Self {
        Self {
            scale: Self::IDENTITY.scale,
            offset,
        }
    }

    /// Whether this is exactly the identity (the untransformed paint path).
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }

    /// `self` applied first, then `outer`.
    pub fn then(self, outer: Self) -> Self {
        if self.is_identity() {
            return outer;
        }
        if outer.is_identity() {
            return self;
        }
        Self {
            scale: Size {
                width: self.scale.width * outer.scale.width,
                height: self.scale.height * outer.scale.height,
            },
            offset: point(
                self.offset.x * outer.scale.width + outer.offset.x,
                self.offset.y * outer.scale.height + outer.offset.y,
            ),
        }
    }

    /// Maps a point.
    pub fn apply(&self, p: Point<Pixels>) -> Point<Pixels> {
        if self.is_identity() {
            return p;
        }
        point(
            p.x * self.scale.width + self.offset.x,
            p.y * self.scale.height + self.offset.y,
        )
    }

    /// Maps a rectangle (normalized if a scale is negative).
    pub fn apply_bounds(&self, bounds: Bounds<Pixels>) -> Bounds<Pixels> {
        if self.is_identity() {
            return bounds;
        }
        let a = self.apply(bounds.origin);
        let b = self.apply(bounds.bottom_right());
        Bounds::from_corners(
            point(a.x.min(b.x), a.y.min(b.y)),
            point(a.x.max(b.x), a.y.max(b.y)),
        )
    }

    /// The inverse, if the transform is invertible.
    pub fn inverse(&self) -> Option<Self> {
        if self.is_identity() {
            return Some(*self);
        }
        let (sx, sy) = (self.scale.width, self.scale.height);
        if sx.abs() < 1e-6 || sy.abs() < 1e-6 {
            return None;
        }
        Some(Self {
            scale: Size {
                width: 1.0 / sx,
                height: 1.0 / sy,
            },
            offset: point(-self.offset.x / sx, -self.offset.y / sy),
        })
    }

    /// The factor lengths without a direction (corner radii, stroke widths,
    /// blur radii) scale by: the smaller axis, so a squash never inflates a
    /// radius past the box it rounds.
    pub fn length_scale(&self) -> f32 {
        self.scale.width.abs().min(self.scale.height.abs())
    }

    /// The scale glyphs are rasterized at: the larger axis (downsampling a
    /// glyph along the other axis stays sharp), quantized to 1/32 so an
    /// animated scale reuses atlas entries instead of minting one per frame.
    pub fn raster_scale(&self) -> f32 {
        let scale = self.scale.width.abs().max(self.scale.height.abs());
        ((scale * 32.0).round() / 32.0).clamp(1.0 / 32.0, 8.0)
    }
}

/// A subtree painted under a transform and a group opacity; layout-neutral.
/// Build with [`layer`].
///
/// ```ignore
/// layer(card).opacity(0.6).scale(0.96).origin(0.5, 0.0)
/// ```
pub struct Layer {
    child: AnyElement,
    opacity: f32,
    scale: Size<f32>,
    origin: Point<f32>,
    translate: Point<Pixels>,
    transform: LayerTransform,
}

/// Wraps `child` in a [`Layer`] at rest (opacity 1, scale 1, centred origin).
pub fn layer(child: impl IntoElement) -> Layer {
    Layer {
        child: child.into_any_element(),
        opacity: 1.0,
        scale: LayerTransform::IDENTITY.scale,
        origin: point(0.5, 0.5),
        translate: Point::default(),
        transform: LayerTransform::IDENTITY,
    }
}

impl Layer {
    /// Group opacity: the subtree fades as one surface.
    pub fn opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    /// Uniform scale about the origin.
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = Size {
            width: scale,
            height: scale,
        };
        self
    }

    /// Per-axis scale about the origin (squash and stretch).
    pub fn scale_xy(mut self, x: f32, y: f32) -> Self {
        self.scale = Size {
            width: x,
            height: y,
        };
        self
    }

    /// The scale origin as a fraction of the layer's box (0.5, 0.5 = centre).
    pub fn origin(mut self, x: f32, y: f32) -> Self {
        self.origin = point(x, y);
        self
    }

    /// Translation, applied like an element offset (layout does not move).
    pub fn translate(mut self, offset: Point<Pixels>) -> Self {
        self.translate = offset;
        self
    }
}

impl IntoElement for Layer {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Layer {
    type RequestLayoutState = ();
    type PrepaintState = Bounds<Pixels>;

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
        let moved = Bounds {
            origin: bounds.origin + self.translate,
            size: bounds.size,
        };
        self.transform = if self.scale == LayerTransform::IDENTITY.scale {
            LayerTransform::IDENTITY
        } else {
            let origin = point(
                moved.origin.x + moved.size.width * self.origin.x,
                moved.origin.y + moved.size.height * self.origin.y,
            );
            LayerTransform::scale_about(origin, self.scale)
        };
        let (child, transform, translate, opacity) =
            (&mut self.child, self.transform, self.translate, self.opacity);
        window.with_element_offset(translate, |window| {
            window.with_layer_transform(transform, |window| {
                window.with_group_opacity(moved, opacity, |window| child.prepaint(window, cx));
            });
        });
        moved
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        moved: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (child, transform, opacity, moved) = (&mut self.child, self.transform, self.opacity, *moved);
        window.with_layer_transform(transform, |window| {
            window.with_group_opacity(moved, opacity, |window| child.paint(window, cx));
        });
    }
}

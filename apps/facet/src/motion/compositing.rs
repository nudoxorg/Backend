//! Compositing: facet's door to the gpui additions (see
//! `vendor/gpui-ce/NUDOX-PATCHES.md`).
//!
//! - [`layer`]`(child).opacity(..).scale(..).scale_xy(..).origin(..).translate(..)`:
//!   a subtree under a 2D transform and a group opacity. The transform is
//!   applied where primitives enter the scene (glyphs re-rasterized at the
//!   layer's scale, hitboxes and masks transformed); the opacity composites the
//!   subtree once from an offscreen group. At rest it is bit-identical to no
//!   layer at all.
//! - [`Window::paint_chamfer_shadows`](gpui::Window::paint_chamfer_shadows):
//!   the blurred shadow of a chamfered rectangle (a cut plate), exact for the
//!   polygon.
//!
//! [`chamfers`] builds the corner cuts of a cut plate.

pub use gpui::{Compositing, Layer, LayerTransform, layer};

use gpui::{Corners, Pixels, px};

/// The corner cuts of a cut plate: `chamfer` top-left and bottom-right, square
/// top-right and bottom-left.
#[must_use]
pub fn chamfers(chamfer: f32) -> Corners<Pixels> {
    Corners {
        top_left: px(chamfer),
        top_right: px(0.0),
        bottom_right: px(chamfer),
        bottom_left: px(0.0),
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)]
mod tests {
    use gpui::{
        Bounds, ContentMask, Corners, FilterBoundary, LayerTransform, PrimitiveBatch, Quad,
        ScaledFilter, ScaledPixels, Scene, Size, point, px, size,
    };
    use smallvec::SmallVec;

    #[test]
    fn transforms_compose_inner_then_outer_and_invert() {
        let inner = LayerTransform::scale_about(
            point(px(10.), px(10.)),
            Size {
                width: 2.,
                height: 3.,
            },
        );
        let outer = LayerTransform::translation(point(px(5.), px(-4.)));
        let both = inner.then(outer);
        let p = point(px(12.), px(11.));
        assert_eq!(both.apply(p), outer.apply(inner.apply(p)));
        assert_eq!(both.apply(p), point(px(19.), px(9.)));
        let back = both.inverse().expect("invertible");
        let q = back.apply(both.apply(p));
        assert!(
            (f32::from(q.x) - f32::from(p.x)).abs() < 1e-4
                && (f32::from(q.y) - f32::from(p.y)).abs() < 1e-4
        );
        let squash = LayerTransform::scale_about(
            point(px(0.), px(0.)),
            Size {
                width: 1.18,
                height: 0.78,
            },
        );
        assert_eq!(squash.raster_scale(), 38. / 32.);
        assert_eq!(squash.length_scale(), 0.78);
    }

    #[test]
    fn the_identity_is_bit_exact() {
        let bounds = Bounds::new(point(px(-0.0), px(3.25)), size(px(7.5), px(2.)));
        let mapped = LayerTransform::IDENTITY.apply_bounds(bounds);
        assert_eq!(
            f32::from(mapped.origin.x).to_bits(),
            f32::from(bounds.origin.x).to_bits()
        );
        assert_eq!(mapped, bounds);
    }

    fn sp(v: f32) -> ScaledPixels {
        ScaledPixels(v)
    }

    fn rect(x: f32, y: f32) -> Bounds<ScaledPixels> {
        Bounds {
            origin: point(sp(x), sp(y)),
            size: size(sp(100.), sp(100.)),
        }
    }

    fn quad_at(x: f32, y: f32) -> Quad {
        Quad {
            bounds: rect(x, y),
            content_mask: ContentMask {
                bounds: Bounds {
                    origin: point(sp(0.), sp(0.)),
                    size: size(sp(1000.), sp(1000.)),
                },
            },
            ..Quad::default()
        }
    }

    fn group(is_start: bool, filters: SmallVec<[ScaledFilter; 4]>) -> FilterBoundary {
        FilterBoundary {
            order: 0,
            bounds: rect(0., 0.),
            content_mask: ContentMask {
                bounds: Bounds {
                    origin: point(sp(0.), sp(0.)),
                    size: size(sp(1000.), sp(1000.)),
                },
            },
            corner_radii: Corners::default(),
            filters,
            opacity: 0.5,
            is_start,
        }
    }

    fn kinds(scene: &mut Scene) -> Vec<&'static str> {
        scene.finish();
        scene
            .batches()
            .map(|batch| match batch {
                PrimitiveBatch::Quads(_) => "quad",
                PrimitiveBatch::FilterBoundary(ix) if scene.filter_boundaries[ix].is_start => {
                    "start"
                }
                PrimitiveBatch::FilterBoundary(_) => "end",
                _ => "other",
            })
            .collect()
    }

    /// A group-opacity group owns everything painted inside it, even content
    /// that does not overlap its start marker (a row falling in from above its
    /// slot), and its end marker carries the union the renderer composites.
    #[test]
    fn an_opacity_group_owns_detached_children_and_ends_with_their_extent() {
        let mut scene = Scene::default();
        scene.insert_primitive(quad_at(0., 0.));
        scene.insert_primitive(group(true, SmallVec::new()));
        scene.insert_primitive(quad_at(300., 300.));
        scene.insert_primitive(quad_at(200., 250.));
        scene.insert_primitive(group(false, SmallVec::new()));
        scene.insert_primitive(quad_at(0., 0.));
        assert_eq!(
            kinds(&mut scene),
            ["quad", "start", "quad", "end", "quad"],
            "the detached children are inside the group"
        );
        let end = scene
            .filter_boundaries
            .iter()
            .find(|boundary| !boundary.is_start)
            .expect("end marker");
        assert_eq!(end.bounds, rect(200., 250.).union(&rect(300., 300.)));
    }

    /// A blur group keeps upstream gpui's semantics (content outside its
    /// bounds is not pulled in, its end marker keeps its own bounds).
    #[test]
    fn a_blur_group_is_unchanged() {
        let mut scene = Scene::default();
        let blur: SmallVec<[ScaledFilter; 4]> = smallvec::smallvec![ScaledFilter::Blur(sp(8.))];
        scene.insert_primitive(group(true, blur.clone()));
        scene.insert_primitive(quad_at(0., 0.));
        scene.insert_primitive(group(false, blur));
        let end = scene
            .filter_boundaries
            .iter()
            .find(|boundary| !boundary.is_start)
            .expect("end marker");
        assert_eq!(end.bounds, rect(0., 0.));
    }
}

#[cfg(all(test, feature = "gallery", target_os = "macos"))]
mod headless;

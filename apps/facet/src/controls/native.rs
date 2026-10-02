//! Small, bounded vector marks for controls whose geometry cannot be expressed
//! as ordinary rectangular `Div`s. Interaction and layout stay in GPUI's
//! standard elements; this element only paints closed path data.

use gpui::{
    App, Bounds, ContentMask, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, PathBuilder, Pixels, Point, Refineable, Style, StyleRefinement, Styled,
    Window, point, px, size,
};
use smallvec::SmallVec;

const MAX_PATHS: usize = 16;
const MAX_POLYGONS: usize = 8192;
const MAX_POINTS: usize = 24;
const MAX_RECTS: usize = 16_384;

#[derive(Clone, Copy, Debug, PartialEq)]
struct LocalPoint {
    x: f32,
    y: f32,
}

impl From<(f32, f32)> for LocalPoint {
    fn from((x, y): (f32, f32)) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Debug)]
enum PathData {
    Fill {
        polygons: SmallVec<[SmallVec<[LocalPoint; 4]>; 4]>,
        color: Hsla,
    },
    Stroke {
        lines: SmallVec<[SmallVec<[LocalPoint; 4]>; 4]>,
        width: f32,
        closed: bool,
        color: Hsla,
    },
}

/// A tiny typed path mark. Points are in the element's logical-pixel viewbox;
/// paint is clipped to the element bounds and follows GPUI's active layer
/// transform and opacity.
#[derive(Clone, Debug)]
pub(crate) struct NativePaths {
    style: StyleRefinement,
    viewbox: (f32, f32),
    paths: SmallVec<[PathData; 4]>,
    polygon_count: usize,
}

pub(crate) fn native_paths(width: f32, height: f32) -> NativePaths {
    NativePaths {
        style: StyleRefinement::default(),
        viewbox: (width.max(0.0), height.max(0.0)),
        paths: SmallVec::new(),
        polygon_count: 0,
    }
}

#[derive(Clone, Copy, Debug)]
struct RectData {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    color: Hsla,
}

/// Bounded, colored axis-aligned fills for measured controls with many
/// differently faded rectangles (for example, a release history). This keeps
/// the renderer on native quads without allocating one layout node per tick.
#[derive(Clone, Debug)]
pub(crate) struct NativeRects {
    style: StyleRefinement,
    viewbox: (f32, f32),
    rects: SmallVec<[RectData; 32]>,
}

pub(crate) fn native_rects(width: f32, height: f32) -> NativeRects {
    NativeRects {
        style: StyleRefinement::default(),
        viewbox: (width.max(0.0), height.max(0.0)),
        rects: SmallVec::new(),
    }
}

impl NativeRects {
    pub(crate) fn add(&mut self, x: f32, y: f32, width: f32, height: f32, color: Hsla) {
        if width <= 0.0 || height <= 0.0 || color.alpha <= 0.0 {
            return;
        }
        assert!(
            self.rects.len() < MAX_RECTS,
            "native control rectangle cap exceeded"
        );
        self.rects.push(RectData {
            x,
            y,
            width,
            height,
            color,
        });
    }
}

impl Styled for NativeRects {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for NativeRects {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for NativeRects {
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
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let (view_w, view_h) = self.viewbox;
        if view_w <= 0.0
            || view_h <= 0.0
            || bounds.size.width <= px(0.0)
            || bounds.size.height <= px(0.0)
        {
            return;
        }
        let sx = f32::from(bounds.size.width) / view_w;
        let sy = f32::from(bounds.size.height) / view_h;
        let x0 = f32::from(bounds.origin.x);
        let y0 = f32::from(bounds.origin.y);
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for rect in &self.rects {
                let bounds = Bounds::new(
                    point(px(x0 + rect.x * sx), px(y0 + rect.y * sy)),
                    size(px(rect.width * sx), px(rect.height * sy)),
                );
                window.paint_quad(gpui::fill(bounds, rect.color));
            }
        });
    }
}

impl NativePaths {
    /// Add a filled polygon of at most twenty-four points. Adjacent polygons
    /// with the same color share one GPU path.
    pub(crate) fn fill(mut self, points: &[(f32, f32)], color: Hsla) -> Self {
        self.add_fill(points, color);
        self
    }

    pub(crate) fn add_fill(&mut self, points: &[(f32, f32)], color: Hsla) {
        self.polygon_count += 1;
        assert!(
            self.polygon_count <= MAX_POLYGONS,
            "native control polygon cap exceeded"
        );
        assert!(
            points.len() <= MAX_POINTS,
            "native control path exceeds point cap"
        );
        let points = points.iter().copied().map(Into::into).collect();
        if let Some(PathData::Fill { polygons, .. }) = self.paths.last_mut().filter(
            |path| matches!(path, PathData::Fill { color: current, .. } if *current == color),
        ) {
            polygons.push(points);
        } else {
            self.push(PathData::Fill {
                polygons: smallvec::smallvec![points],
                color,
            });
        }
    }

    /// Add an open or closed stroked path of at most twelve points.
    pub(crate) fn stroke(
        mut self,
        points: &[(f32, f32)],
        width: f32,
        closed: bool,
        color: Hsla,
    ) -> Self {
        self.add_stroke(points, width, closed, color);
        self
    }

    pub(crate) fn add_stroke(
        &mut self,
        points: &[(f32, f32)],
        width: f32,
        closed: bool,
        color: Hsla,
    ) {
        self.polygon_count += 1;
        assert!(
            self.polygon_count <= MAX_POLYGONS,
            "native control polygon cap exceeded"
        );
        assert!(
            points.len() <= MAX_POINTS,
            "native control path exceeds point cap"
        );
        let points = points.iter().copied().map(Into::into).collect();
        let width = width.max(0.0);
        if let Some(PathData::Stroke { lines, .. }) = self.paths.last_mut().filter(|path| {
            matches!(path, PathData::Stroke { width: current_width, closed: current_closed, color: current_color, .. }
                if *current_width == width && *current_closed == closed && *current_color == color)
        }) {
            lines.push(points);
        } else {
            self.push(PathData::Stroke { lines: smallvec::smallvec![points], width, closed, color });
        }
    }

    fn push(&mut self, path: PathData) {
        assert!(
            self.paths.len() < MAX_PATHS,
            "native control path exceeds path cap"
        );
        self.paths.push(path);
    }
}

impl Styled for NativePaths {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

impl IntoElement for NativePaths {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for NativePaths {
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
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let (view_w, view_h) = self.viewbox;
        if view_w <= 0.0
            || view_h <= 0.0
            || bounds.size.width <= px(0.0)
            || bounds.size.height <= px(0.0)
        {
            return;
        }
        let sx = f32::from(bounds.size.width) / view_w;
        let sy = f32::from(bounds.size.height) / view_h;
        let at = |p: LocalPoint| {
            point(
                px(f32::from(bounds.origin.x) + p.x * sx),
                px(f32::from(bounds.origin.y) + p.y * sy),
            )
        };
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for path in &self.paths {
                match path {
                    PathData::Fill { polygons, color } if color.alpha > 0.0 => {
                        let mut builder = PathBuilder::fill();
                        for polygon in polygons.iter().filter(|polygon| polygon.len() >= 3) {
                            let points: SmallVec<[Point<Pixels>; 24]> =
                                polygon.iter().copied().map(at).collect();
                            builder.add_polygon(&points, true);
                        }
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, *color);
                        }
                    }
                    PathData::Stroke {
                        lines,
                        width,
                        closed,
                        color,
                    } if *width > 0.0 && color.alpha > 0.0 => {
                        let scale = (sx + sy) * 0.5;
                        let mut builder = PathBuilder::stroke(px(width * scale));
                        for line in lines.iter().filter(|line| line.len() >= 2) {
                            let points: SmallVec<[Point<Pixels>; 24]> =
                                line.iter().copied().map(at).collect();
                            builder.add_polygon(&points, *closed);
                        }
                        if let Ok(path) = builder.build() {
                            window.paint_path(path, *color);
                        }
                    }
                    _ => {}
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_PATHS, MAX_POINTS, MAX_POLYGONS, native_paths};
    use gpui::Hsla;

    #[test]
    fn control_paths_have_small_fixed_complexity_limits() {
        assert_eq!((MAX_PATHS, MAX_POLYGONS, MAX_POINTS), (16, 8192, 24));
        let mark = native_paths(12.0, 12.0)
            .fill(&[(0.0, 0.0), (12.0, 0.0), (6.0, 12.0)], Hsla::default())
            .fill(&[(0.0, 12.0), (12.0, 12.0), (6.0, 0.0)], Hsla::default());
        assert_eq!(mark.paths.len(), 1, "same-color polygons are batched");
        assert_eq!(mark.polygon_count, 2);
    }

    #[test]
    #[should_panic(expected = "native control path exceeds point cap")]
    fn paths_reject_unbounded_point_lists() {
        let points = [(0.0, 0.0); MAX_POINTS + 1];
        let _ = native_paths(12.0, 12.0).fill(&points, Hsla::default());
    }
}

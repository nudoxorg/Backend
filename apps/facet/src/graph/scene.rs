//! What the renderer derives once per laid-out world: the picking grid, the
//! edges inside each module, per-package counts, and the camera framings
//! the prototype names (world, focus, neighbourhood).

use super::camera::{View, smooth};
use super::layout::{Box2, Layout, core};
use super::model::{Kind, NodeId, World};
use crate::data::spatial::Aabb;
use crate::motion::Camera;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

/// One frame's full camera projection, shared by drawing, culling and picking.
/// Card room constrains foreground layout without changing this coordinate system.
#[derive(Clone, Copy)]
pub struct Projection {
    view: View,
    cam: Camera,
    world: Box2,
}

impl Projection {
    /// Horizontal world-to-window projection.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn x(&self, x: f32) -> f32 {
        self.view.x + ((f64::from(x) - self.cam.x) * self.scale()) as f32 + self.view.w * 0.5
    }
    /// Vertical world-to-window projection.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn y(&self, y: f32) -> f32 {
        self.view.y + ((f64::from(y) - self.cam.y) * self.scale()) as f32 + self.view.h * 0.5
    }
    /// Full camera scale, in pixels per world unit.
    #[must_use]
    pub fn scale(&self) -> f64 {
        self.view.k(&self.cam)
    }
    /// Window-to-world inverse, used by picking.
    #[must_use]
    pub fn to_world(&self, x: f32, y: f32) -> (f64, f64) {
        self.view.to_world(&self.cam, x, y)
    }
    /// Whether a world box intersects the viewport.
    #[must_use]
    pub fn visible(&self, bounds: &Box2) -> bool {
        bounds.overlaps(&self.world)
    }
    /// Conservative curve culling with room for its antialiased stroke.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn visible_with_margin(&self, bounds: &Box2, pixels: f32) -> bool {
        let margin = f64::from(pixels) / self.scale();
        f64::from(bounds.x1) + margin >= f64::from(self.world.x0)
            && f64::from(bounds.x0) - margin <= f64::from(self.world.x1)
            && f64::from(bounds.y1) + margin >= f64::from(self.world.y0)
            && f64::from(bounds.y0) - margin <= f64::from(self.world.y1)
    }

    /// Whether a window point belongs to this canvas.
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.view.x
            && y >= self.view.y
            && x <= self.view.x + self.view.w
            && y <= self.view.y + self.view.h
    }
}

/// Screen-space symbol shape used by both drawing and picking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// A type or trait.
    Diamond,
    /// A star, callable, or member.
    Square,
}

/// Shared screen-space symbol geometry; zoom never enlarges its hit target in
/// world units. Small symbols retain a ten-pixel target around their centre.
#[derive(Clone, Copy)]
pub struct Glyph {
    /// Semantic zoom core before its drawing cap.
    pub core: f32,
    /// Diamond radius or square half-side, in window pixels.
    pub radius: f32,
    /// Actual silhouette.
    pub shape: Shape,
}

impl Glyph {
    /// The painted silhouette at a displacement from the symbol centre.
    #[must_use]
    pub fn contains(self, dx: f32, dy: f32) -> bool {
        match self.shape {
            Shape::Diamond => dx.abs() + dy.abs() <= self.radius,
            Shape::Square => dx.abs() <= self.radius && dy.abs() <= self.radius,
        }
    }
    /// The silhouette plus the minimum centre target.
    #[must_use]
    pub fn hit(self, dx: f32, dy: f32) -> bool {
        self.contains(dx, dy) || dx.hypot(dy) <= 10.0
    }
}

/// A territory under a point: the package, and the module when inside one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terr {
    /// Package.
    pub pkg: u32,
    /// Module.
    pub module: Option<u32>,
}

/// A camera-independent bundled edge and its conservative curve bounds.
pub(crate) const ROUTE_SAMPLES: usize = 33;

pub(crate) struct HoverEdge {
    pub other: NodeId,
    pub incoming: bool,
    pub points: [[f32; 2]; ROUTE_SAMPLES],
    pub bounds: Box2,
}

/// A hovered symbol's immutable, cached neighbourhood.
pub(crate) struct Neighbourhood {
    pub node: NodeId,

    pub lit: Vec<NodeId>,
    pub edges: Vec<HoverEdge>,
    pub bundles: Vec<HoverBundle>,
    leaves: BoundsIndex<usize>,
    visible_lit: BoundsIndex<NodeId>,
}
/// One shared semantic route represents all its exact member relations.
pub(crate) struct HoverBundle {
    pub route: HoverEdge,
    pub count: usize,
}
impl Neighbourhood {
    pub(crate) fn visit_leaves(&self, projection: &Projection, emit: impl FnMut(usize)) -> usize {
        self.leaves.visit(expanded(projection, 32.0), emit)
    }
    pub(crate) fn visit_lit(&self, projection: &Projection, emit: impl FnMut(NodeId)) -> usize {
        self.visible_lit.visit(expanded(projection, 14.0), emit)
    }
}
fn expanded(projection: &Projection, pixels: f32) -> Box2 {
    #[allow(clippy::cast_possible_truncation)]
    let r = (f64::from(pixels) / projection.scale()) as f32;
    let b = projection.world;
    Box2 {
        x0: b.x0 - r,
        y0: b.y0 - r,
        x1: b.x1 + r,
        y1: b.y1 + r,
    }
}

/// A single bow, with strictly monotone chord progress. Equal normal offsets
/// at both cubic controls prevent accidental inflections/backtracking through
/// hierarchy hubs. Context chooses the bow's side and amount deterministically.
fn relation_curve(
    a: [f32; 2],
    b: [f32; 2],
    context: [f32; 2],
) -> ([[f32; 2]; ROUTE_SAMPLES], Box2) {
    let d = [b[0] - a[0], b[1] - a[1]];
    let length = d[0].hypot(d[1]);
    if length == 0.0 {
        return ([a; ROUTE_SAMPLES], Box2::around(&[a]));
    }
    let n = if length > 1e-6 {
        [-d[1] / length, d[0] / length]
    } else {
        [0.0, 0.0]
    };
    let signed =
        (context[0] - (a[0] + b[0]) * 0.5) * n[0] + (context[1] - (a[1] + b[1]) * 0.5) * n[1];
    let bow = (signed * 0.22).clamp(-length * 0.16, length * 0.16);
    let c1 = [
        a[0] + d[0] / 3.0 + n[0] * bow,
        a[1] + d[1] / 3.0 + n[1] * bow,
    ];
    let c2 = [
        a[0] + d[0] * 2.0 / 3.0 + n[0] * bow,
        a[1] + d[1] * 2.0 / 3.0 + n[1] * bow,
    ];
    let mut points = [[0.0; 2]; ROUTE_SAMPLES];
    for step in 0..ROUTE_SAMPLES {
        let t = step as f32 / (ROUTE_SAMPLES - 1) as f32;
        let u = 1.0 - t;
        points[step] = [
            u * u * u * a[0] + 3.0 * u * u * t * c1[0] + 3.0 * u * t * t * c2[0] + t * t * t * b[0],
            u * u * u * a[1] + 3.0 * u * u * t * c1[1] + 3.0 * u * t * t * c2[1] + t * t * t * b[1],
        ];
    }
    (points, Box2::around(&[a, c1, c2, b]))
}

/// Each point belongs to exactly one cell, so a pointer query needs no
/// world-sized `seen` allocation. Sparse, far-away cells cost no work.
struct PickingGrid {
    points: Vec<[f32; 2]>,
    cells: HashMap<(i32, i32), Vec<NodeId>>,
}

impl PickingGrid {
    #[allow(clippy::cast_possible_truncation)]
    fn cell(x: f32, y: f32) -> (i32, i32) {
        ((x / 3.0).floor() as i32, (y / 3.0).floor() as i32)
    }
    #[allow(clippy::cast_possible_truncation)]
    fn build(points: Vec<[f32; 2]>) -> Self {
        let mut cells: HashMap<_, Vec<_>> = HashMap::new();
        for (i, p) in points.iter().enumerate() {
            cells
                .entry(Self::cell(p[0], p[1]))
                .or_default()
                .push(i as NodeId);
        }
        Self { points, cells }
    }
    fn visit(&self, bounds: &Aabb, mut emit: impl FnMut(NodeId)) {
        let ((x0, y0), (x1, y1)) = (
            Self::cell(bounds.x0, bounds.y0),
            Self::cell(bounds.x1, bounds.y1),
        );
        let mut visit = |ids: &[NodeId]| {
            for &id in ids {
                let p = self.points[id as usize];
                if p[0] >= bounds.x0 && p[0] <= bounds.x1 && p[1] >= bounds.y0 && p[1] <= bounds.y1
                {
                    emit(id);
                }
            }
        };
        let area =
            (i64::from(x1) - i64::from(x0) + 1).max(0) * (i64::from(y1) - i64::from(y0) + 1).max(0);
        if area > self.cells.len() as i64 * 2 {
            for (&(x, y), ids) in &self.cells {
                if x >= x0 && x <= x1 && y >= y0 && y <= y1 {
                    visit(ids);
                }
            }
        } else {
            for y in y0..=y1 {
                for x in x0..=x1 {
                    if let Some(ids) = self.cells.get(&(x, y)) {
                        visit(ids);
                    }
                }
            }
        }
    }
    #[cfg(test)]
    fn query(&self, bounds: &Aabb) -> Vec<NodeId> {
        let mut out = Vec::new();
        self.visit(bounds, |id| out.push(id));
        out.sort_unstable();
        out
    }
}

/// Immutable bounding-volume hierarchy. Leaves retain semantic payloads;
/// queries allocate nothing and test complete occupied geometry, not centres.
struct BoundsIndex<T> {
    entries: Vec<(Box2, T)>,
    branches: Vec<Branch>,
}
struct Branch {
    bounds: Box2,
    start: usize,
    end: usize,
    children: Option<(usize, usize)>,
}
impl<T: Copy> BoundsIndex<T> {
    fn new(mut entries: Vec<(Box2, T)>) -> Self {
        fn split<T>(entries: &mut [(Box2, T)], offset: usize, branches: &mut Vec<Branch>) -> usize {
            let bounds = entries.iter().fold(Box2::EMPTY, |b, e| b.union(e.0));
            let at = branches.len();
            branches.push(Branch {
                bounds,
                start: offset,
                end: offset + entries.len(),
                children: None,
            });
            if entries.len() > 16 {
                let x = bounds.x1 - bounds.x0 >= bounds.y1 - bounds.y0;
                let mid = entries.len() / 2;
                entries.select_nth_unstable_by(mid, |a, b| {
                    let centre = |r: Box2| {
                        if x {
                            r.x0 * 0.5 + r.x1 * 0.5
                        } else {
                            r.y0 * 0.5 + r.y1 * 0.5
                        }
                    };
                    centre(a.0).total_cmp(&centre(b.0))
                });
                let (left, right) = entries.split_at_mut(mid);
                let a = split(left, offset, branches);
                let b = split(right, offset + mid, branches);
                branches[at].children = Some((a, b));
            }
            at
        }
        let mut branches = Vec::new();
        if !entries.is_empty() {
            split(&mut entries, 0, &mut branches);
        }
        Self { entries, branches }
    }
    /// Returns tested payload bounds, allowing structural work assertions.
    fn visit(&self, query: Box2, mut emit: impl FnMut(T)) -> usize {
        fn contains(outer: Box2, inner: Box2) -> bool {
            outer.x0 <= inner.x0
                && outer.y0 <= inner.y0
                && outer.x1 >= inner.x1
                && outer.y1 >= inner.y1
        }
        fn walk<T: Copy>(
            index: &BoundsIndex<T>,
            at: usize,
            query: Box2,
            emit: &mut impl FnMut(T),
        ) -> usize {
            let node = &index.branches[at];
            if !node.bounds.overlaps(&query) {
                return 0;
            }
            if contains(query, node.bounds) {
                for &(_, value) in &index.entries[node.start..node.end] {
                    emit(value);
                }
                return node.end - node.start;
            }
            if let Some((left, right)) = node.children {
                return walk(index, left, query, emit) + walk(index, right, query, emit);
            }
            for &(bounds, value) in &index.entries[node.start..node.end] {
                if bounds.overlaps(&query) {
                    emit(value);
                }
            }
            node.end - node.start
        }
        if self.branches.is_empty() {
            0
        } else {
            walk(self, 0, query, &mut emit)
        }
    }
}

/// A laid-out world, ready to draw and pick.
pub struct Scene {
    /// The world.
    pub world: Arc<World>,
    /// Its positions.
    pub layout: Arc<Layout>,
    /// Every node as a point, for picking.
    points: PickingGrid,
    neighbourhood: Mutex<Option<Arc<Neighbourhood>>>,
    visible_items: BoundsIndex<NodeId>,
    visible_inner: BoundsIndex<(NodeId, NodeId)>,
    largest_package: f32,
    largest_module: f32,
    /// Per module: the item edges that stay inside it.
    pub inner: Vec<Vec<(NodeId, NodeId)>>,
    /// Per package: its items.
    pub pkg_size: Vec<u32>,
    /// Per package: items your code reaches.
    pub pkg_reach: Vec<u32>,
    /// The standard library's package (its edges are not drawn).
    pub std_pkg: Option<u32>,
}

impl Scene {
    /// Derives everything the renderer needs.
    #[must_use]
    pub fn new(world: Arc<World>, layout: Arc<Layout>) -> Self {
        let points = PickingGrid::build(
            (0..world.len())
                .map(|i| [layout.x[i], layout.y[i]])
                .collect(),
        );
        let mut inner = vec![Vec::new(); world.modules.len()];
        for e in &world.item_edges {
            let m = world.nodes[e.from as usize].module;
            if m == world.nodes[e.to as usize].module {
                inner[m as usize].push((e.from, e.to));
            }
        }
        let visible_items = BoundsIndex::new(
            world
                .items
                .iter()
                .map(|&i| {
                    let at = i as usize;
                    let (x, y, r) = (layout.x[at], layout.y[at], layout.r[at]);
                    (
                        Box2 {
                            x0: x - r,
                            y0: y - r,
                            x1: x + r,
                            y1: y + r,
                        },
                        i,
                    )
                })
                .collect(),
        );
        let visible_inner = BoundsIndex::new(
            inner
                .iter()
                .flatten()
                .map(|&(a, b)| {
                    (
                        Box2::around(&[
                            [layout.x[a as usize], layout.y[a as usize]],
                            [layout.x[b as usize], layout.y[b as usize]],
                        ]),
                        (a, b),
                    )
                })
                .collect(),
        );
        let n_pkg = world.packages.len();
        let mut pkg_size = vec![0_u32; n_pkg];
        let mut pkg_reach = vec![0_u32; n_pkg];
        for &i in &world.items {
            let p = world.nodes[i as usize].pkg as usize;
            pkg_size[p] += 1;
            if world.yours_in[i as usize] > 0 {
                pkg_reach[p] += 1;
            }
        }
        #[allow(clippy::cast_possible_truncation)]
        let std_pkg = world
            .packages
            .iter()
            .position(|p| p.name.as_ref() == "std")
            .map(|p| p as u32);
        let largest_package = layout.packages.iter().map(|t| t.r).fold(0.0, f32::max);
        let largest_module = layout.modules.iter().map(|t| t.r).fold(0.0, f32::max);
        Self {
            world,
            layout,
            points,
            neighbourhood: Mutex::new(None),
            visible_items,
            visible_inner,
            largest_package,
            largest_module,
            inner,
            pkg_size,
            pkg_reach,
            std_pkg,
        }
    }

    /// Scene-wide semantic zoom measures; panning cannot change level merely
    /// because a large territory crossed a culling boundary.
    pub(crate) fn level_scales(&self, scale: f64) -> (f64, f64) {
        (
            f64::from(self.largest_package) * scale,
            f64::from(self.largest_module) * scale,
        )
    }

    /// Visits only intersecting occupied item bounds (including member shells).
    pub(crate) fn visit_items(
        &self,
        projection: &Projection,
        pixels: f32,
        emit: impl FnMut(NodeId),
    ) -> usize {
        let r = pixels as f64 / projection.scale();
        let b = projection.world;
        #[allow(clippy::cast_possible_truncation)]
        let query = Box2 {
            x0: b.x0 - r as f32,
            y0: b.y0 - r as f32,
            x1: b.x1 + r as f32,
            y1: b.y1 + r as f32,
        };
        self.visible_items.visit(query, emit)
    }

    /// Segment bounds preserve crossing edges whose endpoints are both outside.
    pub(crate) fn visit_inner(
        &self,
        projection: &Projection,
        emit: impl FnMut((NodeId, NodeId)),
    ) -> usize {
        self.visible_inner.visit(projection.world, emit)
    }

    /// Hover tracks sample the same target many times; derive its adjacency
    /// and sorted membership once. A single-entry cache cannot grow with the
    /// world's size or with a long pointer sweep.
    pub(crate) fn neighbourhood(&self, node: NodeId) -> Arc<Neighbourhood> {
        let mut cached = self
            .neighbourhood
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(neighbours) = &*cached
            && neighbours.node == node
        {
            return neighbours.clone();
        }
        let (outs, ins) = self.world.neighbours(node);
        let mut lit = vec![node];
        lit.extend(outs.iter().chain(ins.iter()).map(|&(i, _)| i));
        if let Some(parent) = self.world.node(node).parent {
            lit.push(parent);
        }
        lit.sort_unstable();
        lit.dedup();
        let edges: Vec<_> = outs
            .iter()
            .map(|&(j, _)| self.hover_edge(node, j, false))
            .chain(ins.iter().map(|&(j, _)| self.hover_edge(j, node, true)))
            .collect();
        let at = self.world.node(node);
        let mut groups: BTreeMap<(u32, Option<u32>, bool, bool), (NodeId, usize)> = BTreeMap::new();
        for edge in &edges {
            let other = self.world.node(edge.other);
            let key = (
                other.pkg,
                (other.pkg == at.pkg).then_some(other.module),
                edge.incoming,
                self.world.yours(edge.other),
            );
            let value = groups.entry(key).or_insert((edge.other, 0));
            value.1 += 1;
        }
        let source = [self.layout.x[node as usize], self.layout.y[node as usize]];
        let context = &self.layout.packages[at.pkg as usize];
        let bundles = groups
            .into_iter()
            .map(|((pkg, module, incoming, _), (other, count))| {
                let hub = module.map_or(&self.layout.packages[pkg as usize], |m| {
                    &self.layout.modules[m as usize]
                });
                let remote = [hub.x, hub.y];
                let (a, b) = if incoming {
                    (remote, source)
                } else {
                    (source, remote)
                };
                let (points, bounds) = relation_curve(a, b, [context.x, context.y]);
                HoverBundle {
                    route: HoverEdge {
                        other,
                        incoming,
                        points,
                        bounds,
                    },
                    count,
                }
            })
            .collect();
        let point_bounds =
            |id: NodeId| Box2::around(&[[self.layout.x[id as usize], self.layout.y[id as usize]]]);
        let leaves = BoundsIndex::new(
            edges
                .iter()
                .enumerate()
                .map(|(i, e)| (point_bounds(e.other), i))
                .collect(),
        );
        let visible_lit = BoundsIndex::new(lit.iter().map(|&id| (point_bounds(id), id)).collect());
        let neighbours = Arc::new(Neighbourhood {
            node,
            lit,
            edges,
            bundles,
            leaves,
            visible_lit,
        });
        *cached = Some(neighbours.clone());
        neighbours
    }

    /// Cache affine world-space bundles once per hovered target. Bounds cover
    /// the control polygon, not just samples, so culling never loses a curve
    /// that enters the view between samples.
    #[allow(clippy::cast_precision_loss)]
    fn hover_edge(&self, from: NodeId, to: NodeId, incoming: bool) -> HoverEdge {
        let (world, lay) = (&self.world, &self.layout);
        let (a, b) = (world.node(from), world.node(to));
        let context = if a.pkg == b.pkg {
            let m = &lay.modules[b.module as usize];
            [m.x, m.y]
        } else {
            let p = &lay.packages[a.pkg as usize];
            [p.x, p.y]
        };
        let (points, bounds) = relation_curve(
            [lay.x[from as usize], lay.y[from as usize]],
            [lay.x[to as usize], lay.y[to as usize]],
            context,
        );
        HoverEdge {
            other: if incoming { from } else { to },
            incoming,
            points,
            bounds,
        }
    }

    /// The canonical projection for drawing and pointer picking.
    #[must_use]
    pub fn projection(&self, view: View, cam: Camera) -> Projection {
        Projection {
            view,
            cam,
            world: view.world_box(&cam),
        }
    }

    /// Exact free foreground room, shared by prism layout and focus framing.
    /// The full camera projection remains unchanged.
    #[must_use]
    pub fn free_view(view: &View, occupied: Option<gpui::Bounds<gpui::Pixels>>) -> View {
        let Some(card) = occupied else {
            return View {
                w: (view.w - Self::card_room(view)).max(1.0),
                ..*view
            };
        };
        let (right, bottom) = (view.x + view.w, view.y + view.h);
        let (x0, y0, x1, y1) = (
            f32::from(card.origin.x).max(view.x),
            f32::from(card.origin.y).max(view.y),
            f32::from(card.right()).min(right),
            f32::from(card.bottom()).min(bottom),
        );
        if x1 <= x0 || y1 <= y0 {
            return *view;
        }
        // These clear strips are derived from the real occupied rectangle.
        // Wide side room preserves the normal two-column reading layout;
        // constrained side room competes with the larger area above or below.
        let margin = 12.0;
        let rooms = [
            View {
                w: (x0 - margin - view.x).max(0.0),
                ..*view
            },
            View {
                x: (x1 + margin).min(right),
                w: (right - x1 - margin).max(0.0),
                ..*view
            },
            View {
                h: (y0 - margin - view.y).max(0.0),
                ..*view
            },
            View {
                y: (y1 + margin).min(bottom),
                h: (bottom - y1 - margin).max(0.0),
                ..*view
            },
        ];
        let wide_side = rooms[..2]
            .iter()
            .filter(|r| !Self::prism_narrow(r))
            .max_by(|a, b| a.w.total_cmp(&b.w));
        let mut chosen = wide_side.copied().unwrap_or_else(|| {
            *rooms
                .iter()
                .max_by(|a, b| (a.w * a.h).total_cmp(&(b.w * b.h)))
                .expect("four foreground strips")
        });
        chosen.w = chosen.w.max(1.0);
        chosen.h = chosen.h.max(1.0);
        chosen
    }

    /// Whether the foreground room fits two comfortably spaced columns.
    #[must_use]
    pub fn prism_narrow(room: &View) -> bool {
        room.w < 900.0
    }

    /// Focus anchor in window coordinates, shared by camera and prism layout.
    /// A merged column leaves the source on its left with clear strand fanout.
    #[must_use]
    pub fn focus_anchor(room: &View) -> (f32, f32) {
        (
            room.x + room.w * if Self::prism_narrow(room) { 0.28 } else { 0.5 },
            room.y + room.h * 0.5,
        )
    }

    /// The whole world, framed (app.js `worldCam`).
    #[must_use]
    pub fn world_cam(&self, view: &View) -> Camera {
        view.frame(self.layout.bounds, 1.06)
    }

    /// The widest the camera may go.
    #[must_use]
    pub fn max_w(&self, view: &View) -> f64 {
        view.fit_w(self.layout.bounds, 1.06) * 1.6
    }

    /// The symbol at reading scale, placed in the middle of the space the
    /// focus card leaves (app.js `focusCam`; `card` px on the right).
    #[must_use]
    pub fn focus_cam(&self, view: &View, i: NodeId, card: f32) -> Camera {
        let t = self.world.top(i) as usize;
        let w = (f64::from(view.w) / 26.0).max(f64::from(self.layout.r[t]) * 7.0);
        Camera::new(
            f64::from(self.layout.x[i as usize]) + f64::from(card / 2.0) * w / f64::from(view.w),
            f64::from(self.layout.y[i as usize]),
            w,
        )
    }

    /// The card width the focus leaves on the right at this view width.
    #[must_use]
    pub fn card_room(view: &View) -> f32 {
        if view.w > 900.0 { 380.0 } else { 0.0 }
    }

    /// The symbol with its neighbourhood, trimmed to the nearest 85 % so one
    /// far edge cannot zoom the world out (app.js `frameOf`).
    #[must_use]
    pub fn frame_of(&self, view: &View, i: NodeId) -> Camera {
        let (x, y) = (self.layout.x[i as usize], self.layout.y[i as usize]);
        let (outs, ins) = self.world.neighbours(i);
        let mut pts: Vec<(f32, f32)> = vec![(x, y)];
        pts.extend(
            outs.iter()
                .chain(&ins)
                .map(|&(j, _)| (self.layout.x[j as usize], self.layout.y[j as usize])),
        );
        let mut d: Vec<f32> = pts.iter().map(|p| (p.0 - x).hypot(p.1 - y)).collect();
        d.sort_by(f32::total_cmp);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let lim = d[((d.len() - 1) as f32 * 0.85).floor() as usize];
        let keep: Vec<(f32, f32)> = pts
            .into_iter()
            .filter(|p| (p.0 - x).hypot(p.1 - y) <= lim + 1e-6)
            .collect();
        let r = (self.layout.r[self.world.top(i) as usize] * 3.2).max(5.0);
        let mut b = keep.iter().fold(Box2::EMPTY, |b, p| b.with(p.0, p.1));
        b = b.with(x - r, y - r).with(x + r, y + r);
        let world_w = view.fit_w(self.layout.bounds, 1.06);
        let w = view.fit_w(b, 1.35).min(world_w * 0.7);
        let aspect = f64::from(view.w) / f64::from(view.h.max(1.0));
        let half = f64::from((x - b.x0).max(b.x1 - x))
            .max(f64::from(y - b.y0) * aspect)
            .max(f64::from(b.y1 - y) * aspect)
            * 2.0
            * 1.2;
        Camera::new(f64::from(x), f64::from(y), w.max(half).min(world_w * 0.7))
    }

    /// How visible a member is at `k` px/unit (it fades in with its item's
    /// on-screen size, 12 → 26 px).
    #[must_use]
    pub fn member_alpha(&self, item: NodeId, k: f64) -> f64 {
        smooth(f64::from(self.layout.r[item as usize]) * k, 12.0, 26.0)
    }

    /// The node under window point `(px, py)`: the nearest within 10 px
    /// (members only once they are visible), app.js `pick`.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn pick(&self, view: &View, cam: &Camera, px: f32, py: f32) -> Option<NodeId> {
        let projected = self.projection(*view, *cam);
        if !projected.contains(px, py) {
            return None;
        }
        let k = projected.scale();
        let (x, y) = projected.to_world(px, py);
        let rad = Self::hit_radius(k) / k;
        let q = Aabb {
            x0: (x - rad) as f32,
            y0: (y - rad) as f32,
            x1: (x + rad) as f32 + 1e-3,
            y1: (y + rad) as f32 + 1e-3,
        };
        let mut best: Option<(f64, NodeId)> = None;
        self.points.visit(&q, |i| {
            let node = &self.world.nodes[i as usize];
            if node.orphan {
                return;
            }
            if let Some(p) = node.parent {
                if self.member_alpha(p, k) < 0.5 {
                    return;
                }
            }
            let dx = ((f64::from(self.layout.x[i as usize]) - x) * k) as f32;
            let dy = ((f64::from(self.layout.y[i as usize]) - y) * k) as f32;
            let glyph = if node.parent.is_some() {
                Self::member_glyph(k)
            } else {
                Self::glyph(node.kind, k)
            };
            let d = f64::from(dx.hypot(dy)) - if node.parent.is_none() { 0.6 } else { 0.1 };
            if glyph.hit(dx, dy) && best.is_none_or(|(bd, bi)| d < bd || (d == bd && i < bi)) {
                best = Some((d, i));
            }
        });
        best.map(|(_, i)| i)
    }

    /// Hover-only retention at adjacent tiny glyphs. The current identity must
    /// still satisfy its exact shared hit shape; a clearly nearer candidate or
    /// leaving that shape switches immediately. Click picking stays stateless.
    #[must_use]
    pub fn pick_stable(
        &self,
        view: &View,
        cam: &Camera,
        px: f32,
        py: f32,
        current: Option<NodeId>,
    ) -> Option<NodeId> {
        let next = self.pick(view, cam, px, py);
        let Some(current) = current.filter(|id| Some(*id) != next) else {
            return next;
        };
        let Some(node) = self.world.nodes.get(current as usize) else {
            return next;
        };
        let projection = self.projection(*view, *cam);
        let k = projection.scale();
        if node.orphan
            || !projection.contains(px, py)
            || node
                .parent
                .is_some_and(|parent| self.member_alpha(parent, k) < 0.5)
        {
            return next;
        }
        let glyph = if node.parent.is_some() {
            Self::member_glyph(k)
        } else {
            Self::glyph(node.kind, k)
        };
        let distance = |id: NodeId| {
            (px - projection.x(self.layout.x[id as usize]))
                .hypot(py - projection.y(self.layout.y[id as usize]))
                - if self.world.nodes[id as usize].parent.is_none() {
                    0.6
                } else {
                    0.1
                }
        };
        let dx = px - projection.x(self.layout.x[current as usize]);
        let dy = py - projection.y(self.layout.y[current as usize]);
        let margin = (glyph.radius.max(10.0) * 0.15).min(1.0);
        if glyph.hit(dx, dy) && next.is_none_or(|id| distance(id) + margin >= distance(current)) {
            Some(current)
        } else {
            next
        }
    }

    /// The package (and module) under world point `(x, y)`.
    #[must_use]
    pub fn territory_at(&self, x: f32, y: f32) -> Option<Terr> {
        for (p, t) in self.layout.packages.iter().enumerate() {
            if !t.bounds.contains(x, y) || !inside(&t.hull, x, y) {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let pkg = p as u32;
            let module = self.layout.package_modules[p].iter().copied().find(|&m| {
                let mt = &self.layout.modules[m as usize];
                mt.bounds.contains(x, y) && inside(&mt.hull, x, y)
            });
            return Some(Terr { pkg, module });
        }
        None
    }

    /// Shared capped screen-space geometry for an item.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn glyph(kind: Kind, k: f64) -> Glyph {
        let core = Self::draw_core(kind) * k as f32;
        let shape = if core < 1.6 {
            Shape::Square
        } else if matches!(
            kind,
            Kind::Trait | Kind::Struct | Kind::Enum | Kind::Type | Kind::Union
        ) {
            Shape::Diamond
        } else {
            Shape::Square
        };
        let radius = if core < 1.6 {
            if core < 0.7 { 0.5 } else { 0.75 }
        } else {
            core.min(9.0) * 0.8 * if shape == Shape::Square { 0.55 } else { 1.0 }
        };
        Glyph {
            core,
            radius,
            shape,
        }
    }

    /// Actual painted radius for a node, including member geometry.
    #[must_use]
    pub fn glyph_radius(&self, node: NodeId, k: f64) -> f32 {
        if self.world.node(node).parent.is_some() {
            Self::member_glyph(k).radius
        } else {
            let glyph = Self::glyph(self.world.node(node).kind, k);
            glyph.radius
                + if glyph.core >= 1.6 && self.world.node(node).kind == Kind::Trait {
                    0.6 * std::f32::consts::SQRT_2
                } else {
                    0.0
                }
        }
    }

    /// Actual painted node bounds in the same window coordinates as picking.
    #[must_use]
    pub fn node_bounds(
        &self,
        view: &View,
        cam: &Camera,
        node: NodeId,
    ) -> Option<gpui::Bounds<gpui::Pixels>> {
        let p = self.projection(*view, *cam);
        if let Some(parent) = self.world.node(node).parent {
            if (self.member_alpha(parent, p.scale()) * 3.0).round() <= 0.0 {
                return None;
            }
        } else if !self.world.is_item(node) {
            return None;
        }
        let r = self.glyph_radius(node, p.scale());
        let (x, y) = (
            p.x(self.layout.x[node as usize]),
            p.y(self.layout.y[node as usize]),
        );
        let (x0, y0, x1, y1) = (
            (x - r).max(view.x),
            (y - r).max(view.y),
            (x + r).min(view.x + view.w),
            (y + r).min(view.y + view.h),
        );
        (x1 > x0 && y1 > y0).then(|| gpui::Bounds {
            origin: gpui::point(gpui::px(x0), gpui::px(y0)),
            size: gpui::size(gpui::px(x1 - x0), gpui::px(y1 - y0)),
        })
    }

    /// Shared square member geometry on its shell.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn member_glyph(k: f64) -> Glyph {
        Glyph {
            core: 0.26 * k as f32,
            radius: (0.26 * k as f32 * 0.8).clamp(1.0, 4.5) * 0.5,
            shape: Shape::Square,
        }
    }

    /// Conservative picking query extent, including the minimum pixel target.
    fn hit_radius(k: f64) -> f64 {
        f64::from(
            Self::glyph(Kind::Struct, k)
                .radius
                .max(Self::glyph(Kind::Function, k).radius)
                .max(10.0),
        )
    }

    /// An item's drawn core radius in world units (the prototype's draw
    /// sizes: types 1.25, functions and aliases 0.8, the rest 0.6).
    #[must_use]
    pub fn draw_core(kind: Kind) -> f32 {
        match kind {
            Kind::Struct | Kind::Enum | Kind::Trait | Kind::Union => core(kind),
            Kind::Function | Kind::Type => 0.8,
            _ => 0.6,
        }
    }
}

/// Even-odd point in polygon.
#[must_use]
pub fn inside(poly: &[[f32; 2]], x: f32, y: f32) -> bool {
    let mut c = false;
    let n = poly.len();
    let mut j = n.wrapping_sub(1);
    for i in 0..n {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            c = !c;
        }
        j = i;
    }
    c
}

#[cfg(test)]
mod tests {
    use super::{PickingGrid, Scene, Shape};
    use crate::data::spatial::Aabb;
    use crate::graph::camera::View;
    use crate::graph::layout::Layout;
    use crate::graph::model::tests::tiny;
    use crate::graph::model::{Edge, Kind, Module, Node, Package, Rel, World};
    use crate::motion::Camera;
    use std::sync::Arc;

    #[test]
    fn visible_bounds_index_matches_brute_force_and_bounds_local_work() {
        use super::BoundsIndex;
        use crate::graph::layout::Box2;
        let mut entries: Vec<_> = (0..65536_u32)
            .map(|i| {
                let x = (i % 256) as f32 * 10.0;
                let y = (i / 256) as f32 * 10.0;
                (
                    Box2 {
                        x0: x - 2.0,
                        y0: y - 2.0,
                        x1: x + 2.0,
                        y1: y + 2.0,
                    },
                    i,
                )
            })
            .collect();
        // Both endpoints lie remotely outside a close viewport; the crossing
        // must survive, as must an off-centre shell extent touching its edge.
        entries.push((
            Box2 {
                x0: -10000.0,
                y0: 33.0,
                x1: 10000.0,
                y1: 33.0,
            },
            65536,
        ));
        entries.push((
            Box2 {
                x0: 100.0,
                y0: 100.0,
                x1: 160.0,
                y1: 160.0,
            },
            65537,
        ));
        let index = BoundsIndex::new(entries.clone());
        assert!(index.branches.len() < entries.len() / 4);
        for n in 0..193 {
            let x = (n * 37 % 2510) as f32 - 12.0;
            let y = (n * 73 % 2510) as f32 - 12.0;
            let query = Box2 {
                x0: x,
                y0: y,
                x1: x + 53.0,
                y1: y + 37.0,
            };
            let mut got = Vec::new();
            let work = index.visit(query, |id| got.push(id));
            got.sort_unstable();
            let expected: Vec<_> = entries
                .iter()
                .filter(|e| e.0.overlaps(&query))
                .map(|e| e.1)
                .collect();
            assert_eq!(got, expected);
            assert!(
                work < 256,
                "local query tested {work} of {} bounds",
                entries.len()
            );
        }
        let query = Box2 {
            x0: 140.0,
            y0: 32.0,
            x1: 150.0,
            y1: 101.0,
        };
        let mut got = Vec::new();
        index.visit(query, |id| got.push(id));
        assert!(got.contains(&65536), "crossing remote endpoints preserved");
        assert!(got.contains(&65537), "shell boundary preserved");
        let mut all = 0;
        let work = index.visit(
            Box2 {
                x0: -20000.0,
                y0: -20000.0,
                x1: 20000.0,
                y1: 20000.0,
            },
            |_| all += 1,
        );
        assert_eq!(all, entries.len());
        assert_eq!(work, entries.len());
        assert_eq!(
            BoundsIndex::<u32>::new(vec![]).visit(query, |_| panic!("empty")),
            0
        );
    }

    #[test]
    fn scene_visible_packets_match_occupied_geometry_across_scale_modes() {
        let world = Arc::new(tiny());
        let layout = Arc::new(Layout::compute(&world));
        let scene = Scene::new(world, layout);
        for width in [480.0, 782.0, 1440.0, 2560.0] {
            for k in [0.1, 1.0, 7.0, 38.0, 100.0, 300.0] {
                for &id in &scene.world.items {
                    let view = View {
                        x: 37.0,
                        y: 53.0,
                        w: width,
                        h: 618.0,
                    };
                    let cam = Camera::new(
                        scene.layout.x[id as usize] as f64,
                        scene.layout.y[id as usize] as f64,
                        width as f64 / k,
                    );
                    let projection = scene.projection(view, cam);
                    let mut got = Vec::new();
                    scene.visit_items(&projection, 40.0, |i| got.push(i));
                    got.sort_unstable();
                    let expected: Vec<_> = scene
                        .world
                        .items
                        .iter()
                        .copied()
                        .filter(|&i| {
                            let (x, y, r) = (
                                projection.x(scene.layout.x[i as usize]),
                                projection.y(scene.layout.y[i as usize]),
                                scene.layout.r[i as usize] * k as f32,
                            );
                            x >= view.x - r - 40.0
                                && x <= view.x + view.w + r + 40.0
                                && y >= view.y - r - 40.0
                                && y <= view.y + view.h + r + 40.0
                        })
                        .collect();
                    assert_eq!(got, expected, "width{width} k{k} node{id}");
                    let mut edges = Vec::new();
                    scene.visit_inner(&projection, |e| edges.push(e));
                    edges.sort_unstable();
                    let mut expected: Vec<_> = scene
                        .inner
                        .iter()
                        .flatten()
                        .copied()
                        .filter(|&(a, b)| {
                            let ax = projection.x(scene.layout.x[a as usize]);
                            let ay = projection.y(scene.layout.y[a as usize]);
                            let bx = projection.x(scene.layout.x[b as usize]);
                            let by = projection.y(scene.layout.y[b as usize]);
                            ax.max(bx) >= view.x
                                && ax.min(bx) <= view.x + view.w
                                && ay.max(by) >= view.y
                                && ay.min(by) <= view.y + view.h
                        })
                        .collect();
                    expected.sort_unstable();
                    assert_eq!(edges, expected);
                }
            }
        }
    }

    #[test]
    fn hover_retention_is_bounded_by_shared_shape_and_nearest_margin() {
        let world = Arc::new(
            World::new(
                vec![Package {
                    name: "pick".into(),
                    version: "0".into(),
                    yours: true,
                    external: false,
                    deps: vec![],
                }],
                vec![Module {
                    pkg: 0,
                    path: "".into(),
                    file: "".into(),
                }],
                vec![
                    Node::new(Kind::Function, "a", 0, 0),
                    Node::new(Kind::Function, "b", 0, 0),
                ],
                vec![],
            )
            .unwrap(),
        );
        let mut layout = Layout::compute(&world);
        layout.x[0] = 0.0;
        layout.y[0] = 0.0;
        layout.x[1] = 3.0;
        layout.y[1] = 0.0;
        let scene = Scene::new(world, Arc::new(layout));
        let view = View {
            x: 37.0,
            y: 19.0,
            w: 480.0,
            h: 618.0,
        };
        let cam = Camera::new(0.0, 0.0, 480.0);
        let projection = scene.projection(view, cam);
        let x = projection.x(0.0);
        let y = projection.y(0.0);
        assert_eq!(scene.pick(&view, &cam, x + 1.9, y), Some(1));
        assert_eq!(scene.pick_stable(&view, &cam, x + 1.9, y, Some(0)), Some(0));
        assert_eq!(scene.pick_stable(&view, &cam, x + 2.1, y, Some(0)), Some(1));
        assert_eq!(
            scene.pick_stable(&view, &cam, x + 10.1, y, Some(0)),
            Some(1)
        );
        assert_eq!(scene.pick_stable(&view, &cam, x + 40.0, y, Some(0)), None);
        assert_eq!(
            scene.pick_stable(&view, &cam, view.x - 0.1, y, Some(0)),
            None
        );
        assert_eq!(
            scene.pick_stable(&view, &cam, x, y, Some(u32::MAX)),
            Some(0)
        );
    }

    #[test]
    fn relation_bows_are_finite_monotone_and_have_no_inflection() {
        for n in 0..257 {
            let a = [n as f32 * 0.31 - 33.0, n as f32 * -0.17 + 7.0];
            let b = [a[0] + (n % 19) as f32 - 9.0, a[1] + (n % 23) as f32 - 11.0];
            let context = [
                (n % 13) as f32 * 317.0 - 1000.0,
                (n % 17) as f32 * -193.0 + 700.0,
            ];
            let (points, bounds) = super::relation_curve(a, b, context);
            let (reverse, _) = super::relation_curve(b, a, context);
            let d = [b[0] - a[0], b[1] - a[1]];
            let len = d[0].hypot(d[1]);
            let mut normals = Vec::new();
            for (i, p) in points.iter().enumerate() {
                assert!(p.iter().all(|v| v.is_finite()));
                assert!(
                    p[0] >= bounds.x0 - 1e-4
                        && p[0] <= bounds.x1 + 1e-4
                        && p[1] >= bounds.y0 - 1e-4
                        && p[1] <= bounds.y1 + 1e-4
                );
                assert!(
                    (p[0] - reverse[super::ROUTE_SAMPLES - 1 - i][0]).abs() < 1e-4
                        && (p[1] - reverse[super::ROUTE_SAMPLES - 1 - i][1]).abs() < 1e-4
                );
                if len > 1e-5 {
                    let progress = ((p[0] - a[0]) * d[0] + (p[1] - a[1]) * d[1]) / (len * len);
                    assert!(
                        (progress - i as f32 / (super::ROUTE_SAMPLES - 1) as f32).abs() < 2e-5,
                        "chord progress {progress} at{i}"
                    );
                    let normal = ((p[0] - a[0]) * (-d[1]) + (p[1] - a[1]) * d[0]) / len;
                    assert!(normal.abs() <= len * 0.12 + 1e-4);
                    normals.push(normal);
                }
            }
            let curvature: Vec<_> = normals
                .windows(3)
                .map(|p| p[2] - 2.0 * p[1] + p[0])
                .collect();
            assert!(curvature.iter().all(|&c| c >= -2e-4) || curvature.iter().all(|&c| c <= 2e-4));
        }
        let (same, _) = super::relation_curve([1.0, 2.0], [1.0, 2.0], [900.0, -700.0]);
        assert!(same.iter().all(|&p| p == [1.0, 2.0]));
    }

    #[test]
    fn high_fanout_packets_preserve_group_counts_and_query_only_visible_leaves() {
        use crate::graph::layout::{Box2, Territory};
        let n = 8192_u32;
        let packages = (0..8)
            .map(|i| Package {
                name: format!("pkg{i}").into(),
                version: "0".into(),
                yours: i < 2,
                external: false,
                deps: vec![],
            })
            .collect();
        let modules = (0..16)
            .map(|i| Module {
                pkg: i / 2,
                path: format!("module{i}").into(),
                file: "".into(),
            })
            .collect();
        let nodes = (0..=n)
            .map(|i| {
                Node::new(
                    if i == 0 { Kind::Trait } else { Kind::Function },
                    format!("node{i}"),
                    i % 8,
                    (i % 8) * 2 + (i / 16) % 2,
                )
            })
            .collect();
        let edges = (1..=n)
            .map(|i| {
                if (i / 8) % 2 == 0 {
                    Edge {
                        from: i,
                        to: 0,
                        rel: Rel::CALLS,
                    }
                } else {
                    Edge {
                        from: 0,
                        to: i,
                        rel: Rel::CALLS,
                    }
                }
            })
            .collect();
        let world = Arc::new(World::new(packages, modules, nodes, edges).unwrap());
        let x: Vec<_> = (0..=n)
            .map(|i| {
                if i == 0 {
                    0.0
                } else if i <= 18 {
                    (i as f32 - 9.0) * 0.1
                } else {
                    10000.0 + i as f32 * 10.0
                }
            })
            .collect();
        let y: Vec<_> = (0..=n)
            .map(|i| {
                if i <= 18 {
                    0.0
                } else {
                    (i % 8) as f32 * 1000.0
                }
            })
            .collect();
        let territory = |ids: Vec<usize>| {
            let b = ids.iter().fold(Box2::EMPTY, |b, &i| b.with(x[i], y[i]));
            let (cx, cy) = b.center();
            Territory {
                x: cx,
                y: cy,
                r: (b.x1 - b.x0).hypot(b.y1 - b.y0) * 0.5 + 1.0,
                hull: vec![[b.x0, b.y0], [b.x1, b.y0], [b.x1, b.y1], [b.x0, b.y1]],
                bounds: b,
            }
        };
        let modules = (0..16)
            .map(|m| {
                territory(
                    (0..=n as usize)
                        .filter(|&i| world.nodes[i].module == m)
                        .collect(),
                )
            })
            .collect();
        let packages = (0..8)
            .map(|p| {
                territory(
                    (0..=n as usize)
                        .filter(|&i| world.nodes[i].pkg == p)
                        .collect(),
                )
            })
            .collect();
        let layout = Arc::new(Layout::from_positions(
            &world,
            x,
            y,
            vec![0.4; n as usize + 1],
            modules,
            packages,
        ));
        let scene = Scene::new(world, layout);
        let packet = scene.neighbourhood(0);
        assert_eq!(packet.edges.len(), n as usize);
        assert_eq!(
            packet.bundles.iter().map(|b| b.count).sum::<usize>(),
            n as usize
        );
        assert_eq!(
            packet.bundles.len(),
            18,
            "two own modules, two directions, seven remote packages"
        );
        for bundle in &packet.bundles {
            let representative = scene.world.node(bundle.route.other);
            let count = packet
                .edges
                .iter()
                .filter(|e| {
                    let other = scene.world.node(e.other);
                    e.incoming == bundle.route.incoming
                        && other.pkg == representative.pkg
                        && (other.pkg != 0 || other.module == representative.module)
                        && scene.world.yours(e.other) == scene.world.yours(bundle.route.other)
                })
                .count();
            assert_eq!(bundle.count, count);
        }
        let view = View {
            x: 37.0,
            y: 53.0,
            w: 480.0,
            h: 618.0,
        };
        let cam = Camera::new(0.0, 0.0, 480.0);
        let projection = scene.projection(view, cam);
        let mut visible = Vec::new();
        let work = packet.visit_leaves(&projection, |i| visible.push(packet.edges[i].other));
        visible.sort_unstable();
        assert_eq!(visible, (1..=18).collect::<Vec<_>>());
        assert!(work < 128, "tested{work}of{n}");
        let mut lit = Vec::new();
        let work = packet.visit_lit(&projection, |i| lit.push(i));
        lit.sort_unstable();
        assert_eq!(lit, (0..=18).collect::<Vec<_>>());
        assert!(work < 128);
    }

    #[test]
    fn ambient_levels_do_not_change_when_large_hulls_leave_the_view() {
        let world = Arc::new(tiny());
        let mut layout = Layout::compute(&world);
        // Pin different territory sizes and distant centres; pure pan crosses
        // the old max-visible-radius threshold while zoom remains unchanged.
        for (i, t) in layout.packages.iter_mut().enumerate() {
            t.x = i as f32 * 4000.0;
            t.y = 0.0;
            t.r = if i == 0 { 1000.0 } else { 10.0 };
            t.bounds = crate::graph::layout::Box2 {
                x0: t.x - t.r,
                y0: -t.r,
                x1: t.x + t.r,
                y1: t.r,
            };
        }
        let scene = Scene::new(world, Arc::new(layout));
        let view = View {
            x: 37.0,
            y: 53.0,
            w: 480.0,
            h: 618.0,
        };
        let mut levels = Vec::new();
        let mut old_visible = Vec::new();
        for x in [0.0, 4000.0, 8000.0] {
            let projection = scene.projection(view, Camera::new(x, 0.0, 480.0));
            levels.push(scene.level_scales(projection.scale()));
            old_visible.push(
                scene
                    .layout
                    .packages
                    .iter()
                    .filter(|t| projection.visible(&t.bounds))
                    .map(|t| t.r)
                    .fold(0.0, f32::max),
            );
        }
        assert!(old_visible.windows(2).any(|v| v[0] != v[1]));
        assert!(levels.windows(2).all(|v| v[0] == v[1]));
    }

    #[test]
    fn animated_hover_reuses_adjacency_and_keeps_the_members_owner_lit() {
        // Pin this cache contract to its own graph; unrelated fixture relations
        // must not silently become an assertion about cache membership.
        let world = Arc::new(
            World::new(
                vec![Package {
                    name: "cache-fixture".into(),
                    version: "0".into(),
                    yours: true,
                    external: false,
                    deps: vec![],
                }],
                vec![Module {
                    pkg: 0,
                    path: "".into(),
                    file: "".into(),
                }],
                vec![
                    Node::new(Kind::Struct, "Owner", 0, 0),
                    Node::new(Kind::Field, "value", 0, 0).member_of(0),
                    Node::new(Kind::Method, "caller", 0, 0).member_of(0),
                    Node::new(Kind::Function, "callee", 0, 0),
                ],
                vec![Edge {
                    from: 2,
                    to: 3,
                    rel: Rel::CALLS,
                }],
            )
            .expect("valid synthetic hover-cache graph"),
        );
        let layout = Arc::new(Layout::compute(&world));
        let scene = Scene::new(world, layout);
        let first = scene.neighbourhood(2);
        assert_eq!(first.lit, vec![0, 2, 3]);
        assert!(
            Arc::ptr_eq(&first, &scene.neighbourhood(2)),
            "hover animation must not rederive adjacency"
        );
        let next = scene.neighbourhood(3);
        assert!(!Arc::ptr_eq(&first, &next));
        assert!(Arc::ptr_eq(&next, &scene.neighbourhood(3)));
        assert_eq!(
            first.lit,
            vec![0, 2, 3],
            "an in-flight snapshot remains immutable after retarget"
        );
    }
    #[test]
    fn projection_and_picking_share_pixels_across_camera_and_view_offsets() {
        let world = Arc::new(tiny());
        let layout = Arc::new(Layout::compute(&world));
        let scene = Scene::new(world, layout);
        for (width, height) in [(480.0, 900.0), (1440.0, 900.0), (2560.0, 1440.0)] {
            for k in [0.1, 1.0, 26.0, 100.0, 300.0] {
                let view = View {
                    x: 37.0,
                    y: 19.0,
                    w: width,
                    h: height,
                };
                let cam = Camera::new(12.0, -17.0, f64::from(width) / k);
                let projected = scene.projection(view, cam);
                for (x, y) in [(12.0, -17.0), (13.5, -16.0), (10.0, -20.0)] {
                    let (back_x, back_y) = projected.to_world(projected.x(x), projected.y(y));
                    assert!(
                        (back_x - f64::from(x)).abs() < 1e-3
                            && (back_y - f64::from(y)).abs() < 1e-3
                    );
                }
                let glyph = Scene::glyph(Kind::Struct, k);
                assert!(glyph.hit(glyph.radius * 0.8, 0.0));
                assert!(
                    !glyph.hit(40.0, 0.0),
                    "zoom cannot select blank pixels forty pixels from a glyph"
                );
                assert!(Scene::member_glyph(k).hit(9.0, 0.0));
                assert!(!Scene::member_glyph(k).hit(40.0, 0.0));
            }
        }
        let glyph = Scene::glyph(Kind::Struct, 100.0);
        assert_eq!(glyph.shape, Shape::Diamond);
        assert!(
            (glyph.radius - 7.2).abs() < 1e-6,
            "painted core is capped, not125pixels"
        );
        assert!(!glyph.contains(7.0, 7.0), "diamond corners are empty");
        let square = Scene::glyph(Kind::Function, 100.0);
        assert!(square.contains(square.radius, square.radius));
    }

    #[test]
    fn measured_foreground_room_preserves_fanout_across_card_placements() {
        use gpui::{Bounds, point, px, size};
        for (view, card, below, wide) in [
            (
                View {
                    x: 37.0,
                    y: 19.0,
                    w: 1440.0,
                    h: 900.0,
                },
                [1097.0, 99.0, 340.0, 175.0],
                false,
                true,
            ),
            (
                View {
                    x: 37.0,
                    y: 19.0,
                    w: 782.0,
                    h: 704.0,
                },
                [459.0, 99.0, 340.0, 175.0],
                true,
                false,
            ),
            (
                View {
                    x: 37.0,
                    y: 19.0,
                    w: 480.0,
                    h: 900.0,
                },
                [57.0, 519.0, 440.0, 380.0],
                false,
                false,
            ),
        ] {
            let occupied = Bounds::new(
                point(px(card[0]), px(card[1])),
                size(px(card[2]), px(card[3])),
            );
            let room = Scene::free_view(&view, Some(occupied));
            assert_eq!(!Scene::prism_narrow(&room), wide);
            if below {
                assert_eq!(room.w, view.w);
                assert!(room.y >= card[1] + card[3] + 12.0);
            }
            assert!(room.x >= view.x && room.y >= view.y);
            assert!(room.x + room.w <= view.x + view.w);
            assert!(room.y + room.h <= view.y + view.h);
            assert!(
                room.x + room.w <= card[0]
                    || room.y + room.h <= card[1]
                    || room.x >= card[0] + card[2]
                    || room.y >= card[1] + card[3]
            );
            let (x, y) = Scene::focus_anchor(&room);
            assert!(x > room.x && x < room.x + room.w && y > room.y && y < room.y + room.h);
            if !wide {
                assert!(
                    x + 100.0 + 28.0 < room.x + room.w,
                    "merged columns have source-to-proxy fanout and label room"
                );
            }
        }
    }

    #[test]
    fn actual_picking_preserves_pixel_targets_at_every_zoom() {
        for kind in [Kind::Struct, Kind::Function, Kind::Trait] {
            let world = Arc::new(
                World::new(
                    vec![Package {
                        name: "pick-fixture".into(),
                        version: "0".into(),
                        yours: true,
                        external: false,
                        deps: vec![],
                    }],
                    vec![Module {
                        pkg: 0,
                        path: "".into(),
                        file: "".into(),
                    }],
                    vec![Node::new(kind, "Only", 0, 0)],
                    vec![],
                )
                .expect("valid isolated picking graph"),
            );
            let layout = Arc::new(Layout::compute(&world));
            let scene = Scene::new(world, layout);
            for (width, height) in [(480.0, 900.0), (1440.0, 900.0), (2560.0, 1440.0)] {
                let view = View {
                    x: 37.0,
                    y: 19.0,
                    w: width,
                    h: height,
                };
                for k in [0.1, 1.0, 26.0, 100.0, 300.0] {
                    let cam = Camera::new(
                        f64::from(scene.layout.x[0]),
                        f64::from(scene.layout.y[0]),
                        f64::from(width) / k,
                    );
                    let (x, y) = view.to_screen(&cam, scene.layout.x[0], scene.layout.y[0]);
                    assert_eq!(scene.pick(&view, &cam, x, y), Some(0));
                    assert_eq!(scene.pick(&view, &cam, x + 9.0, y), Some(0));
                    assert_eq!(
                        scene.pick(&view, &cam, x + 40.0, y),
                        None,
                        "minimum target stays in pixels through the actual sparse query"
                    );
                    assert_eq!(scene.pick(&view, &cam, view.x - 1.0, y), None);
                }
            }
        }
    }

    #[test]
    fn sparse_picking_agrees_with_geometry_without_duplicate_candidates() {
        let points: Vec<_> = (0..2000)
            .map(|i| {
                [
                    ((i * 17) % 200) as f32 - 100.0,
                    ((i * 71) % 200) as f32 - 100.0,
                ]
            })
            .collect();
        let grid = PickingGrid::build(points.clone());
        for bounds in [
            Aabb::new(-3.0, -3.0, 6.0, 6.0),
            Aabb::new(-1000.0, -1000.0, 2000.0, 2000.0),
            Aabb::new(1000.0, 1000.0, 10.0, 10.0),
        ] {
            let got = grid.query(&bounds);
            let expected: Vec<_> = points
                .iter()
                .enumerate()
                .filter(|(_, p)| {
                    p[0] >= bounds.x0 && p[0] <= bounds.x1 && p[1] >= bounds.y0 && p[1] <= bounds.y1
                })
                .map(|(i, _)| i as u32)
                .collect();
            assert_eq!(got, expected);
        }
    }

    #[test]
    fn cached_hover_curve_bounds_contain_every_sample() {
        let world = Arc::new(tiny());
        let layout = Arc::new(Layout::compute(&world));
        let scene = Scene::new(world, layout);
        let neighbours = scene.neighbourhood(3);
        assert!(!neighbours.edges.is_empty());
        for edge in &neighbours.edges {
            assert!(
                edge.points.len() <= 41,
                "animation stack buffer covers every nested bundle"
            );
            for p in &edge.points {
                assert!(
                    p[0] >= edge.bounds.x0 - 1e-4
                        && p[0] <= edge.bounds.x1 + 1e-4
                        && p[1] >= edge.bounds.y0 - 1e-4
                        && p[1] <= edge.bounds.y1 + 1e-4
                );
            }
        }
    }
    #[test]
    fn transition_bounds_only_describe_visible_painted_geometry() {
        let world = Arc::new(tiny());
        let layout = Arc::new(Layout::compute(&world));
        let scene = Scene::new(world, layout);
        let view = View {
            x: 37.0,
            y: 19.0,
            w: 1440.0,
            h: 900.0,
        };
        let (x, y) = (f64::from(scene.layout.x[2]), f64::from(scene.layout.y[2]));
        assert!(
            scene
                .node_bounds(&view, &Camera::new(x, y, 144_000.0), 2)
                .is_none(),
            "an unpainted member has no transition source"
        );
        let bounds = scene
            .node_bounds(&view, &Camera::new(x, y, 14.4), 2)
            .expect("the visible member");
        assert!((f32::from(bounds.size.width) - 4.5).abs() < 1e-4);
        assert!(
            scene
                .node_bounds(&view, &Camera::new(x + 1_000.0, y + 1_000.0, 14.4), 2)
                .is_none(),
            "an offscreen member has no source"
        );
        let (x, y) = (f64::from(scene.layout.x[0]), f64::from(scene.layout.y[0]));
        let cam = Camera::new(x - (720.0 - 3.6) / 100.0, y, 14.4);
        let clipped = scene
            .node_bounds(&view, &cam, 0)
            .expect("partially visible type");
        assert!((f32::from(clipped.right()) - (view.x + view.w)).abs() < 1e-3);
        assert!(f32::from(clipped.size.width) < 14.4);
    }
}

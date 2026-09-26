//! Nested, deterministic layout for the world graph (a port of the
//! prototype's `layout.mjs`, gui-plan §8.2).
//!
//! world → packages → modules → items → members. Each level is a small
//! force simulation (links, exact collision, gravity) seeded on a
//! phyllotaxis spiral by importance, so the 54 k-symbol workspace lays out
//! in a fraction of a second and never becomes a hairball: containment does
//! the clustering, forces only order neighbours. Members sit on shells
//! around their type — parts (variants, fields) inside, methods outside —
//! evenly spaced, so a shell reads as a cut polygon. Territories are the
//! faceted convex hulls of their contents.
//!
//! Every level's simulations are independent, so they run on scoped
//! threads and write to their own slots: the result is a pure function of
//! the world (the same input gives byte-identical positions). The view gets
//! it through [`cached`], which computes on the background executor once per
//! content hash.

use super::model::{Kind, NodeId, Rollup, World};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::f64::consts::PI;
use std::sync::{Arc, Mutex, OnceLock};

/// The golden angle.
const GOLD: f64 = PI * (3.0 - 2.236_067_977_499_79);
/// Arc spacing between members on a shell.
const SPACING: f64 = 0.78;
/// Radial gap between shells.
const SHELL_GAP: f64 = 0.72;
/// Radius of a member's dot.
pub const MEMBER_R: f32 = 0.26;

/// An item's core radius by kind (types are bigger than callables).
#[must_use]
pub const fn core(kind: Kind) -> f32 {
    match kind {
        Kind::Struct | Kind::Enum | Kind::Trait | Kind::Union => 1.25,
        Kind::Macro => 0.7,
        Kind::Constant => 0.6,
        _ => 0.8,
    }
}

/// An axis-aligned box in world units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Box2 {
    /// Left.
    pub x0: f32,
    /// Top.
    pub y0: f32,
    /// Right.
    pub x1: f32,
    /// Bottom.
    pub y1: f32,
}

impl Box2 {
    /// The empty box (extends to anything).
    pub const EMPTY: Self = Self {
        x0: f32::INFINITY,
        y0: f32::INFINITY,
        x1: f32::NEG_INFINITY,
        y1: f32::NEG_INFINITY,
    };

    /// The box around `points`.
    #[must_use]
    pub fn around(points: &[[f32; 2]]) -> Self {
        points.iter().fold(Self::EMPTY, |b, p| b.with(p[0], p[1]))
    }

    /// Grown to include `(x, y)`.
    #[must_use]
    pub fn with(self, x: f32, y: f32) -> Self {
        Self {
            x0: self.x0.min(x),
            y0: self.y0.min(y),
            x1: self.x1.max(x),
            y1: self.y1.max(y),
        }
    }

    /// The union.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Whether they overlap.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }

    /// Whether `(x, y)` is inside (closed).
    #[must_use]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    /// Width.
    #[must_use]
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    /// Height.
    #[must_use]
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// Centre.
    #[must_use]
    pub fn center(&self) -> (f32, f32) {
        ((self.x0 + self.x1) / 2.0, (self.y0 + self.y1) / 2.0)
    }
}

/// A module's or package's place: its centre, the radius its simulation
/// gave it, and its faceted hull.
#[derive(Clone, Debug, PartialEq)]
pub struct Territory {
    /// Centre x.
    pub x: f32,
    /// Centre y.
    pub y: f32,
    /// Enclosing radius.
    pub r: f32,
    /// The faceted convex hull, counter-clockwise in screen space.
    pub hull: Vec<[f32; 2]>,
    /// The hull's box.
    pub bounds: Box2,
}

/// One shell of members around an item: members
/// `shell_members[start..start + len]`, in angular order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shell {
    /// Radius from the item's centre.
    pub r: f32,
    /// First member in [`Layout::shell_members`].
    pub start: u32,
    /// How many.
    pub len: u32,
}

/// Every position in the world graph.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// Node x (items and members).
    pub x: Vec<f32>,
    /// Node y.
    pub y: Vec<f32>,
    /// Node radius: an item's extent (its outer shell), a member's dot.
    pub r: Vec<f32>,
    /// Every module.
    pub modules: Vec<Territory>,
    /// Every package.
    pub packages: Vec<Territory>,
    /// Items per module, most important first.
    pub module_items: Vec<Vec<NodeId>>,
    /// Modules per package.
    pub package_modules: Vec<Vec<u32>>,
    /// Shells per node (`shells[shell_off[i]..shell_off[i + 1]]`).
    pub shells: Vec<Shell>,
    shell_off: Vec<u32>,
    /// Members in shell order.
    pub shell_members: Vec<NodeId>,
    /// Module-level edges (first-seen order over item edges).
    pub module_edges: Vec<Rollup>,
    /// Package-level edges.
    pub package_edges: Vec<Rollup>,
    /// The world's box (all package hulls).
    pub bounds: Box2,
    /// The content hash of the inputs this layout was computed from.
    pub key: [u8; 32],
}

impl Layout {
    /// The shells of item `i` (empty for members and bare items).
    #[must_use]
    pub fn shells_of(&self, i: NodeId) -> &[Shell] {
        let i = i as usize;
        if i + 1 >= self.shell_off.len() {
            return &[];
        }
        &self.shells[self.shell_off[i] as usize..self.shell_off[i + 1] as usize]
    }

    /// The members on one shell, in angular order.
    #[must_use]
    pub fn shell(&self, shell: &Shell) -> &[NodeId] {
        &self.shell_members[shell.start as usize..(shell.start + shell.len) as usize]
    }

    /// Computes the layout (on the calling thread plus scoped workers).
    #[must_use]
    pub fn compute(world: &World) -> Self {
        compute(world, key(world))
    }
}

/// One body of a level's simulation.
#[derive(Clone, Copy, Debug, Default)]
struct Body {
    r: f64,
    w: f64,
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
}

/// Simulation settings.
#[derive(Clone, Copy)]
struct Forces {
    gravity: f64,
    iters: usize,
    charge: f64,
}

/// Collision pairs closer than this are resolved in the final exact pass.
const OVERLAP_EPS: f64 = 1e-4;

/// One level: links + charge + gravity + exact collision, seeded on a
/// phyllotaxis spiral by weight; ends centred on the area-weighted centroid
/// with no two bodies overlapping. `links`: `(a, b, strength)`.
fn simulate(nodes: &mut [Body], links: &[(u32, u32, f64)], f: Forces) {
    let n = nodes.len();
    if n == 0 {
        return;
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| nodes[b].w.total_cmp(&nodes[a].w));
    #[allow(clippy::cast_precision_loss)]
    let mean_r = nodes.iter().map(|b| b.r).sum::<f64>() / n as f64;
    for (k, &i) in order.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let (kf, r) = (k as f64, mean_r * 1.9 * (k as f64 + 0.5).sqrt());
        nodes[i].x = r * (kf * GOLD).cos();
        nodes[i].y = r * (kf * GOLD).sin();
        nodes[i].vx = 0.0;
        nodes[i].vy = 0.0;
    }
    if n == 1 {
        nodes[0].x = 0.0;
        nodes[0].y = 0.0;
        return;
    }
    let mut deg = vec![0.0_f64; n];
    for &(a, b, _) in links {
        deg[a as usize] += 1.0;
        deg[b as usize] += 1.0;
    }
    let mut alpha = 1.0_f64;
    #[allow(clippy::cast_precision_loss)]
    let decay = 1.0 - 0.001_f64.powf(1.0 / f.iters as f64);
    for _ in 0..f.iters {
        alpha += (0.0 - alpha) * decay;
        for &(a, b, s) in links {
            let (a, b) = (a as usize, b as usize);
            let (pa, pb) = (nodes[a], nodes[b]);
            let mut dx = pb.x + pb.vx - pa.x - pa.vx;
            let mut dy = pb.y + pb.vy - pa.y - pa.vy;
            let l = {
                let l = dx.hypot(dy);
                if l == 0.0 { 1e-6 } else { l }
            };
            let target = pa.r + pb.r + 0.6 * mean_r;
            let k = (l - target) / l * alpha * s / deg[a].min(deg[b]) * 0.5;
            dx *= k;
            dy *= k;
            nodes[b].vx -= dx;
            nodes[b].vy -= dy;
            nodes[a].vx += dx;
            nodes[a].vy += dy;
        }
        if f.charge != 0.0 {
            for i in 0..n {
                for j in i + 1..n {
                    let (dx, dy) = (nodes[j].x - nodes[i].x, nodes[j].y - nodes[i].y);
                    let l2 = dx * dx + dy * dy + 1e-6;
                    let k = f.charge * alpha * mean_r * mean_r / l2;
                    nodes[i].vx -= dx * k;
                    nodes[i].vy -= dy * k;
                    nodes[j].vx += dx * k;
                    nodes[j].vy += dy * k;
                }
            }
        }
        for b in nodes.iter_mut() {
            b.vx -= b.x * f.gravity * alpha;
            b.vy -= b.y * f.gravity * alpha;
        }
        for b in nodes.iter_mut() {
            b.x += b.vx;
            b.y += b.vy;
            b.vx *= 0.6;
            b.vy *= 0.6;
        }
        for _ in 0..2 {
            collide(nodes);
        }
    }
    // The prototype stops after its last relaxation pass, which leaves
    // slivers of overlap. Relax a while longer, then spread the level about
    // its centre by the least factor that parts every remaining pair (a
    // uniform scale keeps the arrangement; it is ~1.00x in practice).
    for _ in 0..60 {
        if !collide(nodes) {
            break;
        }
    }
    let mut spread = 1.0_f64;
    for i in 0..n {
        for j in i + 1..n {
            let d = (nodes[j].x - nodes[i].x).hypot(nodes[j].y - nodes[i].y);
            let min = nodes[i].r + nodes[j].r;
            if d < min {
                spread = spread.max(if d > 1e-9 { min / d } else { 2.0 });
            }
        }
    }
    if spread > 1.0 {
        let spread = spread * (1.0 + 1e-6);
        for b in nodes.iter_mut() {
            b.x *= spread;
            b.y *= spread;
        }
    }
    let (mut cx, mut cy, mut tw) = (0.0, 0.0, 0.0);
    for b in nodes.iter() {
        let w = b.r * b.r;
        cx += b.x * w;
        cy += b.y * w;
        tw += w;
    }
    cx /= tw;
    cy /= tw;
    for b in nodes.iter_mut() {
        b.x -= cx;
        b.y -= cy;
    }
}

/// One relaxation pass over every pair; returns whether any pair still
/// overlapped by more than [`OVERLAP_EPS`].
fn collide(nodes: &mut [Body]) -> bool {
    let n = nodes.len();
    let mut any = false;
    for i in 0..n {
        for j in i + 1..n {
            let (dx, dy) = (nodes[j].x - nodes[i].x, nodes[j].y - nodes[i].y);
            let min = nodes[i].r + nodes[j].r;
            let l2 = dx * dx + dy * dy;
            if l2 < min * min {
                let l = {
                    let l = l2.sqrt();
                    if l == 0.0 { 1e-6 } else { l }
                };
                if min - l > OVERLAP_EPS {
                    any = true;
                }
                // Push a hair past contact so the pair reads as apart.
                let push = (min - l) / l * 0.5 * 1.0001;
                let (ra, rb) = (nodes[i].r * nodes[i].r, nodes[j].r * nodes[j].r);
                let wa = rb / (ra + rb);
                nodes[i].x -= dx * push * wa * 2.0;
                nodes[i].y -= dy * push * wa * 2.0;
                nodes[j].x += dx * push * (1.0 - wa) * 2.0;
                nodes[j].y += dy * push * (1.0 - wa) * 2.0;
            }
        }
    }
    any
}

/// The radius enclosing every body about the origin.
fn enclose(nodes: &[Body]) -> f64 {
    nodes
        .iter()
        .fold(0.0, |m, b| f64::max(m, b.x.hypot(b.y) + b.r))
}

/// Runs `job(k)` for every `k < count`, spread over the machine's cores;
/// results come back in `k` order regardless of which thread ran them.
fn parallel<T: Send>(count: usize, cost: impl Fn(usize) -> u64 + Sync, job: impl Fn(usize) -> T + Sync) -> Vec<T> {
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get).min(16);
    if threads <= 1 || count < 2 {
        return (0..count).map(job).collect();
    }
    // Biggest jobs first, dealt round the threads (greedy by cost).
    let mut order: Vec<usize> = (0..count).collect();
    order.sort_by_key(|&k| std::cmp::Reverse(cost(k)));
    let mut lanes: Vec<Vec<usize>> = vec![Vec::new(); threads];
    let mut load = vec![0_u64; threads];
    for k in order {
        let lane = (0..threads).min_by_key(|&t| (load[t], t)).unwrap_or(0);
        load[lane] += cost(k).max(1);
        lanes[lane].push(k);
    }
    let mut slots: Vec<Option<T>> = (0..count).map(|_| None).collect();
    std::thread::scope(|scope| {
        let job = &job;
        let handles: Vec<_> = lanes
            .into_iter()
            .map(|lane| scope.spawn(move || lane.into_iter().map(|k| (k, job(k))).collect::<Vec<_>>()))
            .collect();
        for handle in handles {
            if let Ok(done) = handle.join() {
                for (k, value) in done {
                    slots[k] = Some(value);
                }
            }
        }
    });
    slots
        .into_iter()
        .enumerate()
        .map(|(k, slot)| slot.unwrap_or_else(|| job(k)))
        .collect()
}

/// `n` points on a circle of radius `r` about `(x, y)`, from angle `rot`.
fn ring(x: f64, y: f64, r: f64, n: usize, rot: f64) -> impl Iterator<Item = [f64; 2]> {
    #[allow(clippy::cast_precision_loss)]
    (0..n).map(move |i| {
        let t = rot + i as f64 * 2.0 * PI / n as f64;
        [x + r * t.cos(), y + r * t.sin()]
    })
}

/// Andrew's monotone chain; collinear points dropped.
fn hull(mut pts: Vec<[f64; 2]>) -> Vec<[f64; 2]> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    let cross = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut lo: Vec<[f64; 2]> = Vec::new();
    for &p in &pts {
        while lo.len() >= 2 && cross(lo[lo.len() - 2], lo[lo.len() - 1], p) <= 0.0 {
            lo.pop();
        }
        lo.push(p);
    }
    let mut up: Vec<[f64; 2]> = Vec::new();
    for &p in pts.iter().rev() {
        while up.len() >= 2 && cross(up[up.len() - 2], up[up.len() - 1], p) <= 0.0 {
            up.pop();
        }
        up.push(p);
    }
    up.pop();
    lo.pop();
    lo.extend(up);
    lo
}

/// Facets a convex hull so a territory reads as a cut stone, not a circle:
/// merges edges that turn by less than `min_turn` (radians), as the
/// prototype does, then pushes every remaining edge out, parallel to
/// itself, until it supports the exact hull.
///
/// The prototype stops after the first step, whose chords cut slivers off
/// the contents; the second keeps every edge direction (the same facets)
/// and guarantees the faceted hull contains everything the exact one does.
fn facet(exact: Vec<[f64; 2]>, min_turn: f64) -> Vec<[f64; 2]> {
    let mut out = exact.clone();
    let turn = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        let t = ((c[1] - b[1]).atan2(c[0] - b[0]) - (b[1] - a[1]).atan2(b[0] - a[0])).abs();
        t.min(2.0 * PI - t)
    };
    let mut changed = true;
    while changed && out.len() > 5 {
        changed = false;
        let mut i = 0;
        while i < out.len() && out.len() > 5 {
            let n = out.len();
            if turn(out[(i + n - 1) % n], out[i], out[(i + 1) % n]) < min_turn {
                out.remove(i);
                changed = true;
            } else {
                i += 1;
            }
        }
    }
    support(&out, &exact)
}

/// Moves each edge of the convex polygon `poly` outward along its normal
/// until no point of `points` lies beyond it, then re-intersects
/// neighbouring edges.
fn support(poly: &[[f64; 2]], points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let n = poly.len();
    if n < 3 {
        return poly.to_vec();
    }
    // Orientation: the sign of the signed area.
    let area: f64 = (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            a[0] * b[1] - b[0] * a[1]
        })
        .sum();
    let sign = if area >= 0.0 { 1.0 } else { -1.0 };
    // Each edge as (unit outward normal, offset).
    let lines: Vec<([f64; 2], f64)> = (0..n)
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % n]);
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let l = dx.hypot(dy).max(1e-12);
            let normal = [sign * dy / l, -sign * dx / l];
            let offset = points
                .iter()
                .chain(std::iter::once(&a))
                .map(|p| p[0] * normal[0] + p[1] * normal[1])
                .fold(f64::MIN, f64::max);
            (normal, offset)
        })
        .collect();
    (0..n)
        .map(|i| {
            let (n1, c1) = lines[(i + n - 1) % n];
            let (n2, c2) = lines[i];
            let det = n1[0] * n2[1] - n1[1] * n2[0];
            if det.abs() < 1e-12 {
                return poly[i];
            }
            [(c1 * n2[1] - c2 * n1[1]) / det, (n1[0] * c2 - n2[0] * c1) / det]
        })
        .collect()
}

#[allow(clippy::cast_possible_truncation)]
fn to_f32(poly: &[[f64; 2]]) -> Vec<[f32; 2]> {
    poly.iter().map(|p| [p[0] as f32, p[1] as f32]).collect()
}

/// The content hash of everything the layout reads: packages' ownership,
/// modules' packages and paths, nodes' kind, parent, module and line, and
/// every edge.
#[must_use]
pub fn key(world: &World) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"nudox-graph-layout/1");
    h.update((world.packages.len() as u64).to_le_bytes());
    for p in &world.packages {
        h.update([u8::from(p.yours)]);
    }
    h.update((world.modules.len() as u64).to_le_bytes());
    for m in &world.modules {
        h.update(m.pkg.to_le_bytes());
        h.update((m.path.len() as u64).to_le_bytes());
        h.update(m.path.as_bytes());
    }
    h.update((world.nodes.len() as u64).to_le_bytes());
    for n in &world.nodes {
        h.update([n.kind as u8]);
        h.update(n.parent.map_or(u32::MAX, |p| p).to_le_bytes());
        h.update(n.module.to_le_bytes());
        h.update(n.line.to_le_bytes());
    }
    h.update((world.edges.len() as u64).to_le_bytes());
    for e in &world.edges {
        h.update(e.from.to_le_bytes());
        h.update(e.to.to_le_bytes());
        h.update(e.rel.0.to_le_bytes());
    }
    h.finalize().into()
}

#[allow(clippy::too_many_lines, clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn compute(world: &World, key: [u8; 32]) -> Layout {
    let nn = world.len();
    let nodes = &world.nodes;
    let n_mod = world.modules.len();
    let n_pkg = world.packages.len();
    let mut x = vec![0.0_f32; nn];
    let mut y = vec![0.0_f32; nn];
    let mut r = vec![0.0_f32; nn];

    // ---- level 3: members on shells around their items
    let mut rel: Vec<Vec<(NodeId, f64, f64)>> = vec![Vec::new(); nn];
    let mut shells = Vec::new();
    let mut shell_off = vec![0_u32; nn + 1];
    let mut shell_members = Vec::new();
    for i in 0..nn as u32 {
        shell_off[i as usize] = shells.len() as u32;
        if !world.is_item(i) {
            continue;
        }
        let mut kids: Vec<NodeId> = world.kids(i).to_vec();
        let inner = |k: NodeId| u8::from(!nodes[k as usize].kind.is_part());
        kids.sort_by(|&a, &b| inner(a).cmp(&inner(b)).then(nodes[a as usize].line.cmp(&nodes[b as usize].line)));
        let r0 = f64::from(core(nodes[i as usize].kind));
        let mut radius = r0 + 0.55;
        let mut count = 0_usize;
        let parts: Vec<NodeId> = kids.iter().copied().filter(|&k| inner(k) == 0).collect();
        let does: Vec<NodeId> = kids.iter().copied().filter(|&k| inner(k) == 1).collect();
        for group in [parts, does] {
            let mut q = 0;
            while q < group.len() {
                let cap = ((2.0 * PI * radius / SPACING).floor() as usize).max(4);
                let take = &group[q..(q + cap).min(group.len())];
                let len = take.len() as f64;
                let phase = -PI / 2.0 + if count % 2 == 1 { PI / len } else { 0.0 };
                let start = shell_members.len() as u32;
                for (t, &k) in take.iter().enumerate() {
                    let th = phase + t as f64 * 2.0 * PI / len;
                    rel[i as usize].push((k, radius * th.cos(), radius * th.sin()));
                    shell_members.push(k);
                }
                shells.push(Shell {
                    r: radius as f32,
                    start,
                    len: take.len() as u32,
                });
                count += 1;
                q += take.len();
                radius += SHELL_GAP;
            }
        }
        r[i as usize] = shells.last().filter(|_| count > 0).map_or(r0 as f32, |s: &Shell| s.r + 0.4);
    }
    shell_off[nn] = shells.len() as u32;

    // ---- level 2: items in modules
    let mut module_items: Vec<Vec<NodeId>> = vec![Vec::new(); n_mod];
    for &it in &world.items {
        module_items[nodes[it as usize].module as usize].push(it);
    }
    let mut local_of = vec![u32::MAX; nn]; // item -> index in its module
    for list in &module_items {
        for (k, &id) in list.iter().enumerate() {
            local_of[id as usize] = k as u32;
        }
    }
    let mut module_links: Vec<Vec<(u32, u32, f64)>> = vec![Vec::new(); n_mod];
    for e in &world.item_edges {
        let (ma, mb) = (nodes[e.from as usize].module, nodes[e.to as usize].module);
        if ma == mb {
            let s = (0.4 + 0.2 * f64::from(1 + e.weight).log2()).min(1.0);
            module_links[ma as usize].push((local_of[e.from as usize], local_of[e.to as usize], s));
        }
    }
    let imp = &world.importance_exact;
    let solved: Vec<(Vec<(f64, f64)>, f32)> = parallel(
        n_mod,
        |m| (module_items[m].len() as u64).pow(2),
        |m| {
            let ids = &module_items[m];
            let mut bodies: Vec<Body> = ids
                .iter()
                .map(|&id| {
                    let kind = nodes[id as usize].kind;
                    let typed = matches!(kind, Kind::Struct | Kind::Enum | Kind::Trait);
                    Body {
                        r: f64::from(r[id as usize]) + 0.45,
                        w: f64::from(imp[id as usize]) + if typed { 1.0 } else { 0.0 },
                        ..Body::default()
                    }
                })
                .collect();
            let charge = if ids.len() < 120 { 0.08 } else { 0.0 };
            simulate(&mut bodies, &module_links[m], Forces { gravity: 0.06, iters: 260, charge });
            let radius = (enclose(&bodies).max(2.0) + 0.9) as f32;
            (bodies.iter().map(|b| (b.x, b.y)).collect(), radius)
        },
    );
    let mr: Vec<f32> = solved.iter().map(|s| s.1).collect();

    // ---- level 1: modules in packages
    let mut package_modules: Vec<Vec<u32>> = vec![Vec::new(); n_pkg];
    for (m, module) in world.modules.iter().enumerate() {
        package_modules[module.pkg as usize].push(m as u32);
    }
    let (module_edges, package_edges) = level_edges(world);
    let mut mod_local = vec![u32::MAX; n_mod];
    for list in &package_modules {
        for (k, &m) in list.iter().enumerate() {
            mod_local[m as usize] = k as u32;
        }
    }
    let mut package_links: Vec<Vec<(u32, u32, f64)>> = vec![Vec::new(); n_pkg];
    for e in &module_edges {
        let (pa, pb) = (world.modules[e.from as usize].pkg, world.modules[e.to as usize].pkg);
        if pa == pb {
            let s = (0.3 + 0.15 * f64::from(1 + e.weight).log2()).min(1.0);
            package_links[pa as usize].push((mod_local[e.from as usize], mod_local[e.to as usize], s));
        }
    }
    // The module tree: a::b sits near a (the root holds the top modules).
    for (p, list) in package_modules.iter().enumerate() {
        let mut by_path: HashMap<&str, u32> = HashMap::new();
        for &m in list {
            by_path.insert(world.modules[m as usize].path.as_ref(), m);
        }
        for &m in list {
            let path: &str = world.modules[m as usize].path.as_ref();
            let up = match path.rfind("::") {
                Some(at) => Some(&path[..at]),
                None if !path.is_empty() => Some(""),
                None => None,
            };
            if let Some(&parent) = up.and_then(|up| by_path.get(up)) {
                package_links[p].push((mod_local[m as usize], mod_local[parent as usize], 1.0));
            }
        }
    }
    let placed: Vec<(Vec<(f64, f64)>, f32)> = parallel(
        n_pkg,
        |p| (package_modules[p].len() as u64).pow(2),
        |p| {
            let mut bodies: Vec<Body> = package_modules[p]
                .iter()
                .map(|&m| Body {
                    r: f64::from(mr[m as usize]) + 1.2,
                    w: f64::from(mr[m as usize]),
                    ..Body::default()
                })
                .collect();
            simulate(&mut bodies, &package_links[p], Forces { gravity: 0.05, iters: 300, charge: 0.05 });
            let radius = (enclose(&bodies).max(4.0) + 3.0) as f32;
            (bodies.iter().map(|b| (b.x, b.y)).collect(), radius)
        },
    );
    let pr: Vec<f32> = placed.iter().map(|s| s.1).collect();

    // ---- level 0: packages in the world
    let mut world_bodies: Vec<Body> = (0..n_pkg)
        .map(|p| Body {
            r: f64::from(pr[p]) + 10.0,
            w: f64::from(pr[p]),
            ..Body::default()
        })
        .collect();
    let world_links: Vec<(u32, u32, f64)> = package_edges
        .iter()
        .map(|e| (e.from, e.to, (0.2 + 0.12 * f64::from(1 + e.weight).log2()).min(1.0)))
        .collect();
    simulate(&mut world_bodies, &world_links, Forces { gravity: 0.05, iters: 500, charge: 0.1 });
    let px: Vec<f32> = world_bodies.iter().map(|b| b.x as f32).collect();
    let py: Vec<f32> = world_bodies.iter().map(|b| b.y as f32).collect();

    // ---- compose absolute positions
    let mut mx = vec![0.0_f32; n_mod];
    let mut my = vec![0.0_f32; n_mod];
    for (p, list) in package_modules.iter().enumerate() {
        for (k, &m) in list.iter().enumerate() {
            let (lx, ly) = placed[p].0[k];
            mx[m as usize] = (f64::from(px[p]) + lx) as f32;
            my[m as usize] = (f64::from(py[p]) + ly) as f32;
        }
    }
    for (m, list) in module_items.iter().enumerate() {
        for (k, &it) in list.iter().enumerate() {
            let (lx, ly) = solved[m].0[k];
            let (ix, iy) = (f64::from(mx[m]) + lx, f64::from(my[m]) + ly);
            x[it as usize] = ix as f32;
            y[it as usize] = iy as f32;
            for &(id, dx, dy) in &rel[it as usize] {
                x[id as usize] = (f64::from(x[it as usize]) + dx) as f32;
                y[id as usize] = (f64::from(y[it as usize]) + dy) as f32;
                r[id as usize] = MEMBER_R;
            }
        }
    }

    // ---- hulls, faceted
    let module_hulls: Vec<Vec<[f64; 2]>> = parallel(
        n_mod,
        |m| module_items[m].len() as u64,
        |m| {
            let mut pts: Vec<[f64; 2]> = module_items[m]
                .iter()
                .flat_map(|&it| {
                    ring(
                        f64::from(x[it as usize]),
                        f64::from(y[it as usize]),
                        f64::from(r[it as usize]) + 0.7,
                        10,
                        0.3,
                    )
                })
                .collect();
            if pts.is_empty() {
                pts.extend(ring(f64::from(mx[m]), f64::from(my[m]), 1.5, 6, 0.0));
            }
            facet(hull(pts), 0.28)
        },
    );
    let modules: Vec<Territory> = (0..n_mod)
        .map(|m| {
            let hull = to_f32(&module_hulls[m]);
            Territory { x: mx[m], y: my[m], r: mr[m], bounds: Box2::around(&hull), hull }
        })
        .collect();
    let packages: Vec<Territory> = (0..n_pkg)
        .map(|p| {
            let mut pts: Vec<[f64; 2]> = package_modules[p]
                .iter()
                .flat_map(|&m| {
                    let (cx, cy) = (f64::from(mx[m as usize]), f64::from(my[m as usize]));
                    module_hulls[m as usize].iter().map(move |v| {
                        let (dx, dy) = (v[0] - cx, v[1] - cy);
                        let l = {
                            let l = dx.hypot(dy);
                            if l == 0.0 { 1.0 } else { l }
                        };
                        [v[0] + dx / l * 2.2, v[1] + dy / l * 2.2]
                    })
                })
                .collect();
            if pts.len() < 3 {
                pts.extend(ring(f64::from(px[p]), f64::from(py[p]), f64::from(pr[p]), 8, 0.0));
            }
            let hull = to_f32(&facet(hull(pts), 0.22));
            Territory { x: px[p], y: py[p], r: pr[p], bounds: Box2::around(&hull), hull }
        })
        .collect();
    let bounds = packages.iter().fold(Box2::EMPTY, |b, t| b.union(t.bounds));

    // Items per module, most important first (label and paint priority).
    let mut module_items = module_items;
    for list in &mut module_items {
        list.sort_by(|&a, &b| world.importance[b as usize].total_cmp(&world.importance[a as usize]));
    }
    Layout {
        x,
        y,
        r,
        modules,
        packages,
        module_items,
        package_modules,
        shells,
        shell_off,
        shell_members,
        module_edges,
        package_edges,
        bounds,
        key,
    }
}

/// Item edges rolled up to modules, then modules to packages (first-seen
/// order, self-edges dropped), as `layout.mjs` `modEdges` / `pkgEdges`.
#[must_use]
pub fn level_edges(world: &World) -> (Vec<Rollup>, Vec<Rollup>) {
    let roll = |pairs: &mut dyn Iterator<Item = (u32, u32, Rollup)>| {
        let mut index: HashMap<(u32, u32), usize> = HashMap::new();
        let mut out: Vec<Rollup> = Vec::new();
        for (a, b, e) in pairs {
            if a == b {
                continue;
            }
            if let Some(&k) = index.get(&(a, b)) {
                out[k].weight += e.weight;
                out[k].rel |= e.rel;
            } else {
                index.insert((a, b), out.len());
                out.push(Rollup { from: a, to: b, rel: e.rel, weight: e.weight });
            }
        }
        out
    };
    let nodes = &world.nodes;
    let modules = roll(&mut world
        .item_edges
        .iter()
        .map(|e| (nodes[e.from as usize].module, nodes[e.to as usize].module, *e)));
    let packages = roll(&mut modules
        .iter()
        .map(|e| (world.modules[e.from as usize].pkg, world.modules[e.to as usize].pkg, *e)));
    (modules, packages)
}

impl Layout {
    /// A layout from positions computed elsewhere (the prototype's
    /// `world.js`, for side-by-side comparison of the renderer alone):
    /// shells are recovered from the members' radii and angles, as app.js
    /// does.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)]
    pub fn from_positions(
        world: &World,
        x: Vec<f32>,
        y: Vec<f32>,
        r: Vec<f32>,
        modules: Vec<Territory>,
        packages: Vec<Territory>,
    ) -> Self {
        let nn = world.len();
        let mut module_items: Vec<Vec<NodeId>> = vec![Vec::new(); world.modules.len()];
        for &it in &world.items {
            module_items[world.nodes[it as usize].module as usize].push(it);
        }
        for list in &mut module_items {
            list.sort_by(|&a, &b| world.importance[b as usize].total_cmp(&world.importance[a as usize]));
        }
        let mut package_modules: Vec<Vec<u32>> = vec![Vec::new(); world.packages.len()];
        for (m, module) in world.modules.iter().enumerate() {
            package_modules[module.pkg as usize].push(m as u32);
        }
        let mut shells = Vec::new();
        let mut shell_off = vec![0_u32; nn + 1];
        let mut shell_members = Vec::new();
        for i in 0..nn as u32 {
            shell_off[i as usize] = shells.len() as u32;
            let kids = world.kids(i);
            if kids.is_empty() {
                continue;
            }
            let (cx, cy) = (x[i as usize], y[i as usize]);
            let mut by: std::collections::BTreeMap<i64, Vec<NodeId>> = std::collections::BTreeMap::new();
            for &j in kids {
                let d = (x[j as usize] - cx).hypot(y[j as usize] - cy);
                by.entry((d * 20.0).round() as i64).or_default().push(j);
            }
            for (key, mut list) in by {
                let angle = |j: NodeId| (y[j as usize] - cy).atan2(x[j as usize] - cx);
                list.sort_by(|&a, &b| angle(a).total_cmp(&angle(b)));
                let start = shell_members.len() as u32;
                shell_members.extend(&list);
                #[allow(clippy::cast_precision_loss)]
                shells.push(Shell { r: key as f32 / 20.0, start, len: list.len() as u32 });
            }
        }
        shell_off[nn] = shells.len() as u32;
        let (module_edges, package_edges) = level_edges(world);
        let bounds = packages.iter().fold(Box2::EMPTY, |b, t| b.union(t.bounds));
        Self {
            x,
            y,
            r,
            modules,
            packages,
            module_items,
            package_modules,
            shells,
            shell_off,
            shell_members,
            module_edges,
            package_edges,
            bounds,
            key: [0; 32],
        }
    }
}

type Pending = Arc<(Mutex<Option<Arc<Layout>>>, std::sync::Condvar)>;

/// Layouts by content hash, shared by every window (computed once each).
fn store() -> &'static Mutex<HashMap<[u8; 32], Pending>> {
    static STORE: OnceLock<Mutex<HashMap<[u8; 32], Pending>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The layout of `world`, computed at most once per content hash per
/// process; concurrent callers for the same hash wait for the first.
/// Blocking: call it off the UI thread (see [`cached`]).
#[must_use]
pub fn layout_of(world: &World) -> Arc<Layout> {
    let key = key(world);
    let (slot, first) = {
        let mut map = match store().lock() {
            Ok(map) => map,
            Err(poisoned) => poisoned.into_inner(),
        };
        match map.get(&key) {
            Some(slot) => (Arc::clone(slot), false),
            None => {
                let slot: Pending = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
                map.insert(key, Arc::clone(&slot));
                (slot, true)
            }
        }
    };
    let (lock, ready) = &*slot;
    if first {
        let layout = Arc::new(compute(world, key));
        let mut value = match lock.lock() {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        };
        *value = Some(Arc::clone(&layout));
        ready.notify_all();
        return layout;
    }
    let mut value = match lock.lock() {
        Ok(value) => value,
        Err(poisoned) => poisoned.into_inner(),
    };
    loop {
        if let Some(layout) = value.as_ref() {
            return Arc::clone(layout);
        }
        value = match ready.wait(value) {
            Ok(value) => value,
            Err(poisoned) => poisoned.into_inner(),
        };
    }
}

/// The layout of `world` on the background executor (instant when this
/// content hash was laid out before).
pub fn cached(world: Arc<World>, cx: &gpui::App) -> gpui::Task<Arc<Layout>> {
    cx.background_executor().spawn(async move { layout_of(&world) })
}

#[cfg(test)]
mod tests;

/// Test worlds shared by the graph's tests.
#[cfg(test)]
pub(crate) mod tests_support {
    pub(crate) use super::tests::synthetic;
}

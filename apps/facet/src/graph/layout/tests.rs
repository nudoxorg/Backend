use super::{Layout, MEMBER_R, compute, key, layout_of};
use crate::graph::model::{Edge, Kind, Module, Node, NodeId, Package, Rel, World, pagerank};
use std::f64::consts::PI;
use std::path::PathBuf;

/// A deterministic pseudo-random world: `packages` packages of a few
/// modules each, items of every kind with members, and relations that
/// mostly stay close (same module, same package) like real code.
pub(crate) fn synthetic(packages: u32, seed: u64) -> World {
    let mut s = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
    let mut next = move |n: u32| {
        s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        #[allow(clippy::cast_possible_truncation)]
        let v = ((s >> 33) % u64::from(n.max(1))) as u32;
        v
    };
    let kinds = [Kind::Struct, Kind::Enum, Kind::Trait, Kind::Function, Kind::Type, Kind::Constant, Kind::Macro];
    let mut pkgs = Vec::new();
    let mut modules = Vec::new();
    let mut nodes: Vec<Node> = Vec::new();
    let mut items_of_module: Vec<Vec<NodeId>> = Vec::new();
    for p in 0..packages {
        pkgs.push(Package {
            name: format!("backend-p{p}").into(),
            version: "0.1.0".into(),
            yours: p == 0,
            external: false,
            deps: vec![],
        });
        let mods = 1 + next(9);
        for m in 0..mods {
            let path = if m == 0 { String::new() } else if m % 3 == 0 { format!("m{}::sub{m}", m - 1) } else { format!("m{m}") };
            #[allow(clippy::cast_possible_truncation)]
            let module = modules.len() as u32;
            modules.push(Module { pkg: p, path: path.into(), file: "src/lib.rs".into() });
            let mut items = Vec::new();
            for k in 0..(1 + next(40)) {
                let kind = kinds[next(kinds.len() as u32) as usize];
                #[allow(clippy::cast_possible_truncation)]
                let id = nodes.len() as u32;
                let mut node = Node::new(kind, format!("I{p}_{m}_{k}"), p, module);
                node.line = k * 10;
                nodes.push(node);
                items.push(id);
                if kind.is_type_like() {
                    let (parts, methods) = if next(10) == 0 { (next(60), next(80)) } else { (next(8), next(12)) };
                    for f in 0..parts {
                        let part = if kind == Kind::Enum { Kind::Variant } else { Kind::Field };
                        let mut member = Node::new(part, format!("f{f}"), p, module).member_of(id);
                        member.line = k * 10 + f;
                        nodes.push(member);
                    }
                    for f in 0..methods {
                        let mut member = Node::new(Kind::Method, format!("m{f}"), p, module).member_of(id);
                        member.line = k * 10 + 100 + f;
                        nodes.push(member);
                    }
                }
            }
            items_of_module.push(items);
        }
    }
    let mut edges = Vec::new();
    #[allow(clippy::cast_possible_truncation)]
    let n = nodes.len() as u32;
    let rels = [Rel::HAS, Rel::TAKES, Rel::GIVES, Rel::CALLS, Rel::USES, Rel::TYPE, Rel::IMPL];
    let mut seen = std::collections::HashSet::new();
    for _ in 0..(n * 2) {
        let from = next(n);
        let m = nodes[from as usize].module as usize;
        let to = match next(4) {
            0 | 1 => {
                let list = &items_of_module[m];
                list[next(list.len() as u32) as usize]
            }
            _ => next(n),
        };
        if from != to && seen.insert((from, to)) {
            edges.push(Edge { from, to, rel: rels[next(rels.len() as u32) as usize] });
        }
    }
    match World::new(pkgs, modules, nodes, edges) {
        Ok(world) => world,
        Err(error) => panic!("synthetic world: {error}"),
    }
}

/// The real workspace. A missing file fails the test (a moved fixture must
/// not pass silently) unless `NUDOX_ALLOW_MISSING_WORLD=1`, which skips it
/// loudly.
pub(crate) fn fixture() -> Option<World> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph/world.json");
    let Ok(bytes) = std::fs::read(&path) else {
        assert!(
            std::env::var_os("NUDOX_ALLOW_MISSING_WORLD").is_some_and(|v| v == "1"),
            "world.json absent at {} (set NUDOX_ALLOW_MISSING_WORLD=1 to skip)",
            path.display()
        );
        eprintln!("SKIPPED: world.json absent at {}", path.display());
        return None;
    };
    match World::from_json(&bytes) {
        Ok(world) => Some(world),
        Err(error) => panic!("world.json: {error}"),
    }
}

fn bits(layout: &Layout) -> Vec<u32> {
    let mut out: Vec<u32> = Vec::new();
    for v in layout.x.iter().chain(&layout.y).chain(&layout.r) {
        out.push(v.to_bits());
    }
    for t in layout.modules.iter().chain(&layout.packages) {
        out.extend([t.x.to_bits(), t.y.to_bits(), t.r.to_bits()]);
        for p in &t.hull {
            out.extend([p[0].to_bits(), p[1].to_bits()]);
        }
    }
    out
}

#[test]
fn the_same_world_gives_byte_identical_positions() {
    let world = synthetic(8, 7);
    let (a, b) = (compute(&world, key(&world)), compute(&world, key(&world)));
    assert_eq!(bits(&a), bits(&b));
    // A different world moves things.
    let other = synthetic(8, 8);
    assert_ne!(key(&world), key(&other));
}

/// Pairs of sibling discs at one level: `(i, j, distance, ri + rj)`.
fn overlaps(centres: &[(f64, f64, f64)]) -> Vec<(usize, usize, f64, f64)> {
    let mut out = Vec::new();
    for i in 0..centres.len() {
        for j in i + 1..centres.len() {
            let (a, b) = (centres[i], centres[j]);
            let d = (a.0 - b.0).hypot(a.1 - b.1);
            if d < a.2 + b.2 - 1e-3 {
                out.push((i, j, d, a.2 + b.2));
            }
        }
    }
    out
}

fn assert_no_overlap(world: &World, layout: &Layout) {
    // Items in a module keep their discs (plus the 0.45 margin) apart.
    for (m, items) in layout.module_items.iter().enumerate() {
        let discs: Vec<_> = items
            .iter()
            .map(|&i| (f64::from(layout.x[i as usize]), f64::from(layout.y[i as usize]), f64::from(layout.r[i as usize]) + 0.45))
            .collect();
        let bad = overlaps(&discs);
        assert!(bad.is_empty(), "module {m} ({}): {} overlapping items, first {:?}", world.modules[m].path, bad.len(), bad.first());
    }
    // Modules in a package, by their enclosing radius (+1.2).
    for (p, mods) in layout.package_modules.iter().enumerate() {
        let discs: Vec<_> = mods
            .iter()
            .map(|&m| {
                let t = &layout.modules[m as usize];
                (f64::from(t.x), f64::from(t.y), f64::from(t.r) + 1.2)
            })
            .collect();
        let bad = overlaps(&discs);
        assert!(bad.is_empty(), "package {}: {} overlapping modules, first {:?}", world.packages[p].name, bad.len(), bad.first());
    }
    // Packages in the world (+10).
    let discs: Vec<_> = layout
        .packages
        .iter()
        .map(|t| (f64::from(t.x), f64::from(t.y), f64::from(t.r) + 10.0))
        .collect();
    let bad = overlaps(&discs);
    assert!(bad.is_empty(), "{} overlapping packages, first {:?}", bad.len(), bad.first());
    // And the claim that matters on screen: no two items anywhere overlap.
    let items: Vec<NodeId> = world.items.clone();
    let cell = 8.0_f32;
    let mut grid: std::collections::HashMap<(i32, i32), Vec<NodeId>> = std::collections::HashMap::new();
    for &i in &items {
        #[allow(clippy::cast_possible_truncation)]
        let c = ((layout.x[i as usize] / cell).floor() as i32, (layout.y[i as usize] / cell).floor() as i32);
        grid.entry(c).or_default().push(i);
    }
    let mut worst = 0.0_f32;
    for &i in &items {
        #[allow(clippy::cast_possible_truncation)]
        let c = ((layout.x[i as usize] / cell).floor() as i32, (layout.y[i as usize] / cell).floor() as i32);
        for dx in -1..=1 {
            for dy in -1..=1 {
                for &j in grid.get(&(c.0 + dx, c.1 + dy)).map_or(&[][..], Vec::as_slice) {
                    if j <= i {
                        continue;
                    }
                    let d = (layout.x[i as usize] - layout.x[j as usize]).hypot(layout.y[i as usize] - layout.y[j as usize]);
                    let need = layout.r[i as usize].min(cell / 2.0) + layout.r[j as usize].min(cell / 2.0);
                    worst = worst.max(need - d);
                }
            }
        }
    }
    assert!(worst <= 1e-3, "two items overlap by {worst} world units");
}

#[test]
fn no_level_overlaps_on_a_synthetic_world() {
    let world = synthetic(12, 3);
    let layout = compute(&world, key(&world));
    assert_no_overlap(&world, &layout);
}

#[test]
fn shells_are_evenly_spaced_polygons() {
    let world = synthetic(6, 11);
    let layout = compute(&world, key(&world));
    let mut checked = 0;
    for &i in &world.items {
        let shells = layout.shells_of(i);
        let (cx, cy) = (f64::from(layout.x[i as usize]), f64::from(layout.y[i as usize]));
        for (k, shell) in shells.iter().enumerate() {
            let members = layout.shell(shell);
            assert_eq!(members.len(), shell.len as usize);
            // Parts before methods: never a part on a shell outside a method shell.
            if k > 0 {
                let prev_methods = layout.shell(&shells[k - 1]).iter().any(|&j| world.node(j).kind == Kind::Method);
                let parts = members.iter().any(|&j| world.node(j).kind.is_part());
                assert!(!(prev_methods && parts), "item {i}: a part shell outside a method shell");
                assert!((shell.r - shells[k - 1].r - 0.72).abs() < 1e-4, "shell gap {} -> {}", shells[k - 1].r, shell.r);
            }
            #[allow(clippy::cast_precision_loss)]
            let step = 2.0 * PI / members.len() as f64;
            let angle = |j: NodeId| (f64::from(layout.y[j as usize]) - cy).atan2(f64::from(layout.x[j as usize]) - cx);
            for (q, &j) in members.iter().enumerate() {
                let rr = (f64::from(layout.x[j as usize]) - cx).hypot(f64::from(layout.y[j as usize]) - cy);
                assert!((rr - f64::from(shell.r)).abs() < 1e-3, "member {j} at {rr}, shell {}", shell.r);
                assert!((layout.r[j as usize] - MEMBER_R).abs() < f32::EPSILON);
                if members.len() > 1 {
                    let next = members[(q + 1) % members.len()];
                    let mut d = angle(next) - angle(j);
                    while d < 0.0 {
                        d += 2.0 * PI;
                    }
                    assert!((d - step).abs() < 1e-3, "item {i} shell {k}: step {d} expected {step}");
                }
            }
            checked += 1;
        }
        if let Some(last) = shells.last() {
            assert!((layout.r[i as usize] - (last.r + 0.4)).abs() < 1e-5, "an item's extent is its outer shell + 0.4");
        }
    }
    assert!(checked > 20, "only {checked} shells: the synthetic world has too few members");
}

/// Whether `(x, y)` is inside the convex polygon (either winding), with a
/// tolerance in world units.
fn inside(poly: &[[f32; 2]], x: f32, y: f32, tol: f32) -> bool {
    let n = poly.len();
    let mut sign = 0.0_f32;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let l = ex.hypot(ey).max(1e-9);
        let c = (ex * (y - a[1]) - ey * (x - a[0])) / l;
        if sign == 0.0 && c.abs() > tol {
            sign = c.signum();
        }
        if sign != 0.0 && c * sign < -tol {
            return false;
        }
    }
    true
}

fn assert_hulls_contain(world: &World, layout: &Layout) {
    for (m, items) in layout.module_items.iter().enumerate() {
        let hull = &layout.modules[m].hull;
        for &i in items {
            // The item's whole disc: eight points on its rim.
            for q in 0..8 {
                #[allow(clippy::cast_precision_loss)]
                let t = q as f32 * std::f32::consts::FRAC_PI_4;
                let (x, y) = (layout.x[i as usize] + layout.r[i as usize] * t.cos(), layout.y[i as usize] + layout.r[i as usize] * t.sin());
                assert!(inside(hull, x, y, 1e-3), "module {m} ({}) cuts item {} ({})", world.modules[m].path, i, world.node(i).name);
            }
            for &j in world.kids(i) {
                assert!(inside(hull, layout.x[j as usize], layout.y[j as usize], 1e-3), "module {m} cuts member {j}");
            }
        }
    }
    for (p, mods) in layout.package_modules.iter().enumerate() {
        let hull = &layout.packages[p].hull;
        for &m in mods {
            for v in &layout.modules[m as usize].hull {
                assert!(inside(hull, v[0], v[1], 1e-3), "package {} cuts module {m}", world.packages[p].name);
            }
        }
    }
}

#[test]
fn every_hull_contains_its_contents() {
    let world = synthetic(10, 5);
    let layout = compute(&world, key(&world));
    assert_hulls_contain(&world, &layout);
}

#[test]
fn hulls_are_faceted_not_round() {
    let world = synthetic(10, 5);
    let layout = compute(&world, key(&world));
    for t in layout.modules.iter().chain(&layout.packages) {
        let n = t.hull.len();
        if n <= 5 {
            continue;
        }
        for i in 0..n {
            let (a, b, c) = (t.hull[(i + n - 1) % n], t.hull[i], t.hull[(i + 1) % n]);
            let t1 = (b[1] - a[1]).atan2(b[0] - a[0]);
            let t2 = (c[1] - b[1]).atan2(c[0] - b[0]);
            let mut d = (t2 - t1).abs();
            if d > std::f32::consts::PI {
                d = 2.0 * std::f32::consts::PI - d;
            }
            assert!(d >= 0.2 - 1e-3, "a facet turns by only {d} rad");
        }
    }
}

#[test]
fn pagerank_sums_to_one() {
    let world = synthetic(9, 2);
    let pr = pagerank(world.len(), &world.items, &world.item_edges);
    let sum: f64 = world.items.iter().map(|&i| pr[i as usize]).sum();
    assert!((sum - 1.0).abs() < 1e-9, "PageRank sums to {sum}");
    assert!(world.items.iter().all(|&i| pr[i as usize] > 0.0));
}

#[test]
fn the_cache_computes_once_per_content_hash() {
    let world = synthetic(3, 21);
    let a = layout_of(&world);
    let b = layout_of(&world.clone());
    assert!(std::sync::Arc::ptr_eq(&a, &b));
}

/// The real workspace: all of the above at once, plus the timing (run in
/// release: `cargo test --release -p backend-facet --features graph-wip
/// graph::layout -- --nocapture`).
#[test]
fn the_real_workspace_lays_out_cleanly() {
    let parse = std::time::Instant::now();
    let Some(world) = fixture() else {
        return;
    };
    let parsed = parse.elapsed();
    let started = std::time::Instant::now();
    let layout = compute(&world, key(&world));
    let took = started.elapsed();
    let again = compute(&world, key(&world));
    assert_eq!(bits(&layout), bits(&again), "two runs differ");
    let pr = pagerank(world.len(), &world.items, &world.item_edges);
    let sum: f64 = world.items.iter().map(|&i| pr[i as usize]).sum();
    assert!((sum - 1.0).abs() < 1e-9, "PageRank sums to {sum}");
    assert_no_overlap(&world, &layout);
    assert_hulls_contain(&world, &layout);
    let radius = layout
        .packages
        .iter()
        .map(|t| t.x.hypot(t.y) + t.r)
        .fold(0.0_f32, f32::max);
    eprintln!(
        "layout: {} nodes, {} items, {} item edges, {} module edges, {} package edges, world radius {radius:.0}, {:.1} ms (world.json parse + model {:.1} ms)",
        world.len(),
        world.items.len(),
        world.item_edges.len(),
        layout.module_edges.len(),
        layout.package_edges.len(),
        took.as_secs_f64() * 1000.0,
        parsed.as_secs_f64() * 1000.0
    );
}

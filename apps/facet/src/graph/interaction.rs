//! Graph journeys: breadth-first change reach and the visible tour road.
//! Pure data; the view owns timing and the renderer owns paint.
use super::layout::{Box2, Layout};
use super::model::{Completeness, Kind, NodeId, World};
use std::collections::BTreeMap;

/// Estimate reliable release velocity in screen pixels per millisecond.
/// Coalesced events do not measure physical speed; the total coast remains
/// bounded by one third of the shorter viewport dimension.
pub(crate) fn release_velocity(samples: &[(std::time::Instant, f32, f32)], now: std::time::Instant, width: f32, height: f32) -> Option<(f64, f64)> {
    let newest = samples.last()?;
    if now.saturating_duration_since(newest.0) > std::time::Duration::from_millis(60) { return None; }
    let oldest = samples.iter().find(|sample| newest.0.saturating_duration_since(sample.0) <= std::time::Duration::from_millis(90))?;
    let span = newest.0.saturating_duration_since(oldest.0);
    if span < std::time::Duration::from_millis(16) { return None; }
    let milliseconds = span.as_secs_f64() * 1000.0;
    let (vx, vy) = (f64::from(newest.1 - oldest.1) / milliseconds, f64::from(newest.2 - oldest.2) / milliseconds);
    let speed = vx.hypot(vy);
    let extent = f64::from(width.min(height));
    if !speed.is_finite() || speed == 0.0 || !extent.is_finite() || extent <= 0.0 { return None; }
    let factor = (extent / 3.0 / super::camera::GLIDE_MS / speed).min(1.0);
    Some((vx * factor, vy * factor))
}

/// What would feel a change, counted once per top-level symbol.
#[derive(Clone, Debug)]
pub struct Reach {
    /// Relation completeness belongs to the represented world, not the count.
    completeness: Completeness,
    /// The changed symbol.
    pub source: NodeId,
    /// Breadth-first depth bins (including empty intermediate waves).
    pub waves: Vec<Vec<NodeId>>,
    /// True traversal depth for each top-level symbol (zero at the source).
    pub depth: Vec<Option<u8>>,
    /// All dependents, in wave order.
    pub all: Vec<NodeId>,
    /// The most important first-wave threads, capped once at construction.
    pub threads: Vec<NodeId>,
    /// Dependents belonging to your code.
    pub yours: usize,
    /// Packages and counts, largest first, with deterministic ties.
    pub packages: Vec<(u32, usize)>,
}

impl Reach {
    /// Every relation points from the dependent to its dependency. Parts
    /// pass change through their owner; methods pass it through callers.
    #[must_use]
    pub fn of(world: &World, source: NodeId) -> Self {
        let mut seen = vec![false; world.len()];
        let mut depth = vec![None; world.len()];
        depth[world.top(source) as usize] = Some(0);
        let mut front = vec![source];
        front.extend_from_slice(world.kids(source));
        if world.node(source).kind.is_part() && let Some(owner) = world.node(source).parent {
            front.push(owner);
        }
        for &i in &front { seen[i as usize] = true; }
        let mut waves = Vec::new();
        for d in 1..=8 {
            if front.is_empty() { break; }
            let mut next = Vec::new();
            let mut wave = Vec::new();
            for a in front {
                for (b, _) in world.in_edges(a) {
                    if seen[b as usize] || world.node(b).orphan { continue; }
                    seen[b as usize] = true;
                    next.push(b);
                    if matches!(world.node(b).kind, Kind::Field | Kind::Variant)
                        && let Some(owner) = world.node(b).parent
                        && !seen[owner as usize]
                    {
                        seen[owner as usize] = true;
                        next.push(owner);
                    }
                    let top = world.top(b);
                    if depth[top as usize].is_none() {
                        depth[top as usize] = Some(d);
                        wave.push(top);
                    }
                }
            }
            waves.push(wave);
            front = next;
        }
        while waves.last().is_some_and(Vec::is_empty) { waves.pop(); }
        let all: Vec<_> = waves.iter().flatten().copied().collect();
        let mut threads = waves.first().cloned().unwrap_or_default();
        threads.sort_by(|a, b| world.importance(*b).total_cmp(&world.importance(*a)).then(a.cmp(b)));
        threads.truncate(48);
        let yours = all.iter().filter(|&&i| world.yours(i)).count();
        let mut per = BTreeMap::new();
        for &i in &all { *per.entry(world.node(i).pkg).or_insert(0) += 1; }
        let mut packages: Vec<_> = per.into_iter().collect();
        packages.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Self { completeness: world.knowledge.relations, source, waves, depth, all, threads, yours, packages }
    }

    /// Frames the nearest 90% around the source, so one distant dependent
    /// cannot make the thing being changed vanish into the world.
    #[must_use]
    pub fn bounds(&self, layout: &Layout) -> Box2 {
        let source = [layout.x[self.source as usize], layout.y[self.source as usize]];
        let mut points: Vec<_> = self.all.iter().map(|&i| {
            let p = [layout.x[i as usize], layout.y[i as usize]];
            ((p[0] - source[0]).hypot(p[1] - source[1]), p)
        }).collect();
        points.push((0.0, source));
        points.sort_by(|a, b| a.0.total_cmp(&b.0));
        let percentile = ((points.len() - 1) * 9 / 10).max(usize::from(points.len() > 1));
        let limit = points[percentile].0;
        points.into_iter().filter(|(d, _)| *d <= limit + 1e-6)
            .fold(Box2::EMPTY.with(source[0] - 1.0, source[1] - 1.0).with(source[0] + 1.0, source[1] + 1.0), |b, (_, p)| b.with(p[0], p[1]))
    }

    /// An empty traversal is an absence claim only with complete relation
    /// evidence for this represented world.
    #[must_use]
    pub fn evidence(&self) -> ReachEvidence {
        if !self.all.is_empty() { ReachEvidence::Observed }
        else if self.completeness == Completeness::Complete { ReachEvidence::KnownEmpty }
        else { ReachEvidence::Unknown }
    }

    /// The quiet sentence in the focus card.
    #[must_use]
    pub fn summary(&self) -> String {
        match self.evidence() {
            ReachEvidence::KnownEmpty => return "No declaration in this complete graph projection depends on it.".to_owned(),
            ReachEvidence::Unknown => return "No dependents observed in this graph; relation coverage is unknown.".to_owned(),
            ReachEvidence::Observed => {},
        }
        let mut words = vec![format!("{} direct", self.waves[0].len())];
        if self.waves.len() > 1 { words.push(format!("{} within two steps", self.waves.iter().take(2).map(Vec::len).sum::<usize>())); }
        if self.waves.len() > 2 { words.push(format!("{} in all", self.all.len())); }
        words.push(format!("across {} package{}", self.packages.len(), if self.packages.len() == 1 { "" } else { "s" }));
        if self.yours > 0 { words.push(format!("{} in your code", self.yours)); }
        format!("Observed reach — {}", words.join(" · "))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReachEvidence { Observed, KnownEmpty, Unknown }

/// A package tour as the painter sees it.
#[derive(Clone, Debug)]
pub struct TourRoad {
    /// Stops, in reading order.
    pub stops: Vec<NodeId>,
    /// The current stop.
    pub at: usize,
}

#[cfg(test)]
mod tests {
    use super::Reach;
    #[test]
    fn no_observed_dependents_is_not_complete_absence() {
        let base = tiny();
        let mut world = World::new(base.packages, base.modules, base.nodes, vec![]).expect("observed empty graph");
        let node = 0;
        let unknown = Reach::of(&world, node);
        assert_eq!(unknown.evidence(), super::ReachEvidence::Unknown);
        assert!(unknown.summary().contains("coverage is unknown"));
        world.knowledge.relations = super::Completeness::Complete;
        let empty = Reach::of(&world, node);
        assert_eq!(empty.evidence(), super::ReachEvidence::KnownEmpty);
        assert!(empty.summary().contains("complete graph projection"));
    }

    #[test]
    fn release_rejects_coalesced_or_stale_events_and_bounds_coast() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        for span in [0, 1, 15] {
            let end = start + Duration::from_millis(span);
            assert!(super::release_velocity(&[(start, 0.0, 0.0), (end, 250.0, -50.0)], end, 1440.0, 900.0).is_none());
        }
        for (width, height) in [(1440.0, 900.0), (480.0, 400.0), (96.0, 64.0)] {
            let end = start + Duration::from_millis(32);
            let (vx, vy) = super::release_velocity(&[(start, 0.0, 0.0), (end, 250.0, -50.0)], end, width, height).expect("reliable span");
            assert!(vx.hypot(vy) * super::super::camera::GLIDE_MS <= f64::from(width.min(height)) / 3.0 + 1e-9);
            assert!(vx > 0.0 && vy < 0.0);
            assert!(super::release_velocity(&[(start, 0.0, 0.0), (end, 250.0, -50.0)], end + Duration::from_millis(61), width, height).is_none());
        }
    }

    use crate::graph::model::tests::tiny;
    use crate::graph::model::{Edge, Kind, Node, Rel, World};
    use crate::graph::layout::Layout;

    #[test]
    fn reach_counts_top_level_once_and_propagates_through_a_field_owner() {
        let base = tiny();
        let mut nodes = base.nodes.clone();
        nodes.push(Node::new(Kind::Function, "render", 0, 0));
        let mut edges = base.edges.clone();
        edges.push(Edge { from: 6, to: 0, rel: Rel::TAKES });
        let world = World::new(base.packages.clone(), base.modules.clone(), nodes, edges).expect("fixture");
        let reach = Reach::of(&world, 5);
        assert_eq!(reach.waves, vec![vec![0, 3], vec![6]]);
        assert_eq!(reach.depth[6], Some(2));
        assert_eq!(reach.yours, 2);
        assert_eq!(reach.packages, vec![(0, 2), (1, 1)]);
        assert_eq!(reach.summary(), "Observed reach — 2 direct · 3 within two steps · across 2 packages · 2 in your code");
    }

    #[test]
    fn sparse_waves_keep_true_distance_and_cycles_terminate() {
        let base = tiny();
        let mut nodes = base.nodes.clone();
        nodes.push(Node::new(Kind::Method, "middle", 0, 0).member_of(0));
        nodes.push(Node::new(Kind::Function, "caller", 1, 1));
        let edges = vec![
            Edge { from: 2, to: 5, rel: Rel::GIVES },
            Edge { from: 6, to: 2, rel: Rel::CALLS },
            Edge { from: 7, to: 6, rel: Rel::CALLS },
            Edge { from: 2, to: 7, rel: Rel::CALLS },
        ];
        let world = World::new(base.packages, base.modules, nodes, edges).expect("fixture");
        let reach = Reach::of(&world, 5);
        assert_eq!(reach.waves, vec![vec![0], vec![], vec![7]]);
        assert_eq!(reach.depth[7], Some(3));
        assert!(reach.summary().contains("1 within two steps · 2 in all"));
    }

    #[test]
    fn a_source_field_passes_through_its_owner_and_small_reach_has_extent() {
        let base = tiny();
        let mut edges = base.edges.clone();
        edges.push(Edge { from: 3, to: 0, rel: Rel::TAKES });
        let world = World::new(base.packages, base.modules, base.nodes, edges).expect("fixture");
        assert_eq!(Reach::of(&world, 1).all, vec![3]);
        let layout = Layout::compute(&world);
        let reach = Reach::of(&world, 1);
        let bounds = reach.bounds(&layout);
        assert!(bounds.x0 <= layout.x[3] && bounds.x1 >= layout.x[3]);
        assert!(bounds.y0 <= layout.y[3] && bounds.y1 >= layout.y[3]);
        assert!(bounds.x1 > bounds.x0 && bounds.y1 > bounds.y0);
    }

    #[test]
    fn source_family_is_not_its_own_dependent() {
        let world = tiny();
        assert!(Reach::of(&world, 0).all.is_empty());
        assert_eq!(Reach::of(&world, 3).all, vec![0]);
    }
}

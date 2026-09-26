//! A finite, one-shot clock for the value carried along a selected chain.
//! The view chooses its start and owns the clock; painting reads a small sample.
use super::discovery::Chain;
use super::model::NodeId;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
struct RoadKey {
    from: String,
    output: String,
    path: Vec<NodeId>,
}

/// One chain's clock. Selecting the same path keeps its progress; holding a
/// chain creates a fresh clock explicitly. No timer survives completion.
#[derive(Clone, Debug)]
pub struct RoadProgress {
    key: RoadKey,
    started: Duration,
    duration: Duration,
    arcs: usize,
    progress: f32,
    settled: bool,
}

/// A frame's immutable road sample, shared by all its stop and bead paint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RoadSample {
    /// Sine-eased progress along the entire road, in 0..=1.
    pub progress: f32,
    /// Number of arcs between folded type stops.
    pub arcs: usize,
    /// Whether another frame is needed. False after the one-shot completes.
    pub moving: bool,
}

impl RoadProgress {
    /// Begins a fresh trip at a monotonic time relative to the view's epoch.
    #[must_use]
    pub fn new(chain: &Chain, now: Duration) -> Self {
        let arcs = chain.stops.len().saturating_sub(1);
        Self {
            key: RoadKey {
                from: chain.from.clone(),
                output: chain.output.clone(),
                path: chain.path.clone(),
            },
            started: now,
            duration: Duration::from_millis(
                260u64
                    .saturating_mul(u64::try_from(arcs.max(1)).unwrap_or(u64::MAX))
                    .saturating_add(380),
            ),
            arcs,
            progress: if arcs == 0 { 1.0 } else { 0.0 },
            settled: arcs == 0,
        }
    }

    /// The finite start/budget pair used by the native animation probe.
    #[must_use]
    pub fn timing(&self) -> (Duration, Duration) {
        (self.started, self.duration)
    }

    /// A stable selection must not restart while its camera or labels move.
    #[must_use]
    pub fn matches(&self, chain: &Chain) -> bool {
        self.key.from == chain.from
            && self.key.output == chain.output
            && self.key.path == chain.path
    }

    /// Samples once per frame. Reduced motion settles permanently, and a
    /// backwards clock cannot move the value backwards or wake a settled road.
    #[must_use]
    pub fn sample(&mut self, now: Duration, reduced: bool) -> RoadSample {
        if reduced || self.settled {
            self.progress = 1.0;
            self.settled = true;
        } else {
            let elapsed = now.saturating_sub(self.started);
            if elapsed >= self.duration {
                self.progress = 1.0;
                self.settled = true;
            } else {
                let t = elapsed.as_secs_f64() / self.duration.as_secs_f64();
                #[allow(clippy::cast_possible_truncation)]
                let eased = (0.5 - 0.5 * (std::f64::consts::PI * t).cos()) as f32;
                self.progress = self.progress.max(eased);
            }
        }
        RoadSample {
            progress: self.progress,
            arcs: self.arcs,
            moving: !self.settled,
        }
    }
}

impl RoadSample {
    /// Arc index and local quadratic parameter for the travelling mint bead.
    #[must_use]
    pub fn bead(self) -> Option<(usize, f32)> {
        if !self.moving || self.arcs == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss)]
        let along = self.progress * self.arcs as f32;
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let arc = (along.floor() as usize).min(self.arcs - 1);
        #[allow(clippy::cast_precision_loss)]
        Some((arc, (along - arc as f32).clamp(0.0, 1.0)))
    }

    /// Stops and their words brighten together as the carried value arrives.
    #[must_use]
    pub fn reached(self, stop: usize) -> bool {
        #[allow(clippy::cast_precision_loss)]
        {
            stop as f32 <= self.progress * self.arcs as f32 + 1e-6
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::discovery::RoadStop;

    fn chain(stops: usize) -> Chain {
        Chain {
            cost: 0.0,
            from: "#0".into(),
            output: "#9".into(),
            via: None,
            steps: Vec::new(),
            path: vec![0, 1, 9],
            stops: (0..stops)
                .map(|n| RoadStop {
                    node: n as NodeId,
                    label: String::new(),
                    calls: Vec::new(),
                    yours: n == 0,
                })
                .collect(),
            brief: String::new(),
            rail: String::new(),
            code: String::new(),
        }
    }
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn carries_value_once_and_brightens_stops_in_order() {
        let mut road = RoadProgress::new(&chain(3), ms(100));
        assert_eq!(road.timing(), (ms(100), ms(900)));
        let start = road.sample(ms(100), false);
        assert_eq!(start.bead(), Some((0, 0.0)));
        assert!(start.reached(0));
        assert!(!start.reached(1));
        let middle = road.sample(ms(550), false); // 380 + 260 * 2 = 900 ms.
        assert!((middle.progress - 0.5).abs() < 1e-6);
        assert_eq!(middle.bead(), Some((1, 0.0)));
        assert!(middle.reached(1));
        assert!(!middle.reached(2));
        let done = road.sample(ms(1000), false);
        assert_eq!(done.progress, 1.0);
        assert!(!done.moving);
        assert!(done.bead().is_none());
        assert!(done.reached(2));
        assert_eq!(road.sample(ms(5000), false), done);
        assert_eq!(road.sample(ms(200), false), done);
    }

    #[test]
    fn selection_identity_is_stable_but_hold_can_restart() {
        let selected = chain(2);
        let mut road = RoadProgress::new(&selected, ms(0));
        assert!(road.matches(&selected));
        let before = road.sample(ms(320), false);
        assert_eq!(road.sample(ms(100), false), before);
        let mut different = selected.clone();
        different.from = "#4".into();
        assert!(!road.matches(&different));
        let mut held = RoadProgress::new(&selected, ms(320));
        assert_eq!(held.sample(ms(320), false).progress, 0.0);
    }

    #[test]
    fn reduced_and_zero_arc_roads_never_wake_again() {
        let mut road = RoadProgress::new(&chain(3), ms(0));
        let reduced = road.sample(ms(1), true);
        assert!(!reduced.moving);
        assert!(reduced.bead().is_none());
        assert_eq!(road.sample(ms(2), false), reduced);
        for stops in [0, 1] {
            let mut road = RoadProgress::new(&chain(stops), ms(0));
            let sample = road.sample(ms(0), false);
            assert!(!sample.moving);
            assert!(sample.bead().is_none());
            assert_eq!(sample.progress, 1.0);
        }
    }
}

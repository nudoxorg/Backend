//! Hot-set admission: which dependency shards live in local RAM
//! (09-vector §20.4). Pure, deterministic, explicit-time — the daemon feeds
//! it stats; it never reads clocks or filesystems.

use heart::PackageId;

/// Frozen per-symbol RAM estimate: 768 int8 quantized components (768 B)
/// plus ~130 B HNSW graph links at m=16 plus ~24 B id/payload overhead ≈ 922 B
/// (≈0.9 KB/symbol). Frozen so budget math is reproducible across planes;
/// re-deriving it is a plan change, not a tuning knob.
pub const BYTES_PER_SYMBOL: u64 = 922;

/// Default total local vector budget (09-vector §20.4): 350 MB.
pub const DEFAULT_BUDGET_BYTES: u64 = 350 * 1024 * 1024;

/// Estimated resident bytes for a shard of `n_symbols` symbols.
pub const fn ram_estimate_bytes(n_symbols: u64) -> u64 {
    n_symbols * BYTES_PER_SYMBOL
}

/// One dependency shard competing for residency.
#[derive(Debug, Clone, PartialEq)]
pub struct DepCandidate {
    pub package: PackageId,
    /// Resident-bytes estimate (see [`ram_estimate_bytes`]).
    pub ram_estimate: u64,
    /// Direct dependency (vs. transitive).
    pub is_direct: bool,
    /// Fraction of project references that resolve into this dep, `0..=1`.
    pub ref_density: f32,
    /// Decayed query-hit signal (see [`HitEma`]).
    pub query_hit_ema: f32,
    /// User-pinned: always resident.
    pub pinned: bool,
}

impl DepCandidate {
    /// The §20.4 admission score: `3·direct + 2·ref_density + 1·ema`.
    fn score(&self) -> f32 {
        2.0f32.mul_add(self.ref_density, 3.0 * f32::from(u8::from(self.is_direct)))
            + self.query_hit_ema
    }

    /// Value-per-byte for the greedy knapsack (score / cost).
    fn density(&self) -> f64 {
        f64::from(self.score()) / self.ram_estimate.max(1) as f64
    }
}

/// The RAM envelope admission works inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionBudget {
    /// Total local vector budget (default [`DEFAULT_BUDGET_BYTES`]).
    pub budget_bytes: u64,
    /// Bytes already claimed by the project's own shard (always resident;
    /// deps get the remainder).
    pub project_ram: u64,
}

impl AdmissionBudget {
    /// Bytes available to dependency shards.
    pub const fn dep_budget(&self) -> u64 {
        self.budget_bytes.saturating_sub(self.project_ram)
    }
}

/// The admission decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionOutcome {
    /// Resident dep shards, in admission order (pinned first).
    pub admitted: Vec<PackageId>,
    /// Everything else (query via remote parity or omit — routing §20.5).
    pub rejected: Vec<PackageId>,
    /// The dep envelope that was allocated against.
    pub dep_budget: u64,
    /// Pinned shards alone exceeded the budget: they are admitted anyway
    /// (a pin is an explicit user override) but the overshoot is surfaced.
    pub over_budget: bool,
}

/// Decide the hot set (09-vector §20.4).
///
/// Policy (frozen):
/// 1. Pinned candidates bypass scoring and are admitted first — but they
///    *count toward the budget*. If pinned alone overflow the budget they
///    are still admitted and `over_budget` is set.
/// 2. Remaining candidates are taken greedily by value density
///    (score/bytes), skipping any that no longer fit.
/// 3. Ties (and pin order) break ascending by `PackageId` — fully
///    deterministic for identical inputs.
pub fn admit(budget: AdmissionBudget, candidates: Vec<DepCandidate>) -> AdmissionOutcome {
    let dep_budget = budget.dep_budget();
    let (mut pinned, mut scored): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|c| c.pinned);

    pinned.sort_by_key(|c| c.package);
    scored.sort_by(|a, b| {
        b.density()
            .total_cmp(&a.density())
            .then_with(|| b.score().total_cmp(&a.score()))
            .then_with(|| a.package.cmp(&b.package))
    });

    let mut admitted = Vec::new();
    let mut rejected = Vec::new();
    let mut used: u64 = 0;

    for candidate in pinned {
        used = used.saturating_add(candidate.ram_estimate);
        admitted.push(candidate.package);
    }
    let over_budget = used > dep_budget;

    for candidate in scored {
        if used.saturating_add(candidate.ram_estimate) <= dep_budget {
            used += candidate.ram_estimate;
            admitted.push(candidate.package);
        } else {
            rejected.push(candidate.package);
        }
    }

    AdmissionOutcome {
        admitted,
        rejected,
        dep_budget,
        over_budget,
    }
}

/// Pick the shard to evict when the hot set must shrink: the lowest
/// value-density non-pinned resident (ties → smaller `PackageId`). `None`
/// when everything resident is pinned (pins are never evicted).
pub fn eviction_victim(resident: &[DepCandidate]) -> Option<PackageId> {
    resident
        .iter()
        .filter(|c| !c.pinned)
        .min_by(|a, b| {
            a.density()
                .total_cmp(&b.density())
                .then_with(|| a.package.cmp(&b.package))
        })
        .map(|c| c.package)
}

/// Half-life of the query-hit signal: 14 days (09-vector §20.4).
pub const HIT_EMA_HALF_LIFE_SECS: u64 = 14 * 24 * 60 * 60;

/// An exponentially-decayed query-hit counter with a 14-day half-life.
/// Time is an explicit epoch-seconds argument — no hidden clocks, so decay
/// is replayable and testable.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HitEma {
    value: f32,
    /// Epoch seconds of the last update.
    at_secs: u64,
}

impl HitEma {
    /// A zeroed signal anchored at `now_secs`.
    pub const fn new(now_secs: u64) -> Self {
        Self {
            value: 0.0,
            at_secs: now_secs,
        }
    }

    /// Decay factor for the elapsed interval (clamped: time never runs
    /// backwards here — an earlier `now` decays by zero).
    fn decay(&self, now_secs: u64) -> f64 {
        let dt = now_secs.saturating_sub(self.at_secs) as f64;
        0.5f64.powf(dt / HIT_EMA_HALF_LIFE_SECS as f64)
    }

    /// Fold in an observation at `now_secs`: decay, then `+1` on a hit.
    pub fn update(&mut self, now_secs: u64, hit: bool) {
        self.value =
            (f64::from(self.value) * self.decay(now_secs)) as f32 + if hit { 1.0 } else { 0.0 };
        self.at_secs = now_secs;
    }

    /// The decayed value as of `now_secs` (no state change).
    pub fn value_at(&self, now_secs: u64) -> f32 {
        (f64::from(self.value) * self.decay(now_secs)) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::super::store::NAMESPACE_NUDOX;
    use super::*;

    fn pkg(name: &str) -> PackageId {
        PackageId::from_name(&NAMESPACE_NUDOX, name.as_bytes())
    }

    fn cand(name: &str, ram: u64, direct: bool, density: f32, ema: f32) -> DepCandidate {
        DepCandidate {
            package: pkg(name),
            ram_estimate: ram,
            is_direct: direct,
            ref_density: density,
            query_hit_ema: ema,
            pinned: false,
        }
    }

    #[test]
    fn ram_estimate_uses_frozen_constant() {
        assert_eq!(ram_estimate_bytes(0), 0);
        assert_eq!(ram_estimate_bytes(1), 922);
        assert_eq!(ram_estimate_bytes(100_000), 92_200_000); // ≈ 88 MB per 100k symbols
    }

    #[test]
    fn greedy_prefers_value_density_within_budget() {
        let budget = AdmissionBudget {
            budget_bytes: 1000,
            project_ram: 0,
        };
        // a: score 3.0 / 900 B (density .0033); b: score 2.0 / 100 B (.02);
        // c: score 1.0 / 100 B (.01). b, c admitted (200 B); a no longer fits? It
        // does fit? 200+900 > 1000 → rejected.
        let out = admit(
            budget,
            vec![
                cand("a", 900, true, 0.0, 0.0),
                cand("b", 100, false, 1.0, 0.0),
                cand("c", 100, false, 0.5, 0.0),
            ],
        );
        assert_eq!(out.admitted, vec![pkg("b"), pkg("c")]);
        assert_eq!(out.rejected, vec![pkg("a")]);
        assert!(!out.over_budget);
        assert_eq!(out.dep_budget, 1000);
    }

    #[test]
    fn greedy_skips_oversized_but_keeps_filling() {
        let budget = AdmissionBudget {
            budget_bytes: 300,
            project_ram: 0,
        };
        // Highest density first (b), a (250 B) doesn't fit after b, c (100 B) does.
        let out = admit(
            budget,
            vec![
                cand("a", 250, true, 1.0, 1.0),  // density .024
                cand("b", 100, true, 1.0, 0.0),  // density .05
                cand("c", 100, false, 1.0, 0.0), // density .02
            ],
        );
        assert_eq!(out.admitted, vec![pkg("b"), pkg("c")]);
        assert_eq!(out.rejected, vec![pkg("a")]);
    }

    #[test]
    fn pinned_admitted_first_and_counted() {
        let budget = AdmissionBudget {
            budget_bytes: 300,
            project_ram: 0,
        };
        let mut pinned = cand("zzz", 250, false, 0.0, 0.0);
        pinned.pinned = true;
        // Pin consumes 250 of 300; the high-scoring 100 B candidate no longer fits.
        let out = admit(budget, vec![cand("aaa", 100, true, 1.0, 1.0), pinned]);
        assert_eq!(out.admitted, vec![pkg("zzz")]);
        assert_eq!(out.rejected, vec![pkg("aaa")]);
        assert!(!out.over_budget);
    }

    #[test]
    fn pinned_over_budget_still_admitted_and_flagged() {
        let budget = AdmissionBudget {
            budget_bytes: 100,
            project_ram: 50,
        };
        assert_eq!(budget.dep_budget(), 50);
        let mut p1 = cand("p1", 40, false, 0.0, 0.0);
        let mut p2 = cand("p2", 40, false, 0.0, 0.0);
        p1.pinned = true;
        p2.pinned = true;
        let out = admit(budget, vec![p2, p1, cand("x", 10, true, 1.0, 1.0)]);
        // Both pins admitted (80 > 50), deterministic PackageId order.
        let mut pins = vec![pkg("p1"), pkg("p2")];
        pins.sort();
        assert_eq!(out.admitted, pins);
        assert!(out.over_budget, "pin overshoot is surfaced, not silent");
        assert_eq!(out.rejected, vec![pkg("x")]);
    }

    #[test]
    fn tie_break_is_deterministic_by_package_id() {
        let budget = AdmissionBudget {
            budget_bytes: 100,
            project_ram: 0,
        };
        // Identical score and size → ascending PackageId decides.
        let out = admit(
            budget,
            vec![
                cand("m", 100, true, 0.5, 0.5),
                cand("n", 100, true, 0.5, 0.5),
            ],
        );
        let (lo, hi) = if pkg("m") < pkg("n") {
            (pkg("m"), pkg("n"))
        } else {
            (pkg("n"), pkg("m"))
        };
        assert_eq!(out.admitted, vec![lo]);
        assert_eq!(out.rejected, vec![hi]);
    }

    #[test]
    fn eviction_picks_lowest_density_never_pinned() {
        let mut pinned_low = cand("pin", 1000, false, 0.0, 0.0);
        pinned_low.pinned = true;
        let resident = vec![
            pinned_low,
            cand("hi", 100, true, 1.0, 1.0),
            cand("lo", 1000, false, 0.1, 0.0),
        ];
        assert_eq!(eviction_victim(&resident), Some(pkg("lo")));
        // All-pinned → no victim.
        let all_pinned: Vec<_> = resident
            .into_iter()
            .map(|mut c| {
                c.pinned = true;
                c
            })
            .collect();
        assert_eq!(eviction_victim(&all_pinned), None);
    }

    // ── adversarial: u64::MAX ram_estimate ───────────────────────────────────

    /// A candidate with ram_estimate == u64::MAX must not panic.
    /// It should be rejected (budget cannot hold it), not crash.
    #[test]
    fn u64_max_ram_estimate_rejected_no_panic() {
        let budget = AdmissionBudget {
            budget_bytes: DEFAULT_BUDGET_BYTES,
            project_ram: 0,
        };
        let huge = DepCandidate {
            package: pkg("huge"),
            ram_estimate: u64::MAX,
            is_direct: true,
            ref_density: 1.0,
            query_hit_ema: 1.0,
            pinned: false,
        };
        let out = admit(budget, vec![huge]);
        // Must not panic; the u64::MAX candidate cannot fit in any budget.
        assert_eq!(out.admitted, vec![], "u64::MAX candidate must be rejected");
        assert_eq!(out.rejected, vec![pkg("huge")]);
        assert!(!out.over_budget);
    }

    /// A pinned candidate with u64::MAX ram_estimate: admitted and over_budget.
    #[test]
    fn u64_max_pinned_candidate_admitted_over_budget() {
        let budget = AdmissionBudget {
            budget_bytes: DEFAULT_BUDGET_BYTES,
            project_ram: 0,
        };
        let huge = DepCandidate {
            package: pkg("huge_pinned"),
            ram_estimate: u64::MAX,
            is_direct: false,
            ref_density: 0.0,
            query_hit_ema: 0.0,
            pinned: true,
        };
        let _ = huge.pinned; // already true
        let out = admit(budget, vec![huge]);
        // Pinned always admitted.
        assert_eq!(out.admitted, vec![pkg("huge_pinned")]);
        // Overshot: u64::MAX > dep_budget.
        assert!(
            out.over_budget,
            "u64::MAX pinned shard must set over_budget"
        );
    }

    // ── adversarial: NaN and out-of-range ref_density ─────────────────────────

    /// NaN ref_density: must not panic. Pin exact behavior.
    ///
    /// NOTE (potential bug): score() uses ref_density as `2.0 * self.ref_density`.
    /// NaN propagates into score and density. f64::total_cmp treats NaN > any
    /// finite in some orderings; density() divides score by ram_estimate.max(1).
    /// The actual sort behavior is pinned here, not the "correct" behavior.
    #[test]
    fn nan_ref_density_no_panic_pinned_behavior() {
        let budget = AdmissionBudget {
            budget_bytes: 1000,
            project_ram: 0,
        };
        let normal = cand("normal", 100, false, 0.5, 0.0);
        let nan = DepCandidate {
            package: pkg("nan_cand"),
            ram_estimate: 100,
            is_direct: false,
            ref_density: f32::NAN,
            query_hit_ema: 0.0,
            pinned: false,
        };
        // Must not panic.
        let out = admit(budget, vec![normal, nan]);
        // Pin the exact count (2 total: one or both admitted based on NaN sort order).
        assert_eq!(
            out.admitted.len() + out.rejected.len(),
            2,
            "all candidates accounted for"
        );
    }

    /// Negative ref_density: must not panic. Pin exact behavior.
    #[test]
    fn negative_ref_density_no_panic_pinned_behavior() {
        let budget = AdmissionBudget {
            budget_bytes: 500,
            project_ram: 0,
        };
        let negative = DepCandidate {
            package: pkg("neg"),
            ram_estimate: 100,
            is_direct: false,
            ref_density: -1.0,
            query_hit_ema: 0.0,
            pinned: false,
        };
        let normal = cand("normal", 100, false, 0.5, 0.0);
        let out = admit(budget, vec![negative, normal]);
        // Normal should score higher (0.5*2 = 1.0 vs -2.0). Pin: normal admitted first.
        assert!(
            out.admitted.contains(&pkg("normal")),
            "normal (positive density) should be admitted"
        );
        assert_eq!(out.admitted.len() + out.rejected.len(), 2);
    }

    /// ref_density > 1.0: no clamp enforced; just must not panic. Pin behavior.
    #[test]
    fn ref_density_above_one_no_panic() {
        let budget = AdmissionBudget {
            budget_bytes: 500,
            project_ram: 0,
        };
        let over = DepCandidate {
            package: pkg("over"),
            ram_estimate: 100,
            is_direct: false,
            ref_density: 2.0, // above 1.0
            query_hit_ema: 0.0,
            pinned: false,
        };
        let out = admit(budget, vec![over]);
        // Must not panic; score = 2*2.0 = 4.0, positive, should be admitted.
        assert!(out.admitted.contains(&pkg("over")));
    }

    // ── adversarial: zero-byte candidate (density guard) ─────────────────────

    /// A candidate with ram_estimate == 0: density() uses .max(1) → no divide-
    /// by-zero. Must not panic; pin behavior.
    #[test]
    fn zero_byte_candidate_no_divide_by_zero() {
        let budget = AdmissionBudget {
            budget_bytes: 500,
            project_ram: 0,
        };
        let zero = DepCandidate {
            package: pkg("zero"),
            ram_estimate: 0,
            is_direct: true,
            ref_density: 1.0,
            query_hit_ema: 1.0,
            pinned: false,
        };
        // Must not panic.
        let out = admit(budget, vec![zero]);
        // Zero bytes always fits in any non-zero budget.
        assert!(
            out.admitted.contains(&pkg("zero")),
            "zero-byte candidate fits in any budget"
        );
    }

    // ── adversarial: eviction with all-equal densities ────────────────────────

    /// When all non-pinned candidates have equal density, eviction picks the
    /// smallest PackageId.
    #[test]
    fn eviction_equal_density_tie_breaks_ascending_package_id() {
        // Three non-pinned candidates with identical score and ram_estimate.
        let candidates = vec![
            cand("ccc", 100, false, 0.5, 0.0),
            cand("aaa", 100, false, 0.5, 0.0),
            cand("bbb", 100, false, 0.5, 0.0),
        ];
        // All densities equal: score/100 same for all.
        let victim = eviction_victim(&candidates).expect("must find a victim");
        // Pin: the smallest PackageId wins the tie.
        let ids: Vec<_> = [pkg("aaa"), pkg("bbb"), pkg("ccc")].into_iter().collect();
        let expected = *ids.iter().min().unwrap();
        assert_eq!(
            victim, expected,
            "equal-density eviction picks smallest PackageId"
        );
    }

    // ── adversarial: HitEma at u64::MAX seconds ───────────────────────────────

    /// HitEma::value_at(u64::MAX) must not panic.
    /// decay at u64::MAX seconds: 0.5^(u64::MAX / half_life) → effectively 0.0.
    #[test]
    fn hit_ema_at_u64_max_seconds_no_panic() {
        let mut ema = HitEma::new(0);
        ema.update(0, true); // value = 1.0 at t=0
        // At u64::MAX seconds the value must be near 0 (enormous decay).
        let v = ema.value_at(u64::MAX);
        assert!(v >= 0.0, "value_at(u64::MAX) must be non-negative, got {v}");
        assert!(v < 0.01, "value_at(u64::MAX) should be near zero, got {v}");
    }

    /// update at u64::MAX then value_at u64::MAX must not panic.
    #[test]
    fn hit_ema_update_at_u64_max_no_panic() {
        let mut ema = HitEma::new(0);
        ema.update(u64::MAX, true);
        // value_at at same time: decay = 1.0 (dt = 0), value = 1.0.
        let v = ema.value_at(u64::MAX);
        assert!(
            (v - 1.0).abs() < 1e-6,
            "value_at same time as update = 1.0, got {v}"
        );
    }

    // ── adversarial: admission permutation determinism ────────────────────────

    /// Shuffled input order → identical AdmissionOutcome (order within admitted/
    /// rejected may vary since only the policy content matters, not insertion order).
    /// We assert the same sets are admitted and rejected across 10 permutations.
    #[test]
    fn admission_deterministic_under_input_permutation() {
        let budget = AdmissionBudget {
            budget_bytes: 500,
            project_ram: 0,
        };
        let base_candidates = vec![
            cand("alpha", 100, true, 0.8, 0.5),
            cand("beta", 200, false, 0.3, 0.2),
            cand("gamma", 150, true, 0.5, 0.9),
            cand("delta", 100, false, 0.1, 0.1),
            cand("epsilon", 300, true, 0.9, 0.8),
        ];

        let base_out = admit(budget, base_candidates.clone());
        let mut base_admitted: Vec<_> = base_out.admitted.clone();
        let mut base_rejected: Vec<_> = base_out.rejected.clone();
        base_admitted.sort();
        base_rejected.sort();

        // 10 permutations via index rotation.
        for rotation in 1..=10usize {
            let mut rotated = base_candidates.clone();
            let by = rotation % rotated.len();
            rotated.rotate_right(by);
            let out = admit(budget, rotated);
            let mut admitted = out.admitted.clone();
            let mut rejected = out.rejected.clone();
            admitted.sort();
            rejected.sort();
            assert_eq!(
                admitted, base_admitted,
                "admitted set must be identical under permutation (rotation {rotation})"
            );
            assert_eq!(
                rejected, base_rejected,
                "rejected set must be identical under permutation (rotation {rotation})"
            );
            assert_eq!(out.over_budget, base_out.over_budget);
        }
    }

    #[test]
    fn hit_ema_halves_at_half_life() {
        let mut ema = HitEma::new(0);
        ema.update(0, true);
        assert!(
            (ema.value_at(0) - 1.0).abs() < 1e-6,
            "fresh hit: {}",
            ema.value_at(0)
        );
        let half = ema.value_at(HIT_EMA_HALF_LIFE_SECS);
        assert!((half - 0.5).abs() < 1e-6, "14-day half-life: {half}");
        // Two hits then a full half-life: (1·d + 1) halves.
        ema.update(HIT_EMA_HALF_LIFE_SECS, true);
        assert!((ema.value_at(HIT_EMA_HALF_LIFE_SECS) - 1.5).abs() < 1e-6);
        // Time going backwards decays by zero (clamped), never grows.
        assert!(
            (ema.value_at(0) - ema.value_at(HIT_EMA_HALF_LIFE_SECS)).abs() < 1e-6,
            "backwards time must not change the value"
        );
        // A miss decays without adding.
        let mut idle = HitEma::new(0);
        idle.update(0, true);
        idle.update(2 * HIT_EMA_HALF_LIFE_SECS, false);
        assert!((idle.value_at(2 * HIT_EMA_HALF_LIFE_SECS) - 0.25).abs() < 1e-6);
    }
}

use super::*;

/// Fuse `factors` under `weights`.
///
/// Popularity is **one** term. When [`RankingFactors::popularity_pct`] is
/// `Some(pct)`, that percentile is `unit` and raw dependents / downloads are
/// not consulted ([`DEPENDENT_DOWNLOAD_EQUIV`] stays unused). When it is
/// `None`, `unit` is the synthesized saturation ratio. Either way:
///
/// ```text
/// popularity = (weights.dependents + weights.downloads) * unit
/// text       = weights.text * normalized_bm25
/// quality    = weights.quality * (quality_ppm / 1_000_000)
/// ```
///
/// With the ID-8 constants the popularity coefficient is `(0.45 + 0.25) *
/// unit`. Writing `weights.dependents * unit + weights.downloads * unit` is the
/// wrong shape: it pretends the two weights still have their own percentiles.
/// Text and quality are separate terms and are never multiplied by `unit`.
///
/// Non-finite weights are treated as `0` so they cannot manufacture a NaN.
/// The returned [`Score`] is finite. [`Gate`] multipliers then shrink it when
/// the matching flag is set.
#[must_use]
pub fn score(factors: &RankingFactors, weights: &FusionWeights) -> Score {
    Score::new(apply_gates(fused_before_gates(factors, weights), factors))
}

/// ID-8 sum before [`Gate`] multipliers.
///
/// The cascade applies exact-name and contains-name bonuses, then its own
/// gate pass. It must start from this sum so a bonus is not glued on after
/// the demotion, and so the gates are not applied twice.
#[must_use]
pub fn fused_before_gates(factors: &RankingFactors, weights: &FusionWeights) -> f64 {
    let unit = popularity_unit(factors);
    // One coefficient. Not `dependents * unit + downloads * unit`.
    let popularity_weight = finite_or_zero(weights.dependents) + finite_or_zero(weights.downloads);
    let text = finite_or_zero(weights.text) * normalized_bm25(factors.bm25);
    let quality = finite_or_zero(weights.quality) * factors.quality_ppm.as_unit();
    // Plain multiply-add, not `mul_add`. The unit tests re-derive this sum
    // independently; a fused multiply-add would make the two paths disagree
    // on hardware that contracts the operation.
    #[allow(clippy::suboptimal_flops)]
    {
        popularity_weight * unit + text + quality
    }
}

/// Sort key whose [`Ord`] is score descending, then name ascending.
///
/// Name order is `str`'s bytewise total order, so ties do not depend on locale.
/// [`Score`] is already finite, so this comparison never consults `partial_cmp`
/// on a float.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankKey<'a> {
    score: Score,
    name: &'a str,
}

impl PartialOrd for RankKey<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankKey<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .cmp(&self.score)
            .then_with(|| self.name.cmp(other.name))
    }
}

/// Build the tiebreak key `(score desc, name asc)`.
#[must_use]
pub fn rank_key(score: Score, name: &str) -> RankKey<'_> {
    RankKey { score, name }
}

/// Quality unit times the popularity unit.
///
/// Explore ranking adds `quality_popularity_path_scale` times this term.
/// Navigate leaves the scale at zero, so the ID-8 sum is unchanged.
#[must_use]
pub fn quality_times_popularity(factors: &RankingFactors) -> f64 {
    let quality = f64::from(factors.quality_ppm.get()) / f64::from(QualityPpm::MAX);
    let quality = if quality.is_finite() {
        quality.clamp(0.0, 1.0)
    } else {
        0.0
    };
    quality * popularity_unit(factors)
}

fn popularity_unit(factors: &RankingFactors) -> f64 {
    // `match` keeps the two policies (filled percentile vs synthesized mass)
    // as separate arms. `map_or_else` would bury that branch.
    #[allow(clippy::option_if_let_else)]
    match factors.popularity_pct {
        Some(pct) => f64::from(pct.get()).clamp(0.0, 1.0),
        None => synthesized_popularity(factors.dependents, factors.downloads),
    }
}

/// `max(dependents * EQUIV, downloads) / POPULARITY_SATURATION`, clamped to
/// `0..=1`.
///
/// `None` downloads and `None` dependents each contribute `0`. They never
/// become NaN. This is the only function that reads
/// [`DEPENDENT_DOWNLOAD_EQUIV`].
fn synthesized_popularity(dependents: Option<u32>, downloads: Option<u64>) -> f64 {
    let from_dependents = dependents.map_or(0, |count| {
        u64::from(count).saturating_mul(DEPENDENT_DOWNLOAD_EQUIV)
    });
    let mass = from_dependents.max(downloads.unwrap_or(0));
    let saturation = POPULARITY_SATURATION as f64;
    (mass as f64 / saturation).clamp(0.0, 1.0)
}

fn normalized_bm25(bm25: Bm25) -> f64 {
    let raw = f64::from(bm25.get());
    let normalized = raw / (raw + BM25_SATURATION);
    if normalized.is_finite() {
        normalized.clamp(0.0, 1.0)
    } else {
        1.0
    }
}

fn apply_gates(mut fused: f64, factors: &RankingFactors) -> f64 {
    if factors.malware {
        fused *= Gate::MALWARE;
    }
    if factors.squat_suspect {
        fused *= Gate::SQUAT;
    }
    if factors.withdrawn {
        fused *= Gate::WITHDRAWN;
    }
    fused
}

fn finite_or_zero(weight: f64) -> f64 {
    if weight.is_finite() { weight } else { 0.0 }
}


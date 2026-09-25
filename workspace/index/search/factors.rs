//! Typed factor plane for within-ecosystem ranking.
//!
//! The cascade module still ports the lib.rs post-processor (kink, diversity,
//! bubble). This module is the scoring core that port never grew: INDEX-PLAN
//! ID-8 as named weights on [`FusionWeights`], not a comment above a 1600-line
//! file.
//!
//! # Fusion
//!
//! ID-8 is `0.45*dependents_pct + 0.25*downloads_pct + 0.20*text +
//! 0.10*quality`. A candidate has **one** popularity unit, not two percentile
//! channels:
//!
//! * [`RankingFactors::popularity_pct`] `Some` — use that percentile directly.
//! * `None` — synthesize a unit in `0..=1` from `max(dependents *
//!   DEPENDENT_DOWNLOAD_EQUIV, downloads)` divided by
//!   [`POPULARITY_SATURATION`]. [`DEPENDENT_DOWNLOAD_EQUIV`] is read only on
//!   this path.
//!
//! Those two weights then collapse. The shape
//! `weights.dependents * unit + weights.downloads * unit` is the wrong reading:
//! it looks like two inputs. The popularity term is one coefficient,
//! `(DEPENDENTS + DOWNLOADS) * unit` (ID-8: `(0.45 + 0.25) * unit`). Text and
//! quality stay beside it:
//!
//! ```text
//! popularity = (dependents_weight + downloads_weight) * unit
//! text       = text_weight * normalized_bm25
//! quality    = quality_weight * (quality_ppm / 1_000_000)
//! fused      = popularity + text + quality
//! ```
//!
//! [`FusionWeights::TEXT`] scales **normalized** BM25 (see
//! [`BM25_SATURATION`]), never the raw score. [`FusionWeights::QUALITY`] scales
//! `quality_ppm / 1e6`.
//!
//! # Gates
//!
//! [`Gate::WITHDRAWN`] (`0.25`), [`Gate::SQUAT`], and [`Gate::MALWARE`]
//! multiply the fused score when the matching flag is set. They stack. They are
//! constants on [`Gate`] so a demotion cannot hide as a literal in the cascade.
//! `verified_repo`, `exact_name`, and `contains_name` ride along on
//! [`RankingFactors`] for the ranker; ID-8 has no coefficient for them, so
//! [`score`] does not read them.
//!
//! Pure: no I/O, no clock, no allocator in the scoring path. [`Score`]'s
//! [`Ord`] is total — non-finite floats are clamped away before they can reach
//! a sort.

#![warn(missing_docs)]

use std::{cmp::Ordering, num::FpCategory};

/// One active dependent, in download-equivalents, when no percentile is known.
///
/// Used only by the synthesized-popularity path (`popularity_pct == None`).
/// Aligned with `ranking::popularity::DEPENDENT_DOWNLOAD_EQUIV`.
pub const DEPENDENT_DOWNLOAD_EQUIV: u64 = 2_500;

/// Download-equivalent mass that saturates synthesized popularity at `1`.
///
/// The saturation constant for `max(dependents * DEPENDENT_DOWNLOAD_EQUIV,
/// downloads) / SATURATION`. `10_000_000` matches the synthetic ceiling a
/// percentile of `1.0` already stands for in `ranking::popularity`. Values at
/// or above this map to `1`; missing downloads contribute `0`, not a hole.
pub const POPULARITY_SATURATION: u64 = 10_000_000;

/// Raw BM25 that normalizes to `0.5`.
///
/// Text weight applies to `bm25 / (bm25 + BM25_SATURATION)`, clamped to
/// `0..=1`, not to the raw [`Bm25`]. Zero stays zero; the unit only reaches
/// `1` in the limit, so the text term cannot exceed [`FusionWeights::text`].
pub const BM25_SATURATION: f64 = 10.0;

// Saturation fits the f64 mantissa, so `mass / POPULARITY_SATURATION` is exact
// for every mass up to the constant itself. Dependents cannot overflow `u64`
// when scaled by the equivalence.
const _: () = assert!(POPULARITY_SATURATION <= (1_u64 << 53));
const _: () = assert!(DEPENDENT_DOWNLOAD_EQUIV <= u64::MAX / (u32::MAX as u64));
const _: () = assert!(BM25_SATURATION > 0.0);

/// Quality in parts-per-million, domain `0..=1_000_000`.
///
/// `1_000_000` is quality `1.0`. The ID-8 quality term divides by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualityPpm(u32);

impl QualityPpm {
    /// Inclusive upper bound. Also the divisor that maps ppm onto `0..=1`.
    pub const MAX: u32 = 1_000_000;

    /// `Some` when `ppm <= MAX`.
    #[must_use]
    pub const fn new(ppm: u32) -> Option<Self> {
        if ppm <= Self::MAX {
            Some(Self(ppm))
        } else {
            None
        }
    }

    /// The raw parts-per-million value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }

    /// `quality_ppm / 1_000_000` in `0..=1`.
    #[must_use]
    pub fn as_unit(self) -> f64 {
        f64::from(self.0) / f64::from(Self::MAX)
    }
}

/// Popularity percentile in `0..=1`.
///
/// `Some` means the offline CDF filled the percentile. That is different from
/// `None`, which means "unknown" and falls through to synthesized mass.
/// A stored `0.0` is a known-unpopular package, not missing data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopularityPct(f32);

impl PopularityPct {
    /// `Some` when `value` is finite and inside `0..=1`.
    ///
    /// `-0.0` is stored as `+0.0`. NaN and infinities are `None`.
    #[must_use]
    pub fn new(value: f32) -> Option<Self> {
        if !value.is_finite() || value < 0.0 || value > 1.0 {
            return None;
        }
        Some(Self(canonical_nonnegative_f32(value)))
    }

    /// The percentile in `0..=1`.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Eq for PopularityPct {}

/// Non-negative BM25 relevance from retrieval.
///
/// Unbounded above. [`score`] normalizes it before applying
/// [`FusionWeights::TEXT`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bm25(f32);

impl Bm25 {
    /// `Some` when `value` is finite and `>= 0`.
    ///
    /// `-0.0` is stored as `+0.0`. NaN and infinities are `None`.
    #[must_use]
    pub fn new(value: f32) -> Option<Self> {
        if !value.is_finite() || value < 0.0 {
            return None;
        }
        Some(Self(canonical_nonnegative_f32(value)))
    }

    /// The raw BM25 value.
    #[must_use]
    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Eq for Bm25 {}

/// Every signal the ID-8 fuse reads, plus the flags a ranker still needs.
///
/// Construct the newtypes with their constructors so a candidate cannot carry
/// a NaN percentile or a negative BM25 into [`score`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankingFactors {
    /// Retrieval relevance. Weighted only after normalization.
    pub bm25: Bm25,
    /// Quality in parts-per-million. Weighted as `ppm / 1_000_000`.
    pub quality_ppm: QualityPpm,
    /// Reverse-dependency count. Read only when [`Self::popularity_pct`] is
    /// `None`, and then only as `count * DEPENDENT_DOWNLOAD_EQUIV`.
    pub dependents: Option<u32>,
    /// Calibrated downloads. `None` is missing data (contributes `0` to the
    /// synthesized mass), not a NaN.
    pub downloads: Option<u64>,
    /// Pre-blended popularity percentile. When `Some`, raw dependents and
    /// downloads are ignored.
    pub popularity_pct: Option<PopularityPct>,
    /// Yanked or withdrawn. Multiplies the fused score by [`Gate::WITHDRAWN`].
    pub withdrawn: bool,
    /// Typosquat / name land-grab. Multiplies by [`Gate::SQUAT`].
    pub squat_suspect: bool,
    /// Known malware. Multiplies by [`Gate::MALWARE`].
    pub malware: bool,
    /// Declared repository path contains the package name.
    ///
    /// Carried for the ranker. [`score`] does not weight it.
    pub verified_repo: bool,
    /// The query matched the package name exactly.
    ///
    /// Carried for the ranker. [`score`] does not weight it.
    pub exact_name: bool,
    /// The query is contained in the package name.
    ///
    /// Carried for the ranker. [`score`] does not weight it.
    pub contains_name: bool,
}

/// ID-8 fusion coefficients.
///
/// [`Self::TEXT`] multiplies normalized BM25. [`Self::QUALITY`] multiplies
/// `quality_ppm / 1_000_000`. [`Self::DEPENDENTS`] and [`Self::DOWNLOADS`] are
/// not applied as two percentile channels; [`score`] adds them into one
/// popularity coefficient.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FusionWeights {
    /// Coefficient on the popularity unit, dependents share (ID-8: `0.45`).
    pub dependents: f64,
    /// Coefficient on the popularity unit, downloads share (ID-8: `0.25`).
    pub downloads: f64,
    /// Coefficient on normalized BM25 (ID-8: `0.20`).
    pub text: f64,
    /// Coefficient on `quality_ppm / 1_000_000` (ID-8: `0.10`).
    pub quality: f64,
}

impl FusionWeights {
    /// ID-8 dependents weight.
    pub const DEPENDENTS: f64 = 0.45;
    /// ID-8 downloads weight.
    pub const DOWNLOADS: f64 = 0.25;
    /// ID-8 text weight. Applies to normalized BM25, not the raw score.
    pub const TEXT: f64 = 0.20;
    /// ID-8 quality weight. Applies to `quality_ppm / 1_000_000`.
    pub const QUALITY: f64 = 0.10;

    /// The ID-8 weight vector. Dependents `0.45`, downloads `0.25`, text
    /// `0.20`, quality `0.10`.
    pub const ID8: Self = Self {
        dependents: Self::DEPENDENTS,
        downloads: Self::DOWNLOADS,
        text: Self::TEXT,
        quality: Self::QUALITY,
    };
}

impl Default for FusionWeights {
    fn default() -> Self {
        Self::ID8
    }
}

/// Multiplicative demotions applied after the ID-8 sum.
///
/// Constants, not cascade locals. Malware, squat, and withdrawn stack by
/// multiplication. None of them is zero, so a flagged package stays ordered
/// instead of collapsing into a tie at the origin.
pub struct Gate;

impl Gate {
    /// Withdrawn / yanked versions stay findable, at a quarter of the fused
    /// score.
    pub const WITHDRAWN: f64 = 0.25;
    /// Typosquat / name land-grab. Same default as
    /// `ranking::gates::GateConfig::squat_factor`.
    pub const SQUAT: f64 = 0.05;
    /// Known malware. Same default as
    /// `ranking::gates::GateConfig::malware_factor`.
    pub const MALWARE: f64 = 0.001;
}

/// A fused rank score.
///
/// The inner `f64` is always finite, and `-0.0` is stored as `+0.0`, so [`Ord`]
/// agrees with [`PartialEq`] and sorts are deterministic. NaN becomes `0`.
/// Infinities clamp to [`f64::MIN`] / [`f64::MAX`].
#[derive(Debug, Clone, Copy)]
pub struct Score(f64);

impl Score {
    /// Clamp `raw` onto a finite value. See the type-level contract.
    #[must_use]
    pub fn new(raw: f64) -> Self {
        let finite = match raw.classify() {
            FpCategory::Infinite if raw.is_sign_negative() => f64::MIN,
            FpCategory::Infinite => f64::MAX,
            FpCategory::Nan | FpCategory::Zero => 0.0,
            FpCategory::Normal | FpCategory::Subnormal => raw,
        };
        Self(finite)
    }

    /// The finite score.
    #[must_use]
    pub const fn get(self) -> f64 {
        self.0
    }
}

impl PartialEq for Score {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Score {}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

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
    let unit = popularity_unit(factors);
    // One coefficient. Not `dependents * unit + downloads * unit`.
    let popularity_weight = finite_or_zero(weights.dependents) + finite_or_zero(weights.downloads);
    let text = finite_or_zero(weights.text) * normalized_bm25(factors.bm25);
    let quality = finite_or_zero(weights.quality) * factors.quality_ppm.as_unit();
    // Plain multiply-add, not `mul_add`. The unit tests re-derive this sum
    // independently; a fused multiply-add would make the two paths disagree
    // on hardware that contracts the operation.
    #[allow(clippy::suboptimal_flops)]
    let fused = popularity_weight * unit + text + quality;
    Score::new(apply_gates(fused, factors))
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

fn canonical_nonnegative_f32(value: f32) -> f32 {
    if value.to_bits() == (-0.0f32).to_bits() {
        0.0
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::*;

    fn ppm(value: u32) -> QualityPpm {
        QualityPpm::new(value).expect("quality ppm in 0..=1_000_000")
    }

    fn bm25(value: f32) -> Bm25 {
        Bm25::new(value).expect("bm25 >= 0")
    }

    fn pct(value: f32) -> PopularityPct {
        PopularityPct::new(value).expect("popularity in 0..=1")
    }

    fn zeroed() -> RankingFactors {
        RankingFactors {
            bm25: bm25(0.0),
            quality_ppm: ppm(0),
            dependents: None,
            downloads: None,
            popularity_pct: None,
            withdrawn: false,
            squat_suspect: false,
            malware: false,
            verified_repo: false,
            exact_name: false,
            contains_name: false,
        }
    }

    fn id8_ceiling() -> f64 {
        let weights = FusionWeights::ID8;
        weights.dependents + weights.downloads + weights.text + weights.quality
    }

    fn assert_in_id8_bound(s: Score) {
        assert!(s.get().is_finite(), "score must be finite, got {}", s.get());
        assert!(s.get() >= 0.0, "score must be >= 0, got {}", s.get());
        let ceiling = id8_ceiling();
        assert!(
            s.get() <= ceiling + 1e-9,
            "score {} exceeds ID-8 ceiling {ceiling}",
            s.get()
        );
    }

    #[test]
    fn ties_break_score_desc_then_name_asc() {
        let mut low = zeroed();
        low.quality_ppm = ppm(100_000);
        let mut high = zeroed();
        high.quality_ppm = ppm(900_000);
        let low_s = score(&low, &FusionWeights::ID8);
        let high_s = score(&high, &FusionWeights::ID8);
        assert!(high_s > low_s);
        assert_eq!(low_s, score(&low, &FusionWeights::ID8));

        let mut rows = [
            ("zeta", low_s),
            ("alpha", low_s),
            ("mu", high_s),
            ("beta", low_s),
        ];
        rows.sort_by(|a, b| rank_key(a.1, a.0).cmp(&rank_key(b.1, b.0)));
        assert_eq!(rows.map(|(name, _)| name), ["mu", "alpha", "beta", "zeta"]);

        let mut again = [
            ("zeta", low_s),
            ("alpha", low_s),
            ("mu", high_s),
            ("beta", low_s),
        ];
        again.sort_by(|a, b| rank_key(a.1, a.0).cmp(&rank_key(b.1, b.0)));
        assert_eq!(rows, again);

        assert_eq!(
            rank_key(low_s, "alpha").cmp(&rank_key(low_s, "alpha")),
            Ordering::Equal
        );
    }

    #[test]
    fn withdrawn_multiplies_the_whole_score_by_a_quarter() {
        let mut clean = zeroed();
        clean.quality_ppm = ppm(800_000);
        clean.bm25 = bm25(4.0);
        clean.downloads = Some(100_000);
        let clean_s = score(&clean, &FusionWeights::ID8);

        let mut yanked = clean;
        yanked.withdrawn = true;
        let yanked_s = score(&yanked, &FusionWeights::ID8);

        assert_eq!(yanked_s, Score::new(clean_s.get() * Gate::WITHDRAWN));
        assert!(yanked_s < clean_s);
        assert!((yanked_s.get() / clean_s.get() - 0.25).abs() < 1e-12);
    }

    #[test]
    fn none_downloads_does_not_nan() {
        let missing = zeroed();
        let missing_s = score(&missing, &FusionWeights::ID8);
        assert!(missing_s.get().is_finite());
        assert!(!missing_s.get().is_nan());
        assert_eq!(missing_s, Score::new(0.0));

        let mut deps_only = missing;
        deps_only.downloads = None;
        deps_only.dependents = Some(100);
        let got = score(&deps_only, &FusionWeights::ID8);
        assert!(got.get().is_finite());
        assert!(!got.get().is_nan());

        let mass = (100_u64).saturating_mul(DEPENDENT_DOWNLOAD_EQUIV) as f64;
        let unit = mass / POPULARITY_SATURATION as f64;
        let expect = Score::new((FusionWeights::DEPENDENTS + FusionWeights::DOWNLOADS) * unit);
        assert_eq!(got, expect);
        assert!(got > missing_s);
    }

    #[test]
    fn score_is_finite_and_inside_the_id8_bound() {
        // Small table: numeric corners × gate flags. The loop is the property
        // (every combination is finite and bounded); the rows are the generator.
        let qualities = [0_u32, 1_000_000];
        let bm25s = [0.0_f32, 10.0, 100.0];
        let percentiles: [Option<f32>; 4] = [None, Some(0.0), Some(0.5), Some(1.0)];
        let downloads: [Option<u64>; 4] = [None, Some(0), Some(10_000_000), Some(u64::MAX)];
        let dependents: [Option<u32>; 3] = [None, Some(0), Some(4_000)];
        let flags = [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true),
            (true, true, true),
        ];

        let mut checked = 0_u32;
        for &quality in &qualities {
            for &raw_bm25 in &bm25s {
                for &percentile in &percentiles {
                    for &download in &downloads {
                        for &dependent in &dependents {
                            for &(withdrawn, squat_suspect, malware) in &flags {
                                let factors = RankingFactors {
                                    bm25: bm25(raw_bm25),
                                    quality_ppm: ppm(quality),
                                    dependents: dependent,
                                    downloads: download,
                                    popularity_pct: percentile.map(pct),
                                    withdrawn,
                                    squat_suspect,
                                    malware,
                                    verified_repo: false,
                                    exact_name: false,
                                    contains_name: false,
                                };
                                assert_in_id8_bound(score(&factors, &FusionWeights::ID8));
                                checked += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(checked, 2 * 3 * 4 * 4 * 3 * 5);
    }

    #[test]
    fn percentile_is_one_term_and_ignores_raw_mass() {
        let mut with_raw = zeroed();
        with_raw.popularity_pct = Some(pct(0.4));
        with_raw.downloads = Some(u64::MAX);
        with_raw.dependents = Some(u32::MAX);
        with_raw.bm25 = bm25(10.0);
        with_raw.quality_ppm = ppm(250_000);

        let mut bare = with_raw;
        bare.downloads = None;
        bare.dependents = None;
        assert_eq!(
            score(&with_raw, &FusionWeights::ID8),
            score(&bare, &FusionWeights::ID8),
            "a filled percentile must not stack on top of downloads or dependents"
        );

        let unit = f64::from(pct(0.4).get());
        let text = FusionWeights::TEXT * (10.0 / (10.0 + BM25_SATURATION));
        let quality = FusionWeights::QUALITY * (250_000.0 / 1_000_000.0);
        let popularity = (FusionWeights::DEPENDENTS + FusionWeights::DOWNLOADS) * unit;
        let expect = Score::new(popularity + text + quality);
        assert_eq!(score(&bare, &FusionWeights::ID8), expect);
    }

    #[test]
    fn known_zero_percentile_is_not_missing_data() {
        let mut known = zeroed();
        known.popularity_pct = Some(pct(0.0));
        known.downloads = Some(POPULARITY_SATURATION);
        known.dependents = Some(10_000);

        let mut missing = known;
        missing.popularity_pct = None;

        assert_eq!(score(&known, &FusionWeights::ID8), Score::new(0.0));
        assert!(score(&missing, &FusionWeights::ID8) > score(&known, &FusionWeights::ID8));
    }

    #[test]
    fn synthesized_mass_saturates_and_takes_the_max() {
        let mut at_cap = zeroed();
        at_cap.downloads = Some(POPULARITY_SATURATION);
        let mut over = zeroed();
        over.downloads = Some(POPULARITY_SATURATION.saturating_mul(4));
        let mut from_deps = zeroed();
        // 4_000 dependents * 2_500 = 10_000_000, the saturation constant.
        from_deps.dependents = Some(4_000);
        from_deps.downloads = Some(1);

        let cap = score(&at_cap, &FusionWeights::ID8);
        assert_eq!(cap, score(&over, &FusionWeights::ID8));
        assert_eq!(cap, score(&from_deps, &FusionWeights::ID8));
        assert_eq!(
            cap,
            Score::new(FusionWeights::DEPENDENTS + FusionWeights::DOWNLOADS)
        );
    }

    #[test]
    fn name_flags_do_not_enter_the_sum() {
        let mut plain = zeroed();
        plain.quality_ppm = ppm(500_000);
        plain.bm25 = bm25(3.0);
        plain.popularity_pct = Some(pct(0.2));
        let mut flagged = plain;
        flagged.exact_name = true;
        flagged.contains_name = true;
        flagged.verified_repo = true;
        assert_eq!(
            score(&plain, &FusionWeights::ID8),
            score(&flagged, &FusionWeights::ID8)
        );
    }

    #[test]
    fn gates_stack_and_stay_finite() {
        let mut clean = zeroed();
        clean.quality_ppm = ppm(1_000_000);
        clean.popularity_pct = Some(pct(1.0));
        let base = score(&clean, &FusionWeights::ID8);
        assert_in_id8_bound(base);

        let mut flagged = clean;
        flagged.malware = true;
        flagged.squat_suspect = true;
        flagged.withdrawn = true;
        let got = score(&flagged, &FusionWeights::ID8);
        let expect = Score::new(base.get() * Gate::MALWARE * Gate::SQUAT * Gate::WITHDRAWN);
        assert_eq!(got, expect);
        assert!(got.get().is_finite());
        assert!(got < base);
        assert!(got.get() > 0.0);
    }

    #[test]
    fn non_finite_weights_cannot_produce_nan() {
        let mut weights = FusionWeights::ID8;
        weights.text = f64::NAN;
        weights.quality = f64::INFINITY;
        weights.dependents = f64::NEG_INFINITY;
        let mut factors = zeroed();
        factors.bm25 = bm25(5.0);
        factors.quality_ppm = ppm(1_000_000);
        factors.downloads = None;
        factors.popularity_pct = Some(pct(0.5));
        let got = score(&factors, &weights);
        assert!(got.get().is_finite());
        assert!(!got.get().is_nan());
    }

    #[test]
    fn score_ord_is_total_over_non_finite_inputs() {
        let samples = [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.0,
            0.0,
            1.0,
            -1.0,
            f64::MIN,
            f64::MAX,
        ];
        let mut forward: Vec<Score> = samples.into_iter().map(Score::new).collect();
        let mut backward = forward.clone();
        forward.sort();
        backward.reverse();
        backward.sort();
        assert_eq!(forward, backward);
        for score in &forward {
            assert!(score.get().is_finite());
        }
        assert_eq!(Score::new(f64::NAN), Score::new(0.0));
        assert_eq!(Score::new(-0.0), Score::new(0.0));
    }

    #[test]
    fn constructors_reject_the_outside_of_the_domain() {
        assert_eq!(QualityPpm::new(0).unwrap().get(), 0);
        assert_eq!(
            QualityPpm::new(1_000_000).unwrap().as_unit().to_bits(),
            1.0f64.to_bits()
        );
        assert_eq!(
            QualityPpm::new(500_000).unwrap().as_unit().to_bits(),
            0.5f64.to_bits()
        );
        assert!(QualityPpm::new(1_000_001).is_none());

        assert!(PopularityPct::new(0.0).is_some());
        assert!(PopularityPct::new(1.0).is_some());
        assert!(PopularityPct::new(-0.0).is_some());
        assert_eq!(PopularityPct::new(-0.0).unwrap().get().to_bits(), 0);
        assert!(PopularityPct::new(-0.01).is_none());
        assert!(PopularityPct::new(1.01).is_none());
        assert!(PopularityPct::new(f32::NAN).is_none());
        assert!(PopularityPct::new(f32::INFINITY).is_none());
        assert!(PopularityPct::new(f32::NEG_INFINITY).is_none());

        assert!(Bm25::new(0.0).is_some());
        assert_eq!(Bm25::new(-0.0).unwrap().get().to_bits(), 0);
        assert!(Bm25::new(-1.0).is_none());
        assert!(Bm25::new(f32::NAN).is_none());
        assert!(Bm25::new(f32::INFINITY).is_none());
        assert_eq!(Bm25::new(12.5).unwrap().get().to_bits(), 12.5f32.to_bits());
    }

    #[test]
    fn id8_constants_match_the_plan() {
        assert_eq!(FusionWeights::DEPENDENTS.to_bits(), 0.45f64.to_bits());
        assert_eq!(FusionWeights::DOWNLOADS.to_bits(), 0.25f64.to_bits());
        assert_eq!(FusionWeights::TEXT.to_bits(), 0.20f64.to_bits());
        assert_eq!(FusionWeights::QUALITY.to_bits(), 0.10f64.to_bits());
        assert_eq!(FusionWeights::default(), FusionWeights::ID8);
        let sum = id8_ceiling();
        assert!((sum - 1.0).abs() < 1e-9);
        assert_eq!(Gate::WITHDRAWN.to_bits(), 0.25f64.to_bits());
        assert_eq!(DEPENDENT_DOWNLOAD_EQUIV, 2_500);
        assert_eq!(POPULARITY_SATURATION, 10_000_000);
    }
}

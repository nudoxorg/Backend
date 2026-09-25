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


fn canonical_nonnegative_f32(value: f32) -> f32 {
    if value.to_bits() == (-0.0f32).to_bits() {
        0.0
    } else {
        value
    }
}

mod formula;
pub use formula::{fused_before_gates, quality_times_popularity, rank_key, score};

#[cfg(test)]
mod tests;

//! Tuning knobs for the cascade. Defaults match the historic Navigate path.

use super::super::gates::GateConfig;

/// Tuning knobs.  All constants match the lib.rs defaults.
#[derive(Debug, Clone)]
pub struct RankingConfig {
    /// Quality threshold for the "+1 kink" in score fusion.
    /// Items above this threshold get `quality + 1.0` applied so textual
    /// relevance dominates; items below get raw `quality` so quality dominates.
    /// Lib.rs constant: `> 0.4`.
    pub quality_kink_threshold: f32,

    /// Flat bonus added to the fused score when `name == query` (exact match).
    /// Set high enough to overcome BM25 noise from keyword-spam crates.
    pub exact_name_bonus: f32,

    /// Upper cap on the contains-name boost expressed as a multiple of the
    /// maximum BM25 seen in the top-4 results.  Mirrors the lib.rs cap of
    /// `(top_4 * 3 + boosted) / 4`.
    pub contains_cap_fraction: f32,

    /// Maximum number of candidates eligible for the contains-name bonus.
    /// Lib.rs stops boosting after `boosted_matches >= 5`.
    pub contains_max_boosted: usize,

    /// Minimum population a keyword must appear in to qualify as a dividing
    /// keyword (`> 2` in lib.rs).
    pub dividing_min_pop: u32,

    /// Population fraction above which a keyword is considered "too common" to
    /// divide.  Lib.rs uses `5/8 * N`.
    pub dividing_too_common_fraction: f32,

    /// Population fraction below which a keyword gets the ×2 weight boost.
    /// Lib.rs uses `N / 3`.
    pub dividing_good_pop_fraction: f32,

    /// Absolute population floor for the ×2 weight boost (`>= 10` in lib.rs).
    pub dividing_good_pop_min: u32,

    /// Minimum result-set size for the diversity pass to activate.
    /// Lib.rs checks `keyword_sets.len() < 25` and returns early.
    pub dividing_min_set_size: usize,

    /// Quality threshold for the representative pull-up pass.
    /// Lib.rs: `max(max_quality * 0.97, 0.55)`.
    pub representative_quality_fraction: f32,

    /// Absolute quality floor for the representative pull-up pass.
    pub representative_quality_floor: f32,

    /// Download threshold fraction for the representative pull-up pass.
    /// Lib.rs: `max(max_downloads * 9/10, 100_000)`.
    pub representative_downloads_fraction: f32,

    /// Absolute downloads floor for the representative pull-up pass.
    pub representative_downloads_floor: u64,

    /// Lower bound on downloads for the bubble-sort swap to fire.
    /// Lib.rs: `b.monthly_downloads > 200`.
    pub bubble_downloads_min: u64,

    /// Upper bound on downloads for the bubble-sort swap to fire.
    /// Lib.rs: `b.monthly_downloads < 1_000_000`.
    pub bubble_downloads_max: u64,

    /// Ratio threshold for the bubble-sort swap.
    /// Lib.rs: `a.downloads * 3 < b.downloads`.
    pub bubble_ratio: u64,

    /// Contains-name specificity factor when the query is marked specific
    /// (separators / long). Default `0.9` (Navigate / historic lib.rs).
    /// [`super::policy::RankingPolicy::config_for_intent`] lowers this for
    /// Explore.
    pub contains_specificity_when_specific: f32,

    /// Contains-name specificity factor when the query is generic.
    /// Default `0.15` (Navigate / historic lib.rs).
    pub contains_specificity_when_generic: f32,

    /// Additive quality × log-popularity path scale in score fusion.
    /// Default `0.0` (Navigate / historic behaviour). Explore sets this `> 0`
    /// so high quality + downloads can outrank keyword-spam name contains.
    ///
    /// Formula applied in [`fuse_scores`]:
    /// `score += scale * quality * log2(popularity_weight(1) + 1) / 20`
    pub quality_popularity_path_scale: f32,

    /// Hard spam / squat / malware gate knobs (exact-bonus floor +
    /// multipliers).
    pub gate: GateConfig,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            quality_kink_threshold: 0.4,
            exact_name_bonus: 10.0,
            contains_cap_fraction: 0.75, // (top4*3 + boosted)/4 ≈ top4*0.75 + boosted*0.25
            contains_max_boosted: 5,
            dividing_min_pop: 2,
            dividing_too_common_fraction: 5.0 / 8.0,
            dividing_good_pop_fraction: 1.0 / 3.0,
            dividing_good_pop_min: 10,
            dividing_min_set_size: 25,
            representative_quality_fraction: 0.97,
            representative_quality_floor: 0.55,
            representative_downloads_fraction: 0.9,
            representative_downloads_floor: 100_000,
            bubble_downloads_min: 200,
            bubble_downloads_max: 1_000_000,
            bubble_ratio: 3,
            // Intent-sensitive knobs — defaults match historic Navigate behaviour
            // so existing `rank` / `rank_full` callers stay bit-compatible.
            contains_specificity_when_specific: 0.9,
            contains_specificity_when_generic: 0.15,
            quality_popularity_path_scale: 0.0,
            gate: GateConfig::default(),
        }
    }
}

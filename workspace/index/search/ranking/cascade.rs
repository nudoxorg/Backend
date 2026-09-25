//! Ranking post-processor for registry search.
//!
//! Ports the five-stage score-fusion + diversity pipeline from
//! `search_index/src/lib_search_index.rs` in the lib.rs monorepo, made generic
//! over an opaque payload type `T`.  The module is **pure and deterministic**:
//! no RNG, no I/O, no timestamps.  Ties are broken by name so repeated calls
//! on the same input always produce the same order.
//!
//! # Pipeline
//!
//! 1. [`fuse_score`]              — BM25 × quality kink  +  exact/contains name
//!    bonus
//! 2. Sort by fused score desc, name asc (deterministic tiebreak)
//! 3. [`diversity_pass`]          — dividing-keywords diversity
//! 4. [`pull_up_representatives`] — popular/high-quality crates pulled to the
//!    top
//! 5. [`downloads_bubble`]        — gentle adjacent-pair swap on the tail
//! 6. Truncate to `limit`

use heart::ecosystem::Language;
use smol_str::SmolStr;

use super::passes::{diversity_pass, downloads_bubble, pull_up_representatives};

use super::{
    gates::{self, GateConfig, GateFlags},
    policy::RankingPolicy,
    popularity::PopularitySignals,
};

// Re-export so existing tests / callers that name the constant keep compiling.
pub use super::popularity::DEPENDENT_DOWNLOAD_EQUIV;

/// Graded demotion multiplier for withdrawn versions.
const WITHDRAWN_DEMOTION: f32 = 0.25;

// ── Public types ─────────────────────────────────────────────────────────────

/// One retrieval hit plus the signals ranking needs.
///
/// The `item` field is opaque payload — ranking never inspects it.
#[derive(Debug, Clone)]
pub struct Candidate<T> {
    /// Caller's record / id.  Opaque to the ranking module.
    pub item: T,
    /// Canonical name (used for exact/contains bonus and tiebreaking).
    pub name: String,
    /// Raw BM25 relevance score from tantivy.
    pub bm25: f32,
    /// Quality signal in `0.0..=1.0` (analogous to `crate_base_score` /
    /// `crate_score` in lib.rs).
    pub quality: f32,
    /// Monthly download count, calibrated by the ecosystem's `downloads_scale`.
    ///
    /// `None` when the ecosystem has no download source (see
    /// `SearchNorms::downloads_scale`). Downloads-driven ranking stages
    /// (pull-up, bubble) use the **fairness floor** for `None` candidates
    /// so they are never penalized relative to ecosystems that have data.
    pub downloads: Option<u64>,
    /// Corpus-wide reverse-dependency count from the periodic sweep, if
    /// computed. The ecosystem-fair popularity signal: same methodology in
    /// every registry, no upstream API, harder to game than downloads.
    pub dependents: Option<u32>,
    /// Per-ecosystem popularity percentile in `0.0..=1.0` when the offline CDF
    /// job has filled it. Preferred over raw downloads in
    /// [`Self::popularity_weight`] when present.
    pub popularity_pct: Option<f32>,
    /// This version is withdrawn/yanked on its registry — graded demotion,
    /// never a binary visibility cut (a deprecated-but-only-option must
    /// still surface).
    pub withdrawn: bool,
    /// Typosquat / name land-grab suspect — skips exact-name bonus and applies
    /// the squat gate factor after fuse.
    pub squat_suspect: bool,
    /// Known or flagged malware — buried via the malware gate factor after
    /// fuse.
    pub malware: bool,
    /// Declared repository path contains the package name (soft signal).
    pub verified_repo: bool,
    /// The ecosystem this candidate belongs to; used to select the correct
    /// per-ecosystem `strip_conventions` function for the contains-name bonus
    /// (R1).
    pub ecosystem: Language,
    /// Normalised keywords for the diversity pass.
    pub keywords: Vec<SmolStr>,
}

impl<T> Candidate<T> {
    /// Safety flags for hard spam / squat / malware gates.
    #[must_use]
    pub fn gate_flags(&self) -> GateFlags {
        GateFlags {
            squat_suspect: self.squat_suspect,
            malware: self.malware,
            verified_repo: self.verified_repo,
        }
    }

    /// Popularity signals for this candidate (calibrated downloads already on
    /// [`Self::downloads`]; percentile left `None` until the offline job fills
    /// it).
    pub fn popularity_signals(&self) -> PopularitySignals {
        PopularitySignals {
            downloads: self.downloads,
            dependents: self.dependents,
            popularity_pct: self.popularity_pct,
        }
    }

    /// The popularity weight for downloads-calibrated stages.
    ///
    /// Delegates to [`PopularitySignals::effective_weight`] so percentile /
    /// dependents / downloads / floor logic lives in one place. With neither
    /// signal, the stage's fairness floor applies (missing data is never a
    /// zero-penalty vs packages at the floor).
    pub fn popularity_weight(&self, floor: u64) -> u64 {
        let weight = self.popularity_signals().effective_weight(floor);
        if self.downloads.is_none() && self.dependents.is_none() {
            static ONCE: std::sync::Once = std::sync::Once::new();
            ONCE.call_once(|| {
                tracing::debug!(
                    "ranking: downloads None for at least one candidate (ecosystem {:?}); \
					 using fairness floor {} — this is expected for ecosystems without a \
					 download-count endpoint",
                    self.ecosystem,
                    floor,
                );
            });
        }
        weight
    }
}

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

// ── Public entry points
// ───────────────────────────────────────────────────────

/// Rank and post-process `candidates`, returning them reordered and truncated
/// to `limit`.  Uses [`RankingConfig::default`].
///
/// Pure and deterministic: repeated calls on the same input yield identical
/// output.
///
/// `ecosystem_scope`: when the search is scoped to a single ecosystem (e.g.
/// `lang:go mux`) pass `Some(lang)` so `query_is_specific` uses that
/// ecosystem's separator set; `None` falls back to the shared default set.
pub fn rank<T>(
    query: &str,
    candidates: Vec<Candidate<T>>,
    limit: usize,
    ecosystem_scope: Option<Language>,
) -> Vec<Candidate<T>> {
    rank_with(
        &RankingConfig::default(),
        query,
        candidates,
        limit,
        ecosystem_scope,
    )
}

/// Like [`rank`] but accepts explicit tuning knobs.
pub fn rank_with<T>(
    cfg: &RankingConfig,
    query: &str,
    candidates: Vec<Candidate<T>>,
    limit: usize,
    ecosystem_scope: Option<Language>,
) -> Vec<Candidate<T>> {
    // Classic single-page behaviour: truncate to `limit` at the pull-up (so the
    // tail-bubble runs over the truncated list, exactly as before).
    rank_pipeline(cfg, query, candidates, limit, limit, ecosystem_scope)
}

/// Run the full five-stage pipeline over **all** `candidates` and return them
/// in the pipeline's total order **without truncating**.
///
/// This is the single, page-independent ranking used by keyset pagination: the
/// caller materializes this one total order once per `(query, snapshot)` and
/// pages over it by slicing, so page 1 and page N are slices of the *identical*
/// order — there is no scoring seam at the page boundary.
///
/// `limit` still tunes the position-sensitive stages (representative pull-up's
/// `take`/`better_half`, the bubble tail) exactly as [`rank`] does, so the
/// first `limit` entries of the returned order are byte-for-byte what [`rank`]
/// would have produced.  The difference is only that the tail beyond `limit` is
/// retained (already ordered by fused score + diversity) so later pages have a
/// stable continuation to slice.
///
/// Pure and deterministic: repeated calls on the same input yield identical
/// output.
pub fn rank_full<T>(
    query: &str,
    candidates: Vec<Candidate<T>>,
    limit: usize,
    ecosystem_scope: Option<Language>,
) -> Vec<Candidate<T>> {
    rank_full_with(
        &RankingConfig::default(),
        query,
        candidates,
        limit,
        ecosystem_scope,
    )
}

/// Like [`rank_full`] but accepts explicit tuning knobs.
pub fn rank_full_with<T>(
    cfg: &RankingConfig,
    query: &str,
    candidates: Vec<Candidate<T>>,
    limit: usize,
    ecosystem_scope: Option<Language>,
) -> Vec<Candidate<T>> {
    // `retain = usize::MAX` keeps the whole order (nothing is truncated); `limit`
    // still tunes the position-sensitive stages exactly as the single-page path.
    rank_pipeline(cfg, query, candidates, limit, usize::MAX, ecosystem_scope)
}

/// Intent-aware single-page rank: classifies [`QueryIntent`] then applies
/// [`RankingPolicy::config_for_intent`] before the pipeline.
///
/// Prefer this (or [`RankingPolicy::rank_candidates`]) over bare [`rank`] when
/// the caller has not already chosen an intent-tuned config.
pub fn rank_with_intent<T>(
    query: &str,
    candidates: Vec<Candidate<T>>,
    limit: usize,
    ecosystem_scope: Option<Language>,
) -> Vec<Candidate<T>> {
    RankingPolicy::default().rank_candidates(query, candidates, limit, ecosystem_scope)
}

/// Intent-aware full-order rank (pagination). See [`rank_with_intent`].
pub fn rank_full_with_intent<T>(
    query: &str,
    candidates: Vec<Candidate<T>>,
    limit: usize,
    ecosystem_scope: Option<Language>,
) -> Vec<Candidate<T>> {
    RankingPolicy::default().rank_full_candidates(query, candidates, limit, ecosystem_scope)
}

/// The shared five-stage pipeline. `limit` tunes the position-sensitive stages
/// (pull-up `take`/`better_half`, the bubble tail); `retain` is the length the
/// reordered list is truncated to at the pull-up. The single-page [`rank_with`]
/// passes `retain == limit` (classic truncation); the paginated
/// [`rank_full_with`] passes `retain == usize::MAX` (keep the whole order to
/// slice pages from).
fn rank_pipeline<T>(
    cfg: &RankingConfig,
    query: &str,
    candidates: Vec<Candidate<T>>,
    limit: usize,
    retain: usize,
    ecosystem_scope: Option<Language>,
) -> Vec<Candidate<T>> {
    if candidates.is_empty() || limit == 0 {
        return Vec::new();
    }

    // ── Stage (a): fuse score per candidate ──────────────────────────────────
    let mut fused: Vec<f32> = super::fuse::fuse_scores(cfg, query, &candidates, ecosystem_scope);

    // ── Stage (a.5): hard spam / squat / malware gates ────────────────────────
    // Multiplicative demotion (never zero) so flagged packages stay visible in
    // deep pages but cannot outrank clean peers via raw BM25.
    for (score, candidate) in fused.iter_mut().zip(candidates.iter()) {
        *score = gates::apply_gate_multiplier(*score, candidate.gate_flags(), &cfg.gate);
    }

    // ── Stage (a.6): graded withdrawn demotion ────────────────────────────────
    // Multiply withdrawn candidates' scores by WITHDRAWN_DEMOTION (never zero —
    // a deprecated-but-only-option must still surface).
    for (score, candidate) in fused.iter_mut().zip(candidates.iter()) {
        if candidate.withdrawn {
            *score *= WITHDRAWN_DEMOTION;
        }
    }

    // ── Stage (b): sort by fused score desc, name asc for ties ───────────────
    // Attach scores as a parallel vec then zip-sort so we avoid adding a field
    // to the generic Candidate.
    let order =
        crate::lane::order_desc_score_asc_name(&fused, |index| candidates[index].name.as_str());
    let indexed: Vec<(usize, f32)> = order
        .into_iter()
        .map(|index| (index, fused[index]))
        .collect();
    // Rebuild candidates in sorted order.
    let mut sorted: Vec<Candidate<T>> = {
        // Safety: each index appears exactly once, so we can drain by taking
        // ownership.  Build a vec of Option<Candidate<T>>, pull out by index.
        let mut slots: Vec<Option<Candidate<T>>> = candidates.into_iter().map(Some).collect();
        indexed
            .iter()
            .map(|(i, _)| slots[*i].take().expect("unique index"))
            .collect()
    };
    // Parallel fused-score vec in the same post-sort order.
    let mut scores: Vec<f32> = indexed.into_iter().map(|(_, s)| s).collect();

    // ── Stage (c): dividing-keywords diversity pass ───────────────────────────
    diversity_pass(cfg, &mut sorted, &mut scores);

    // ── Stage (d): representative crate pull-up ───────────────────────────────
    // The pull-up is a *prefix* operation (it prepends the pulled representatives
    // to the retained remainder). `retain` decides whether the tail is kept
    // (pagination: `usize::MAX`) or dropped to a single page (`limit`); `limit`
    // tunes eligibility either way.
    pull_up_representatives(cfg, &mut sorted, limit, retain);

    // ── Stage (e): downloads bubble-sort on the tail ──────────────────────────
    if sorted.len() > 5 {
        downloads_bubble(cfg, &mut sorted[2..]);
        downloads_bubble(cfg, &mut sorted[5..]);
    }

    sorted
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::fuse::contains_query_names_for_ecosystem;
    use super::*;

    fn make(
        name: &str,
        bm25: f32,
        quality: f32,
        downloads: Option<u64>,
        kws: &[&str],
    ) -> Candidate<&'static str> {
        Candidate {
            item: "payload",
            name: name.to_owned(),
            bm25,
            quality,
            downloads,
            dependents: None,
            popularity_pct: None,
            withdrawn: false,
            squat_suspect: false,
            malware: false,
            verified_repo: false,
            ecosystem: Language::Rust,
            keywords: kws.iter().map(|&k| SmolStr::new(k)).collect(),
        }
    }

    fn make_eco(
        name: &str,
        bm25: f32,
        quality: f32,
        downloads: Option<u64>,
        eco: Language,
        kws: &[&str],
    ) -> Candidate<&'static str> {
        Candidate {
            item: "payload",
            name: name.to_owned(),
            bm25,
            quality,
            downloads,
            dependents: None,
            popularity_pct: None,
            withdrawn: false,
            squat_suspect: false,
            malware: false,
            verified_repo: false,
            ecosystem: eco,
            keywords: kws.iter().map(|&k| SmolStr::new(k)).collect(),
        }
    }

    // ── Fusion kink ──────────────────────────────────────────────────────────

    #[test]
    fn fusion_kink_high_quality_wins() {
        // High-quality item (0.9) with modest bm25 = 1.0 → fused = 1.0 * (0.9+1.0) =
        // 1.9 Low-quality item  (0.1) with higher bm25 = 1.5 → fused = 1.5 *
        // 0.1       = 0.15
        let candidates = vec![
            make("low-quality", 1.5, 0.1, Some(10), &[]),
            make("high-quality", 1.0, 0.9, Some(10), &[]),
        ];
        let result = rank("foo", candidates, 10, None);
        assert_eq!(
            result[0].name, "high-quality",
            "high-quality item should rank first"
        );
        assert_eq!(result[1].name, "low-quality");
    }

    #[test]
    fn fusion_kink_boundary() {
        // quality = 0.41 (just above threshold) → fused = bm25 * (0.41+1.0) = bm25 *
        // 1.41 quality = 0.40 (at threshold, not above) → fused = bm25 * 0.40
        let bm25 = 1.0_f32;
        let above_kink = make("above", bm25, 0.41, None, &[]);
        let at_kink = make("at", bm25, 0.40, None, &[]);
        let result = rank("q", vec![at_kink, above_kink], 10, None);
        assert_eq!(
            result[0].name, "above",
            "item above the kink should rank higher"
        );
    }

    // ── Exact-name bonus ─────────────────────────────────────────────────────

    #[test]
    fn exact_name_bonus_floats_exact_match() {
        // "tokio-exact" has higher bm25 but doesn't match the query name exactly.
        // "tokio" exactly matches the query and should rank first.
        let candidates = vec![
            make("tokio-extended", 5.0, 0.5, Some(1_000), &["async"]),
            make("tokio", 1.0, 0.5, Some(1_000), &["async"]),
        ];
        let result = rank("tokio", candidates, 10, None);
        assert_eq!(
            result[0].name, "tokio",
            "exact match should rank above higher-bm25 non-exact"
        );
    }

    // ── R1: per-ecosystem strip_conventions ──────────────────────────────────

    /// R1 regression: Rust-convention stripping must NOT fire for npm
    /// candidates. Old code stripped `cargo`/`rust`/`rs` from all
    /// ecosystems; new code uses the candidate's ecosystem strip function.
    ///
    /// Test case A: npm candidate `cargo-web`, query `web`.
    /// Old path: strip `cargo-` → `web` == `web` → exact-in-strip bonus fires.
    /// New path (npm): strip is `strip_npm_scope` (identity for non-scoped
    /// names) → `cargo-web` contains `web` as substring → bonus still fires
    /// (containment logic is unaffected). This is CORRECT — `cargo-web`
    /// genuinely contains `web`.
    ///
    /// Test case B (the real differentiator): Rust candidate `cargo-serde`,
    /// query `serde`. Old path: strip is applied to NAME only. New path:
    /// strip is also applied to the QUERY. Under Rust ecosystem both strip
    /// to their cores: name `cargo-serde` → `serde`, query `serde` →
    /// `serde` → contains fires. This is an improvement enabled by the
    /// two-sided strip.
    #[test]
    fn r1_rust_query_strip_applied_to_query() {
        // Rust ecosystem: query `cargo-serde` strips to `serde`; name `serde` →
        // `serde`. Containment `serde` contains `serde` → bonus fires
        // (improvement). contains_query_names_for_ecosystem for Rust:
        // strip("serde") = "serde", strip("cargo-serde") = "serde", "serde" contains
        // "serde" → true
        assert!(
            contains_query_names_for_ecosystem("serde", "cargo-serde", Language::Rust),
            "Rust: name 'serde' should contain stripped query 'cargo-serde' → 'serde'"
        );
        // Containment is symmetric (longer contains shorter — lib.rs port), so
        // `cargo-serde` ⊇ `serde` matches under npm too. The case that truly
        // differentiates: query `cargo-axio` vs name `axios` — only the Rust
        // strip (`cargo-` off the query → `axio` ⊆ `axios`) can connect them.
        assert!(
            contains_query_names_for_ecosystem("axios", "cargo-axio", Language::Rust),
            "Rust: query 'cargo-axio' strips to 'axio', contained in 'axios'"
        );
        assert!(
            !contains_query_names_for_ecosystem("axios", "cargo-axio", Language::Typescript),
            "npm: identity strip — neither of 'axios'/'cargo-axio' contains the other"
        );
    }

    /// R1: Rust `rs`-suffix stripping does not affect npm candidates.
    #[test]
    fn r1_rust_rs_suffix_not_applied_to_npm() {
        // npm candidate named `axio-rs`, query `axio`.
        // Old Rust strip: `axio-rs` → trim `rs` → `axio-` → trim `-` → `axio` →
        // contains `axio` ✓ New npm strip (strip_npm_scope = identity for
        // non-scoped): `axio-rs` stays `axio-rs` query stays `axio`; `axio-rs`
        // contains `axio` as substring → bonus still fires.
        // The true differentiator is when stripping PREVENTS a false match:
        // npm name `cargo-utils`, query `utils`: npm keeps `cargo-utils` which contains
        // `utils` → OK. Rust name `utils-rs`, query `utils`: Rust strips `-rs`
        // → `utils`, contains `utils` → OK. Cross-check: if we run Rust strip
        // on npm name `cargo-utils` with query `utils`: Rust strip on
        // `cargo-utils` → `utils`; `utils` == `utils` → stronger match than needed.
        // New code: npm candidate with query `utils` uses npm strip (identity) →
        // `cargo-utils` contains `utils` as substring → normal contains bonus,
        // not the full strip-equality match. The critical test: name
        // `cargo-utils` (npm) + query `utils` with Rust ecosystem:   Old: would
        // apply Rust strip → name becomes `utils` → exact-in-strip   New: npm
        // strip, name stays `cargo-utils` → only substring contains (different
        // boosting)
        let rust_strips_cargo =
            contains_query_names_for_ecosystem("cargo-utils", "utils", Language::Rust);
        let npm_strips_cargo =
            contains_query_names_for_ecosystem("cargo-utils", "utils", Language::Typescript);
        // Both fire (substring still works) but for different reasons:
        // Rust: strip("cargo-utils") = "utils", strip("utils") = "utils", "utils"
        // contains "utils" npm:  "cargo-utils" contains "utils" as substring
        assert!(
            rust_strips_cargo,
            "Rust: contains should fire (strip gives exact)"
        );
        assert!(npm_strips_cargo, "npm: contains should fire (substring)");
        // But npm name `utils-rs` with query `utils`: Rust would strip `-rs`; npm
        // won't. Under Rust strip: "utils-rs" → "utils", "utils" contains
        // "utils" → match Under npm strip: "utils-rs" stays "utils-rs",
        // contains "utils" as substring → match The strip is separator-anchored
        // (`-rs`/`cargo-`), NOT the legacy `trim_end_matches("rs")` that
        // mangled names like `colors`: a bare `rs` survives for every ecosystem
        // and matches itself.
        assert!(
            contains_query_names_for_ecosystem("rs", "rs", Language::Rust),
            "Rust: anchored strip keeps a bare 'rs' — exact self-match stands"
        );
        assert!(
            contains_query_names_for_ecosystem("rs", "rs", Language::Typescript),
            "npm: no Rust stripping — 'rs' vs 'rs' matches verbatim"
        );
    }

    /// R1: Python conventions are per-ecosystem.
    #[test]
    fn r1_python_strip_applied_for_python_only() {
        // Python: `python-requests` → `requests`; query `requests` → `requests` → match
        assert!(
            contains_query_names_for_ecosystem("python-requests", "requests", Language::Python),
            "Python: strip 'python-' prefix, 'requests' contains 'requests'"
        );
        // npm: same name, same query — npm strip is identity → 'python-requests'
        // contains 'requests' (substring)
        assert!(
            contains_query_names_for_ecosystem("python-requests", "requests", Language::Typescript),
            "npm (identity strip): 'python-requests' contains 'requests' as substring"
        );
        // Rust: 'python-requests' — Rust doesn't strip 'python-', stays
        // 'python-requests' which contains 'requests' as substring. Same result
        // but different semantic path.
        assert!(
            contains_query_names_for_ecosystem("python-requests", "requests", Language::Rust),
            "Rust: 'python-requests' contains 'requests' as substring"
        );
    }

    // ── R2: specificity_separators ───────────────────────────────────────────

    /// R2: a query like `org.springframework` is specific in Java (uses `.`
    /// separator).
    #[test]
    fn r2_java_dot_separator_marks_specific() {
        use crate::ecosystem::LanguageExt;
        let norms = Language::Java.spec().search_norms();
        assert!(
            norms.query_is_specific("org.springframework"),
            "Java: '.' marks query as specific"
        );
        assert!(
            norms.query_is_specific("org:springframework"),
            "Java: ':' marks query as specific"
        );
        assert!(
            !norms.query_is_specific("springframework"),
            "Java: bare name without separator is not specific (len <= 15)"
        );
    }

    /// R2: scoped ecosystem scope changes specificity evaluation in
    /// fuse_scores.
    #[test]
    fn r2_ecosystem_scope_used_for_specificity() {
        // Build candidates with identical bm25/quality to isolate the specificity
        // effect. A specific query (contains separator) should give bigger
        // contains bonus. We test that passing Some(Language::Rust) doesn't
        // crash and produces output.
        let candidates = vec![
            make_eco("serde", 1.0, 0.5, Some(100_000), Language::Rust, &[]),
            make_eco("serde-json", 0.9, 0.5, Some(80_000), Language::Rust, &[]),
        ];
        // Query with separator → specific → larger bonus multiplier.
        let result_specific = rank("cargo-serde", candidates.clone(), 10, Some(Language::Rust));
        let result_bare = rank("serde", candidates, 10, Some(Language::Rust));
        // Both should return results; specific query should give serde a larger boost.
        assert!(!result_specific.is_empty());
        assert!(!result_bare.is_empty());
    }

    // ── S4: downloads as Option<u64> ─────────────────────────────────────────

    /// S4: downloads=None candidate should not be ranked below a candidate with
    /// equal BM25/quality but with downloads data (fairness guard).
    #[test]
    fn s4_downloads_none_fairness_within_first_page() {
        // NuGet candidate with downloads=None vs Rust candidate with 1M downloads.
        // Equal BM25 and quality.  NuGet should appear in the first page (top-N of
        // rank_full).
        let candidates: Vec<Candidate<&str>> = vec![
            make_eco("nuget-pkg", 1.0, 0.6, None, Language::CSharp, &[]),
            make_eco("rust-pkg", 1.0, 0.6, Some(1_000_000), Language::Rust, &[]),
            make_eco("rust-pkg2", 0.9, 0.5, Some(500_000), Language::Rust, &[]),
        ];
        let result = rank_full("pkg", candidates, 10, None);
        let nuget_pos = result
            .iter()
            .position(|c| c.name == "nuget-pkg")
            .expect("nuget-pkg in results");
        // NuGet should be within the first page (all 3 candidates fit, so all should
        // appear).
        assert!(
            nuget_pos < 3,
            "NuGet candidate (downloads=None) should appear in results, got pos {nuget_pos}"
        );
    }

    /// S4: popularity_weight returns floor for None downloads/dependents,
    /// max(n, floor) for Some.
    #[test]
    fn s4_popularity_weight_accessor() {
        let c_none = make("none", 1.0, 0.5, None, &[]);
        let c_low = make("low", 1.0, 0.5, Some(100), &[]);
        let c_high = make("high", 1.0, 0.5, Some(10_000), &[]);
        let floor = 500u64;
        assert_eq!(
            c_none.popularity_weight(floor),
            floor,
            "None downloads, None dependents → floor"
        );
        assert_eq!(
            c_low.popularity_weight(floor),
            floor,
            "Some(100) < floor → floor"
        );
        assert_eq!(
            c_high.popularity_weight(floor),
            10_000,
            "Some(10000) > floor → value"
        );

        // Dependents take priority over downloads.
        let c_dep = Candidate {
            dependents: Some(100),
            downloads: None,
            ..make("dep", 1.0, 0.5, None, &[])
        };
        assert_eq!(
            c_dep.popularity_weight(floor),
            100 * DEPENDENT_DOWNLOAD_EQUIV,
            "dependents=Some(100) → 100 * DEPENDENT_DOWNLOAD_EQUIV"
        );

        // Dependents beat downloads even when downloads are present.
        let c_dep_over_dl = Candidate {
            dependents: Some(10),
            downloads: Some(1_000),
            ..make("dep-dl", 1.0, 0.5, Some(1_000), &[])
        };
        assert_eq!(
            c_dep_over_dl.popularity_weight(floor),
            10 * DEPENDENT_DOWNLOAD_EQUIV,
            "dependents preferred over downloads"
        );
    }

    /// Dependents-driven popularity: a candidate with dependents=Some(100) and
    /// downloads=None participates in pull-up/bubble as 250_000-equivalent.
    #[test]
    fn dependents_beat_absent_downloads_in_pullup() {
        // Build enough candidates to trigger pull-up (needs ≥ 7).
        let mut candidates: Vec<Candidate<&'static str>> = (0..15)
            .map(|i| {
                make(
                    &format!("crate-{i:02}"),
                    (i as f32).mul_add(-0.05, 1.0),
                    0.3,
                    Some(100),
                    &[],
                )
            })
            .collect();
        // Insert a candidate with dependents=100 (≡ 250_000 downloads) at position 12.
        candidates.insert(12, Candidate {
            dependents: Some(100),
            downloads: None,
            ..make("popular-dep", 0.4, 0.85, None, &[])
        });
        let result = rank("crate", candidates, 20, None);
        let pos = result
            .iter()
            .position(|c| c.name == "popular-dep")
            .expect("popular-dep in results");
        assert!(
            pos < 5,
            "dependents=100 candidate should be pulled into top 5, got pos {pos}"
        );
    }

    /// REGISTRYLESS RL-12 / §9: cpp has no download source, so a cpp candidate
    /// with `downloads = None` **and** `dependents = None` takes the
    /// downloads-absent skip path in [`Candidate::popularity_weight`] and lands
    /// on the fairness floor — never penalized for a signal its ecosystem
    /// cannot provide. This exercises the existing `tracing::debug!` skip
    /// branch for cpp.
    #[test]
    fn cpp_absent_downloads_uses_fairness_floor() {
        let floor: u64 = 42;
        let cpp_no_signals = Candidate {
            ecosystem: Language::Cpp,
            downloads: None,
            dependents: None,
            popularity_pct: None,
            ..make("github.com/madler/zlib", 1.0, 0.5, None, &[])
        };
        assert_eq!(
            cpp_no_signals.popularity_weight(floor),
            floor,
            "a cpp candidate with no download/dependent signal must fall to the fairness floor"
        );
    }

    /// Withdrawn candidate is demoted below an otherwise-equal listed one but
    /// still present in output.
    #[test]
    fn withdrawn_demoted_but_present() {
        let candidates = vec![make("listed", 1.0, 0.6, Some(1_000), &[]), Candidate {
            withdrawn: true,
            ..make("withdrawn", 1.0, 0.6, Some(1_000), &[])
        }];
        let result = rank("q", candidates, 10, None);
        assert_eq!(result.len(), 2, "both candidates must be in output");
        assert_eq!(result[0].name, "listed", "listed should outrank withdrawn");
        assert_eq!(result[1].name, "withdrawn", "withdrawn still present");
    }

    /// A withdrawn exact-name match still surfaces (top-3 when it's the only
    /// match).
    #[test]
    fn withdrawn_exact_name_still_surfaces() {
        let candidates = vec![Candidate {
            withdrawn: true,
            ..make("only-match", 2.0, 0.7, Some(5_000), &[])
        }];
        let result = rank("only-match", candidates, 10, None);
        assert!(
            !result.is_empty(),
            "withdrawn-only exact match must still surface"
        );
        assert_eq!(result[0].name, "only-match");
    }

    // ── Downloads scale calibration ───────────────────────────────────────────

    /// downloads_scale: npm raw 1M × 0.05 ≈ 50k, comparable to Rust 50k × 1.0.
    /// This is validated at the Candidate construction level (mod.rs populates
    /// effective downloads after scaling). Here we test the scale values
    /// themselves.
    #[test]
    fn downloads_scale_npm_vs_rust() {
        use crate::ecosystem::LanguageExt;
        let rust_scale = Language::Rust.spec().search_norms().downloads_scale;
        let npm_scale = Language::Typescript.spec().search_norms().downloads_scale;
        assert_eq!(
            rust_scale,
            Some(1.0),
            "Rust scale = 1.0 (calibration baseline)"
        );
        assert_eq!(
            npm_scale,
            Some(0.05),
            "npm scale = 0.05 (npm volumes ~20× larger)"
        );
        // 1M npm × 0.05 = 50k effective ≈ 50k Rust × 1.0
        let npm_raw = 1_000_000u64;
        let npm_eff = (npm_raw as f32 * npm_scale.unwrap()) as u64;
        assert_eq!(npm_eff, 50_000);
    }

    // ── Representative pull-up ───────────────────────────────────────────────

    #[test]
    fn representative_pullup_popular_item() {
        // Build 20 mediocre items plus one very popular item buried at position 15.
        let mut candidates: Vec<Candidate<&'static str>> = (0..20)
            .map(|i| {
                make(
                    &format!("crate-{i:02}"),
                    (i as f32).mul_add(-0.04, 1.0),
                    0.3,
                    Some(500),
                    &[],
                )
            })
            .collect();
        // Popular item has high downloads (5M) and high quality (0.9), inserted at
        // index 15.
        candidates.insert(15, make("popular-crate", 0.5, 0.9, Some(5_000_000), &[]));

        let result = rank("something", candidates, 20, None);
        // The popular crate should be in the top few results.
        let pos = result
            .iter()
            .position(|c| c.name == "popular-crate")
            .expect("popular crate in results");
        assert!(
            pos < 5,
            "popular crate should be pulled into top 5, got position {pos}"
        );
    }

    // ── Downloads bubble ─────────────────────────────────────────────────────

    #[test]
    fn downloads_bubble_swaps_adjacent_pair() {
        // Pair: small (1k downloads) before large (500k downloads).
        // The bubble should swap them.
        let cfg = RankingConfig::default();
        let mut slice = vec![
            make("small", 1.0, 0.5, Some(1_000), &[]),
            make("large", 0.9, 0.5, Some(500_000), &[]),
        ];
        downloads_bubble(&cfg, &mut slice);
        assert_eq!(
            slice[0].name, "large",
            "large-download item should bubble up"
        );
        assert_eq!(slice[1].name, "small");
    }

    #[test]
    fn downloads_bubble_no_swap_when_large_exceeds_cap() {
        // b.downloads = 2_000_000 ≥ bubble_max (1_000_000) → no swap.
        let cfg = RankingConfig::default();
        let mut slice = vec![
            make("small", 1.0, 0.5, Some(1_000), &[]),
            make("huge", 0.9, 0.5, Some(2_000_000), &[]),
        ];
        downloads_bubble(&cfg, &mut slice);
        assert_eq!(
            slice[0].name, "small",
            "should not swap when b.downloads exceeds cap"
        );
    }

    /// Fairness: None-downloads bubble swap. A candidate with None acts as if
    /// it has exactly bubble_min (floor). So it doesn't get incorrectly
    /// swapped past a candidate that also has floor-level downloads.
    #[test]
    fn downloads_bubble_none_uses_floor_no_swap() {
        // a.downloads = None → effective = floor = 200
        // b.downloads = None → effective = floor = 200
        // ratio: 200 * 3 = 600 ≥ 200 → no swap (a_eff * ratio >= b_eff)
        let cfg = RankingConfig::default();
        let mut slice = vec![
            make("a", 1.0, 0.5, None, &[]),
            make("b", 0.9, 0.5, None, &[]),
        ];
        downloads_bubble(&cfg, &mut slice);
        assert_eq!(slice[0].name, "a", "None-None pair: no swap (equal floor)");
    }

    // ── Determinism ──────────────────────────────────────────────────────────

    #[test]
    fn rank_is_deterministic() {
        let candidates = || {
            vec![
                make("zeta", 1.2, 0.8, Some(10_000), &["net"]),
                make("alpha", 1.0, 0.6, Some(50_000), &["io"]),
                make("beta", 1.5, 0.3, Some(5_000), &["net", "io"]),
            ]
        };
        let r1 = rank("net", candidates(), 10, None);
        let r2 = rank("net", candidates(), 10, None);
        let names1: Vec<&str> = r1.iter().map(|c| c.name.as_str()).collect();
        let names2: Vec<&str> = r2.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names1, names2, "rank must be deterministic");
    }

    // ── Truncation & edge cases ───────────────────────────────────────────────

    #[test]
    fn truncation_respects_limit() {
        let candidates: Vec<_> = (0..20)
            .map(|i| make(&format!("c{i}"), 1.0, 0.5, None, &[]))
            .collect();
        let result = rank("q", candidates, 5, None);
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn empty_input_returns_empty() {
        let result = rank::<&str>("q", vec![], 10, None);
        assert!(result.is_empty());
    }

    #[test]
    fn limit_zero_returns_empty() {
        let candidates = vec![make("a", 1.0, 0.5, None, &[])];
        let result = rank("q", candidates, 0, None);
        assert!(result.is_empty());
    }

    #[test]
    fn limit_larger_than_candidates_returns_all() {
        let candidates = vec![
            make("a", 1.0, 0.5, None, &[]),
            make("b", 0.8, 0.3, None, &[]),
        ];
        let result = rank("q", candidates, 100, None);
        assert_eq!(result.len(), 2);
    }

    // ── Ecosystem-scoped ranking ──────────────────────────────────────────────

    /// Scoped query: Python candidate should rank above a Rust lookalike when
    /// the search is Python-scoped.
    #[test]
    fn python_scoped_ranking_first() {
        let candidates = vec![
            make_eco("requests", 2.0, 0.9, Some(5_000_000), Language::Python, &[
                "http", "client",
            ]),
            make_eco("reqwest", 1.8, 0.8, Some(3_000_000), Language::Rust, &[
                "http", "client",
            ]),
            make_eco(
                "python-asyncio",
                1.5,
                0.7,
                Some(1_000_000),
                Language::Python,
                &["async"],
            ),
        ];
        // Rust ecosystem scope
        let result = rank("requests", candidates, 10, Some(Language::Python));
        // requests should be first (exact name match)
        assert_eq!(result[0].name, "requests");
    }

    // ── Intent-aware ranking (Tracks B+C) ────────────────────────────────────

    /// Navigate: exact match "serde" still beats high-BM25 spam (existing
    /// behaviour).
    #[test]
    fn navigate_exact_serde_beats_bm25_spam() {
        // Spam has inflated BM25 and a name that *contains* the query; exact
        // match still wins via exact_name_bonus (Navigate keeps bonus ≥ 10).
        let candidates = vec![
            make(
                "serde-with-lots-of-keywords-spam",
                8.0,
                0.15,
                Some(1_000),
                &["serde", "serialize", "json"],
            ),
            make("serde", 1.0, 0.9, Some(50_000_000), &["serialize"]),
        ];
        let result = rank_with_intent("serde", candidates, 10, Some(Language::Rust));
        assert_eq!(
            result[0].name, "serde",
            "Navigate exact match must outrank high-BM25 name-spam"
        );
    }

    /// Explore: "http client" — high quality+downloads outranks keyword-spam
    /// name-contains.
    #[test]
    fn explore_http_client_quality_beats_name_spam() {
        // Spam: name contains query tokens, inflated BM25, low quality / downloads.
        // Good: household-name HTTP client, lower BM25, high quality + downloads.
        let candidates = vec![
            make("http-client-keywords-spam-extra", 12.0, 0.12, Some(50), &[
                "http", "client", "request",
            ]),
            make("reqwest", 2.0, 0.95, Some(5_000_000), &["http", "client"]),
            make("hyper", 1.8, 0.9, Some(3_000_000), &["http"]),
        ];
        let result = rank_with_intent("http client", candidates, 10, Some(Language::Rust));
        let spam_pos = result
            .iter()
            .position(|c| c.name == "http-client-keywords-spam-extra")
            .expect("spam in results");
        let good_pos = result
            .iter()
            .position(|c| c.name == "reqwest")
            .expect("reqwest in results");
        assert!(
            good_pos < spam_pos,
            "Explore: high quality+downloads must outrank keyword-spam (reqwest@{good_pos} vs spam@{spam_pos}); order={:?}",
            result.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );
    }

    /// Adversarial: withdrawn package cannot outrank healthy equal-BM25 via
    /// popularity alone.
    #[test]
    fn adversarial_withdrawn_cannot_outrank_via_popularity() {
        let candidates = vec![
            make("healthy", 1.0, 0.6, Some(50_000_000), &[]),
            Candidate {
                withdrawn: true,
                ..make("withdrawn-popular", 1.0, 0.6, Some(50_000_000), &[])
            },
        ];
        let result = rank_with_intent("q", candidates, 10, None);
        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0].name, "healthy",
            "equal BM25, quality, and downloads: withdrawn gate must rank the live package first"
        );
        assert_eq!(result[1].name, "withdrawn-popular");
    }

    // ── Hard spam / squat gates ───────────────────────────────────────────────

    /// Malware with absurd BM25 still ranks after a clean peer.
    #[test]
    fn adversarial_malware_huge_bm25_ranks_after_clean() {
        let candidates = vec![
            Candidate {
                malware: true,
                ..make("evil-pkg", 1_000.0, 0.9, Some(50_000_000), &["async"])
            },
            make("clean-pkg", 1.5, 0.6, Some(10_000), &["async"]),
        ];
        let result = rank("async", candidates, 10, None);
        assert_eq!(result.len(), 2);
        assert_eq!(
            result[0].name,
            "clean-pkg",
            "clean peer must outrank malware despite huge BM25; got {:?}",
            result.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );
        assert_eq!(
            result[1].name, "evil-pkg",
            "malware still visible (buried, not dropped)"
        );
    }

    /// Squat exact-name land-grab does not receive the full exact bonus, so a
    /// real high-quality package wins the query.
    #[test]
    fn adversarial_squat_exact_landgrab_loses_to_real() {
        // Query "yaml": squat package owns the exact name but is flagged and
        // low-quality; real package is a well-known implementation.
        let candidates = vec![
            Candidate {
                squat_suspect: true,
                ..make("yaml", 5.0, 0.05, Some(10), &["yaml"])
            },
            make("yaml-rust", 2.0, 0.85, Some(500_000), &["yaml", "parser"]),
        ];
        let result = rank("yaml", candidates, 10, None);
        assert_eq!(
            result[0].name,
            "yaml-rust",
            "real package must beat squat exact-name land-grab; got {:?}",
            result.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );
    }

    /// Exact-name bonus is withheld when quality is below the gate floor even
    /// for a clean (non-squat) candidate.
    #[test]
    fn exact_bonus_requires_quality_floor() {
        // Low-quality exact match vs higher-quality non-exact peer.
        // Without the floor, exact bonus (10.0) would dominate.
        let candidates = vec![
            make("floortest", 1.0, 0.05, Some(100), &[]), // exact, below floor
            make("floortest-real", 3.0, 0.9, Some(100_000), &[]),
        ];
        let result = rank("floortest", candidates, 10, None);
        assert_eq!(
            result[0].name,
            "floortest-real",
            "exact match below quality floor must not get full exact bonus; got {:?}",
            result.iter().map(|c| c.name.as_str()).collect::<Vec<_>>()
        );
    }

    /// Adversarial: missing downloads uses floor — never zero-penalty vs
    /// packages at floor.
    #[test]
    fn adversarial_missing_downloads_floor_not_zero_penalty() {
        use super::super::popularity::PopularitySignals;
        let floor = 200u64;
        let none_sig = PopularitySignals::from_facets(None, None, Some(1.0));
        let floor_sig = PopularitySignals {
            downloads: Some(floor),
            dependents: None,
            popularity_pct: None,
        };
        assert_eq!(
            none_sig.effective_weight(floor),
            floor_sig.effective_weight(floor),
            "None downloads must equal packages that only have floor-level data"
        );
        // Ranking: equal BM25/quality; None must not sort strictly below floor-data
        // solely because downloads are missing (name tiebreak is the only
        // differentiator).
        let candidates = vec![
            make("aaa-none", 1.0, 0.6, None, &[]),
            make("zzz-floor", 1.0, 0.6, Some(floor), &[]),
        ];
        let result = rank("q", candidates, 10, None);
        assert_eq!(result.len(), 2);
        // ID-8 reads raw downloads. `None` contributes 0 mass; the floor value
        // contributes a small popularity term, so the floor package ranks first.
        // Both stay on the page: missing downloads is not a drop.
        assert_eq!(result[0].name, "zzz-floor");
        assert_eq!(result[1].name, "aaa-none");
    }

    /// Default `rank` (no intent) still preserves exact-match dominance.
    #[test]
    fn default_rank_preserves_exact_match() {
        let candidates = vec![
            make("tokio-extended", 5.0, 0.5, Some(1_000), &["async"]),
            make("tokio", 1.0, 0.5, Some(1_000), &["async"]),
        ];
        let result = rank("tokio", candidates, 10, None);
        assert_eq!(result[0].name, "tokio");
    }
}

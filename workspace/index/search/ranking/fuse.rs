//! Query-relative score fusion for one page of candidates.
//!
//! The base is the ID-8 factor sum. Exact and contains bonuses are applied
//! here because they depend on the query, not on catalog signals.
//!
//! [`fuse_scores`] reuses one lowercase buffer and keeps a four-slot top of
//! BM25. [`fuse_scores_allocating`] lowercases every name and sorts the whole
//! BM25 vector. It exists so a differential test can fail a rewrite that
//! drifts, and so the speed gate fails if the hot path calls it.

use heart::ecosystem::Language;

use super::{
    cascade::{Candidate, RankingConfig},
    gates,
};

/// Fused score for every candidate, in input order.
pub(super) fn fuse_scores<T>(
    cfg: &RankingConfig,
    query: &str,
    candidates: &[Candidate<T>],
    ecosystem_scope: Option<Language>,
) -> Vec<f32> {
    fuse(cfg, query, candidates, ecosystem_scope, false)
}

/// Allocating twin of [`fuse_scores`]. Not the function the pipeline calls.
pub(super) fn fuse_scores_allocating<T>(
    cfg: &RankingConfig,
    query: &str,
    candidates: &[Candidate<T>],
    ecosystem_scope: Option<Language>,
) -> Vec<f32> {
    fuse(cfg, query, candidates, ecosystem_scope, true)
}

fn fuse<T>(
    cfg: &RankingConfig,
    query: &str,
    candidates: &[Candidate<T>],
    ecosystem_scope: Option<Language>,
    allocate: bool,
) -> Vec<f32> {
    use crate::ecosystem::{LanguageExt, search::DEFAULT_SPECIFICITY_SEPARATORS};

    let top4_score = if allocate {
        fourth_by_sort(candidates)
    } else {
        fourth_by_slots(candidates)
    };

    let query_is_specific = ecosystem_scope.map_or_else(
        || query.contains(DEFAULT_SPECIFICITY_SEPARATORS) || query.len() > 15,
        |lang| lang.spec().search_norms().query_is_specific(query),
    );

    let query_lower = query.to_ascii_lowercase();
    let mut name_lower = String::new();
    let mut boosted = 0usize;
    let mut fused = Vec::with_capacity(candidates.len());

    for candidate in candidates {
        let factors = crate::search::id8::ranking_factors(candidate);
        let mut score = crate::search::factors::fused_before_gates(
            &factors,
            &crate::search::factors::FusionWeights::ID8,
        ) as f32;
        let scale = cfg.quality_popularity_path_scale;
        if scale > 0.0 {
            score += scale * crate::search::factors::quality_times_popularity(&factors) as f32;
        }

        if allocate {
            name_lower = candidate.name.to_ascii_lowercase();
        } else {
            fold_ascii_lowercase(&mut name_lower, &candidate.name);
        }

        if name_lower == query_lower {
            if gates::exact_bonus_eligible(candidate.quality, candidate.squat_suspect, &cfg.gate) {
                score += cfg.exact_name_bonus;
            }
        } else if boosted < cfg.contains_max_boosted
            && contains_query_names_for_ecosystem(&name_lower, &query_lower, candidate.ecosystem)
        {
            let quality_bonus =
                candidate.quality.mul_add(candidate.quality, 0.25) * 2.0_f32.min(1.1);
            let specificity = if query_is_specific {
                cfg.contains_specificity_when_specific
            } else {
                cfg.contains_specificity_when_generic
            };
            let bonus_factor = 1.0 + quality_bonus * specificity * 2.0 / (2.0 + boosted as f32);
            let boosted_score = score * bonus_factor;
            let cap = top4_score * 3.0 + boosted_score;
            let capped = (cap / 4.0).max(score * (bonus_factor - 1.0).mul_add(0.1, 1.0));
            score = if boosted_score > top4_score {
                capped
            } else {
                boosted_score
            };
            boosted += 1;
        }

        fused.push(score);
    }

    fused
}

/// Lowercase `name` into `buffer`, keeping the buffer's allocation.
pub(super) fn fold_ascii_lowercase(buffer: &mut String, name: &str) {
    buffer.clear();
    buffer.push_str(name);
    buffer.make_ascii_lowercase();
}

fn fourth_by_sort<T>(candidates: &[Candidate<T>]) -> f32 {
    let mut top4_bm25: Vec<f32> = candidates.iter().map(|candidate| candidate.bm25).collect();
    top4_bm25.sort_unstable_by(|left, right| {
        right.partial_cmp(left).unwrap_or(std::cmp::Ordering::Equal)
    });
    top4_bm25
        .get(3)
        .copied()
        .unwrap_or_else(|| top4_bm25.first().copied().unwrap_or(0.0))
}

/// The value a descending sort would leave at index 3, or the maximum when
/// fewer than four scores exist.
fn fourth_by_slots<T>(candidates: &[Candidate<T>]) -> f32 {
    let mut top = [0.0_f32; 4];
    let mut filled = 0usize;
    for candidate in candidates {
        insert_desc(&mut top, &mut filled, candidate.bm25);
    }
    if filled == 0 {
        0.0
    } else if filled < 4 {
        top[0]
    } else {
        top[3]
    }
}

fn insert_desc(top: &mut [f32; 4], filled: &mut usize, value: f32) {
    if *filled < 4 {
        let mut index = *filled;
        while index > 0 && comes_first(value, top[index - 1]) {
            top[index] = top[index - 1];
            index -= 1;
        }
        top[index] = value;
        *filled += 1;
    } else if comes_first(value, top[3]) {
        let mut index = 3;
        while index > 0 && comes_first(value, top[index - 1]) {
            top[index] = top[index - 1];
            index -= 1;
        }
        top[index] = value;
    }
}

/// `value` belongs ahead of `existing` in the same order as
/// `sort_unstable_by(|a, b| b.partial_cmp(a).unwrap_or(Equal))`.
fn comes_first(value: f32, existing: f32) -> bool {
    matches!(existing.partial_cmp(&value), Some(std::cmp::Ordering::Less))
}

pub(super) fn contains_query_names_for_ecosystem(
    name: &str,
    query: &str,
    ecosystem: Language,
) -> bool {
    use crate::ecosystem::LanguageExt;
    let strip = ecosystem.spec().search_norms().strip_conventions;
    let stripped_name = strip(name).trim_matches(|ch| ch == '-' || ch == '_');
    let stripped_query = strip(query).trim_matches(|ch| ch == '-' || ch == '_');
    if stripped_name.is_empty() || stripped_query.is_empty() {
        return false;
    }
    let (long, short) = if stripped_name.len() >= stripped_query.len() {
        (stripped_name, stripped_query)
    } else {
        (stripped_query, stripped_name)
    };
    long.contains(short)
}

#[cfg(test)]
mod tests {
    use super::*;
    use heart::ecosystem::Language;

    fn candidate(name: &str, bm25: f32) -> Candidate<()> {
        Candidate {
            item: (),
            name: name.to_owned(),
            bm25,
            quality: 0.8,
            downloads: Some(1_000),
            dependents: Some(10),
            popularity_pct: None,
            withdrawn: false,
            squat_suspect: false,
            malware: false,
            verified_repo: false,
            ecosystem: Language::Rust,
            keywords: Vec::new(),
        }
    }

    #[test]
    fn explore_scale_lifts_quality_times_popularity_and_zero_scale_does_not() {
        let mut quiet = candidate("quiet", 1.0);
        quiet.quality = 0.1;
        quiet.downloads = Some(0);
        quiet.dependents = Some(0);
        let mut known = candidate("known", 1.0);
        known.quality = 1.0;
        known.popularity_pct = Some(1.0);
        known.downloads = None;
        known.dependents = None;

        let mut off = RankingConfig::default();
        off.quality_popularity_path_scale = 0.0;
        off.exact_name_bonus = 0.0;
        let mut on = off.clone();
        on.quality_popularity_path_scale = 2.5;

        let base = fuse_scores(&off, "unrelated", &[quiet.clone(), known.clone()], None);
        let lifted = fuse_scores(&on, "unrelated", &[quiet.clone(), known.clone()], None);
        assert!((lifted[0] - base[0]).abs() < 1e-5, "zero popularity adds nothing");
        let gain = lifted[1] - base[1];
        assert!(gain > 2.0 && gain < 3.0, "scale 2.5 times quality 1 times popularity 1");
    }

    #[test]
    fn reused_buffer_matches_the_allocating_fuse() {
        let cfg = RankingConfig::default();
        let candidates = vec![
            candidate("Serde", 3.0),
            candidate("serde_json", 2.5),
            candidate("tokio", 9.0),
            candidate("cargo-serde", 1.0),
            candidate("tiny", 0.2),
            candidate("serde", 4.0),
        ];
        let fast = fuse_scores(&cfg, "serde", &candidates, Some(Language::Rust));
        let slow = fuse_scores_allocating(&cfg, "serde", &candidates, Some(Language::Rust));
        assert_eq!(fast.len(), slow.len());
        for (left, right) in fast.iter().zip(&slow) {
            assert_eq!(left.to_bits(), right.to_bits());
        }
    }

    #[test]
    fn fourth_slot_matches_a_full_sort() {
        let samples = [
            vec![1.0, 9.0, 3.0, 8.0, 2.0, 7.0],
            vec![4.0],
            vec![1.0, 1.0, 1.0, 1.0, 1.0],
            vec![0.5, 0.4, 0.3],
        ];
        for sample in samples {
            let candidates: Vec<_> = sample
                .iter()
                .enumerate()
                .map(|(index, score)| candidate(&format!("n{index}"), *score))
                .collect();
            assert_eq!(
                fourth_by_slots(&candidates).to_bits(),
                fourth_by_sort(&candidates).to_bits()
            );
        }
    }
}

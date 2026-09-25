//! ID-8 ordering of retrieval candidates.
//!
//! Each [`crate::search::ranking::Candidate`] is scored as
//! [`super::factors::RankingFactors`] under
//! [`super::factors::FusionWeights::ID8`]. [`super::factors::rank_key`] then
//! orders score descending, name ascending.
//!
//! `exact_name` and `contains_name` stay false: [`Candidate`] does not carry
//! query-relative name matches. `verified_repo` is copied off the candidate.

#![warn(missing_docs)]

use crate::search::ranking::Candidate;

use super::factors::{self, Bm25, PopularityPct, QualityPpm, RankingFactors};

/// Order `candidates` by the ID-8 fused score, highest first.
///
/// Equal scores break by name ascending (`str`'s bytewise order) through
/// [`super::factors::rank_key`]. The payload `T` is moved and never read.
#[must_use]
pub fn rank<T>(mut candidates: Vec<Candidate<T>>) -> Vec<Candidate<T>> {
    candidates.sort_by(|left, right| {
        factors::rank_key(id8_score(left), &left.name)
            .cmp(&factors::rank_key(id8_score(right), &right.name))
    });
    candidates
}

fn id8_score<T>(candidate: &Candidate<T>) -> factors::Score {
    factors::score(&ranking_factors(candidate), &factors::FusionWeights::ID8)
}

pub(crate) fn ranking_factors<T>(candidate: &Candidate<T>) -> RankingFactors {
    RankingFactors {
        bm25: bm25_or_zero(candidate.bm25),
        quality_ppm: quality_ppm(candidate.quality),
        dependents: candidate.dependents,
        downloads: candidate.downloads,
        popularity_pct: candidate.popularity_pct.and_then(PopularityPct::new),
        withdrawn: candidate.withdrawn,
        squat_suspect: candidate.squat_suspect,
        malware: candidate.malware,
        verified_repo: candidate.verified_repo,
        // No query is in scope, and `Candidate` has no exact/contains fields.
        exact_name: false,
        contains_name: false,
    }
}

/// Non-finite and negative BM25 are not relevance. They become zero.
fn bm25_or_zero(raw: f32) -> Bm25 {
    Bm25::new(raw).unwrap_or_else(|| Bm25::new(0.0).expect("0.0 is a valid Bm25"))
}

/// Map quality from `0..=1` onto [`QualityPpm`], clamping out-of-range values.
///
/// Non-finite input has no position on that interval, so it becomes `0`.
fn quality_ppm(quality: f32) -> QualityPpm {
    let unit = if quality.is_finite() {
        quality.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let scaled = (f64::from(unit) * f64::from(QualityPpm::MAX)).round();
    let ppm = if scaled.is_finite() && scaled >= 0.0 {
        (scaled as u32).min(QualityPpm::MAX)
    } else {
        0
    };
    QualityPpm::new(ppm).expect("ppm was clamped into QualityPpm::MAX")
}

#[cfg(test)]
mod tests {
    use heart::ecosystem::Language;

    use super::*;

    fn candidate(name: &str, withdrawn: bool) -> Candidate<&'static str> {
        Candidate {
            item: "payload",
            name: name.to_owned(),
            bm25: 1.5,
            quality: 0.5,
            downloads: Some(1_000),
            dependents: Some(4),
            popularity_pct: Some(0.4),
            withdrawn,
            squat_suspect: false,
            malware: false,
            verified_repo: false,
            ecosystem: Language::Rust,
            keywords: Vec::new(),
        }
    }

    #[test]
    fn tied_scores_sort_by_name_ascending() {
        let zeta = candidate("zeta", false);
        let alpha = candidate("alpha", false);
        assert_eq!(id8_score(&zeta), id8_score(&alpha));

        let ranked = rank(vec![zeta, alpha]);
        assert_eq!(
            ranked
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "zeta"]
        );
        assert!(ranked.iter().all(|candidate| candidate.item == "payload"));
    }

    #[test]
    fn withdrawn_sorts_below_an_equal_peer() {
        let yanked = candidate("aaa", true);
        let live = candidate("zzz", false);
        assert!(id8_score(&yanked) < id8_score(&live));

        // "aaa" would win a pure name tie. Withdrawal has to bury it anyway.
        let ranked = rank(vec![yanked, live]);
        assert_eq!(
            ranked
                .iter()
                .map(|candidate| candidate.name.as_str())
                .collect::<Vec<_>>(),
            ["zzz", "aaa"]
        );
    }
}

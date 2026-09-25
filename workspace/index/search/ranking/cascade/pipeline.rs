//! Five-stage rank: ID-8 fuse, gates, withdrawn demotion, diversity, pull-up,
//! bubble.
//!
//! `limit` tunes the position-sensitive stages. `retain` is how much of the
//! reordered list those stages keep. Single-page rank passes `retain == limit`.
//! Full-order rank passes `retain == usize::MAX` so later pages slice the same
//! order.

use heart::ecosystem::Language;

use super::{
    super::{
        gates,
        passes::{diversity_pass, downloads_bubble, pull_up_representatives},
        policy::RankingPolicy,
    },
    candidate::Candidate,
    config::RankingConfig,
};

/// Graded demotion multiplier for withdrawn versions.
const WITHDRAWN_DEMOTION: f32 = 0.25;

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
    let mut fused: Vec<f32> =
        super::super::fuse::fuse_scores(cfg, query, &candidates, ecosystem_scope);

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

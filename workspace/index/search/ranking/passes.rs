//! Post-score passes the cascade runs after the ID-8 fuse.
//!
//! Diversity demotes an over-represented keyword. Pull-up lifts a few
//! high-quality or heavily downloaded rows. The downloads bubble swaps an
//! adjacent pair when the lower row is much more popular. Each pass keeps
//! the name tiebreak.

use super::cascade::{Candidate, RankingConfig};

// ── Stage (c): diversity pass
// ─────────────────────────────────────────────────

/// Diversity pass: find the keyword that best splits the result set into two
/// meaning-groups and demote items over-represented in one group.
///
/// Ports `dividing_keywords_inner` and `popular_dividing_keyword` from lib.rs.
///
/// The algorithm:
/// 1. Deduplicate identical keyword-sets (spammy boilerplate families).
/// 2. Find the keyword with population in `(min_pop, too_common_pop]` that
///    maximises a weighted count (items in the top half count double).
/// 3. Items in the `[good_pop_min, N/3]` band get weight ×2.
/// 4. `query-` prefix and `-query` suffix keywords count double because they
///    signal a meaningful sub-domain rather than a synonym.
/// 5. Demote (halve the fused score) for candidates that carry the chosen
///    keyword, beyond the first few.
pub(super) fn diversity_pass<T>(
    cfg: &RankingConfig,
    candidates: &mut [Candidate<T>],
    scores: &mut [f32],
) {
    let n = candidates.len();
    if n < cfg.dividing_min_set_size {
        return;
    }

    // Deduplicate identical keyword-sets (mirrors lib.rs `dupes` HashSet on
    // `keywords_s`).
    // Build a key per candidate (sorted, joined) then track which keys repeat.
    let kw_keys: Vec<String> = candidates
        .iter()
        .map(|c| {
            let mut kws: Vec<&str> = c.keywords.iter().map(smol_str::SmolStr::as_str).collect();
            kws.sort_unstable();
            kws.join("\x00")
        })
        .collect();

    // Count keyword populations (only one entry per unique keyword-set).
    // weight: items in the top half (index < n/2) contribute 2, others 1 —
    // analogous to lib.rs `2048` vs `1024` tiers (ratio is 2:1).
    use std::collections::HashMap;
    let mut keyword_pop: HashMap<&str, u32> = HashMap::new();
    let mut seen_sets: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let half = n / 2;
    for (i, c) in candidates.iter().enumerate() {
        let key = kw_keys[i].as_str();
        if !seen_sets.insert(key) {
            continue; // duplicate keyword-set — skip
        }
        let weight: u32 = if i < half { 2 } else { 1 };
        for kw in &c.keywords {
            *keyword_pop.entry(kw.as_str()).or_default() += weight;
        }
    }

    let too_common_pop = ((n as f32 * cfg.dividing_too_common_fraction) as u32).max(1);
    let good_pop = ((n as f32 * cfg.dividing_good_pop_fraction) as u32).max(1);

    // Select the best dividing keyword.
    let dividing_kw: Option<&str> = keyword_pop
        .iter()
        .filter(|&(_, &pop)| pop > cfg.dividing_min_pop && pop <= too_common_pop)
        .map(|(&kw, &pop)| {
            // ×2 weight bonus for the well-populated band.
            let effective = if pop >= cfg.dividing_good_pop_min && pop <= good_pop {
                pop * 2
            } else {
                pop
            };
            (kw, effective)
        })
        .max_by_key(|(_, eff)| *eff)
        .map(|(kw, _)| kw);

    let Some(dk) = dividing_kw else { return };

    // Demote candidates that carry `dk`, sparing the first few.
    // Mirrors lib.rs: factor = (0.9 - i/10).max(0.33) applied starting at
    // position 4 for the "too common" pass.  Here we do a simpler demote:
    // halve the score for every carrier beyond position 3.
    let mut carrier_count = 0usize;
    for (i, (c, score)) in candidates.iter().zip(scores.iter_mut()).enumerate() {
        if i < 3 {
            continue; // top 3 are always kept unchanged
        }
        if c.keywords.iter().any(|k| k.as_str() == dk) {
            let factor = (0.9 - carrier_count as f32 / 10.0).max(0.33);
            *score *= factor;
            carrier_count += 1;
        }
    }

    // Same total order as the lane radix: score descending, then name, then
    // input index. A non-finite score takes its IEEE place instead of tying.
    let order =
        crate::lane::order_desc_score_asc_name(scores, |index| candidates[index].name.as_str());
    apply_permutation(candidates, scores, order);
}

// ── Stage (d): representative pull-up ────────────────────────────────────────

/// Pull up well-known items from the better half of the result set.
///
/// Ports `move_representative_crates_to_top` from lib.rs.
///
/// Rules:
/// - Always keep the top-3 as-is (they are "already there").
/// - From `docs[3..better_half]` (better_half ≈ min(N/3, 50)):
///   - Pull up ≤3 items whose `quality >= max(max_quality*0.97, 0.55)`.
///   - Pull up ≤3 (minus already pulled) items whose `downloads >=
///     max(max_downloads*0.9, 100_000)` and `quality >= 0.55`.
/// - Re-sort the pulled set by a mixed quality×downloads score.
/// - Prepend to the remaining list, truncated to `retain`.
///
/// `limit` tunes the eligibility (`take`, `better_half`) exactly as lib.rs
/// does; `retain` is the length the reordered list is truncated to.  Passing a
/// `retain` at least `candidates.len()` (e.g. `usize::MAX`) keeps the whole
/// order (used by keyset pagination, which pages over the full order); passing
/// `retain == limit` reproduces the classic single-page truncation.
///
/// Fairness guard: `popularity_weight(floor)` is used everywhere instead of
/// raw `downloads` so `None`-downloads candidates participate at the floor and
/// are never sorted below candidates that merely have data.
pub(super) fn pull_up_representatives<T>(
    cfg: &RankingConfig,
    candidates: &mut Vec<Candidate<T>>,
    limit: usize,
    retain: usize,
) {
    let n = candidates.len();
    if n < 7 {
        candidates.truncate(retain);
        return;
    }

    let better_half = (n / 3).min(50);
    if better_half <= 5 {
        candidates.truncate(retain);
        return;
    }

    let take = (limit / 70).clamp(1, 3);
    let top = &candidates[..better_half];
    // Use popularity_weight at the representative floor for eligibility.
    let dl_floor = cfg.representative_downloads_floor;
    let max_downloads = top
        .iter()
        .map(|c| c.popularity_weight(dl_floor))
        .max()
        .unwrap_or(0);
    let max_quality = top.iter().map(|c| c.quality).fold(0.0_f32, f32::max);

    let high_rank =
        (max_quality * cfg.representative_quality_fraction).max(cfg.representative_quality_floor);
    let high_dl = ((max_downloads as f64 * f64::from(cfg.representative_downloads_fraction))
        as u64)
        .max(cfg.representative_downloads_floor);

    // Collect indices (within `candidates`) to pull up, skipping top-3.
    let mut pull_indices: Vec<usize> = Vec::new();

    // Quality-based pull-up.
    for (i, c) in candidates[..better_half].iter().enumerate().skip(3) {
        if pull_indices.len() >= take {
            break;
        }
        if c.quality >= high_rank {
            pull_indices.push(i);
        }
    }

    // Downloads-based pull-up.
    let got = pull_indices.len();
    let dl_take = take.saturating_sub(got).max(1);
    let mut dl_pulled = 0;
    for (i, c) in candidates[..better_half].iter().enumerate().skip(3) {
        if dl_pulled >= dl_take {
            break;
        }
        if pull_indices.contains(&i) {
            continue;
        }
        let eff = c.popularity_weight(dl_floor);
        let min_score = if eff >= 1_000_000 { 0.32 } else { 0.55 };
        if c.quality >= min_score && eff >= high_dl {
            pull_indices.push(i);
            dl_pulled += 1;
        }
    }

    // Always keep top-3.
    pull_indices.extend(0..3_usize.min(n));
    pull_indices.sort_unstable();
    pull_indices.dedup();

    // Extract pulled candidates in reverse index order (so drain doesn't shift).
    let mut top_crates: Vec<Candidate<T>> = pull_indices
        .iter()
        .rev()
        .map(|&i| candidates.remove(i))
        .collect();
    top_crates.reverse(); // restore original relative order

    let mixed_scores: Vec<f32> = top_crates
        .iter()
        .map(|candidate| {
            (0.3 + candidate.quality)
                * candidate.quality
                * ((candidate.popularity_weight(dl_floor) + 25_000) as f32).log2()
        })
        .collect();
    let order = crate::lane::order_desc_score_asc_name(&mixed_scores, |index| {
        top_crates[index].name.as_str()
    });
    let mut slots: Vec<Option<Candidate<T>>> = top_crates.drain(..).map(Some).collect();
    for index in order {
        top_crates.push(slots[index].take().expect("each pulled row is taken once"));
    }

    // Truncate remaining, prepend top crates.
    candidates.truncate(retain.saturating_sub(top_crates.len()));
    let tail = std::mem::take(candidates);
    candidates.extend(top_crates);
    candidates.extend(tail);
}

// ── Stage (e): downloads bubble-sort ─────────────────────────────────────────

/// Gently swap adjacent pairs where a much-more-downloaded item sits just
/// below a tiny one.
///
/// Ports `downloads_bubble_sort` from lib.rs.
///
/// Swap condition: `b.downloads > bubble_min && b.downloads < bubble_max &&
/// a.downloads * ratio < b.downloads`.
///
/// Fairness guard: both `a` and `b` use `popularity_weight(floor)` where
/// `floor = bubble_downloads_min` so `None`-downloads candidates are treated
/// as if they have exactly the floor count (they participate neutrally).
pub(super) fn downloads_bubble<T>(cfg: &RankingConfig, slice: &mut [Candidate<T>]) {
    let floor = cfg.bubble_downloads_min;
    for pair in slice.as_chunks_mut::<2>().0 {
        let [a, b] = pair;
        let a_eff = a.popularity_weight(floor);
        let b_eff = b.popularity_weight(floor);
        if b_eff > cfg.bubble_downloads_min
            && b_eff < cfg.bubble_downloads_max
            && a_eff.saturating_mul(cfg.bubble_ratio) < b_eff
        {
            std::mem::swap(a, b);
        }
    }
}

// ── Helpers
// ───────────────────────────────────────────────────────────────────

/// Apply a permutation (given as a vec of destination-to-source indices) to
/// two parallel slices in-place.  The permutation must be a bijection over
/// `0..n`.
fn apply_permutation<T>(items: &mut [Candidate<T>], scores: &mut [f32], mut perm: Vec<usize>) {
    // Cycle-follower in-place permutation: O(n) time, O(n) mark space.
    let n = items.len();
    debug_assert_eq!(n, scores.len());
    debug_assert_eq!(n, perm.len());
    let mut done = vec![false; n];
    for start in 0..n {
        if done[start] || perm[start] == start {
            done[start] = true;
            continue;
        }
        let mut current = start;
        loop {
            let next = perm[current];
            if next == start {
                done[current] = true;
                break;
            }
            items.swap(current, next);
            scores.swap(current, next);
            // After swapping, `next` now holds what was at `current`, which is
            // where `start`'s item should ultimately land — keep following.
            perm[current] = current; // mark as settled
            done[current] = true;
            current = next;
        }
        perm[current] = current;
        done[current] = true;
    }
}

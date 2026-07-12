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
//! 1. [`fuse_score`]              — BM25 × quality kink  +  exact/contains name bonus
//! 2. Sort by fused score desc, name asc (deterministic tiebreak)
//! 3. [`diversity_pass`]          — dividing-keywords diversity
//! 4. [`pull_up_representatives`] — popular/high-quality crates pulled to the top
//! 5. [`downloads_bubble`]        — gentle adjacent-pair swap on the tail
//! 6. Truncate to `limit`

use smol_str::SmolStr;

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
	/// Monthly download count (used by the bubble-sort and representative
	/// pull-up passes).
	pub downloads: u64,
	/// Normalised keywords for the diversity pass.
	pub keywords: Vec<SmolStr>,
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
}

impl Default for RankingConfig {
	fn default() -> Self {
		Self {
			quality_kink_threshold:           0.4,
			exact_name_bonus:                 10.0,
			contains_cap_fraction:            0.75, // (top4*3 + boosted)/4 ≈ top4*0.75 + boosted*0.25
			contains_max_boosted:             5,
			dividing_min_pop:                 2,
			dividing_too_common_fraction:     5.0 / 8.0,
			dividing_good_pop_fraction:       1.0 / 3.0,
			dividing_good_pop_min:            10,
			dividing_min_set_size:            25,
			representative_quality_fraction:  0.97,
			representative_quality_floor:     0.55,
			representative_downloads_fraction: 0.9,
			representative_downloads_floor:   100_000,
			bubble_downloads_min:             200,
			bubble_downloads_max:             1_000_000,
			bubble_ratio:                     3,
		}
	}
}

// ── Public entry points ───────────────────────────────────────────────────────

/// Rank and post-process `candidates`, returning them reordered and truncated
/// to `limit`.  Uses [`RankingConfig::default`].
///
/// Pure and deterministic: repeated calls on the same input yield identical
/// output.
pub fn rank<T>(query: &str, candidates: Vec<Candidate<T>>, limit: usize) -> Vec<Candidate<T>> {
	rank_with(&RankingConfig::default(), query, candidates, limit)
}

/// Like [`rank`] but accepts explicit tuning knobs.
pub fn rank_with<T>(
	cfg: &RankingConfig,
	query: &str,
	candidates: Vec<Candidate<T>>,
	limit: usize,
) -> Vec<Candidate<T>> {
	// Classic single-page behaviour: truncate to `limit` at the pull-up (so the
	// tail-bubble runs over the truncated list, exactly as before).
	rank_pipeline(cfg, query, candidates, limit, limit)
}

/// Run the full five-stage pipeline over **all** `candidates` and return them in
/// the pipeline's total order **without truncating**.
///
/// This is the single, page-independent ranking used by keyset pagination: the
/// caller materializes this one total order once per `(query, snapshot)` and
/// pages over it by slicing, so page 1 and page N are slices of the *identical*
/// order — there is no scoring seam at the page boundary.
///
/// `limit` still tunes the position-sensitive stages (representative pull-up's
/// `take`/`better_half`, the bubble tail) exactly as [`rank`] does, so the first
/// `limit` entries of the returned order are byte-for-byte what [`rank`] would
/// have produced.  The difference is only that the tail beyond `limit` is
/// retained (already ordered by fused score + diversity) so later pages have a
/// stable continuation to slice.
///
/// Pure and deterministic: repeated calls on the same input yield identical
/// output.
pub fn rank_full<T>(query: &str, candidates: Vec<Candidate<T>>, limit: usize) -> Vec<Candidate<T>> {
	rank_full_with(&RankingConfig::default(), query, candidates, limit)
}

/// Like [`rank_full`] but accepts explicit tuning knobs.
pub fn rank_full_with<T>(
	cfg: &RankingConfig,
	query: &str,
	candidates: Vec<Candidate<T>>,
	limit: usize,
) -> Vec<Candidate<T>> {
	// `retain = usize::MAX` keeps the whole order (nothing is truncated); `limit`
	// still tunes the position-sensitive stages exactly as the single-page path.
	rank_pipeline(cfg, query, candidates, limit, usize::MAX)
}

/// The shared five-stage pipeline. `limit` tunes the position-sensitive stages
/// (pull-up `take`/`better_half`, the bubble tail); `retain` is the length the
/// reordered list is truncated to at the pull-up. The single-page [`rank_with`]
/// passes `retain == limit` (classic truncation); the paginated [`rank_full_with`]
/// passes `retain == usize::MAX` (keep the whole order to slice pages from).
fn rank_pipeline<T>(
	cfg: &RankingConfig,
	query: &str,
	candidates: Vec<Candidate<T>>,
	limit: usize,
	retain: usize,
) -> Vec<Candidate<T>> {
	if candidates.is_empty() || limit == 0 {
		return Vec::new();
	}

	// ── Stage (a): fuse score per candidate ──────────────────────────────────
	let fused: Vec<f32> = fuse_scores(cfg, query, &candidates);

	// ── Stage (b): sort by fused score desc, name asc for ties ───────────────
	// Attach scores as a parallel vec then zip-sort so we avoid adding a field
	// to the generic Candidate.
	let mut indexed: Vec<(usize, f32)> = fused.into_iter().enumerate().collect();
	indexed.sort_unstable_by(|(ai, ascore), (bi, bscore)| {
		bscore
			.partial_cmp(ascore)
			.unwrap_or(std::cmp::Ordering::Equal)
			.then_with(|| candidates[*ai].name.cmp(&candidates[*bi].name))
	});
	// Rebuild candidates in sorted order.
	let mut sorted: Vec<Candidate<T>> = {
		// Safety: each index appears exactly once, so we can drain by taking
		// ownership.  Build a vec of Option<Candidate<T>>, pull out by index.
		let mut slots: Vec<Option<Candidate<T>>> =
			candidates.into_iter().map(Some).collect();
		indexed.iter().map(|(i, _)| slots[*i].take().expect("unique index")).collect()
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

// ── Stage (a): score fusion ───────────────────────────────────────────────────

/// Compute a fused score for every candidate.
///
/// Ports `tweak_score` (the tantivy collector hook) and `assign_doc_score`
/// from lib.rs.
///
/// The fusion formula is:
/// ```text
/// q = if quality > threshold { quality + 1.0 } else { quality }
/// fused = bm25 * q
/// ```
/// Then an exact-name bonus or a capped contains-name bonus is added.
fn fuse_scores<T>(cfg: &RankingConfig, query: &str, candidates: &[Candidate<T>]) -> Vec<f32> {
	// Pre-compute the top-4 BM25 (the cap target for the contains bonus).
	let mut top4_bm25: Vec<f32> = candidates.iter().map(|c| c.bm25).collect();
	top4_bm25.sort_unstable_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
	let top4_score = top4_bm25.get(3).copied().unwrap_or(top4_bm25.first().copied().unwrap_or(0.0));

	// Is the query specific (contains separators or is long)?
	// Lib.rs: `query_is_specific = query.contains(['-', '_']) || query.len() > 15`
	let query_is_specific = query.contains(['-', '_']) || query.len() > 15;

	let query_lower = query.to_ascii_lowercase();
	let mut boosted = 0usize;
	let mut fused = Vec::with_capacity(candidates.len());

	for c in candidates {
		// ── BM25 × quality kink (tweak_score / assign_doc_score) ─────────────
		let q = if c.quality > cfg.quality_kink_threshold {
			c.quality + 1.0
		} else {
			c.quality
		};
		let mut score = c.bm25 * q;

		// ── Exact / contains name bonus (assign_doc_score / contains_query) ───
		let name_lower = c.name.to_ascii_lowercase();
		if name_lower == query_lower {
			// Exact match: flat bonus, replaces the fused score if larger.
			// Lib.rs caps at `max_relevance * match_multiplier`; here we model
			// it as a large additive bonus so the order is equivalent.
			score += cfg.exact_name_bonus;
		} else if boosted < cfg.contains_max_boosted
			&& contains_query_names(&name_lower, &query_lower)
		{
			// Contains bonus: capped so keyword-spam can't reach top-3.
			// Lib.rs formula: `capped = ((top4 * 3 + boosted_score) / 4).max(minimal)`
			// with bonus multiplier = 1 + quality_bonus * specificity_factor.
			let quality_bonus =
				(c.quality * c.quality + 0.25) * 2.0_f32.min(1.1);
			let specificity = if query_is_specific { 0.9 } else { 0.15 };
			let bonus_factor =
				1.0 + quality_bonus * specificity * 2.0 / (2.0 + boosted as f32);
			let boosted_score = score * bonus_factor;
			let cap = top4_score * 3.0 + boosted_score;
			let capped = (cap / 4.0).max(score * (1.0 + (bonus_factor - 1.0) * 0.1));
			score = if boosted_score > top4_score { capped } else { boosted_score };
			boosted += 1;
		}

		fused.push(score);
	}

	fused
}

/// Ports `contains_query` from lib.rs: strips `cargo`/`rust` prefix and `rs`
/// suffix then checks substring containment (longer contains shorter).
fn contains_query_names(name: &str, query: &str) -> bool {
	let a = name
		.strip_prefix("cargo")
		.or_else(|| name.strip_prefix("rust"))
		.unwrap_or(name)
		.trim_end_matches("rs")
		.trim_matches(['-', '_']);
	if a.is_empty() {
		return false;
	}
	// longer.contains(shorter)
	let (long, short) = if a.len() >= query.len() { (a, query) } else { (query, a) };
	long.contains(short)
}

// ── Stage (c): diversity pass ─────────────────────────────────────────────────

/// Diversity pass: find the keyword that best splits the result set into two
/// meaning-groups and demote items over-represented in one group.
///
/// Ports `dividing_keywords_inner` and `popular_dividing_keyword` from lib.rs.
///
/// The algorithm:
/// 1. Deduplicate identical keyword-sets (spammy boilerplate families).
/// 2. Find the keyword with population in `(min_pop, too_common_pop]`
///    that maximises a weighted count (items in the top half count double).
/// 3. Items in the `[good_pop_min, N/3]` band get weight ×2.
/// 4. `query-` prefix and `-query` suffix keywords count double because they
///    signal a meaningful sub-domain rather than a synonym.
/// 5. Demote (halve the fused score) for candidates that carry the chosen
///    keyword, beyond the first few.
fn diversity_pass<T>(cfg: &RankingConfig, candidates: &mut [Candidate<T>], scores: &mut [f32]) {
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
			let mut kws: Vec<&str> = c.keywords.iter().map(|s| s.as_str()).collect();
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

	// Re-sort (stable relative to the name tiebreak already applied).
	let n = candidates.len();
	// Build index vec and sort by (score desc, name asc).
	let mut idx: Vec<usize> = (0..n).collect();
	idx.sort_unstable_by(|&a, &b| {
		scores[b]
			.partial_cmp(&scores[a])
			.unwrap_or(std::cmp::Ordering::Equal)
			.then_with(|| candidates[a].name.cmp(&candidates[b].name))
	});
	// Apply permutation in-place.
	apply_permutation(candidates, scores, idx);
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
///   - Pull up ≤3 (minus already pulled) items whose
///     `downloads >= max(max_downloads*0.9, 100_000)` and `quality >= 0.55`.
/// - Re-sort the pulled set by a mixed quality×downloads score.
/// - Prepend to the remaining list, truncated to `retain`.
///
/// `limit` tunes the eligibility (`take`, `better_half`) exactly as lib.rs does;
/// `retain` is the length the reordered list is truncated to.  Passing a `retain`
/// at least `candidates.len()` (e.g. `usize::MAX`) keeps the whole order (used by
/// keyset pagination, which pages over the full order); passing `retain == limit`
/// reproduces the classic single-page truncation.
fn pull_up_representatives<T>(
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
	let max_downloads = top.iter().map(|c| c.downloads).max().unwrap_or(0);
	let max_quality = top
		.iter()
		.map(|c| c.quality)
		.fold(0.0_f32, f32::max);

	let high_rank = (max_quality * cfg.representative_quality_fraction)
		.max(cfg.representative_quality_floor);
	let high_dl = ((max_downloads as f64 * cfg.representative_downloads_fraction as f64) as u64)
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
		let min_score = if c.downloads >= 1_000_000 { 0.32 } else { 0.55 };
		if c.quality >= min_score && c.downloads >= high_dl {
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

	// Sort the pulled crates by mixed score: (0.3 + score_proxy) * quality * log2(downloads + 25000)
	// We don't carry the fused score here, so proxy with quality as the relevance signal.
	top_crates.sort_unstable_by(|a, b| {
		let mixed = |c: &Candidate<T>| {
			(0.3 + c.quality) * c.quality * ((c.downloads + 25_000) as f32).log2()
		};
		mixed(b)
			.partial_cmp(&mixed(a))
			.unwrap_or(std::cmp::Ordering::Equal)
			.then_with(|| a.name.cmp(&b.name))
	});

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
fn downloads_bubble<T>(cfg: &RankingConfig, slice: &mut [Candidate<T>]) {
	for pair in slice.chunks_exact_mut(2) {
		let [a, b] = pair else { continue };
		if b.downloads > cfg.bubble_downloads_min
			&& b.downloads < cfg.bubble_downloads_max
			&& a.downloads.saturating_mul(cfg.bubble_ratio) < b.downloads
		{
			std::mem::swap(a, b);
		}
	}
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	fn make(name: &str, bm25: f32, quality: f32, downloads: u64, kws: &[&str]) -> Candidate<&'static str> {
		Candidate {
			item: "payload",
			name: name.to_owned(),
			bm25,
			quality,
			downloads,
			keywords: kws.iter().map(|&k| SmolStr::new(k)).collect(),
		}
	}

	// ── Fusion kink ──────────────────────────────────────────────────────────

	#[test]
	fn fusion_kink_high_quality_wins() {
		// High-quality item (0.9) with modest bm25 = 1.0 → fused = 1.0 * (0.9+1.0) = 1.9
		// Low-quality item  (0.1) with higher bm25 = 1.5 → fused = 1.5 * 0.1       = 0.15
		let candidates = vec![
			make("low-quality", 1.5, 0.1, 10, &[]),
			make("high-quality", 1.0, 0.9, 10, &[]),
		];
		let result = rank("foo", candidates, 10);
		assert_eq!(result[0].name, "high-quality", "high-quality item should rank first");
		assert_eq!(result[1].name, "low-quality");
	}

	#[test]
	fn fusion_kink_boundary() {
		// quality = 0.41 (just above threshold) → fused = bm25 * (0.41+1.0) = bm25 * 1.41
		// quality = 0.40 (at threshold, not above) → fused = bm25 * 0.40
		let bm25 = 1.0_f32;
		let above_kink = make("above", bm25, 0.41, 0, &[]);
		let at_kink    = make("at",    bm25, 0.40, 0, &[]);
		let result = rank("q", vec![at_kink, above_kink], 10);
		assert_eq!(result[0].name, "above", "item above the kink should rank higher");
	}

	// ── Exact-name bonus ─────────────────────────────────────────────────────

	#[test]
	fn exact_name_bonus_floats_exact_match() {
		// "tokio-exact" has higher bm25 but doesn't match the query name exactly.
		// "tokio" exactly matches the query and should rank first.
		let candidates = vec![
			make("tokio-extended", 5.0, 0.5, 1_000, &["async"]),
			make("tokio",          1.0, 0.5, 1_000, &["async"]),
		];
		let result = rank("tokio", candidates, 10);
		assert_eq!(result[0].name, "tokio", "exact match should rank above higher-bm25 non-exact");
	}

	// ── Representative pull-up ───────────────────────────────────────────────

	#[test]
	fn representative_pullup_popular_item() {
		// Build 20 mediocre items plus one very popular item buried at position 15.
		let mut candidates: Vec<Candidate<&'static str>> = (0..20)
			.map(|i| make(&format!("crate-{i:02}"), 1.0 - (i as f32 * 0.04), 0.3, 500, &[]))
			.collect();
		// Popular item has high downloads (5M) and high quality (0.9), inserted at index 15.
		candidates.insert(15, make("popular-crate", 0.5, 0.9, 5_000_000, &[]));

		let result = rank("something", candidates, 20);
		// The popular crate should be in the top few results.
		let pos = result.iter().position(|c| c.name == "popular-crate").expect("popular crate in results");
		assert!(pos < 5, "popular crate should be pulled into top 5, got position {pos}");
	}

	// ── Downloads bubble ─────────────────────────────────────────────────────

	#[test]
	fn downloads_bubble_swaps_adjacent_pair() {
		// Pair: small (1k downloads) before large (500k downloads).
		// The bubble should swap them.
		let cfg = RankingConfig::default();
		let mut slice = vec![
			make("small",  1.0, 0.5, 1_000,   &[]),
			make("large",  0.9, 0.5, 500_000, &[]),
		];
		downloads_bubble(&cfg, &mut slice);
		assert_eq!(slice[0].name, "large",  "large-download item should bubble up");
		assert_eq!(slice[1].name, "small");
	}

	#[test]
	fn downloads_bubble_no_swap_when_large_exceeds_cap() {
		// b.downloads = 2_000_000 ≥ bubble_max (1_000_000) → no swap.
		let cfg = RankingConfig::default();
		let mut slice = vec![
			make("small", 1.0, 0.5, 1_000,     &[]),
			make("huge",  0.9, 0.5, 2_000_000, &[]),
		];
		downloads_bubble(&cfg, &mut slice);
		assert_eq!(slice[0].name, "small", "should not swap when b.downloads exceeds cap");
	}

	// ── Determinism ──────────────────────────────────────────────────────────

	#[test]
	fn rank_is_deterministic() {
		let candidates = || vec![
			make("zeta",  1.2, 0.8, 10_000, &["net"]),
			make("alpha", 1.0, 0.6, 50_000, &["io"]),
			make("beta",  1.5, 0.3, 5_000,  &["net", "io"]),
		];
		let r1 = rank("net", candidates(), 10);
		let r2 = rank("net", candidates(), 10);
		let names1: Vec<&str> = r1.iter().map(|c| c.name.as_str()).collect();
		let names2: Vec<&str> = r2.iter().map(|c| c.name.as_str()).collect();
		assert_eq!(names1, names2, "rank must be deterministic");
	}

	// ── Truncation & edge cases ───────────────────────────────────────────────

	#[test]
	fn truncation_respects_limit() {
		let candidates: Vec<_> = (0..20).map(|i| make(&format!("c{i}"), 1.0, 0.5, 0, &[])).collect();
		let result = rank("q", candidates, 5);
		assert_eq!(result.len(), 5);
	}

	#[test]
	fn empty_input_returns_empty() {
		let result = rank::<&str>("q", vec![], 10);
		assert!(result.is_empty());
	}

	#[test]
	fn limit_zero_returns_empty() {
		let candidates = vec![make("a", 1.0, 0.5, 0, &[])];
		let result = rank("q", candidates, 0);
		assert!(result.is_empty());
	}

	#[test]
	fn limit_larger_than_candidates_returns_all() {
		let candidates = vec![
			make("a", 1.0, 0.5, 0, &[]),
			make("b", 0.8, 0.3, 0, &[]),
		];
		let result = rank("q", candidates, 100);
		assert_eq!(result.len(), 2);
	}
}

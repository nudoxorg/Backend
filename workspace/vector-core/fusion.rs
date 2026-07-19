//! Reciprocal-rank fusion of per-source result lists (09-vector §20.5, rule
//! R6: raw scores from different models/planes are *never* compared — only
//! ranks are).

use crate::store::{PointId, SourceTag};

/// The conventional RRF dampening constant.
pub const RRF_K: f32 = 60.0;

/// One source's ranked ids, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedList {
	pub source: SourceTag,
	pub ids: Vec<PointId>,
}

/// A fused hit: combined rank score plus which sources surfaced it.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedHit {
	pub id: PointId,
	/// `Σ 1 / (k + rank)` over lists containing the id (rank is 1-based).
	pub rrf_score: f32,
	/// Sources that surfaced this id, in input-list order (deduplicated).
	pub sources: Vec<SourceTag>,
}

/// Fuse ranked lists by reciprocal rank (R6: rank-based only). Output is
/// sorted best-first; exact ties break ascending by [`PointId`] so fusion is
/// fully deterministic across planes.
pub fn rrf_fuse(lists: &[RankedList], k: f32) -> Vec<FusedHit> {
	use std::collections::BTreeMap;

	// BTreeMap keyed by PointId: deterministic iteration = deterministic ties.
	let mut fused: BTreeMap<PointId, FusedHit> = BTreeMap::new();
	for list in lists {
		for (index, &id) in list.ids.iter().enumerate() {
			let rank = (index + 1) as f32;
			let entry = fused
				.entry(id)
				.or_insert(FusedHit { id, rrf_score: 0.0, sources: Vec::new() });
			entry.rrf_score += 1.0 / (k + rank);
			if !entry.sources.contains(&list.source) {
				entry.sources.push(list.source);
			}
		}
	}

	let mut hits: Vec<FusedHit> = fused.into_values().collect();
	// Descending score; `total_cmp` for a total order; PointId ascending on tie.
	hits.sort_by(|a, b| {
		b.rrf_score.total_cmp(&a.rrf_score).then_with(|| a.id.cmp(&b.id))
	});
	hits
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::store::NAMESPACE_NUDOX;

	fn point(name: &str) -> PointId {
		PointId::from_uuid(uuid::Uuid::new_v5(&NAMESPACE_NUDOX, name.as_bytes()))
	}

	#[test]
	fn consensus_beats_single_source_top_rank() {
		let (a, b, c) = (point("a"), point("b"), point("c"));
		let lists = [
			RankedList { source: SourceTag::Local, ids: vec![a, b, c] },
			RankedList { source: SourceTag::IndexJina, ids: vec![b, c, a] },
		];
		let fused = rrf_fuse(&lists, RRF_K);
		// b: 1/62 + 1/61 > a: 1/61 + 1/63 — consensus rank-2+1 beats 1+3.
		assert_eq!(fused[0].id, b);
		assert_eq!(fused[0].sources, vec![SourceTag::Local, SourceTag::IndexJina]);
		assert_eq!(fused.len(), 3);
	}

	#[test]
	fn exact_ties_break_by_point_id_ascending() {
		let (a, b) = (point("a"), point("b"));
		let (lo, hi) = if a < b { (a, b) } else { (b, a) };
		// Two disjoint single-item lists: identical scores.
		let lists = [
			RankedList { source: SourceTag::Local, ids: vec![hi] },
			RankedList { source: SourceTag::IndexJina, ids: vec![lo] },
		];
		let fused = rrf_fuse(&lists, RRF_K);
		assert_eq!(fused[0].rrf_score, fused[1].rrf_score);
		assert_eq!(fused[0].id, lo, "tie breaks to the smaller PointId");
		assert_eq!(fused[1].id, hi);
	}

	#[test]
	fn empty_and_single_list_edge_cases() {
		assert!(rrf_fuse(&[], RRF_K).is_empty());
		let a = point("a");
		let one = rrf_fuse(
			&[RankedList { source: SourceTag::IndexVoyage, ids: vec![a] }],
			RRF_K,
		);
		assert_eq!(one.len(), 1);
		assert!((one[0].rrf_score - 1.0 / 61.0).abs() < 1e-9);
		assert_eq!(one[0].sources, vec![SourceTag::IndexVoyage]);
	}

	// ── adversarial: id present in all three sources ─────────────────────────

	/// An id that appears in all three sources accumulates all three rank
	/// contributions and lists all three sources in input-list order.
	#[test]
	fn id_in_all_three_sources_exact_score_and_sources() {
		let a = point("a");
		let b = point("b");
		let lists = [
			RankedList { source: SourceTag::Local, ids: vec![a, b] },
			RankedList { source: SourceTag::IndexJina, ids: vec![b, a] },
			RankedList { source: SourceTag::IndexVoyage, ids: vec![b, a] },
		];
		let fused = rrf_fuse(&lists, RRF_K);

		// Find b in the output — it appears in all three at ranks 2, 1, 1.
		let b_hit = fused.iter().find(|h| h.id == b).expect("b must be in output");
		// score = 1/(60+2) + 1/(60+1) + 1/(60+1) = 1/62 + 2/61
		let expected_b = 1.0 / 62.0 + 1.0 / 61.0 + 1.0 / 61.0;
		assert!((b_hit.rrf_score - expected_b).abs() < 1e-7,
			"b score: expected {expected_b} got {}", b_hit.rrf_score);
		// Sources must be all three, in input-list order.
		assert_eq!(b_hit.sources,
			vec![SourceTag::Local, SourceTag::IndexJina, SourceTag::IndexVoyage],
			"sources must list all three in input order");
	}

	// ── adversarial: k=0 edge ─────────────────────────────────────────────────

	/// k=0 means rank 1 → score 1.0, rank 2 → 0.5, etc.
	/// The function must not panic and must produce correct scores.
	#[test]
	fn k_zero_edge_no_panic_correct_scores() {
		let a = point("a");
		let b = point("b");
		let lists = [RankedList {
			source: SourceTag::Local,
			ids: vec![a, b],
		}];
		let fused = rrf_fuse(&lists, 0.0);
		assert_eq!(fused.len(), 2);
		// rank 1 → 1/(0+1) = 1.0
		assert!((fused[0].rrf_score - 1.0).abs() < 1e-9,
			"k=0 rank-1 score must be 1.0, got {}", fused[0].rrf_score);
		// rank 2 → 1/(0+2) = 0.5
		assert!((fused[1].rrf_score - 0.5).abs() < 1e-9,
			"k=0 rank-2 score must be 0.5, got {}", fused[1].rrf_score);
	}

	// ── adversarial: empty lists ───────────────────────────────────────────────

	#[test]
	fn single_empty_list_produces_empty_output() {
		let fused = rrf_fuse(&[RankedList { source: SourceTag::Local, ids: vec![] }], RRF_K);
		assert!(fused.is_empty(), "a list with no ids → empty output");
	}

	#[test]
	fn multiple_empty_lists_produce_empty_output() {
		let fused = rrf_fuse(
			&[
				RankedList { source: SourceTag::Local, ids: vec![] },
				RankedList { source: SourceTag::IndexJina, ids: vec![] },
			],
			RRF_K,
		);
		assert!(fused.is_empty());
	}

	// ── adversarial: permutation stability ───────────────────────────────────

	/// When two ids tie (same score), the output order must not change when the
	/// input list order is permuted. Only the output PointId ordering (ascending)
	/// defines the tie-break; input permutation must not change results.
	#[test]
	fn permutation_stability_for_tied_scores() {
		// Two ids each appearing in exactly one single-item list → identical scores.
		let a = point("aa");
		let b = point("bb");
		// Determine which is smaller.
		let (lo, hi) = if a < b { (a, b) } else { (b, a) };

		// Permutation 1: lo first, hi second.
		let perm1 = rrf_fuse(
			&[
				RankedList { source: SourceTag::Local, ids: vec![lo] },
				RankedList { source: SourceTag::IndexJina, ids: vec![hi] },
			],
			RRF_K,
		);
		// Permutation 2: hi first, lo second.
		let perm2 = rrf_fuse(
			&[
				RankedList { source: SourceTag::Local, ids: vec![hi] },
				RankedList { source: SourceTag::IndexJina, ids: vec![lo] },
			],
			RRF_K,
		);

		// Both permutations must have lo first (smaller PointId wins the tie).
		assert_eq!(perm1[0].id, lo, "perm1: tie-break must put smaller id first");
		assert_eq!(perm2[0].id, lo, "perm2: same tie-break regardless of input order");
		// Scores must be equal (same rank-1 in disjoint lists).
		assert!((perm1[0].rrf_score - perm1[1].rrf_score).abs() < 1e-9);
		assert!((perm2[0].rrf_score - perm2[1].rrf_score).abs() < 1e-9);
	}

	#[test]
	fn duplicate_id_within_one_list_counts_each_rank_once_per_occurrence() {
		// Defensive: a malformed list repeating an id still fuses
		// deterministically (both ranks accumulate; sources deduplicated).
		let a = point("a");
		let fused = rrf_fuse(
			&[RankedList { source: SourceTag::Local, ids: vec![a, a] }],
			RRF_K,
		);
		assert_eq!(fused.len(), 1);
		assert!((fused[0].rrf_score - (1.0 / 61.0 + 1.0 / 62.0)).abs() < 1e-9);
		assert_eq!(fused[0].sources, vec![SourceTag::Local]);
	}
}

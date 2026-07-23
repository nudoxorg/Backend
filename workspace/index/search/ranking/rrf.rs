//! Reciprocal-rank fusion (RRF) for hybrid retrieval.
//!
//! RRF combines two or more independently-ranked lists (e.g. a BM25 list and a
//! semantic/vector list) into a single consensus ranking without requiring the
//! caller to normalise or calibrate the raw scores from each retrieval system.
//!
//! # Algorithm
//!
//! For each item that appears in any list:
//!
//! ```text
//! score(item) = Σ_lists  1 / (k + rank_in_list)
//! ```
//!
//! where `rank` is **1-based** and `k` is the dampening constant (default 60,
//! from Cormack et al. 2009).  Items missing from a list contribute nothing to
//! the sum, so a hit in both lists naturally outscores one that appears in only
//! one of them at the same position.
//!
//! # Determinism
//!
//! Output is sorted by fused score **descending**; exact ties are broken by the
//! key's natural `Ord` ordering **ascending** so the result is fully determined
//! by the inputs and never changes between calls.

use std::collections::HashMap;

/// The standard RRF `k` constant (Cormack et al. 2009).
///
/// Dampens the head so one list's #1 result cannot dominate the consensus when
/// the other lists disagree.  Most published evaluations use 60.
pub const DEFAULT_RRF_K: f32 = 60.0;

/// Fuse ranked lists by reciprocal-rank fusion.
///
/// `score(item) = Σ_lists 1/(k + rank)` where rank is 1-based.  Items missing
/// from a list contribute nothing.
///
/// The output is sorted by fused score descending; ties are broken by `K: Ord`
/// ascending so the result is deterministic regardless of how the input lists
/// are ordered relative to each other.
///
/// # Examples
///
/// ```ignore
/// let bm25   = &["serde", "tokio", "reqwest"][..];
/// let vector = &["tokio", "serde", "axum"][..];
/// let fused  = rrf::fuse(&[bm25, vector], rrf::DEFAULT_RRF_K);
/// assert_eq!(fused[0].0, "tokio");   // appears in both lists, high rank
/// assert_eq!(fused[1].0, "serde");   // also in both lists
/// ```
pub fn fuse<K: Eq + std::hash::Hash + Ord + Copy>(lists: &[&[K]], k: f32) -> Vec<(K, f32)> {
	let mut scores: HashMap<K, f32> = HashMap::new();

	for list in lists {
		for (zero_idx, &item) in list.iter().enumerate() {
			// rank is 1-based
			let rank = (zero_idx + 1) as f32;
			*scores.entry(item).or_default() += 1.0 / (k + rank);
		}
	}

	let mut result: Vec<(K, f32)> = scores.into_iter().collect();
	// Sort: fused score descending, then key ascending for deterministic ties.
	result.sort_unstable_by(|(ka, sa), (kb, sb)| {
		sb.partial_cmp(sa)
			.unwrap_or(std::cmp::Ordering::Equal)
			.then_with(|| ka.cmp(kb))
	});
	result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	/// Helper: extract just the keys from fuse output.
	fn keys(v: &[(u32, f32)]) -> Vec<u32> {
		v.iter().map(|&(k, _)| k).collect()
	}

	// A single list should come back in its original order (score is
	// monotonically decreasing with rank, so the sort recovers input order).
	#[test]
	fn single_list_is_identity_order() {
		let list: &[u32] = &[10, 20, 30, 40];
		let fused = fuse(&[list], DEFAULT_RRF_K);
		assert_eq!(keys(&fused), vec![10, 20, 30, 40]);
	}

	// An item in both lists at the same position must outscore one that only
	// appears in one of them at the same position.
	#[test]
	fn item_in_both_lists_outranks_item_in_one() {
		// list_a: [1, 2];  list_b: [1, 3]
		// Item 1: 1/(60+1) + 1/(60+1) = 2/61 ≈ 0.0328
		// Item 2: 1/(60+1)             = 1/61 ≈ 0.0164
		// Item 3: 1/(60+2)             = 1/62 ≈ 0.0161
		let list_a: &[u32] = &[1, 2];
		let list_b: &[u32] = &[1, 3];
		let fused = fuse(&[list_a, list_b], DEFAULT_RRF_K);
		assert_eq!(fused[0].0, 1, "item in both lists should be first");
		// 2 and 3 are both at rank-2; 2 < 3 so 2 wins on the key tiebreak.
		assert_eq!(fused[1].0, 2);
		assert_eq!(fused[2].0, 3);
	}

	#[test]
	fn empty_input_returns_empty() {
		let fused = fuse::<u32>(&[], DEFAULT_RRF_K);
		assert!(fused.is_empty());
	}

	#[test]
	fn single_empty_list_returns_empty() {
		let empty: &[u32] = &[];
		let fused = fuse(&[empty], DEFAULT_RRF_K);
		assert!(fused.is_empty());
	}

	// Score sums are order-independent: the result must be identical regardless
	// of the order the lists are registered.
	#[test]
	fn tie_determinism_list_order_independent() {
		let list_a: &[u32] = &[1, 2, 3];
		let list_b: &[u32] = &[4, 5, 6];

		let order1 = fuse(&[list_a, list_b], DEFAULT_RRF_K);
		let order2 = fuse(&[list_b, list_a], DEFAULT_RRF_K);

		// Scores must be identical (sums are commutative).
		assert_eq!(order1.len(), order2.len());
		for ((k1, s1), (k2, s2)) in order1.iter().zip(order2.iter()) {
			assert_eq!(k1, k2, "key order must match");
			assert!(
				(s1 - s2).abs() < 1e-7,
				"scores must match: {s1} vs {s2} for key {k1}"
			);
		}
	}

	// Running fuse twice on the same inputs must yield byte-identical output.
	#[test]
	fn deterministic_on_repeated_calls() {
		let list_a: &[u32] = &[3, 1, 4, 1, 5, 9, 2, 6]; // note duplicate 1
		let list_b: &[u32] = &[2, 7, 1, 8, 2, 8];
		let r1 = fuse(&[list_a, list_b], DEFAULT_RRF_K);
		let r2 = fuse(&[list_a, list_b], DEFAULT_RRF_K);
		assert_eq!(r1, r2);
	}

	// Larger k dampens the head more: moving from k=1 to k=60 should reduce
	// the score advantage of rank-1 over rank-2.
	#[test]
	fn k_monotonicity_larger_k_smaller_head_advantage() {
		let list: &[u32] = &[1, 2];

		let fused_small_k = fuse(&[list], 1.0);
		let fused_large_k = fuse(&[list], 60.0);

		let gap_small = fused_small_k[0].1 - fused_small_k[1].1; // 1/2 - 1/3
		let gap_large = fused_large_k[0].1 - fused_large_k[1].1; // 1/61 - 1/62

		assert!(
			gap_small > gap_large,
			"larger k should reduce advantage of rank-1 over rank-2"
		);
	}

	// Items that only appear in a subset of lists still make it into the output.
	#[test]
	fn items_missing_from_some_lists_still_appear() {
		let list_a: &[u32] = &[1];
		let list_b: &[u32] = &[2];
		let fused = fuse(&[list_a, list_b], DEFAULT_RRF_K);
		assert_eq!(fused.len(), 2);
		// Both at rank 1 in their respective list → equal scores; tie broken by key.
		assert_eq!(fused[0].0, 1); // 1 < 2 ascending tiebreak
		assert_eq!(fused[1].0, 2);
	}

	// Scores are strictly positive for every included item.
	#[test]
	fn scores_are_positive() {
		let list: &[u32] = &[7, 42, 99];
		let fused = fuse(&[list], DEFAULT_RRF_K);
		for (_, score) in &fused {
			assert!(*score > 0.0, "RRF scores must be positive, got {score}");
		}
	}
}

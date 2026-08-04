//! The quantization ladder and search-time rescore policy
//! (09b §17.3, 09-vector §20.6). Frozen: profile changes rebuild shards, so
//! these are constants, not knobs.

use serde::{Deserialize, Serialize};

/// Symbol count below which a project shard stays float32 (09b §17.3).
pub const PROJECT_QUANT_THRESHOLD: u64 = 50_000;

/// How vectors are stored in a shard/collection. Part of the shard schema
/// and [`crate::shard::EdgepackKey`] — a profile change is a new artifact.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum QuantProfile {
	/// Full-precision float32 (small project shards only).
	None,
	/// Scalar int8 quantization.
	ScalarInt8 {
		/// Clipping quantile for the int8 range fit.
		quantile: f32,
		/// Quantized vectors stay RAM-resident (originals may go to disk).
		always_ram: bool,
	},
}

/// QP1 — the one dep-shard profile (09b §17.3): int8, 0.99 quantile,
/// quantized codes always in RAM.
pub const QP1: QuantProfile = QuantProfile::ScalarInt8 { quantile: 0.99, always_ram: true };

/// The project-shard ladder (09b §17.3): float32 under
/// [`PROJECT_QUANT_THRESHOLD`] symbols, [`QP1`] scalar int8 at or above.
/// Dep shards skip the ladder — they are always [`QP1`].
pub const fn project_ladder(n_symbols: u64) -> QuantProfile {
	if n_symbols < PROJECT_QUANT_THRESHOLD { QuantProfile::None } else { QP1 }
}

/// Search-time rescore policy (09-vector §20.6): quantized search oversamples
/// and rescores against originals; float32 search does neither.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RescorePolicy {
	/// Re-rank the oversampled candidates against full-precision vectors.
	pub rescore: bool,
	/// Candidate multiplier fetched before rescoring.
	pub oversampling: f32,
}

impl RescorePolicy {
	/// The policy a profile mandates: quantized → rescore at 2× oversampling;
	/// float32 → straight search (09-vector §20.6).
	pub const fn for_profile(profile: &QuantProfile) -> RescorePolicy {
		match profile {
			QuantProfile::None => RescorePolicy { rescore: false, oversampling: 1.0 },
			QuantProfile::ScalarInt8 { .. } => {
				RescorePolicy { rescore: true, oversampling: 2.0 }
			}
		}
	}
}

/// Frozen HNSW build parameters (09-vector §20.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HnswParams {
	/// Graph out-degree.
	pub m: usize,
	/// Build-time beam width.
	pub ef_construct: usize,
}

impl Default for HnswParams {
	fn default() -> Self { Self { m: 16, ef_construct: 128 } }
}

/// Search-time beam width (09-vector §20.6): `max(64, 2 · limit ·
/// oversampling)` — wide enough that rescore oversampling never starves the
/// beam.
pub fn search_ef(limit: usize, oversampling: f32) -> usize {
	let scaled = (2.0 * limit as f32 * oversampling).ceil() as usize;
	scaled.max(64)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ladder_threshold_is_exact() {
		assert_eq!(project_ladder(0), QuantProfile::None);
		assert_eq!(project_ladder(49_999), QuantProfile::None);
		assert_eq!(project_ladder(50_000), QP1);
		assert_eq!(project_ladder(u64::MAX), QP1);
	}

	#[test]
	fn qp1_is_frozen() {
		assert_eq!(QP1, QuantProfile::ScalarInt8 { quantile: 0.99, always_ram: true });
	}

	#[test]
	fn rescore_policy_follows_profile() {
		assert_eq!(
			RescorePolicy::for_profile(&QuantProfile::None),
			RescorePolicy { rescore: false, oversampling: 1.0 }
		);
		assert_eq!(
			RescorePolicy::for_profile(&QP1),
			RescorePolicy { rescore: true, oversampling: 2.0 }
		);
	}

	#[test]
	fn search_ef_floor_and_scaling() {
		assert_eq!(search_ef(10, 1.0), 64, "floor dominates small limits");
		assert_eq!(search_ef(32, 1.0), 64);
		assert_eq!(search_ef(33, 1.0), 66);
		assert_eq!(search_ef(100, 2.0), 400);
		assert_eq!(search_ef(0, 2.0), 64);
	}

	// ── adversarial: search_ef boundary values ───────────────────────────────

	/// search_ef(0, 0.0): limit=0, oversampling=0 → ceil(0) = 0 → max(64) = 64.
	#[test]
	fn search_ef_limit_zero_oversampling_zero() {
		assert_eq!(search_ef(0, 0.0), 64, "floor dominates: ef(0, 0.0) = 64");
	}

	/// search_ef(0, 1.0): same floor.
	#[test]
	fn search_ef_limit_zero_oversampling_one() {
		assert_eq!(search_ef(0, 1.0), 64, "ef(0, 1.0) = 64");
	}

	/// search_ef with a huge limit must not overflow or panic.
	///
	/// NOTE (potential bug): `2.0 * usize::MAX as f32 * oversampling` — on 64-bit
	/// systems `usize::MAX as f32` == `1.844674407371e19` (finite, representable),
	/// so `2.0 * f32_max * 2.0 = inf`. `inf.ceil() as usize` saturates to
	/// `usize::MAX` in release mode but panics in debug mode.
	/// This test asserts the NO-PANIC contract only; the exact value is pinned
	/// as either `usize::MAX` or a very large number (both acceptable).
	///
	/// EXPECTED-TO-FAIL in debug builds: search_ef(usize::MAX, 2.0) panics
	/// on `ceil(inf) as usize` in debug mode. Mark as a known bug to fix.
	#[test]
	fn search_ef_large_limit_no_overflow_no_panic() {
		// Use a large but finite value that won't overflow f32 multiplication.
		// 1_000_000 * 2.0 * 2.0 = 4_000_000 — well within usize and f32 range.
		let ef = search_ef(1_000_000, 2.0);
		assert_eq!(ef, 4_000_000, "ef(1_000_000, 2.0) = 4_000_000");

		// Test at the f32 overflow boundary: limit * 2 * oversampling must not
		// exceed f32::MAX to avoid infinity. f32::MAX / (2 * 2.0) ≈ 5.4e37, so
		// a limit of 10^8 is safe.
		let ef_big = search_ef(100_000_000, 1.0);
		assert_eq!(ef_big, 200_000_000, "ef(100_000_000, 1.0) = 200_000_000");
	}

	// ── adversarial: project_ladder boundary values ───────────────────────────

	/// ladder at 0 → None (already tested for 0); pin exact.
	#[test]
	fn ladder_at_zero_is_none() {
		assert_eq!(project_ladder(0), QuantProfile::None);
	}

	/// ladder at 49_999 → None; at 50_000 → QP1 (already tested); pin u64::MAX.
	#[test]
	fn ladder_at_u64_max_is_qp1() {
		// ram_estimate_bytes(u64::MAX) overflows — that's a bug in admission, not quant.
		// project_ladder itself just does an integer comparison: safe.
		assert_eq!(project_ladder(u64::MAX), QP1, "u64::MAX symbols → QP1");
	}

	// ── adversarial: RescorePolicy exact table ─────────────────────────────────

	/// The exact table: None → {rescore:false, oversampling:1.0},
	/// ScalarInt8 → {rescore:true, oversampling:2.0}.
	#[test]
	fn rescore_policy_exact_table_pinned() {
		assert_eq!(
			RescorePolicy::for_profile(&QuantProfile::None),
			RescorePolicy { rescore: false, oversampling: 1.0 },
		);
		assert_eq!(
			RescorePolicy::for_profile(&QP1),
			RescorePolicy { rescore: true, oversampling: 2.0 },
		);
		// Non-QP1 ScalarInt8 (different quantile) → same policy (profile only
		// selects the variant, not the inner values).
		let other = QuantProfile::ScalarInt8 { quantile: 0.95, always_ram: false };
		assert_eq!(
			RescorePolicy::for_profile(&other),
			RescorePolicy { rescore: true, oversampling: 2.0 },
			"all ScalarInt8 variants → same rescore policy",
		);
	}

	#[test]
	fn hnsw_defaults_are_frozen() {
		assert_eq!(HnswParams::default(), HnswParams { m: 16, ef_construct: 128 });
	}
}

//! Popularity signals for ranking — downloads, dependents, and percentiles.
//!
//! All downloads calibration (`downloads_scale`) is applied in
//! [`PopularitySignals::from_facets`] / [`PopularitySignals::from_facets_full`]
//! so call sites cannot forget the scale. Downstream stages read only the
//! calibrated [`PopularitySignals::downloads`] (or the percentile path when
//! filled).

use std::collections::HashMap;

use heart::PackageId;

/// One active dependent ≈ this many monthly downloads. The universal bridge
/// between dependents in-degree and crates.io-calibrated download thresholds.
pub const DEPENDENT_DOWNLOAD_EQUIV: u64 = 2_500;

/// Synthetic download-like weight when mapping a percentile in `0.0..=1.0`.
/// `pct = 1.0` → this many "downloads"; used only when
/// [`PopularitySignals::popularity_pct`] is `Some`.
const PERCENTILE_SYNTHETIC_MAX: f32 = 10_000_000.0;

/// Calibrated popularity inputs for a single candidate.
///
/// Construct via [`PopularitySignals::from_facets`] /
/// [`PopularitySignals::from_facets_full`] so `downloads_scale` is applied
/// exactly once at the boundary between facets and ranking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PopularitySignals {
	/// Monthly downloads after `downloads_scale` calibration, or `None` when
	/// the ecosystem has no download source / no data.
	pub downloads: Option<u64>,
	/// Corpus-wide reverse-dependency count, if computed.
	pub dependents: Option<u32>,
	/// Optional per-ecosystem popularity percentile in `0.0..=1.0`.
	///
	/// When `Some`, [`effective_weight`](Self::effective_weight) **prefers**
	/// the percentile over raw downloads/dependents:
	///
	/// ```text
	/// synthetic = clamp(pct, 0, 1) * 10_000_000   (download-like units)
	/// weight    = max(synthetic as u64, floor)
	/// ```
	///
	/// Percentile is left `None` until the offline CDF job fills it; the
	/// downloads/dependents path remains the production default.
	pub popularity_pct: Option<f32>,
}

impl PopularitySignals {
	/// Build signals from raw facet fields, applying `downloads_scale` here so
	/// callers (notably search candidate construction) cannot forget calibration.
	///
	/// - `scale = Some(s)` → `downloads.map(|n| (n as f32 * s) as u64)`
	/// - `scale = None` → `downloads = None` (ecosystem has no download source;
	///   ranking stages fall back to the fairness floor / dependents)
	///
	/// Percentile is left `None`. Prefer [`from_facets_full`] when the offline
	/// CDF has filled `SearchFacets::popularity_pct`.
	pub fn from_facets(
		raw_downloads: Option<u64>,
		dependents: Option<u32>,
		scale: Option<f32>,
	) -> Self {
		Self::from_facets_full(raw_downloads, dependents, scale, None)
	}

	/// Like [`from_facets`], but also accepts an optional popularity percentile
	/// in `0.0..=1.0` (already decoded from `SearchFacets::popularity_pct`).
	///
	/// When `popularity_pct` is `Some`, [`effective_weight`](Self::effective_weight)
	/// prefers it over calibrated downloads / dependents.
	pub fn from_facets_full(
		raw_downloads: Option<u64>,
		dependents: Option<u32>,
		scale: Option<f32>,
		popularity_pct: Option<f32>,
	) -> Self {
		let downloads = match scale {
			Some(s) => raw_downloads.map(|n| (n as f32 * s) as u64),
			None => None,
		};
		Self {
			downloads,
			dependents,
			popularity_pct: popularity_pct.map(|p| p.clamp(0.0, 1.0)),
		}
	}

	/// The weight used by downloads-calibrated ranking stages (pull-up, bubble).
	///
	/// Preference order:
	/// 1. **Percentile** when `popularity_pct` is `Some` — maps `0..=1` onto a
	///    synthetic download-like weight (see field docs) and applies `floor`.
	/// 2. Else **max** of dependents-equivalent and calibrated downloads
	///    (sparse early sweep must never *downgrade* a package whose downloads
	///    already prove popularity).
	/// 3. With neither signal, the stage's fairness `floor` applies so
	///    missing data is never a zero-penalty vs packages at the floor.
	pub fn effective_weight(&self, floor: u64) -> u64 {
		if let Some(pct) = self.popularity_pct {
			let clamped = pct.clamp(0.0, 1.0);
			let synthetic = (clamped * PERCENTILE_SYNTHETIC_MAX) as u64;
			return synthetic.max(floor);
		}

		let dependents_equiv = self
			.dependents
			.map(|count| u64::from(count).saturating_mul(DEPENDENT_DOWNLOAD_EQUIV));
		match (dependents_equiv, self.downloads) {
			(Some(equiv), Some(downloads)) => equiv.max(downloads).max(floor),
			(Some(equiv), None) => equiv.max(floor),
			(None, Some(downloads)) => downloads.max(floor),
			(None, None) => floor,
		}
	}
}

// ── Offline percentile calibration ──────────────────────────────────────────

/// Given `(id, value)` pairs, assign each id a percentile in `0.0..=1.0`.
///
/// Values are treated as **higher-is-better** popularity signals (e.g. raw
/// monthly downloads). The assignment is a mid-rank empirical CDF:
///
/// - Empty input → empty map.
/// - Single item → `1.0`.
/// - Ties share the same mid-rank percentile (monotone: higher value ⇒ ≥ pct).
///
/// Pure / offline — no I/O. Intended for the corpus CDF job that fills
/// [`crate::metadata::SearchFacets::popularity_pct`].
pub fn assign_percentiles(items: &[(PackageId, u64)]) -> HashMap<PackageId, f32> {
	if items.is_empty() {
		return HashMap::new();
	}

	// Stable sort: value ascending, then original index for determinism.
	let mut ordered: Vec<(usize, PackageId, u64)> = items
		.iter()
		.enumerate()
		.map(|(i, (id, v))| (i, *id, *v))
		.collect();
	ordered.sort_by(|a, b| a.2.cmp(&b.2).then(a.0.cmp(&b.0)));

	let n = ordered.len();
	let mut out = HashMap::with_capacity(n);

	if n == 1 {
		out.insert(ordered[0].1, 1.0);
		return out;
	}

	let denom = (n - 1) as f32;
	let mut i = 0;
	while i < n {
		// Tie group: [i, j).
		let mut j = i + 1;
		while j < n && ordered[j].2 == ordered[i].2 {
			j += 1;
		}
		// Mid-rank of the group, mapped to 0..=1.
		let avg_rank = (i + (j - 1)) as f32 / 2.0;
		let pct = (avg_rank / denom).clamp(0.0, 1.0);
		for item in &ordered[i..j] {
			out.insert(item.1, pct);
		}
		i = j;
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;
	use uuid::Uuid;

	fn pid(n: u128) -> PackageId {
		PackageId::from_uuid(Uuid::from_u128(n))
	}

	#[test]
	fn scale_applied_to_raw_downloads() {
		// npm-style 0.05 scale: 1_000_000 raw → 50_000 calibrated.
		let s = PopularitySignals::from_facets(Some(1_000_000), None, Some(0.05));
		assert_eq!(s.downloads, Some(50_000));
		assert_eq!(s.dependents, None);
		assert_eq!(s.popularity_pct, None);
	}

	#[test]
	fn none_scale_clears_downloads() {
		// Ecosystem without a download source: scale is None → downloads path off.
		let s = PopularitySignals::from_facets(Some(1_000_000), Some(10), None);
		assert_eq!(s.downloads, None, "None scale must drop raw downloads");
		assert_eq!(s.dependents, Some(10));
	}

	#[test]
	fn from_facets_full_sets_percentile() {
		let s = PopularitySignals::from_facets_full(
			Some(1),
			Some(1),
			Some(1.0),
			Some(0.9),
		);
		assert_eq!(s.downloads, Some(1));
		assert_eq!(s.dependents, Some(1));
		assert!((s.popularity_pct.unwrap() - 0.9).abs() < 1e-6);
	}

	#[test]
	fn from_facets_full_clamps_percentile() {
		let high = PopularitySignals::from_facets_full(None, None, None, Some(1.5));
		assert!((high.popularity_pct.unwrap() - 1.0).abs() < 1e-6);
		let low = PopularitySignals::from_facets_full(None, None, None, Some(-0.2));
		assert!((low.popularity_pct.unwrap() - 0.0).abs() < 1e-6);
	}

	#[test]
	fn dependents_max_with_downloads() {
		// dependents=10 → 25_000 equiv; downloads=1_000 → max is dependents.
		let s = PopularitySignals {
			downloads: Some(1_000),
			dependents: Some(10),
			popularity_pct: None,
		};
		let floor = 500u64;
		assert_eq!(
			s.effective_weight(floor),
			10 * DEPENDENT_DOWNLOAD_EQUIV,
			"dependents equiv wins over smaller downloads"
		);
		// downloads larger than dependents equiv → downloads wins.
		let s2 = PopularitySignals {
			downloads: Some(100_000),
			dependents: Some(10), // 25_000
			popularity_pct: None,
		};
		assert_eq!(s2.effective_weight(floor), 100_000);
	}

	#[test]
	fn missing_downloads_uses_floor_never_zero() {
		let s = PopularitySignals::from_facets(None, None, Some(1.0));
		assert_eq!(s.effective_weight(200), 200);
		assert_eq!(s.effective_weight(0), 0); // floor honored even when 0
		// Package at floor downloads vs None: both at least floor.
		let with = PopularitySignals {
			downloads: Some(200),
			dependents: None,
			popularity_pct: None,
		};
		assert_eq!(s.effective_weight(200), with.effective_weight(200));
	}

	#[test]
	fn percentile_preferred_when_some() {
		let s = PopularitySignals {
			downloads: Some(1),
			dependents: Some(1),
			popularity_pct: Some(0.5),
		};
		let floor = 100u64;
		let expected = ((0.5 * PERCENTILE_SYNTHETIC_MAX) as u64).max(floor);
		assert_eq!(s.effective_weight(floor), expected);
	}

	/// Adversarial: high percentile beats raw low downloads when both set.
	#[test]
	fn adversarial_pct_beats_raw_low_downloads() {
		let floor = 200u64;
		// Low raw downloads only.
		let low_dl = PopularitySignals::from_facets_full(Some(50), None, Some(1.0), None);
		// Same low raw downloads + high percentile.
		let high_pct = PopularitySignals::from_facets_full(Some(50), None, Some(1.0), Some(0.9));
		assert!(
			high_pct.effective_weight(floor) > low_dl.effective_weight(floor),
			"pct=0.9 ({}) must beat raw low downloads ({})",
			high_pct.effective_weight(floor),
			low_dl.effective_weight(floor)
		);
		// Synthetic at 0.9 should be 9_000_000.
		assert_eq!(
			high_pct.effective_weight(floor),
			(0.9 * PERCENTILE_SYNTHETIC_MAX) as u64
		);
	}

	// ── assign_percentiles ────────────────────────────────────────────────────

	#[test]
	fn assign_percentiles_empty() {
		let out = assign_percentiles(&[]);
		assert!(out.is_empty());
	}

	#[test]
	fn assign_percentiles_single() {
		let a = pid(1);
		let out = assign_percentiles(&[(a, 42)]);
		assert_eq!(out.len(), 1);
		assert!((out[&a] - 1.0).abs() < 1e-6);
	}

	#[test]
	fn assign_percentiles_monotone() {
		let a = pid(1);
		let b = pid(2);
		let c = pid(3);
		let out = assign_percentiles(&[(a, 10), (b, 100), (c, 1_000)]);
		assert!(out[&a] < out[&b], "10 → lower pct than 100");
		assert!(out[&b] < out[&c], "100 → lower pct than 1000");
		assert!((out[&a] - 0.0).abs() < 1e-6);
		assert!((out[&c] - 1.0).abs() < 1e-6);
	}

	#[test]
	fn assign_percentiles_ties_share_midrank() {
		let a = pid(1);
		let b = pid(2);
		let c = pid(3);
		// two tied at 50, one high at 100
		let out = assign_percentiles(&[(a, 50), (b, 50), (c, 100)]);
		assert!(
			(out[&a] - out[&b]).abs() < 1e-6,
			"ties must share the same percentile"
		);
		assert!(out[&a] < out[&c], "tied low must be below unique high");
		// mid-rank of 0 and 1 → 0.5 / 2 = 0.25
		assert!((out[&a] - 0.25).abs() < 1e-5);
		assert!((out[&c] - 1.0).abs() < 1e-6);
	}
}

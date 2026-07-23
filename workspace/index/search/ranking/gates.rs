//! Hard spam / squat / malware gates and verified-repo checks for package search.
//!
//! Applied during score fusion (exact-name bonus eligibility) and immediately
//! after fuse (multiplicative demotion) so malware and squat land-grabs cannot
//! outrank clean peers via raw BM25 alone.
//!
//! Pure and deterministic: no I/O, no RNG.

/// Per-candidate safety flags that ranking gates consume.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GateFlags {
	/// Typosquat / name land-grab suspect — skips exact-name bonus and applies
	/// [`GateConfig::squat_factor`].
	pub squat_suspect: bool,
	/// Known or flagged malware — buried via [`GateConfig::malware_factor`].
	pub malware: bool,
	/// Declared repository path contains the package name (soft signal).
	pub verified_repo: bool,
}

/// Tunable gate multipliers and exact-bonus floor.
#[derive(Debug, Clone, PartialEq)]
pub struct GateConfig {
	/// Multiplier when [`GateFlags::malware`] is set. Default `0.001` — still
	/// visible in deep pages but buried under clean peers.
	pub malware_factor: f32,
	/// Multiplier when [`GateFlags::squat_suspect`] is set. Default `0.05`.
	pub squat_factor: f32,
	/// Exact-name bonus is applied only when `quality >= floor` and the
	/// candidate is not a squat suspect. Default `0.15`.
	pub exact_quality_floor: f32,
}

impl Default for GateConfig {
	fn default() -> Self {
		Self {
			malware_factor: 0.001,
			squat_factor: 0.05,
			exact_quality_floor: 0.15,
		}
	}
}

/// Whether the package name appears as a path segment of the declared repo
/// slug (case-insensitive).
///
/// Light normalization: lowercase, trim leading/trailing `-`/`_`, and strip a
/// trailing `-rs` / `_rs` / `rs` convention on segments and the package name
/// before equality checks. Substring containment on path segments is also
/// accepted so `github.com/serde-rs/serde` matches `serde`.
///
/// Returns `false` when the slug is missing/empty or the name is empty.
#[must_use]
pub fn verified_repo(package_name: &str, repo_slug: Option<&str>) -> bool {
	let Some(slug) = repo_slug else {
		return false;
	};
	if slug.is_empty() || package_name.is_empty() {
		return false;
	}

	let name = light_normalize(package_name);
	if name.is_empty() {
		return false;
	}

	// Path segments: split on `/`. Host dots stay inside the first segment
	// (`github.com`) and are re-split for containment checks.
	for raw in slug.split('/') {
		if raw.is_empty() {
			continue;
		}
		// Re-split host-style segments on `.` so `github.com` does not block
		// later matching; real repo names rarely contain dots.
		for part in raw.split('.') {
			if segment_matches(part, &name) {
				return true;
			}
		}
	}
	false
}

/// Apply hard gate multipliers to a fused score. Order: malware then squat.
/// Factors stack when both flags are set.
#[must_use]
pub fn apply_gate_multiplier(score: f32, flags: GateFlags, cfg: &GateConfig) -> f32 {
	let mut s = score;
	if flags.malware {
		s *= cfg.malware_factor;
	}
	if flags.squat_suspect {
		s *= cfg.squat_factor;
	}
	s
}

/// Whether the exact-name bonus may fire for this candidate under `cfg`.
#[must_use]
pub fn exact_bonus_eligible(quality: f32, squat_suspect: bool, cfg: &GateConfig) -> bool {
	quality >= cfg.exact_quality_floor && !squat_suspect
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn light_normalize(s: &str) -> String {
	let lower = s.to_ascii_lowercase();
	let trimmed = lower.trim_matches(['-', '_']);
	strip_rs_convention(trimmed).to_owned()
}

/// Strip a trailing Rust-style `-rs` / `_rs` / bare `rs` suffix lightly.
fn strip_rs_convention(s: &str) -> &str {
	if let Some(rest) = s.strip_suffix("-rs") {
		return rest.trim_end_matches(['-', '_']);
	}
	if let Some(rest) = s.strip_suffix("_rs") {
		return rest.trim_end_matches(['-', '_']);
	}
	// Bare trailing "rs" only when preceded by a separator-like boundary and
	// the stem is non-empty (avoid turning "rs" itself into "").
	if s.len() > 2 && s.ends_with("rs") {
		let stem = &s[..s.len() - 2];
		if stem.ends_with('-') || stem.ends_with('_') {
			return stem.trim_end_matches(['-', '_']);
		}
	}
	s
}

fn segment_matches(segment: &str, name: &str) -> bool {
	let seg = light_normalize(segment);
	if seg.is_empty() {
		return false;
	}
	// Exact equality after light normalize, or either side contains the other
	// when both are non-trivial (≥ 2 chars) to avoid single-letter false hits.
	if seg == name {
		return true;
	}
	if name.len() >= 2 && seg.contains(name) {
		return true;
	}
	if seg.len() >= 2 && name.contains(&seg) {
		return true;
	}
	false
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
	use super::*;

	// ── verified_repo pure unit tests ─────────────────────────────────────────

	#[test]
	fn verified_repo_serde_github_slug() {
		assert!(
			verified_repo("serde", Some("github.com/serde-rs/serde")),
			"repo last segment must match package name"
		);
	}

	#[test]
	fn verified_repo_host_owner_repo_form() {
		// Facets store `host/owner/repo` lowercase without scheme.
		assert!(verified_repo("tokio", Some("github/tokio-rs/tokio")));
		assert!(verified_repo("serde", Some("github/serde-rs/serde")));
	}

	#[test]
	fn verified_repo_unrelated_is_false() {
		assert!(!verified_repo(
			"yaml",
			Some("github.com/someone/totally-unrelated")
		));
	}

	#[test]
	fn verified_repo_empty_slug_or_name() {
		assert!(!verified_repo("serde", None));
		assert!(!verified_repo("serde", Some("")));
		assert!(!verified_repo("", Some("github.com/serde-rs/serde")));
	}

	#[test]
	fn verified_repo_case_insensitive() {
		assert!(verified_repo("SerDe", Some("GitHub.com/Serde-RS/SerDe")));
	}

	// ── gate multipliers ──────────────────────────────────────────────────────

	#[test]
	fn malware_multiplies_by_factor() {
		let cfg = GateConfig::default();
		let flags = GateFlags {
			malware: true,
			..GateFlags::default()
		};
		let out = apply_gate_multiplier(10.0, flags, &cfg);
		assert!((out - 10.0 * cfg.malware_factor).abs() < 1e-6);
	}

	#[test]
	fn squat_multiplies_by_factor() {
		let cfg = GateConfig::default();
		let flags = GateFlags {
			squat_suspect: true,
			..GateFlags::default()
		};
		let out = apply_gate_multiplier(10.0, flags, &cfg);
		assert!((out - 10.0 * cfg.squat_factor).abs() < 1e-6);
	}

	#[test]
	fn clean_flags_leave_score_unchanged() {
		let cfg = GateConfig::default();
		let out = apply_gate_multiplier(7.5, GateFlags::default(), &cfg);
		assert!((out - 7.5).abs() < 1e-6);
	}

	#[test]
	fn malware_and_squat_stack() {
		let cfg = GateConfig::default();
		let flags = GateFlags {
			malware: true,
			squat_suspect: true,
			..GateFlags::default()
		};
		let out = apply_gate_multiplier(100.0, flags, &cfg);
		let expected = 100.0 * cfg.malware_factor * cfg.squat_factor;
		assert!((out - expected).abs() < 1e-9);
	}

	// ── exact-bonus eligibility ───────────────────────────────────────────────

	#[test]
	fn exact_bonus_requires_quality_floor() {
		let cfg = GateConfig::default();
		assert!(!exact_bonus_eligible(0.10, false, &cfg));
		assert!(exact_bonus_eligible(0.15, false, &cfg));
		assert!(exact_bonus_eligible(0.90, false, &cfg));
	}

	#[test]
	fn exact_bonus_denied_for_squat() {
		let cfg = GateConfig::default();
		assert!(!exact_bonus_eligible(0.99, true, &cfg));
	}

	// ── adversarial: malware buried under clean peer ──────────────────────────

	#[test]
	fn adversarial_malware_huge_bm25_ranks_after_clean() {
		// Pure gate math: after fusion-like scores, malware * 0.001 cannot beat
		// a modest clean peer.
		let cfg = GateConfig::default();
		let malware_fused = 1_000.0_f32; // absurd BM25 win
		let clean_fused = 2.0_f32;
		let malware_final = apply_gate_multiplier(
			malware_fused,
			GateFlags {
				malware: true,
				..GateFlags::default()
			},
			&cfg,
		);
		let clean_final = apply_gate_multiplier(clean_fused, GateFlags::default(), &cfg);
		assert!(
			malware_final < clean_final,
			"malware {malware_final} must rank below clean {clean_final}"
		);
	}
}

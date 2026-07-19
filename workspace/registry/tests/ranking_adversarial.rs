//! Adversarial ranking invariants — hard gates, not soft nDCG averages.
//!
//! Each test encodes one quality invariant from the package-discovery plan
//! using synthetic [`Candidate`]s and the pure [`rank`] / [`rank_full`]
//! pipeline.  Failures mean a ranking change broke an intentional behaviour
//! (exact-name priority, quality kink, withdrawn demotion, fairness floor,
//! dependents vs downloads, cross-eco strip), not merely a fixture drift.
//!
//! Deterministic and network-free: no IO, no RNG, no timestamps.

use heart::ecosystem::Language;
use heart::{Score, Scored};
use registry::search::multi_parent;
use registry::search::ranking::{Candidate, rank, rank_full};
use smol_str::SmolStr;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn cand(
	name: &str,
	bm25: f32,
	quality: f32,
	downloads: Option<u64>,
	eco: Language,
) -> Candidate<&'static str> {
	Candidate {
		item: "payload",
		name: name.to_owned(),
		bm25,
		quality,
		downloads,
		dependents: None,
		popularity_pct: None,
		withdrawn: false,
		squat_suspect: false,
		malware: false,
		verified_repo: false,
		ecosystem: eco,
		keywords: Vec::new(),
	}
}

fn cand_full(
	name: &str,
	bm25: f32,
	quality: f32,
	downloads: Option<u64>,
	dependents: Option<u32>,
	withdrawn: bool,
	eco: Language,
	keywords: &[&str],
) -> Candidate<&'static str> {
	Candidate {
		item: "payload",
		name: name.to_owned(),
		bm25,
		quality,
		downloads,
		dependents,
		popularity_pct: None,
		withdrawn,
		squat_suspect: false,
		malware: false,
		verified_repo: false,
		ecosystem: eco,
		keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
	}
}

fn names<'a>(result: &'a [Candidate<&'static str>]) -> Vec<&'a str> {
	result.iter().map(|c| c.name.as_str()).collect()
}

fn pos(result: &[Candidate<&'static str>], name: &str) -> usize {
	result
		.iter()
		.position(|c| c.name == name)
		.unwrap_or_else(|| panic!("expected {name:?} in ranking, got {:?}", names(result)))
}

// ── 1. Exact-name beats keyword spam ──────────────────────────────────────────

/// Navigate: exact package name must beat a keyword-stuffed higher-BM25 spam
/// crate (Rust crates.io style).
#[test]
fn exact_name_beats_keyword_spam_rust() {
	let candidates = vec![
		cand_full(
			"serde-json-serialize-derive-spam",
			8.0,
			0.12,
			Some(2_000),
			Some(3),
			false,
			Language::Rust,
			&["serde", "json", "serialize", "derive", "encode"],
		),
		cand(
			"serde",
			1.0,
			0.9,
			Some(25_000_000),
			Language::Rust,
		),
		cand(
			"serde_json",
			2.5,
			0.88,
			Some(12_000_000),
			Language::Rust,
		),
	];
	let result = rank("serde", candidates, 10, Some(Language::Rust));
	assert_eq!(
		result[0].name, "serde",
		"exact-name serde must beat keyword spam; got {:?}",
		names(&result)
	);
}

/// Navigate: npm-style exact name must beat keyword spam.
#[test]
fn exact_name_beats_keyword_spam_npm() {
	let candidates = vec![
		cand_full(
			"lodash-array-object-collection-fp-spam",
			9.0,
			0.08,
			Some(500),
			None,
			false,
			Language::Typescript,
			&["lodash", "array", "object", "collection", "fp"],
		),
		cand(
			"lodash",
			1.2,
			0.93,
			Some(50_000_000),
			Language::Typescript,
		),
		cand(
			"lodash-es",
			2.0,
			0.85,
			Some(8_000_000),
			Language::Typescript,
		),
	];
	let result = rank("lodash", candidates, 10, Some(Language::Typescript));
	assert_eq!(
		result[0].name, "lodash",
		"exact-name lodash must beat npm keyword spam; got {:?}",
		names(&result)
	);
}

// ── 2. Quality kink on explore-like multiword queries ─────────────────────────

/// Explore: low quality + high BM25 loses to high quality + moderate BM25.
///
/// This is the quality-kink invariant: `quality > 0.4` multiplies BM25 by
/// `(quality + 1)`, so abandoned keyword spam cannot dominate task queries.
#[test]
fn quality_kink_high_quality_beats_bm25_spam_on_explore() {
	let candidates = vec![
		cand_full(
			"http-client-all-keywords-spam",
			12.0,
			0.05,
			Some(40),
			Some(0),
			false,
			Language::Rust,
			&["http", "client", "request", "async", "tls", "api"],
		),
		cand_full(
			"reqwest",
			2.5,
			0.92,
			Some(15_000_000),
			Some(40_000),
			false,
			Language::Rust,
			&["http", "client", "async"],
		),
		cand_full(
			"hyper",
			2.2,
			0.9,
			Some(20_000_000),
			Some(35_000),
			false,
			Language::Rust,
			&["http", "client", "server"],
		),
	];
	let result = rank("http client", candidates, 10, Some(Language::Rust));
	let spam_pos = pos(&result, "http-client-all-keywords-spam");
	let reqwest_pos = pos(&result, "reqwest");
	assert!(
		reqwest_pos < spam_pos,
		"quality kink: reqwest (pos {reqwest_pos}) must outrank BM25 spam (pos {spam_pos}); got {:?}",
		names(&result)
	);
	assert_eq!(
		result[0].name, "reqwest",
		"high-quality http client should be #1 on explore; got {:?}",
		names(&result)
	);
}

// ── 3. Withdrawn demotion ─────────────────────────────────────────────────────

/// Withdrawn candidates are multiplied by 0.25 and must not sit above an
/// otherwise-equal non-withdrawn peer.
#[test]
fn withdrawn_demotion_cannot_outrank_equal_peer() {
	let candidates = vec![
		cand_full(
			"withdrawn-peer",
			2.0,
			0.7,
			Some(100_000),
			Some(500),
			true,
			Language::Rust,
			&[],
		),
		cand_full(
			"listed-peer",
			2.0,
			0.7,
			Some(100_000),
			Some(500),
			false,
			Language::Rust,
			&[],
		),
	];
	let result = rank("peer", candidates, 10, None);
	assert_eq!(result.len(), 2, "both peers must remain visible");
	assert_eq!(
		result[0].name, "listed-peer",
		"listed peer must outrank withdrawn equal; got {:?}",
		names(&result)
	);
	assert_eq!(result[1].name, "withdrawn-peer");
}

// ── 4. Fairness floor ─────────────────────────────────────────────────────────

/// A candidate with `downloads=None` must not be sorted *below* a peer that
/// only differs by `downloads=Some(floor_value)` solely for missing data.
///
/// Fairness: popularity stages treat `None` as the stage floor, so missing
/// download telemetry is neutral — not a penalty.
#[test]
fn fairness_floor_missing_downloads_not_penalized_vs_floor_value() {
	// bubble_downloads_min default floor = 200.  When both sit at the floor
	// effective popularity, fused-score order (name tiebreak) decides — the
	// None candidate must not be forced below the Some(floor) peer by the
	// downloads stages alone.
	let floor = 200u64;
	let candidates = vec![
		cand("alpha-none", 1.0, 0.6, None, Language::CSharp),
		cand(
			"beta-floor",
			1.0,
			0.6,
			Some(floor),
			Language::Rust,
		),
	];
	let result = rank_full("pkg", candidates, 10, None);
	let none_pos = pos(&result, "alpha-none");
	let floor_pos = pos(&result, "beta-floor");
	// Same BM25/quality → fused scores equal → name asc: alpha-none before beta-floor.
	// The fairness invariant is that None is not *worse* than floor.
	assert!(
		none_pos <= floor_pos,
		"downloads=None must not rank below downloads=Some(floor) solely for missing data; \
		 none_pos={none_pos} floor_pos={floor_pos} order={:?}",
		names(&result)
	);
}

/// Stronger form: None vs a high download count still participates (present
/// in results); the fairness floor only equalizes missing vs floor-level data.
#[test]
fn fairness_floor_none_still_present_alongside_popular() {
	let candidates = vec![
		cand("nuget-pkg", 1.0, 0.6, None, Language::CSharp),
		cand(
			"rust-pkg",
			1.0,
			0.6,
			Some(1_000_000),
			Language::Rust,
		),
		cand(
			"rust-pkg2",
			0.9,
			0.5,
			Some(500_000),
			Language::Rust,
		),
	];
	let result = rank_full("pkg", candidates, 10, None);
	assert!(
		result.iter().any(|c| c.name == "nuget-pkg"),
		"downloads=None candidate must remain in the ranked list"
	);
}

// ── 5. Dependents beat raw downloads when stronger ────────────────────────────

/// `popularity_weight` takes max(dependents_equiv, downloads).  A candidate
/// with strong dependents and weak/absent downloads must outrank a downloads-
/// only peer with lower effective popularity when other signals match.
#[test]
fn dependents_beat_raw_downloads_when_stronger() {
	// dependents=100 → 100 * 2500 = 250_000 effective.
	// Peer with downloads=10_000 only → lower popularity.
	// Equal BM25/quality so fusion ties; pull-up / mixed sort use popularity.
	// With only 2 candidates pull-up is inactive — use popularity_weight directly
	// and a larger set so pull-up can fire, OR assert on the accessor + rank order
	// when fused scores already differ slightly in favor of the dependents candidate.
	let strong_deps = cand_full(
		"dep-strong",
		1.0,
		0.7,
		Some(5_000),
		Some(100), // ≡ 250_000
		false,
		Language::Rust,
		&[],
	);
	let dl_only = cand_full(
		"dl-only",
		1.0,
		0.7,
		Some(10_000),
		None,
		false,
		Language::Rust,
		&[],
	);

	// Unit invariant on the weight itself (hard, not soft).
	assert!(
		strong_deps.popularity_weight(200) > dl_only.popularity_weight(200),
		"dependents=100 must yield higher popularity_weight than downloads=10k alone"
	);

	// Build a pull-up-eligible set (≥7) so popularity-driven stages can act.
	let mut candidates: Vec<Candidate<&'static str>> = (0..12)
		.map(|i| {
			cand(
				&format!("filler-{i:02}"),
				0.5 - (i as f32 * 0.01),
				0.35,
				Some(300),
				Language::Rust,
			)
		})
		.collect();
	candidates.push(strong_deps);
	candidates.push(dl_only);

	let result = rank("crate", candidates, 20, Some(Language::Rust));
	let dep_pos = pos(&result, "dep-strong");
	let dl_pos = pos(&result, "dl-only");
	assert!(
		dep_pos < dl_pos,
		"strong dependents must rank above downloads-only peer; dep={dep_pos} dl={dl_pos}; {:?}",
		names(&result)
	);
}

// ── 6. Cross-eco strip (Rust cargo- must not apply to npm) ────────────────────

/// Ranking path: a TypeScript candidate must use npm strip conventions, so a
/// Rust-style `cargo-` query strip that would connect under Rust must NOT
/// promote an npm package via a false strip path.
///
/// Documents the ecosystem field on [`Candidate`]: contains-bonus is per
/// candidate ecosystem, never a hard-coded Rust strip for all languages.
#[test]
fn cross_eco_rust_strip_does_not_wrongly_apply_to_npm_via_rank() {
	// Under Rust strip: query "cargo-axio" → "axio", name "axios" contains "axio".
	// Under npm strip: identity — neither name contains the other as full strip-eq.
	// We rank both candidates with the *same* query and require that the npm
	// candidate is not spuriously exact-boosted above a true npm exact match.
	let candidates = vec![
		cand("axios", 1.0, 0.5, Some(10_000), Language::Typescript),
		cand(
			"cargo-axio-lookalike",
			1.0,
			0.5,
			Some(10_000),
			Language::Typescript,
		),
		// A true exact match on npm for a different query is not the point —
		// we assert relative ordering under the cargo-axio query for TS ecoscope.
	];
	let result = rank(
		"cargo-axio",
		candidates,
		10,
		Some(Language::Typescript),
	);
	// With identity strip, neither name equals "cargo-axio" and containment:
	// "axios" vs "cargo-axio" — neither contains the other.
	// "cargo-axio-lookalike" contains "cargo-axio" as substring → contains bonus.
	// So lookalike may rank first (legit substring), but axios must NOT receive
	// a Rust-only strip bonus that would invent a match.
	// Hard invariant: axios must not jump above lookalike solely via Rust strip.
	let axios_pos = pos(&result, "axios");
	let lookalike_pos = pos(&result, "cargo-axio-lookalike");
	assert!(
		lookalike_pos <= axios_pos,
		"npm path: cargo-axio-lookalike (substring) should rank ≥ axios \
		 (Rust strip must not invent axios match); order={:?}",
		names(&result)
	);
}

/// Positive control: under Rust ecosystem scope + Rust candidate, cargo- prefix
/// strip on the query connects `cargo-serde` → `serde`.
#[test]
fn cross_eco_rust_strip_does_apply_for_rust_candidates() {
	// Keep BM25 close so the contains-bonus from strip is the decisive edge
	// (not quality kink alone). Query `cargo-serde` strips to `serde`.
	let candidates = vec![
		cand("serde", 2.0, 0.9, Some(25_000_000), Language::Rust),
		cand(
			"unrelated-crate",
			2.1,
			0.55,
			Some(100_000),
			Language::Rust,
		),
	];
	let result = rank(
		"cargo-serde",
		candidates,
		10,
		Some(Language::Rust),
	);
	// serde should get contains-bonus via two-sided strip and rank first.
	assert_eq!(
		result[0].name, "serde",
		"Rust strip should connect cargo-serde → serde; got {:?}",
		names(&result)
	);
}

// ── 7. multi_parent::merge (optional entity collapse) ─────────────────────────

/// Multi-parent merge collapses same (name, version) across origins and keeps
/// the highest score.  Hard identity invariant for entity ranking.
#[test]
fn multi_parent_merge_keeps_highest_score() {
	// Lightweight construction mirroring registry_publish fixtures without
	// needing the full common module helper set beyond GlobalPackage paths.
	use heart::{Edition, PackageVersion, RegistryOrigin, ResolutionState, Toolchain};
	use registry::package::{Coordinates, PackageName};
	use registry::{GlobalPackage, Package};

	fn rust_pkg(name: &str, version: &str, origin: RegistryOrigin) -> GlobalPackage {
		let package = Package {
			coordinates: Coordinates {
				origin,
				name: PackageName::new(Language::Rust, name).expect("valid name"),
				version: PackageVersion::try_from((Language::Rust, version))
					.expect("valid version"),
			},
			toolchain: Toolchain::Rust {
				compiler: semver::Version::new(1, 85, 0),
				edition: Edition::E2024,
			},
		};
		GlobalPackage {
			id: package.id(),
			package,
			state: ResolutionState::Unindexed { needed: false },
			facets: None,
		}
	}

	fn score(v: f32) -> Score {
		Score::try_new(v).expect("finite")
	}

	let crates = rust_pkg("serde", "1.0.0", RegistryOrigin::CratesIo);
	let mirror = rust_pkg(
		"serde",
		"1.0.0",
		RegistryOrigin::Custom {
			name: "mirror.example".into(),
			url: url::Url::parse("https://mirror.example").expect("url"),
		},
	);
	assert_ne!(crates.id, mirror.id, "distinct origins → distinct package ids");

	let merged = multi_parent::merge(vec![
		Scored::new(crates.clone(), score(0.4)),
		Scored::new(mirror.clone(), score(0.9)),
	]);
	assert_eq!(merged.len(), 1, "same name+version must collapse to one entity");
	assert_eq!(
		merged[0].representative.value.id, mirror.id,
		"higher score must win the multi_parent representative"
	);

	let merged_flip = multi_parent::merge(vec![
		Scored::new(crates.clone(), score(0.95)),
		Scored::new(mirror.clone(), score(0.1)),
	]);
	assert_eq!(
		merged_flip[0].representative.value.id, crates.id,
		"score inversion must change the representative"
	);
}

// ── 8. Spam land-grab judgment discipline (ranking-winnable pool) ─────────────

/// Package named with keyword spam (high BM25, quality 0.05) must lose to the
/// real package on an explore query.  The land-grab must not be treatable as
/// gain-3 in fixtures (enforced separately in ranking_ndcg lint); here we
/// assert the ranking order itself is winnable.
#[test]
fn spam_landgrab_loses_to_real_package_on_explore() {
	let candidates = vec![
		cand_full(
			"yaml-parser",
			10.0,
			0.05,
			Some(20),
			None,
			false,
			Language::Rust,
			&["yaml", "parser", "config", "serde"],
		),
		cand_full(
			"serde_yaml",
			2.6,
			0.88,
			Some(8_000_000),
			Some(12_000),
			false,
			Language::Rust,
			&["yaml", "serde"],
		),
	];
	let result = rank("yaml parser", candidates, 10, Some(Language::Rust));
	assert_eq!(
		result[0].name, "serde_yaml",
		"real yaml package must beat land-grab spam; got {:?}",
		names(&result)
	);
}

// ── 9. Hard spam / squat / malware gates ──────────────────────────────────────

/// Malware with absurd BM25 is multiplied by ~0.001 and must rank after a clean peer.
#[test]
fn malware_huge_bm25_ranks_after_clean_peer() {
	let candidates = vec![
		Candidate {
			malware: true,
			..cand("evil-pkg", 1_000.0, 0.9, Some(50_000_000), Language::Rust)
		},
		cand("clean-pkg", 1.5, 0.6, Some(10_000), Language::Rust),
	];
	let result = rank("async", candidates, 10, None);
	assert_eq!(result.len(), 2, "malware stays visible (buried, not dropped)");
	assert_eq!(
		result[0].name, "clean-pkg",
		"clean peer must outrank malware despite huge BM25; got {:?}",
		names(&result)
	);
	assert_eq!(result[1].name, "evil-pkg");
}

/// Squat_suspect exact-name land-grab does NOT get the full exact bonus and is
/// further multiplied by the squat factor — so the real package wins.
///
/// Previously unwinnable: exact_name_bonus (+10) dominated quality kink. The
/// gate path (skip exact bonus when squat_suspect || quality < floor, then
/// × squat_factor) makes legitimate packages rank above land-grabs.
#[test]
fn squat_exact_name_landgrab_does_not_get_full_exact_bonus() {
	let candidates = vec![
		Candidate {
			squat_suspect: true,
			..cand("yaml", 5.0, 0.05, Some(10), Language::Rust)
		},
		cand("yaml-rust", 2.0, 0.85, Some(500_000), Language::Rust),
	];
	let result = rank("yaml", candidates, 10, Some(Language::Rust));
	assert_eq!(
		result[0].name, "yaml-rust",
		"real package must beat squat exact-name land-grab; got {:?}",
		names(&result)
	);
}

/// Pure unit coverage for verified_repo lives in `search::gates` tests; this
/// documents that a clean exact match with quality above the floor still wins
/// (gates must not demote legitimate packages).
#[test]
fn clean_exact_match_still_gets_exact_bonus() {
	let candidates = vec![
		cand("serde", 1.0, 0.9, Some(50_000_000), Language::Rust),
		cand("serde-with-spam-keywords", 8.0, 0.2, Some(1_000), Language::Rust),
	];
	let result = rank("serde", candidates, 10, Some(Language::Rust));
	assert_eq!(
		result[0].name, "serde",
		"clean exact match above quality floor must keep exact bonus; got {:?}",
		names(&result)
	);
}

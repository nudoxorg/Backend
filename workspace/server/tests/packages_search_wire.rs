//! Track A — package search wire path: ecosystem scope + synonym expansion.
//!
//! Guards the production wiring that threads API `ecosystems` and process
//! heuristics (`Synonyms`) into `PackageSearchRequest` / `collect_ranked_hits`
//! rather than leaving `ecosystem: None` and empty `expanded_terms` hard-coded.

use std::io::Write as _;
use std::path::PathBuf;

use heart::{
	Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain,
};
use registry::{
	GlobalPackage, Package,
	metadata::{SearchFacets, Synonyms},
	package::{Coordinates, PackageName},
	search::{PackageSearchRequest, StructuredQuery, search_page, tantivy::PackageIndex},
};
use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
	fn new(prefix: &str) -> Self {
		let path = std::env::temp_dir()
			.join(format!("packages-search-wire-{prefix}-{}", uuid::Uuid::new_v4()));
		std::fs::create_dir_all(&path).unwrap();
		Self(path)
	}
	fn path(&self) -> &std::path::Path { &self.0 }
}

impl Drop for TempDir {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

fn rust_package(name: &str, quality_ppm: u32, keywords: &[&str]) -> GlobalPackage {
	let coordinates = Coordinates {
		origin: RegistryOrigin::CratesIo,
		name: PackageName::new(Language::Rust, name).expect("fixture name"),
		version: PackageVersion::try_from((Language::Rust, "1.0.0")).expect("fixture version"),
	};
	let package = Package {
		coordinates,
		toolchain: Toolchain::Rust {
			compiler: semver::Version::new(1, 85, 0),
			edition: Edition::E2024,
		},
	};
	let id = package.id();
	GlobalPackage {
		id,
		package,
		state: ResolutionState::Unindexed { needed: false },
		facets: Some(SearchFacets {
			keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
			quality_ppm,
			..Default::default()
		}),
	}
}

fn go_package(name: &str, quality_ppm: u32, keywords: &[&str]) -> GlobalPackage {
	// Go module paths are the package name; keep it simple for fixtures.
	let coordinates = Coordinates {
		origin: RegistryOrigin::GoProxy,
		name: PackageName::new(Language::Go, name).expect("fixture go name"),
		version: PackageVersion::try_from((Language::Go, "v1.0.0")).expect("fixture go version"),
	};
	let package = Package {
		coordinates,
		toolchain: Toolchain::Go {
			compiler: semver::Version::new(1, 22, 0),
		},
	};
	let id = package.id();
	GlobalPackage {
		id,
		package,
		state: ResolutionState::Unindexed { needed: false },
		facets: Some(SearchFacets {
			keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
			quality_ppm,
			..Default::default()
		}),
	}
}

fn build_index(dir: &TempDir, records: &[GlobalPackage]) -> PackageIndex {
	let mut index = PackageIndex::open(dir.path()).expect("tempdir index opens");
	index.absorb(records.iter(), 1).expect("records absorb");
	index
}

/// Same selection rule as `search_packages`: empty → None; else first element.
fn package_search_ecosystem(ecosystems: &[Language]) -> Option<Language> {
	// v1 multi-eco policy: first listed only (see search_packages comment).
	ecosystems.first().copied()
}

fn make_synonyms(csv: &str) -> Synonyms {
	let dir = tempfile::tempdir().expect("synonyms tempdir");
	let path = dir.path().join("tag-synonyms.csv");
	{
		let mut f = std::fs::File::create(&path).unwrap();
		f.write_all(csv.as_bytes()).unwrap();
	}
	// Synonyms::new keeps no open handle on the dir; load before drop.
	let synonyms = Synonyms::new(dir.path()).expect("Synonyms::new");
	// Keep files alive until after load — already loaded, OK to drop dir.
	// (Synonyms owns its HashMap; path is only needed at construction.)
	let _ = dir;
	synonyms
}

// ---------------------------------------------------------------------------
// Ecosystem scope wiring
// ---------------------------------------------------------------------------

/// PackageSearchRequest.ecosystem Some(Go) must surface as StructuredQuery.ecosystem Some(Go)
/// after parse — the same parse call `collect_ranked_hits` uses.
#[test]
fn registry_query_ecosystem_go_produces_structured_scope() {
	let query = PackageSearchRequest {
		text: "mux".to_owned(),
		ecosystem: Some(Language::Go),
		limit: 10,
		after: None,
		semantic: Vec::new(),
	};
	let sq = StructuredQuery::parse(&query.text, query.ecosystem);
	assert_eq!(sq.ecosystem, Some(Language::Go));
	assert_eq!(sq.terms, "mux");
}

/// Inline `lang:go` token also scopes when the API leaves ecosystem unset.
#[test]
fn lang_go_token_scopes_structured_query() {
	let sq = StructuredQuery::parse("lang:go mux", None);
	assert_eq!(sq.ecosystem, Some(Language::Go));
	assert_eq!(sq.terms, "mux");
}

/// Empty ecosystems vec must not invent a scope (unscoped production path).
#[test]
fn empty_ecosystems_vec_does_not_force_ecosystem() {
	let ecosystems: Vec<Language> = Vec::new();
	assert_eq!(package_search_ecosystem(&ecosystems), None);

	// Parse path with None must stay unscoped even if free text looks language-y.
	let sq = StructuredQuery::parse("mux", None);
	assert_eq!(sq.ecosystem, None, "unscoped free text must not invent an ecosystem");
}

/// Exactly one ecosystem scopes; multi takes first (v1).
#[test]
fn ecosystems_selection_single_and_multi() {
	assert_eq!(
		package_search_ecosystem(&[Language::Go]),
		Some(Language::Go)
	);
	assert_eq!(
		package_search_ecosystem(&[Language::Go, Language::Rust]),
		Some(Language::Go),
		"v1 multi-eco takes the first listed ecosystem"
	);
	assert_eq!(
		package_search_ecosystem(&[Language::Rust, Language::Python]),
		Some(Language::Rust)
	);
}

/// Integration: ecosystem=Go scopes hits away from a same-named Rust package.
#[tokio::test]
async fn ecosystem_go_scopes_search_results() {
	let dir = TempDir::new("eco-scope");
	let go_mux = go_package("github.com/gorilla/mux", 700_000, &["mux", "router"]);
	let rust_mux = rust_package("mux", 800_000, &["mux", "async"]);
	let index = build_index(&dir, &[go_mux.clone(), rust_mux.clone()]);

	// Unscoped: both worlds match a bare "mux" (or at least more than one).
	let open = search_page(
		&index,
		&PackageSearchRequest {
			text: "mux".to_owned(),
			ecosystem: None,
			limit: 10,
			after: None,
			semantic: Vec::new(),
		},
		None,
	)
	.await
	.expect("unscoped query executes");
	assert!(
		open.items.len() >= 2,
		"unscoped search must see both ecosystems; got {}",
		open.items.len()
	);

	// API-scoped to Go: only the Go package.
	let scoped = search_page(
		&index,
		&PackageSearchRequest {
			text: "mux".to_owned(),
			ecosystem: Some(Language::Go),
			limit: 10,
			after: None,
			semantic: Vec::new(),
		},
		None,
	)
	.await
	.expect("scoped query executes");
	assert_eq!(scoped.items.len(), 1, "Go scope must filter, not merely rank");
	assert_eq!(scoped.items[0].value.id, go_mux.id);
	assert_eq!(
		scoped.items[0].value.package.coordinates.ecosystem(),
		Language::Go
	);

	// Inline lang: token scopes equivalently when API ecosystem is None.
	let lang_token = search_page(
		&index,
		&PackageSearchRequest {
			text: "lang:go mux".to_owned(),
			ecosystem: None,
			limit: 10,
			after: None,
			semantic: Vec::new(),
		},
		None,
	)
	.await
	.expect("lang:go query executes");
	assert_eq!(lang_token.items.len(), 1);
	assert_eq!(lang_token.items[0].value.id, go_mux.id);
}

// ---------------------------------------------------------------------------
// Synonym expansion wiring
// ---------------------------------------------------------------------------

/// When synonyms are loaded, expand_synonyms fills expanded_terms (unit path).
#[test]
fn synonym_term_expands_when_table_loaded() {
	let synonyms = make_synonyms("http-client,reqwest,4\n");
	let mut sq = StructuredQuery::parse("http-client", None);
	assert!(sq.expanded_terms.is_empty(), "precondition: empty until expand");
	sq.expand_synonyms(&synonyms);
	assert!(
		sq.expanded_terms.contains(&"reqwest".to_owned()),
		"expanded_terms must contain 'reqwest'; got {:?}",
		sq.expanded_terms
	);
}

/// Absent synonyms leave expanded_terms empty (production skip path).
#[test]
fn absent_synonyms_leave_expanded_terms_empty() {
	let sq = StructuredQuery::parse("http-client", None);
	assert!(sq.expanded_terms.is_empty());
}

/// Integration: search_page with synonyms recalls a doc via EXPANDED tier.
#[tokio::test]
async fn search_page_with_synonyms_expands_and_recalls() {
	let dir = TempDir::new("synonym-wire");
	let reqwest_pkg = rust_package("reqwest", 600_000, &["reqwest", "http", "client"]);
	let unrelated = rust_package("rayon", 500_000, &["parallel", "rayon"]);
	let index = build_index(&dir, &[reqwest_pkg.clone(), unrelated]);

	let synonyms = make_synonyms("http-client,reqwest,4\n");

	// Without synonyms the free term may miss the package (no name/keyword hit).
	// With synonyms, EXPANDED tier must surface reqwest.
	let page = search_page(
		&index,
		&PackageSearchRequest {
			text: "http-client".to_owned(),
			ecosystem: None,
			limit: 10,
			after: None,
			semantic: Vec::new(),
		},
		Some(&synonyms),
	)
	.await
	.expect("synonym-expanded query executes");

	let hit_ids: Vec<_> = page.items.iter().map(|s| s.value.id).collect();
	assert!(
		hit_ids.contains(&reqwest_pkg.id),
		"reqwest must be recalled via expanded synonym; hits: {:?}",
		hit_ids
	);
}

// ---------------------------------------------------------------------------
// Adversarial: unscoped + multi-parent + pagination still work
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unscoped_search_still_works() {
	let dir = TempDir::new("unscoped");
	let serde = rust_package("serde", 900_000, &["serialization"]);
	let index = build_index(&dir, &[serde.clone()]);

	let page = search_page(
		&index,
		&PackageSearchRequest {
			text: "serde".to_owned(),
			ecosystem: None,
			limit: 10,
			after: None,
			semantic: Vec::new(),
		},
		None,
	)
	.await
	.expect("unscoped query executes");

	assert_eq!(page.items.len(), 1);
	assert_eq!(page.items[0].value.id, serde.id);
}

#[tokio::test]
async fn multi_parent_dedup_and_pagination_smoke() {
	let dir = TempDir::new("pagination");

	// Multi-parent pair: same logical name/version, distinct origins → one hit.
	let primary = rust_package("serde", 800_000, &["serialization"]);
	let mirror_coords = Coordinates {
		origin: RegistryOrigin::Custom {
			name: "mirror.example".into(),
			url: url::Url::parse("https://mirror.example").expect("fixture url"),
		},
		name: PackageName::new(Language::Rust, "serde").expect("fixture name"),
		version: PackageVersion::try_from((Language::Rust, "1.0.0")).expect("fixture version"),
	};
	let mirror_pkg = Package {
		coordinates: mirror_coords,
		toolchain: Toolchain::Rust {
			compiler: semver::Version::new(1, 85, 0),
			edition: Edition::E2024,
		},
	};
	let mirror = GlobalPackage {
		id: mirror_pkg.id(),
		package: mirror_pkg,
		state: ResolutionState::Unindexed { needed: false },
		facets: primary.facets.clone(),
	};
	assert_ne!(primary.id, mirror.id);

	// Plus a corpus large enough to page.
	let mut records = vec![primary, mirror];
	for i in 0..15_u32 {
		records.push(rust_package(
			&format!("crate-{i:02}"),
			i * 40_000,
			&["async", "crate"],
		));
	}
	let index = build_index(&dir, &records);

	// Multi-parent collapses to one for "serde".
	let serde_page = search_page(
		&index,
		&PackageSearchRequest {
			text: "serde".to_owned(),
			ecosystem: None,
			limit: 10,
			after: None,
			semantic: Vec::new(),
		},
		None,
	)
	.await
	.expect("serde query");
	assert_eq!(
		serde_page.items.len(),
		1,
		"multi-parent must collapse to one result"
	);

	// Pagination: page 1 then resume with next cursor.
	let page1 = search_page(
		&index,
		&PackageSearchRequest {
			text: "crate".to_owned(),
			ecosystem: None,
			limit: 5,
			after: None,
			semantic: Vec::new(),
		},
		None,
	)
	.await
	.expect("page 1");
	assert_eq!(page1.items.len(), 5);
	assert!(page1.next.is_some(), "more pages expected");

	use registry::search::SearchKey;
	let cursor = heart::Cursor::<SearchKey>::decode(page1.next.as_ref().unwrap())
		.expect("cursor decodes");
	let page2 = search_page(
		&index,
		&PackageSearchRequest {
			text: "crate".to_owned(),
			ecosystem: None,
			limit: 5,
			after: Some(cursor),
			semantic: Vec::new(),
		},
		None,
	)
	.await
	.expect("page 2");
	assert!(!page2.items.is_empty(), "page 2 must return remaining hits");

	// No overlap between page 1 and page 2.
	let page1_ids: std::collections::HashSet<_> =
		page1.items.iter().map(|s| s.value.id).collect();
	for hit in &page2.items {
		assert!(
			!page1_ids.contains(&hit.value.id),
			"pagination must not duplicate hits across pages"
		);
	}
}

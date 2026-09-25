#![cfg(feature = "server")]
//! ID-8 ranking scenarios exercised through the package search wire path.
//!
//! Builds a tantivy [`PackageIndex`], hydrates [`GlobalPackage`] fixtures with
//! explicit [`SearchFacets::dependents`], and asserts ordering from
//! [`search_page`] — the same harness shape as `packages_search_wire.rs`.

#[allow(unused_imports)]
use index::server::registry;
use std::path::PathBuf;

use heart::{Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain};
use index::ecosystem::PackageNameExt as _;
use registry::{
    GlobalPackage, Package,
    metadata::SearchFacets,
    package::{Coordinates, PackageName},
    search::{PackageSearchRequest, search_page, tantivy::PackageIndex},
};
use smol_str::SmolStr;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(prefix: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "lib-rs-rank-scenarios-{prefix}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rust_package(
    name: &str,
    quality_ppm: u32,
    keywords: &[&str],
    downloads: Option<u64>,
    dependents: Option<u32>,
) -> GlobalPackage {
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
            downloads,
            dependents,
            ..Default::default()
        }),
    }
}

fn build_index(dir: &TempDir, records: &[GlobalPackage]) -> PackageIndex {
    let mut index = PackageIndex::open(dir.path()).expect("tempdir index opens");
    index.absorb(records.iter(), 1).expect("records absorb");
    index
}

async fn ranked_names(index: &PackageIndex, query: &str) -> Vec<String> {
    let page = search_page(
        index,
        &PackageSearchRequest {
            text: query.to_owned(),
            ecosystem: Some(Language::Rust),
            limit: 10,
            after: None,
            semantic: Vec::new(),
        },
        None,
    )
    .await
    .expect("search_page executes");

    page.items
        .iter()
        .map(|hit| hit.value.package.coordinates.name.canonical().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Scenario 1 — dependents dominate when text/quality/downloads tie
// ---------------------------------------------------------------------------

/// Three keyword-matched crates with identical quality and downloads must sort
/// by dependents descending (1000, 100, 10) under ID-8's dependents-heavy fuse.
#[tokio::test]
async fn http_keyword_matches_rank_by_dependents_descending() {
    let dir = TempDir::new("dependents-order");
    let tiny = rust_package(
        "alpha-net",
        500_000,
        &["http"],
        Some(10_000),
        Some(10),
    );
    let mega = rust_package(
        "beta-net",
        500_000,
        &["http"],
        Some(10_000),
        Some(1_000),
    );
    let mid = rust_package(
        "gamma-net",
        500_000,
        &["http"],
        Some(10_000),
        Some(100),
    );
    let index = build_index(&dir, &[tiny.clone(), mega.clone(), mid.clone()]);

    let names = ranked_names(&index, "http").await;
    assert_eq!(
        names,
        vec![
            "beta-net".to_owned(),
            "gamma-net".to_owned(),
            "alpha-net".to_owned(),
        ],
        "expected dependents order 1000 → 100 → 10"
    );
}

// ---------------------------------------------------------------------------
// Scenario 2 — max(dependents * 2500, downloads) popularity unit
// ---------------------------------------------------------------------------

/// Popularity unit is `max(dependents * DEPENDENT_DOWNLOAD_EQUIV, downloads) /
/// POPULARITY_SATURATION` (see `search/factors/formula.rs::synthesized_popularity`).
///
/// Both masses stay under `POPULARITY_SATURATION` (10_000_000). A saturated
/// pair would clamp to the same unit and the order would be a tiebreak.
/// `dependents=2000` → mass `5_000_000`. Downloads one below that mass must
/// lose when the text match ties.
#[tokio::test]
async fn dependents_equiv_beats_huge_downloads_when_text_ties() {
    const DEP_EQUIV: u64 = 2_500;
    const SATURATION: u64 = 10_000_000;
    const DEPENDENTS: u32 = 2_000;

    let dependents_mass = u64::from(DEPENDENTS) * DEP_EQUIV;
    assert!(
        dependents_mass < SATURATION,
        "fixture must stay below the popularity clamp"
    );
    let huge_downloads = dependents_mass - 1;
    assert!(huge_downloads < dependents_mass);

    let dir = TempDir::new("dependents-vs-downloads");
    let dep_heavy = rust_package(
        "dep-heavy",
        500_000,
        &["network"],
        Some(0),
        Some(DEPENDENTS),
    );
    let dl_heavy = rust_package(
        "dl-heavy",
        500_000,
        &["network"],
        Some(huge_downloads),
        Some(0),
    );
    let index = build_index(&dir, &[dl_heavy.clone(), dep_heavy.clone()]);

    let names = ranked_names(&index, "network").await;
    assert_eq!(
        names,
        vec!["dep-heavy".to_owned(), "dl-heavy".to_owned()],
        "max(dependents*2500, downloads) must rank dep-heavy above dl-heavy"
    );
}

// ---------------------------------------------------------------------------
// Scenario 3 — exact name bonus vs keyword-only recall
// ---------------------------------------------------------------------------

/// When dependents and quality tie, a crate whose name equals the query gets
/// the cascade `exact_name_bonus` (default 10.0 in `RankingConfig`) and must
/// outrank a crate that only shares the query as a keyword slug.
#[tokio::test]
async fn exact_name_outranks_keyword_only_match() {
    let dir = TempDir::new("exact-vs-keyword");
    let named = rust_package("widget", 500_000, &["ui"], Some(1_000), Some(50));
    let keyword_only = rust_package(
        "noise-crate",
        500_000,
        &["widget"],
        Some(1_000),
        Some(50),
    );
    let index = build_index(&dir, &[keyword_only.clone(), named.clone()]);

    let names = ranked_names(&index, "widget").await;
    assert_eq!(
        names,
        vec!["widget".to_owned(), "noise-crate".to_owned()],
        "exact name match must beat keyword-only recall when popularity ties"
    );
}

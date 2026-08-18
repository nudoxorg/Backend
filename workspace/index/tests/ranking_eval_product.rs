//! Golden ranking judgment exercised through the product package-search path.

use heart::{Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain};
use index::{
    GlobalPackage, Package,
    ecosystem::PackageNameExt as _,
    metadata::SearchFacets,
    package::{Coordinates, PackageName},
    search::{
        PackageSearchDeps, PackageSearchRequest,
        eval::{GoldenQuery, ndcg_at_k},
        pipeline::retrieve_and_rank,
        tantivy::PackageIndex,
    },
};
use smol_str::SmolStr;

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
            keywords: keywords
                .iter()
                .map(|&keyword| SmolStr::new(keyword))
                .collect(),
            quality_ppm,
            ..Default::default()
        }),
    }
}

#[tokio::test]
async fn golden_serde_query_scores_product_ranking() {
    let fixture: GoldenQuery =
        serde_json::from_str(include_str!("ranking_serde.json")).expect("golden fixture");
    let tempdir = tempfile::tempdir().expect("tempdir");
    let mut index = PackageIndex::open(tempdir.path()).expect("open package index");
    let records: Vec<GlobalPackage> = fixture
        .pool
        .iter()
        .map(|entry| {
            rust_package(
                &entry.name,
                (entry.quality.clamp(0.0, 1.0) * 1_000_000.0) as u32,
                &entry
                    .keywords
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    index.absorb(records.iter(), 1).expect("absorb fixture");

    let request = PackageSearchRequest {
        text: fixture.query.clone(),
        ecosystem: Some(Language::Rust),
        limit: 4,
        after: None,
        semantic: Vec::new(),
    };
    let ranked = retrieve_and_rank(&index, &request, PackageSearchDeps::default())
        .await
        .expect("product ranking path");
    let names: Vec<&str> = ranked
        .iter()
        .map(|hit| hit.value.package.coordinates.name.canonical())
        .collect();
    let score = ndcg_at_k(&names, &fixture.judgments, 3);
    assert!(
        score > 0.999,
        "golden serde ranking regressed: nDCG@3={score}, names={names:?}"
    );
}

#[tokio::test]
async fn product_ranking_is_insertion_order_independent() {
    let fixture: GoldenQuery =
        serde_json::from_str(include_str!("ranking_serde.json")).expect("golden fixture");
    let first_dir = tempfile::tempdir().expect("first tempdir");
    let second_dir = tempfile::tempdir().expect("second tempdir");
    let mut first_index = PackageIndex::open(first_dir.path()).expect("open first package index");
    let mut second_index =
        PackageIndex::open(second_dir.path()).expect("open second package index");
    let records: Vec<GlobalPackage> = fixture
        .pool
        .iter()
        .map(|entry| {
            rust_package(
                &entry.name,
                (entry.quality.clamp(0.0, 1.0) * 1_000_000.0) as u32,
                &entry
                    .keywords
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    first_index
        .absorb(records.iter(), 1)
        .expect("absorb fixture");
    second_index
        .absorb(records.iter().rev(), 1)
        .expect("absorb reversed fixture");

    let request = PackageSearchRequest {
        text: fixture.query,
        ecosystem: Some(Language::Rust),
        limit: 4,
        after: None,
        semantic: Vec::new(),
    };
    let first = retrieve_and_rank(&first_index, &request, PackageSearchDeps::default())
        .await
        .expect("forward product ranking");
    let second = retrieve_and_rank(&second_index, &request, PackageSearchDeps::default())
        .await
        .expect("reversed product ranking");
    let first_names: Vec<_> = first
        .iter()
        .map(|hit| hit.value.package.coordinates.name.canonical().to_string())
        .collect();
    let second_names: Vec<_> = second
        .iter()
        .map(|hit| hit.value.package.coordinates.name.canonical().to_string())
        .collect();
    assert_eq!(
        first_names, second_names,
        "identical product queries must produce stable ordering"
    );
}

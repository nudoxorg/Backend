//! End-to-end: a typed query plan, compiled into tantivy, returns the same
//! packages a second index returns, and FAST columns match the facet formula.

use heart::{Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain};
use index::{
    GlobalPackage, Package,
    ecosystem::PackageNameExt as _,
    metadata::SearchFacets,
    package::{Coordinates, PackageName},
    search::{structured::StructuredQuery, tantivy::PackageIndex},
};
use smol_str::SmolStr;

fn rust_package(
    name: &str,
    dependencies: &[&str],
    quality_ppm: u32,
    downloads: u64,
    popularity_pct: u16,
    license: &str,
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
            quality_ppm,
            downloads: Some(downloads),
            dependencies: dependencies.iter().map(|dep| SmolStr::new(*dep)).collect(),
            license: Some(SmolStr::new(license)),
            popularity_pct: Some(popularity_pct),
            description: Some(SmolStr::new("a serialization framework")),
            ..Default::default()
        }),
    }
}

fn names_for(index: &PackageIndex, text: &str) -> Vec<String> {
    let structured = StructuredQuery::parse(text, Some(Language::Rust));
    let hits = index
        .query_structured(&structured, 16)
        .expect("query executes");
    hits.into_iter()
        .map(|(package, _)| package.package.coordinates.name.canonical().to_string())
        .collect()
}

#[test]
fn planned_query_matches_a_second_index_and_the_fast_columns() {
    let forward = vec![
        rust_package("serde", &["tokio"], 800_000, 50_000, 9_000, "mit"),
        rust_package("rand", &["getrandom"], 100_000, 10, 100, "apache-2.0"),
    ];
    let mut reversed = forward.clone();
    reversed.reverse();

    let first_dir = tempfile::tempdir().expect("first dir");
    let second_dir = tempfile::tempdir().expect("second dir");
    let mut first = PackageIndex::open(first_dir.path()).expect("open first");
    let mut second = PackageIndex::open(second_dir.path()).expect("open second");
    first.absorb(forward.iter(), 1).expect("absorb forward");
    second.absorb(reversed.iter(), 1).expect("absorb reversed");

    for query in [
        "serde",
        "dep:tokio",
        "license:mit",
        "\"serialization framework\"",
    ] {
        assert_eq!(
            names_for(&first, query),
            names_for(&second, query),
            "{query}"
        );
    }

    let serde_hits = names_for(&first, "dep:tokio");
    assert_eq!(serde_hits, vec!["serde".to_string()]);
    let license_hits = names_for(&first, "license:apache-2.0");
    assert_eq!(license_hits, vec!["rand".to_string()]);

    let serde = &forward[0];
    let fast = first
        .fast_ranking_signals(serde.id)
        .expect("fast read")
        .expect("serde is indexed");
    assert_eq!(fast.quality_ppm, 800_000);
    assert_eq!(fast.downloads, 50_000);
    assert_eq!(fast.popularity_pct_ppm, 900_000);

    let missing = first
        .fast_ranking_signals(heart::PackageId::from_uuid(uuid::Uuid::nil()))
        .expect("missing read");
    assert!(missing.is_none());
}

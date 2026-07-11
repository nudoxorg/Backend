//! Diagram: **postgres also handles search for the registry**, and **tantivy is
//! the abstraction over the information we get from postgres for registry
//! search, isolating the multi-parent setup.**
//!
//! TDD specs for `registry::search`. The replica-local tantivy index runs in
//! tempdirs offline; the postgres-sync half is gated on
//! `REGISTRY_TEST_POSTGRES`/`DATABASE_URL`.

mod common;

use heart::{Edition, Language, PackageVersion, RegistryOrigin, ResolutionState, Toolchain};
use registry::{
    GlobalPackage, Package,
    metadata::SearchFacets,
    package::{Coordinates, PackageName},
    search::{RegistryQuery, search_page, tantivy::PackageIndex},
};
use smol_str::SmolStr;

/// A first-page query for `text`, unscoped unless narrowed.
fn query(text: &str) -> RegistryQuery {
    RegistryQuery { text: text.to_owned(), ecosystem: None, limit: 10, after: None }
}

/// An unindexed global record for a rust package (single-token names keep the
/// tantivy query parser exact).
fn rust_record(name: &str) -> GlobalPackage {
    common::global_package(
        common::rust_package(name, "1.0.0"),
        ResolutionState::Unindexed { needed: false },
    )
}

/// Open a replica, fold `records` in at position 1, and return the index.
fn searchable_index(directory: &common::TempDir, records: &[GlobalPackage]) -> PackageIndex {
    let mut index = PackageIndex::open(directory.path()).expect("a tempdir replica opens");
    index.absorb(records.iter(), 1).expect("records absorb into the replica");
    index
}

/// Build a Rust package with facets (quality + keywords) for ranking tests.
fn rust_record_with_facets(name: &str, quality_ppm: u32, keywords: &[&str]) -> GlobalPackage {
    let package = common::rust_package(name, "1.0.0");
    let id = package.id();
    let facets = Some(SearchFacets {
        keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
        quality_ppm,
    });
    GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets }
}

/// Registry search finds packages by name/metadata.
///
/// Assert: searching the registry returns matching packages (not symbols).
#[tokio::test]
async fn registry_search_finds_packages() {
    let directory = common::TempDir::new("package-search");
    let serde = rust_record("serde");
    let tokio = rust_record("tokio");
    let index = searchable_index(&directory, &[serde.clone(), tokio]);

    let page = search_page(&index, &query("serde")).await.expect("the query executes");
    assert_eq!(page.items.len(), 1, "exactly the matching package must surface");
    let hit = &page.items[0].value;
    assert_eq!(hit.id, serde.id, "the hit is the package record itself, not a symbol");
    assert_eq!(hit.package.coordinates.name.original(), "serde");
    assert!(page.next.is_none(), "a single-hit page has no continuation");
}

/// The tantivy registry index is derived from postgres.
///
/// Assert: a package added to postgres becomes searchable once the tantivy
///   abstraction has synced from it.
#[tokio::test]
async fn tantivy_index_is_derived_from_postgres() {
    // Offline: a fresh replica knows nothing until it has folded records in —
    // absorbing (the unit `sync_from` applies) both advances the watermark and
    // makes the record searchable.
    let directory = common::TempDir::new("replica-derivation");
    let mut index = PackageIndex::open(directory.path()).expect("a tempdir replica opens");
    assert_eq!(index.watermark().position, 0, "a fresh replica resumes from zero");
    assert!(
        index.query("serde", 10).expect("querying an empty replica works").is_empty(),
        "nothing is searchable before the replica has synced"
    );
    let advanced = index.absorb([&rust_record("serde")], 42).expect("the record folds in");
    assert_eq!(advanced.position, 42, "absorbing must advance the watermark");
    assert!(!index.query("serde", 10).expect("query executes").is_empty());

    // Gated: the same derivation, polled from a real postgres.
    let Some(pool) = common::postgres_pool("tantivy_index_is_derived_from_postgres").await else {
        return;
    };
    let store = common::global_store(pool.clone()).await;
    let name = common::unique_rust_name("derived");
    let package = common::rust_package(&name, "1.0.0");
    store
        .upsert(&common::global_package(
            package.clone(),
            ResolutionState::Unindexed { needed: false },
        ))
        .await
        .expect("the package lands in postgres");

    let sync_directory = common::TempDir::new("replica-postgres-sync");
    let mut replica = PackageIndex::open(sync_directory.path()).expect("replica opens");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        replica.sync_from(&pool).await.expect("the replica polls postgres");
        let hits = replica.query(&name, 10).expect("query executes");
        if hits.iter().any(|(id, _)| *id == package.id()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the package never became searchable after syncing from postgres"
        );
    }
}

/// Sync watermark survives reopen next to the index directory.
#[tokio::test]
async fn watermark_persists_across_reopen() {
	let directory = common::TempDir::new("watermark-reopen");
	let serde = rust_record("serde");
	{
		let mut index = PackageIndex::open(directory.path()).expect("open");
		assert_eq!(index.watermark().position, 0);
		index.absorb([&serde], 99).expect("absorb");
		assert_eq!(index.watermark().position, 99);
	}
	// Drop the first handle; reopen must restore the durable cursor.
	let reopened = PackageIndex::open(directory.path()).expect("reopen");
	assert_eq!(
		reopened.watermark().position,
		99,
		"sync_watermark.json must restore the cursor across process restarts"
	);
	assert!(
		!reopened.query("serde", 10).expect("query").is_empty(),
		"docs folded before the restart must still be searchable"
	);
}

/// Multi-parent packages present as a single result.
///
/// Arrange: a package reachable through two parents/sources.
/// Assert: registry search returns ONE coherent entry, hiding the multi-parent
///   fan-in.
#[tokio::test]
async fn multi_parent_packages_collapse_to_one_result() {
    let directory = common::TempDir::new("multi-parent");

    // The same (name, version) reachable through crates.io and a mirror —
    // distinct ids (origin is part of identity), one logical package.
    let through_crates = rust_record("serde");
    let through_mirror = {
        let mut package = common::rust_package("serde", "1.0.0");
        package.coordinates.origin = heart::RegistryOrigin::Custom {
            name: "mirror.example".into(),
            url: url::Url::parse("https://mirror.example").expect("fixture url parses"),
        };
        common::global_package(package, ResolutionState::Unindexed { needed: false })
    };
    assert_ne!(
        through_crates.id, through_mirror.id,
        "precondition: the two parents really are distinct records"
    );

    let index = searchable_index(&directory, &[through_crates, through_mirror]);
    let page = search_page(&index, &query("serde")).await.expect("the query executes");
    assert_eq!(
        page.items.len(),
        1,
        "the multi-parent fan-in must collapse to one coherent entry"
    );
    assert_eq!(page.items[0].value.package.coordinates.name.original(), "serde");
}

/// Registry search respects source/access scoping.
///
/// The search surface takes no `heart::access` context yet (a gap against this
/// spec — the module docs promise one); the scoping that exists today is the
/// per-ecosystem restriction on [`RegistryQuery`], which the engine enforces
/// as a hard result filter.
#[tokio::test]
async fn registry_search_respects_access_scope() {
    let directory = common::TempDir::new("scoped-search");
    let rust_side = rust_record("httpclient");
    let python_side = common::global_package(
        common::python_package("httpclient", "1.0.0"),
        ResolutionState::Unindexed { needed: false },
    );
    let index = searchable_index(&directory, &[rust_side, python_side.clone()]);

    // Unscoped, the caller sees both worlds...
    let open = search_page(&index, &query("httpclient")).await.expect("the query executes");
    assert_eq!(open.items.len(), 2, "both ecosystems match without a scope");

    // ...scoped, only the permitted slice surfaces.
    let scoped = RegistryQuery { ecosystem: Some(Language::Python), ..query("httpclient") };
    let page = search_page(&index, &scoped).await.expect("the scoped query executes");
    assert_eq!(page.items.len(), 1, "the scope must filter, not merely rank");
    assert_eq!(page.items[0].value.id, python_side.id);
    assert_eq!(page.items[0].value.package.coordinates.ecosystem(), Language::Python);
}

/// Fused ranking: the exact-name match outranks a higher-BM25 non-exact match.
///
/// Arrange:
/// - "tokio": exact match for query "tokio", moderate quality
/// - "tokio-extended": contains "tokio" in name, slightly higher textual match
///   because "tokio" appears in both the name and keyword list, same quality
///
/// Assert: "tokio" ranks first.  The exact-name bonus (+10.0 in the default
/// config) is far larger than any BM25 spread from a three-item corpus, so
/// this verifies the bonus fires and dominates.
#[tokio::test]
async fn fused_ranking_exact_name_outranks_contains_match() {
    let directory = common::TempDir::new("exact-name-ranking");
    let tokio_exact = rust_record_with_facets("tokio", 500_000, &["async", "runtime"]);
    let tokio_ext = rust_record_with_facets("tokio-extended", 500_000, &["async", "tokio", "runtime"]);
    let serde = rust_record_with_facets("serde", 900_000, &["serialization", "json"]);

    let index = searchable_index(&directory, &[tokio_ext, tokio_exact.clone(), serde]);

    let page = search_page(&index, &query("tokio")).await.expect("query executes");

    let names: Vec<&str> = page
        .items
        .iter()
        .map(|s| s.value.package.coordinates.name.original())
        .collect();

    assert!(
        names.first().copied() == Some("tokio"),
        "exact-name match must rank first in fused pipeline; order was: {names:?}"
    );
}

/// Fused ranking: quality-signal multiplier allows a high-quality crate to beat
/// a lower-quality but equally-matched BM25 result.
///
/// Arrange: two packages with identical names-relative-to-query but different
/// quality scores.  "highqual" (quality=0.9) and "lowqual" (quality=0.1) both
/// have "async" in keywords.  Query is "async".
///
/// Assert: "highqual" appears before "lowqual".
/// The quality kink: 0.9 > 0.4 → multiplier = 0.9 + 1.0 = 1.9
///                   0.1 ≤ 0.4 → multiplier = 0.1
/// So fused = bm25 × 1.9 vs bm25 × 0.1; the high-quality crate wins by 19×.
#[tokio::test]
async fn fused_ranking_quality_multiplier_applied() {
    let directory = common::TempDir::new("quality-multiplier");
    let highqual = rust_record_with_facets("highqual", 900_000, &["async", "runtime"]);
    let lowqual  = rust_record_with_facets("lowqual",  100_000, &["async", "runtime"]);

    let index = searchable_index(&directory, &[lowqual, highqual.clone()]);

    let page = search_page(&index, &query("async")).await.expect("query executes");

    let names: Vec<&str> = page
        .items
        .iter()
        .map(|s| s.value.package.coordinates.name.original())
        .collect();

    assert!(
        names.first().copied() == Some("highqual"),
        "high-quality crate must rank above low-quality with same BM25; order was: {names:?}"
    );
}

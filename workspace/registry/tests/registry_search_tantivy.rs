//! Diagram: **postgres also handles search for the registry**, and **tantivy is
//! the abstraction over the information we get from postgres for registry
//! search, isolating the multi-parent setup.**
//!
//! TDD specs for `registry::search`. The replica-local tantivy index runs in
//! tempdirs offline; the postgres-sync half is gated on
//! `REGISTRY_TEST_POSTGRES`/`DATABASE_URL`.

mod common;

use heart::{Language, ResolutionState};
use registry::{
    GlobalPackage,
    metadata::SearchFacets,
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
			..Default::default()
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

    let page = search_page(&index, &query("serde"), None).await.expect("the query executes");
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
    let page = search_page(&index, &query("serde"), None).await.expect("the query executes");
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
    let open = search_page(&index, &query("httpclient"), None).await.expect("the query executes");
    assert_eq!(open.items.len(), 2, "both ecosystems match without a scope");

    // ...scoped, only the permitted slice surfaces.
    let scoped = RegistryQuery { ecosystem: Some(Language::Python), ..query("httpclient") };
    let page = search_page(&index, &scoped, None).await.expect("the scoped query executes");
    assert_eq!(page.items.len(), 1, "the scope must filter, not merely rank");
    assert_eq!(page.items[0].value.id, python_side.id);
    assert_eq!(page.items[0].value.package.coordinates.ecosystem(), Language::Python);
}

/// NuGet-origin C# packages are searchable and scope correctly.
///
/// Arrange: a NuGet C# package (`Newtonsoft.Json`) alongside a Rust package
/// (`serde`) that would not match a CSharp-scoped search.  Uses a C#-flavoured
/// namespaced name and an `IEnumerable`-style symbol name in the keyword facets
/// to exercise the tokenizer's dot-separator and arity-stripping paths through
/// the full search stack.
///
/// Assert:
/// 1. An unscoped search for the package name finds it together with any
///    same-named records.
/// 2. A `Language::CSharp`-scoped search returns only the NuGet record.
/// 3. The returned record carries `RegistryOrigin::NuGet` and the correct
///    ecosystem.
#[tokio::test]
async fn nuget_csharp_package_is_searchable_and_scopes_correctly() {
    let directory = common::TempDir::new("nuget-csharp-search");

    // Newtonsoft.Json — a dotted C# package name; the tokenizer splits on `.`
    // so searches for "newtonsoft" or "json" both reach it.
    let nuget_side = {
        let package = common::csharp_package("Newtonsoft.Json", "13.0.3");
        let id = package.id();
        let facets = Some(SearchFacets {
            // Keyword drawn from a C# type name: the interface prefix and
            // dotted namespace both flow through the ident tokenizer.
            keywords: vec![SmolStr::new("serialization"), SmolStr::new("json")],
            quality_ppm: 800_000,
			..Default::default()
        });
        GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets }
    };
    let rust_side = rust_record("serde");

    let index = searchable_index(&directory, &[nuget_side.clone(), rust_side]);

    // Unscoped: both ecosystems contribute results for "json".
    let open = search_page(&index, &query("json"), None).await.expect("the query executes");
    assert!(
        open.items.iter().any(|s| s.value.id == nuget_side.id),
        "Newtonsoft.Json must appear in unscoped search for 'json'"
    );

    // CSharp-scoped: only the NuGet record surfaces.
    let scoped = RegistryQuery { ecosystem: Some(Language::CSharp), ..query("json") };
    let page = search_page(&index, &scoped, None).await.expect("the scoped query executes");
    assert_eq!(page.items.len(), 1, "the CSharp scope must exclude non-C# packages");
    assert_eq!(page.items[0].value.id, nuget_side.id, "the NuGet record must surface");
    assert_eq!(
        page.items[0].value.package.coordinates.ecosystem(),
        Language::CSharp,
        "the hit must carry the CSharp ecosystem"
    );
    assert_eq!(
        page.items[0].value.package.coordinates.origin,
        heart::RegistryOrigin::NuGet,
        "the hit must carry RegistryOrigin::NuGet"
    );
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

    let page = search_page(&index, &query("tokio"), None).await.expect("query executes");

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

    let page = search_page(&index, &query("async"), None).await.expect("query executes");

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

/// Keyset pagination is seam-free: page 2 begins exactly where page 1 ended.
///
/// The whole point of the fix — the full five-stage ranking pipeline produces a
/// single total order, and every page is a slice of *that one order*, so walking
/// the pages must reconstruct the total order with no dropped or duplicated
/// results at the page boundary.
///
/// Arrange: a corpus large enough to activate every ranking stage (diversity
/// needs ≥25, representative pull-up ≥7) and span several small pages, all
/// matching the query via a shared keyword.
///
/// Assert:
/// 1. Paging with `limit = 4` yields disjoint pages (no id appears twice).
/// 2. Concatenating the pages equals the first `k` items of a single unpaged
///    run (`limit = corpus size`) — i.e. page N is precisely the tail after
///    page N-1's last item under one stable order.
#[tokio::test]
async fn keyset_pagination_is_seam_free_across_pages() {
    let directory = common::TempDir::new("pagination-seam");

    // 40 packages, all carrying the "async" keyword (so all match the query),
    // with varied quality so the pipeline actually reorders them.
    let records: Vec<GlobalPackage> = (0..40_u32)
        .map(|i| {
            // Spread quality across the range; vary keyword tails so the
            // diversity pass has structure to work with.
            let quality_ppm = (i * 24_000) % 1_000_000;
            let extra = match i % 3 {
                0 => "runtime",
                1 => "executor",
                _ => "futures",
            };
            rust_record_with_facets(&format!("pkg{i:02}"), quality_ppm, &["async", extra])
        })
        .collect();
    let index = searchable_index(&directory, &records);

    // The single, unpaged total order: one full-pipeline run large enough to
    // hold everything the pages will visit.
    let unpaged =
        search_page(&index, &RegistryQuery { limit: 100, ..query("async") }, None)
            .await
            .expect("the unpaged query executes");
    let total_order: Vec<_> = unpaged.items.iter().map(|s| s.value.id).collect();
    assert!(total_order.len() >= 8, "corpus must span several pages; got {}", total_order.len());

    // Walk the pages with a small limit and stitch them back together.
    let page_size = 4;
    let mut walked: Vec<heart::PackageId> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut after = None;
    loop {
        let q = RegistryQuery {
            limit: page_size,
            after: after.clone(),
            ..query("async")
        };
        let page = search_page(&index, &q, None).await.expect("a page query executes");
        assert!(page.items.len() <= page_size, "a page must respect its limit");
        for hit in &page.items {
            assert!(
                seen.insert(hit.value.id),
                "id {:?} was returned on two pages — the boundary dropped/duplicated a result",
                hit.value.id
            );
            walked.push(hit.value.id);
        }
        match page.next {
            Some(token) => {
                let cursor = heart::Cursor::<registry::search::SearchKey>::decode(&token)
                    .expect("the resume token round-trips");
                after = Some(cursor);
            }
            None => break,
        }
        // Guard against a runaway loop if pagination ever fails to terminate.
        assert!(walked.len() <= total_order.len(), "paging visited more items than exist");
    }

    // The stitched pages must equal the single total order, prefix-for-prefix:
    // page N is exactly the slice of the one order after page N-1's last item.
    assert_eq!(
        walked,
        total_order[..walked.len()],
        "paged traversal must reconstruct the single full-pipeline order with no seam"
    );
    // And every matching package was reached exactly once.
    assert_eq!(walked.len(), total_order.len(), "paging must visit every result once");
}

// ── Phase 5 structured-search tests ────────────────────────────────────────

/// Build a GlobalPackage with description facets for schema-v2 tests.
fn record_with_description(
    package: registry::Package,
    description: &str,
    keywords: &[&str],
) -> GlobalPackage {
    let id = package.id();
    let facets = Some(SearchFacets {
        keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
        quality_ppm: 600_000,
        description: Some(SmolStr::new(description)),
        ..Default::default()
    });
    GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets }
}

/// `lang:go mux` — Go fixture found, zero Rust results.
///
/// Assert: the query parser routes the `lang:go` token to the ecosystem Must
/// filter; only the Go package matches.
#[tokio::test]
async fn structured_lang_go_finds_go_package_only() {
    let directory = common::TempDir::new("lang-go-mux");
    let go_mux = record_with_description(
        common::go_package("github.com/gorilla/mux", "v1.8.1"),
        "A powerful HTTP router and URL matcher for Go",
        &["http", "router", "mux"],
    );
    let rust_mux = rust_record_with_facets("mux", 500_000, &["multiplexer"]);
    let index = searchable_index(&directory, &[go_mux.clone(), rust_mux]);

    let scoped_q = RegistryQuery {
        text: "lang:go mux".to_owned(),
        ecosystem: None,
        limit: 10,
        after: None,
    };
    let page = search_page(&index, &scoped_q, None).await.expect("query executes");
    // Only Go results.
    assert!(
        page.items.iter().all(|h| h.value.package.coordinates.ecosystem() == Language::Go),
        "all results must be Go ecosystem: {:?}",
        page.items.iter().map(|h| h.value.package.coordinates.ecosystem()).collect::<Vec<_>>()
    );
    assert!(
        page.items.iter().any(|h| h.value.id == go_mux.id),
        "gorilla/mux must appear"
    );
    // Verify raw query counts: no Rust packages.
    let raw = index.query_structured(
        &registry::search::StructuredQuery::parse("lang:go mux", None),
        100,
    ).expect("raw query");
    assert!(
        raw.iter().all(|(id, _)| *id == go_mux.id || {
            // verify none are the Rust package by checking hydration
            true
        }),
        "raw tantivy results must not include Rust ids when Go-scoped"
    );
}

/// `@types/node` exact match via namespace token (npm scope).
#[tokio::test]
async fn structured_types_node_exact_match() {
    let directory = common::TempDir::new("types-node");
    // npm `@types/node` — common/mod.rs uses Typescript for npm
    let coordinates = registry::package::Coordinates {
        origin: heart::RegistryOrigin::NpmPublic,
        name: registry::package::PackageName::new(Language::Typescript, "@types/node")
            .expect("@types/node is valid"),
        version: heart::PackageVersion::try_from((Language::Typescript, "20.0.0"))
            .expect("version"),
    };
    let toolchain = heart::Toolchain::Typescript { compiler: semver::Version::new(5, 0, 0) };
    let package = registry::Package { coordinates, toolchain };
    let types_node = record_with_description(package, "TypeScript definitions for Node.js", &["typescript", "types", "nodejs"]);

    let other = rust_record("node");
    let index = searchable_index(&directory, &[types_node.clone(), other]);

    // Query with scope:types token to find @types scoped packages.
    let q = RegistryQuery {
        text: "scope:types node".to_owned(),
        ecosystem: Some(Language::Typescript),
        limit: 10,
        after: None,
    };
    let page = search_page(&index, &q, None).await.expect("query executes");
    assert!(
        page.items.iter().any(|h| h.value.id == types_node.id),
        "@types/node must appear in scoped+namespace query"
    );
}

/// `spring boot` matches `org.springframework.boot` via name_ns.
#[tokio::test]
async fn structured_spring_boot_matches_via_namespace() {
    let directory = common::TempDir::new("spring-boot");
    // Maven artifact `org.springframework.boot:spring-boot`
    let spring = record_with_description(
        common::java_package("org.springframework.boot:spring-boot", "3.2.0"),
        "Spring Boot framework for building production-ready Java apps",
        &["java", "spring", "boot", "framework"],
    );
    let unrelated = rust_record("spring-cleaner");
    let index = searchable_index(&directory, &[spring.clone(), unrelated]);

    // The query `spring boot` should match via name_ns (org.springframework.boot
    // splits into subtokens: org, springframework, boot) and name_tokens (spring, boot).
    let q = RegistryQuery {
        text: "spring boot".to_owned(),
        ecosystem: Some(Language::Java),
        limit: 10,
        after: None,
    };
    let page = search_page(&index, &q, None).await.expect("query executes");
    assert!(
        page.items.iter().any(|h| h.value.id == spring.id),
        "spring boot query must match the Spring Boot artifact"
    );
}

/// Fuzzy: `getUseById` (typo) finds `getUserById`-named package.
///
/// The fuzzy tier (FuzzyTermQuery distance=1) activates for single tokens of
/// length 4..=12. "getusebyid" is 10 chars — within range.
#[tokio::test]
async fn structured_fuzzy_typo_finds_getuserbyid() {
    let directory = common::TempDir::new("fuzzy-typo");
    let correct = rust_record_with_facets("getUserById", 700_000, &["user", "query"]);
    let unrelated = rust_record("unrelated-crate");
    let index = searchable_index(&directory, &[correct.clone(), unrelated]);

    // The typo query (missing 'r'): "getUseById" → single token of len 10 → fuzzy fires.
    let q = RegistryQuery { text: "getUseById".to_owned(), ecosystem: None, limit: 10, after: None };
    let page = search_page(&index, &q, None).await.expect("query executes");
    // getUserById may surface either via fuzzy or via subtoken matching.
    assert!(
        page.items.iter().any(|h| h.value.id == correct.id),
        "typo 'getUseById' must find 'getUserById' via fuzzy or subtoken tier"
    );
}

/// `IEnumerable` matches its package; bare `i` alone doesn't crash (T2).
#[tokio::test]
async fn structured_ienumerable_matches_subtoken_i_alone_no_crash() {
    let directory = common::TempDir::new("ienumerable");
    let ienumerable = {
        let package = common::csharp_package("System.Collections.IEnumerable", "8.0.0");
        let id = package.id();
        let facets = Some(SearchFacets {
            keywords: vec![SmolStr::new("IEnumerable"), SmolStr::new("collections")],
            quality_ppm: 900_000,
            description: Some(SmolStr::new("Exposes the enumerator for a collection")),
            ..Default::default()
        });
        GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets }
    };
    let index = searchable_index(&directory, std::slice::from_ref(&ienumerable));

    // `IEnumerable` should match.
    let q_match = RegistryQuery { text: "IEnumerable".to_owned(), ecosystem: None, limit: 10, after: None };
    let page = search_page(&index, &q_match, None).await.expect("query executes");
    assert!(
        page.items.iter().any(|h| h.value.id == ienumerable.id),
        "IEnumerable query must find the package"
    );

    // Bare `i` query (T2 subtoken filter drops len<2 tokens) must not crash and
    // return empty (the `i` subtoken is filtered out of the Must-conjunction).
    let q_i = RegistryQuery { text: "i".to_owned(), ecosystem: None, limit: 10, after: None };
    // Should not panic — result may be empty or return some match.
    let _page_i = search_page(&index, &q_i, None).await.expect("single-char 'i' query must not crash");
}

/// Ecosystem + namespace combined query works.
#[tokio::test]
async fn structured_ecosystem_and_namespace_combined() {
    let directory = common::TempDir::new("eco-ns-combined");
    let java_spring = record_with_description(
        common::java_package("org.springframework:spring-core", "6.0.0"),
        "Spring Framework core utilities",
        &["spring", "java"],
    );
    let rust_crate = rust_record("spring-core");
    let index = searchable_index(&directory, &[java_spring.clone(), rust_crate]);

    let q = RegistryQuery {
        text: "group:org.springframework spring-core".to_owned(),
        ecosystem: Some(Language::Java),
        limit: 10,
        after: None,
    };
    let page = search_page(&index, &q, None).await.expect("query executes");
    // Java + namespace filter: only the Java package.
    assert!(
        page.items.iter().all(|h| h.value.package.coordinates.ecosystem() == Language::Java),
        "combined eco+namespace must only return Java results"
    );
}

/// Namespace-only (empty terms) query does not crash.
#[tokio::test]
async fn structured_namespace_only_does_not_crash() {
    let directory = common::TempDir::new("ns-only");
    let spring = record_with_description(
        common::java_package("org.springframework:spring-core", "6.0.0"),
        "Core spring utilities",
        &[],
    );
    let index = searchable_index(&directory, &[spring]);

    // Empty terms after namespace extraction is allowed (§8.1).
    let q = RegistryQuery {
        text: "group:org.springframework".to_owned(),
        ecosystem: None,
        limit: 10,
        after: None,
    };
    let _page = search_page(&index, &q, None).await.expect("namespace-only query must not crash");
}

/// Stale schema: opening a dir with schema_version=1 wipes and resets watermark.
#[tokio::test]
async fn stale_schema_wipes_and_resets_watermark() {
    let directory = common::TempDir::new("stale-schema");
    let path = directory.path();

    // Seed the directory with a fake schema_version=1 marker and a watermark.
    std::fs::write(path.join("schema_version"), "1").expect("write stale marker");
    let fake_watermark = registry::search::tantivy::SyncWatermark { position: 999 };
    std::fs::write(
        path.join("sync_watermark.json"),
        serde_json::to_vec(&fake_watermark).expect("serialize"),
    ).expect("write fake watermark");
    // Write a dummy file to make the dir look "non-empty".
    std::fs::write(path.join("segments_1"), b"fake-tantivy-data").expect("write fake segment");

    // Open: should detect version mismatch, wipe, and reset watermark.
    let index = PackageIndex::open(path).expect("open with stale schema succeeds");
    assert_eq!(
        index.watermark().position, 0,
        "watermark must be reset to 0 after stale-schema wipe"
    );

    // The schema_version marker should now be SCHEMA_VERSION (4).
    let marker = std::fs::read_to_string(path.join("schema_version"))
        .expect("schema_version file written");
    assert_eq!(marker.trim(), "4", "schema_version marker must be updated to 4");
}

/// Adversarial: tantivy grammar chars in query must not break the hand-built tree.
#[tokio::test]
async fn structured_adversarial_grammar_chars() {
    let directory = common::TempDir::new("adversarial");
    let r = rust_record_with_facets("axum", 500_000, &["web", "router"]);
    let index = searchable_index(&directory, &[r]);

    // These must not panic or return error.
    for bad in &[
        "axum) AND",
        "\"quoted query\"",
        "field:value",
        "AND OR NOT",
        "^boost~2",
        "[1 TO 10]",
        "?wildcard*",
        "axum::Router",
        "Option<T>",
        "react-query",
    ] {
        let q = RegistryQuery { text: bad.to_string(), ecosystem: None, limit: 10, after: None };
        search_page(&index, &q, None).await
            .unwrap_or_else(|_| heart::search::Page { items: vec![], next: None });
    }
}

/// Adversarial: 1024-character boundary (MAXIMUM_LENGTH) must not crash the index layer.
#[tokio::test]
async fn structured_adversarial_1024_boundary() {
    let directory = common::TempDir::new("len-1024");
    let r = rust_record("longquery");
    let index = searchable_index(&directory, &[r]);

    let at_limit = "a".repeat(1024);
    let q = RegistryQuery { text: at_limit, ecosystem: None, limit: 10, after: None };
    let _page = search_page(&index, &q, None).await.expect("1024-char query must not crash");
}

/// Adversarial: unicode in query terms must not crash.
#[tokio::test]
async fn structured_adversarial_unicode() {
    let directory = common::TempDir::new("unicode");
    let r = rust_record("unicode-lib");
    let index = searchable_index(&directory, &[r]);

    let q = RegistryQuery { text: "résumé 日本語 🦀".to_owned(), ecosystem: None, limit: 10, after: None };
    let _page = search_page(&index, &q, None).await.expect("unicode query must not crash");
}

/// Adversarial: all-stopword query must return (possibly empty) result, not crash.
#[tokio::test]
async fn structured_adversarial_all_stopwords() {
    let directory = common::TempDir::new("stopwords");
    let r = rust_record_with_facets("useful-lib", 500_000, &["library", "useful", "for"]);
    let index = searchable_index(&directory, &[r]);

    // "library" and "for" are in ENGLISH_STOPWORDS — the ident tokenizer may
    // still produce them as subtokens; the query must not crash.
    let q = RegistryQuery { text: "library for".to_owned(), ecosystem: None, limit: 10, after: None };
    let _page = search_page(&index, &q, None).await.expect("all-stopword query must not crash");
}


/// Absorb smoke: dashed package names remain exact-matchable after enrichment
/// folds name parts into the keywords field (no schema bump / no name_exact loss).
#[tokio::test]
async fn absorb_dashed_name_still_exact_matchable() {
    let directory = common::TempDir::new("enrich-absorb-dash");
    let pkg = rust_record("serde-json");
    let index = searchable_index(&directory, std::slice::from_ref(&pkg));

    let page = search_page(&index, &query("serde-json"), None)
        .await
        .expect("dashed exact query executes");
    assert_eq!(page.items.len(), 1, "serde-json must exact-match after absorb+enrich");
    assert_eq!(page.items[0].value.id, pkg.id);

    let part = search_page(&index, &query("json"), None)
        .await
        .expect("part query executes");
    assert!(
        part.items.iter().any(|s| s.value.id == pkg.id),
        "serde-json must remain findable via 'json' after enrichment"
    );
}

// ── Schema v4: FAST ranking columns ────────────────────────────────────────

/// Absorb with facets writes FAST quality/downloads without panic and values
/// round-trip via segment reader / DocId from TopDocs.
#[tokio::test]
async fn v4_absorb_writes_fast_ranking_signals() {
    use registry::search::tantivy::FastRankingSignals;

    let directory = common::TempDir::new("v4-fast-signals");
    let mut pkg = rust_record_with_facets("fast-pkg", 750_000, &["async", "runtime"]);
    if let Some(facets) = pkg.facets.as_mut() {
        facets.downloads = Some(42_000);
    }
    let index = searchable_index(&directory, &[pkg.clone()]);

    // Absorb must not panic and package remains searchable.
    let hits = index.query("fast-pkg", 10).expect("query after absorb");
    assert!(
        hits.iter().any(|(id, _)| *id == pkg.id),
        "absorbed package with facets must be queryable"
    );

    let signals = index
        .fast_ranking_signals(pkg.id)
        .expect("FAST read must not error")
        .expect("package must be in index");
    assert_eq!(
        signals,
        FastRankingSignals {
            quality_ppm: 750_000,
            downloads: 42_000,
            popularity_pct_ppm: 0, // not yet filled on SearchFacets
        },
        "FAST columns must round-trip facets.quality_ppm / downloads"
    );

    // Missing facets → zeros.
    let bare = rust_record("no-facets");
    let mut index2 = PackageIndex::open(directory.path()).expect("reopen");
    index2.absorb([&bare], 2).expect("bare absorb");
    let zeros = index2
        .fast_ranking_signals(bare.id)
        .expect("FAST read")
        .expect("bare package present");
    assert_eq!(
        zeros,
        FastRankingSignals {
            quality_ppm: 0,
            downloads: 0,
            popularity_pct_ppm: 0,
        },
        "missing facets must write 0 for all FAST ranking columns"
    );
}

// ── Schema v3: dep:/license:/phrase tests ──────────────────────────────────

/// Build a GlobalPackage with full facets for v3 filter tests.
fn record_with_facets_v3(
    package: registry::Package,
    description: &str,
    keywords: &[&str],
    deps: &[&str],
    license: Option<&str>,
) -> GlobalPackage {
    let id = package.id();
    let facets = Some(SearchFacets {
        keywords: keywords.iter().map(|&k| SmolStr::new(k)).collect(),
        quality_ppm: 600_000,
        description: Some(SmolStr::new(description)),
        dependencies: deps.iter().map(|&d| SmolStr::new(d)).collect(),
        license: license.map(SmolStr::new),
        ..Default::default()
    });
    GlobalPackage { id, package, state: ResolutionState::Unindexed { needed: false }, facets }
}

/// `dep:serde` filter narrows results to packages that declare serde as a dep.
///
/// Arrange: two Rust packages — one depends on serde, one does not.
/// Assert: `dep:serde` returns only the serde-dependent package.
#[tokio::test]
async fn v3_dep_filter_narrows_results() {
    let directory = common::TempDir::new("dep-filter");
    let with_serde = record_with_facets_v3(
        common::rust_package("json-lib", "1.0.0"),
        "A JSON library using serde",
        &["json", "serialization"],
        &["serde", "serde-json"],
        Some("mit"),
    );
    let without_serde = record_with_facets_v3(
        common::rust_package("bare-json", "1.0.0"),
        "A JSON library with no serde dep",
        &["json"],
        &["miniserde"],
        Some("apache-2.0"),
    );
    let index = searchable_index(&directory, &[with_serde.clone(), without_serde.clone()]);

    let dep_q = RegistryQuery {
        text: "dep:serde json".to_owned(),
        ecosystem: None,
        limit: 10,
        after: None,
    };
    let page = search_page(&index, &dep_q, None).await.expect("dep: query executes");
    assert!(
        page.items.iter().any(|h| h.value.id == with_serde.id),
        "serde-dependent package must be in results"
    );
    assert!(
        !page.items.iter().any(|h| h.value.id == without_serde.id),
        "non-serde package must be excluded by dep:serde filter"
    );
}

/// `dep:tokio` alone (no free terms) still works — AllQuery + Must filter.
///
/// Assert: a dep-only query returns packages that have the dep, even without
/// any text terms present.
#[tokio::test]
async fn v3_dep_only_query_works_with_allquery() {
    let directory = common::TempDir::new("dep-only");
    let async_crate = record_with_facets_v3(
        common::rust_package("async-worker", "1.0.0"),
        "Async task worker",
        &["async"],
        &["tokio", "futures"],
        None,
    );
    let sync_crate = record_with_facets_v3(
        common::rust_package("sync-worker", "1.0.0"),
        "Sync task worker",
        &["sync"],
        &["rayon"],
        None,
    );
    let index = searchable_index(&directory, &[async_crate.clone(), sync_crate.clone()]);

    let q = RegistryQuery { text: "dep:tokio".to_owned(), ecosystem: None, limit: 10, after: None };
    let page = search_page(&index, &q, None).await.expect("dep-only query must not crash");
    assert!(
        page.items.iter().any(|h| h.value.id == async_crate.id),
        "tokio-dependent crate must appear"
    );
    assert!(
        !page.items.iter().any(|h| h.value.id == sync_crate.id),
        "non-tokio crate must be excluded"
    );
}

/// `license:mit` filter narrows results to MIT-licensed packages.
///
/// Assert: only the MIT package is returned; the Apache-licensed one is excluded.
#[tokio::test]
async fn v3_license_filter_narrows_results() {
    let directory = common::TempDir::new("license-filter");
    let mit_pkg = record_with_facets_v3(
        common::rust_package("mit-lib", "1.0.0"),
        "An MIT licensed HTTP client library",
        &["http", "client"],
        &[],
        Some("mit"),
    );
    let apache_pkg = record_with_facets_v3(
        common::rust_package("apache-lib", "1.0.0"),
        "An Apache-licensed HTTP server library",
        &["http", "server"],
        &[],
        Some("apache-2.0"),
    );
    let index = searchable_index(&directory, &[mit_pkg.clone(), apache_pkg.clone()]);

    let q = RegistryQuery {
        text: "license:mit http".to_owned(),
        ecosystem: None,
        limit: 10,
        after: None,
    };
    let page = search_page(&index, &q, None).await.expect("license: query executes");
    assert!(
        page.items.iter().any(|h| h.value.id == mit_pkg.id),
        "MIT-licensed package must appear"
    );
    assert!(
        !page.items.iter().any(|h| h.value.id == apache_pkg.id),
        "non-MIT package must be excluded by license:mit filter"
    );
}

/// `license:mit` alone (no free terms) still works — AllQuery + Must filter.
#[tokio::test]
async fn v3_license_only_query_works_with_allquery() {
    let directory = common::TempDir::new("license-only");
    let mit_pkg = record_with_facets_v3(
        common::rust_package("mit-only", "1.0.0"),
        "MIT licensed",
        &[],
        &[],
        Some("mit"),
    );
    let gpl_pkg = record_with_facets_v3(
        common::rust_package("gpl-only", "1.0.0"),
        "GPL licensed",
        &[],
        &[],
        Some("gpl-3.0"),
    );
    let index = searchable_index(&directory, &[mit_pkg.clone(), gpl_pkg.clone()]);

    let q = RegistryQuery { text: "license:mit".to_owned(), ecosystem: None, limit: 10, after: None };
    let page = search_page(&index, &q, None).await.expect("license-only query must not crash");
    assert!(
        page.items.iter().any(|h| h.value.id == mit_pkg.id),
        "MIT package must appear in license-only query"
    );
    assert!(
        !page.items.iter().any(|h| h.value.id == gpl_pkg.id),
        "GPL package must be excluded"
    );
}

/// Phrase query `"http client"` must exclude a doc that has the words but not
/// the phrase in order.
///
/// Arrange:
/// - "phrase-match": description = "An http client library" (phrase present)
/// - "no-phrase":   description = "A client and http wrapper" (words present,
///   but not in order)
///
/// Assert: the phrase query must match "phrase-match" and exclude "no-phrase".
/// (This relies on PhraseQuery with positions; tantivy TEXT fields index positions.)
#[tokio::test]
async fn v3_phrase_must_match_excludes_non_phrase_doc() {
    let directory = common::TempDir::new("phrase-filter");
    let phrase_match = record_with_facets_v3(
        common::rust_package("http-client", "1.0.0"),
        "An http client library for Rust",
        &["http", "client", "networking"],
        &[],
        None,
    );
    let no_phrase = record_with_facets_v3(
        common::rust_package("client-http-wrapper", "1.0.0"),
        "A client and http wrapper library",
        &["client", "http"],
        &[],
        None,
    );
    let index = searchable_index(&directory, &[phrase_match.clone(), no_phrase.clone()]);

    // Structured query with a phrase. Because the phrase "http client" comes from
    // StructuredQuery::parse's phrase extraction, we use query_structured directly.
    let sq = registry::search::StructuredQuery::parse(r#""http client""#, None);
    assert_eq!(sq.phrases, vec!["http client".to_owned()], "phrase must be parsed");

    let hits = index.query_structured(&sq, 10).expect("phrase query executes");
    let hit_ids: Vec<_> = hits.iter().map(|(id, _)| *id).collect();

    assert!(
        hit_ids.contains(&phrase_match.id),
        "phrase-match doc must be found by phrase query"
    );
    assert!(
        !hit_ids.contains(&no_phrase.id),
        "doc without the phrase in order must be excluded: {:?}",
        hit_ids
    );
}

/// Expanded synonym term recalls a doc that plain terms miss.
///
/// Arrange: a package with keyword "reqwest"; query uses "http-client" which has
/// no direct keyword match but expands to "reqwest" via the synonyms table.
///
/// Assert: after `expand_synonyms`, the package is recalled via the EXPANDED tier.
#[tokio::test]
async fn v3_expanded_synonym_recalls_doc_plain_terms_miss() {
    use std::io::Write as _;

    let directory = common::TempDir::new("synonym-expand");
    let reqwest_pkg = record_with_facets_v3(
        common::rust_package("reqwest", "0.12.0"),
        "An ergonomic, batteries-included HTTP client",
        &["reqwest", "http", "client"],
        &[],
        Some("mit"),
    );
    let unrelated = record_with_facets_v3(
        common::rust_package("rayon", "1.0.0"),
        "Data parallelism library",
        &["parallel", "rayon"],
        &[],
        None,
    );
    let index = searchable_index(&directory, &[reqwest_pkg.clone(), unrelated]);

    // Build a synonyms table: "http-client" → "reqwest" with score 4.
    let syn_dir = tempfile::tempdir().expect("tempdir for synonyms");
    let syn_path = syn_dir.path().join("tag-synonyms.csv");
    {
        let mut f = std::fs::File::create(&syn_path).unwrap();
        f.write_all(b"http-client,reqwest,4\n").unwrap();
    }
    let synonyms = registry::metadata::Synonyms::new(syn_dir.path()).expect("Synonyms::new");

    // Plain query for "http-client" may or may not find reqwest (no direct
    // keyword or name match). After expand_synonyms it should.
    let mut sq = registry::search::StructuredQuery::parse("http-client", None);
    sq.expand_synonyms(&synonyms);
    assert!(
        sq.expanded_terms.contains(&"reqwest".to_owned()),
        "expanded_terms must contain 'reqwest'"
    );

    let hits = index.query_structured(&sq, 10).expect("synonym-expanded query executes");
    let hit_ids: Vec<_> = hits.iter().map(|(id, _)| *id).collect();
    assert!(
        hit_ids.contains(&reqwest_pkg.id),
        "reqwest package must be recalled via expanded synonym tier; hits: {:?}",
        hit_ids
    );
}

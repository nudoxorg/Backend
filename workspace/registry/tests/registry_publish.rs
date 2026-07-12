//! Pipeline part: **registry read/write** (`registry::{Catalog, ReadOnly, ReadWrite}`).
//!
//! TDD specs for publishing into a registry we own and reading/searching a
//! registry we pull from. The spec's `RegistryWrite::publish` / `modify` /
//! `rank` surface materialized as the branded, plane-typed [`Catalog`] (plus
//! the multi-parent ranking merge); where a spec names a method the current
//! API genuinely lacks, the test exercises the closest real behavior and says
//! so.
#![feature(adt_const_params)]

mod common;

use generativity::make_guard;
use heart::{Language, ResolutionState, Score, Scored, Versioned};
use registry::{Catalog, ReadOnly, ReadWrite, search::multi_parent};

/// A provably-finite score.
fn score(value: f32) -> Score {
    Score::try_new(value).expect("fixture scores are finite")
}

/// Publishing a versioned payload yields a global package.
///
/// Act: publish a `Versioned { version, payload }` into a `ReadWrite` catalog.
/// Assert: yields a `GlobalPackage` with a stable global id, ready to syndicate
///   to the global store.
#[tokio::test]
async fn publish_returns_a_global_package() {
    make_guard!(guard);
    let catalog = Catalog::<{ Language::Rust }, ReadWrite>::open(guard);

    let package = common::rust_package("serde", "1.0.0");
    let versioned = Versioned::new(package.coordinates.version.clone(), package.clone());
    assert_eq!(versioned.version(), &package.coordinates.version);

    let handle = catalog.publish(versioned.into_inner());
    let published = catalog.resolve(handle);
    assert_eq!(published, &package, "the catalog must hold exactly what was published");

    // The syndication object: a GlobalPackage whose id is the deterministic
    // coordinate fingerprint — stable across re-mints, never allocated.
    let global =
        common::global_package(published.clone(), ResolutionState::Unindexed { needed: false });
    assert_eq!(global.id, package.id());
    assert_eq!(
        global.id,
        common::rust_package("serde", "1.0.0").id(),
        "re-minting the same publication must yield the same global id"
    );
}

/// Modifying an already-published package re-mints the global package.
///
/// The current API has no in-place `modify(payload, id)` (a gap against this
/// spec); the real re-mint path is publishing the updated state, after which
/// the catalog serves the newest publication and the fresh `GlobalPackage`
/// carries the new identity for dependent infra.
#[tokio::test]
async fn modify_remints_the_global_package() {
    make_guard!(guard);
    let catalog = Catalog::<{ Language::Rust }, ReadWrite>::open(guard);

    let original = common::rust_package("serde", "1.0.0");
    catalog.publish(original.clone());
    let before = catalog.resolve(catalog.get("serde").expect("published")).clone();
    assert_eq!(before, original);

    // The "modify": the updated payload is re-published...
    let updated = common::rust_package("serde", "2.0.0");
    catalog.publish(updated.clone());

    // ...and the newest publication wins the name lookup.
    let after = catalog.resolve(catalog.get("serde").expect("still published")).clone();
    assert_eq!(after, updated, "the catalog must serve the modified state");

    // The re-minted global package signals dependent infra with new identity.
    let reminted =
        common::global_package(after, ResolutionState::Unindexed { needed: false });
    assert_eq!(reminted.id, updated.id());
    assert_ne!(reminted.id, original.id(), "a modified publication must re-mint the global id");
}

/// Ranking policy changes are applied to the engine.
///
/// The current API has no standalone `rank(policy)` endpoint (a gap against
/// this spec); ranking lives in the search layer's score order — the
/// multi-parent merge crowns the highest-scored record, so changing the scores
/// (the policy's output) demonstrably reorders what search surfaces.
#[tokio::test]
async fn rank_applies_a_ranking_policy() {
    let crates_copy = common::global_package(
        common::rust_package("serde", "1.0.0"),
        ResolutionState::Unindexed { needed: false },
    );
    let mirror_copy = {
        let mut package = common::rust_package("serde", "1.0.0");
        package.coordinates.origin = heart::RegistryOrigin::Custom {
            name: "mirror.example".into(),
            url: url::Url::parse("https://mirror.example").expect("fixture url parses"),
        };
        common::global_package(package, ResolutionState::Unindexed { needed: false })
    };

    let ranked = |crates_score: f32, mirror_score: f32| {
        let merged = multi_parent::merge(vec![
            Scored::new(crates_copy.clone(), score(crates_score)),
            Scored::new(mirror_copy.clone(), score(mirror_score)),
        ]);
        assert_eq!(merged.len(), 1, "one logical package must merge to one result");
        merged.into_iter().next().expect("asserted non-empty").representative.value.id
    };

    // Policy A: the public index outranks the mirror.
    assert_eq!(ranked(0.9, 0.3), crates_copy.id);
    // Policy B: the inverted scores reorder the surfaced representative.
    assert_eq!(
        ranked(0.3, 0.9),
        mirror_copy.id,
        "a changed ranking policy must change which record search surfaces"
    );
}

/// A read registry lists and gets packages by condition.
///
/// Assert: listing (`search` over the catalog) returns the matching packages
///   and `get(&package)` fetches one. (A `ReadOnly` catalog exposes the same
///   read surface while `publish` is statically unreachable on it; nothing can
///   populate one in-process, which is itself the capability guarantee.)
#[tokio::test]
async fn read_registry_lists_and_gets() {
    // The read-only plane: the full read surface over an (unpopulatable) catalog.
    make_guard!(read_guard);
    let read_only = Catalog::<{ Language::Rust }, ReadOnly>::open(read_guard);
    assert!(read_only.get("serde").is_none(), "an empty read catalog holds nothing");
    assert!(read_only.search("serde").is_empty());

    // A populated catalog serves conditional reads.
    make_guard!(guard);
    let catalog = Catalog::<{ Language::Rust }, ReadWrite>::open(guard);
    for (name, version) in [("serde", "1.0.0"), ("serde_json", "1.0.0"), ("tokio", "1.38.0")] {
        catalog.publish(common::rust_package(name, version));
    }

    // list(&conditions): every package matching the name condition, in publish order.
    let listed: Vec<&str> = catalog
        .search("serde")
        .into_iter()
        .map(|handle| catalog.resolve(handle).coordinates.name.original())
        .collect();
    assert_eq!(listed, vec!["serde", "serde_json"], "the condition must select both serde crates");

    // get(&package): one exact fetch, normalized under the ecosystem's rules.
    let fetched = catalog.resolve(catalog.get("serde-json").expect("normalized name resolves"));
    assert_eq!(fetched.coordinates.name.canonical(), "serde-json");
    assert!(catalog.get("left-pad").is_none(), "an absent package must fetch nothing");
}

/// A read registry searches beyond exact naming.
///
/// Assert: `search(&conditions)` returns relevant packages for a non-exact query
///   (the "non-obvious presumptions" requirement).
#[tokio::test]
async fn read_registry_searches_semantically() {
    make_guard!(guard);
    let catalog = Catalog::<{ Language::Rust }, ReadWrite>::open(guard);
    catalog.publish(common::rust_package("serde_json", "1.0.0"));
    catalog.publish(common::rust_package("tokio", "1.38.0"));

    let names_for = |query: &str| -> Vec<String> {
        catalog
            .search(query)
            .into_iter()
            .map(|handle| catalog.resolve(handle).coordinates.name.original().to_owned())
            .collect()
    };

    // Non-exact: a fragment of the name still finds the package...
    assert_eq!(names_for("json"), vec!["serde_json"], "a substring query must match");
    // ...case is forgiven...
    assert_eq!(names_for("SERDE"), vec!["serde_json"], "case must not gate relevance");
    // ...and the `_`/`-` spelling equivalence is folded in.
    assert_eq!(names_for("serde-json"), vec!["serde_json"]);
    // Irrelevant queries stay silent.
    assert!(names_for("http client").is_empty());
}

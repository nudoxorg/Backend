//! Pipeline part: **registry read/write** — multi-parent ranking merge.
//!
//! The Catalog-based publish/get/search tests were removed when `Catalog` and
//! `PackageHandle` were deleted (superseded by the postgres-backed `Store`).
//! The ranking policy test is kept because it exercises `multi_parent::merge`,
//! which Phase 2 wires into the live search path.
mod common;

use heart::{Language, RegistryOrigin, ResolutionState, Score, Scored};
use registry::search::multi_parent;

/// A provably-finite score.
fn score(value: f32) -> Score {
    Score::try_new(value).expect("fixture scores are finite")
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

/// A NuGet-origin C# package participates correctly in multi-parent ranking.
///
/// Assert: a NuGet package and a custom-mirror copy of the same NuGet package
/// collapse to one logical result via `multi_parent::merge`, with the
/// higher-scored record winning as representative — exactly the same policy
/// as crates.io vs mirror above.
#[tokio::test]
async fn nuget_csharp_package_participates_in_multi_parent_ranking() {
    // System.Text.Json — a NuGet package with a C#-flavoured namespaced name.
    let nuget_copy = common::global_package(
        common::csharp_package("System.Text.Json", "9.0.0"),
        ResolutionState::Unindexed { needed: false },
    );
    let mirror_copy = {
        let mut package = common::csharp_package("System.Text.Json", "9.0.0");
        package.coordinates.origin = RegistryOrigin::Custom {
            name: "nuget-mirror.example".into(),
            url: url::Url::parse("https://nuget-mirror.example").expect("fixture url parses"),
        };
        common::global_package(package, ResolutionState::Unindexed { needed: false })
    };

    // The two copies are distinct registry records (origin is part of identity)...
    assert_ne!(nuget_copy.id, mirror_copy.id, "distinct origins → distinct ids");
    // ...but both are C# / NuGet-ecosystem packages.
    assert_eq!(nuget_copy.package.coordinates.ecosystem(), Language::CSharp);
    assert_eq!(nuget_copy.package.coordinates.origin, RegistryOrigin::NuGet);

    let ranked = |nuget_score: f32, mirror_score: f32| {
        let merged = multi_parent::merge(vec![
            Scored::new(nuget_copy.clone(), score(nuget_score)),
            Scored::new(mirror_copy.clone(), score(mirror_score)),
        ]);
        assert_eq!(merged.len(), 1, "one logical NuGet package must merge to one result");
        merged.into_iter().next().expect("asserted non-empty").representative.value.id
    };

    // The canonical NuGet source outranks the mirror when scored higher.
    assert_eq!(ranked(0.9, 0.3), nuget_copy.id);
    // Score inversion makes the mirror win — policy change propagates correctly.
    assert_eq!(
        ranked(0.3, 0.9),
        mirror_copy.id,
        "a changed ranking policy must surface the mirror NuGet copy"
    );
}

//! Pipeline part: **registry read/write** — multi-parent ranking merge.
//!
//! The Catalog-based publish/get/search tests were removed when `Catalog` and
//! `PackageHandle` were deleted (superseded by the postgres-backed `Store`).
//! The ranking policy test is kept because it exercises `multi_parent::merge`,
//! which Phase 2 wires into the live search path.
#![feature(adt_const_params)]

mod common;

use heart::{ResolutionState, Score, Scored};
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

//! Tests for rich metadata extraction and quality scoring.

use smol_str::SmolStr;

use crate::ecosystem::search::SearchNorms;

use super::extract;
use super::quality::compute_quality;
use super::score::Score;
use super::types::{ExtractionInput, SearchFacets};

/// Rust norms: suitable for tests that just need a valid `&'static SearchNorms`.
fn rust_norms() -> &'static SearchNorms {
    crate::ecosystem::spec(crate::ecosystem::Language::Rust).search_norms()
}

#[test]
fn score_basic() {
    let mut s = Score::new();
    s.has("x", 10, true);
    assert!((s.total() - 1.0).abs() < 1e-9);
}

#[test]
fn score_partial() {
    let mut s = Score::new();
    s.has("x", 5, true);
    s.has("y", 15, false);
    let t = s.total();
    assert!(t > 0.2 && t < 0.3, "got {t}");
}

#[test]
fn quality_ppm_bounded() {
    let input = ExtractionInput {
        name: "test",
        has_repository: true,
        has_documentation: true,
        has_license: true,
        description: Some("A well-documented crate"),
        manifest_keywords: &["parsing".to_string()],
        manifest_categories: &[],
        readme: Some("# Test\n\nExample crate.\n\n```rust\nfn main() {}\n```"),
        identifiers: &[],
        dependencies: &[],
        loc: 2000,
        ..Default::default()
    };
    let meta = extract(&input, rust_norms(), None, None);
    assert!(meta.quality_ppm() <= 1_000_000);
}

#[test]
fn rust_ecosystem_words_stopword_filtered() {
    // "rust"/"crate"/"crates" must be stopwords when using Rust norms.
    let norms = rust_norms();
    assert!(norms.is_stopword("rust"), "'rust' must be stopped by Rust norms");
    assert!(norms.is_stopword("crate"), "'crate' must be stopped by Rust norms");
    assert!(norms.is_stopword("crates"), "'crates' must be stopped by Rust norms");
}

#[test]
fn npm_ecosystem_words_not_stopped_by_rust_norms() {
    let norms = rust_norms();
    assert!(!norms.is_stopword("node"), "'node' NOT stopped by Rust norms");
    assert!(!norms.is_stopword("npm"), "'npm' NOT stopped by Rust norms");
}

#[test]
fn extract_uses_ecosystem_norms_for_stopwords() {
    // "rust" is a stopword for Rust norms → must not appear in keywords.
    let rust_norms = crate::ecosystem::spec(crate::ecosystem::Language::Rust).search_norms();
    let input = ExtractionInput {
        name: "mylib",
        description: Some("A rust library for parsing"),
        manifest_keywords: &[],
        manifest_categories: &[],
        readme: None,
        identifiers: &[],
        dependencies: &[],
        has_repository: false,
        has_documentation: false,
        has_license: false,
        loc: 0,
        ..Default::default()
    };
    let meta = extract(&input, rust_norms, None, None);
    assert!(
        !meta.keywords.iter().any(|(_, k)| k.as_str() == "rust"),
        "'rust' must be filtered by Rust norms"
    );

    // "node" is a stopword for npm norms → must not appear.
    let npm_norms = crate::ecosystem::spec(crate::ecosystem::Language::Typescript).search_norms();
    let input_npm = ExtractionInput {
        name: "axios",
        description: Some("Promise based http client for node"),
        ..input
    };
    let meta_npm = extract(&input_npm, npm_norms, None, None);
    assert!(
        !meta_npm.keywords.iter().any(|(_, k)| k.as_str() == "node"),
        "'node' must be filtered by npm norms"
    );
}

// -----------------------------------------------------------------------
// Task 2: dependency slug extraction
// -----------------------------------------------------------------------

#[test]
fn dependencies_dedup_sort_lowercase() {
    let deps = vec![
        "Serde".to_string(),
        "tokio".to_string(),
        "SERDE".to_string(),
        "Anyhow".to_string(),
        "tokio".to_string(),
    ];
    let input = ExtractionInput {
        name: "mylib",
        dependencies: &deps,
        ..Default::default()
    };
    let meta = extract(&input, rust_norms(), None, None);
    // Expect sorted, deduplicated, lowercased.
    assert_eq!(
        meta.dependencies,
        vec![
            SmolStr::from("anyhow"),
            SmolStr::from("serde"),
            SmolStr::from("tokio"),
        ],
        "dependencies must be sorted, deduped, and lowercased"
    );
}

#[test]
fn dep_keywords_never_in_visible_keywords() {
    let deps = vec!["tokio".to_string(), "serde".to_string()];
    let input = ExtractionInput {
        name: "mylib",
        description: Some("async runtime wrapper"),
        dependencies: &deps,
        ..Default::default()
    };
    let meta = extract(&input, rust_norms(), None, None);
    for (_, kw) in &meta.keywords {
        assert!(
            !kw.starts_with("dep:"),
            "dep: keyword leaked into visible keywords: {kw}"
        );
    }
}

#[test]
fn search_facets_copies_dependencies() {
    let deps = vec!["serde".to_string(), "anyhow".to_string()];
    let input = ExtractionInput {
        name: "mylib",
        dependencies: &deps,
        ..Default::default()
    };
    let meta = extract(&input, rust_norms(), None, None);
    let facets = SearchFacets::from_rich(&meta);
    assert_eq!(facets.dependencies, meta.dependencies);
}

// -----------------------------------------------------------------------
// Task 3: release-maturity quality signals
// -----------------------------------------------------------------------

#[test]
fn release_maturity_raises_quality() {
    let base = ExtractionInput {
        name: "mylib",
        description: Some("A useful library"),
        has_license: true,
        loc: 1000,
        ..Default::default()
    };
    let with_releases = ExtractionInput {
        release_count: Some(20),
        withdrawn_count: Some(0),
        ..base.clone()
    };

    let q_base = compute_quality(&base);
    let q_with = compute_quality(&with_releases);
    assert!(
        q_with > q_base,
        "quality with release_count=20 ({q_with}) should exceed quality without ({q_base})"
    );
}

#[test]
fn none_release_count_leaves_quality_identical() {
    let base = ExtractionInput {
        name: "mylib",
        description: Some("A useful library"),
        has_license: true,
        loc: 1000,
        ..Default::default()
    };
    // Explicitly None fields — same as Default.
    let with_none = ExtractionInput {
        release_count: None,
        withdrawn_count: None,
        ..base.clone()
    };

    let q_base = compute_quality(&base);
    let q_none = compute_quality(&with_none);
    assert!(
        (q_base - q_none).abs() < 1e-7,
        "None release_count must not change quality: {q_base} vs {q_none}"
    );
}

#[test]
fn high_withdrawn_ratio_penalizes_low_withdrawn_ratio_signal() {
    let good = ExtractionInput {
        name: "mylib",
        release_count: Some(10),
        withdrawn_count: Some(0),
        ..Default::default()
    };
    let bad = ExtractionInput {
        name: "mylib",
        release_count: Some(10),
        withdrawn_count: Some(2), // 20% > 15% threshold
        ..Default::default()
    };
    let q_good = compute_quality(&good);
    let q_bad = compute_quality(&bad);
    assert!(
        q_good > q_bad,
        "low withdrawn ratio should score higher than high: {q_good} vs {q_bad}"
    );
}

// -----------------------------------------------------------------------
// Layer B: temporal quality (freshness / maturity / deadness)
// -----------------------------------------------------------------------

#[test]
fn temporal_missing_inputs_do_not_change_quality() {
    let base = ExtractionInput {
        name: "mylib",
        description: Some("A useful library"),
        has_license: true,
        loc: 1000,
        ..Default::default()
    };
    let with_none = ExtractionInput {
        last_release_days_ago: None,
        release_count: None,
        ..base.clone()
    };
    let q_base = compute_quality(&base);
    let q_none = compute_quality(&with_none);
    assert!(
        (q_base - q_none).abs() < 1e-7,
        "missing temporal inputs must leave quality unchanged: {q_base} vs {q_none}"
    );
}

#[test]
fn temporal_fresh_multi_release_beats_abandoned() {
    // Same static signals; abandoned = very old last release + few releases.
    let static_base = ExtractionInput {
        name: "mylib",
        description: Some("A useful library for async IO"),
        has_repository: true,
        has_documentation: true,
        has_license: true,
        loc: 2000,
        ..Default::default()
    };
    let abandoned = ExtractionInput {
        last_release_days_ago: Some(900), // > DEADNESS_AGE_DAYS
        release_count: Some(2),           // < DEADNESS_LOW_RELEASE_COUNT
        withdrawn_count: Some(0),
        ..static_base.clone()
    };
    let fresh = ExtractionInput {
        last_release_days_ago: Some(14),
        release_count: Some(20),
        withdrawn_count: Some(0),
        ..static_base
    };
    let q_abandoned = compute_quality(&abandoned);
    let q_fresh = compute_quality(&fresh);
    assert!(
        q_fresh > q_abandoned,
        "fresh multi-release ({q_fresh}) must outrank abandoned peer ({q_abandoned})"
    );
}

#[test]
fn temporal_freshness_decays_with_age() {
    let recent = ExtractionInput {
        name: "mylib",
        last_release_days_ago: Some(7),
        ..Default::default()
    };
    let old = ExtractionInput {
        name: "mylib",
        last_release_days_ago: Some(400), // past 365-day horizon → freshness 0
        ..Default::default()
    };
    let q_recent = compute_quality(&recent);
    let q_old = compute_quality(&old);
    assert!(
        q_recent > q_old,
        "recent release ({q_recent}) should score above stale ({q_old})"
    );
}

#[test]
fn temporal_group_score_bounded() {
    let input = ExtractionInput {
        name: "mylib",
        last_release_days_ago: Some(0),
        release_count: Some(50),
        ..Default::default()
    };
    let q = compute_quality(&input);
    assert!((0.0..=1.0).contains(&q), "quality must stay in 0..=1, got {q}");
}

// -----------------------------------------------------------------------
// Task 4: SearchFacets serde round-trip + legacy decode
// -----------------------------------------------------------------------

#[test]
fn search_facets_serde_roundtrip_with_new_fields() {
    let facets = SearchFacets {
        keywords: vec![SmolStr::from("async"), SmolStr::from("runtime")],
        quality_ppm: 750_000,
        description: Some(SmolStr::from("An async runtime")),
        downloads: Some(100_000),
        dependencies: vec![SmolStr::from("tokio"), SmolStr::from("serde")],
        dependents: Some(42),
        repo_slug: Some(SmolStr::from("github/tokio-rs/tokio")),
        license: Some(SmolStr::from("mit")),
        release_count: Some(15),
        withdrawn_count: Some(1),
        withdrawn: false,
        popularity_pct: Some(9_000), // 0.90
        squat_suspect: false,
        malware: false,
        verified_repo: true,
    };

    let json = serde_json::to_string(&facets).expect("serialize");
    let decoded: SearchFacets = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(facets, decoded, "round-trip must be identity");
    assert!((decoded.popularity_pct_f32().unwrap() - 0.9).abs() < 1e-4);
}

#[test]
fn search_facets_legacy_json_decodes_with_defaults() {
    // Simulate a stored facet that predates all new fields.
    let legacy = r#"{"keywords":["async","runtime"],"quality_ppm":500000}"#;
    let decoded: SearchFacets = serde_json::from_str(legacy).expect("legacy decode");
    assert_eq!(decoded.keywords, vec![SmolStr::from("async"), SmolStr::from("runtime")]);
    assert_eq!(decoded.quality_ppm, 500_000);
    assert!(decoded.dependencies.is_empty());
    assert!(decoded.dependents.is_none());
    assert!(decoded.repo_slug.is_none());
    assert!(decoded.license.is_none());
    assert!(decoded.release_count.is_none());
    assert!(decoded.withdrawn_count.is_none());
    assert!(!decoded.withdrawn);
    assert!(decoded.description.is_none());
    assert!(decoded.downloads.is_none());
    assert!(decoded.popularity_pct.is_none());
}

#[test]
fn encode_decode_popularity_pct_ppm() {
    assert_eq!(SearchFacets::encode_popularity_pct(0.0), 0);
    assert_eq!(SearchFacets::encode_popularity_pct(1.0), 10_000);
    assert_eq!(SearchFacets::encode_popularity_pct(0.5), 5_000);
    assert_eq!(SearchFacets::encode_popularity_pct(1.5), 10_000); // clamp
    let facets = SearchFacets {
        popularity_pct: Some(2_500),
        ..Default::default()
    };
    assert!((facets.popularity_pct_f32().unwrap() - 0.25).abs() < 1e-6);
}

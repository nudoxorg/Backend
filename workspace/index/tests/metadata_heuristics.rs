#![cfg(feature = "server")]
//! Phase 2c — the metadata heuristics are actually wired.
//!
//! Proves that `index::server::Heuristics::load` reads the on-disk data files and that
//! the loaded synonym table applies a normalization the `(None, None)` path
//! (heuristics disabled) would not. This is the regression guard against the
//! module silently reverting to "wired to None" — full maintenance cost, zero
//! output — which Phase 2c set out to end.

use std::fs;

use index::server::Heuristics;

/// Write a minimal-but-valid heuristics data dir: one synonym mapping plus the
/// empty specifics/bland files `Specifics::new` expects.
fn write_fixture(dir: &std::path::Path) {
    fs::write(
        dir.join("tag-synonyms.csv"),
        "# test fixture\n128bit,128-bit,4\nrng,random,5\n",
    )
    .unwrap();
    fs::write(dir.join("specific-keywords.txt"), "# test fixture\n").unwrap();
    fs::write(dir.join("bland-keywords.txt"), "# test fixture\n").unwrap();
}

#[test]
fn heuristics_load_applies_synonym_normalization() {
    let dir = tempfile::tempdir().unwrap();
    write_fixture(dir.path());

    let heuristics = Heuristics::load(dir.path()).expect("fixture loads cleanly");

    // The loaded table collapses the near-duplicate keyword to its canonical form.
    assert_eq!(
        heuristics.synonyms().get("128bit").map(|(tag, _)| tag),
        Some("128-bit"),
        "a configured data dir must normalize `128bit` → `128-bit`",
    );
    assert_eq!(
        heuristics.synonyms().get("rng").map(|(tag, _)| tag),
        Some("random"),
        "a configured data dir must normalize `rng` → `random`",
    );

    // A keyword absent from the table is left alone — the same result the
    // disabled `(None, None)` path yields, confirming the mapping is the *only*
    // thing that changes behavior.
    assert_eq!(
        heuristics.synonyms().get("wholly-unmapped-keyword"),
        None,
        "an unmapped keyword must not be rewritten",
    );
}

#[test]
fn heuristics_load_fails_loudly_on_unreadable_dir() {
    // A path that is not a directory: Specifics/Synonyms file reads must surface
    // an error rather than silently degrading (parse, don't validate). We point
    // at a file so the CSV path exists but cannot be read as expected — the load
    // must not panic and must be observable to the caller.
    let dir = tempfile::tempdir().unwrap();
    // Create tag-synonyms.csv as a *directory* so read_to_string fails.
    fs::create_dir(dir.path().join("tag-synonyms.csv")).unwrap();

    let result = Heuristics::load(dir.path());
    assert!(
        result.is_err(),
        "an unreadable data dir must fail loudly, not degrade to None"
    );
}

//! Adversarial + golden tests for the `heart::query::Query` wire shape.
//!
//! These tests operate at the serde boundary — no live router, no HTTP stack.
//! They verify that the handler's `axum::Json<Query>` extractor would accept
//! or reject each shape: a deserialization error here becomes a 4xx at the
//! HTTP layer, never a 500.

use heart::query::{Query, QueryMode, Target};
use serde_json::json;

/// An unrecognised `target` variant must be rejected — unknown enum tag → 4xx,
/// the handler never sees the value.
#[test]
fn unknown_target_variant_is_rejected() {
    let result = serde_json::from_value::<Query>(json!({
        "target": "Nonsense",
        "text": "x"
    }));
    assert!(
        result.is_err(),
        "unknown target variant must not deserialize"
    );
}

/// A body with only `target` and `text` must deserialize; all other fields
/// default so that callers are not forced to enumerate every knob.
#[test]
fn minimal_body_deserializes_with_defaults() {
    let query: Query = serde_json::from_value(json!({
        "target": "Packages",
        "text": "tokio"
    }))
    .expect("minimal body must deserialize");
    assert_eq!(query.page.limit, 30, "default page limit is 30");
    assert_eq!(query.mode, QueryMode::Precise, "default mode is Precise");
    assert!(
        query.scope.ecosystems.is_empty(),
        "default scope has no ecosystem filter"
    );
}

/// `Usages` without the required `of` field must be rejected — a handler
/// receiving this would have no stable reference to walk.
#[test]
fn usages_target_requires_of() {
    let missing_of = serde_json::from_value::<Query>(json!({
        "target": { "Usages": {} },
        "text": ""
    }));
    assert!(
        missing_of.is_err(),
        "Usages without `of` must not deserialize"
    );

    // A well-formed reference does parse.
    let ok: Query = serde_json::from_value(json!({
        "target": { "Usages": { "of": "F:rust/axum#deadbeef" } },
        "text": ""
    }))
    .expect("Usages with a valid `of` must deserialize");
    assert!(
        matches!(ok.target, Target::Usages { .. }),
        "target must be Usages variant"
    );
}

/// A `StableReference` that does not match the `F:<eco>/<pkg>#<hex>` grammar
/// must be rejected — the handler can never construct a valid lookup key from it.
#[test]
fn usages_target_rejects_malformed_stable_reference() {
    let result = serde_json::from_value::<Query>(json!({
        "target": { "Usages": { "of": "not-a-ref" } },
        "text": ""
    }));
    assert!(
        result.is_err(),
        "malformed stable reference must not deserialize"
    );
}

/// A package path with slashes in the stem is valid (REGISTRYLESS-PLAN RL-10):
/// `<pkg>` may itself contain `/`; `#` is the sole terminator.
#[test]
fn slash_bearing_usages_target_parses() {
    let query: Query = serde_json::from_value(json!({
        "target": { "Usages": { "of": "F:cpp/github.com/curl/curl#deadbeef" } },
        "text": ""
    }))
    .expect("registryless slash-bearing reference must deserialize (RL-10)");
    assert!(
        matches!(query.target, Target::Usages { .. }),
        "target must be Usages variant"
    );
}

/// `mode: "semantic"` opts the caller into the dense embedding path; anything
/// else remains on the default lexical surface.
#[test]
fn semantic_mode_opt_in() {
    let query: Query = serde_json::from_value(json!({
        "target": "Symbols",
        "text": "how to parse json",
        "mode": "semantic"
    }))
    .expect("semantic mode body must deserialize");
    assert_eq!(query.mode, QueryMode::Semantic, "mode must be Semantic");
}

/// The `cursor` field is an opaque string — the handler passes it through
/// verbatim; the engine interprets its encoding.
#[test]
fn cursor_is_opaque_string() {
    let query: Query = serde_json::from_value(json!({
        "target": "Packages",
        "text": "x",
        "page": { "limit": 5, "cursor": "abc" }
    }))
    .expect("page with opaque cursor must deserialize");
    assert_eq!(
        query.page.cursor,
        Some("abc".to_string()),
        "cursor must round-trip verbatim"
    );
    assert_eq!(
        query.page.limit, 5,
        "limit must be taken from the wire value"
    );
}

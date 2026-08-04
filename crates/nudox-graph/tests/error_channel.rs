//! Tests that query errors propagate correctly through the stream.
//!
//! These are the most important tests in the crate.  They verify the
//! LR-6 contract: errors from the corpus (missing package, invalid key) must
//! surface as `Err` items in the output stream — not as panics, and not as
//! silently empty results.

use std::sync::Arc;

use futures::StreamExt as _;
use nudox_graph::adapter::{CorpusAdapter, GraphError};
use nudox_store::corpus::Corpus;
use trustfall::{Schema, execute_query_async};

fn schema() -> Schema {
    Schema::parse(include_str!("../schema.graphql")).expect("schema must parse")
}

/// Querying for a symbol whose key encodes a package that is not loaded must
/// yield a `GraphError::PackageNotLoaded` through the fork's error channel —
/// not a panic, not a silently empty stream.
///
/// The query uses `@filter(op: "=", value: ["$key"])` on the `Symbols`
/// entrypoint, which triggers the O(1) pushdown path through `Corpus::entry`.
/// The corpus is empty so `package()` returns `None`, which the adapter maps
/// to `GraphError::PackageNotLoaded`.
#[tokio::test]
async fn missing_package_yields_error() {
    // The key encodes a package ("cargo:nonexistent") that is not in the corpus.
    // The 64-char hex suffix is a valid IntroId encoding (all zeros).
    let key = "cargo:nonexistent#0000000000000000000000000000000000000000000000000000000000000000";

    let corpus = Corpus::new(); // empty — no packages loaded
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    let vars = [("key".to_owned(), trustfall::FieldValue::String(key.into()))];
    let mut stream = execute_query_async(
        &schema,
        adapter,
        "{ Symbols { key @filter(op: \"=\", value: [\"$key\"]) @output } }",
        vars.into_iter().collect(),
    )
    .expect("query must parse");

    // The stream should yield at least one item and that item must be an Err.
    let first = stream
        .next()
        .await
        .expect("stream must yield at least one item, not be empty");
    let err = first.expect_err("expected an error for a missing package, got Ok");

    // Downcast to verify the *specific* error variant is PackageNotLoaded.
    // The stream carries `Box<dyn std::error::Error>` so we check via Display.
    let msg = err.to_string();
    assert!(
        msg.contains("package not loaded"),
        "expected PackageNotLoaded error, got: {msg}"
    );
}

/// Querying for a symbol whose key is syntactically invalid (no `#` separator)
/// must yield a `GraphError::InvalidKey` — again, not a panic, not silence.
#[tokio::test]
async fn invalid_key_yields_error() {
    let corpus = Corpus::new();
    let schema = schema();
    let adapter = Arc::new(CorpusAdapter::new(corpus));

    let vars = [("key".to_owned(), trustfall::FieldValue::String("not-a-valid-key".into()))];
    let mut stream = execute_query_async(
        &schema,
        adapter,
        "{ Symbols { key @filter(op: \"=\", value: [\"$key\"]) @output } }",
        vars.into_iter().collect(),
    )
    .expect("query must parse");

    let first = stream
        .next()
        .await
        .expect("stream must yield at least one item, not be empty");
    let err = first.expect_err("expected an error for an invalid key, got Ok");

    let msg = err.to_string();
    assert!(
        msg.contains("invalid stable-ref key"),
        "expected InvalidKey error, got: {msg}"
    );
}

/// Verify the `GraphError` variants have the expected Display text.
///
/// This is a compile-time check (the variants exist) plus a runtime check
/// (the Display strings match the `#[error]` annotations).
#[test]
fn graph_error_display() {
    use nudox_ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};

    let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("foo"));
    let e = GraphError::PackageNotLoaded(lineage);
    assert!(e.to_string().contains("cargo:foo"), "PackageNotLoaded display: {e}");

    let intro = IntroId::from_raw([0u8; 32]);
    let e2 = GraphError::SymbolNotFound(intro);
    assert!(e2.to_string().contains("symbol not found"), "SymbolNotFound display: {e2}");

    let e3 = GraphError::InvalidKey("bad".to_owned());
    assert!(e3.to_string().contains("invalid stable-ref key"), "InvalidKey display: {e3}");
}

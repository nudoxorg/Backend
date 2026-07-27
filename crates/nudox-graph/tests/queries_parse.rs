//! Verify that every named `.trustfall` query file in `src/queries/` parses
//! successfully against the schema.
//!
//! The test walks the directory at compile time using `include_str!` for each
//! known file, but uses a runtime `std::fs::read_dir` scan so that adding a
//! new `.trustfall` file is automatically covered without touching this file.
//!
//! A query "parses correctly" if `execute_query_async` returns `Ok(stream)`.
//! The stream may immediately produce errors (empty corpus), but the *parse*
//! and *schema-validation* phase succeeded.

use std::{
    collections::BTreeMap,
    path::Path,
    sync::Arc,
};

use nudox_graph::adapter::CorpusAdapter;
use nudox_store::corpus::Corpus;
use trustfall::{FieldValue, Schema, execute_query_async};

fn schema() -> Schema {
    Schema::parse(include_str!("../schema.graphql")).expect("schema must parse")
}

fn empty_adapter() -> Arc<CorpusAdapter> {
    Arc::new(CorpusAdapter::new(Corpus::new()))
}

/// Dummy variable values that cover every variable name used across all
/// `.trustfall` files.  A missing variable causes a parse/validation error, so
/// we supply plausible values for every name that appears in any query.
fn all_dummy_vars() -> BTreeMap<String, FieldValue> {
    [
        ("key".to_owned(), FieldValue::String("cargo:foo#0000000000000000000000000000000000000000000000000000000000000000".into())),
        ("package".to_owned(), FieldValue::String("cargo:foo".into())),
        ("lineage".to_owned(), FieldValue::String("cargo:foo".into())),
        ("name".to_owned(), FieldValue::String("my_fn".into())),
        ("kind".to_owned(), FieldValue::String("Function".into())),
        ("query".to_owned(), FieldValue::String("foo".into())),
    ]
    .into_iter()
    .collect()
}

/// Walk `src/queries/` at runtime and assert every `.trustfall` file parses.
///
/// LR-7: this is the guard that ensures `schema.graphql` remains the single
/// source of truth — a schema incompatibility will break this test before it
/// can reach production.
#[test]
fn all_trustfall_queries_parse() {
    let schema = schema();
    let adapter = empty_adapter();

    let queries_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/queries");

    let mut found = 0usize;
    for entry in std::fs::read_dir(&queries_dir)
        .unwrap_or_else(|e| panic!("cannot read src/queries/: {e}"))
    {
        let entry = entry.unwrap_or_else(|e| panic!("dir entry error: {e}"));
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("trustfall") {
            continue;
        }
        found += 1;
        let query = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        execute_query_async(
            &schema,
            Arc::clone(&adapter),
            &query,
            all_dummy_vars(),
        )
        .unwrap_or_else(|e| {
            panic!(
                "query {} failed to parse:\n{e}\n\nQuery text:\n{query}",
                path.display()
            )
        });
    }

    assert!(
        found >= 5,
        "expected at least 5 .trustfall files in src/queries/, found {found}"
    );
}

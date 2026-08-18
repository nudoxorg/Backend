//! Verify that every named `.trustfall` query file in `src/graph/queries/` parses
//! successfully against the schema.
//!
//! The test walks the directory at compile time using `include_str!` for each
//! known file, but uses a runtime `std::fs::read_dir` scan so that adding a
//! new `.trustfall` file is automatically covered without touching this file.
//!
//! A query "parses correctly" if `execute_query_async` returns `Ok(stream)`.
//! The stream may immediately produce errors (empty corpus), but the *parse*
//! and *schema-validation* phase succeeded.

use std::{collections::BTreeMap, path::Path, sync::Arc};

use nudox_engine::graph::adapter::CorpusAdapter;
use nudox_engine::store::corpus::Corpus;
use trustfall::{FieldValue, Schema, execute_query_async};

fn schema() -> Schema {
    Schema::parse(include_str!("../../schema.graphql")).expect("schema must parse")
}

fn empty_adapter() -> Arc<CorpusAdapter> {
    Arc::new(CorpusAdapter::new(Corpus::new()))
}

/// A plausible value for every variable name any query might reference.
fn dummy_value(name: &str) -> FieldValue {
    match name {
        "key" => FieldValue::String(
            "cargo:foo#0000000000000000000000000000000000000000000000000000000000000000".into(),
        ),
        "package" | "lineage" => FieldValue::String("cargo:foo".into()),
        "kind" => FieldValue::String("Function".into()),
        // Anything else is a name-ish string; queries filter on strings.
        _ => FieldValue::String("my_fn".into()),
    }
}

/// The variables a specific query actually references.
///
/// Supplying the union of all variables to every query does not work: Trustfall
/// rejects arguments a query does not use ("One or more of the provided
/// arguments are not used in this query"), and rightly so — an unused argument
/// is almost always a typo'd variable name that would otherwise pass silently.
///
/// So we scan the query text for `$ident` and supply exactly those. This also
/// means adding a query with a new variable needs no edit here.
fn vars_for(query: &str) -> BTreeMap<String, FieldValue> {
    let mut names: Vec<String> = Vec::new();
    let bytes = query.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && (bytes[end].is_ascii_alphanumeric() || bytes[end] == b'_') {
                end += 1;
            }
            if end > start {
                names.push(query[start..end].to_owned());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    names.sort();
    names.dedup();
    names
        .into_iter()
        .map(|n| {
            let v = dummy_value(&n);
            (n, v)
        })
        .collect()
}

/// Walk `src/graph/queries/` at runtime and assert every `.trustfall` file parses.
///
/// LR-7: this is the guard that ensures `schema.graphql` remains the single
/// source of truth — a schema incompatibility will break this test before it
/// can reach production.
#[test]
fn all_trustfall_queries_parse() {
    let schema = schema();
    let adapter = empty_adapter();

    let queries_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/graph/queries");

    let mut found = 0usize;
    for entry in std::fs::read_dir(&queries_dir)
        .unwrap_or_else(|e| panic!("cannot read src/graph/queries/: {e}"))
    {
        let entry = entry.unwrap_or_else(|e| panic!("dir entry error: {e}"));
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("trustfall") {
            continue;
        }
        found += 1;
        let query = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        let _stream = execute_query_async(&schema, Arc::clone(&adapter), &query, vars_for(&query))
            .unwrap_or_else(|e| {
                panic!(
                    "query {} failed to parse:\n{e}\n\nQuery text:\n{query}",
                    path.display()
                )
            });
    }

    assert!(
        found >= 5,
        "expected at least 5 .trustfall files in src/graph/queries/, found {found}"
    );
}

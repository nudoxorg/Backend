//! Regression test: the `key`-equality pushdown must not enumerate packages
//! it doesn't need.
//!
//! Test strategy: load two packages into a corpus, execute a `Symbols` query
//! that filters to a key belonging to *one* of them, and assert that the
//! full-scan counter (`packages_scanned`) is zero — i.e. the adapter resolved
//! the query through `Corpus::entry` (O(1) path) without calling `packages()`
//! at all.
//!
//! If the pushdown silently regresses to a scan, `packages_scanned` will be
//! ≥ 2 (one per package) and the assertion will fail.

use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

use futures::StreamExt as _;
use nudox_graph::adapter::CorpusAdapter;
use nudox_store::{
    corpus::Corpus,
    package::{PackageView, Provenance},
};
use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::{Entry, Node, Symbol, Visibility},
    kind::Kind,
    kinds::Function,
    view::IrView,
};
use trustfall::{FieldValue, Schema, execute_query_async};

fn schema() -> Schema {
    Schema::parse(include_str!("../schema.graphql")).expect("schema must parse")
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn lineage(eco: &str, name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new(eco), PackageName::new(name))
}

fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn make_package(eco: &str, name: &str) -> Arc<PackageView> {
    let lid = lineage(eco, name);
    let mut table = PristineIntroTable::new();
    table.insert_live(
        intro(1),
        Entry::new(sym("my_fn"), Node::build(None::<nudox_ir::index::RawRef>, []), Kind::Function(Function::builder().build())),
        None,
    );
    let view = IrView::with_package(lid, table);
    Arc::new(PackageView::build(view, Provenance::TrustedLocal))
}

/// Asserts that a `@filter(op: "=", value: ["$key"])` on the `Symbols`
/// entrypoint never calls `Corpus::packages()` (i.e. `packages_scanned == 0`)
/// even when two packages are loaded, because the adapter resolves through
/// `Corpus::entry` instead of enumerating.
#[tokio::test]
async fn key_filter_does_not_scan_second_package() {
    let counter = Arc::new(AtomicUsize::new(0));

    let corpus = Corpus::new();
    // Load TWO packages.
    corpus.insert(make_package("cargo", "pkg-alpha")).await;
    corpus.insert(make_package("cargo", "pkg-beta")).await;

    let schema = schema();
    // The adapter is wired with a probe counter.  The counter is only
    // incremented on the full-scan path inside `Symbols`; it is never
    // incremented on the `key` pushdown path.
    let adapter = Arc::new(CorpusAdapter::new_with_counter(corpus, Arc::clone(&counter)));

    // Build a key that belongs to pkg-alpha (so the query can actually resolve).
    let key = StableRef::new(lineage("cargo", "pkg-alpha"), intro(1)).to_string();

    let vars = [("key".to_owned(), FieldValue::String(key.into()))];
    let _results: Vec<_> = execute_query_async(
        &schema,
        adapter,
        "{ Symbols { key @filter(op: \"=\", value: [\"$key\"]) @output } }",
        vars.into_iter().collect(),
    )
    .expect("query must parse")
    .collect()
    .await;

    // The counter must still be zero: the pushdown bypassed `packages()`.
    let scanned = counter.load(Ordering::Relaxed);
    assert_eq!(
        scanned, 0,
        "expected 0 packages scanned via pushdown, but counter = {scanned} (regression: key filter degraded to full scan)"
    );
}

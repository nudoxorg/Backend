//! Pipeline part: **end-to-end** (compile → blob → index → search).
//!
//! The integration spec that ties every subsystem together: tracking a package
//! should make its symbols searchable across all three surfaces.

/// A tracked package becomes searchable by name after sync.
///
/// Arrange: add a small fixture package and wait for sync to finish.
/// Act: a precise (text) search for one of its symbols.
/// Assert: the symbol is returned with its package/version.
#[tokio::test]
async fn tracked_package_is_text_searchable() {
    todo!("assert end-to-end: add -> sync -> text search");
}

/// The same package is reachable via semantic search.
///
/// Assert: a natural-language / code-snippet query surfaces the package's
///   symbols via the vector store.
#[tokio::test]
async fn tracked_package_is_semantically_searchable() {
    todo!("assert end-to-end semantic search");
}

/// The same package is reachable via graph expansion.
///
/// Assert: expanding one of its symbols surfaces related symbols from the graph
///   store.
#[tokio::test]
async fn tracked_package_is_graph_expandable() {
    todo!("assert end-to-end graph expansion");
}

/// The package is parsed once and fanned out to every sink.
///
/// Assert: after one sync, the text index, vector store, graph store, and blob
///   store all reflect the package (single projection, multiple sinks).
#[tokio::test]
async fn package_is_parsed_once_and_fanned_out() {
    todo!("assert parse-once / fan-out across all sinks");
}

/// A deferred external-library symbol becomes searchable after its lib resolves.
///
/// Assert: a symbol referencing an as-yet-unindexed library is deferred, then
///   becomes searchable once that library is registered/resolved.
#[tokio::test]
async fn deferred_external_symbol_resolves_later() {
    todo!("assert deferred-then-resolved external symbol flow");
}

//! Diagram: **postgres also handles search for the registry**, and **tantivy is
//! the abstraction over the information we get from postgres for registry
//! search, isolating the multi-parent setup.**
//!
//! TDD specs (`todo!()`) for `registry::search`.

/// Registry search finds packages by name/metadata.
///
/// Assert: searching the registry returns matching packages (not symbols).
#[tokio::test]
async fn registry_search_finds_packages() {
    todo!("assert package-level registry search");
}

/// The tantivy registry index is derived from postgres.
///
/// Assert: a package added to postgres becomes searchable once the tantivy
///   abstraction has synced from it.
#[tokio::test]
async fn tantivy_index_is_derived_from_postgres() {
    todo!("assert tantivy registry index syncs from postgres");
}

/// Multi-parent packages present as a single result.
///
/// Arrange: a package reachable through two parents/sources.
/// Assert: registry search returns ONE coherent entry, hiding the multi-parent
///   fan-in.
#[tokio::test]
async fn multi_parent_packages_collapse_to_one_result() {
    todo!("assert multi-parent isolation -> single result");
}

/// Registry search respects source/access scoping.
///
/// Assert: results are limited to sources the caller may see (ties into
///   `heart::access`).
#[tokio::test]
async fn registry_search_respects_access_scope() {
    todo!("assert access-scoped registry search");
}

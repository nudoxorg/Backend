//! Diagram: **postgres is in charge of the health of a particular project and
//! whether it's been parsed, etc.**
//!
//! TDD specs (`todo!()`) for `registry::health`.

/// A new project is unparsed / pending.
///
/// Assert: health starts as not-yet-parsed.
#[tokio::test]
async fn new_project_is_unparsed() {
    todo!("assert initial unparsed health");
}

/// A successfully indexed project reports healthy + parsed + loaded.
///
/// Assert: after indexing, health is parsed and loaded into the runtime stores.
#[tokio::test]
async fn indexed_project_is_healthy_and_loaded() {
    todo!("assert healthy/parsed/loaded after indexing");
}

/// A failed parse reports degraded with the error recorded.
///
/// Assert: a parse failure flips health to degraded and stores the reason.
#[tokio::test]
async fn failed_parse_is_degraded() {
    todo!("assert degraded health on parse failure");
}

/// Health reflects whether a project is loaded into the runtime stores.
///
/// Assert: a project parsed but not yet loaded is distinguishable from a fully
///   loaded one.
#[tokio::test]
async fn health_distinguishes_parsed_from_loaded() {
    todo!("assert parsed-vs-loaded distinction in health");
}

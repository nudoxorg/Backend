//! Pipeline part: **registry read/write** (`registry::{Registry, RegistryWrite, ReadOnly, ReadWrite}`).
//!
//! TDD specs for publishing into a registry we own and reading/searching a
//! registry we pull from.

/// Publishing a versioned payload yields a global package.
///
/// Act: `RegistryWrite::<ReadWrite>::publish(Versioned { version, payload })`.
/// Assert: returns a `GlobalPackage` with a stable global id, ready to syndicate
///   to the global store.
#[tokio::test]
async fn publish_returns_a_global_package() {
    todo!("assert publish() mints a GlobalPackage");
}

/// Modifying an already-published package re-mints the global package.
///
/// Assert: `modify(payload, id)` updates name/description/yank and returns the
///   newly minted `GlobalPackage`, signaling dependent infra.
#[tokio::test]
async fn modify_remints_the_global_package() {
    todo!("assert modify() updates state + returns a new GlobalPackage");
}

/// Ranking policy changes are applied to the engine.
///
/// Assert: `rank(policy)` succeeds and subsequent searches reflect the new
///   ordering.
#[tokio::test]
async fn rank_applies_a_ranking_policy() {
    todo!("assert rank() changes search ordering");
}

/// A read registry lists and gets packages by condition.
///
/// Assert: `Registry::<ReadOnly>::list(&conditions)` returns the matching packages and
///   `get(&package)` fetches one.
#[tokio::test]
async fn read_registry_lists_and_gets() {
    todo!("assert list()/get() over a read registry");
}

/// A read registry searches beyond exact naming.
///
/// Assert: `search(&conditions)` returns relevant packages for a non-exact query
///   (the "non-obvious presumptions" requirement).
#[tokio::test]
async fn read_registry_searches_semantically() {
    todo!("assert search() returns non-exact matches");
}

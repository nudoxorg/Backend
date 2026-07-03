//! Pipeline part: **deterministic global identity** (`registry::identity`).
//!
//! TDD specs for the version-agnostic `SymbolId` derivation. The whole
//! point is that any system recomputes the same id offline — never random.

/// The same instance + entry URI always yields the same id.
///
/// Act: `compute(instance, uri)` twice.
/// Assert: both calls return the identical `SymbolId` (UUID v5).
#[test]
fn id_is_deterministic() {
    todo!("assert compute() is stable for the same inputs");
}

/// Different entry URIs yield different ids.
#[test]
fn different_uris_give_different_ids() {
    todo!("assert distinct URIs -> distinct ids");
}

/// Different terminus instances yield different ids for the same URI.
///
/// Assert: the instance is salted into the hash (so `org-a/db` and `org-b/db`
///   diverge for the same symbol).
#[test]
fn different_instances_give_different_ids() {
    todo!("assert the instance salts the id");
}

/// The id matches the canonical NUL-separated v5 formula.
///
/// Assert: `compute(instance, uri)` equals
///   `UUIDv5(NUDOX_SYMBOL_NS, "{instance}\0{uri}")` — guarding the layering so
///   external systems can reproduce it.
#[test]
fn id_matches_the_canonical_formula() {
    todo!("assert compute() == v5(NS, instance\\0uri)");
}

/// Entry URIs are stable round-trips (parse ∘ display == identity).
///
/// Assert: building an `EntryUri` then parsing its string form yields the same
///   URI, including Rust crate-slug canonicalization.
#[test]
fn entry_uri_round_trips() {
    todo!("assert EntryUri parse/display round-trip");
}

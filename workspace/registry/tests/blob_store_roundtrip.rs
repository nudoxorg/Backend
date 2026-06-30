//! Pipeline part: **the registry store** (`registry::store`).
//!
//! TDD specs for the package-addressed durable store over `object_store`
//! (S3 / local). The system is built around `Package`: you `put`/`get` a
//! package's parsed blob *by the package*, with the object location derived from
//! its coordinates — there is no opaque ref to round-trip.

/// A package's blob round-trips: what you put is what you get back.
///
/// Act: `store.put(&package, &blob)` then `store.get(&package)`.
/// Assert: the retrieved blob equals the stored one.
#[tokio::test]
async fn put_then_get_round_trips() {
    todo!("assert put/get round-trips a package's blob");
}

/// The location is derived deterministically from the package coordinates.
///
/// Assert: two packages with different language/name/version land at different
///   object-store paths; the same package always maps to the same path.
#[tokio::test]
async fn location_is_derived_from_package_coordinates() {
    todo!("assert deterministic, collision-free package locations");
}

/// Re-putting the same package overwrites in place (no duplicate objects).
///
/// Assert: putting a package twice leaves a single object at its location.
#[tokio::test]
async fn reput_overwrites_in_place() {
    todo!("assert idempotent put at the package's location");
}

/// Getting a package that was never stored reports a backend not-found.
#[tokio::test]
async fn get_unknown_package_errors() {
    todo!("assert StoreError::Backend(NotFound) for an unstored package");
}

/// A re-parse with a changed representation is detected via the freshness hash.
///
/// Assert: `metadata::hash::freshness(recorded, recomputed)` is `Stale` when the
///   code/treesitter hash changed, `Fresh` when it didn't — the signal that
///   gates pulling a library down again.
#[tokio::test]
async fn changed_representation_is_stale() {
    todo!("assert freshness Stale on changed hash, Fresh otherwise");
}

/// Packages persist across a store reopen (S3/local durability).
///
/// Assert: after reopening the store over the same backend, a previously-put
///   package's blob is still retrievable.
#[tokio::test]
async fn packages_persist_across_reopen() {
    todo!("assert durability across store reopen");
}

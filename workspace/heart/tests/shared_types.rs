//! Pipeline part: **shared generic types** (`heart`).
//!
//! TDD specs for the cross-cutting primitives every subsystem leans on.

/// `Id<T>` compares and hashes by its inner UUID, ignoring the type tag.
///
/// Assert: two `Id<T>` over the same UUID are equal and hash equal; the phantom
///   type does not affect identity.
#[test]
fn id_equality_ignores_the_type_tag() {
    todo!("assert Id equality/hashing by inner uuid");
}

/// `Hit<T>` carries a value and an optional score.
#[test]
fn hit_pairs_value_with_optional_score() {
    todo!("assert Hit carries value + optional score");
}

/// `Versioned<T>` pairs an object with its version.
#[test]
fn versioned_pairs_object_with_version() {
    todo!("assert Versioned pairs object with version");
}

/// `Progressive::is_complete` is true once progress reaches 100.
#[test]
fn progressive_completes_at_one_hundred() {
    todo!("assert is_complete() at >= 100");
}

/// `Sink::deliver` retries transient failures via the configured backoff.
///
/// Assert: a sink whose `retryable` returns true is retried until it succeeds;
///   a non-retryable error is surfaced immediately.
#[tokio::test]
async fn sink_deliver_retries_transient_failures() {
    todo!("assert Sink::deliver retry semantics");
}

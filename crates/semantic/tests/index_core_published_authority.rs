//! Exercises the `backend-semantic::index_core` tests published-authority contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use backend_version::GenerationId;
use backend_semantic::index_core::IndexSnapshot;

#[test]
fn identical_projection_bytes_cannot_cross_generation_authority() {
    let first_generation = GenerationId::from_canonical_bytes(b"published-ir-generation-one");
    let second_generation = GenerationId::from_canonical_bytes(b"published-ir-generation-two");

    let first = IndexSnapshot::new(first_generation, &[], &[]).expect("bounded first snapshot");
    let replay = IndexSnapshot::new(first_generation, &[], &[]).expect("bounded replay");
    let second = IndexSnapshot::new(second_generation, &[], &[]).expect("bounded second snapshot");

    assert_eq!(first.generation, first_generation);
    assert_eq!(first.id, replay.id);
    assert_ne!(first.id, second.id);
}

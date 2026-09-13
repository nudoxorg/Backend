//! Exercises the `server-index-publish` tests compiler-snapshot contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
mod journey;

#[test]
fn ir_publishes_reopens_and_seals_to_real_segments() -> Result<(), journey::TestError> {
    journey::run()
}

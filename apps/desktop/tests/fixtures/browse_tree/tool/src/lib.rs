//! Compile-fail checks, run on demand.

/// Runs every `tests/ui/*.rs` case.
pub fn check() {
    trybuild::TestCases::new().compile_fail("tests/ui/*.rs");
}

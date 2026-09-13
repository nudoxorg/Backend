//! Exercises the `heart-hydration` tests compile-fail contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
//! Compile-time authority boundary for verified generation closure.

#[test]
fn verified_generation_cannot_be_forged() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}

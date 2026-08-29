//! Compile-time authority boundary for verified generation closure.

#[test]
fn verified_generation_cannot_be_forged() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}

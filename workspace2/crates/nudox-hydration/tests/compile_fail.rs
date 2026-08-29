//! Compile-time authority boundaries for hydration publication.

#[test]
fn readiness_witness_cannot_be_forged() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/*.rs");
}

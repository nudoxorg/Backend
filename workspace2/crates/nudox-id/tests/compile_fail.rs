#[test]
fn duplicate_closed_registry_codes_fail_at_enum_construction() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/duplicate_domain_code.rs");
    tests.compile_fail("tests/ui/duplicate_encoding_code.rs");
}

//! Compile-fail proofs for the closed identity marker registry.
//! Duplicate durable codes and cross-domain authority binding must be rejected by rustc itself.
//! These fixtures defend invariants that runtime validation should never need to rediscover.

#[test]
/// Compiles every negative fixture and requires the intended type or discriminant error.
fn duplicate_closed_registry_codes_fail_at_enum_construction() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/duplicate_domain_code.rs");
    tests.compile_fail("tests/ui/duplicate_encoding_code.rs");
    tests.compile_fail("tests/ui/cross_domain_authority_bind.rs");
}

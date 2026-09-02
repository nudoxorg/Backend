//! Compile-fail proofs for the TypeScript source-coordinate boundary.
//! OXC byte spans and TSZ UTF-16 spans must never substitute for each other.
//! Successful compilation would expose a cross-unit coordinate substitution vulnerability.

#[test]
/// Compiles the negative fixture and requires the intended type mismatch.
fn utf8_byte_spans_are_not_utf16_authority_spans() {
    let tests = trybuild::TestCases::new();
    tests.compile_fail("tests/ui/utf8_span_is_not_a_utf16_source_span.rs");
}

//! Chief-owned public compiler and publication falsifiers.

use nudox_compile_driver::{CompileFailure, CompileRequest, CompileScratch, compile};
use nudox_compile_vocab::Language;

#[test]
fn distinct_equal_shape_declarations_produce_distinct_semantic_ir() -> Result<(), CompileFailure> {
    let alpha_source = b"pub fn alpha() {}";
    let bravo_source = b"pub fn bravo() {}";
    assert_eq!(alpha_source.len(), bravo_source.len());

    let mut alpha_output = [0; 256];
    let mut bravo_output = [0; 256];
    let alpha = compile(
        CompileRequest {
            language: Language::Rust,
            source: alpha_source,
        },
        CompileScratch {
            fragment_output: &mut alpha_output,
        },
    )?;
    let bravo = compile(
        CompileRequest {
            language: Language::Rust,
            source: bravo_source,
        },
        CompileScratch {
            fragment_output: &mut bravo_output,
        },
    )?;

    assert_ne!(alpha.source.digest, bravo.source.digest);
    assert_ne!(alpha.fragment.as_ref(), bravo.fragment.as_ref());
    Ok(())
}

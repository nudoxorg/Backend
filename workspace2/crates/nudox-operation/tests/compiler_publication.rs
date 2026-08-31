//! Chief-owned public compiler and publication falsifiers.

use nudox_compile_driver::{CompileFailure, CompileRequest, CompileScratch, compile};
use nudox_compile_vocab::Language;
use nudox_ir_format::{EntityKind, PrimitiveType, TypeNode};

#[test]
fn distinct_equal_shape_declarations_produce_distinct_semantic_ir() -> Result<(), CompileFailure> {
    let alpha_source = b"pub const alpha: bool = true;";
    let bravo_source = b"pub const bravo: i32 = 1    ;";
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

    assert_ne!(alpha.source.identity, bravo.source.identity);
    assert_ne!(alpha.fragment.as_ref(), bravo.fragment.as_ref());
    assert!(
        alpha
            .fragment
            .entities()
            .map(|entity| (entity.kind, entity.name.raw))
            .eq([(EntityKind::Constant, 0)])
    );
    assert!(
        bravo
            .fragment
            .entities()
            .map(|entity| (entity.kind, entity.name.raw))
            .eq([(EntityKind::Constant, 0)])
    );
    assert!(
        alpha
            .fragment
            .atoms()
            .map(|atom| atom.bytes)
            .eq([b"alpha".as_slice()])
    );
    assert!(
        bravo
            .fragment
            .atoms()
            .map(|atom| atom.bytes)
            .eq([b"bravo".as_slice()])
    );
    assert!(
        alpha
            .fragment
            .type_nodes()
            .eq([TypeNode::Primitive(PrimitiveType::Bool)])
    );
    assert!(
        bravo
            .fragment
            .type_nodes()
            .eq([TypeNode::Primitive(PrimitiveType::I32)])
    );
    Ok(())
}

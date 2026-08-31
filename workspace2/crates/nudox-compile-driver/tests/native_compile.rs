use nudox_compile_driver::{CompileFailure, CompileRequest, CompileScratch, NativeTool, compile};
use nudox_compile_vocab::Language;

#[test]
fn native_adapters_parse_real_source_before_lending_compact_ir() -> Result<(), CompileFailure> {
    let cases = [
        (Language::Rust, b"pub const RUST_VALID: u8 = 1;".as_slice()),
        (Language::Python, b"PYTHON_VALID = 1\n".as_slice()),
        (
            Language::Clang,
            b"const char *clang_valid = \"yes\";".as_slice(),
        ),
    ];
    for (language, source) in cases {
        let mut output = [0xa5; 128];
        let output_pointer = output.as_ptr();
        let fragment_len = {
            let compiled = compile(
                CompileRequest { language, source },
                CompileScratch {
                    fragment_output: &mut output,
                },
            )?;
            assert_eq!(compiled.language, language);
            assert_eq!(
                compiled.source.byte_len,
                u32::try_from(source.len()).unwrap_or(u32::MAX)
            );
            assert!(core::ptr::eq(
                compiled.fragment.as_ref().as_ptr(),
                output_pointer
            ));
            assert_eq!(compiled.fragment.entities().count(), 1);
            assert_eq!(compiled.fragment.type_nodes().count(), 1);
            compiled.fragment.as_ref().len()
        };
        assert!(output[fragment_len..].iter().all(|byte| *byte == 0xa5));
    }
    Ok(())
}

#[test]
fn syntax_rejection_retains_the_exact_source_identity() {
    let source = b"pub const = ;";
    let mut output = [0xa5; 128];
    let failure = compile(
        CompileRequest {
            language: Language::Rust,
            source,
        },
        CompileScratch {
            fragment_output: &mut output,
        },
    );
    match failure {
        Err(CompileFailure::NativeRejected {
            language,
            tool,
            source_identity: rejected,
            ..
        }) => {
            assert_eq!(language, Language::Rust);
            assert_eq!(tool, NativeTool::Rustc);
            assert_eq!(
                rejected.byte_len,
                u32::try_from(source.len()).unwrap_or(u32::MAX)
            );
            assert!(output.iter().all(|byte| *byte == 0xa5));
        }
        _ => panic!("expected a Rust native rejection"),
    }
}

#[test]
fn unavailable_tooling_is_an_explicit_typed_terminal() {
    let source = b"export const unavailable: number = 1;";
    let mut output = [0xa5; 128];
    assert!(matches!(
        compile(
            CompileRequest {
                language: Language::TypeScript,
                source,
            },
            CompileScratch {
                fragment_output: &mut output,
            },
        ),
        Err(CompileFailure::ToolingUnavailable {
            language: Language::TypeScript,
            tool: NativeTool::TypeScriptCompiler,
            source_identity: rejected,
        }) if rejected.byte_len == u32::try_from(source.len()).unwrap_or(u32::MAX)
    ));
    assert!(output.iter().all(|byte| *byte == 0xa5));
}

#[test]
fn source_identity_rejects_an_input_ignoring_compile_mutant() -> Result<(), CompileFailure> {
    let first_source = b"pub const FIRST: u8 = 1;";
    let second_source = b"pub const THIRD: u8 = 1;";
    let mut first_output = [0; 128];
    let mut second_output = [0; 128];
    let first = compile(
        CompileRequest {
            language: Language::Rust,
            source: first_source,
        },
        CompileScratch {
            fragment_output: &mut first_output,
        },
    )?;
    let second = compile(
        CompileRequest {
            language: Language::Rust,
            source: second_source,
        },
        CompileScratch {
            fragment_output: &mut second_output,
        },
    )?;
    assert_eq!(first.source.byte_len, second.source.byte_len);
    assert_ne!(first.source.digest, second.source.digest);
    Ok(())
}

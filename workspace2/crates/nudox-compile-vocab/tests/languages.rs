use nudox_compile_vocab::{Language, MAX_NATIVE_DIAGNOSTIC_BYTES, NativeTool};

#[test]
fn compiler_corpus_language_families_are_closed_and_complete() {
    assert_eq!(
        Language::ALL,
        [
            Language::Rust,
            Language::TypeScript,
            Language::Python,
            Language::Go,
            Language::Java,
            Language::CSharp,
            Language::Clang,
        ]
    );
}

#[test]
fn compiler_native_tool_families_are_closed_and_canonically_ordered() {
    assert_eq!(
        NativeTool::ALL,
        [
            NativeTool::Rustc,
            NativeTool::Clang,
            NativeTool::Python,
            NativeTool::TypeScriptCompiler,
            NativeTool::GoCompiler,
            NativeTool::JavaCompiler,
            NativeTool::CSharpCompiler,
        ]
    );
}

#[test]
fn compiler_native_diagnostic_retention_has_one_closed_portable_capacity() {
    assert_eq!(MAX_NATIVE_DIAGNOSTIC_BYTES, 256);
}

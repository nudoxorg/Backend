//! Contract tests for the closed language, tool, and diagnostic-capacity vocabulary.
//! Literal arrays pin canonical order because recipe reports depend on that order remaining stable.
//! Capacity assertions prevent transport adapters from silently selecting divergent limits.
use compiler_vocabulary::{Language, MAX_NATIVE_DIAGNOSTIC_BYTES, NativeTool};

#[test]
/// Pins the complete seven-language schedule and its canonical iteration order.
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
/// Pins the one-to-one native-tool schedule used by compiler capability reports.
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
/// Pins the single portable diagnostic retention limit shared by every frontend.
fn compiler_native_diagnostic_retention_has_one_closed_portable_capacity() {
    assert_eq!(MAX_NATIVE_DIAGNOSTIC_BYTES, 256);
}

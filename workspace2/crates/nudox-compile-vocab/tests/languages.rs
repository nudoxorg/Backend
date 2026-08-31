use nudox_compile_vocab::Language;

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

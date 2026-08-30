//! Deterministic corpus oracle for the eventual full product journey.

mod support;

use support::multilingual_corpus::{
    CorpusLanguage, CorpusPackage, CorpusRenderError, PACKAGE_COUNT, SOURCE_BYTE_LIMIT,
    corpus_packages,
};

const EXPECTED_PACKAGES_PER_LANGUAGE: usize = 51;
const EXPECTED_PACKAGE_COUNT: usize = 204;

#[test]
fn corpus_has_two_hundred_four_stable_multilingual_packages() -> Result<(), CorpusRenderError> {
    let packages = corpus_packages();

    assert_eq!(packages.len(), EXPECTED_PACKAGE_COUNT);
    assert_eq!(PACKAGE_COUNT, EXPECTED_PACKAGE_COUNT);
    let language_counts = verify_rendered_packages(packages)?;
    for language in CorpusLanguage::ALL {
        assert_eq!(
            language_counts.count(language),
            EXPECTED_PACKAGES_PER_LANGUAGE
        );
    }

    Ok(())
}

#[test]
fn corpus_rendering_preflights_exact_output_without_writing() {
    const EXACT_RUST_SOURCE_BYTES: usize = 42;
    let package = CorpusPackage {
        ordinal: 0,
        language: CorpusLanguage::Rust,
    };
    let mut output = [0xa5; EXACT_RUST_SOURCE_BYTES - 1];

    assert_eq!(
        package.render(&mut output),
        Err(CorpusRenderError::InsufficientOutput {
            required: EXACT_RUST_SOURCE_BYTES,
            available: EXACT_RUST_SOURCE_BYTES - 1,
        })
    );
    assert!(output.iter().all(|byte| *byte == 0xa5));
}

#[test]
fn corpus_rendering_has_stable_language_source_and_symbol_bytes() -> Result<(), CorpusRenderError> {
    let package = CorpusPackage {
        ordinal: 0,
        language: CorpusLanguage::Rust,
    };
    let mut output = [0xa5; SOURCE_BYTE_LIMIT];

    let rendered = package.render(&mut output)?;
    assert_eq!(
        rendered.source,
        "pub const package_000: &str = \"rust-000\";\n"
    );
    assert_eq!(rendered.expected_symbol, "package_000");
    Ok(())
}

fn verify_rendered_packages(
    packages: impl Iterator<Item = CorpusPackage>,
) -> Result<LanguageCounts, CorpusRenderError> {
    let mut language_counts = LanguageCounts::default();
    let mut source_output = [0xa5; SOURCE_BYTE_LIMIT];
    let mut observed = 0;
    for package in packages {
        language_counts.observe(package.language);
        assert_eq!(package.ordinal, observed);
        let rendered_len = {
            let rendered = package.render(&mut source_output)?;
            assert!(rendered.source.contains(rendered.expected_symbol));
            assert!(!rendered.expected_symbol.is_empty());
            assert!(rendered.source.len() <= SOURCE_BYTE_LIMIT);
            rendered.source.len()
        };
        assert!(
            source_output
                .get(rendered_len..)
                .is_some_and(|tail| tail.iter().all(|byte| *byte == 0xa5))
        );
        source_output.fill(0xa5);
        observed += 1;
    }
    assert_eq!(observed, EXPECTED_PACKAGE_COUNT);
    Ok(language_counts)
}

#[derive(Default)]
struct LanguageCounts {
    rust: usize,
    typescript: usize,
    python: usize,
    csharp: usize,
}

impl LanguageCounts {
    const fn observe(&mut self, language: CorpusLanguage) {
        match language {
            CorpusLanguage::Rust => self.rust += 1,
            CorpusLanguage::TypeScript => self.typescript += 1,
            CorpusLanguage::Python => self.python += 1,
            CorpusLanguage::CSharp => self.csharp += 1,
        }
    }

    const fn count(&self, language: CorpusLanguage) -> usize {
        match language {
            CorpusLanguage::Rust => self.rust,
            CorpusLanguage::TypeScript => self.typescript,
            CorpusLanguage::Python => self.python,
            CorpusLanguage::CSharp => self.csharp,
        }
    }
}

//! Exercises public C and C++ collector contracts without assuming a host libclang installation.
//! The tests prove canonical profile admission and pre-native source rejection deterministically.
//! Live direct-authority fixtures belong to a provisioned libclang environment, never an ignored test.

use compiler_languages_clang::{ClangInput, ClangScratch, CollectError, collect};
use compiler_vocabulary::{CStandard, LanguageProfile, RustEdition};
use core::sync::atomic::AtomicBool;

/// Holds the exact public error fact required by one collector boundary test.
#[derive(Debug, thiserror::Error)]
enum TestError {
    /// A collector result differed from the required closed public failure.
    #[error("unexpected collector result: {0:?}")]
    Unexpected(CollectError),
    /// A canonical profile narrowing result differed from its required semantic variant.
    #[error("canonical profile narrowing changed the C standard")]
    Profile,
}

#[test]
fn canonical_c_profile_is_preserved_without_filename_inference() -> Result<(), TestError> {
    let input = ClangInput::from_profile(
        c"translation.c",
        b"int main(void) { return 0; }",
        LanguageProfile::C(CStandard::C23),
    )
    .map_err(|_| TestError::Profile)?;
    match input {
        ClangInput::C {
            standard: CStandard::C23,
            ..
        } => Ok(()),
        _ => Err(TestError::Profile),
    }
}

#[test]
fn non_clang_canonical_profile_is_retained_as_a_typed_rejection() -> Result<(), TestError> {
    match ClangInput::from_profile(
        c"translation.rs",
        b"fn main() {}",
        LanguageProfile::Rust(RustEdition::Rust2024),
    ) {
        Err(error) if error.profile == LanguageProfile::Rust(RustEdition::Rust2024) => Ok(()),
        Err(_) | Ok(_) => Err(TestError::Profile),
    }
}

#[test]
fn nul_source_rejects_before_native_loading_or_any_scanner_fallback() -> Result<(), TestError> {
    let mut declarations = [];
    let mut types = [];
    let mut type_edges = [];
    let mut references = [];
    let mut diagnostics = [];
    let mut includes = [];
    let mut overrides = [];
    let input = ClangInput::C {
        file_name: c"translation.c",
        source: b"int\0main(void);",
        standard: CStandard::C23,
    };
    let scratch = ClangScratch {
        declarations: &mut declarations,
        types: &mut types,
        type_edges: &mut type_edges,
        references: &mut references,
        diagnostics: &mut diagnostics,
        includes: &mut includes,
        overrides: &mut overrides,
    };
    match collect(input, scratch) {
        Err(CollectError::SourceContainsNul) => Ok(()),
        Err(error) => Err(TestError::Unexpected(error)),
        Ok(_) => Err(TestError::Profile),
    }
}

#[test]
fn cancelled_collection_does_not_load_native_authority_or_fallback() -> Result<(), TestError> {
    let cancellation = AtomicBool::new(true);
    let mut declarations = [];
    let mut types = [];
    let mut type_edges = [];
    let mut references = [];
    let mut diagnostics = [];
    let mut includes = [];
    let mut overrides = [];
    let input = ClangInput::C {
        file_name: c"translation.c",
        source: b"int main(void);",
        standard: CStandard::C23,
    };
    let scratch = ClangScratch {
        declarations: &mut declarations,
        types: &mut types,
        type_edges: &mut type_edges,
        references: &mut references,
        diagnostics: &mut diagnostics,
        includes: &mut includes,
        overrides: &mut overrides,
    };
    match compiler_languages_clang::collect_cancellable(input, scratch, &cancellation) {
        Err(CollectError::Cancelled) => Ok(()),
        Err(error) => Err(TestError::Unexpected(error)),
        Ok(_) => Err(TestError::Profile),
    }
}

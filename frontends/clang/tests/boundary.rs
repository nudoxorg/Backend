//! Exercises public C and C++ collector contracts without assuming a host libclang installation.
//! The tests prove canonical profile admission and pre-native source rejection deterministically.
//! Live direct-authority fixtures belong to a provisioned libclang environment, never an ignored test.

use backend_frontend_clang::legacy::{
    ClangInput, ClangScratch, CollectError, DatabaseError, MAX_DATABASE_ARGUMENTS, collect,
};
use backend_semantic::vocabulary::{CStandard, LanguageProfile, RustEdition};
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
    match backend_frontend_clang::legacy::collect_cancellable(input, scratch, &cancellation) {
        Err(CollectError::Cancelled) => Ok(()),
        Err(error) => Err(TestError::Unexpected(error)),
        Ok(_) => Err(TestError::Profile),
    }
}

#[test]
fn database_argument_capacity_rejects_without_truncation() {
    let values = [c"-DVALUE=1"; MAX_DATABASE_ARGUMENTS + 1];
    let result = ClangInput::from_database(c"main.c", b"int main;", &values, c".");
    match result {
        Err(error) => {
            assert_eq!(error.required, MAX_DATABASE_ARGUMENTS + 1);
            assert_eq!(error.capacity, MAX_DATABASE_ARGUMENTS);
        }
        Ok(_) => assert!(false, "over-capacity command was accepted"),
    }
}

#[test]
fn absent_database_is_an_explicit_typed_terminal() {
    let result = backend_frontend_clang::legacy::CompilationDatabase::from_directory(
        std::path::Path::new("/definitely/no/compile_commands-here"),
    );
    assert!(matches!(
        result,
        Err(DatabaseError::Absent | DatabaseError::Native)
    ));
}

/// A C++ single-header entry must receive C++ arguments, never the C default.
///
/// This is the argument-discovery regression behind the real-package corpus
/// failures: `nlohmann-json/single_include/nlohmann/json.hpp` and
/// `catch2/single_include/catch2/catch.hpp` are the raw-largest sources of
/// their packages, both `.hpp`, and libclang infers C++ from that extension —
/// so the previous `-std=c11` default produced the fatal
/// `invalid argument '-std=c11' not allowed with 'C++'` and libclang's
/// `CXError_ASTReadError` before any declaration was visited.
#[test]
fn cpp_header_entry_defaults_to_cxx_arguments_and_flag_first_shape() -> Result<(), TestError> {
    let dir = tempfile::tempdir().map_err(|_| TestError::Profile)?;
    let root = dir.path().join("pkg-root");
    std::fs::create_dir_all(root.join("single_include/pkg")).map_err(|_| TestError::Profile)?;
    let header = root.join("single_include/pkg/pkg.hpp");
    std::fs::write(&header, b"").map_err(|_| TestError::Profile)?;
    let project = backend_frontend_clang::ClangProject::open(&root, &header)
        .map_err(|_| TestError::Profile)?;
    let arguments = project.arguments();

    assert!(
        arguments.first().is_some_and(|argument| argument.starts_with('-')),
        "argument vector must be flag-first (no argv[0]): {arguments:?}"
    );
    assert!(
        arguments.ends_with(&[
            "-std=c++17".to_owned(),
            "-x".to_owned(),
            "c++".to_owned(),
        ]),
        "a .hpp entry must default to explicit C++ arguments: {arguments:?}"
    );
    assert!(
        !arguments.iter().any(|argument| argument == "-std=c11"),
        "a .hpp entry must never receive the C default: {arguments:?}"
    );
    Ok(())
}

/// An ambiguous `.h` entry keeps the C default: libclang infers C from that
/// extension, and C projects legitimately select `.h` entries.
#[test]
fn ambiguous_h_entry_keeps_the_c_default() -> Result<(), TestError> {
    let dir = tempfile::tempdir().map_err(|_| TestError::Profile)?;
    let root = dir.path().join("pkg-root");
    std::fs::create_dir_all(root.join("include")).map_err(|_| TestError::Profile)?;
    let header = root.join("include/pkg.h");
    std::fs::write(&header, b"").map_err(|_| TestError::Profile)?;
    let project = backend_frontend_clang::ClangProject::open(&root, &header)
        .map_err(|_| TestError::Profile)?;
    let arguments = project.arguments();

    assert!(
        arguments.last().is_some_and(|argument| argument == "-std=c11"),
        "a .h entry must keep the C default: {arguments:?}"
    );
    assert!(
        !arguments.windows(2).any(|window| window == ["-x", "c++"]),
        "a .h entry must not force the C++ dialect: {arguments:?}"
    );
    Ok(())
}

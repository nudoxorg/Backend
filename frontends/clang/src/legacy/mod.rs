//! Collects direct, bounded libclang facts for one C or C++ translation unit.
//! The crate owns no IR encoding, compiler driver, process execution, or fallback parser.
//! Its public records are source-byte addressed and remain valid after libclang is disposed.
//!
//! A caller supplies a closed [`ClangProfile`], borrowed source authority, and every fact slot.
//! [`collect`] either fills prefixes of those slots from libclang or returns a typed failure.
//! Native-library absence is explicit; the crate never scans source as a substitute authority.
//!
//! CANONICAL AUTHORITY PATH: this `legacy` module IS the production
//! native-authority lane. The engine driver imports its symbols directly from
//! this module; there is no intervening adapter. The crate-level
//! `syntax_frontend()` constructor is the separate, documented structural
//! baseline and never substitutes for this authority.

#![allow(
    unsafe_code,
    reason = "the one contained libclang FFI boundary is reviewed and cannot be expressed safely"
)]

mod collect;
mod error;
pub mod facts;
mod ffi;
mod input;
pub mod purl;
mod scratch;

pub use self::collect::{collect, collect_cancellable};
pub use self::error::{
    CollectError, DatabaseError, NativeApi, NativeFailure, ParseFailure, ScratchLane,
};
pub use self::facts::{
    BuiltinClass, ClangFacts, DeclarationFact, DeclarationId, DeclarationKind, DefinitionState,
    DiagnosticFact, DiagnosticSeverity, IncludeFact, IntegerRank, MAX_CLANG_DECLARATIONS,
    MAX_CLANG_DIAGNOSTICS, MAX_CLANG_FACTS, MAX_CLANG_INCLUDES, MAX_CLANG_OVERRIDES,
    MAX_CLANG_REFERENCES, MAX_CLANG_TYPE_EDGES, MAX_CLANG_TYPES, MethodVirtuality, OverrideFact,
    ReferenceFact, ReferenceKind, ReferenceTarget, SYMBOL_IDENTITY_BYTES, SourceDependencyKind,
    SourceSpan, StorageClass, SymbolIdentity, TypeEdge, TypeFact, TypeId, TypeKind, TypeQualifiers,
    TypeRelation,
};
pub use self::input::{
    ClangInput, DatabaseArgumentError, DatabaseArguments, MAX_DATABASE_ARGUMENTS,
    UnsupportedLanguageProfile,
};
pub use self::scratch::ClangScratch;

/// One command retained by the native compilation database authority.
#[derive(Debug)]
pub struct CompilationCommand {
    pub(crate) file_name: std::ffi::CString,
    pub(crate) arguments: Vec<std::ffi::CString>,
    pub(crate) directory: std::ffi::CString,
}

impl CompilationCommand {
    /// Source path as returned by the command's final database argument.
    pub fn file_name(&self) -> &std::ffi::CStr {
        &self.file_name
    }
    /// All command arguments, including the compiler and source path, verbatim.
    pub fn arguments(&self) -> &[std::ffi::CString] {
        &self.arguments
    }

    /// Working directory recorded by the compilation database.
    pub fn directory(&self) -> &std::ffi::CStr {
        &self.directory
    }
}

/// Native compilation-database snapshot with bounded command arguments.
#[derive(Debug)]
pub struct CompilationDatabase {
    pub(crate) commands: Vec<CompilationCommand>,
}

impl CompilationDatabase {
    /// Opens `compile_commands.json` through libclang's runtime-loaded authority.
    pub fn from_directory(directory: &std::path::Path) -> Result<Self, DatabaseError> {
        ffi::load_database(directory)
    }

    /// Returns commands in native database order.
    pub fn commands(&self) -> &[CompilationCommand] {
        &self.commands
    }
}

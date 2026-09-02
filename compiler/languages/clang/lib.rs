//! Collects direct, bounded libclang facts for one C or C++ translation unit.
//! The crate owns no IR encoding, compiler driver, process execution, or fallback parser.
//! Its public records are source-byte addressed and remain valid after libclang is disposed.
//!
//! A caller supplies a closed [`ClangProfile`], borrowed source authority, and every fact slot.
//! [`collect`] either fills prefixes of those slots from libclang or returns a typed failure.
//! Native-library absence is explicit; the crate never scans source as a substitute authority.

#![allow(
    unsafe_code,
    reason = "the one contained libclang FFI boundary is reviewed and cannot be expressed safely"
)]

mod collect;
mod error;
pub mod facts;
mod ffi;
mod input;
mod scratch;

pub use collect::{collect, collect_cancellable};
pub use error::{CollectError, NativeApi, NativeFailure, ParseFailure, ScratchLane};
pub use facts::{
    ClangFacts, DeclarationFact, DeclarationId, DeclarationKind, DefinitionState, DiagnosticFact,
    DiagnosticSeverity, IncludeFact, ReferenceFact, ReferenceKind, ReferenceTarget,
    SYMBOL_IDENTITY_BYTES, SourceDependencyKind, SourceSpan, SymbolIdentity, TypeEdge, TypeFact,
    TypeId, TypeKind, TypeQualifiers, TypeRelation,
};
pub use input::{ClangInput, UnsupportedLanguageProfile};
pub use scratch::ClangScratch;

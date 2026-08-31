//! Defines terminal common behavior for `compiler-application`, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the terminal common invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Allocation-free facts shared by native and durable terminal projection.

use compiler_driver::NativeDiagnostic;
use interface_core::{
    CompilerAttempt, CompilerCause, CompilerDiagnostic, CompilerTerminal, NativeIoFact,
};

pub(super) const fn compile(
    source: interface_core::SourceAuthority,
    recipe: compiler_vocabulary::CompileRecipeFact,
    cause: CompilerCause,
) -> CompilerTerminal {
    CompilerTerminal::Compile {
        attempted: attempt(source, recipe),
        cause,
    }
}

pub(super) const fn compile_from_driver(
    source: compiler_ir::SourceIdentity,
    recipe: compiler_vocabulary::CompileRecipeFact,
    cause: CompilerCause,
) -> CompilerTerminal {
    compile(source_authority(source), recipe, cause)
}

pub(super) const fn attempt(
    source: interface_core::SourceAuthority,
    recipe: compiler_vocabulary::CompileRecipeFact,
) -> CompilerAttempt {
    CompilerAttempt {
        source,
        recipe: recipe.identity,
    }
}

pub(crate) const fn source_authority(
    source: compiler_ir::SourceIdentity,
) -> interface_core::SourceAuthority {
    interface_core::SourceAuthority {
        identity: source.identity,
        byte_len: source.byte_len,
    }
}

pub(super) fn compiler_diagnostic(diagnostic: NativeDiagnostic<'_>) -> Option<CompilerDiagnostic> {
    CompilerDiagnostic::from_native(diagnostic.bytes, diagnostic.observed, diagnostic.truncated)
}

pub(super) fn io_fact(error: &std::io::Error) -> NativeIoFact {
    NativeIoFact {
        kind: error.kind(),
        raw_os_code: error.raw_os_error(),
    }
}

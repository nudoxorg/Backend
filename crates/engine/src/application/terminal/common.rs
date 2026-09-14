//! Defines terminal common behavior for the `backend-engine` application, whose purpose is to bind application requests to native compilation and durable publication.
//! This module owns the terminal common invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Allocation-free facts shared by native and durable terminal projection.

use crate::driver::{AuthorityDiagnostic, NativeDiagnostic};
use backend_library::interface::{
    CompilerAttempt, CompilerCause, CompilerDiagnostic, CompilerTerminal, NativeIoFact,
};

pub(super) const fn compile(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    cause: CompilerCause,
) -> CompilerTerminal {
    CompilerTerminal::Compile {
        attempted: attempt(source, recipe),
        cause,
    }
}

pub(super) const fn compile_from_driver(
    source: backend_semantic::ir::SourceIdentity,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
    cause: CompilerCause,
) -> CompilerTerminal {
    compile(source_authority(source), recipe, cause)
}

pub(super) const fn attempt(
    source: backend_library::interface::SourceAuthority,
    recipe: backend_semantic::vocabulary::CompileRecipeFact,
) -> CompilerAttempt {
    CompilerAttempt {
        source,
        recipe: recipe.identity,
    }
}

pub(crate) const fn source_authority(
    source: backend_semantic::ir::SourceIdentity,
) -> backend_library::interface::SourceAuthority {
    backend_library::interface::SourceAuthority {
        identity: source.identity,
        byte_len: source.byte_len,
    }
}

pub(super) fn compiler_diagnostic(diagnostic: NativeDiagnostic<'_>) -> Option<CompilerDiagnostic> {
    CompilerDiagnostic::from_native(diagnostic.bytes, diagnostic.observed, diagnostic.truncated)
}

/// Copies a bounded authority diagnostic before its caller-owned scratch lease ends.
pub(super) fn authority_diagnostic(
    diagnostic: AuthorityDiagnostic<'_>,
) -> Option<CompilerDiagnostic> {
    CompilerDiagnostic::from_native(
        diagnostic.primary,
        diagnostic.observed,
        diagnostic.truncated,
    )
}

pub(super) fn io_fact(error: &std::io::Error) -> NativeIoFact {
    NativeIoFact {
        kind: error.kind(),
        raw_os_code: error.raw_os_error(),
    }
}

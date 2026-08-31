//! Allocation-free facts shared by native and durable terminal projection.

use nudox_compile_driver::NativeDiagnostic;
use wave_application_core::{
    CompilerAttempt, CompilerCause, CompilerDiagnostic, CompilerTerminal, NativeIoFact,
};

pub(super) const fn compile(
    source: wave_application_core::SourceAuthority,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    cause: CompilerCause,
) -> CompilerTerminal {
    CompilerTerminal::Compile {
        attempted: attempt(source, recipe),
        cause,
    }
}

pub(super) const fn compile_from_driver(
    source: nudox_ir_format::SourceIdentity,
    recipe: nudox_compile_vocab::CompileRecipeFact,
    cause: CompilerCause,
) -> CompilerTerminal {
    compile(source_authority(source), recipe, cause)
}

pub(super) const fn attempt(
    source: wave_application_core::SourceAuthority,
    recipe: nudox_compile_vocab::CompileRecipeFact,
) -> CompilerAttempt {
    CompilerAttempt {
        source,
        recipe: recipe.identity,
    }
}

pub(crate) const fn source_authority(
    source: nudox_ir_format::SourceIdentity,
) -> wave_application_core::SourceAuthority {
    wave_application_core::SourceAuthority {
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

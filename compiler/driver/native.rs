//! Defines native behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Caller-authorized native parsing, owned work artifacts, and bounded child terminals.

use compiler_vocabulary::NativeTool;

use crate::{
    lower,
    types::{
        CompileControl, CompileFailure, CompileRecipeFact, CompileScratch, NativeRecipe,
        SourceIdentity,
    },
};

mod child;
pub use clang::{
    ClangDiagnostic, ClangDiagnosticSeverity, ClangFailure, ClangPhase, ClangSourceSpan,
};
#[cfg_attr(
    not(clang_native),
    allow(
        dead_code,
        reason = "unlinked builds keep the analysis surface compilable for its typed-unavailable terminal and proofs; the linked drive arm is its production caller"
    )
)]
pub(crate) mod clang;
mod csharp;
mod diagnostic;
mod frontend;
mod go;
mod java;
mod terminal;
pub(crate) mod typescript;
mod work;

pub(crate) fn parse_with_native_tool<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    recipe: NativeRecipe<'source, 'toolchain>,
    source: SourceIdentity,
    recipe_fact: CompileRecipeFact,
    scratch: CompileScratch<'diagnostic, 'work>,
    control: CompileControl<'cancel>,
    authority: &mut lower::Authority<'source>,
) -> Result<(), CompileFailure<'diagnostic>> {
    match recipe.toolchain.tool {
        NativeTool::Rustc => {
            frontend::drive::<frontend::RustFrontend>(recipe, source, recipe_fact, scratch, control)
        }
        NativeTool::Clang => {
            frontend::ClangFrontend::drive(recipe, source, recipe_fact, scratch, control, authority)
        }
        NativeTool::Python => frontend::drive::<frontend::PythonFrontend>(
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        NativeTool::TypeScriptCompiler => frontend::drive::<typescript::TypeScriptFrontend>(
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        NativeTool::CSharpCompiler => {
            frontend::drive::<csharp::CSharpFrontend>(recipe, source, recipe_fact, scratch, control)
        }
        NativeTool::GoCompiler => {
            frontend::drive::<go::GoFrontend>(recipe, source, recipe_fact, scratch, control)
        }
        NativeTool::JavaCompiler => {
            frontend::drive::<java::JavaFrontend>(recipe, source, recipe_fact, scratch, control)
        }
    }
}

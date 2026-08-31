//! Defines native behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Caller-authorized native parsing, owned work artifacts, and bounded child terminals.

use compiler_vocabulary::NativeTool;

use crate::types::{
    CompileControl, CompileFailure, CompileRecipeFact, CompileScratch, NativeRecipe, SourceIdentity,
};

mod child;
mod csharp;
mod diagnostic;
mod frontend;
mod go;
mod java;
mod terminal;
mod typescript;
mod work;

pub(crate) fn parse_with_native_tool<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
    recipe: NativeRecipe<'source, 'toolchain>,
    source: SourceIdentity,
    recipe_fact: CompileRecipeFact,
    scratch: CompileScratch<'diagnostic, 'work>,
    control: CompileControl<'cancel>,
) -> Result<(), CompileFailure<'diagnostic>> {
    match recipe.toolchain.tool {
        NativeTool::Rustc => {
            frontend::drive::<frontend::RustFrontend>(recipe, source, recipe_fact, scratch, control)
        }
        NativeTool::Clang => frontend::drive::<frontend::ClangFrontend>(
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
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

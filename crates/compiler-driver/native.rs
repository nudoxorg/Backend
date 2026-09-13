//! Defines native behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Caller-authorized native parsing, owned work artifacts, and bounded child terminals.

use backend_semantic::vocabulary::LanguageProfile;

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
    match recipe.profile {
        LanguageProfile::Rust(profile) => frontend::drive::<frontend::RustFrontend>(
            profile,
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        LanguageProfile::C(profile) => frontend::drive::<frontend::ClangFrontend>(
            frontend::ClangProfile::C(profile),
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        LanguageProfile::Cxx(profile) => frontend::drive::<frontend::ClangFrontend>(
            frontend::ClangProfile::Cxx(profile),
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        LanguageProfile::Python(profile) => frontend::drive::<frontend::PythonFrontend>(
            profile,
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        LanguageProfile::TypeScript(profile) => frontend::drive::<typescript::TypeScriptFrontend>(
            profile,
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        LanguageProfile::CSharp(profile) => frontend::drive::<csharp::CSharpFrontend>(
            profile,
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        LanguageProfile::Go(profile) => frontend::drive::<go::GoFrontend>(
            profile,
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
        LanguageProfile::Java(profile) => frontend::drive::<java::JavaFrontend>(
            profile,
            recipe,
            source,
            recipe_fact,
            scratch,
            control,
        ),
    }
}

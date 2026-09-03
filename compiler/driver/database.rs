//! Additive driver entry for one translation unit selected by libclang's database.

use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

use compiler_ir::{FragmentError, FragmentView, SourceIdentity};
use compiler_languages_clang::{
    ClangInput, CompilationDatabase, DatabaseArgumentError, DatabaseError, MAX_DATABASE_ARGUMENTS,
};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};
use heart_identity::SourceFactDomain;
use thiserror::Error;

use crate::{CompiledFragment, ResolvedToolchain, lower};

/// Typed terminal for the database-selected translation-unit entry.
#[derive(Debug, Error)]
pub enum DatabaseCompileFailure<'source> {
    #[error("compilation database could not be opened")]
    Database(#[source] DatabaseError),
    #[error("translation unit was not present in the compilation database")]
    TranslationUnitAbsent,
    #[error("database arguments were rejected")]
    Arguments(#[source] DatabaseArgumentError),
    #[error("libclang rejected the database translation unit")]
    Authority(#[source] compiler_languages_clang::CollectError),
    #[error("database translation unit fact was rejected by canonical admission")]
    Rejected {
        source_identity: SourceIdentity,
        recipe: CompileRecipeFact,
        rejected: crate::FactRejection,
    },
    #[error("database translation unit was cancelled")]
    Cancelled { input: &'source [u8] },
    #[error("database translation unit could not be lowered")]
    Lowering(#[source] compiler_vocabulary::LoweringUnsupported),
    #[error("database translation unit fragment was invalid")]
    Fragment(#[source] FragmentError),
    #[error("source is too large for its identity")]
    SourceLength {
        actual: usize,
        #[source]
        cause: std::num::TryFromIntError,
    },
}

/// Compiles the exact source of one command selected from `compile_commands.json`.
/// The source and fragment remain caller-owned; database arguments are borrowed
/// only for the native collection transaction.
pub fn compile_database_translation_unit<'source, 'toolchain, 'cancel, 'output>(
    database_directory: &Path,
    translation_unit: &Path,
    profile: LanguageProfile,
    stage: Stage,
    source: &'source [u8],
    toolchain: ResolvedToolchain<'toolchain>,
    cancelled: &'cancel AtomicBool,
    output: &'output mut [u8],
) -> Result<CompiledFragment<'output>, DatabaseCompileFailure<'source>> {
    if cancelled.load(Ordering::Acquire) {
        return Err(DatabaseCompileFailure::Cancelled { input: source });
    }
    let database = CompilationDatabase::from_directory(database_directory)
        .map_err(DatabaseCompileFailure::Database)?;
    let requested = translation_unit.to_string_lossy();
    let command = database
        .commands()
        .iter()
        .find(|command| command.file_name().to_bytes() == requested.as_bytes())
        .ok_or(DatabaseCompileFailure::TranslationUnitAbsent)?;
    let arguments = command.arguments();
    if arguments.len() > MAX_DATABASE_ARGUMENTS {
        return Err(DatabaseCompileFailure::Arguments(DatabaseArgumentError {
            required: arguments.len(),
            capacity: MAX_DATABASE_ARGUMENTS,
        }));
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(DatabaseCompileFailure::Cancelled { input: source });
    }
    let mut borrowed = [c""; MAX_DATABASE_ARGUMENTS];
    for (slot, argument) in borrowed.iter_mut().zip(arguments) {
        *slot = argument.as_c_str();
    }
    let argument_slice = &borrowed[..arguments.len()];
    let file_name = command.file_name();
    let input = ClangInput::from_database(file_name, source, argument_slice, command.directory())
        .map_err(DatabaseCompileFailure::Arguments)?;
    let byte_len =
        u32::try_from(source.len()).map_err(|cause| DatabaseCompileFailure::SourceLength {
            actual: source.len(),
            cause,
        })?;
    let source_identity = SourceIdentity {
        identity: heart_identity::ContentId::<SourceFactDomain>::from_canonical_bytes(source),
        byte_len,
    };
    let recipe = CompileRecipeFact::derive(
        profile,
        stage,
        NativeTool::Clang,
        source_identity.identity,
        toolchain.identity,
    );
    let bytes = lower::clang::lower_database(
        input,
        source,
        source_identity,
        recipe,
        profile,
        cancelled,
        output,
    )
    .map_err(|cause| match cause {
        lower::clang::ClangCollectError::Authority(
            compiler_languages_clang::CollectError::Cancelled,
        ) => DatabaseCompileFailure::Cancelled { input: source },
        lower::clang::ClangCollectError::Authority(cause) => {
            DatabaseCompileFailure::Authority(cause)
        }
        lower::clang::ClangCollectError::Rejected(rejected) => {
            DatabaseCompileFailure::Rejected {
                source_identity,
                recipe,
                rejected,
            }
        }
        lower::clang::ClangCollectError::Lowering(cause) => DatabaseCompileFailure::Lowering(cause),
    })?;
    let fragment = FragmentView::validate(bytes).map_err(DatabaseCompileFailure::Fragment)?;
    Ok(CompiledFragment {
        source: source_identity,
        recipe,
        fragment,
    })
}

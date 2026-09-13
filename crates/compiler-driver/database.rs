//! Typed compilation-database entry for one Clang translation unit.
//!
//! Database selection and native argument borrowing stay at this adapter
//! boundary.  The selected unit then enters the same Clang fact collector and
//! compact canonical admission path as the ordinary driver.

use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use compiler_ir::{
    CanonicalDataError, FragmentError, FragmentView, PrepareError, SourceIdentity, WriteError,
};
use compiler_languages_clang::{
    ClangInput, CompilationDatabase, DatabaseArgumentError, DatabaseError, MAX_DATABASE_ARGUMENTS,
};
use backend_semantic::vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};
use backend_version::SourceFactDomain;
use thiserror::Error;

use crate::{CompiledFragment, ResolvedToolchain, lower};

/// Exact terminal from compilation-database selection through compact admission.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DatabaseCompileFailure<'source> {
    /// The compilation database could not be opened or decoded.
    #[error("compilation database could not be opened")]
    Database(#[source] DatabaseError),
    /// The requested translation unit had no database command.
    #[error("translation unit {requested:?} was absent from the compilation database")]
    TranslationUnitAbsent {
        /// Exact caller-selected path.
        requested: PathBuf,
    },
    /// The selected native argument lane was malformed or exceeded its bound.
    #[error("compilation database arguments were rejected")]
    Arguments(#[source] DatabaseArgumentError),
    /// A non-Clang profile reached the Clang database adapter.
    #[error("language profile {profile:?} is incompatible with the Clang database adapter")]
    Profile {
        /// Exact rejected profile.
        profile: LanguageProfile,
    },
    /// The database adapter was asked for a stage that produces no fragment.
    #[error("stage {stage:?} does not produce a compilation-database fragment")]
    Stage {
        /// Exact rejected stage.
        stage: Stage,
    },
    /// Libclang rejected the selected translation unit.
    #[error("libclang rejected the database translation unit")]
    Authority(#[source] compiler_languages_clang::CollectError),
    /// The bounded canonical fact lane rejected one exact fact.
    #[error("database translation unit rejected fact {rejected:?} for {recipe:?}")]
    Rejected {
        /// Entered source identity.
        source_identity: SourceIdentity,
        /// Exact closed lowering recipe.
        recipe: CompileRecipeFact,
        /// Exact fact ordinal, name length, and typed cause.
        rejected: crate::FactRejection,
    },
    /// A libclang authority coordinate could not enter the shared semantic lane.
    #[error("database translation unit projection failed: {fault:?}")]
    Projection {
        /// Exact native fact that could not be projected.
        fault: crate::ClangProjectionFault,
    },
    /// Cancellation was observed before canonical admission.
    #[error("database translation unit was cancelled")]
    Cancelled {
        /// Exact source lease governed by the cancelled operation.
        input: &'source [u8],
    },
    /// A supported authority fact has no closed lowering recipe.
    #[error("database translation unit could not be lowered")]
    Lowering(#[source] backend_semantic::vocabulary::LoweringUnsupported),
    /// Canonical ordering or identity preparation rejected the fact image.
    #[error("database translation unit could not canonicalize its facts")]
    Canonical {
        /// Entered source identity.
        source_identity: SourceIdentity,
        /// Exact closed lowering recipe.
        recipe: CompileRecipeFact,
        /// Original canonicalization source.
        #[source]
        cause: CanonicalDataError,
    },
    /// Compact fragment preparation rejected the canonical data.
    #[error("database translation unit could not prepare its fragment")]
    Prepare {
        /// Entered source identity.
        source_identity: SourceIdentity,
        /// Exact closed lowering recipe.
        recipe: CompileRecipeFact,
        /// Original preparation source.
        #[source]
        cause: PrepareError,
    },
    /// Caller-owned output could not receive the prepared fragment.
    #[error("database translation unit could not write its fragment")]
    Write {
        /// Entered source identity.
        source_identity: SourceIdentity,
        /// Exact closed lowering recipe.
        recipe: CompileRecipeFact,
        /// Original write source.
        #[source]
        cause: WriteError,
    },
    /// An extension named an atom outside the admitted provisional lane.
    #[error("database translation unit extension atom {provisional} at row {row} is unbound")]
    ExtensionAtom {
        /// Entered source identity.
        source_identity: SourceIdentity,
        /// Exact closed lowering recipe.
        recipe: CompileRecipeFact,
        /// Extension row containing the bad coordinate.
        row: usize,
        /// Rejected provisional atom coordinate.
        provisional: u32,
        /// Number of admitted provisional atoms.
        atom_count: usize,
    },
    /// A language extension named a type-parameter range outside its pool.
    #[error(
        "database translation unit extension row {row} names type-parameter range {start}+{length} outside {element_count} elements"
    )]
    ExtensionTypeParameters {
        /// Entered source identity.
        source_identity: SourceIdentity,
        /// Exact closed lowering recipe.
        recipe: CompileRecipeFact,
        /// Extension row containing the bad range.
        row: usize,
        /// First claimed element.
        start: u32,
        /// Claimed element count.
        length: u32,
        /// Complete admitted element count.
        element_count: usize,
    },
    /// The bytes written by admission did not validate as a compact fragment.
    #[error("database translation unit fragment was invalid")]
    Fragment(#[source] FragmentError),
    /// Source length could not fit the compact source identity width.
    #[error("source has {actual} bytes, exceeding the compact identity width")]
    SourceLength {
        /// Complete observed source length.
        actual: usize,
        /// Checked conversion source.
        #[source]
        cause: std::num::TryFromIntError,
    },
}

/// Compiles one exact source buffer under the command selected from
/// `compile_commands.json`.
///
/// Database strings are borrowed only during native collection.  The returned
/// fragment borrows only `output`, while cancellation and the source remain
/// observable on their exact failure terminals.
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
    if !matches!(profile, LanguageProfile::C(_) | LanguageProfile::Cxx(_)) {
        return Err(DatabaseCompileFailure::Profile { profile });
    }
    if stage != Stage::LowerIr {
        return Err(DatabaseCompileFailure::Stage { stage });
    }
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
        .ok_or_else(|| DatabaseCompileFailure::TranslationUnitAbsent {
            requested: translation_unit.to_path_buf(),
        })?;
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
    let input = ClangInput::from_database(
        command.file_name(),
        source,
        &borrowed[..arguments.len()],
        command.directory(),
    )
    .map_err(DatabaseCompileFailure::Arguments)?;
    compile_database_input(input, profile, stage, source, toolchain, cancelled, output)
}

fn compile_database_input<'input, 'source, 'toolchain, 'cancel, 'output>(
    input: ClangInput<'input>,
    profile: LanguageProfile,
    stage: Stage,
    source: &'source [u8],
    toolchain: ResolvedToolchain<'toolchain>,
    cancelled: &'cancel AtomicBool,
    output: &'output mut [u8],
) -> Result<CompiledFragment<'output>, DatabaseCompileFailure<'source>> {
    let byte_len =
        u32::try_from(source.len()).map_err(|cause| DatabaseCompileFailure::SourceLength {
            actual: source.len(),
            cause,
        })?;
    let source_identity = SourceIdentity {
        identity: backend_version::ContentId::<SourceFactDomain>::from_canonical_bytes(source),
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
        lower::clang::ClangCollectError::Rejected(rejected) => DatabaseCompileFailure::Rejected {
            source_identity,
            recipe,
            rejected,
        },
        lower::clang::ClangCollectError::Projection(fault) => {
            DatabaseCompileFailure::Projection { fault }
        }
        lower::clang::ClangCollectError::Lowering(cause) => DatabaseCompileFailure::Lowering(cause),
        lower::clang::ClangCollectError::Admission(cause) => {
            admission_failure(source_identity, recipe, cause)
        }
    })?;
    let fragment = FragmentView::validate(bytes).map_err(DatabaseCompileFailure::Fragment)?;
    Ok(CompiledFragment {
        source: source_identity,
        recipe,
        fragment,
    })
}

fn admission_failure(
    source_identity: SourceIdentity,
    recipe: CompileRecipeFact,
    cause: lower::AdmissionFault,
) -> DatabaseCompileFailure<'static> {
    match cause {
        lower::AdmissionFault::Canonical(cause) => DatabaseCompileFailure::Canonical {
            source_identity,
            recipe,
            cause,
        },
        lower::AdmissionFault::Prepare(cause) => DatabaseCompileFailure::Prepare {
            source_identity,
            recipe,
            cause,
        },
        lower::AdmissionFault::Write(cause) => DatabaseCompileFailure::Write {
            source_identity,
            recipe,
            cause,
        },
        lower::AdmissionFault::ExtensionAtom {
            row,
            provisional,
            atom_count,
        } => DatabaseCompileFailure::ExtensionAtom {
            source_identity,
            recipe,
            row,
            provisional,
            atom_count,
        },
        lower::AdmissionFault::ExtensionTypeParameters {
            row,
            start,
            length,
            element_count,
        } => DatabaseCompileFailure::ExtensionTypeParameters {
            source_identity,
            recipe,
            row,
            start,
            length,
            element_count,
        },
    }
}

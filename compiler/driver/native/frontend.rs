//! Defines native frontend behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native frontend invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{ffi::OsString, path::Path, process::Command};
#[cfg(clang_native)]
use std::{sync::atomic::Ordering, time::Instant};

#[cfg(clang_native)]
use crate::types::NativeDiagnostic;
use crate::types::{
    CompileControl, CompileFailure, CompileRecipeFact, CompileScratch, NativeArtifactRole,
    NativeRecipe, NativeWorkError, NativeWorkPhase, NativeWorkPrimary, ResolvedToolchain,
    SourceIdentity,
};

use super::{
    child::drive_child,
    clang::ClangFailure,
    work::{
        cleanup_native_work, compound_native_work_cleanup, prepare_native_work,
        remove_file_if_present,
    },
};

const RUST_METADATA_FILE: &str = "compiler-probe.rmeta";

pub(super) trait NativeFrontend {
    /// Materializes only this adapter's exact owned input/configuration artifacts.
    fn prepare(_native_work: &Path, _source: &[u8]) -> Result<(), NativeWorkError> {
        Ok(())
    }

    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command;

    /// Whether exact request source must be sent through the child input lease.
    fn source_via_stdin() -> bool {
        true
    }

    /// Removes only this adapter's exact owned inputs and outputs after child reaping.
    fn cleanup(_native_work: &Path) -> Result<(), NativeWorkError> {
        Ok(())
    }
}

pub(super) struct RustFrontend;
pub(super) struct ClangFrontend;
pub(super) struct PythonFrontend;

impl ClangFrontend {
    /// Drives the direct libclang semantic authority over the exact caller source.
    ///
    /// No process is launched: the declared executable stays a caller identity, the exact
    /// link-time libclang authority is the only semantic engine, and the caller-owned work
    /// directory is the only include root, so no ambient search path is consulted. The
    /// typed facts the analysis produces stream into the caller's authority lanes and
    /// become the multi-declaration emission seam's production input.
    pub(super) fn drive<'source, 'toolchain, 'cancel, 'diagnostic, 'work>(
        recipe: NativeRecipe<'source, 'toolchain>,
        source: SourceIdentity,
        recipe_fact: CompileRecipeFact,
        scratch: CompileScratch<'diagnostic, 'work>,
        control: CompileControl<'cancel>,
        authority: &mut crate::lower::Authority<'source>,
    ) -> Result<(), CompileFailure<'diagnostic>> {
        #[cfg(not(clang_native))]
        #[allow(
            unused_variables,
            reason = "the unavailable terminal is selected before any analysis inputs can be consumed"
        )]
        {
            // The link-time authority is a build fact, not a race with caller control
            // state: an authority that cannot exist fails before observing cancellation.
            Err(CompileFailure::ClangFrontend {
                source_identity: source,
                recipe: recipe_fact,
                cause: ClangFailure::LibclangUnavailable,
            })
        }
        #[cfg(clang_native)]
        {
            use super::clang::{
                AnalysisInput, AnalysisScratch, ClangSourceLanguage, MAX_ANALYSIS_SCRATCH_BYTES,
            };

            let diagnostic = NativeDiagnostic {
                bytes: &[],
                observed: 0,
                truncated: false,
            };
            if control.cancelled.load(Ordering::Acquire) {
                return Err(CompileFailure::Cancelled {
                    source_identity: source,
                    recipe: recipe_fact,
                    diagnostic,
                });
            }
            if Instant::now() >= control.deadline {
                return Err(CompileFailure::DeadlineExceeded {
                    source_identity: source,
                    recipe: recipe_fact,
                    diagnostic,
                });
            }
            // Exact-bound scratch for one analysis (the frozen interim resource ledger);
            // becomes caller-provided when the emission seam lands.
            let mut identity_scratch = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
            let mut fact_scratch = vec![0_u8; MAX_ANALYSIS_SCRATCH_BYTES];
            super::clang::analyze(
                AnalysisInput {
                    include_root: scratch.native_work,
                    source_name: Path::new("source.c"),
                    source_language: ClangSourceLanguage::C,
                    source: recipe.source,
                },
                None,
                Some(control.cancelled),
                Some(control.deadline),
                AnalysisScratch {
                    identity: &mut identity_scratch,
                    facts: &mut fact_scratch,
                },
                |fact| authority.record(fact),
            )
            .map(|_report| ())
            .map_err(|error| match ClangFailure::from(error) {
                ClangFailure::Cancelled { .. } => CompileFailure::Cancelled {
                    source_identity: source,
                    recipe: recipe_fact,
                    diagnostic,
                },
                ClangFailure::DeadlineExceeded { .. } => CompileFailure::DeadlineExceeded {
                    source_identity: source,
                    recipe: recipe_fact,
                    diagnostic,
                },
                cause => CompileFailure::ClangFrontend {
                    source_identity: source,
                    recipe: recipe_fact,
                    cause,
                },
            })
        }
    }
}

impl NativeFrontend for RustFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        let mut metadata = OsString::from("--emit=metadata=");
        metadata.push(native_work.join(RUST_METADATA_FILE));
        command.args([
            "--crate-type=lib",
            "--edition=2024",
            "--crate-name=compiler_probe",
        ]);
        command.arg(metadata).arg("-").current_dir(native_work);
        command
    }

    fn cleanup(native_work: &Path) -> Result<(), NativeWorkError> {
        remove_file_if_present(
            native_work.join(RUST_METADATA_FILE),
            NativeArtifactRole::RustMetadata,
        )
    }
}

impl NativeFrontend for PythonFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command.args([
            "-c",
            "import sys; compile(sys.stdin.read(), '<heart>', 'exec')",
        ]);
        command.current_dir(native_work);
        command
    }
}

pub(super) fn drive<
    'source,
    'toolchain,
    'cancel,
    'diagnostic,
    'work,
    ConcreteFrontend: NativeFrontend,
>(
    recipe: NativeRecipe<'source, 'toolchain>,
    source: SourceIdentity,
    recipe_fact: CompileRecipeFact,
    scratch: CompileScratch<'diagnostic, 'work>,
    control: CompileControl<'cancel>,
) -> Result<(), CompileFailure<'diagnostic>> {
    prepare_native_work(scratch.native_work).map_err(|cause| CompileFailure::NativeWork {
        source_identity: source,
        recipe: recipe_fact,
        phase: NativeWorkPhase::Prepare,
        cause,
    })?;
    if let Err(cause) = ConcreteFrontend::prepare(scratch.native_work, recipe.source) {
        return match cleanup_native_work::<ConcreteFrontend>(scratch.native_work) {
            Ok(()) => Err(CompileFailure::NativeWork {
                source_identity: source,
                recipe: recipe_fact,
                phase: NativeWorkPhase::Prepare,
                cause,
            }),
            Err(cleanup) => Err(CompileFailure::NativeWorkCleanup {
                source_identity: source,
                recipe: recipe_fact,
                primary: NativeWorkPrimary::Prepare { cause },
                cleanup,
            }),
        };
    }
    let CompileScratch {
        diagnostic_output,
        native_work,
    } = scratch;
    let result = drive_child::<ConcreteFrontend>(
        recipe,
        source,
        recipe_fact,
        diagnostic_output,
        native_work,
        control,
    );
    match cleanup_native_work::<ConcreteFrontend>(native_work) {
        Ok(()) => result,
        Err(cleanup) => match result {
            Ok(()) => Err(CompileFailure::NativeWork {
                source_identity: source,
                recipe: recipe_fact,
                phase: NativeWorkPhase::Cleanup,
                cause: cleanup,
            }),
            Err(primary) => Err(compound_native_work_cleanup(
                primary,
                source,
                recipe_fact,
                cleanup,
            )),
        },
    }
}

use std::{ffi::OsString, path::Path, process::Command};

use crate::types::{
    CompileControl, CompileFailure, CompileRecipeFact, CompileScratch, NativeArtifactRole,
    NativeRecipe, NativeWorkError, NativeWorkPhase, NativeWorkPrimary, ResolvedToolchain,
    SourceIdentity,
};

use super::{
    child::drive_child,
    work::{
        cleanup_native_work, compound_native_work_cleanup, prepare_native_work,
        remove_file_if_present,
    },
};

const RUST_METADATA_FILE: &str = "nudox-probe.rmeta";

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

impl NativeFrontend for RustFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        let mut metadata = OsString::from("--emit=metadata=");
        metadata.push(native_work.join(RUST_METADATA_FILE));
        command.args([
            "--crate-type=lib",
            "--edition=2024",
            "--crate-name=nudox_probe",
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

impl NativeFrontend for ClangFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args(["-x", "c", "-fsyntax-only", "-w", "-"])
            .current_dir(native_work);
        command
    }
}

impl NativeFrontend for PythonFrontend {
    fn command(toolchain: ResolvedToolchain<'_>, native_work: &Path) -> Command {
        let mut command = Command::new(toolchain.executable());
        command.args([
            "-c",
            "import sys; compile(sys.stdin.read(), '<nudox>', 'exec')",
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

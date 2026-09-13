//! Defines native frontend behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native frontend invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{ffi::OsString, path::Path, process::Command};

use backend_semantic::vocabulary::{CStandard, CxxStandard, PythonVersion, RustEdition};

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

const RUST_METADATA_FILE: &str = "compiler-probe.rmeta";

pub(super) trait NativeFrontend {
    type Profile: Copy;

    /// Materializes only this adapter's exact owned input/configuration artifacts.
    fn prepare(
        _profile: Self::Profile,
        _native_work: &Path,
        _source: &[u8],
    ) -> Result<(), NativeWorkError> {
        Ok(())
    }

    fn command(
        profile: Self::Profile,
        toolchain: ResolvedToolchain<'_>,
        native_work: &Path,
    ) -> Command;

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
    type Profile = RustEdition;

    fn command(
        profile: Self::Profile,
        toolchain: ResolvedToolchain<'_>,
        native_work: &Path,
    ) -> Command {
        let mut command = Command::new(toolchain.executable());
        let mut metadata = OsString::from("--emit=metadata=");
        metadata.push(native_work.join(RUST_METADATA_FILE));
        command
            .args(["--crate-type=lib", "--edition", rust_edition(profile)])
            .arg("--crate-name=compiler_probe");
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
    type Profile = ClangProfile;

    fn command(
        profile: Self::Profile,
        toolchain: ResolvedToolchain<'_>,
        native_work: &Path,
    ) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args([
                "-x",
                profile.language(),
                profile.standard(),
                "-fsyntax-only",
                "-w",
                "-",
            ])
            .current_dir(native_work);
        command
    }
}

impl NativeFrontend for PythonFrontend {
    type Profile = PythonVersion;

    fn command(
        profile: Self::Profile,
        toolchain: ResolvedToolchain<'_>,
        native_work: &Path,
    ) -> Command {
        let mut command = Command::new(toolchain.executable());
        command
            .args([
                "-c",
                "import ast,sys; ast.parse(sys.stdin.read(), '<heart>', 'exec', feature_version=(3,int(sys.argv[1])))",
            ])
            .arg(python_minor(profile));
        command.current_dir(native_work);
        command
    }
}

#[derive(Clone, Copy)]
pub(super) enum ClangProfile {
    C(CStandard),
    Cxx(CxxStandard),
}

impl ClangProfile {
    const fn language(self) -> &'static str {
        match self {
            Self::C(_) => "c",
            Self::Cxx(_) => "c++",
        }
    }

    const fn standard(self) -> &'static str {
        match self {
            Self::C(CStandard::C11) => "-std=c11",
            Self::C(CStandard::C17) => "-std=c17",
            Self::C(CStandard::C23) => "-std=c23",
            Self::Cxx(CxxStandard::Cxx17) => "-std=c++17",
            Self::Cxx(CxxStandard::Cxx20) => "-std=c++20",
            Self::Cxx(CxxStandard::Cxx23) => "-std=c++23",
            Self::Cxx(CxxStandard::Cxx26) => "-std=c++26",
        }
    }
}

const fn rust_edition(profile: RustEdition) -> &'static str {
    match profile {
        RustEdition::Rust2015 => "2015",
        RustEdition::Rust2018 => "2018",
        RustEdition::Rust2021 => "2021",
        RustEdition::Rust2024 => "2024",
    }
}

const fn python_minor(profile: PythonVersion) -> &'static str {
    match profile {
        PythonVersion::Python310 => "10",
        PythonVersion::Python311 => "11",
        PythonVersion::Python312 => "12",
        PythonVersion::Python313 => "13",
        PythonVersion::Python314 => "14",
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
    profile: ConcreteFrontend::Profile,
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
    if let Err(cause) = ConcreteFrontend::prepare(profile, scratch.native_work, recipe.source) {
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
        profile,
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

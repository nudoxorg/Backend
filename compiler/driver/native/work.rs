//! Defines native work behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the native work invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
use std::{fs, io, path::Path};

use crate::types::{
    CompileFailure, CompileRecipeFact, NativeArtifactRole, NativeWorkError, NativeWorkPhase,
    NativeWorkPrimary, SourceIdentity,
};

use super::frontend::NativeFrontend;

pub(super) fn prepare_native_work(native_work: &Path) -> Result<(), NativeWorkError> {
    if !native_work.is_absolute() {
        return Err(NativeWorkError::RelativeDirectory);
    }
    let mut entries = fs::read_dir(native_work).map_err(NativeWorkError::Inspect)?;
    match entries.next() {
        Some(Ok(_entry)) => Err(NativeWorkError::NotEmpty),
        Some(Err(cause)) => Err(NativeWorkError::Inspect(cause)),
        None => Ok(()),
    }
}

pub(super) fn cleanup_native_work<ConcreteFrontend: NativeFrontend>(
    native_work: &Path,
) -> Result<(), NativeWorkError> {
    ConcreteFrontend::cleanup(native_work)?;
    prepare_native_work(native_work)
}

pub(super) fn write_artifact(
    path: &Path,
    bytes: &[u8],
    artifact: NativeArtifactRole,
) -> Result<(), NativeWorkError> {
    fs::write(path, bytes).map_err(|cause| NativeWorkError::WriteArtifact { artifact, cause })
}

pub(super) fn create_artifact_directory(
    path: &Path,
    artifact: NativeArtifactRole,
) -> Result<(), NativeWorkError> {
    fs::create_dir(path)
        .map_err(|cause| NativeWorkError::CreateArtifactDirectory { artifact, cause })
}

pub(super) fn remove_file_if_present(
    path: std::path::PathBuf,
    artifact: NativeArtifactRole,
) -> Result<(), NativeWorkError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(cause) => Err(NativeWorkError::RemoveArtifact { artifact, cause }),
    }
}

pub(super) fn remove_directory_if_present(
    path: std::path::PathBuf,
    artifact: NativeArtifactRole,
) -> Result<(), NativeWorkError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(cause) => Err(NativeWorkError::RemoveArtifact { artifact, cause }),
    }
}

pub(super) fn compound_native_work_cleanup<'diagnostic>(
    primary: CompileFailure<'diagnostic>,
    source: SourceIdentity,
    recipe: CompileRecipeFact,
    cleanup: NativeWorkError,
) -> CompileFailure<'diagnostic> {
    let primary = match primary {
        CompileFailure::NativeWork {
            phase: NativeWorkPhase::Prepare,
            cause,
            ..
        } => NativeWorkPrimary::Prepare { cause },
        CompileFailure::ToolStart { cause, .. } => NativeWorkPrimary::ToolStart { cause },
        CompileFailure::MissingToolInput { .. } => NativeWorkPrimary::MissingToolInput,
        CompileFailure::MissingToolInputCleanup { cleanup, .. } => {
            NativeWorkPrimary::MissingToolInputCleanup { cleanup }
        }
        CompileFailure::MissingToolDiagnostic { .. } => NativeWorkPrimary::MissingToolDiagnostic,
        CompileFailure::MissingToolDiagnosticCleanup { cleanup, .. } => {
            NativeWorkPrimary::MissingToolDiagnosticCleanup { cleanup }
        }
        CompileFailure::ToolInput { cause, .. } => NativeWorkPrimary::ToolInput { cause },
        CompileFailure::ToolInputCleanup { cause, cleanup, .. } => {
            NativeWorkPrimary::ToolInputCleanup { cause, cleanup }
        }
        CompileFailure::ToolTerminate { cause, .. } => NativeWorkPrimary::ToolTerminate { cause },
        CompileFailure::ToolWait { cause, .. } => NativeWorkPrimary::ToolWait { cause },
        CompileFailure::ToolWaitCleanup { cause, cleanup, .. } => {
            NativeWorkPrimary::ToolWaitCleanup { cause, cleanup }
        }
        CompileFailure::ToolDiagnosticRead { cause, .. } => {
            NativeWorkPrimary::ToolDiagnosticRead { cause }
        }
        CompileFailure::ToolDiagnosticReadCleanup { cause, cleanup, .. } => {
            NativeWorkPrimary::ToolDiagnosticReadCleanup { cause, cleanup }
        }
        CompileFailure::NativeWorkerPanic { cause, .. } => NativeWorkPrimary::WorkerPanic { cause },
        CompileFailure::Cancelled { diagnostic, .. } => NativeWorkPrimary::Cancelled { diagnostic },
        CompileFailure::DeadlineExceeded { diagnostic, .. } => {
            NativeWorkPrimary::DeadlineExceeded { diagnostic }
        }
        CompileFailure::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
            ..
        } => NativeWorkPrimary::DiagnosticLimit {
            limit,
            observed,
            diagnostic,
        },
        CompileFailure::NativeRejected {
            status, diagnostic, ..
        } => NativeWorkPrimary::NativeRejected { status, diagnostic },
        CompileFailure::SourceLength { .. }
        | CompileFailure::UnsupportedStage { .. }
        | CompileFailure::ToolchainSelectionMismatch { .. }
        | CompileFailure::ToolchainMismatch { .. }
        | CompileFailure::ExtensionAtomUnbound { .. }
        | CompileFailure::NativeWork {
            phase: NativeWorkPhase::Cleanup,
            ..
        }
        | CompileFailure::NativeWorkCleanup { .. }
        | CompileFailure::ToolingUnavailable { .. }
        | CompileFailure::Authority { .. }
        | CompileFailure::AuthorityInputRequired { .. }
        | CompileFailure::AuthorityInputProfileMismatch { .. }
        | CompileFailure::LoweringUnsupported { .. }
        | CompileFailure::Build { .. }
        | CompileFailure::Prepare { .. }
        | CompileFailure::Write { .. }
        | CompileFailure::Validate { .. } => return primary,
    };
    CompileFailure::NativeWorkCleanup {
        source_identity: source,
        recipe,
        primary,
        cleanup,
    }
}
